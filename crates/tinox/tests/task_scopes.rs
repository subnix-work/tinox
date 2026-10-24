//! `scope { }` error reporting (issue #301, phase 3). The e2e harness merges
//! stderr into the compared output, so the part of the contract that lives
//! on stderr -- a failure that loses to another error is *reported*, never
//! silently dropped -- is checked here with stdout and stderr kept apart.

use std::process::Command;

const SOURCE: &str = r#"
class Main {
    async fnc boom(msg: String) -> Int64 { throw msg
    return 0 }

    fnc bodyAndChildFail() -> Nothing {
        scope {
            const b = spawn Main::boom("child second")
            throw "body first"
        }
    }

    fnc twoChildrenFail() -> Nothing {
        scope {
            const a = spawn Main::boom("first child")
            const b = spawn Main::boom("second child")
        }
    }

    fnc main() -> Int32 {
        try { Main::bodyAndChildFail() } catch (e: String) { println("caught: " + e) }
        try { Main::twoChildrenFail() } catch (e: String) { println("caught: " + e) }
        return 0
    }
}
"#;

#[test]
fn losing_errors_are_reported_on_stderr_not_dropped() {
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let dir = std::env::temp_dir().join(format!("tinox-task-scopes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Main.tnx"), SOURCE).unwrap();
    let exe = dir.join("main_bin");

    let build = Command::new(tinox)
        .args(["build", "Main.tnx", "-o"])
        .arg(&exe)
        .current_dir(&dir)
        .output()
        .expect("run tinox build");
    assert!(
        build.status.success(),
        "build failed:\n{}{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    // Repeat: this is thread timing, not a single deterministic path.
    for _ in 0..20 {
        let run = Command::new(&exe).output().expect("run compiled program");
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        // The body's own error wins over a child's; among several failing
        // children the first one (spawn order) is the scope's error.
        assert_eq!(stdout, "caught: body first\ncaught: first child\n", "stderr: {stderr}");
        assert!(stderr.contains("task error suppressed: child second"), "stderr: {stderr}");
        assert!(stderr.contains("task error suppressed: second child"), "stderr: {stderr}");
        assert!(run.status.success(), "stderr: {stderr}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
