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
        .env("TINOX_GC", "tinox")
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
