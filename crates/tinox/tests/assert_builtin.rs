//! #589: a failing `assert` aborts with a diagnostic instead of passing an integer to the
//! pointer-typed `tinox_panic` (which used to segfault).

use std::process::Command;

#[test]
fn failed_assert_reports_a_diagnostic_and_exits_1() {
    let dir = std::env::temp_dir().join(format!("tinox_assert_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Main.tnx"),
        "class Main {\n    fnc main() -> Int32 {\n        assert(true)\n        println(\"true survived\")\n        assert(1 == 2)\n        println(\"unreachable\")\n        return 0\n    }\n}\n",
    )
    .unwrap();
    let exe = dir.join("assert_exe");
    let build = Command::new(env!("CARGO_BIN_EXE_tinox"))
        .current_dir(&dir)
        .args(["build", "Main.tnx", "-o"])
        .arg(&exe)
        .output()
        .unwrap();
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));
    let run = Command::new(&exe).output().unwrap();
    assert_eq!(run.status.code(), Some(1), "expected a clean exit 1, not a signal");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(stdout.contains("true survived") && !stdout.contains("unreachable"), "{stdout}");
    assert!(stderr.contains("PANIC: assertion failed"), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}
