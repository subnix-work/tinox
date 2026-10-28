//! Issue #496: machine-readable `--message-format=json` for build/check and
//! `--format json` / `--list` for test. Contract tests for schema version 1.
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

fn tinox() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tinox"))
}

fn project(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let d = std::env::temp_dir().join(format!("tinox_msgfmt_{}_{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("src")).unwrap();
    std::fs::create_dir_all(d.join("tests")).unwrap();
    std::fs::write(
        d.join("tinox.toml"),
        format!("[package]\ncompiler = \"{}\"\nname = \"msgfmt\"\nversion = \"0.1.0\"\n", env!("CARGO_PKG_VERSION")),
    )
    .unwrap();
    for (path, text) in files {
        std::fs::write(d.join(path), text).unwrap();
    }
    d
}

/// Every stdout line must be one JSON object with `reason` and `schema_version`.
fn events(stdout: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(|l| {
            let v: Value = serde_json::from_str(l).unwrap_or_else(|e| panic!("stdout line is not JSON ({e}): {l:?}"));
            assert_eq!(v["schema_version"], 1, "{v}");
            assert!(v["reason"].is_string(), "{v}");
            v
        })
        .collect()
}

const MAIN_OK: &str = "class Main\n{\n    fnc main() -> Int32\n    {\n        println(\"program output\")\n        return 0\n    }\n}\n";
const MAIN_BAD: &str = "class Main\n{\n    fnc main() -> Int32\n    {\n        return absent\n    }\n}\n";

fn run(d: &Path, args: &[&str]) -> std::process::Output {
    tinox().current_dir(d).args(args).output().unwrap()
}

#[test]
fn failed_check_streams_diagnostics_then_build_finished() {
    let d = project("bad", &[("src/Main.tnx", MAIN_BAD)]);
    let out = run(&d, &["check", "--message-format=json"]);
    assert_eq!(out.status.code(), Some(1));
    let ev = events(&out.stdout);
    let msgs: Vec<&Value> = ev.iter().filter(|e| e["reason"] == "compiler-message").collect();
    assert!(!msgs.is_empty());
    assert_eq!(msgs[0]["diagnostic"]["code"], "TINOX-TYPE");
    assert!(msgs[0]["diagnostic"]["range"].is_object());
    let last = ev.last().unwrap();
    assert_eq!(last["reason"], "build-finished");
    assert_eq!(last["success"], false);
    assert_eq!(last["exit_code"], 1);
    assert_eq!(last["command"], "check");
    assert!(last["artifact"].is_null());
}

#[test]
fn successful_build_reports_the_artifact_and_keeps_stdout_clean() {
    let d = project("ok", &[("src/Main.tnx", MAIN_OK)]);
    // two-token spelling too
    let out = run(&d, &["build", "--message-format", "json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let ev = events(&out.stdout);
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!(ev[0]["reason"], "build-finished");
    assert_eq!(ev[0]["success"], true);
    let artifact = ev[0]["artifact"].as_str().unwrap();
    assert!(Path::new(artifact).is_file(), "artifact {artifact} must exist");
    assert_eq!(ev[0]["command"], "build");
}

#[test]
fn unknown_message_format_is_rejected() {
    let d = project("fmt", &[("src/Main.tnx", MAIN_OK)]);
    let out = run(&d, &["check", "--message-format=yaml"]);
    assert_eq!(out.status.code(), Some(1));
}

const TESTS: &str = "class SampleTest\n{\n    @Test(\"adds\")\n    fn testAdd() -> Bool\n    {\n        println(\"from passing test\")\n        return 1 + 1 == 2\n    }\n\n    @Test(\"fails\")\n    fn testFail() -> Bool\n    {\n        println(\"from failing test\")\n        return false\n    }\n}\n";

#[test]
fn test_discovery_lists_tests_without_running_them() {
    // the test body would create this marker if it ran
    let marker = std::env::temp_dir().join(format!("tinox_msgfmt_marker_{}", std::process::id()));
    let _ = std::fs::remove_file(&marker);
    let body = TESTS.replace("println(\"from passing test\")", &format!("fileWriteAllText(\"{}\", \"ran\")", marker.display()));
    let d = project("list", &[("src/Main.tnx", MAIN_OK), ("tests/SampleTest.tnx", &body)]);
    let out = run(&d, &["test", "--list", "--format", "json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let ev = events(&out.stdout);
    let found: Vec<&Value> = ev.iter().filter(|e| e["reason"] == "test-discovered").collect();
    assert_eq!(found.len(), 2, "{ev:?}");
    assert_eq!(found[0]["description"], "adds");
    assert_eq!(found[0]["class"], "SampleTest");
    assert_eq!(found[0]["method"], "testAdd");
    assert_eq!(found[0]["line"], 4);
    assert!(found[0]["file"].as_str().unwrap().ends_with("SampleTest.tnx"));
    assert!(!marker.exists(), "discovery must not execute tests");
    let last = ev.last().unwrap();
    assert_eq!((last["reason"].as_str().unwrap(), last["total"].as_u64().unwrap(), last["listed_only"].as_bool().unwrap()), ("test-run-finished", 2, true));
    assert_eq!(last["success"], true);
}

#[test]
fn test_run_streams_started_passed_and_failed_with_output() {
    let d = project("run", &[("src/Main.tnx", MAIN_OK), ("tests/SampleTest.tnx", TESTS)]);
    let out = run(&d, &["test", "--format", "json"]);
    assert_eq!(out.status.code(), Some(1), "a failing test fails the run");
    let ev = events(&out.stdout); // also proves stdout carries nothing but events
    let reasons: Vec<&str> = ev.iter().map(|e| e["reason"].as_str().unwrap()).collect();
    assert_eq!(reasons, ["test-started", "test-passed", "test-started", "test-failed", "test-run-finished"], "{reasons:?}");
    assert_eq!(ev[1]["description"], "adds");
    assert!(ev[1]["duration_ms"].is_u64());
    assert!(ev[1]["output"].as_str().unwrap().contains("from passing test"));
    assert_eq!(ev[3]["description"], "fails");
    assert_eq!(ev[3]["exit_code"], 1);
    assert_eq!(ev[3]["phase"], "run");
    assert!(ev[3]["output"].as_str().unwrap().contains("from failing test"));
    assert_eq!(ev[4]["total"], 2);
    assert_eq!(ev[4]["passed"], 1);
    assert_eq!(ev[4]["failed"], 1);
    assert_eq!(ev[4]["success"], false);
}

#[test]
fn errors_in_a_test_file_are_collection_error_events_not_noise() {
    let broken = TESTS.replace("return 1 + 1 == 2", "return absent");
    let d = project("cfail", &[("src/Main.tnx", MAIN_OK), ("tests/SampleTest.tnx", &broken)]);
    let out = run(&d, &["test", "--message-format=json"]);
    assert_eq!(out.status.code(), Some(1));
    let ev = events(&out.stdout);
    let err = ev.iter().find(|e| e["reason"] == "test-collection-error").expect("collection error event");
    assert!(err["file"].as_str().unwrap().ends_with("SampleTest.tnx"));
    assert!(err["message"].as_str().unwrap().contains("undefined variable"));
    let last = ev.last().unwrap();
    assert_eq!((last["reason"].as_str().unwrap(), last["collection_errors"].as_u64().unwrap(), last["success"].as_bool().unwrap()), ("test-run-finished", 1, false));
}

#[test]
fn text_mode_is_unchanged() {
    let d = project("text", &[("src/Main.tnx", MAIN_OK), ("tests/SampleTest.tnx", TESTS)]);
    let out = run(&d, &["test"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("PASS  adds") && stdout.contains("FAIL  fails"), "{stdout}");
    let list = run(&d, &["test", "--list"]);
    assert!(String::from_utf8_lossy(&list.stdout).contains("2 tests found"));
}
