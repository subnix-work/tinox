//! `textDocument/codeAction` (issue #494): add a missing import and organise
//! imports. The compiler emits no suggested edits (diagnostics schema v1), so
//! the fixes are derived here from the diagnostic text plus the project and
//! standard-library indexes.

use crate::navigation::DeclType;
use std::collections::HashMap;
use tinox_lexer::{Keyword, Lexer, TokenKind};
use tower_lsp::lsp_types::{Position, Range, TextEdit};

/// The name behind an "unresolved 'X::member' ... (missing import?)" diagnostic.
pub fn missing_import_name(message: &str) -> Option<String> {
    if !message.contains("missing import?") {
        return None;
    }
    let rest = message.strip_prefix("unresolved '")?;
    let end = rest.find("::").or_else(|| rest.find('\''))?;
    let name = &rest[..end];
    (!name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')).then(|| name.to_string())
}

/// Import paths that would provide type `name`: project files first.
pub fn import_candidates(name: &str, project: &[(String, &str)], stdlib: &[(String, Vec<String>)]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (_, text) in project {
        let Some(idx) = crate::navigation::FileIndex::build(tower_lsp::lsp_types::Url::parse("file:///x.tnx").unwrap(), (*text).to_string()) else { continue };
        if !idx.decls.iter().any(|d| d.kind == DeclType::Type && d.name == name) {
            continue;
        }
        let ns = namespace_of(&idx.toks);
        out.push(match ns {
            Some(ns) => format!("{ns}.{name}"),
            None => name.to_string(),
        });
    }
    for (path, classes) in stdlib {
        if classes.iter().any(|c| c == name) {
            out.push(path.clone());
        }
    }
    out.sort();
    out.dedup();
    out
}

fn namespace_of(toks: &[crate::navigation::Tok]) -> Option<String> {
    let i = toks.iter().position(|t| t.kw(Keyword::Namespace))?;
    let mut parts = Vec::new();
    let mut j = i + 1;
    while j < toks.len() {
        match &toks[j].kind {
            TokenKind::Ident(s) => parts.push(s.clone()),
            TokenKind::Dot => {}
            _ => break,
        }
        j += 1;
    }
    (!parts.is_empty()).then(|| parts.join("."))
}

/// Where a new `import` line goes: after the last import, otherwise before
/// the first declaration (after leading comments), as an edit.
pub fn add_import_edit(text: &str, path: &str) -> TextEdit {
    let line_of = |byte: usize| text[..byte].matches('\n').count() as u32;
    let mut last_import_line = None;
    let mut first_decl_line = None;
    if let Ok(toks) = Lexer::new(text).tokenize() {
        let byte_of: Vec<usize> = text.char_indices().map(|(b, _)| b).chain(std::iter::once(text.len())).collect();
        for (i, t) in toks.iter().enumerate() {
            let start = byte_of[t.span.start.offset as usize];
            let at_line_start = i == 0 || toks[i - 1].span.end.line < t.span.start.line;
            if !at_line_start {
                continue;
            }
            match t.kind {
                TokenKind::Keyword(Keyword::Import) => last_import_line = Some(line_of(start)),
                TokenKind::Keyword(Keyword::Module) => {}
                TokenKind::Eof => {}
                _ if first_decl_line.is_none() => first_decl_line = Some(line_of(start)),
                _ => {}
            }
        }
    }
    let line_text = |l: u32| text.lines().nth(l as usize).unwrap_or("");
    match (last_import_line, first_decl_line) {
        (Some(l), _) => {
            // end of the (single-line) import statement
            let _ = line_text(l);
            TextEdit { range: Range { start: Position { line: l + 1, character: 0 }, end: Position { line: l + 1, character: 0 } }, new_text: format!("import {path}\n") }
        }
        (None, Some(l)) => TextEdit {
            range: Range { start: Position { line: l, character: 0 }, end: Position { line: l, character: 0 } },
            new_text: format!("import {path}\n\n"),
        },
        (None, None) => TextEdit { range: Range::default(), new_text: format!("import {path}\n") },
    }
}

/// Sorts and de-duplicates the import block and drops unused *type* imports
/// (last segment capitalised and never used as an identifier). Returns `None`
/// when the file has no organisable block or it is already organised.
pub fn organize_imports(text: &str) -> Option<TextEdit> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let import_lines: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| l.trim_start().starts_with("import ")).map(|(i, _)| i).collect();
    let (&first, &last) = (import_lines.first()?, import_lines.last()?);
    // only a block made of import lines alone (comments between would be lost)
    if (first..=last).any(|i| !lines[i].trim_start().starts_with("import ")) {
        return None;
    }
    let toks = Lexer::new(text).tokenize().ok()?;
    let mut used: HashMap<String, usize> = HashMap::new();
    for t in &toks {
        if let TokenKind::Ident(n) = &t.kind {
            if t.span.start.line as usize > last + 1 || (t.span.start.line as usize) <= first {
                *used.entry(n.clone()).or_default() += 1;
            }
        }
    }
    // identifiers inside string interpolations
    for l in &lines[last + 1..] {
        let mut rest = *l;
        while let Some(p) = rest.find("${") {
            let seg: String = rest[p + 2..].chars().take_while(|c| *c != '}').collect();
            for w in seg.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
                if !w.is_empty() {
                    *used.entry(w.to_string()).or_default() += 1;
                }
            }
            rest = &rest[p + 2..];
        }
    }
    let mut paths: Vec<String> = Vec::new();
    for line in &lines[first..=last] {
        let path = line.trim().trim_start_matches("import ").trim().trim_end_matches(';').trim().to_string();
        let tail = path.rsplit('.').next().unwrap_or(&path).to_string();
        let type_import = tail.chars().next().is_some_and(char::is_uppercase);
        if type_import && !used.contains_key(&tail) {
            continue;
        }
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths.sort();
    let block: String = paths.iter().map(|p| format!("import {p}\n")).collect();
    let old: String = lines[first..=last].concat();
    if block == old {
        return None;
    }
    let end_line = (last + 1) as u32;
    Some(TextEdit {
        range: Range { start: Position { line: first as u32, character: 0 }, end: Position { line: end_line, character: 0 } },
        new_text: block,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_the_unresolved_name() {
        let m = "unresolved 'Watcher::create': no type, enum, or static method named 'Watcher' in scope (missing import?)";
        assert_eq!(missing_import_name(m).as_deref(), Some("Watcher"));
        assert_eq!(missing_import_name("undefined variable x"), None);
    }

    #[test]
    fn candidates_from_project_and_stdlib() {
        let project = vec![("a".to_string(), "namespace demo.model\n{\n    class Person\n    {\n    }\n}\n")];
        let stdlib = vec![("tinox.core.fs".to_string(), vec!["Watcher".to_string(), "Fs".to_string()])];
        assert_eq!(import_candidates("Person", &project, &stdlib), ["demo.model.Person"]);
        assert_eq!(import_candidates("Watcher", &project, &stdlib), ["tinox.core.fs"]);
        assert!(import_candidates("Nope", &project, &stdlib).is_empty());
    }

    fn apply(text: &str, e: &TextEdit) -> String {
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        let off = |p: Position| lines.iter().take(p.line as usize).map(|l| l.len()).sum::<usize>() + p.character as usize;
        let mut s = text.to_string();
        s.replace_range(off(e.range.start)..off(e.range.end), &e.new_text);
        s
    }

    #[test]
    fn import_goes_after_the_last_import_or_before_the_first_declaration() {
        let with = "import a.B\nimport a.C\n\nclass Main\n{\n}\n";
        assert_eq!(apply(with, &add_import_edit(with, "x.Y")), "import a.B\nimport a.C\nimport x.Y\n\nclass Main\n{\n}\n");
        let without = "// header\nclass Main\n{\n}\n";
        assert_eq!(apply(without, &add_import_edit(without, "x.Y")), "// header\nimport x.Y\n\nclass Main\n{\n}\n");
    }

    #[test]
    fn organize_sorts_dedupes_and_drops_unused_type_imports() {
        let src = "import z.Used\nimport a.Unused\nimport tinox.core.fs\nimport z.Used\n\nclass Main\n{\n    fnc f() -> Used\n    {\n        return Used::make()\n    }\n}\n";
        let e = organize_imports(src).unwrap();
        assert_eq!(apply(src, &e), "import tinox.core.fs\nimport z.Used\n\nclass Main\n{\n    fnc f() -> Used\n    {\n        return Used::make()\n    }\n}\n");
        let ok = "import a.B\nimport b.C\n\nclass Main\n{\n    fnc f() -> B\n    {\n        return C::make()\n    }\n}\n";
        assert!(organize_imports(ok).is_none(), "already organised");
        assert!(organize_imports("class Main\n{\n}\n").is_none());
        let commented = "import b.X\n// note\nimport a.X\nclass Main\n{\n    fnc f() -> X\n    {\n        return X::m()\n    }\n}\n";
        assert!(organize_imports(commented).is_none(), "comments inside the block are never discarded");
    }
}
