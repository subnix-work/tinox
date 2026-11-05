//! Issue #547: @BeforeEach/@AfterEach fixtures and @Case parameterized tests, plus the
//! exact `--filter` selector (#582).
#![cfg(unix)]

use serde_json::Value;
use std::fs;
use std::process::Command;

fn project(name: &str, suite: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("tinox-fixtures-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("tinox.toml"),
        format!("[package]\nname = \"fx\"\nversion = \"0.1.0\"\ncompiler = \"{}\"\n", env!("CARGO_PKG_VERSION")),
    )
    .unwrap();
    fs::write(root.join("Suite.tnx"), suite).unwrap();
    root
}

fn run(root: &std::path::Path, args: &[&str]) -> (bool, String, Vec<Value>) {
    let out = Command::new(env!("CARGO_BIN_EXE_tinox")).arg("test").args(args).current_dir(root).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let events = stdout.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    (out.status.success(), stdout, events)
}

const SUITE: &str = r#"class Suite {
    @BeforeEach
    fn setUp() -> Nothing {
        println("SETUP")
    }

    @AfterEach
    fn tearDown() -> Nothing {
        println("TEARDOWN")
    }

    @Test("adds")
    @Case(1, 2, 3)
    @Case(10, 5, 15)
    @Case(0, 0, 0)
    fn adds(a: Int64, b: Int64, expected: Int64) -> Bool {
        println("ADD " + a.toString())
        return a + b == expected
    }

    @Test("greets")
    @Case("x", true)
    fn greets(name: String, loud: Bool) -> Bool {
        return loud && name == "x"
    }

    @Test("throws")
    fn boom() -> Bool {
        throw "boom"
        return true
    }
}
"#;

#[test]
fn fixtures_and_cases_run_with_teardown_on_failure() {
    let root = project("run", SUITE);
    let (_, text, _) = run(&root, &["Suite.tnx"]);
    assert_eq!(text.matches("SETUP").count(), 5, "{text}");
    assert_eq!(text.matches("TEARDOWN").count(), 5, "teardown must run after every test, incl. the throwing one:\n{text}");
    assert!(text.contains("PASS  adds [case 2]"), "{text}");
    assert!(text.contains("FAIL  throws"), "{text}");
    assert!(text.contains("5 tests — 4 passed, 1 failed"), "{text}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn discovery_lists_every_case_with_stable_identity_and_filter_selects_by_method() {
    let root = project("list", SUITE);
    let (_, _, events) = run(&root, &["Suite.tnx", "--list", "--format", "json"]);
    let found: Vec<(String, Option<u64>)> = events
        .iter()
        .filter(|e| e["reason"] == "test-discovered")
        .map(|e| (e["method"].as_str().unwrap().to_string(), e["case"].as_u64()))
        .collect();
    assert_eq!(found.len(), 5, "{found:?}");
    assert!(found.contains(&("adds".to_string(), Some(2))));
    assert!(found.contains(&("boom".to_string(), None)));
    let (_, text, _) = run(&root, &["Suite.tnx", "--filter", "Suite::adds"]);
    assert!(text.contains("3 tests — 3 passed, 0 failed"), "{text}");
    assert!(!text.contains("FAIL"), "{text}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn invalid_cases_and_hooks_are_compile_errors() {
    let root = project(
        "bad",
        "class Suite {\n    @Test(\"x\")\n    @Case(1)\n    fn t(a: Int64, b: Int64) -> Bool { return true }\n}\n",
    );
    let (ok, text, _) = run(&root, &["Suite.tnx"]);
    assert!(!ok);
    assert!(text.contains("collection") || text.contains("@Case has 1 argument"), "{text}");
    let _ = fs::remove_dir_all(root);
}
