//! `textDocument/semanticTokens` (issue #493).
//!
//! Classifies identifiers from the token index of the single open file:
//! types by their declaration keyword, members by how they are reached
//! (`.`, `::`, struct-literal key), locals and parameters by the declarations
//! of their enclosing function. Types declared in other files fall back to
//! the naming convention (an upper-case initial) and are marked `class`;
//! standard-library and builtin types additionally carry `defaultLibrary`.

use crate::navigation::{local_decl_count, FileIndex};
use std::collections::{HashMap, HashSet};
use tinox_lexer::{Keyword, TokenKind};
use tower_lsp::lsp_types::{SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokensLegend, Url};

const T_NAMESPACE: u32 = 0;
const T_CLASS: u32 = 1;
const T_ENUM: u32 = 2;
const T_INTERFACE: u32 = 3;
const T_STRUCT: u32 = 4;
const T_TYPE_PARAM: u32 = 5;
const T_PARAMETER: u32 = 6;
const T_VARIABLE: u32 = 7;
const T_PROPERTY: u32 = 8;
const T_ENUM_MEMBER: u32 = 9;
const T_FUNCTION: u32 = 10;
const T_METHOD: u32 = 11;
const T_DECORATOR: u32 = 12;

const M_DECLARATION: u32 = 1;
const M_READONLY: u32 = 1 << 1;
const M_STATIC: u32 = 1 << 2;
const M_DEFAULT_LIBRARY: u32 = 1 << 3;

const BUILTIN_TYPES: &[&str] = &[
    "Int8", "Int16", "Int32", "Int64", "UInt8", "UInt16", "UInt32", "UInt64", "Float32", "Float64", "Bool",
    "Char", "String", "List", "Map", "Set", "Array", "Option", "Result", "Task", "Channel",
];

pub fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: ["namespace", "class", "enum", "interface", "struct", "typeParameter", "parameter", "variable", "property", "enumMember", "function", "method", "decorator"]
            .iter()
            .map(|s| SemanticTokenType::new(s))
            .collect(),
        token_modifiers: ["declaration", "readonly", "static", "defaultLibrary"].iter().map(|s| SemanticTokenModifier::new(s)).collect(),
    }
}

/// An absolute token: zero-based line, UTF-16 column and length.
#[derive(Debug, Clone, PartialEq)]
pub struct Abs {
    pub line: u32,
    pub col: u32,
    pub len: u32,
    pub ty: u32,
    pub mods: u32,
}

fn type_kind_from_keyword(k: &TokenKind) -> u32 {
    match k {
        TokenKind::Keyword(Keyword::Enum) => T_ENUM,
        TokenKind::Keyword(Keyword::Interface | Keyword::Trait) => T_INTERFACE,
        TokenKind::Keyword(Keyword::Immutable) => T_STRUCT,
        _ => T_CLASS,
    }
}

/// Parses `<A, B>` after token `i` and returns the names.
fn generic_params(f: &FileIndex, i: usize, out: &mut HashSet<String>) {
    if !f.toks.get(i).is_some_and(|t| t.is(&TokenKind::Less)) {
        return;
    }
    let mut depth = 0i32;
    let mut j = i;
    while j < f.toks.len() {
        match f.toks[j].kind {
            TokenKind::Less => depth += 1,
            TokenKind::Greater => depth -= 1,
            TokenKind::GreaterGreater => depth -= 2,
            _ => {
                if depth == 1 {
                    if let Some(n) = f.toks[j].ident() {
                        out.insert(n.to_string());
                    }
                }
            }
        }
        if depth <= 0 {
            break;
        }
        j += 1;
    }
}

