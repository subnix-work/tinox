//! Issue #493: semantic tokens (full, delta, range) through the real server.
mod common;
use common::*;
use serde_json::json;

const SRC: &str = "class Main\n{\n    fnc main() -> Int32\n    {\n        const n: Int64 = 1\n        return 0\n    }\n}\n";

#[test]
fn full_delta_and_range() {
    let root = temp_project("sem");
    let file = root.join("Main.tnx");
    std::fs::write(&file, SRC).unwrap();
    let mut lsp = Lsp::start(&root, false);
    let opts = lsp.init["capabilities"]["semanticTokensProvider"].clone();
    assert_eq!(opts["full"]["delta"], true);
    assert_eq!(opts["range"], true);
    assert!(opts["legend"]["tokenTypes"].as_array().unwrap().iter().any(|t| t == "method"));
    lsp.open(&file, SRC);
    let uri = url(&file);

    let full = lsp.request("textDocument/semanticTokens/full", json!({"textDocument": {"uri": uri}}));
    let data = full["data"].as_array().unwrap();
    assert!(!data.is_empty() && data.len() % 5 == 0);
    let id = full["resultId"].as_str().unwrap().to_string();

    // unchanged document: an empty edit list
    let d = lsp.request("textDocument/semanticTokens/full/delta", json!({"textDocument": {"uri": uri}, "previousResultId": id}));
    assert_eq!(d["edits"].as_array().unwrap().len(), 0, "{d}");

    // changed document: one edit, and a new result id
    lsp.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri, "version": 2}, "contentChanges": [{"text": SRC.replace("const n:", "var m:")}]}),
    );
    let new_id = d["resultId"].as_str().unwrap().to_string();
    let d2 = lsp.request("textDocument/semanticTokens/full/delta", json!({"textDocument": {"uri": uri}, "previousResultId": new_id}));
    assert_eq!(d2["edits"].as_array().unwrap().len(), 1, "{d2}");

    // an unknown previous id falls back to a full answer
    let d3 = lsp.request("textDocument/semanticTokens/full/delta", json!({"textDocument": {"uri": uri}, "previousResultId": "nope"}));
    assert!(d3["data"].is_array(), "{d3}");

    // range: only line 4
    let r = lsp.request(
        "textDocument/semanticTokens/range",
        json!({"textDocument": {"uri": uri}, "range": {"start": pos(4, 0), "end": pos(4, 40)}}),
    );
    let rd = r["data"].as_array().unwrap();
    assert!(!rd.is_empty() && rd.len() < data.len());
    assert_eq!(rd[0], 4, "first token of a range answer is relative to line 0: {rd:?}");
    lsp.shutdown();
}
