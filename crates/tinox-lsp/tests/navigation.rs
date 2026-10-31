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

// Issue #554: same-named members of unrelated types are told apart with the checker's receiver types.
const CA: &str = "class Alpha\n{\n    var label: String\n\n    fn describe() -> String\n    {\n        return this.label\n    }\n}\n";
const CB: &str = "class Beta\n{\n    var label: String\n\n    fn describe() -> String\n    {\n        return this.label\n    }\n}\n";
const CC: &str = "class Gamma extends Alpha\n{\n    fn describe() -> String\n    {\n        return \"gamma\"\n    }\n}\n";
const USE: &str = "import Alpha\nimport Beta\nimport Gamma\n\nclass Use\n{\n    fnc run(a: Alpha, b: Beta, g: Gamma) -> String\n    {\n        const x = a.describe()\n        const y = b.describe()\n        const z = g.describe()\n        const l = a.label\n        const m = b.label\n        return x + y + z + l + m\n    }\n}\n";

fn typed_setup(name: &str, extra: &[(&str, &str)]) -> (std::path::PathBuf, Lsp) {
    let root = temp_project(name);
    let mut files = vec![("Alpha.tnx", CA), ("Beta.tnx", CB), ("Gamma.tnx", CC), ("Use.tnx", USE)];
    files.extend_from_slice(extra);
    for (n, t) in &files {
        std::fs::write(root.join(n), t).unwrap();
    }
    let mut lsp = Lsp::start(&root, false);
    for (n, t) in &files {
        lsp.open(&root.join(n), t);
    }
    (root, lsp)
}

fn lines_of(r: &serde_json::Value, file: &str) -> Vec<u64> {
    let mut v: Vec<u64> = r
        .as_array()
        .unwrap()
        .iter()
        .filter(|l| l["uri"].as_str().unwrap().ends_with(file))
        .map(|l| l["range"]["start"]["line"].as_u64().unwrap())
        .collect();
    v.sort();
    v
}

#[test]
fn references_distinguish_same_named_members_of_unrelated_types() {
    let (root, mut lsp) = typed_setup("typed-refs", &[]);
    let uri = url(&root.join("Alpha.tnx"));
    // `describe` declared in Alpha (line 4, col 7)
    let r = lsp.request("textDocument/references", json!({"textDocument": {"uri": uri}, "position": pos(4, 8), "context": {"includeDeclaration": true}}));
    // Alpha.describe, the override in Gamma, a.describe() and g.describe() -- never Beta's
    assert_eq!(lines_of(&r, "Beta.tnx"), Vec::<u64>::new(), "{r}");
    assert_eq!(lines_of(&r, "Alpha.tnx"), vec![4], "{r}");
    assert_eq!(lines_of(&r, "Gamma.tnx"), vec![2], "{r}");
    assert_eq!(lines_of(&r, "Use.tnx"), vec![8, 10], "{r}");
    // the field `label` of Beta
    let uri_b = url(&root.join("Beta.tnx"));
    let r = lsp.request("textDocument/references", json!({"textDocument": {"uri": uri_b}, "position": pos(2, 9), "context": {"includeDeclaration": true}}));
    assert_eq!(lines_of(&r, "Alpha.tnx"), Vec::<u64>::new(), "{r}");
    assert_eq!(lines_of(&r, "Beta.tnx"), vec![2, 6], "{r}");
    assert_eq!(lines_of(&r, "Use.tnx"), vec![12], "{r}");
    lsp.shutdown();
}

#[test]
fn rename_of_a_shared_member_name_edits_only_the_intended_type() {
    let (root, mut lsp) = typed_setup("typed-rename", &[]);
    let uri = url(&root.join("Alpha.tnx"));
    let r = lsp.request("textDocument/rename", json!({"textDocument": {"uri": uri}, "position": pos(2, 9), "newName": "title"}));
    let changes = r["changes"].as_object().unwrap();
    let touched: Vec<&String> = changes.keys().collect();
    assert!(touched.iter().all(|u| !u.ends_with("Beta.tnx")), "{r}");
    let edits = |file: &str| changes.iter().find(|(u, _)| u.ends_with(file)).map(|(_, e)| e.as_array().unwrap().len()).unwrap_or(0);
    assert_eq!(edits("Alpha.tnx"), 2, "{r}"); // declaration and this.label
    assert_eq!(edits("Use.tnx"), 1, "{r}"); // a.label only, not b.label
    // renaming the overridden method moves the override and every call through the hierarchy
    let r = lsp.request("textDocument/rename", json!({"textDocument": {"uri": uri}, "position": pos(4, 8), "newName": "text"}));
    let changes = r["changes"].as_object().unwrap();
    assert!(changes.keys().all(|u| !u.ends_with("Beta.tnx")), "{r}");
    assert!(changes.keys().any(|u| u.ends_with("Gamma.tnx")), "{r}");
    lsp.shutdown();
}

#[test]
fn rename_refuses_when_a_receiver_type_cannot_be_resolved() {
    let unknown = "class Mystery\n{\n    fnc run(items: List<Alpha>) -> Nothing\n    {\n        const f = (v) => v.label\n    }\n}\n";
    let (root, mut lsp) = typed_setup("typed-unresolved", &[("Mystery.tnx", unknown)]);
    let uri = url(&root.join("Alpha.tnx"));
    let r = lsp.request_raw("textDocument/rename", json!({"textDocument": {"uri": uri}, "position": pos(2, 9), "newName": "title"}));
    let message = r["error"]["message"].as_str().unwrap_or_default().to_string();
    assert!(message.contains("no resolved receiver type") && message.contains("Mystery.tnx"), "{r}");
    lsp.shutdown();
}

#[test]
fn checked_member_navigation_uses_unsaved_buffers_and_utf16_columns() {
    let root = temp_project("typed-overlay");
    // On disk `Use` does not touch Beta.label at all; the editor buffer does, after an emoji.
    let disk_use = "import Alpha\nimport Beta\n\nclass Use\n{\n    fnc run(a: Alpha, b: Beta) -> String\n    {\n        return a.describe()\n    }\n}\n";
    let buffer_use = "import Alpha\nimport Beta\n\nclass Use\n{\n    fnc run(a: Alpha, b: Beta) -> String\n    {\n        const t = \"\u{1F600}\" + b.label\n        return a.describe() + t\n    }\n}\n";
    for (n, t) in [("Alpha.tnx", CA), ("Beta.tnx", CB), ("Use.tnx", disk_use)] {
        std::fs::write(root.join(n), t).unwrap();
    }
    let mut lsp = Lsp::start(&root, false);
    lsp.open(&root.join("Alpha.tnx"), CA);
    lsp.open(&root.join("Beta.tnx"), CB);
    lsp.open(&root.join("Use.tnx"), buffer_use);
    let uri_b = url(&root.join("Beta.tnx"));
    let r = lsp.request("textDocument/references", json!({"textDocument": {"uri": uri_b}, "position": pos(2, 9), "context": {"includeDeclaration": false}}));
    let in_use: Vec<&serde_json::Value> = r.as_array().unwrap().iter().filter(|l| l["uri"].as_str().unwrap().ends_with("Use.tnx")).collect();
    assert_eq!(in_use.len(), 1, "{r}");
    // `        const t = "😀" + b.label`: the emoji is two UTF-16 units, so `label` starts at column 27
    assert_eq!(in_use[0]["range"]["start"], json!({"line": 7, "character": 27}), "{r}");
    lsp.shutdown();
}
