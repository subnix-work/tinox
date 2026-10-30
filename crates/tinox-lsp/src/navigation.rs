//! Project-wide navigation without type information: find references,
//! rename and workspace symbols (issue #491).
//!
//! The analysis is token based. Every `.tnx` file of the project is lexed
//! (string interpolations included), declarations are indexed, and every
//! identifier occurrence is classified by what the language guarantees
//! syntactically:
//!
//! * **Types** (`class`/`interface`/`enum`/`immutable`): one declaration per
//!   project, referenced by bare name.
//! * **Top-level functions**: same, never preceded by `.` or `::`.
//! * **Members** (methods, fields): referenced after `.`/`::` or as a
//!   struct-literal key. Without receiver types a use cannot be tied to its
//!   class, so `references` reports every candidate, and `rename` is only
//!   offered when the name is declared by exactly one project type and
//!   cannot be a standard-library or builtin member.
//! * **Locals and parameters**: scoped to the enclosing function; renamed
//!   only when the function declares the name exactly once.
//!
//! Anything that cannot be shown safe is refused with an explanation rather
//! than guessed ("no silent garbage").

use std::collections::{HashMap, HashSet};
use tinox_lexer::{Keyword, Lexer, TokenKind};
use tower_lsp::lsp_types::{Location, Position, Range, SymbolInformation, SymbolKind, TextEdit, Url};

/// Names that exist on builtin or intrinsic types, so a project member with
/// the same name may be used through them too.
const BUILTIN_MEMBERS: &[&str] = &[
    "len", "push", "pop", "get", "set", "toString", "contains", "substring", "trim", "split", "keys",
    "values", "size", "insert", "remove", "indexOf", "charAt", "chars", "startsWith", "endsWith", "replace",
    "toUpperCase", "toLowerCase", "isEmpty", "join", "map", "filter", "sort", "reverse", "clear", "add",
    "name", "parse", "first", "last", "format", "toInt64", "toFloat64", "clone", "equals", "hashCode",
];

/// Class-level annotations that synthesise members from fields.
const GENERATING_ANNOTATIONS: &[&str] = &[
    "Getter", "Setter", "Data", "Builder", "JsonSerializable", "Entity", "Table", "Immutable",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclType {
    Type,
    Function,
    Method,
    Field,
}

#[derive(Debug, Clone)]
pub(crate) struct Tok {
    pub(crate) kind: TokenKind,
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) nl_before: bool,
}

