//! Issue #514: compile/link once per test file while retaining process isolation.
#![cfg(unix)]

use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};

struct Project(PathBuf);
impl Project {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("tinox-suite-reuse-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("tools")).unwrap();
        fs::write(root.join("tinox.toml"), format!(
            "[package]\nname = \"suite\"\nversion = \"0.1.0\"\ncompiler = \"{}\"\n\n[native.test]\nsources = [\"state.c\"]\n", env!("CARGO_PKG_VERSION")
        )).unwrap();
        fs::write(root.join("state.c"), "#include <stdint.h>\n#include <unistd.h>\nstatic int64_t calls;\nint64_t next_call(void) { return ++calls; }\nint64_t process_id(void) { return getpid(); }\n").unwrap();
        for tool in ["llc", "cc"] {
            let resolved = Command::new("sh")
                .args(["-c", &format!("command -v {tool}")])
                .output()
                .unwrap();
            assert!(resolved.status.success());
            let real = String::from_utf8(resolved.stdout).unwrap();
            let script = format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> "$SUITE_SPY/{tool}.log"
'{}' "$@"
status=$?
if [ "$status" = 0 ] && [ '{tool}' = cc ]; then
    previous=''
    for arg do
        if [ "$previous" = -o ]; then
            case "$arg" in .tinox_test_*) cp "$arg" "$SUITE_SPY/saved-test" ;; esac
        fi
        previous="$arg"
    done
fi
exit "$status"
"#,
                real.trim().replace('\'', "'\\''")
            );
            let path = root.join("tools").join(tool);
            fs::write(&path, script).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self(root)
    }
    fn write_suite(&self, methods: &str) {
        fs::write(self.0.join("Suite.tnx"), format!("import tinox.core.collections\nimport tinox.core.uri\nextern fn next_call() -> Int64\nextern fn process_id() -> Int64\nclass Suite {{\n{methods}\n}}\n")).unwrap();
    }
    fn run(&self, extra: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tinox"))
            .args(["test", "Suite.tnx", "--format", "json"])
            .args(extra)
            .current_dir(&self.0)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.0.join("tools").display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("SUITE_SPY", &self.0)
            .output()
            .unwrap()
    }
    fn events(out: &Output) -> Vec<Value> {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn invocations(&self, tool: &str) -> usize {
        fs::read_to_string(self.0.join(format!("{tool}.log")))
            .unwrap_or_default()
            .lines()
            .filter(|line| line.contains(".tinox_test_"))
            .count()
    }
    fn assert_clean(&self) {
        assert!(!fs::read_dir(&self.0).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".tinox_test_")));
    }
}
impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn one_four_and_eight_methods_share_codegen_and_link_but_not_native_globals() {
    for count in [1, 4, 8] {
        let p = Project::new(&format!("isolation-{count}"));
        let methods: String = (0..count).map(|i| format!("@Test(\"method {i}\") fn test{i}() -> Bool {{\nprintln(process_id())\nreturn next_call() == 1\n}}\n")).collect();
        p.write_suite(&methods);
        let listed = p.run(&["--list"]);
        assert!(listed.status.success());
        assert_eq!(
            Project::events(&listed)
                .iter()
                .filter(|e| e["reason"] == "test-discovered")
                .count(),
            count
        );
        assert_eq!(p.invocations("llc"), 0);
        assert_eq!(p.invocations("cc"), 0);
        let out = p.run(&[]);
        assert!(
            out.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let events = Project::events(&out);
        let passes: Vec<_> = events
            .iter()
            .filter(|e| e["reason"] == "test-passed")
            .collect();
        assert_eq!(passes.len(), count);
        let pids: std::collections::HashSet<_> = passes
            .iter()
            .map(|e| e["output"].as_str().unwrap().trim())
            .collect();
        assert_eq!(
            pids.len(),
            count,
            "each method must execute in a fresh process"
        );
        assert_eq!(p.invocations("llc"), 1);
        assert_eq!(p.invocations("cc"), 1);
        for args in [
            vec![],
            vec!["--tinox-test-index", "-1"],
            vec!["--tinox-test-index", "999"],
            vec!["--tinox-test-index", "garbage"],
            vec!["--tinox-test-index", "99999999999999999999999999999"],
        ] {
            assert_eq!(
                Command::new(p.0.join("saved-test"))
                    .args(args)
                    .status()
                    .unwrap()
                    .code(),
                Some(2)
            );
        }
        p.assert_clean();
    }
}

#[test]
fn shared_compile_failure_reports_each_method_and_cleans_ir() {
    let p = Project::new("compile-error");
    p.write_suite("@Test(\"first\") fn first() -> Bool { return true }\n@Test(\"second\") fn second() -> Bool { return false }");
    // Fail after IR emission so this checks one toolchain attempt and cleanup too.
    fs::write(p.0.join("tools/llc"), "#!/bin/sh\nprintf 'attempt\\n' >> \"$SUITE_SPY/attempts\"\nprintf 'deliberate llc failure\\n' >&2\nexit 1\n").unwrap();
    let out = p.run(&[]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        fs::read_to_string(p.0.join("attempts")).unwrap(),
        "attempt\n"
    );
    let events = Project::events(&out);
    let reasons: Vec<_> = events
        .iter()
        .map(|e| e["reason"].as_str().unwrap())
        .collect();
    assert_eq!(
        reasons,
        [
            "test-started",
            "test-failed",
            "test-started",
            "test-failed",
            "test-run-finished"
        ]
    );
    for failed in [&events[1], &events[3]] {
        assert_eq!(failed["phase"], "compile");
        assert!(failed["message"]
            .as_str()
            .unwrap()
            .contains("deliberate llc failure"));
    }
    assert_eq!(events[4]["passed"], 0);
    assert_eq!(events[4]["failed"], 2);
    p.assert_clean();
}
