//! Bug 467: completion panicked (and never answered) when the cursor sat in a
//! line holding multi-byte characters, because a debug snippet was sliced at an
//! arbitrary byte offset.
mod common;
use common::*;
use serde_json::json;

#[test]
fn completion_in_a_line_with_multibyte_characters_is_answered() {
    let root = temp_project("mb");
    let file = root.join("Main.tnx");
    let src = "class Main\n{\n    // \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\n    fnc main() -> Int32\n    {\n        return 0\n    }\n}\n";
    std::fs::write(&file, src).unwrap();
    let mut lsp = Lsp::start(&root, false);
    lsp.open(&file, src);
    let uri = url(&file);
    for col in [0, 5, 6, 7, 20, 26, 33] {
        // `request` blocks until the server answers: a panic would hang/fail here
        let _ = lsp.request("textDocument/completion", json!({"textDocument": {"uri": uri}, "position": pos(2, col)}));
    }
    // the server must still be usable afterwards
    let _ = lsp.request("textDocument/documentSymbol", json!({"textDocument": {"uri": uri}}));
    lsp.shutdown();
}
