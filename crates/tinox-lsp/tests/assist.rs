//! Issue #494: signature help and code actions through the real server.
mod common;
use common::*;
use serde_json::json;

#[test]
fn signature_help_for_project_function_static_and_member_calls() {
    let root = temp_project("sig");
    let file = root.join("Main.tnx");
    let src = "class Main\n{\n    fnc main() -> Int32\n    {\n        const a = Fs::copyFile(\"x\", \n        return 0\n    }\n}\n";
    std::fs::write(&file, src).unwrap();
    let mut lsp = Lsp::start(&root, false);
    assert_eq!(lsp.init["capabilities"]["signatureHelpProvider"]["triggerCharacters"], json!(["(", ","]));
    lsp.open(&file, src);
    let uri = url(&file);
    // cursor after `"x", ` on line 4
    let r = lsp.request("textDocument/signatureHelp", json!({"textDocument": {"uri": uri}, "position": pos(4, 36)}));
    let sig = &r["signatures"][r["activeSignature"].as_u64().unwrap() as usize];
    assert!(sig["label"].as_str().unwrap().contains("copyFile"), "{r}");
    assert_eq!(r["activeParameter"], 1, "{r}");
    assert_eq!(sig["parameters"].as_array().unwrap().len(), 2);
    // outside any call: nothing
    let none = lsp.request("textDocument/signatureHelp", json!({"textDocument": {"uri": uri}, "position": pos(7, 0)}));
    assert!(none.is_null());
    lsp.shutdown();
}

#[test]
fn quick_fix_adds_the_missing_import_and_applies_cleanly() {
    let root = temp_project("fix");
    let file = root.join("Main.tnx");
    let src = "class Main\n{\n    fnc main() -> Int32\n    {\n        const w = Watcher::create(10)\n        return 0\n    }\n}\n";
    std::fs::write(&file, src).unwrap();
    let mut lsp = Lsp::start(&root, false);
    lsp.open(&file, src);
    let diags = lsp.diagnostics_for(&file, |d| d.iter().any(|x| x["message"].as_str().unwrap_or("").contains("missing import")));
    let diag = diags.iter().find(|x| x["message"].as_str().unwrap().contains("missing import")).unwrap().clone();
    let actions = lsp.request(
        "textDocument/codeAction",
        json!({"textDocument": {"uri": url(&file)}, "range": diag["range"], "context": {"diagnostics": [diag], "only": ["quickfix"]}}),
    );
    let a = &actions[0];
    assert_eq!(a["title"], "Import tinox.core.fs", "{actions}");
    assert_eq!(a["isPreferred"], true);
    let edit = &a["edit"]["changes"][url(&file)][0];
    assert_eq!(edit["newText"], "import tinox.core.fs\n\n");
    // applying it makes the diagnostic disappear
    let fixed = format!("import tinox.core.fs\n\n{src}");
    lsp.notify("textDocument/didChange", json!({"textDocument": {"uri": url(&file), "version": 2}, "contentChanges": [{"text": fixed}]}));
    lsp.diagnostics_for(&file, |d| !d.iter().any(|x| x["message"].as_str().unwrap_or("").contains("missing import")));
    lsp.shutdown();
}

#[test]
fn organize_imports_is_offered_only_when_it_changes_something() {
    let root = temp_project("org");
    let file = root.join("Main.tnx");
    let messy = "import tinox.core.process\nimport tinox.core.fs\nimport tinox.core.fs\n\nclass Main\n{\n    fnc main() -> Int32\n    {\n        return 0\n    }\n}\n";
    std::fs::write(&file, messy).unwrap();
    let mut lsp = Lsp::start(&root, false);
    lsp.open(&file, messy);
    let uri = url(&file);
    let range = json!({"start": pos(0, 0), "end": pos(0, 0)});
    let r = lsp.request("textDocument/codeAction", json!({"textDocument": {"uri": uri}, "range": range, "context": {"diagnostics": [], "only": ["source.organizeImports"]}}));
    assert_eq!(r[0]["kind"], "source.organizeImports", "{r}");
    assert_eq!(r[0]["edit"]["changes"][&uri][0]["newText"], "import tinox.core.fs\nimport tinox.core.process\n");

    let tidy = "import tinox.core.fs\nimport tinox.core.process\n\nclass Main\n{\n}\n";
    lsp.notify("textDocument/didChange", json!({"textDocument": {"uri": uri, "version": 2}, "contentChanges": [{"text": tidy}]}));
    let r = lsp.request("textDocument/codeAction", json!({"textDocument": {"uri": uri}, "range": range, "context": {"diagnostics": [], "only": ["source.organizeImports"]}}));
    assert!(r.is_null(), "{r}");
    lsp.shutdown();
}