/// name -> (token type, extra modifiers) of the declarations inside one function scope.
fn local_kinds(f: &FileIndex, start: usize, end: usize) -> HashMap<String, (u32, u32)> {
    let mut out = HashMap::new();
    // parameters: `name :` directly inside the parameter parentheses
    let params_close = f.close_of.get(&start).copied().unwrap_or(start);
    for i in start + 1..params_close {
        if let (Some(n), Some(next)) = (f.toks[i].ident(), f.toks.get(i + 1)) {
            let prev = &f.toks[i - 1];
            if next.is(&TokenKind::Colon) && (prev.is(&TokenKind::LParen) || prev.is(&TokenKind::Comma)) {
                out.entry(n.to_string()).or_insert((T_PARAMETER, 0));
            }
        }
    }
    for i in params_close + 1..=end.min(f.toks.len() - 1) {
        let Some(n) = f.toks[i].ident() else { continue };
        let prev = &f.toks[i - 1];
        if let TokenKind::Keyword(k) = &prev.kind {
            match k {
                Keyword::Var => {
                    out.entry(n.to_string()).or_insert((T_VARIABLE, 0));
                }
                Keyword::Const | Keyword::Val | Keyword::Let => {
                    out.entry(n.to_string()).or_insert((T_VARIABLE, M_READONLY));
                }
                Keyword::For => {
                    out.entry(n.to_string()).or_insert((T_VARIABLE, M_READONLY));
                }
                _ => {}
            }
        } else if prev.is(&TokenKind::LParen) && i >= 2 && f.toks[i - 2].kw(Keyword::For) {
            out.entry(n.to_string()).or_insert((T_VARIABLE, M_READONLY));
        } else if f.toks.get(i + 1).is_some_and(|t| t.is(&TokenKind::Colon))
            && (prev.is(&TokenKind::LParen) || prev.is(&TokenKind::Comma))
            && f.struct_literal_key_of(i).is_none()
        {
            out.entry(n.to_string()).or_insert((T_PARAMETER, 0));
        } else if f.toks.get(i + 1).is_some_and(|t| t.is(&TokenKind::FatArrow)) {
            out.entry(n.to_string()).or_insert((T_PARAMETER, 0));
        }
    }
    out
}

