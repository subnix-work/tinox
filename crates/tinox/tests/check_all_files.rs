//! Issue #475: `tinox check` without a file argument checks every source file
//! of the project, not only the code reachable from the entry point.

use std::fs;
use std::process::Command;

fn project(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("tinox-check-all-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("tinox.toml"),
        format!("[package]\ncompiler = \"{}\"\nname = \"checkall\"\nversion = \"0.1.0\"\n", env!("CARGO_PKG_VERSION")),
    )
    .unwrap();
    fs::write(
        dir.join("src/Main.tnx"),
        "class Main\n{\n    fnc main() -> Int32\n    {\n        return 0\n    }\n}\n",
    )
    .unwrap();
    dir
}

#[test]
fn check_reports_errors_in_files_not_reachable_from_main() {
    let dir = project("broken");
    fs::write(dir.join("src/Unused.tnx"), "class Unused\n{\n    fn f() -> Int64\n    {\n        return \"x\"\n    }\n}\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_tinox")).arg("check").current_dir(&dir).output().unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("Unused.tnx"), "{text}");
}

#[test]
fn check_success_message_states_what_was_checked() {
    let dir = project("ok");
    fs::write(dir.join("src/Other.tnx"), "class Other\n{\n    fn f() -> Int64\n    {\n        return 1\n    }\n}\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_tinox")).arg("check").current_dir(&dir).output().unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "{text}");
    assert!(text.contains("checked 2 source files"), "{text}");
}
