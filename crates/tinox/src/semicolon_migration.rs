//! One-shot migration for issue #445: strips statement semicolons from
//! Tinox sources (`tinox migrate-semicolons [--check] <path>...`).
//!
//! Works on the token stream of the old (semicolon) syntax:
//! - a `;` (or, directly inside an `enum` body, a `,`) that ends a statement is
//!   deleted when nothing but a comment follows on its line, and replaced by a
//!   line break when another statement follows on the same line;
//! - `for ( init ; cond ; update )` header separators are kept;
//! - an operator that starts a continuation line (`\n  && x`) is moved to the
//!   end of the previous line, because under the new rules only a trailing
//!   operator continues a statement.

use std::path::{Path, PathBuf};

use std::sync::atomic::{AtomicBool, Ordering};

use tinox_lexer::{Keyword, Lexer, Token, TokenKind};

/// `--force`: embedded literals need no statement keyword (fragments such as `println(x);`).
static FORCE: AtomicBool = AtomicBool::new(false);

struct Edit {
    start: usize,
    end: usize,
    text: String,
}

#[derive(PartialEq, Clone, Copy)]
enum Ctx {
    Brace,
    EnumBrace,
    Paren,
    ForHeader,
}

fn is_continuation_op(k: &TokenKind) -> bool {
    matches!(
        k,
        TokenKind::Plus
            | TokenKind::Minus
            | TokenKind::Star
            | TokenKind::Slash
            | TokenKind::Percent
            | TokenKind::EqualsEquals
            | TokenKind::BangEquals
            | TokenKind::AmpAmp
            | TokenKind::BarBar
            | TokenKind::Bar
            | TokenKind::Ampersand
            | TokenKind::Caret
            | TokenKind::LessLess
            | TokenKind::GreaterGreater
            | TokenKind::Question
            | TokenKind::QuestionQuestion
            | TokenKind::QuestionColon
            | TokenKind::Colon
            | TokenKind::Equals
            | TokenKind::PlusEquals
            | TokenKind::MinusEquals
    )
}

fn op_text(k: &TokenKind) -> &'static str {
    match k {
        TokenKind::Plus => "+",
        TokenKind::Minus => "-",
        TokenKind::Star => "*",
        TokenKind::Slash => "/",
        TokenKind::Percent => "%",
        TokenKind::EqualsEquals => "==",
        TokenKind::BangEquals => "!=",
        TokenKind::AmpAmp => "&&",
        TokenKind::BarBar => "||",
        TokenKind::Bar => "|",
        TokenKind::Ampersand => "&",
        TokenKind::Caret => "^",
        TokenKind::LessLess => "<<",
        TokenKind::GreaterGreater => ">>",
        TokenKind::Question => "?",
        TokenKind::QuestionQuestion => "??",
        TokenKind::QuestionColon => "?:",
        TokenKind::Colon => ":",
        TokenKind::Equals => "=",
        TokenKind::PlusEquals => "+=",
        TokenKind::MinusEquals => "-=",
        _ => unreachable!(),
    }
}

/// Convert one source text. Returns the new text, or an error when the file
/// cannot be lexed.
pub fn migrate_source(src: &str) -> Result<String, String> {
    let chars: Vec<char> = src.chars().collect();
    let edits = compute_edits(src, false)?;
    apply_edits(&chars, edits)
}

fn apply_edits(chars: &[char], mut edits: Vec<Edit>) -> Result<String, String> {
    edits.sort_by_key(|e| (e.start, e.end));
    for w in edits.windows(2) {
        if w[0].end > w[1].start {
            return Err("overlapping edits".to_string());
        }
    }
    let mut out = String::with_capacity(chars.len());
    let mut cursor = 0usize;
    for e in &edits {
        out.extend(&chars[cursor..e.start]);
        out.push_str(&e.text);
        cursor = e.end;
    }
    out.extend(&chars[cursor..]);
    Ok(out)
}

