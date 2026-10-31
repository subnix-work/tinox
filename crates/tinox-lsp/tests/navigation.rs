//! Issue #491: references, rename and workspace symbols through the real server.
mod common;
use common::*;
use serde_json::json;

const PERSON: &str = "namespace demo.model\n{\n    class Person\n    {\n        var id: Int64\n        var label: String\n    }\n}\n";
const SERVICE: &str = "import demo.model.Person\n\nnamespace demo.service\n{\n    class Service\n    {\n        fn make(count: Int64) -> Person\n        {\n            const p = Person { id: count, label: \"x\" }\n            return p\n        }\n    }\n}\n";

fn setup(rename_files: bool, name: &str) -> (std::path::PathBuf, Lsp) {
    let root = temp_project(name);
    std::fs::create_dir_all(root.join("demo/model")).unwrap();
    std::fs::create_dir_all(root.join("demo/service")).unwrap();
    std::fs::write(root.join("demo/model/Person.tnx"), PERSON).unwrap();
    std::fs::write(root.join("demo/service/Service.tnx"), SERVICE).unwrap();
    let mut lsp = Lsp::start(&root, rename_files);
    lsp.open(&root.join("demo/model/Person.tnx"), PERSON);
    lsp.open(&root.join("demo/service/Service.tnx"), SERVICE);
    (root, lsp)
}

#[test]
fn capabilities_advertise_navigation() {
    let root = temp_project("caps");
    let lsp = Lsp::start(&root, false);
    let caps = &lsp.init["capabilities"];
    assert_eq!(caps["referencesProvider"], true);
    assert_eq!(caps["renameProvider"]["prepareProvider"], true);
    assert_eq!(caps["workspaceSymbolProvider"], true);
    lsp.shutdown();
}

#[test]
fn references_span_files() {
    let (root, mut lsp) = setup(true, "refs");
    let uri = url(&root.join("demo/model/Person.tnx"));
    let r = lsp.request(
        "textDocument/references",
        json!({"textDocument": {"uri": uri}, "position": pos(2, 12), "context": {"includeDeclaration": true}}),
    );
    let locs = r.as_array().unwrap();
    assert_eq!(locs.len(), 4, "{r}");
    assert!(locs.iter().any(|l| l["uri"].as_str().unwrap().ends_with("Service.tnx")));
    lsp.shutdown();
}

#[test]
fn rename_type_renames_the_file_when_the_client_supports_it() {
    let (root, mut lsp) = setup(true, "ren");
    let uri = url(&root.join("demo/model/Person.tnx"));
    let prep = lsp.request("textDocument/prepareRename", json!({"textDocument": {"uri": uri}, "position": pos(2, 12)}));
    assert_eq!(prep["placeholder"], "Person");
    let r = lsp.request("textDocument/rename", json!({"textDocument": {"uri": uri}, "position": pos(2, 12), "newName": "Human"}));
    let ops = r["documentChanges"].as_array().unwrap();
    assert!(ops.iter().any(|o| o["kind"] == "rename" && o["newUri"].as_str().unwrap().ends_with("Human.tnx")), "{r}");
    let edits: usize = ops.iter().filter_map(|o| o["edits"].as_array()).map(|e| e.len()).sum();
    assert_eq!(edits, 4);
    lsp.shutdown();
}

#[test]
fn rename_without_file_operation_support_warns_the_user() {
    let (root, mut lsp) = setup(false, "warn");
    let uri = url(&root.join("demo/model/Person.tnx"));
    let r = lsp.request("textDocument/rename", json!({"textDocument": {"uri": uri}, "position": pos(2, 12), "newName": "Human"}));
    assert!(r["changes"].is_object(), "{r}");
    // the warning arrives as a window/showMessage notification
    let _ = lsp.request("textDocument/prepareRename", json!({"textDocument": {"uri": uri}, "position": pos(2, 12)}));
    assert!(
        lsp.inbox.iter().any(|m| m["method"] == "window/showMessage" && m["params"]["message"].as_str().unwrap().contains("rename")),
        "no showMessage in {:?}",
        lsp.inbox
    );
    lsp.shutdown();
}

#[test]
fn unsafe_rename_is_refused_with_a_reason() {
    let (root, mut lsp) = setup(true, "refuse");
    let uri = url(&root.join("demo/model/Person.tnx"));
    // `Int64` is not a project symbol
    let r = lsp.request_raw("textDocument/prepareRename", json!({"textDocument": {"uri": uri}, "position": pos(4, 18)}));
    assert!(r["error"]["message"].as_str().unwrap().contains("not declared in this project"), "{r}");
    lsp.shutdown();
}

#[test]
fn workspace_symbols_find_types_and_members() {
    let (_root, mut lsp) = setup(true, "syms");
    let r = lsp.request("workspace/symbol", json!({"query": "serv"}));
    let names: Vec<&str> = r.as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"Service"), "{names:?}");
    let r = lsp.request("workspace/symbol", json!({"query": "make"}));
    assert_eq!(r[0]["containerName"], "Service");
    lsp.shutdown();
}

