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
