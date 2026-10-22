//! Issue #310: explicit Linux target selection (`TINOX_TARGET`).
use std::process::Command;

fn tinox() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tinox"))
}

fn hello(dir: &std::path::Path) -> std::path::PathBuf {
    let src = dir.join("Main.tnx");
    std::fs::write(&src, "class Main { fnc main() -> Int32 { println(\"hi\"); return 0; } }\n").unwrap();
    src
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("tinox_target_{}_{}", name, std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn unknown_target_fails_before_producing_artifact() {
    let d = tmp("unknown");
    let src = hello(&d);
    let out = d.join("out");
    let r = tinox().args(["build", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("TINOX_TARGET", "riscv64").output().unwrap();
    assert!(!r.status.success());
    assert!(String::from_utf8_lossy(&r.stderr).contains("unsupported TINOX_TARGET"));
    assert!(!out.exists());
}

#[test]
fn non_native_target_is_rejected() {
    let foreign = if std::env::consts::ARCH == "aarch64" { "x86_64" } else { "aarch64" };
    let d = tmp("foreign");
    let src = hello(&d);
    let out = d.join("out");
    let r = tinox().args(["build", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("TINOX_TARGET", foreign).output().unwrap();
    assert!(!r.status.success());
    assert!(String::from_utf8_lossy(&r.stderr).contains("cross-compilation is not supported"));
    assert!(!out.exists());
}

#[test]
fn native_target_builds_and_runs() {
    let host = if std::env::consts::ARCH == "aarch64" { "arm64" } else { "amd64" };
    let d = tmp("native");
    let src = hello(&d);
    let out = d.join("out");
    let r = tinox().args(["build", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("TINOX_TARGET", host).output().unwrap();
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let run = Command::new(&out).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "hi");
}
