//! `tinox fmt` is a layout-only formatter (issue #465): for every `.tnx` in the
//! repository it must keep every token and comment, be idempotent, and never
//! produce something that no longer lexes/parses.

use std::fs;
use std::path::{Path, PathBuf};
use tinox_lexer::{Lexer, TokenKind};
use tinox_parser::{format_source, Parser};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target" || n == ".tinox") {
                continue;
            }
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "tnx") {
            out.push(path);
        }
    }
}

/// Non-whitespace token kinds including comments (but not Whitespace/Newline).
fn significant_tokens(src: &str) -> Vec<TokenKind> {
    Lexer::new(src)
        .tokenize_with_trivia()
        .unwrap_or_else(|e| panic!("lex failed: {e:?}"))
        .into_iter()
        .map(|t| t.kind)
        .filter(|k| !matches!(k, TokenKind::Whitespace | TokenKind::Newline))
        .collect()
}

#[test]
fn fmt_preserves_tokens_and_comments_and_is_idempotent() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    for dir in ["tests/e2e", "examples", "crates/tinox-core", "crates/tinox-core-ext"] {
        collect(&root.join(dir), &mut files);
    }
    assert!(files.len() > 100, "expected many .tnx files, found {}", files.len());
    let mut failures = Vec::new();
    for file in &files {
        let src = fs::read_to_string(file).unwrap();
        // Skip files that don't lex/parse today (nothing to format).
        let Ok(tokens) = Lexer::new(&src).tokenize() else { continue };
        if Parser::new(tokens).parse().is_err() {
            continue;
        }
        let once = match format_source(&src) {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("{}: format failed: {e:?}", file.display()));
                continue;
            }
        };
        if significant_tokens(&src) != significant_tokens(&once) {
            failures.push(format!("{}: tokens or comments changed", file.display()));
        }
        match format_source(&once) {
            Ok(twice) if twice == once => {}
            Ok(_) => failures.push(format!("{}: not idempotent", file.display())),
            Err(e) => failures.push(format!("{}: second pass failed: {e:?}", file.display())),
        }
        let Ok(tokens) = Lexer::new(&once).tokenize() else {
            failures.push(format!("{}: formatted output does not lex", file.display()));
            continue;
        };
        if Parser::new(tokens).parse().is_err() {
            failures.push(format!("{}: formatted output does not parse", file.display()));
        }
    }
    assert!(failures.is_empty(), "{} files failed:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn fmt_reindents_and_keeps_comments() {
    let src = "import tinox.core.json\n\n// keep me\nclass Main\n{\n    fnc main() -> Int32 {\n            const xs = [1, 2, 3]\n        const ys = xs.map(x => x * 2) // trailing\n\tif (xs.len() > 1) {\n      println(\"a\")\n        }\n        return 0\n    }\n}\n";
    let out = format_source(src).unwrap();
    assert!(out.contains("import tinox.core.json"));
    assert!(out.contains("// keep me"));
    assert!(out.contains("xs.map(x => x * 2) // trailing"));
    assert!(out.contains("\n        if (xs.len() > 1) {\n            println(\"a\")\n        }\n"));
}
