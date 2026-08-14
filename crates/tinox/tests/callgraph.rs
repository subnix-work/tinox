//! Issue #186: `tinox graph` end-to-end coverage. Runs the real compiled
//! `tinox` binary against real example projects (not the callgraph.rs
//! module's internals directly) -- `examples/rest_with_mini` is the
//! issue's own suggested prototype target, chosen specifically because
//! `UserController.getUser` calls `findUserIndex(users, id)` with no
//! receiver at all (a same-class method call written as a bare
//! identifier, the load-bearing edge case this test exists to catch --
//! see CLAUDE.md's "tinox graph" section for why this is real, not
//! hypothetical). One smaller smoke test per remaining entry-point kind
//! (CLI/WebSocket/AMQP) proves the full matrix resolves real entry points
//! end to end, without re-testing the walker's every branch (that's what
//! covers those in more depth, if ever needed).

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// Runs `tinox graph <entry> --out <out>` and returns the written file's
/// contents. Panics with the process's stderr on failure -- a test
/// failure should show WHY the compiler rejected it, not just "false".
fn run_graph(entry_rel: &str) -> String {
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let out = std::env::temp_dir().join(format!(
        "tinox-graph-test-{}-{}.mmd",
        entry_rel.replace(['/', '.'], "_"),
        std::process::id()
    ));
    let _ = std::fs::remove_file(&out);

    let output = Command::new(tinox)
        .arg("graph")
        .arg(repo_root().join(entry_rel))
        .arg("--out")
        .arg(&out)
        .output()
        .expect("spawn tinox graph");

    assert!(
        output.status.success(),
        "tinox graph {} failed:\nstdout: {}\nstderr: {}",
        entry_rel,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let mmd = std::fs::read_to_string(&out).unwrap_or_else(|e| panic!("read {}: {e}", out.display()));
    let _ = std::fs::remove_file(&out);
    mmd
}

#[test]
fn rest_with_mini_resolves_bare_same_class_call() {
    let mmd = run_graph("examples/rest_with_mini/Main.tnx");

    assert!(mmd.starts_with("flowchart TD"), "not a Mermaid flowchart:\n{mmd}");

    // Every @GET/@POST handler is an entry node.
    for entry in ["UserController_listUsers", "UserController_getUser", "UserController_createUser", "UserController_renameUser"] {
        assert!(mmd.contains(&format!("class {entry} entry")), "missing entry node {entry}:\n{mmd}");
    }

    // The load-bearing case: getUser calls findUserIndex(users, id) with
    // no receiver at all -- must resolve as a same-class direct edge, not
    // an unresolved call.
    assert!(
        mmd.contains("UserController_getUser --> UserController_findUserIndex"),
        "bare same-class call to findUserIndex not resolved as a direct edge:\n{mmd}"
    );

    // findUserIndex calls users.len() -- List isn't a project class, so
    // this must stop at the boundary (external), not silently vanish.
    assert!(mmd.contains("UserController_findUserIndex --> List_len"), "missing List.len edge:\n{mmd}");
    assert!(mmd.contains("class List_len external"), "List.len should be marked external:\n{mmd}");

    // getUser's ctx.response.status(...).json(...) chain can't be
    // statically resolved (receiver isn't a bare ident/This/New) -- must
    // show up as unresolved, not be silently dropped.
    assert!(mmd.contains("unresolved[\"? (unresolved calls)\"]"), "unresolved sink node missing:\n{mmd}");
    assert!(mmd.contains("UserController_getUser -.- unresolved"), "getUser's unresolved calls not linked:\n{mmd}");
}

#[test]
fn cli_command_entry_point_found() {
    let mmd = run_graph("examples/GreetCommand.tnx");
    assert!(mmd.contains("class GreetCommand_run entry"), "CLI @Command entry point (fixed `run` method) not found:\n{mmd}");
}

#[test]
fn websocket_entry_points_found_and_stop_at_stdlib_boundary() {
    let mmd = run_graph("examples/ws_echo_annotated/Main.tnx");
    for entry in ["EchoEndpoint_onOpen", "EchoEndpoint_onMessage", "EchoEndpoint_onClose"] {
        assert!(mmd.contains(&format!("class {entry} entry")), "missing WS entry node {entry}:\n{mmd}");
    }
    // onMessage calls Ws::sendText(...) (the EnumValue-shaped static-call
    // syntax) -- must resolve as a real edge to the stdlib boundary, not
    // silently vanish, and must NOT recurse into Ws's own internals.
    assert!(mmd.contains("EchoEndpoint_onMessage --> Ws_sendText"), "Ws::sendText call not resolved:\n{mmd}");
    assert!(mmd.contains("class Ws_sendText external"), "Ws.sendText should be external, not expanded:\n{mmd}");
    assert!(!mmd.contains("Ws_writeFrame"), "traversal expanded into Ws's own internals past the stdlib boundary:\n{mmd}");
}

#[test]
fn amqp10_consumer_entry_point_found() {
    let mmd = run_graph("examples/amqp10_consumer_annotated/Main.tnx");
    assert!(mmd.contains("class DemoConsumer_onMessage entry"), "AMQP 1.0 consumer entry point not found:\n{mmd}");
}