// Issue #555: a workspace larger than the scan budget is reported as partial and never treated as
// proof that a rename is safe.
fn big_project(name: &str, extra: usize) -> (std::path::PathBuf, String, String) {
    let root = temp_project(name);
    std::fs::create_dir_all(root.join("demo/model")).unwrap();
    std::fs::write(root.join("demo/model/Person.tnx"), PERSON).unwrap();
    for i in 0..extra {
        // later in path order than demo/, so these are the files the budget drops
        let dir = root.join(format!("zzz/pkg{i}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("Filler{i}.tnx")), format!("class Filler{i}\n{{\n    var n: Int64\n}}\n")).unwrap();
    }
    let uri = url(&root.join("demo/model/Person.tnx"));
    (root, uri, PERSON.to_string())
}

#[test]
fn incomplete_workspace_index_refuses_rename_and_warns_for_references() {
    let (root, uri, text) = big_project("budget", 6);
    let mut lsp = Lsp::start_with_env(&root, false, &[("TINOX_LSP_MAX_FILES", "3")]);
    lsp.open(&root.join("demo/model/Person.tnx"), &text);
    let r = lsp.request_raw("textDocument/rename", json!({"textDocument": {"uri": uri}, "position": pos(2, 12), "newName": "Human"}));
    let message = r["error"]["message"].as_str().unwrap_or_default().to_string();
    assert!(message.contains("rename refused") && message.contains("not indexed") && message.contains("TINOX_LSP_MAX_FILES"), "{r}");
    let prep = lsp.request_raw("textDocument/prepareRename", json!({"textDocument": {"uri": uri}, "position": pos(2, 12)}));
    assert!(prep["error"]["message"].as_str().unwrap_or_default().contains("rename refused"), "{prep}");
    // references still answer, but the user is told the index is partial (once)
    let refs = lsp.request("textDocument/references", json!({"textDocument": {"uri": uri}, "position": pos(2, 12), "context": {"includeDeclaration": true}}));
    assert!(refs.is_array(), "{refs}");
    let _ = lsp.request("textDocument/references", json!({"textDocument": {"uri": uri}, "position": pos(2, 12), "context": {"includeDeclaration": true}}));
    let warnings = lsp
        .inbox
        .iter()
        .filter(|m| m["method"] == "window/showMessage" && m["params"]["message"].as_str().unwrap_or_default().contains("workspace index incomplete"))
        .count();
    assert_eq!(warnings, 1, "{:?}", lsp.inbox);
    lsp.shutdown();
}

#[test]
fn complete_index_within_budget_still_renames() {
    let (root, uri, text) = big_project("budget-ok", 2);
    let mut lsp = Lsp::start_with_env(&root, false, &[("TINOX_LSP_MAX_FILES", "10")]);
    lsp.open(&root.join("demo/model/Person.tnx"), &text);
    let r = lsp.request("textDocument/rename", json!({"textDocument": {"uri": uri}, "position": pos(2, 12), "newName": "Human"}));
    assert!(r["changes"].is_object() || r["documentChanges"].is_array(), "{r}");
    lsp.shutdown();
}

#[test]
fn invalid_budget_setting_is_reported_not_ignored() {
    let (root, _uri, _text) = big_project("budget-bad", 0);
    let mut lsp = Lsp::start_with_env(&root, false, &[("TINOX_LSP_MAX_FILES", "lots")]);
    // `initialized` is a notification; its warning may trail the first responses
    let mut seen = false;
    for _ in 0..50 {
        let _ = lsp.request("workspace/symbol", json!({"query": "x"}));
        seen = lsp.inbox.iter().any(|m| m["method"] == "window/showMessage" && m["params"]["message"].as_str().unwrap_or_default().contains("TINOX_LSP_MAX_FILES"));
        if seen {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(seen, "{:?}", lsp.inbox);
    lsp.shutdown();
}

/// Issue #555 benchmark (run with `-- --ignored --nocapture`): dense and deep layouts above the
/// default 5000-file budget, first (cold) and repeated (cached) workspace symbol requests.
#[test]
#[ignore]
fn large_workspace_timings() {
    for (label, per_dir) in [("dense (60 files/dir)", 60usize), ("deep (1 file/dir)", 1)] {
        let root = temp_project(&format!("bench-{per_dir}"));
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::write(root.join("a/Person.tnx"), PERSON).unwrap();
        let total = 6000;
        for i in 0..total {
            let dir = root.join(format!("z/d{}", i / per_dir));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("Filler{i}.tnx")), format!("class Filler{i}\n{{\n    var n: Int64\n}}\n")).unwrap();
        }
        let mut lsp = Lsp::start_with_env(&root, false, &[("TINOX_LSP_MAX_FILES", "20000")]);
        lsp.open(&root.join("a/Person.tnx"), PERSON);
        let mut times = Vec::new();
        for _ in 0..4 {
            let t = std::time::Instant::now();
            let r = lsp.request("workspace/symbol", json!({"query": "Filler59"}));
            assert!(r.is_array());
            times.push(t.elapsed().as_millis());
        }
        println!("{label}: {total} files, workspace/symbol ms (cold, then cached): {times:?}");
        lsp.shutdown();
    }
}