impl Tok {
    pub(crate) fn ident(&self) -> Option<&str> {
        match &self.kind {
            TokenKind::Ident(s) => Some(s),
            _ => None,
        }
    }
    pub(crate) fn is(&self, k: &TokenKind) -> bool {
        &self.kind == k
    }
    pub(crate) fn kw(&self, k: Keyword) -> bool {
        self.kind == TokenKind::Keyword(k)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DeclSite {
    pub(crate) name: String,
    pub(crate) kind: DeclType,
    pub(crate) container: Option<String>,
    pub(crate) tok: usize,
    /// Class-level annotation names of the declaring class (members only).
    pub(crate) class_annotations: Vec<String>,
    pub(crate) is_extern: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct FnRange {
    pub(crate) name_tok: usize,
    /// First token of the scope (the parameter list's `(`).
    pub(crate) start: usize,
    /// Last token of the scope (body `}` or the parameter list's `)`).
    pub(crate) end: usize,
}

pub(crate) struct FileIndex {
    pub(crate) uri: Url,
    pub(crate) text: String,
    pub(crate) line_starts: Vec<usize>,
    pub(crate) toks: Vec<Tok>,
    pub(crate) decls: Vec<DeclSite>,
    pub(crate) fns: Vec<FnRange>,
    /// Matching bracket index for `(`/`)`, `{`/`}`, `[`/`]`.
    pub(crate) close_of: HashMap<usize, usize>,
    pub(crate) open_of: HashMap<usize, usize>,
    pub(crate) class_ranges: Vec<(String, usize, usize)>,
}

impl FileIndex {
    pub(crate) fn build(uri: Url, text: String) -> Option<Self> {
        let raw = Lexer::new(&text).tokenize_with_trivia().ok()?;
        // The lexer reports character indices; everything here works on bytes.
        let byte_of = char_to_byte(&text);
        let mut toks: Vec<Tok> = Vec::new();
        let mut nl = false;
        for t in raw {
            match &t.kind {
                TokenKind::Newline => nl = true,
                TokenKind::Whitespace | TokenKind::Comment(_) | TokenKind::DocComment(_) | TokenKind::Eof => {}
                TokenKind::InterpString(_) => {
                    let (s, e) = (byte_of[t.span.start.offset as usize], byte_of[t.span.end.offset as usize]);
                    toks.push(Tok { kind: t.kind.clone(), start: s, end: e, nl_before: nl });
                    nl = false;
                    expand_interpolation(&text, s, e, &mut toks);
                }
                _ => {
                    let (s, e) = (byte_of[t.span.start.offset as usize], byte_of[t.span.end.offset as usize]);
                    toks.push(Tok { kind: t.kind, start: s, end: e, nl_before: nl });
                    nl = false;
                }
            }
        }
        let mut line_starts = vec![0usize];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        let mut idx = FileIndex {
            uri,
            text,
            line_starts,
            toks,
            decls: Vec::new(),
            fns: Vec::new(),
            close_of: HashMap::new(),
            open_of: HashMap::new(),
            class_ranges: Vec::new(),
        };
        idx.match_brackets();
        idx.scan_decls();
        Some(idx)
    }

    pub(crate) fn match_brackets(&mut self) {
        let mut stack: Vec<(usize, TokenKind)> = Vec::new();
        for (i, t) in self.toks.iter().enumerate() {
            match t.kind {
                TokenKind::LParen | TokenKind::LBrace | TokenKind::LBracket => stack.push((i, t.kind.clone())),
                TokenKind::RParen | TokenKind::RBrace | TokenKind::RBracket => {
                    let want = match t.kind {
                        TokenKind::RParen => TokenKind::LParen,
                        TokenKind::RBrace => TokenKind::LBrace,
                        _ => TokenKind::LBracket,
                    };
                    if let Some(pos) = stack.iter().rposition(|(_, k)| *k == want) {
                        let (o, _) = stack[pos];
                        stack.truncate(pos);
                        self.close_of.insert(o, i);
                        self.open_of.insert(i, o);
                    }
                }
                _ => {}
            }
        }
    }

    pub(crate) fn annotations_before(&self, kw_idx: usize) -> Vec<String> {
        // `@Name` / `@Name(args)` groups directly in front of a declaration.
        let mut out = Vec::new();
        let mut i = kw_idx;
        while i > 0 {
            let j = i - 1;
            if self.toks[j].is(&TokenKind::RParen) {
                if let Some(&o) = self.open_of.get(&j) {
                    if o >= 2 && self.toks[o - 2].is(&TokenKind::At) {
                        if let Some(n) = self.toks[o - 1].ident() {
                            out.push(n.to_string());
                        }
                        i = o - 2;
                        continue;
                    }
                }
                break;
            }
            if j >= 1 && self.toks[j - 1].is(&TokenKind::At) && self.toks[j].ident().is_some() {
                out.push(self.toks[j].ident().unwrap().to_string());
                i = j - 1;
                continue;
            }
            // access modifiers between annotations and the keyword
            if matches!(
                self.toks[j].kind,
                TokenKind::Keyword(Keyword::Public | Keyword::Private | Keyword::Protected | Keyword::Abstract | Keyword::Final | Keyword::Static)
            ) {
                i = j;
                continue;
            }
            break;
        }
        out
    }

    pub(crate) fn scan_decls(&mut self) {
        let n = self.toks.len();
        // Pass 1: types and their body ranges.
        let mut i = 0;
        while i < n {
            let kind_kw = matches!(
                self.toks[i].kind,
                TokenKind::Keyword(Keyword::Class | Keyword::Interface | Keyword::Enum | Keyword::Trait | Keyword::Immutable)
            );
            if kind_kw && i + 1 < n {
                if let Some(name) = self.toks[i + 1].ident().map(String::from) {
                    let annotations = self.annotations_before(i);
                    self.decls.push(DeclSite {
                        name: name.clone(),
                        kind: DeclType::Type,
                        container: None,
                        tok: i + 1,
                        class_annotations: annotations,
                        is_extern: false,
                    });
                    if !self.toks[i].kw(Keyword::Immutable) {
                        // body: first `{` before the next declaration-ish keyword
                        let mut j = i + 2;
                        while j < n && !self.toks[j].is(&TokenKind::LBrace) {
                            if j > i + 2 && self.toks[j].nl_before && !self.sig_continues(j) {
                                break;
                            }
                            j += 1;
                        }
                        if j < n && self.toks[j].is(&TokenKind::LBrace) {
                            if let Some(&c) = self.close_of.get(&j) {
                                self.class_ranges.push((name, j, c));
                            }
                        }
                    }
                }
            }
            i += 1;
        }
        // Pass 2: functions, then members and extern functions.
        let mut i = 0;
        while i < n {
            let is_fn = matches!(self.toks[i].kind, TokenKind::Keyword(Keyword::Fn | Keyword::Fnc));
            if is_fn && i + 1 < n {
                if let Some(name) = self.toks[i + 1].ident().map(String::from) {
                    // optional generic parameters, then `(`
                    let mut p = i + 2;
                    if p < n && self.toks[p].is(&TokenKind::Less) {
                        let mut depth = 0i32;
                        while p < n {
                            match self.toks[p].kind {
                                TokenKind::Less => depth += 1,
                                TokenKind::Greater => depth -= 1,
                                TokenKind::GreaterGreater => depth -= 2,
                                _ => {}
                            }
                            p += 1;
                            if depth <= 0 {
                                break;
                            }
                        }
                    }
                    if p < n && self.toks[p].is(&TokenKind::LParen) {
                        if let Some(&close) = self.close_of.get(&p) {
                            let after = self.skip_return_type(close + 1);
                            let end = if after < n && self.toks[after].is(&TokenKind::LBrace) {
                                self.close_of.get(&after).copied().unwrap_or(close)
                            } else {
                                close
                            };
                            let is_extern = i > 0 && self.toks[i - 1].kw(Keyword::Extern);
                            let container = self.innermost_class(i);
                            let in_class = container.is_some() && !self.inside_fn_body(i);
                            let kind = if in_class { DeclType::Method } else { DeclType::Function };
                            let class_annotations = container
                                .as_ref()
                                .and_then(|c| self.decls.iter().find(|d| d.kind == DeclType::Type && &d.name == c))
                                .map(|d| d.class_annotations.clone())
                                .unwrap_or_default();
                            self.decls.push(DeclSite {
                                name,
                                kind,
                                container: if in_class { container } else { None },
                                tok: i + 1,
                                class_annotations,
                                is_extern,
                            });
                            self.fns.push(FnRange { name_tok: i + 1, start: p, end });
                        }
                    }
                }
            }
            // fields: `var|val|let|const Name` directly in a class body
            if matches!(
                self.toks[i].kind,
                TokenKind::Keyword(Keyword::Var | Keyword::Val | Keyword::Let | Keyword::Const)
            ) && i + 1 < n
            {
                if let (Some(name), Some(container)) = (self.toks[i + 1].ident().map(String::from), self.innermost_class(i)) {
                    if !self.inside_fn_body(i) {
                        let class_annotations = self
                            .decls
                            .iter()
                            .find(|d| d.kind == DeclType::Type && d.name == container)
                            .map(|d| d.class_annotations.clone())
                            .unwrap_or_default();
                        self.decls.push(DeclSite {
                            name,
                            kind: DeclType::Field,
                            container: Some(container),
                            tok: i + 1,
                            class_annotations,
                            is_extern: false,
                        });
                    }
                }
            }
            i += 1;
        }
    }

    /// Tokens that may continue a class header on a new line.
    fn sig_continues(&self, j: usize) -> bool {
        matches!(
            self.toks[j].kind,
            TokenKind::Keyword(Keyword::Extends | Keyword::Implements) | TokenKind::Comma | TokenKind::LBrace
        ) || (j > 0 && matches!(self.toks[j - 1].kind, TokenKind::Comma | TokenKind::Keyword(Keyword::Extends | Keyword::Implements)))
    }

    /// Index of the token after an optional `-> Type` / `throws X` clause.
    fn skip_return_type(&self, mut i: usize) -> usize {
        let n = self.toks.len();
        let mut angle = 0i32;
        let mut paren = 0i32;
        let mut consumed_any = false;
        while i < n {
            let t = &self.toks[i];
            let allowed = match &t.kind {
                TokenKind::ThinArrow | TokenKind::Keyword(Keyword::Throws) => true,
                TokenKind::Ident(_) | TokenKind::Question | TokenKind::ColonColon | TokenKind::Dot => true,
                TokenKind::Keyword(Keyword::Nothing | Keyword::Never | Keyword::Any | Keyword::Fn | Keyword::Fnc) => true,
                TokenKind::LBracket | TokenKind::RBracket => true,
                TokenKind::Less => {
                    angle += 1;
                    true
                }
                TokenKind::Greater => {
                    angle -= 1;
                    true
                }
                TokenKind::GreaterGreater => {
                    angle -= 2;
                    true
                }
                TokenKind::LParen => {
                    paren += 1;
                    true
                }
                TokenKind::RParen => {
                    paren -= 1;
                    true
                }
                TokenKind::Comma => angle > 0 || paren > 0,
                _ => false,
            };
            if !allowed {
                break;
            }
            // A new line ends the signature unless we are inside <...> / (...)
            // or the line starts the return arrow.
            if consumed_any && t.nl_before && angle <= 0 && paren <= 0 && !matches!(t.kind, TokenKind::ThinArrow) {
                break;
            }
            consumed_any = true;
            i += 1;
        }
        i
    }

    pub(crate) fn innermost_class(&self, tok: usize) -> Option<String> {
        self.class_ranges
            .iter()
            .filter(|(_, o, c)| *o < tok && tok < *c)
            .min_by_key(|(_, o, c)| c - o)
            .map(|(n, _, _)| n.clone())
    }

    pub(crate) fn inside_fn_body(&self, tok: usize) -> bool {
        self.fns.iter().any(|f| f.start < tok && tok <= f.end && self.toks[f.end].is(&TokenKind::RBrace) && f.name_tok != tok)
    }

    pub(crate) fn innermost_fn(&self, tok: usize) -> Option<&FnRange> {
        self.fns
            .iter()
            .filter(|f| f.start <= tok && tok <= f.end)
            .min_by_key(|f| f.end - f.start)
    }

    pub(crate) fn position(&self, byte: usize) -> Position {
        let line = match self.line_starts.binary_search(&byte) {
            Ok(l) => l,
            Err(l) => l - 1,
        };
        let character: usize = self.text[self.line_starts[line]..byte].chars().map(char::len_utf16).sum();
        Position { line: line as u32, character: character as u32 }
    }

    pub(crate) fn range(&self, tok: &Tok) -> Range {
        Range { start: self.position(tok.start), end: self.position(tok.end) }
    }

    pub(crate) fn prev(&self, i: usize) -> Option<&Tok> {
        i.checked_sub(1).map(|p| &self.toks[p])
    }

    pub(crate) fn next(&self, i: usize) -> Option<&Tok> {
        self.toks.get(i + 1)
    }

    pub(crate) fn in_import_or_header(&self, i: usize) -> bool {
        // `import a.b.C` and `namespace`/`module` lines: walk back to line start.
        let mut j = i;
        while j > 0 && !self.toks[j].nl_before {
            j -= 1;
        }
        matches!(self.toks[j].kind, TokenKind::Keyword(Keyword::Import | Keyword::Module | Keyword::Namespace))
    }

    pub(crate) fn is_import(&self, i: usize) -> bool {
        let mut j = i;
        while j > 0 && !self.toks[j].nl_before {
            j -= 1;
        }
        self.toks[j].kw(Keyword::Import)
    }

    /// `Type { key: value, ... }` -- returns the literal's type name when
    /// token `i` is a key of a struct literal.
    pub(crate) fn struct_literal_key_of(&self, i: usize) -> Option<String> {
        let next = self.next(i)?;
        if !next.is(&TokenKind::Colon) {
            return None;
        }
        let prev = self.prev(i)?;
        if !(prev.is(&TokenKind::LBrace) || prev.is(&TokenKind::Comma)) {
            return None;
        }
        // nearest unclosed `{`
        let mut depth = 0i32;
        let mut j = i;
        while j > 0 {
            j -= 1;
            match self.toks[j].kind {
                TokenKind::RBrace | TokenKind::RParen | TokenKind::RBracket => depth += 1,
                TokenKind::LParen | TokenKind::LBracket => {
                    if depth == 0 {
                        return None;
                    }
                    depth -= 1;
                }
                TokenKind::LBrace => {
                    if depth == 0 {
                        let owner = self.prev(j)?;
                        return owner.ident().map(String::from);
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
        None
    }
}

/// `table[i]` is the byte offset of character `i` (one extra entry for the end).
pub(crate) fn char_to_byte(text: &str) -> Vec<usize> {
    let mut v: Vec<usize> = text.char_indices().map(|(b, _)| b).collect();
    v.push(text.len());
    v
}

/// Adds the identifier tokens found inside `${...}` segments of an
/// interpolated string literal as ordinary tokens with absolute offsets.
fn expand_interpolation(text: &str, start: usize, end: usize, toks: &mut Vec<Tok>) {
    let bytes = text.as_bytes();
    let mut i = start;
    while i + 1 < end.min(bytes.len()) {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == b'$' && bytes[i + 1] == b'{' {
            let seg_start = i + 2;
            let mut depth = 1;
            let mut j = seg_start;
            while j < end.min(bytes.len()) && depth > 0 {
                match bytes[j] {
                    b'{' => depth += 1,
                    b'}' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            let seg_end = j.saturating_sub(1);
            if seg_end >= seg_start {
                let segment = &text[seg_start..seg_end];
                let seg_bytes = char_to_byte(segment);
                if let Ok(inner) = Lexer::new(segment).tokenize() {
                    for t in inner {
                        if matches!(t.kind, TokenKind::Eof) {
                            continue;
                        }
                        toks.push(Tok {
                            kind: t.kind,
                            start: seg_start + seg_bytes[t.span.start.offset as usize],
                            end: seg_start + seg_bytes[t.span.end.offset as usize],
                            nl_before: false,
                        });
                    }
                }
            }
            i = j;
            continue;
        }
        i += 1;
    }
}

pub struct Ctx {
    /// Member names (methods/fields) of standard-library classes.
    pub stdlib_member_names: HashSet<String>,
}

fn build_all(files: &[(Url, String)]) -> Vec<FileIndex> {
    files.iter().filter_map(|(u, t)| FileIndex::build(u.clone(), t.clone())).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Sym {
    Type(String),
    Function(String),
    Member(String),
    Local { name: String, fn_start: usize, file: usize },
}

struct Target<'a> {
    sym: Sym,
    file: usize,
    tok: usize,
    files: &'a [FileIndex],
}

fn resolve<'a>(files: &'a [FileIndex], at: &Url, offset: usize) -> Result<Target<'a>, String> {
    let fi = files.iter().position(|f| &f.uri == at).ok_or("file is not part of the analysed project or has lexical errors")?;
    let f = &files[fi];
    let tok = f
        .toks
        .iter()
        .position(|t| t.ident().is_some() && t.start <= offset && offset <= t.end)
        .ok_or("no identifier at this position")?;
    let name = f.toks[tok].ident().unwrap().to_string();
    let declared_types: Vec<&DeclSite> = files.iter().flat_map(|x| x.decls.iter()).filter(|d| d.kind == DeclType::Type && d.name == name).collect();
    let prev = f.prev(tok);
    let after_access = prev.is_some_and(|p| p.is(&TokenKind::Dot) || p.is(&TokenKind::ColonColon));
    // import path segments naming a project type
    if f.is_import(tok) && !declared_types.is_empty() {
        return Ok(Target { sym: Sym::Type(name), file: fi, tok, files });
    }
    if !after_access && !declared_types.is_empty() {
        return Ok(Target { sym: Sym::Type(name), file: fi, tok, files });
    }
    // declaration sites of members on this very token
    if f.decls.iter().any(|d| d.tok == tok && matches!(d.kind, DeclType::Method | DeclType::Field)) {
        return Ok(Target { sym: Sym::Member(name), file: fi, tok, files });
    }
    if f.decls.iter().any(|d| d.tok == tok && d.kind == DeclType::Function) {
        return Ok(Target { sym: Sym::Function(name), file: fi, tok, files });
    }
    if after_access || f.struct_literal_key_of(tok).is_some() {
        return Ok(Target { sym: Sym::Member(name), file: fi, tok, files });
    }
    // bare identifier: local/parameter of the enclosing function, or a top-level function
    if let Some(func) = f.innermost_fn(tok) {
        if local_decl_count(f, func, &name) > 0 {
            return Ok(Target { sym: Sym::Local { name, fn_start: func.start, file: fi }, file: fi, tok, files });
        }
    }
    if files.iter().flat_map(|x| x.decls.iter()).any(|d| d.kind == DeclType::Function && d.name == name) {
        return Ok(Target { sym: Sym::Function(name), file: fi, tok, files });
    }
    Err(format!("`{name}` is not declared in this project (standard library, builtin or unresolved)"))
}

/// Number of declarations of `name` inside a function scope: parameters,
/// `var/val/let/const`, `for x in`, `catch (x:`, and lambda parameters.
pub(crate) fn local_decl_count(f: &FileIndex, func: &FnRange, name: &str) -> usize {
    let mut count = 0;
    for i in func.start..=func.end.min(f.toks.len() - 1) {
        if f.toks[i].ident() != Some(name) {
            continue;
        }
        let prev = f.prev(i);
        let next = f.next(i);
        let decl_kw = prev.is_some_and(|p| {
            matches!(p.kind, TokenKind::Keyword(Keyword::Var | Keyword::Val | Keyword::Let | Keyword::Const))
        });
        let for_var = prev.is_some_and(|p| p.kw(Keyword::For)) && next.is_some_and(|n| n.kw(Keyword::In));
        let for_paren = prev.is_some_and(|p| p.is(&TokenKind::LParen))
            && i >= 2
            && f.toks[i - 2].kw(Keyword::For)
            && next.is_some_and(|n| n.kw(Keyword::In));
        let typed_param = next.is_some_and(|n| n.is(&TokenKind::Colon))
            && prev.is_some_and(|p| p.is(&TokenKind::LParen) || p.is(&TokenKind::Comma))
            && f.struct_literal_key_of(i).is_none();
        let lambda_param = next.is_some_and(|n| n.is(&TokenKind::FatArrow))
            && prev.is_some_and(|p| p.is(&TokenKind::LParen) || p.is(&TokenKind::Comma) || p.is(&TokenKind::Equals));
        if decl_kw || for_var || for_paren || typed_param || lambda_param {
            count += 1;
        }
    }
    count
}

struct Occurrence {
    file: usize,
    tok: usize,
    is_decl: bool,
}

fn collect(t: &Target, ctx: &Ctx) -> (Vec<Occurrence>, Vec<String>) {
    let files = t.files;
    let mut out = Vec::new();
    let mut notes = Vec::new();
    match &t.sym {
        Sym::Type(name) => {
            for (fi, f) in files.iter().enumerate() {
                for (i, tok) in f.toks.iter().enumerate() {
                    if tok.ident() != Some(name) {
                        continue;
                    }
                    let after_access = f.prev(i).is_some_and(|p| p.is(&TokenKind::Dot));
                    if after_access && !f.is_import(i) {
                        continue;
                    }
                    if f.in_import_or_header(i) && !f.is_import(i) {
                        continue;
                    }
                    let is_decl = f.decls.iter().any(|d| d.tok == i && d.kind == DeclType::Type);
                    out.push(Occurrence { file: fi, tok: i, is_decl });
                }
            }
        }
        Sym::Function(name) => {
            for (fi, f) in files.iter().enumerate() {
                for (i, tok) in f.toks.iter().enumerate() {
                    if tok.ident() != Some(name) {
                        continue;
                    }
                    if f.prev(i).is_some_and(|p| p.is(&TokenKind::Dot) || p.is(&TokenKind::ColonColon)) {
                        continue;
                    }
                    if f.struct_literal_key_of(i).is_some() {
                        continue;
                    }
                    let is_decl = f.decls.iter().any(|d| d.tok == i && d.kind == DeclType::Function);
                    out.push(Occurrence { file: fi, tok: i, is_decl });
                }
            }
        }
        Sym::Member(name) => {
            let owners: Vec<String> = {
                let mut v: Vec<String> = files
                    .iter()
                    .flat_map(|x| x.decls.iter())
                    .filter(|d| matches!(d.kind, DeclType::Method | DeclType::Field) && &d.name == name)
                    .filter_map(|d| d.container.clone())
                    .collect();
                v.sort();
                v.dedup();
                v
            };
            notes = owners;
            let _ = ctx;
            for (fi, f) in files.iter().enumerate() {
                for (i, tok) in f.toks.iter().enumerate() {
                    if tok.ident() != Some(name) {
                        continue;
                    }
                    let access = f.prev(i).is_some_and(|p| p.is(&TokenKind::Dot) || p.is(&TokenKind::ColonColon));
                    let is_decl = f
                        .decls
                        .iter()
                        .any(|d| d.tok == i && matches!(d.kind, DeclType::Method | DeclType::Field));
                    let key = f.struct_literal_key_of(i).is_some_and(|lit| notes.contains(&lit));
                    if access || is_decl || key {
                        out.push(Occurrence { file: fi, tok: i, is_decl });
                    }
                }
            }
        }
        Sym::Local { name, fn_start, file } => {
            let f = &files[*file];
            if let Some(func) = f.fns.iter().find(|x| x.start == *fn_start) {
                for i in func.start..=func.end.min(f.toks.len() - 1) {
                    if f.toks[i].ident() != Some(name) {
                        continue;
                    }
                    if f.prev(i).is_some_and(|p| p.is(&TokenKind::Dot) || p.is(&TokenKind::ColonColon)) {
                        continue;
                    }
                    if f.struct_literal_key_of(i).is_some() {
                        continue;
                    }
                    let prev = f.prev(i);
                    let is_decl = prev.is_some_and(|p| {
                        matches!(p.kind, TokenKind::Keyword(Keyword::Var | Keyword::Val | Keyword::Let | Keyword::Const))
                    }) || f.next(i).is_some_and(|n| n.is(&TokenKind::Colon))
                        && prev.is_some_and(|p| p.is(&TokenKind::LParen) || p.is(&TokenKind::Comma));
                    out.push(Occurrence { file: *file, tok: i, is_decl });
                }
            }
        }
    }
    (out, notes)
}

pub fn references(files: &[(Url, String)], at: &Url, offset: usize, include_declaration: bool, ctx: &Ctx) -> Result<Vec<Location>, String> {
    let idx = build_all(files);
    let target = resolve(&idx, at, offset)?;
    let (occ, _) = collect(&target, ctx);
    Ok(occ
        .into_iter()
        .filter(|o| include_declaration || !o.is_decl)
        .map(|o| {
            let f = &idx[o.file];
            Location { uri: f.uri.clone(), range: f.range(&f.toks[o.tok]) }
        })
        .collect())
}

pub struct PreparedRename {
    pub range: Range,
    pub placeholder: String,
}

pub fn prepare_rename(files: &[(Url, String)], at: &Url, offset: usize, ctx: &Ctx) -> Result<PreparedRename, String> {
    let idx = build_all(files);
    let target = resolve(&idx, at, offset)?;
    check_renamable(&target, ctx)?;
    let f = &idx[target.file];
    let tok = &f.toks[target.tok];
    Ok(PreparedRename { range: f.range(tok), placeholder: tok.ident().unwrap().to_string() })
}

fn check_renamable(t: &Target, ctx: &Ctx) -> Result<(), String> {
    let files = t.files;
    match &t.sym {
        Sym::Type(name) => {
            let sites: Vec<&DeclSite> = files.iter().flat_map(|f| f.decls.iter()).filter(|d| d.kind == DeclType::Type && &d.name == name).collect();
            if sites.len() != 1 {
                return Err(format!("`{name}` is declared {} times in the project; rename would be ambiguous", sites.len()));
            }
        }
        Sym::Function(name) => {
            let sites: Vec<&DeclSite> = files.iter().flat_map(|f| f.decls.iter()).filter(|d| d.kind == DeclType::Function && &d.name == name).collect();
            if sites.len() != 1 {
                return Err(format!("function `{name}` is declared {} times in the project; rename would be ambiguous", sites.len()));
            }
            if sites[0].is_extern {
                return Err(format!("`{name}` is an extern fn bound to a C symbol; it cannot be renamed"));
            }
        }
        Sym::Member(name) => {
            let sites: Vec<&DeclSite> = files
                .iter()
                .flat_map(|f| f.decls.iter())
                .filter(|d| matches!(d.kind, DeclType::Method | DeclType::Field) && &d.name == name)
                .collect();
            let mut owners: Vec<&str> = sites.iter().filter_map(|d| d.container.as_deref()).collect();
            owners.sort();
            owners.dedup();
            if sites.is_empty() {
                return Err(format!("member `{name}` is not declared in this project (standard library or unresolved)"));
            }
            if owners.len() > 1 {
                return Err(format!(
                    "member `{name}` is declared by {} types ({}); without receiver types a rename could touch the wrong one",
                    owners.len(),
                    owners.join(", ")
                ));
            }
            if sites.len() > 1 {
                return Err(format!("member `{name}` is declared {} times in `{}` (overloads); rename would be ambiguous", sites.len(), owners[0]));
            }
            if ctx.stdlib_member_names.contains(name.as_str()) || BUILTIN_MEMBERS.contains(&name.as_str()) {
                return Err(format!("`{name}` is also a standard-library or builtin member name; uses of it cannot be told apart without type information"));
            }
            if sites[0].kind == DeclType::Field {
                if let Some(a) = sites[0].class_annotations.iter().find(|a| GENERATING_ANNOTATIONS.contains(&a.as_str())) {
                    return Err(format!("`{name}` belongs to a class annotated @{a}, which generates members from field names; rename it by hand"));
                }
            }
        }
        Sym::Local { name, fn_start, file } => {
            let f = &files[*file];
            let func = f.fns.iter().find(|x| x.start == *fn_start).ok_or("function scope not found")?;
            let n = local_decl_count(f, func, name);
            if n != 1 {
                return Err(format!("`{name}` is declared {n} times in this function (shadowing); rename would be ambiguous"));
            }
        }
    }
    Ok(())
}

pub struct RenameResult {
    pub edits: HashMap<Url, Vec<TextEdit>>,
    /// (old, new) file URIs when a type's file must follow its name.
    pub file_rename: Option<(Url, Url)>,
}

pub fn rename(files: &[(Url, String)], at: &Url, offset: usize, new_name: &str, ctx: &Ctx) -> Result<RenameResult, String> {
    if new_name.is_empty() || !new_name.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_') || !new_name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Err(format!("`{new_name}` is not a valid identifier"));
    }
    if Keyword::lookup(new_name).is_some() {
        return Err(format!("`{new_name}` is a keyword"));
    }
    let idx = build_all(files);
    let target = resolve(&idx, at, offset)?;
    check_renamable(&target, ctx)?;
    let old = match &target.sym {
        Sym::Type(n) | Sym::Function(n) | Sym::Member(n) | Sym::Local { name: n, .. } => n.clone(),
    };
    if old == new_name {
        return Err("the new name equals the old name".into());
    }
    // The new name must not collide with something visible in the same scope.
    match &target.sym {
        Sym::Type(_) => {
            if idx.iter().flat_map(|f| f.decls.iter()).any(|d| d.kind == DeclType::Type && d.name == new_name) {
                return Err(format!("a type named `{new_name}` already exists"));
            }
        }
        Sym::Function(_) => {
            if idx.iter().flat_map(|f| f.decls.iter()).any(|d| d.kind == DeclType::Function && d.name == new_name) {
                return Err(format!("a function named `{new_name}` already exists"));
            }
        }
        Sym::Member(_) => {
            let owner = idx
                .iter()
                .flat_map(|f| f.decls.iter())
                .find(|d| matches!(d.kind, DeclType::Method | DeclType::Field) && d.name == old)
                .and_then(|d| d.container.clone());
            if idx.iter().flat_map(|f| f.decls.iter()).any(|d| matches!(d.kind, DeclType::Method | DeclType::Field) && d.name == new_name && d.container == owner) {
                return Err(format!("`{}` already has a member named `{new_name}`", owner.unwrap_or_default()));
            }
        }
        Sym::Local { fn_start, file, .. } => {
            let f = &idx[*file];
            let func = f.fns.iter().find(|x| x.start == *fn_start).unwrap();
            if local_decl_count(f, func, new_name) > 0 {
                return Err(format!("`{new_name}` is already declared in this function"));
            }
        }
    }
    let (occ, _) = collect(&target, ctx);
    let mut edits: HashMap<Url, Vec<TextEdit>> = HashMap::new();
    for o in &occ {
        let f = &idx[o.file];
        edits.entry(f.uri.clone()).or_default().push(TextEdit { range: f.range(&f.toks[o.tok]), new_text: new_name.to_string() });
    }
    let mut file_rename = None;
    if let Sym::Type(_) = &target.sym {
        // One type per file: the file name must follow the class name.
        if let Some(decl_occ) = occ.iter().find(|o| o.is_decl) {
            let f = &idx[decl_occ.file];
            if let Ok(path) = f.uri.to_file_path() {
                if path.file_stem().and_then(|s| s.to_str()) == Some(old.as_str()) {
                    let new_path = path.with_file_name(format!("{new_name}.tnx"));
                    if new_path.exists() {
                        return Err(format!("{} already exists", new_path.display()));
                    }
                    if let Ok(new_uri) = Url::from_file_path(&new_path) {
                        file_rename = Some((f.uri.clone(), new_uri));
                    }
                }
            }
        }
    }
    Ok(RenameResult { edits, file_rename })
}

/// Fuzzy (case-insensitive subsequence) match.
fn fuzzy(query: &str, name: &str) -> bool {
    let mut q = query.chars().flat_map(char::to_lowercase);
    let mut cur = q.next();
    for c in name.chars().flat_map(char::to_lowercase) {
        if Some(c) == cur {
            cur = q.next();
        }
    }
    cur.is_none()
}

#[allow(deprecated)]
pub fn workspace_symbols(files: &[(Url, String)], query: &str) -> Vec<SymbolInformation> {
    let idx = build_all(files);
    let mut out = Vec::new();
    for f in &idx {
        for d in &f.decls {
            if !fuzzy(query, &d.name) {
                continue;
            }
            let kind = match d.kind {
                DeclType::Type => SymbolKind::CLASS,
                DeclType::Function => SymbolKind::FUNCTION,
                DeclType::Method => SymbolKind::METHOD,
                DeclType::Field => SymbolKind::FIELD,
            };
            out.push(SymbolInformation {
                name: d.name.clone(),
                kind,
                tags: None,
                deprecated: None,
                location: Location { uri: f.uri.clone(), range: f.range(&f.toks[d.tok]) },
                container_name: d.container.clone(),
            });
        }
    }
    // exact and prefix matches first, then by name
    out.sort_by_key(|s| (!s.name.eq_ignore_ascii_case(query), !s.name.to_lowercase().starts_with(&query.to_lowercase()), s.name.clone()));
    out.truncate(500);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(p: &str) -> Url {
        Url::parse(&format!("file:///proj/{p}")).unwrap()
    }

    fn ctx() -> Ctx {
        Ctx { stdlib_member_names: HashSet::new() }
    }

    fn offset_of(text: &str, needle: &str, nth: usize) -> usize {
        text.match_indices(needle).nth(nth).unwrap().0 + 1
    }

    fn apply(files: &[(Url, String)], res: &RenameResult) -> HashMap<Url, String> {
        let mut out = HashMap::new();
        for (u, t) in files {
            let mut edits = res.edits.get(u).cloned().unwrap_or_default();
            let line_starts: Vec<usize> = std::iter::once(0).chain(t.match_indices('\n').map(|(i, _)| i + 1)).collect();
            let to_byte = |p: Position| {
                let ls = line_starts[p.line as usize];
                let mut units = 0;
                for (i, c) in t[ls..].char_indices() {
                    if units == p.character as usize {
                        return ls + i;
                    }
                    units += c.len_utf16();
                }
                t.len()
            };
            edits.sort_by_key(|e| std::cmp::Reverse((e.range.start.line, e.range.start.character)));
            let mut s = t.clone();
            for e in edits {
                let (a, b) = (to_byte(e.range.start), to_byte(e.range.end));
                s.replace_range(a..b, &e.new_text);
            }
            out.insert(u.clone(), s);
        }
        out
    }

    const PERSON: &str = "namespace demo.model\n{\n    class Person\n    {\n        var id: Int64\n        var label: String\n\n        fn describe() -> String\n        {\n            return \"p${this.id}\"\n        }\n    }\n}\n";
    const SERVICE: &str = "import demo.model.Person\n\nnamespace demo.service\n{\n    class Service\n    {\n        fn make(count: Int64) -> Person\n        {\n            const p = Person { id: count, label: \"x\" }\n            var total: Int64 = count\n            total = total + p.id\n            println(\"n=${count}\")\n            return p\n        }\n    }\n}\n";

    fn project() -> Vec<(Url, String)> {
        vec![(url("demo/model/Person.tnx"), PERSON.into()), (url("demo/service/Service.tnx"), SERVICE.into())]
    }

    #[test]
    fn type_references_include_import_declaration_and_uses() {
        let files = project();
        let at = url("demo/model/Person.tnx");
        let refs = references(&files, &at, offset_of(PERSON, "Person", 0), true, &ctx()).unwrap();
        // declaration + import + return type + struct literal
        assert_eq!(refs.len(), 4, "{refs:?}");
        let no_decl = references(&files, &at, offset_of(PERSON, "Person", 0), false, &ctx()).unwrap();
        assert_eq!(no_decl.len(), 3);
    }

    #[test]
    fn rename_type_updates_all_files_and_proposes_file_rename() {
        let files = project();
        let at = url("demo/model/Person.tnx");
        let res = rename(&files, &at, offset_of(PERSON, "Person", 0), "Human", &ctx()).unwrap();
        let out = apply(&files, &res);
        let svc = &out[&url("demo/service/Service.tnx")];
        assert!(svc.contains("import demo.model.Human"));
        assert!(svc.contains("-> Human") && svc.contains("Human { id: count"));
        assert!(!svc.contains("Person"));
        assert!(out[&at].contains("class Human"));
        // one type per file: the file follows the type name
        assert_eq!(res.file_rename, Some((url("demo/model/Person.tnx"), url("demo/model/Human.tnx"))));
    }

    #[test]
    fn rename_unique_field_updates_declaration_access_literal_key_and_interpolation() {
        let files = project();
        let at = url("demo/model/Person.tnx");
        let res = rename(&files, &at, offset_of(PERSON, "label", 0), "title", &ctx()).unwrap();
        let out = apply(&files, &res);
        assert!(out[&at].contains("var title: String"));
        assert!(out[&url("demo/service/Service.tnx")].contains("title: \"x\""));
        let res = rename(&files, &at, offset_of(PERSON, "id", 0), "key", &ctx()).unwrap();
        let out = apply(&files, &res);
        assert!(out[&at].contains("var key: Int64"));
        assert!(out[&at].contains("p${this.key}"), "interpolation not renamed: {}", out[&at]);
        let svc = &out[&url("demo/service/Service.tnx")];
        assert!(svc.contains("Person { key: count") && svc.contains("p.key"));
    }

    #[test]
    fn rename_local_covers_interpolation_and_leaves_struct_keys() {
        let files = project();
        let at = url("demo/service/Service.tnx");
        let res = rename(&files, &at, offset_of(SERVICE, "count", 0), "amount", &ctx()).unwrap();
        let out = apply(&files, &res);
        let svc = &out[&at];
        assert!(svc.contains("make(amount: Int64)"));
        assert!(svc.contains("id: amount, label"), "struct key must stay, value renamed: {svc}");
        assert!(svc.contains("var total: Int64 = amount"));
        assert!(svc.contains("n=${amount}"));
        assert!(!svc.contains("count"));
    }

    #[test]
    fn rename_refuses_ambiguous_shadowed_stdlib_and_generated_members() {
        let a = "namespace x\n{\n    class A\n    {\n        fn run() -> Int64\n        {\n            return 1\n        }\n        fn size() -> Int64\n        {\n            return 2\n        }\n    }\n}\n";
        let b = "namespace x\n{\n    class B\n    {\n        fn run() -> Int64\n        {\n            return 3\n        }\n    }\n}\n";
        let files = vec![(url("x/A.tnx"), a.to_string()), (url("x/B.tnx"), b.to_string())];
        let at = url("x/A.tnx");
        let err = rename(&files, &at, offset_of(a, "run", 0), "go", &ctx()).err().unwrap();
        assert!(err.contains("declared by 2 types"), "{err}");
        // references still list every candidate
        assert_eq!(references(&files, &at, offset_of(a, "run", 0), true, &ctx()).unwrap().len(), 2);
        let err = rename(&files, &at, offset_of(a, "size", 0), "count", &ctx()).err().unwrap();
        assert!(err.contains("builtin member name"), "{err}");

        let shadow = "namespace x\n{\n    class C\n    {\n        fn f(v: Int64) -> Int64\n        {\n            const g = (v: Int64) => v\n            return v\n        }\n    }\n}\n";
        let files = vec![(url("x/C.tnx"), shadow.to_string())];
        let err = rename(&files, &url("x/C.tnx"), offset_of(shadow, "v:", 0), "w", &ctx()).err().unwrap();
        assert!(err.contains("shadowing"), "{err}");

        let gen = "@Getter\nclass D\n{\n    var secret: Int64\n}\n";
        let files = vec![(url("D.tnx"), gen.to_string())];
        let err = rename(&files, &url("D.tnx"), offset_of(gen, "secret", 0), "hidden", &ctx()).err().unwrap();
        assert!(err.contains("@Getter"), "{err}");
    }

    #[test]
    fn rename_rejects_invalid_names_keywords_and_collisions() {
        let files = project();
        let at = url("demo/model/Person.tnx");
        let off = offset_of(PERSON, "label", 0);
        assert!(rename(&files, &at, off, "1bad", &ctx()).err().unwrap().contains("not a valid identifier"));
        assert!(rename(&files, &at, off, "class", &ctx()).err().unwrap().contains("keyword"));
        assert!(rename(&files, &at, off, "id", &ctx()).err().unwrap().contains("already has a member"));
        assert!(prepare_rename(&files, &at, offset_of(PERSON, "Int64", 0), &ctx()).is_err());
    }

    #[test]
    fn workspace_symbols_are_fuzzy_and_carry_containers() {
        let syms = workspace_symbols(&project(), "desc");
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "describe");
        assert_eq!(syms[0].container_name.as_deref(), Some("Person"));
        let syms = workspace_symbols(&project(), "pers");
        assert!(syms.iter().any(|s| s.name == "Person" && s.kind == SymbolKind::CLASS));
        assert!(workspace_symbols(&project(), "zzz").is_empty());
    }

    #[test]
    fn positions_are_utf16() {
        let src = "namespace x\n{\n    class E\n    {\n        fn f() -> String\n        {\n            const s = \"😀\"\n            return s\n        }\n    }\n}\n";
        let files = vec![(url("x/E.tnx"), src.to_string())];
        let refs = references(&files, &url("x/E.tnx"), offset_of(src, "s =", 0), true, &ctx()).unwrap();
        assert_eq!(refs.len(), 2);
        let ret = refs.iter().find(|l| l.range.start.line == 7).unwrap();
        assert_eq!(ret.range.start.character, 19);
    }
}

/// Replace the AST's zero-width symbol ranges with real extents (#577): `range`
/// covers the declaration from its keyword through the closing brace (or the
/// end of its line when it has no body), `selectionRange` the identifier.
/// Symbols the token scan cannot match keep their original ranges.
pub fn refine_document_symbols(syms: &mut [tower_lsp::lsp_types::DocumentSymbol], uri: &Url, text: &str) {
    let Some(idx) = FileIndex::build(uri.clone(), text.to_string()) else { return };
    let mut used = HashSet::new();
    refine_level(syms, None, &idx, &mut used);
}

fn kind_of_symbol(kind: SymbolKind) -> Option<DeclType> {
    if kind == SymbolKind::CLASS || kind == SymbolKind::ENUM || kind == SymbolKind::INTERFACE || kind == SymbolKind::STRUCT {
        Some(DeclType::Type)
    } else if kind == SymbolKind::FUNCTION {
        Some(DeclType::Function)
    } else if kind == SymbolKind::METHOD {
        Some(DeclType::Method)
    } else if kind == SymbolKind::FIELD {
        Some(DeclType::Field)
    } else {
        None
    }
}

fn refine_level(
    syms: &mut [tower_lsp::lsp_types::DocumentSymbol],
    parent: Option<&str>,
    idx: &FileIndex,
    used: &mut HashSet<usize>,
) {
    for sym in syms.iter_mut() {
        let owner = parent.map(String::from);
        if let Some(kind) = kind_of_symbol(sym.kind) {
            let found = idx.decls.iter().enumerate().find(|(i, d)| {
                !used.contains(i) && d.name == sym.name && d.kind == kind && d.container == owner
            });
            if let Some((i, d)) = found {
                used.insert(i);
                let name_tok = d.tok;
                let first = name_tok.saturating_sub(1);
                let last = match kind {
                    DeclType::Type => idx
                        .class_ranges
                        .iter()
                        .find(|(n, o, _)| *n == sym.name && *o > name_tok)
                        .map(|(_, _, c)| *c)
                        .unwrap_or(name_tok),
                    DeclType::Function | DeclType::Method => idx
                        .fns
                        .iter()
                        .find(|f| f.name_tok == name_tok)
                        .map(|f| f.end)
                        .unwrap_or(name_tok),
                    DeclType::Field => {
                        let mut e = name_tok;
                        while e + 1 < idx.toks.len() && !idx.toks[e + 1].nl_before && !idx.toks[e + 1].is(&TokenKind::RBrace) {
                            e += 1;
                        }
                        e
                    }
                };
                sym.selection_range = idx.range(&idx.toks[name_tok]);
                sym.range = Range { start: idx.position(idx.toks[first].start), end: idx.position(idx.toks[last].end) };
            }
        } else if sym.kind == SymbolKind::ENUM_MEMBER {
            if let Some(tok) = parent.and_then(|p| {
                let (_, o, c) = idx.class_ranges.iter().find(|(n, _, _)| n == p)?;
                (*o + 1..*c).find(|&t| idx.toks[t].ident() == Some(sym.name.as_str()))
            }) {
                sym.selection_range = idx.range(&idx.toks[tok]);
                sym.range = sym.selection_range;
            }
        }
        let name = sym.name.clone();
        if let Some(children) = sym.children.as_mut() {
            refine_level(children, Some(&name), idx, used);
        }
    }
}

#[cfg(test)]
mod symbol_range_tests {
    use super::*;

    #[test]
    fn symbols_get_full_extents_and_identifier_selections() {
        let text = "class Caller\n{\n    fnc run() -> Int64\n    {\n        return 1\n    }\n}\n";
        let tokens = tinox_lexer::Lexer::new(text).tokenize().unwrap();
        let ast = tinox_parser::Parser::new(tokens).parse().unwrap();
        let mut syms = crate::analysis::document_symbols(&ast);
        let uri = Url::parse("file:///tmp/Caller.tnx").unwrap();
        refine_document_symbols(&mut syms, &uri, text);
        let class = &syms[0];
        assert_eq!((class.range.start.line, class.range.end.line, class.range.end.character), (0, 6, 1));
        assert_eq!((class.selection_range.start.character, class.selection_range.end.character), (6, 12));
        let run = &class.children.as_ref().unwrap()[0];
        assert_eq!((run.range.start.line, run.range.end.line), (2, 5));
        assert_eq!((run.selection_range.start.line, run.selection_range.start.character, run.selection_range.end.character), (2, 8, 11));
    }
}
