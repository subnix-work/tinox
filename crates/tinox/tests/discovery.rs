//! CLI regression coverage for #293: discovery and collection must never
//! turn an omitted or broken suite into a successful CI result.
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Project(PathBuf);
impl Project {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("tinox-discovery-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("tinox.toml"),
            "[package]\ncompiler = \"2.6.0\"\nname = \"discovery\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        Self(root)
    }
    fn write(&self, path: &str, source: &str) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tinox"))
            .arg("test")
            .args(args)
            .current_dir(&self.0)
            .output()
            .unwrap()
    }
}
impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn summary(out: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}
fn nested(passes: bool) -> String {
    format!("namespace acme {{ class Suite {{ @Test(\"nested\") fn test() -> Bool {{ return {passes} }} }} }}")
}

#[test]
fn nested_discovery_agrees_with_explicit_file() {
    let p = Project::new("nested");
    for passes in [false, true] {
        p.write("tests/acme/Suite.tnx", &nested(passes));
        for args in [vec![], vec!["tests/acme/Suite.tnx"]] {
            let out = p.run(&args);
            assert_eq!(out.status.success(), passes, "{}", summary(&out));
            assert!(
                summary(&out).contains(if passes {
                    "1 passed, 0 failed"
                } else {
                    "0 passed, 1 failed"
                }),
                "{}",
                summary(&out)
            );
        }
    }
}

#[test]
fn collection_errors_fail_even_with_passing_tests() {
    let p = Project::new("collection");
    p.write("tests/Broken.tnx", "class Broken { @Test fn nope( }");
    for args in [vec![], vec!["tests/Broken.tnx"]] {
        let out = p.run(&args);
        assert!(!out.status.success(), "{}", summary(&out));
        assert!(summary(&out).contains("1 collection errors"));
    }
    p.write("tests/acme/Suite.tnx", &nested(true));
    let out = p.run(&[]);
    assert!(!out.status.success(), "{}", summary(&out));
    assert!(summary(&out).contains("1 passed, 0 failed"));
    assert!(summary(&out).contains("1 collection errors"));
}

#[test]
fn empty_missing_and_invalid_namespace_suites_fail() {
    let p = Project::new("empty");
    assert!(!p.run(&[]).status.success());
    p.write("src/Empty.tnx", "class Empty {}");
    let out = p.run(&[]);
    assert!(!out.status.success());
    assert!(summary(&out).contains("no @Test methods found"));
    assert!(!p.run(&["Missing.tnx"]).status.success());
    p.write("tests/wrong/Suite.tnx", &nested(true));
    assert!(!p.run(&[]).status.success());
}

#[test]
fn sorted_recursive_scan_skips_generated_and_symlinked_files() {
    let p = Project::new("sorted");
    p.write("src/acme/Suite.tnx", &nested(true));
    p.write(
        "tests/Flat.tnx",
        "class Flat { @Test fn flat() -> Bool { return true } }",
    );
    for dir in [".tinox", "target", "build", "dist", "node_modules"] {
        p.write(&format!("tests/{dir}/Broken.tnx"), "invalid source");
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(p.0.join("tests"), p.0.join("tests/cycle")).unwrap();
        std::os::unix::fs::symlink(p.0.join("tests/Flat.tnx"), p.0.join("tests/Alias.tnx"))
            .unwrap();
    }
    let out = p.run(&[]);
    let text = summary(&out);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("2 tests — 2 passed, 0 failed"), "{text}");
    assert!(text.find("src/acme/Suite.tnx").unwrap() < text.find("tests/Flat.tnx").unwrap());
}

#[test]
fn watch_recovers_from_empty_and_failed_suites_and_detects_deletion() {
    let p = Project::new("watch");
    let log = p.0.join("watch.log");
    let stdout = fs::File::create(&log).unwrap();
    let stderr = stdout.try_clone().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_tinox"))
        .args(["test", "--watch"])
        .current_dir(&p.0)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wait_for(&log, "no test files found");
        assert!(child.try_wait().unwrap().is_none());
        p.write("tests/acme/Suite.tnx", &nested(false));
        wait_for(&log, "0 passed, 1 failed");
        p.write("tests/acme/Suite.tnx", &nested(true));
        wait_for(&log, "1 passed, 0 failed");
        fs::remove_file(p.0.join("tests/acme/Suite.tnx")).unwrap();
        p.write("src/Empty.tnx", "class Empty {}");
        wait_for(&log, "0 tests — 0 passed, 0 failed");
        assert!(child.try_wait().unwrap().is_none());
    }));
    let _ = child.kill();
    let _ = child.wait();
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}
fn wait_for(path: &PathBuf, expected: &str) {
    let start = Instant::now();
    loop {
        let text = fs::read_to_string(path).unwrap();
        if text.contains(expected) {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "missing {expected}: {text}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
