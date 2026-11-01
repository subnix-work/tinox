//! Helper for issue #524: `tinox migrate-exceptions [--check] [--class Name] <dir-or-file>...`
//! rewrites `throw "text"` (a string literal or a concatenation starting with one) into
//! `throw AppError { message: "text" }`, declares `@Throws(AppError)` on the enclosing
//! method and creates the `@Error` class once in the first directory argument.
//!
//! Deliberately conservative: a rethrow (`throw e`), any other non-literal `throw` and
//! every `catch (e: String)` stay untouched and are listed in the report, because
//! their meaning depends on types the token stream does not have. `catch (e: String)`
//! keeps working (it sees `CODE: message`), the checker only flags the leftovers.

use std::path::{Path, PathBuf};

use tinox_lexer::{Keyword, Lexer, Token, TokenKind};

struct Edit {
    at: usize,
    end: usize,
    text: String,
}

#[derive(Default)]
struct Report {
    rewritten: usize,
    annotated: usize,
    manual: Vec<String>,
    catch_string: usize,
}

fn is_cont(k: &TokenKind) -> bool {
    matches!(
        k,
        TokenKind::Plus | TokenKind::Minus | TokenKind::Star | TokenKind::Slash | TokenKind::Percent
            | TokenKind::EqualsEquals | TokenKind::BangEquals | TokenKind::AmpAmp | TokenKind::BarBar
            | TokenKind::Question | TokenKind::Colon | TokenKind::Comma | TokenKind::Dot
    )
}

fn migrate(src: &str, class: &str, file: &str, report: &mut Report) -> Result<String, String> {
    let chars: Vec<char> = src.chars().collect();
    let tokens: Vec<Token> = Lexer::new(src)
        .tokenize()
        .map_err(|e| format!("lex error: {:?}", e.first().map(|e| e.message.clone())))?
        .into_iter()
        .filter(|t| !t.kind.is_trivia())
        .collect();
    let mut edits: Vec<Edit> = Vec::new();
    // innermost-last stack of `{` blocks, each with the `fn`/`fnc` token index that owns it
    let mut blocks: Vec<Option<usize>> = Vec::new();
    let mut pending_fn: Option<usize> = None;
    let mut paren = 0i32;
    let mut annotated: Vec<usize> = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        match &t.kind {
            TokenKind::Keyword(Keyword::Fn) | TokenKind::Keyword(Keyword::Fnc) => pending_fn = Some(i),
            TokenKind::LParen | TokenKind::LBracket => paren += 1,
            TokenKind::RParen | TokenKind::RBracket => paren -= 1,
            TokenKind::LBrace if paren <= 0 => blocks.push(pending_fn.take()),
            TokenKind::RBrace if paren <= 0 => {
                blocks.pop();
            }
            TokenKind::Keyword(Keyword::Catch) => {
                if let (Some(a), Some(b), Some(c)) = (tokens.get(i + 1), tokens.get(i + 2), tokens.get(i + 3)) {
                    let (name_at, ty) = if a.kind == TokenKind::LParen { (b, c) } else { (a, b) };
                    let _ = name_at;
                    let ty = if a.kind == TokenKind::LParen { tokens.get(i + 4) } else { Some(ty) };
                    if matches!(ty.map(|t| &t.kind), Some(TokenKind::Ident(n)) if n == "String") {
                        report.catch_string += 1;
                    }
                }
            }
            TokenKind::Keyword(Keyword::Throw) => {
                let line = t.span.start.line;
                let Some(first) = tokens.get(i + 1) else { continue };
                if !matches!(first.kind, TokenKind::String(_) | TokenKind::InterpString(_) | TokenKind::RawString(_)) {
                    report.manual.push(format!("{file}:{line}: non-literal throw left as is"));
                    continue;
                }
                // end of the thrown expression: next line start at depth 0 (unless a trailing operator continues it)
                let mut depth = 0i32;
                let mut last = i + 1;
                let mut j = i + 2;
                while j < tokens.len() {
                    let tk = &tokens[j];
                    let new_line = tokens[j - 1].span.end.line < tk.span.start.line;
                    if depth == 0 && new_line && !is_cont(&tokens[j - 1].kind) && !matches!(tk.kind, TokenKind::Dot) {
                        break;
                    }
                    match tk.kind {
                        TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
                        TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                            depth -= 1;
                            if depth < 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    last = j;
                    j += 1;
                }
                edits.push(Edit { at: first.span.start.offset as usize, end: first.span.start.offset as usize, text: format!("{class} {{ message: ") });
                let end = tokens[last].span.end.offset as usize;
                edits.push(Edit { at: end, end, text: " }".to_string() });
                report.rewritten += 1;
                if let Some(Some(f)) = blocks.iter().rev().find(|b| b.is_some()) {
                    if !annotated.contains(f) {
                        annotated.push(*f);
                    }
                } else {
                    report.manual.push(format!("{file}:{line}: throw outside a method, add @Throws by hand"));
                }
            }
            _ => {}
        }
    }
    for f in annotated {
        let ft = &tokens[f];
        // insert above a directly preceding doc comment (annotations come before the doc)
        let anchor = match f.checked_sub(1).map(|p| &tokens[p]) {
            Some(p) if matches!(p.kind, TokenKind::DocComment(_)) => p.span.start.offset as usize,
            _ => ft.span.start.offset as usize,
        };
        let mut ls = anchor;
        while ls > 0 && chars[ls - 1] != '\n' {
            ls -= 1;
        }
        let indent: String = chars[ls..].iter().take_while(|c| **c == ' ' || **c == '\t').collect();
        let head: String = chars[ls..anchor.max(ls)].iter().collect();
        if !head.trim().is_empty() {
            report.manual.push(format!("{file}:{}: method header shares a line, add @Throws({class}) by hand", ft.span.start.line));
            continue;
        }
        // an existing @Throws on this member is extended by hand
        let above: String = chars[..ls].iter().rev().take(240).collect::<Vec<_>>().into_iter().rev().collect();
        if above.trim_end().lines().last().is_some_and(|l| l.trim_start().starts_with("@Throws")) {
            report.manual.push(format!("{file}:{}: method already has @Throws, add {class} by hand", ft.span.start.line));
            continue;
        }
        edits.push(Edit { at: ls, end: ls, text: format!("{indent}@Throws({class})\n") });
        report.annotated += 1;
    }
    edits.sort_by_key(|e| (e.at, e.end));
    let mut out = String::with_capacity(src.len() + 64);
    let mut cursor = 0usize;
    for e in &edits {
        out.extend(&chars[cursor..e.at]);
        out.push_str(&e.text);
        cursor = e.end.max(e.at);
    }
    out.extend(&chars[cursor..]);
    Ok(out)
}

