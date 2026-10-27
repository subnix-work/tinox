//! Issue #495: diagnostics quality, debounce, staleness and cross-file refresh.
mod common;
use common::*;
use serde_json::json;

const GOOD: &str = "class Main\n{\n    fnc main() -> Int32\n    {\n        return 0\n    }\n}\n";
const BAD: &str = "class Main\n{\n    fnc main() -> Int32\n    {\n        return absent\n    }\n}\n";

fn has_undefined(d: &[serde_json::Value]) -> bool {
    d.iter().any(|x| x["message"].as_str().unwrap_or("").contains("undefined variable"))
}

#[test]
fn ranges_are_exact_and_carry_code_source_and_severity() {
    let root = temp_project("d_range");
    let file = root.join("Main.tnx");
    std::fs::write(&file, BAD).unwrap();
    let mut lsp = Lsp::start(&root, false);
    lsp.open(&file, BAD);
    let d = lsp.diagnostics_for(&file, |d| has_undefined(d));
    let e = d.iter().find(|x| x["message"].as_str().unwrap().contains("undefined variable")).unwrap();
    assert_eq!(e["source"], "tinox");
    assert_eq!(e["code"], "TINOX-TYPE");
    assert_eq!(e["severity"], 1);
    assert_eq!(e["range"]["start"]["line"], 4);
    let (a, b) = (e["range"]["start"]["character"].as_u64().unwrap(), e["range"]["end"]["character"].as_u64().unwrap());
    assert_eq!(b - a, "absent".len() as u64, "range must cover exactly the identifier: {e}");
    assert_eq!(e["range"]["end"]["line"], 4);
    lsp.shutdown();
}

#[test]
fn a_burst_of_edits_publishes_only_the_final_state() {
    let root = temp_project("d_burst");
    let file = root.join("Main.tnx");
    std::fs::write(&file, GOOD).unwrap();
    let mut lsp = Lsp::start(&root, false);
    lsp.open(&file, GOOD);
    lsp.diagnostics_for(&file, |_| true);
    let before = lsp.published(&file).len();
    let uri = url(&file);
    for v in 2..=21 {
        let text = if v % 2 == 0 { BAD } else { GOOD };
        lsp.notify("textDocument/didChange", json!({"textDocument": {"uri": uri, "version": v}, "contentChanges": [{"text": text}]}));
    }
    // version 21 is GOOD: the end state has no error and nothing stale may follow it
    lsp.drain(1500);
    let published = lsp.published(&file);
    let new = &published[before..];
    assert!(!new.is_empty(), "the final state must be analysed");
    assert!(new.len() <= 3, "debounce should collapse 20 edits, got {} publishes", new.len());
    assert!(new.last().unwrap().is_empty(), "last publish must reflect the final (valid) text: {new:?}");
    lsp.shutdown();
}

#[test]
fn out_of_order_older_versions_are_ignored() {
    let root = temp_project("d_order");
    let file = root.join("Main.tnx");
    std::fs::write(&file, GOOD).unwrap();
    let mut lsp = Lsp::start(&root, false);
    lsp.open(&file, GOOD);
    lsp.diagnostics_for(&file, |_| true);
    let uri = url(&file);
    lsp.notify("textDocument/didChange", json!({"textDocument": {"uri": uri, "version": 5}, "contentChanges": [{"text": GOOD}]}));
    lsp.notify("textDocument/didChange", json!({"textDocument": {"uri": uri, "version": 3}, "contentChanges": [{"text": BAD}]}));
    lsp.drain(800);
    assert!(lsp.published(&file).iter().all(|d| !has_undefined(d)), "a late older version must not win");
    lsp.shutdown();
}

#[test]
fn closing_a_document_clears_its_diagnostics() {
    let root = temp_project("d_close");
    let file = root.join("Main.tnx");
    std::fs::write(&file, BAD).unwrap();
    let mut lsp = Lsp::start(&root, false);
    lsp.open(&file, BAD);
    lsp.diagnostics_for(&file, |d| has_undefined(d));
    lsp.notify("textDocument/didClose", json!({"textDocument": {"uri": url(&file)}}));
    lsp.diagnostics_for(&file, |d| d.is_empty());
    lsp.shutdown();
}

#[test]
fn unsaved_edits_of_an_imported_file_refresh_its_users() {
    let root = temp_project("d_cross");
    let util = root.join("Util.tnx");
    let main = root.join("Main.tnx");
    let util_src = "class Util\n{\n    fnc answer() -> Int64\n    {\n        return 42\n    }\n}\n";
    let main_src = "import Util\n\nclass Main\n{\n    fnc main() -> Int32\n    {\n        const n: Int64 = Util::answer()\n        return 0\n    }\n}\n";
    std::fs::write(&util, util_src).unwrap();
    std::fs::write(&main, main_src).unwrap();
    let mut lsp = Lsp::start(&root, false);
    lsp.open(&util, util_src);
    lsp.open(&main, main_src);
    let clean = lsp.diagnostics_for(&main, |_| true);
    assert!(clean.is_empty(), "baseline must be clean: {clean:?}");

    // remove the method in the OPEN buffer of Util, without saving
    let broken = util_src.replace("answer", "other");
    lsp.notify("textDocument/didChange", json!({"textDocument": {"uri": url(&util), "version": 2}, "contentChanges": [{"text": broken}]}));
    let d = lsp.diagnostics_for(&main, |d| !d.is_empty());
    assert!(d[0]["message"].as_str().unwrap().contains("answer"), "{d:?}");

    // restore it: Main's diagnostics clear again, still unsaved
    lsp.notify("textDocument/didChange", json!({"textDocument": {"uri": url(&util), "version": 3}, "contentChanges": [{"text": util_src}]}));
    lsp.diagnostics_for(&main, |d| d.is_empty());
    assert_eq!(std::fs::read_to_string(&util).unwrap(), util_src, "disk was never touched");
    lsp.shutdown();
}
