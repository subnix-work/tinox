//! `tinox i18n extract [--out FILE] [path...]` (issue #588): collects the texts passed to `tr`, `trc` and
//! `trn` into a gettext `.pot` template, so catalogs can be produced with the standard tools (msginit,
//! msgmerge, msgfmt). Works on the token stream, so it finds calls wherever they are; the compiler already
//! requires string literals for these texts (see the type checker), so nothing a program translates is missed.
//! Calls that are not plain literals are reported with their position and make the command fail.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use tinox_lexer::{Lexer, Token, TokenKind};

#[derive(Default, PartialEq, Eq, PartialOrd, Ord, Clone)]
struct Key {
    context: Option<String>,
    id: String,
    plural: Option<String>,
}

#[derive(Default)]
struct Extracted {
    entries: BTreeMap<Key, Vec<String>>,
    problems: Vec<String>,
}

fn literal(token: Option<&Token>) -> Option<&str> {
    match token.map(|t| &t.kind) {
        Some(TokenKind::String(s)) | Some(TokenKind::RawString(s)) => Some(s),
        _ => None,
    }
}

fn scan(src: &str, file: &str, out: &mut Extracted) -> Result<(), String> {
    let tokens: Vec<Token> = Lexer::new(src)
        .tokenize()
        .map_err(|e| format!("{file}: {e:?}"))?
        .into_iter()
        .filter(|t| !t.kind.is_trivia())
        .collect();
    for i in 0..tokens.len() {
        let TokenKind::Ident(name) = &tokens[i].kind else { continue };
        let texts = match name.as_str() {
            "tr" => 1,
            "trc" | "trn" => 2,
            _ => continue,
        };
        // `Foo::tr(...)` and `x.tr(...)` are user methods, not the built-in.
        if i > 0 && matches!(tokens[i - 1].kind, TokenKind::Dot | TokenKind::ColonColon) {
            continue;
        }
        if !matches!(tokens.get(i + 1).map(|t| &t.kind), Some(TokenKind::LParen)) {
            continue;
        }
        let at = format!("{file}:{}", tokens[i].span.start.line);
        // The text arguments are consecutive, comma separated literals.
        let mut texts_found = Vec::new();
        let mut cursor = i + 2;
        for n in 0..texts {
            match literal(tokens.get(cursor)) {
                Some(s) => texts_found.push(s.to_string()),
                None => {
                    out.problems.push(format!("{at}: argument {} of {name}() is not a plain string literal", n + 1));
                    break;
                }
            }
            cursor += 1;
            if n + 1 < texts {
                if !matches!(tokens.get(cursor).map(|t| &t.kind), Some(TokenKind::Comma)) {
                    out.problems.push(format!("{at}: {name}() needs {texts} text arguments"));
                    break;
                }
                cursor += 1;
            }
        }
        if texts_found.len() != texts {
            continue;
        }
        // The last text must be the whole argument, not the start of an expression such as "a" + b.
        let closes = match name.as_str() {
            "trn" => TokenKind::Comma,
            _ => TokenKind::RParen,
        };
        if tokens.get(cursor).map(|t| &t.kind) != Some(&closes) {
            out.problems.push(format!("{at}: the texts of {name}() must be plain string literals, not expressions"));
            continue;
        }
        let key = match name.as_str() {
            "tr" => Key { context: None, id: texts_found[0].clone(), plural: None },
            "trc" => Key { context: Some(texts_found[0].clone()), id: texts_found[1].clone(), plural: None },
            _ => Key { context: None, id: texts_found[0].clone(), plural: Some(texts_found[1].clone()) },
        };
        out.entries.entry(key).or_default().push(at);
    }
    Ok(())
}

fn collect(path: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    if path.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(path)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        entries.sort();
        for entry in entries {
            let hidden = entry.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.') || n == "target");
            if !hidden {
                collect(&entry, files)?;
            }
        }
    } else if path.extension().is_some_and(|e| e == "tnx") {
        files.push(path.to_path_buf());
    }
    Ok(())
}

fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn render(entries: &BTreeMap<Key, Vec<String>>, package: &str) -> String {
    let mut pot = String::new();
    pot.push_str("# Translation template generated by `tinox i18n extract`.\n");
    pot.push_str("msgid \"\"\nmsgstr \"\"\n");
    let _ = writeln!(pot, "\"Project-Id-Version: {package}\\n\"");
    pot.push_str("\"MIME-Version: 1.0\\n\"\n\"Content-Type: text/plain; charset=UTF-8\\n\"\n\"Content-Transfer-Encoding: 8bit\\n\"\n");
    pot.push_str("\"Plural-Forms: nplurals=INTEGER; plural=EXPRESSION;\\n\"\n");
    for (key, refs) in entries {
        pot.push('\n');
        let _ = writeln!(pot, "#: {}", refs.join(" "));
        if let Some(c) = &key.context {
            let _ = writeln!(pot, "msgctxt {}", quote(c));
        }
        let _ = writeln!(pot, "msgid {}", quote(&key.id));
        if let Some(p) = &key.plural {
            let _ = writeln!(pot, "msgid_plural {}", quote(p));
            pot.push_str("msgstr[0] \"\"\nmsgstr[1] \"\"\n");
        } else {
            pot.push_str("msgstr \"\"\n");
        }
    }
    pot
}