pub fn compute(text: &str, uri: &Url, stdlib_types: &HashSet<String>) -> Option<Vec<Abs>> {
    let f = FileIndex::build(uri.clone(), text.to_string())?;
    let mut decl_kinds: HashMap<String, u32> = HashMap::new();
    let mut type_params: HashSet<String> = HashSet::new();
    for d in &f.decls {
        match d.kind {
            crate::navigation::DeclType::Type => {
                decl_kinds.insert(d.name.clone(), type_kind_from_keyword(&f.toks[d.tok - 1].kind));
                generic_params(&f, d.tok + 1, &mut type_params);
            }
            crate::navigation::DeclType::Function | crate::navigation::DeclType::Method => {
                generic_params(&f, d.tok + 1, &mut type_params);
            }
            _ => {}
        }
    }
    let mut locals: HashMap<usize, HashMap<String, (u32, u32)>> = HashMap::new();
    let mut out: Vec<Abs> = Vec::new();
    for (i, tok) in f.toks.iter().enumerate() {
        let Some(name) = tok.ident() else { continue };
        let prev = f.prev(i);
        let next = f.next(i);
        let next_is_call = next.is_some_and(|n| n.is(&TokenKind::LParen));
        let after_dot = prev.is_some_and(|p| p.is(&TokenKind::Dot));
        let after_colons = prev.is_some_and(|p| p.is(&TokenKind::ColonColon));
        let upper = name.chars().next().is_some_and(char::is_uppercase);
        let site = f.decls.iter().find(|d| d.tok == i);

        let (ty, mods) = if prev.is_some_and(|p| p.is(&TokenKind::At)) {
            (T_DECORATOR, 0)
        } else if f.in_import_or_header(i) {
            if f.is_import(i) && upper && !next.is_some_and(|n| n.is(&TokenKind::Dot)) {
                (type_ref_kind(name, &decl_kinds, stdlib_types).0, type_ref_kind(name, &decl_kinds, stdlib_types).1)
            } else {
                (T_NAMESPACE, 0)
            }
        } else if let Some(d) = site {
            let kw = i.checked_sub(1).map(|p| &f.toks[p].kind);
            match d.kind {
                crate::navigation::DeclType::Type => (type_kind_from_keyword(kw.unwrap_or(&TokenKind::Eof)), M_DECLARATION),
                crate::navigation::DeclType::Function => (T_FUNCTION, M_DECLARATION),
                crate::navigation::DeclType::Method => {
                    let st = if matches!(kw, Some(TokenKind::Keyword(Keyword::Fnc))) { M_STATIC } else { 0 };
                    (T_METHOD, M_DECLARATION | st)
                }
                crate::navigation::DeclType::Field => {
                    let ro = if matches!(kw, Some(TokenKind::Keyword(Keyword::Const | Keyword::Val | Keyword::Let))) { M_READONLY } else { 0 };
                    (T_PROPERTY, M_DECLARATION | ro)
                }
            }
        } else if f.struct_literal_key_of(i).is_some() {
            (T_PROPERTY, 0)
        } else if after_dot {
            if next_is_call { (T_METHOD, 0) } else { (T_PROPERTY, 0) }
        } else if after_colons {
            let owner = i.checked_sub(2).and_then(|p| f.toks[p].ident());
            if next_is_call {
                (T_METHOD, M_STATIC)
            } else if owner.is_some_and(|o| decl_kinds.get(o) == Some(&T_ENUM)) {
                (T_ENUM_MEMBER, M_READONLY | M_STATIC)
            } else {
                (T_PROPERTY, M_STATIC)
            }
        } else if let Some(m) = enum_member_decl(&f, i) {
            m
        } else if let Some(func) = f.innermost_fn(i).filter(|func| {
            locals.entry(func.start).or_insert_with(|| local_kinds(&f, func.start, func.end)).contains_key(name)
                || local_decl_count(&f, func, name) > 0
        }) {
            let kinds = locals.entry(func.start).or_insert_with(|| local_kinds(&f, func.start, func.end));
            let (t, m) = kinds.get(name).copied().unwrap_or((T_VARIABLE, 0));
            let decl = prev.is_some_and(|p| {
                matches!(p.kind, TokenKind::Keyword(Keyword::Var | Keyword::Val | Keyword::Let | Keyword::Const | Keyword::For))
            }) || (t == T_PARAMETER && next.is_some_and(|n| n.is(&TokenKind::Colon)) && prev.is_some_and(|p| p.is(&TokenKind::LParen) || p.is(&TokenKind::Comma)));
            (t, m | if decl { M_DECLARATION } else { 0 })
        } else if type_params.contains(name) {
            (T_TYPE_PARAM, 0)
        } else if decl_kinds.contains_key(name) || upper {
            type_ref_kind(name, &decl_kinds, stdlib_types)
        } else if next_is_call {
            (T_FUNCTION, 0)
        } else {
            continue;
        };
        out.push(Abs { line: 0, col: 0, len: name.chars().map(|c| c.len_utf16() as u32).sum(), ty, mods });
        let p = f.position(tok.start);
        let last = out.last_mut().unwrap();
        last.line = p.line;
        last.col = p.character;
    }
    out.sort_by_key(|t| (t.line, t.col));
    out.dedup_by_key(|t| (t.line, t.col));
    Some(out)
}

fn type_ref_kind(name: &str, decl_kinds: &HashMap<String, u32>, stdlib: &HashSet<String>) -> (u32, u32) {
    if let Some(k) = decl_kinds.get(name) {
        (*k, 0)
    } else if BUILTIN_TYPES.contains(&name) || stdlib.contains(name) {
        (T_CLASS, M_DEFAULT_LIBRARY)
    } else {
        (T_CLASS, 0)
    }
}