fn collect(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if matches!(name, "target" | ".git" | ".tinox" | "node_modules") {
            return;
        }
        if let Ok(rd) = std::fs::read_dir(path) {
            let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
            entries.sort();
            for e in entries {
                collect(&e, out);
            }
        }
    } else if path.extension().and_then(|e| e.to_str()) == Some("tnx") {
        out.push(path.to_path_buf());
    }
}

fn code_for(class: &str) -> String {
    let mut out = String::new();
    for (i, ch) in class.chars().enumerate() {
        if ch.is_ascii_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(ch.to_ascii_uppercase());
    }
    out
}

pub fn run(args: &[String]) -> Result<(), String> {
    let check = args.iter().any(|a| a == "--check");
    let mut class = "AppError".to_string();
    let mut paths = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--check" => {}
            "--class" => class = it.next().ok_or("--class needs a name")?.clone(),
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            _ => paths.push(a.clone()),
        }
    }
    if paths.is_empty() {
        return Err("usage: tinox migrate-exceptions [--check] [--class Name] <dir-or-file>...".to_string());
    }
    let root = Path::new(&paths[0]);
    let root_dir = if root.is_dir() { root.to_path_buf() } else { root.parent().map(Path::to_path_buf).unwrap_or_default() };
    let mut files = Vec::new();
    for p in &paths {
        collect(Path::new(p), &mut files);
    }
    let class_file = root_dir.join(format!("{class}.tnx"));
    let mut report = Report::default();
    let mut changed = 0;
    for f in &files {
        if *f == class_file {
            continue;
        }
        let src = std::fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display()))?;
        let mut new = migrate(&src, &class, &f.display().to_string(), &mut report).map_err(|e| format!("{}: {e}", f.display()))?;
        if new != src {
            // sibling files in the class directory import it by its short name
            if f.parent() == Some(root_dir.as_path()) && !new.contains(&format!("import {class}")) {
                new = format!("import {class}\n{new}");
            } else if f.parent() != Some(root_dir.as_path()) {
                report.manual.push(format!("{}: add an import for {class}", f.display()));
            }
            changed += 1;
            if !check {
                std::fs::write(f, new).map_err(|e| format!("{}: {e}", f.display()))?;
            }
        }
    }
    if report.rewritten > 0 && !class_file.exists() {
        let body = format!("@Error(\"{}\", 500)\nclass {class} {{\n    var message: String\n}}\n", code_for(&class));
        if !check {
            std::fs::write(&class_file, body).map_err(|e| format!("{}: {e}", class_file.display()))?;
        }
    }
    for m in &report.manual {
        println!("manual: {m}");
    }
    println!(
        "{} throw(s) rewritten, {} method(s) annotated, {} file(s) {}, {} catch (e: String) left (still valid)",
        report.rewritten,
        report.annotated,
        changed,
        if check { "would change" } else { "migrated" },
        report.catch_string
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_on(src: &str) -> (String, Report) {
        let mut r = Report::default();
        let out = migrate(src, "AppError", "t.tnx", &mut r).unwrap();
        (out, r)
    }

    #[test]
    fn rewrites_literal_throw_and_annotates_method() {
        let (out, r) = run_on("class A {\n    fn f(x: Int64) -> Int64 {\n        if (x < 0) {\n            throw \"neg: \" + x.toString()\n        }\n        return x\n    }\n}\n");
        assert!(out.contains("    @Throws(AppError)\n    fn f"), "{out}");
        assert!(out.contains("throw AppError { message: \"neg: \" + x.toString() }\n"), "{out}");
        assert_eq!((r.rewritten, r.annotated), (1, 1));
    }

    #[test]
    fn annotation_goes_above_the_doc_comment() {
        let (out, _) = run_on("class A {\n    /** doc */\n    fn f() -> Nothing {\n        throw \"x\"\n    }\n}\n");
        assert!(out.contains("    @Throws(AppError)\n    /** doc */\n    fn f"), "{out}");
    }

    #[test]
    fn leaves_rethrows_and_reports_them() {
        let (out, r) = run_on("class A {\n    fn f() -> Nothing {\n        try { g() } catch (e: String) { throw e }\n    }\n}\n");
        assert!(!out.contains("AppError"), "{out}");
        assert_eq!((r.rewritten, r.manual.len(), r.catch_string), (0, 1, 1));
    }
}
