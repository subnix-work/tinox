//! `textDocument/formatting` and `rangeFormatting` on top of `tinox fmt`
//! (issue #492).
//!
//! The formatter is layout-only and opinionated, so the editor's tab size and
//! `insertSpaces` options are deliberately not applied: the result is always
//! byte-identical to `tinox fmt`. A document that does not parse is never
//! touched; the client gets the reason instead.

use tower_lsp::lsp_types::{Position, Range, TextEdit};

/// Formats `text` exactly like `tinox fmt`, or explains why it cannot.
pub fn format_document(text: &str) -> Result<String, String> {
    let tokens = tinox_lexer::Lexer::new(text)
        .tokenize()
        .map_err(|errors| format!("cannot format: {}", errors.first().map(|e| e.message.clone()).unwrap_or_default()))?;
    tinox_parser::Parser::new(tokens).parse().map_err(|bag| {
        let first = bag.errors.first();
        format!(
            "cannot format a file with syntax errors ({}{})",
            first.map(|e| e.message.as_str()).unwrap_or("parse error"),
            first.map(|e| format!(" at line {}", e.span.start.line)).unwrap_or_default()
        )
    })?;
    tinox_parser::format_source(text).map_err(|errors| format!("cannot format: {}", errors.first().map(|e| e.message.clone()).unwrap_or_default()))
}

/// One changed block of whole lines: `old[start..end]` becomes `new_lines`.
#[derive(Debug, PartialEq)]
pub struct Hunk {
    pub start: usize,
    pub end: usize,
    pub new_lines: Vec<String>,
}

/// Longest-common-subsequence diff over lines, after trimming the common
/// prefix and suffix. Bounded: beyond ~16M cells the middle block is
/// replaced as a single hunk instead.
pub fn diff_lines(old: &str, new: &str) -> Vec<Hunk> {
    let a: Vec<&str> = old.split_inclusive('\n').collect();
    let b: Vec<&str> = new.split_inclusive('\n').collect();
    let mut pre = 0;
    while pre < a.len() && pre < b.len() && a[pre] == b[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < a.len() - pre && suf < b.len() - pre && a[a.len() - 1 - suf] == b[b.len() - 1 - suf] {
        suf += 1;
    }
    let (am, bm) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    if am.is_empty() && bm.is_empty() {
        return Vec::new();
    }
    let single = || vec![Hunk { start: pre, end: pre + am.len(), new_lines: bm.iter().map(|s| s.to_string()).collect() }];
    if am.is_empty() || bm.is_empty() || am.len().saturating_mul(bm.len()) > 16_000_000 {
        return single();
    }
    // lcs[i][j] = LCS length of am[i..], bm[j..]
    let (n, m) = (am.len(), bm.len());
    let mut lcs = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[at(i, j)] = if am[i] == bm[j] { lcs[at(i + 1, j + 1)] + 1 } else { lcs[at(i + 1, j)].max(lcs[at(i, j + 1)]) };
        }
    }
    let mut hunks: Vec<Hunk> = Vec::new();
    let (mut i, mut j) = (0, 0);
    let mut open: Option<Hunk> = None;
    let flush = |open: &mut Option<Hunk>, hunks: &mut Vec<Hunk>| {
        if let Some(h) = open.take() {
            hunks.push(h);
        }
    };
    while i < n || j < m {
        if i < n && j < m && am[i] == bm[j] {
            flush(&mut open, &mut hunks);
            i += 1;
            j += 1;
        } else if j < m && (i == n || lcs[at(i, j + 1)] >= lcs[at(i + 1, j)]) {
            let h = open.get_or_insert(Hunk { start: pre + i, end: pre + i, new_lines: Vec::new() });
            h.new_lines.push(bm[j].to_string());
            j += 1;
        } else {
            let h = open.get_or_insert(Hunk { start: pre + i, end: pre + i, new_lines: Vec::new() });
            h.end = pre + i + 1;
            i += 1;
        }
    }
    flush(&mut open, &mut hunks);
    // A block replaced line for line is a series of independent edits: finer
    // edits keep cursors and undo granular and let range formatting pick lines.
    hunks
        .into_iter()
        .flat_map(|h| {
            if h.end - h.start == h.new_lines.len() && h.new_lines.len() > 1 {
                h.new_lines
                    .into_iter()
                    .enumerate()
                    .map(|(k, l)| Hunk { start: h.start + k, end: h.start + k + 1, new_lines: vec![l] })
                    .collect::<Vec<_>>()
            } else {
                vec![h]
            }
        })
        .collect()
}

fn utf16_len(s: &str) -> u32 {
    s.chars().map(|c| c.len_utf16() as u32).sum()
}

/// Converts hunks into LSP edits over `old`.
pub fn to_edits(old: &str, hunks: &[Hunk]) -> Vec<TextEdit> {
    let lines: Vec<&str> = old.split_inclusive('\n').collect();
    let end_of_doc = || {
        let last = lines.last().copied().unwrap_or("");
        if last.ends_with('\n') {
            Position { line: lines.len() as u32, character: 0 }
        } else {
            Position { line: lines.len().saturating_sub(1) as u32, character: utf16_len(last) }
        }
    };
    hunks
        .iter()
        .map(|h| {
            let start = if h.start >= lines.len() { end_of_doc() } else { Position { line: h.start as u32, character: 0 } };
            let end = if h.end >= lines.len() { end_of_doc() } else { Position { line: h.end as u32, character: 0 } };
            TextEdit { range: Range { start, end }, new_text: h.new_lines.concat() }
        })
        .collect()
}