/// `enum Color { Red, Green(Int64) }`: entries directly in an enum body.
fn enum_member_decl(f: &FileIndex, i: usize) -> Option<(u32, u32)> {
    let prev = f.prev(i)?;
    if !(prev.is(&TokenKind::LBrace) || prev.is(&TokenKind::Comma)) {
        return None;
    }
    let (name, open, close) = f
        .class_ranges
        .iter()
        .filter(|(_, o, c)| *o < i && i < *c)
        .min_by_key(|(_, o, c)| c - o)?;
    let _ = (open, close);
    let decl = f.decls.iter().find(|d| &d.name == name && d.kind == crate::navigation::DeclType::Type)?;
    if f.toks[decl.tok - 1].kw(Keyword::Enum) && f.innermost_class(i).as_deref() == Some(name.as_str()) && !f.inside_fn_body(i) {
        Some((T_ENUM_MEMBER, M_DECLARATION | M_READONLY | M_STATIC))
    } else {
        None
    }
}

/// LSP relative encoding.
pub fn encode(tokens: &[Abs]) -> Vec<SemanticToken> {
    let (mut pl, mut pc) = (0u32, 0u32);
    tokens
        .iter()
        .map(|t| {
            let delta_line = t.line - pl;
            let delta_start = if delta_line == 0 { t.col - pc } else { t.col };
            pl = t.line;
            pc = t.col;
            SemanticToken { delta_line, delta_start, length: t.len, token_type: t.ty, token_modifiers_bitset: t.mods }
        })
        .collect()
}

/// Tokens whose line lies in `[start_line, end_line]`.
pub fn in_range(tokens: &[Abs], start_line: u32, end_line: u32) -> Vec<Abs> {
    tokens.iter().filter(|t| t.line >= start_line && t.line <= end_line).cloned().collect()
}