pub fn run(args: &[String]) -> Result<(), String> {
    if args.first().map(String::as_str) != Some("extract") {
        return Err("usage: tinox i18n extract [--out FILE|-] [path...]  (default: src/ -> messages.pot)".into());
    }
    let mut out_path = "messages.pot".to_string();
    let mut inputs: Vec<PathBuf> = Vec::new();
    let mut rest = args[1..].iter();
    while let Some(arg) = rest.next() {
        if arg == "--out" {
            out_path = rest.next().ok_or("--out needs a file name (or - for stdout)")?.clone();
        } else {
            inputs.push(PathBuf::from(arg));
        }
    }
    let root = crate::pm::find_project_root();
    if inputs.is_empty() {
        inputs.push(root.as_ref().map(|r| r.join("src")).unwrap_or_else(|| PathBuf::from("src")));
    }
    let mut files = Vec::new();
    for input in &inputs {
        collect(input, &mut files)?;
    }
    if files.is_empty() {
        return Err("no .tnx files found".into());
    }
    let mut extracted = Extracted::default();
    for file in &files {
        let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
        let shown = root.as_ref().and_then(|r| file.strip_prefix(r).ok()).unwrap_or(file);
        scan(&text, &shown.to_string_lossy(), &mut extracted)?;
    }
    if !extracted.problems.is_empty() {
        return Err(format!("texts that cannot be extracted:\n  {}", extracted.problems.join("\n  ")));
    }
    let package = crate::read_project_name().unwrap_or_else(|| "app".to_string());
    let pot = render(&extracted.entries, &package);
    if out_path == "-" {
        print!("{pot}");
    } else {
        std::fs::write(&out_path, &pot).map_err(|e| format!("{out_path}: {e}"))?;
        eprintln!("{} message(s) from {} file(s) written to {out_path}", extracted.entries.len(), files.len());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(src: &str) -> Extracted {
        let mut e = Extracted::default();
        scan(src, "Main.tnx", &mut e).unwrap();
        e
    }

    #[test]
    fn finds_all_three_forms_and_merges_references() {
        let e = extract(
            "class Main {\nfnc main() -> Int32 {\nprintln(tr(\"Hello\"))\nprintln(trc(\"menu\", \"Open\"))\nprintln(trn(\"%1 file\", \"%1 files\", 3))\nprintln(tr(\"Hello\"))\nreturn 0\n}\n}\n",
        );
        assert!(e.problems.is_empty(), "{:?}", e.problems);
        assert_eq!(e.entries.len(), 3);
        let hello = e.entries.get(&Key { context: None, id: "Hello".into(), plural: None }).unwrap();
        assert_eq!(hello, &vec!["Main.tnx:3".to_string(), "Main.tnx:6".to_string()]);
        assert!(e.entries.contains_key(&Key { context: Some("menu".into()), id: "Open".into(), plural: None }));
        assert!(e.entries.contains_key(&Key { context: None, id: "%1 file".into(), plural: Some("%1 files".into()) }));
    }

    #[test]
    fn user_methods_named_tr_and_non_literals_are_handled() {
        let e = extract("class Main {\nfnc main() -> Int32 {\nFoo::tr(\"not ours\")\nx.tr(\"nor this\")\nreturn 0\n}\n}\n");
        assert!(e.entries.is_empty() && e.problems.is_empty());
        let e = extract("class Main {\nfnc main() -> Int32 {\nprintln(tr(name))\nprintln(tr(\"a\" + b))\nreturn 0\n}\n}\n");
        assert_eq!(e.problems.len(), 2, "{:?}", e.problems);
        assert!(e.problems[0].starts_with("Main.tnx:3"));
    }

    #[test]
    fn output_escapes_and_orders_deterministically() {
        let mut entries = BTreeMap::new();
        entries.insert(Key { context: None, id: "say \"hi\"\n\\".into(), plural: None }, vec!["a.tnx:1".into()]);
        let pot = render(&entries, "demo");
        assert!(pot.contains("msgid \"say \\\"hi\\\"\\n\\\\\""), "{pot}");
        assert!(pot.contains("#: a.tnx:1"));
    }
}
