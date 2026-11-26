//! Exercise the normal build selector and the emitted executable (#712).
#![cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]

use std::{path::Path, process::Command};

#[test]
fn local_collector_preserves_roots_and_reclaims_cycles_without_boehm() {
    let temp = std::env::temp_dir().join(format!("tinox-local-gc-{}", std::process::id()));
    std::fs::create_dir_all(&temp).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/support/local_gc");
    let exe = temp.join("app");
    let build = Command::new(env!("CARGO_BIN_EXE_tinox"))
        .current_dir(fixture)
        .env_remove("TINOX_GC")
        .env_remove("TINOX_CFLAGS")
        .args(["build", "Main.tnx", "--release", "-o"])
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let linked = Command::new("ldd").arg(&exe).output().unwrap();
    assert!(linked.status.success());
    assert!(!String::from_utf8_lossy(&linked.stdout).contains("libgc.so"));
    for _ in 0..20 {
        let run = Command::new("timeout")
            .arg("20")
            .arg(&exe)
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{:?}: {}",
            run.status,
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&run.stdout).trim(),
            "roots and cycles OK"
        );
    }
    std::fs::remove_dir_all(temp).unwrap();
}

#[test]
fn rebuilding_the_same_output_switches_between_default_and_boehm() {
    let dir = std::env::temp_dir().join(format!("tinox-gc-switch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Main.tnx"), "class Main { fnc main() -> Int32 { println(42)\nreturn 0 } }").unwrap();
    for collector in [None, Some("boehm"), None] {
        let mut build = Command::new(env!("CARGO_BIN_EXE_tinox"));
        build.current_dir(&dir).env_remove("TINOX_GC").env_remove("TINOX_CFLAGS");
        if let Some(value) = collector { build.env("TINOX_GC", value); }
        let result = build.args(["build", "Main.tnx", "-o", "app"]).output().unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        let app = dir.join("app");
        let libs = Command::new("ldd").arg(&app).output().unwrap();
        assert!(libs.status.success());
        assert_eq!(String::from_utf8_lossy(&libs.stdout).contains("libgc.so"), collector.is_some());
        let run = Command::new(&app).output().unwrap();
        assert!(run.status.success());
        assert_eq!(run.stdout, b"42\n");
    }
    std::fs::remove_dir_all(dir).unwrap();
}