/// Smallest single edit turning `old` into `new`, in the flat `u32` data
/// representation `semanticTokens/full/delta` uses (5 integers per token).
pub fn delta(old: &[SemanticToken], new: &[SemanticToken]) -> Option<(u32, u32, Vec<SemanticToken>)> {
    let mut pre = 0;
    while pre < old.len() && pre < new.len() && old[pre] == new[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < old.len() - pre && suf < new.len() - pre && old[old.len() - 1 - suf] == new[new.len() - 1 - suf] {
        suf += 1;
    }
    if pre == old.len() && pre == new.len() {
        return None;
    }
    Some((pre as u32 * 5, (old.len() - pre - suf) as u32 * 5, new[pre..new.len() - suf].to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url() -> Url {
        Url::parse("file:///proj/Main.tnx").unwrap()
    }

    fn names(text: &str) -> Vec<(String, u32, u32)> {
        let toks = compute(text, &url(), &HashSet::new()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        toks.iter()
            .map(|t| {
                let line = lines[t.line as usize];
                let s: String = {
                    let mut units = 0;
                    let mut out = String::new();
                    for c in line.chars() {
                        if units >= t.col && units < t.col + t.len {
                            out.push(c);
                        }
                        units += c.len_utf16() as u32;
                    }
                    out
                };
                (s, t.ty, t.mods)
            })
            .collect()
    }

    const SAMPLE: &str = "import tinox.core.fs\n\n@Getter\nclass Box<T>\n{\n    const limit: Int64 = 3\n    var value: T\n\n    fnc make(count: Int64) -> Box<T>\n    {\n        const b = Box { value: count }\n        var total: Int64 = 0\n        total = total + b.value\n        println(\"n=${total}\")\n        return b.size()\n    }\n}\n\nenum Color\n{\n    Red,\n    Green\n}\n";

    fn find<'a>(v: &'a [(String, u32, u32)], name: &str, nth: usize) -> &'a (String, u32, u32) {
        v.iter().filter(|(n, _, _)| n == name).nth(nth).unwrap_or_else(|| panic!("{name}#{nth} missing in {v:?}"))
    }

    #[test]
    fn golden_classification() {
        let v = names(SAMPLE);
        assert_eq!(find(&v, "tinox", 0).1, T_NAMESPACE);
        assert_eq!(find(&v, "Getter", 0).1, T_DECORATOR);
        assert_eq!(*find(&v, "Box", 0), ("Box".into(), T_CLASS, M_DECLARATION));
        assert_eq!(find(&v, "T", 0).1, T_TYPE_PARAM);
        assert_eq!(*find(&v, "limit", 0), ("limit".into(), T_PROPERTY, M_DECLARATION | M_READONLY));
        assert_eq!(*find(&v, "value", 0), ("value".into(), T_PROPERTY, M_DECLARATION));
        assert_eq!(find(&v, "Int64", 0), &("Int64".to_string(), T_CLASS, M_DEFAULT_LIBRARY));
        assert_eq!(*find(&v, "make", 0), ("make".into(), T_METHOD, M_DECLARATION | M_STATIC));
        assert_eq!(*find(&v, "count", 0), ("count".into(), T_PARAMETER, M_DECLARATION));
        assert_eq!(find(&v, "count", 1).1, T_PARAMETER);
        assert_eq!(*find(&v, "b", 0), ("b".into(), T_VARIABLE, M_READONLY | M_DECLARATION));
        assert_eq!(find(&v, "total", 0).2 & M_DECLARATION, M_DECLARATION);
        assert_eq!(find(&v, "total", 1).2 & M_DECLARATION, 0);
        assert_eq!(find(&v, "total", 3).1, T_VARIABLE, "interpolation ${{total}}");
        assert_eq!(find(&v, "size", 0).1, T_METHOD);
        assert_eq!(*find(&v, "Red", 0), ("Red".into(), T_ENUM_MEMBER, M_DECLARATION | M_READONLY | M_STATIC));
        assert_eq!(find(&v, "Color", 0).1, T_ENUM);
    }

    #[test]
    fn encoding_is_relative_and_utf16() {
        let text = "class E\n{\n    fn f() -> String\n    {\n        const s = \"😀\" + t\n        return s\n    }\n}\n";
        let toks = compute(text, &url(), &HashSet::new()).unwrap();
        let enc = encode(&toks);
        let mut line = 0;
        let mut col = 0;
        for (a, e) in toks.iter().zip(&enc) {
            line += e.delta_line;
            col = if e.delta_line == 0 { col + e.delta_start } else { e.delta_start };
            assert_eq!((line, col), (a.line, a.col));
        }
        // `s` after the emoji line is on line 5 at column 15 ("        return " = 15)
        assert!(toks.iter().any(|t| t.line == 5 && t.col == 15 && t.len == 1));
    }

    #[test]
    fn delta_is_a_single_minimal_edit() {
        let a = encode(&compute(SAMPLE, &url(), &HashSet::new()).unwrap());
        assert!(delta(&a, &a).is_none());
        let edited = SAMPLE.replace("count: Int64", "amount: Int64").replace("count }", "amount }");
        let b = encode(&compute(&edited, &url(), &HashSet::new()).unwrap());
        let (start, del, data) = delta(&a, &b).unwrap();
        // applying the edit to the flat data reproduces the new data
        let flat = |v: &[SemanticToken]| -> Vec<u32> {
            v.iter().flat_map(|t| [t.delta_line, t.delta_start, t.length, t.token_type, t.token_modifiers_bitset]).collect()
        };
        let mut fa = flat(&a);
        fa.splice(start as usize..(start + del) as usize, flat(&data));
        assert_eq!(fa, flat(&b));
    }

    #[test]
    fn large_file_stays_within_budget() {
        let mut text = String::from("class Big\n{\n");
        for i in 0..2500 {
            text.push_str(&format!("    fn m{i}(a: Int64) -> Int64\n    {{\n        const x = a + {i}\n        return x\n    }}\n"));
        }
        text.push_str("}\n");
        assert!(text.lines().count() >= 10_000);
        let start = std::time::Instant::now();
        let toks = compute(&text, &url(), &HashSet::new()).unwrap();
        let elapsed = start.elapsed();
        assert!(toks.len() > 10_000);
        // generous bound for an unoptimised test build; release is far faster
        assert!(elapsed < std::time::Duration::from_secs(20), "took {elapsed:?}");
    }
}