/// `embedded`: the text is a string literal of a Rust file; only touch it when it
/// really looks like Tinox code (lexes cleanly, has a statement keyword and a `;`).
fn compute_edits(src: &str, embedded: bool) -> Result<Vec<Edit>, String> {
    let chars: Vec<char> = src.chars().collect();
    let tokens: Vec<Token> = Lexer::new(src)
        .tokenize()
        .map_err(|e| format!("lex error: {:?}", e.first().map(|e| e.message.clone())))?;
    if embedded {
        let has_semi = tokens.iter().any(|t| t.kind == TokenKind::Semicolon || t.kind == TokenKind::Keyword(Keyword::Enum));
        let has_kw = tokens.iter().any(|t| matches!(t.kind, TokenKind::Keyword(_)));
        if !(has_semi && (has_kw || FORCE.load(Ordering::Relaxed))) {
            return Ok(Vec::new());
        }
    }

    let mut edits: Vec<Edit> = Vec::new();
    let mut stack: Vec<Ctx> = Vec::new();
    let mut pending_enum = false;

    let line_indent = |idx: usize| -> String {
        let mut ls = idx;
        while ls > 0 && chars[ls - 1] != '\n' {
            ls -= 1;
        }
        chars[ls..].iter().take_while(|c| **c == ' ' || **c == '\t').collect()
    };

    for (i, t) in tokens.iter().enumerate() {
        let at_block_level = matches!(stack.last(), None | Some(Ctx::Brace) | Some(Ctx::EnumBrace));
        let prev = if i > 0 { Some(&tokens[i - 1]) } else { None };

        // Operator starting a continuation line (old syntax: no `;` before it).
        if at_block_level && is_continuation_op(&t.kind) {
            if let Some(p) = prev {
                let newline = p.span.end.line < t.span.start.line;
                let prev_open = matches!(
                    p.kind,
                    TokenKind::Semicolon
                        | TokenKind::LBrace
                        | TokenKind::RBrace
                        | TokenKind::Comma
                        | TokenKind::FatArrow
                        | TokenKind::ThinArrow
                        | TokenKind::Dot
                        | TokenKind::ColonColon
                        | TokenKind::LParen
                        | TokenKind::LBracket
                        | TokenKind::Bang
                        | TokenKind::Tilde
                        | TokenKind::Keyword(Keyword::Else)
                ) || is_continuation_op(&p.kind);
                if newline && !prev_open {
                    let ps = p.span.end.offset as usize;
                    let mut ts = t.span.start.offset as usize;
                    let te = t.span.end.offset as usize;
                    // remove the operator and the whitespace after it
                    let mut te2 = te;
                    while te2 < chars.len() && (chars[te2] == ' ' || chars[te2] == '\t') {
                        te2 += 1;
                    }
                    // also drop whitespace before it only if the line is otherwise blank
                    let _ = &mut ts;
                    edits.push(Edit { start: ps, end: ps, text: format!(" {}", op_text(&t.kind)) });
                    edits.push(Edit { start: ts, end: te2, text: String::new() });
                }
            }
        }

        match &t.kind {
            TokenKind::Keyword(Keyword::Enum) => pending_enum = true,
            TokenKind::LBrace => {
                stack.push(if pending_enum { Ctx::EnumBrace } else { Ctx::Brace });
                pending_enum = false;
            }
            TokenKind::LParen => {
                let is_for = matches!(prev.map(|p| &p.kind), Some(TokenKind::Keyword(Keyword::For)));
                stack.push(if is_for { Ctx::ForHeader } else { Ctx::Paren });
            }
            TokenKind::LBracket => stack.push(Ctx::Paren),
            TokenKind::RBrace | TokenKind::RParen | TokenKind::RBracket => {
                stack.pop();
            }
            TokenKind::Semicolon | TokenKind::Comma => {
                let top = stack.last().copied();
                let is_term = match t.kind {
                    TokenKind::Semicolon => top != Some(Ctx::ForHeader),
                    _ => top == Some(Ctx::EnumBrace),
                };
                if !is_term {
                    continue;
                }
                let idx = t.span.start.offset as usize;
                let mut j = idx + 1;
                while j < chars.len() && (chars[j] == ' ' || chars[j] == '\t') {
                    j += 1;
                }
                let rest_empty = j >= chars.len()
                    || chars[j] == '\n'
                    || chars[j] == '\r'
                    || (chars[j] == '/' && chars.get(j + 1) == Some(&'/'));
                if rest_empty || chars[j] == '}' {
                    edits.push(Edit { start: idx, end: idx + 1, text: String::new() });
                } else {
                    let ind = line_indent(idx);
                    edits.push(Edit { start: idx, end: j, text: format!("\n{ind}") });
                }
            }
            _ => {}
        }
    }

    Ok(edits)
}