/// Edits turning `text` into its formatted form (empty when already formatted).
pub fn format_edits(text: &str) -> Result<Vec<TextEdit>, String> {
    let formatted = format_document(text)?;
    Ok(to_edits(text, &diff_lines(text, &formatted)))
}

/// Like [`format_edits`] but only for hunks that touch the lines of `range`.
pub fn format_range_edits(text: &str, range: Range) -> Result<Vec<TextEdit>, String> {
    let formatted = format_document(text)?;
    let hunks: Vec<Hunk> = diff_lines(text, &formatted)
        .into_iter()
        .filter(|h| {
            let (first, last) = (range.start.line as usize, range.end.line as usize);
            // an insertion (start == end) at line k touches line k
            h.start <= last && (h.end > first || (h.end == h.start && h.start >= first))
        })
        .collect();
    Ok(to_edits(text, &hunks))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(text: &str, mut edits: Vec<TextEdit>) -> String {
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        let offset = |p: Position| -> usize {
            let mut off: usize = lines.iter().take(p.line as usize).map(|l| l.len()).sum();
            if let Some(l) = lines.get(p.line as usize) {
                let mut units = 0;
                for (i, c) in l.char_indices() {
                    if units >= p.character {
                        return off + i;
                    }
                    units += c.len_utf16() as u32;
                }
                off += l.len();
            }
            off
        };
        edits.sort_by_key(|e| std::cmp::Reverse((e.range.start.line, e.range.start.character)));
        let mut s = text.to_string();
        for e in edits {
            let (a, b) = (offset(e.range.start), offset(e.range.end));
            s.replace_range(a..b, &e.new_text);
        }
        s
    }

    const MESSY: &str = "class Main {\n  fnc main() -> Int32 {\n        println(\"a\")\n   return 0 }\n}\n";

    #[test]
    fn edits_reproduce_the_cli_formatter_and_are_idempotent() {
        let formatted = format_document(MESSY).unwrap();
        assert_ne!(formatted, MESSY);
        assert_eq!(apply(MESSY, format_edits(MESSY).unwrap()), formatted);
        assert!(format_edits(&formatted).unwrap().is_empty(), "second pass must be a no-op");
        assert_eq!(format_document(&formatted).unwrap(), formatted);
    }

    #[test]
    fn syntax_errors_are_reported_not_formatted() {
        let err = format_edits("class Main {").unwrap_err();
        assert!(err.contains("syntax errors"), "{err}");
    }

    #[test]
    fn unchanged_lines_are_not_in_the_edit() {
        let src = "// keep me\nclass Main {\n  fnc main() -> Int32 {\n        return 0 }\n}\n";
        let edits = format_edits(src).unwrap();
        assert!(edits.iter().all(|e| e.range.start.line >= 1), "{edits:?}");
        assert_eq!(apply(src, edits), format_document(src).unwrap());
    }

    #[test]
    fn range_formatting_only_touches_the_selected_lines() {
        // two messy methods; format only the first one's lines
        let src = "class A {\n  fnc a() -> Int32 {\n          return 1 }\n  fnc b() -> Int32 {\n          return 2 }\n}\n";
        let full = apply(src, format_edits(src).unwrap());
        let range = Range { start: Position { line: 1, character: 0 }, end: Position { line: 2, character: 0 } };
        let partial = apply(src, format_range_edits(src, range).unwrap());
        assert_ne!(partial, src);
        assert_ne!(partial, full, "the second method must stay untouched");
        assert!(partial.contains("          return 2 }"), "{partial}");
    }

    #[test]
    fn diff_handles_documents_without_trailing_newline() {
        let hunks = diff_lines("a\nb", "a\nb\n");
        let edits = to_edits("a\nb", &hunks);
        assert_eq!(apply("a\nb", edits), "a\nb\n");
        let hunks = diff_lines("", "x\n");
        assert_eq!(apply("", to_edits("", &hunks)), "x\n");
    }

    #[test]
    fn examples_format_idempotently() {
        // Deeply nested sources recurse deeply in the parser; give the test
        // the stack the CLI runs with.
        std::thread::Builder::new().stack_size(256 << 20).spawn(examples_format_idempotently_body).unwrap().join().unwrap();
    }

    fn examples_format_idempotently_body() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/e2e");
        let mut checked = 0;
        for entry in std::fs::read_dir(dir).unwrap().flatten().take(400) {
            let p = if entry.path().is_dir() { entry.path().join("Main.tnx") } else { entry.path() };
            if p.extension().is_none_or(|e| e != "tnx") || !p.is_file() {
                continue;
            }
            let src = std::fs::read_to_string(&p).unwrap();
            let Ok(once) = format_document(&src) else { continue };
            let twice = format_document(&once).unwrap_or_else(|e| panic!("{}: reformat failed: {e}", p.display()));
            assert_eq!(once, twice, "{} is not idempotent", p.display());
            assert_eq!(apply(&src, format_edits(&src).unwrap()), once, "{}", p.display());
            checked += 1;
        }
        assert!(checked > 50, "only {checked} files checked");
    }
}