/// A string literal found in a Rust source file: raw char range of its content
/// plus the decoded text with a map decoded-char -> raw char range.
struct RustLiteral {
    decoded: String,
    map: Vec<(usize, usize)>,
    content_start: usize,
    content_end: usize,
    is_raw: bool,
    has_real_newline: bool,
}

fn scan_rust_literals(chars: &[char]) -> Vec<RustLiteral> {
    let mut lits = Vec::new();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        // comments
        if c == '/' && i + 1 < n && chars[i + 1] == '/' {
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < n && chars[i + 1] == '*' {
            let mut depth = 1;
            i += 2;
            while i < n && depth > 0 {
                if chars[i] == '/' && i + 1 < n && chars[i + 1] == '*' {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && i + 1 < n && chars[i + 1] == '/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        // char literal vs lifetime
        if c == '\'' {
            if i + 2 < n && chars[i + 1] == '\\' {
                // escaped char literal: skip to closing quote
                let mut j = i + 2;
                while j < n && chars[j] != '\'' {
                    j += 1;
                }
                i = j + 1;
                continue;
            }
            if i + 2 < n && chars[i + 2] == '\'' {
                i += 3;
                continue;
            }
            i += 1;
            continue;
        }
        // raw string: r"..", r#".."#, br#".."#
        let mut k = i;
        if (c == 'b' && i + 1 < n && chars[i + 1] == 'r') || c == 'r' {
            k = if c == 'b' { i + 2 } else { i + 1 };
            let mut hashes = 0;
            while k < n && chars[k] == '#' {
                hashes += 1;
                k += 1;
            }
            let prev_ident = i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_');
            if k < n && chars[k] == '"' && !prev_ident {
                let content_start = k + 1;
                let mut j = content_start;
                let mut end = None;
                while j < n {
                    if chars[j] == '"' && (0..hashes).all(|h| chars.get(j + 1 + h) == Some(&'#')) {
                        end = Some(j);
                        break;
                    }
                    j += 1;
                }
                let Some(end) = end else { break };
                let decoded: String = chars[content_start..end].iter().collect();
                let map = (content_start..end).map(|x| (x, x + 1)).collect();
                lits.push(RustLiteral {
                    decoded,
                    map,
                    content_start,
                    content_end: end,
                    is_raw: true,
                    has_real_newline: true,
                });
                i = end + 1 + hashes;
                continue;
            }
        }
        // normal (byte) string
        if c == '"' || (c == 'b' && i + 1 < n && chars[i + 1] == '"' && !(i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_'))) {
            let open = if c == 'b' { i + 1 } else { i };
            let content_start = open + 1;
            let mut j = content_start;
            let mut decoded = String::new();
            let mut map = Vec::new();
            let mut has_nl = false;
            while j < n && chars[j] != '"' {
                if chars[j] == '\\' && j + 1 < n {
                    let e = chars[j + 1];
                    match e {
                        'n' => { decoded.push('\n'); map.push((j, j + 2)); j += 2; }
                        't' => { decoded.push('\t'); map.push((j, j + 2)); j += 2; }
                        'r' => { decoded.push('\r'); map.push((j, j + 2)); j += 2; }
                        '0' => { decoded.push('\0'); map.push((j, j + 2)); j += 2; }
                        '\\' | '"' | '\'' => { decoded.push(e); map.push((j, j + 2)); j += 2; }
                        'u' => {
                            let mut k2 = j + 2;
                            let mut hex = String::new();
                            if k2 < n && chars[k2] == '{' {
                                k2 += 1;
                                while k2 < n && chars[k2] != '}' { hex.push(chars[k2]); k2 += 1; }
                                k2 += 1;
                            }
                            let ch = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32).unwrap_or('\u{fffd}');
                            decoded.push(ch);
                            map.push((j, k2));
                            j = k2;
                        }
                        'x' => {
                            let hex: String = chars[j + 2..(j + 4).min(n)].iter().collect();
                            let ch = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32).unwrap_or('\u{fffd}');
                            decoded.push(ch);
                            map.push((j, j + 4));
                            j += 4;
                        }
                        '\n' | '\r' => {
                            // line continuation: skip the newline and leading whitespace of the next line
                            has_nl = true;
                            j += 2;
                            while j < n && chars[j].is_whitespace() { j += 1; }
                        }
                        _ => { decoded.push(e); map.push((j, j + 2)); j += 2; }
                    }
                } else {
                    if chars[j] == '\n' { has_nl = true; }
                    decoded.push(chars[j]);
                    map.push((j, j + 1));
                    j += 1;
                }
            }
            lits.push(RustLiteral {
                decoded,
                map,
                content_start,
                content_end: j,
                is_raw: false,
                has_real_newline: has_nl,
            });
            i = j + 1;
            continue;
        }
        let _ = k;
        i += 1;
    }
    lits
}

/// Migrate the Tinox code embedded in the string literals of a Rust file.
#[allow(dead_code)]
pub fn migrate_rust_source(src: &str) -> Result<String, String> {
    migrate_rust_source_scoped(src, false)
}

/// Literals that belong together: one literal, or all literals of a `concat!(...)`.
struct Group {
    decoded: String,
    /// per decoded char: (raw start, raw end, index of the literal it came from)
    map: Vec<(usize, usize, usize)>,
    pieces: Vec<RustLiteral>,
}

fn group_literals(chars: &[char], lits: Vec<RustLiteral>) -> Vec<Group> {
    let sep_ok = |from: usize, to: usize| -> bool {
        // text between the end of one literal and the start of the next
        let mut i = from;
        while i < to {
            let c = chars[i];
            if c.is_whitespace() || c == ',' {
                i += 1;
            } else if c == '/' && chars.get(i + 1) == Some(&'/') {
                while i < to && chars[i] != '\n' {
                    i += 1;
                }
            } else {
                return false;
            }
        }
        true
    };
    // raw index of the opening quote of a literal and the index just after its closing quote
    let open_of = |l: &RustLiteral| -> usize {
        let mut i = l.content_start - 1;
        if l.is_raw {
            while i > 0 && chars[i - 1] == '#' {
                i -= 1;
            }
        }
        i
    };
    let close_of = |l: &RustLiteral| -> usize {
        let mut i = l.content_end + 1;
        if l.is_raw {
            while chars.get(i) == Some(&'#') {
                i += 1;
            }
        }
        i
    };
    let concat_needle: Vec<char> = "concat!(".chars().collect();
    let preceded_by_concat = |open: usize| -> bool {
        let mut i = open;
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        i >= concat_needle.len() && chars[i - concat_needle.len()..i] == concat_needle[..]
    };
    let mut groups: Vec<Group> = Vec::new();
    let mut iter = lits.into_iter().peekable();
    while let Some(first) = iter.next() {
        let mut pieces = vec![first];
        if preceded_by_concat(open_of(&pieces[0])) {
            while let Some(next) = iter.peek() {
                let prev = pieces.last().unwrap();
                if sep_ok(close_of(prev), open_of(next)) {
                    pieces.push(iter.next().unwrap());
                } else {
                    break;
                }
            }
        }
        let mut decoded = String::new();
        let mut map = Vec::new();
        for (pi, piece) in pieces.iter().enumerate() {
            decoded.push_str(&piece.decoded);
            map.extend(piece.map.iter().map(|&(a, b)| (a, b, pi)));
        }
        groups.push(Group { decoded, map, pieces });
    }
    groups
}

/// `tests_only`: touch only literals after the first `#[cfg(test)]` (production
/// code of `src/` files emits Tinox source that is migrated by hand).
fn migrate_rust_source_scoped(src: &str, tests_only: bool) -> Result<String, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut all: Vec<Edit> = Vec::new();
    let test_start = if tests_only {
        let needle: Vec<char> = "#[cfg(test)]".chars().collect();
        (0..chars.len().saturating_sub(needle.len()))
            .find(|&i| chars[i..i + needle.len()] == needle[..])
            .unwrap_or(usize::MAX)
    } else {
        0
    };
    for group in group_literals(&chars, scan_rust_literals(&chars)) {
        if group.pieces[0].content_start < test_start {
            continue;
        }
        let d = &group.decoded;
        if d.contains("#include") || d.contains("(void)") || d.contains("; do ") {
            continue;
        }
        if !d.contains(';') && !d.contains("enum") {
            continue;
        }
        let Ok(edits) = compute_edits(d, true) else { continue };
        let encode = |piece: &RustLiteral, text: &str| -> String {
            let mut out = String::new();
            for ch in text.chars() {
                match ch {
                    '\n' if !piece.is_raw && !piece.has_real_newline => out.push_str("\\n"),
                    '"' if !piece.is_raw => out.push_str("\\\""),
                    '\\' if !piece.is_raw => out.push_str("\\\\"),
                    _ => out.push(ch),
                }
            }
            out
        };
        for e in edits {
            if e.start == e.end {
                // pure insertion
                let (raw, pi) = if e.start < group.map.len() {
                    (group.map[e.start].0, group.map[e.start].2)
                } else {
                    let last = group.pieces.len() - 1;
                    (group.pieces[last].content_end, last)
                };
                all.push(Edit { start: raw, end: raw, text: encode(&group.pieces[pi], &e.text) });
                continue;
            }
            // replacement/deletion: split per literal piece so nothing outside a
            // string's content is ever touched
            let mut first = true;
            let mut i = e.start;
            while i < e.end {
                let pi = group.map[i].2;
                let mut j = i;
                while j + 1 < e.end && group.map[j + 1].2 == pi {
                    j += 1;
                }
                let text = if first { encode(&group.pieces[pi], &e.text) } else { String::new() };
                all.push(Edit { start: group.map[i].0, end: group.map[j].1, text });
                first = false;
                i = j + 1;
            }
        }
    }
    apply_edits(&chars, all)
}


/// Migrate fenced Tinox code blocks of a Markdown file. Labelled blocks
/// (`tinox`, `tnx`) are always converted; unlabelled blocks only when they look
/// like Tinox (`fn`/`fnc` with `->`, or an `import tinox`).
pub fn migrate_markdown(src: &str) -> Result<String, String> {
    let mut out = String::with_capacity(src.len());
    let mut lines = src.split_inclusive('\n').peekable();
    while let Some(line) = lines.next() {
        out.push_str(line);
        let trimmed = line.trim_start();
        if !trimmed.starts_with("```") {
            continue;
        }
        let lang = trimmed.trim_start_matches('`').trim();
        let labelled = lang == "tinox" || lang == "tnx";
        if !labelled && !lang.is_empty() {
            // some other language: copy verbatim up to the closing fence
            for l in lines.by_ref() {
                out.push_str(l);
                if l.trim_start().starts_with("```") {
                    break;
                }
            }
            continue;
        }
        let mut block = String::new();
        let mut closing: Option<&str> = None;
        for l in lines.by_ref() {
            if l.trim_start().starts_with("```") {
                closing = Some(l);
                break;
            }
            block.push_str(l);
        }
        let looks_tinox = labelled
            || ((block.contains("fnc ") || block.contains("fn ")) && block.contains("->"))
            || block.contains("import tinox");
        match if looks_tinox { migrate_source(&block) } else { Err(String::new()) } {
            Ok(new) => out.push_str(&new),
            Err(_) => out.push_str(&block),
        }
        if let Some(c) = closing {
            out.push_str(c);
        }
    }
    Ok(out)
}

fn collect(path: &Path, out: &mut Vec<PathBuf>, rust: bool, md: bool) {
    if path.is_dir() {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if matches!(name, "target" | ".git" | ".tinox" | "node_modules") {
            return;
        }
        if let Ok(rd) = std::fs::read_dir(path) {
            let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
            entries.sort();
            for e in entries {
                collect(&e, out, rust, md);
            }
        }
    } else if path.extension().and_then(|e| e.to_str()) == Some(if rust { "rs" } else if md { "md" } else { "tnx" }) {
        out.push(path.to_path_buf());
    }
}

/// Parse every file with the current (semicolon-free) parser and report errors.
fn verify(files: &[PathBuf]) -> usize {
    let mut bad = 0;
    for f in files {
        let Ok(src) = std::fs::read_to_string(f) else { continue };
        let toks = match Lexer::new(&src).tokenize() {
            Ok(t) => t,
            Err(e) => {
                bad += 1;
                eprintln!("{}: lex error {:?}", f.display(), e.first().map(|e| (e.span.start.line, e.message.clone())));
                continue;
            }
        };
        if let Err(bag) = tinox_parser::Parser::new(toks).parse() {
            bad += 1;
            for e in bag.errors.iter().take(3) {
                eprintln!("{}:{}: {}", f.display(), e.span.start.line, e.message);
            }
        }
    }
    bad
}

pub fn run(args: &[String]) -> Result<(), String> {
    let check = args.iter().any(|a| a == "--check");
    let verify_only = args.iter().any(|a| a == "--verify");
    let rust = args.iter().any(|a| a == "--rust");
    let md = args.iter().any(|a| a == "--md");
    FORCE.store(args.iter().any(|a| a == "--force"), Ordering::Relaxed);
    let mut files = Vec::new();
    for a in args.iter().filter(|a| !a.starts_with("--")) {
        collect(Path::new(a), &mut files, rust, md);
    }
    if files.is_empty() {
        return Err("usage: tinox migrate-semicolons [--check] <file-or-dir>...".to_string());
    }
    if verify_only {
        let bad = verify(&files);
        println!("{} file(s) verified, {} with parse errors", files.len(), bad);
        return if bad > 0 { Err(format!("{bad} file(s) do not parse")) } else { Ok(()) };
    }
    let (mut changed, mut failed) = (0, 0);
    for f in &files {
        // the lexer tests intentionally keep lexing `;`
        if f.file_name().and_then(|n| n.to_str()) == Some("semicolon_migration.rs")
            || f.components().any(|c| c.as_os_str() == "tinox-lexer")
            // its newline-termination tests intentionally contain `;`
            || f.ends_with("tinox-parser/src/parser.rs")
        {
            continue;
        }
        let src = std::fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display()))?;
        match if md { migrate_markdown(&src) } else if rust { migrate_rust_source_scoped(&src, f.components().any(|c| c.as_os_str() == "src")) } else { migrate_source(&src) } {
            Ok(new) => {
                if new != src {
                    changed += 1;
                    if !check {
                        std::fs::write(f, new).map_err(|e| format!("{}: {e}", f.display()))?;
                    }
                }
            }
            Err(e) => {
                failed += 1;
                eprintln!("{}: {e}", f.display());
            }
        }
    }
    println!("{} file(s) {}, {} unchanged, {} failed", changed, if check { "would change" } else { "migrated" }, files.len() - changed - failed, failed);
    if failed > 0 {
        return Err(format!("{failed} file(s) could not be migrated"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // The inputs are deliberately written in the OLD syntax; `run` skips this
    // file so `--rust` never rewrites them.

    #[test]
    fn strips_terminators() {
        assert_eq!(migrate_source("class Main {\n  fn f() { return 1; }\n}\n").unwrap(), "class Main {\n  fn f() { return 1 }\n}\n");
    }

    #[test]
    fn splits_same_line_statements() {
        assert_eq!(migrate_source("fn f() {\n  a(); b();\n}\n").unwrap(), "fn f() {\n  a()\n  b()\n}\n");
    }

    #[test]
    fn keeps_for_header_separators() {
        assert_eq!(migrate_source("fn f() {\n  for (var i = 0; i < 3; i++) { g(); }\n}\n").unwrap(), "fn f() {\n  for (var i = 0; i < 3; i++) { g() }\n}\n");
    }

    #[test]
    fn moves_leading_operator_to_previous_line() {
        assert_eq!(migrate_source("fn f() {\n  const x = a\n    && b;\n}\n").unwrap(), "fn f() {\n  const x = a &&\n    b\n}\n");
    }

    #[test]
    fn enum_variants_one_per_line() {
        assert_eq!(migrate_source("enum E { A, B(Int64), C }\n").unwrap(), "enum E { A\nB(Int64)\nC }\n");
    }

    #[test]
    fn trailing_comment_stays() {
        assert_eq!(migrate_source("fn f() {\n  a(); // note\n}\n").unwrap(), "fn f() {\n  a() // note\n}\n");
    }

    #[test]
    fn rust_literal_is_migrated() {
        let src = "let s = \"fn main() -> Int32 { return 42; }\";\n";
        assert_eq!(migrate_rust_source(src).unwrap(), "let s = \"fn main() -> Int32 { return 42 }\";\n");
    }

    #[test]
    fn rust_literal_statements_split_with_escape() {
        let src = "let s = \"fn f() { a(); b(); }\";\n";
        assert_eq!(migrate_rust_source(src).unwrap(), "let s = \"fn f() { a()\\nb() }\";\n");
    }
}
