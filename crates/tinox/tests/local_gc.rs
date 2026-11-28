//! Exercise the normal build selector and the emitted executable (#712).
#![cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"), target_env = "gnu"))]

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

/// #723: small objects are allocated from thread-owned spans without the heap lock. Many threads
/// allocating, freeing and being stopped by collections must see exactly what Boehm sees.
#[test]
fn threaded_allocation_matches_boehm_across_collections() {
    let dir = std::env::temp_dir().join(format!("tinox-gc-threads-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmarks/gc_stress/Main.tnx");
    let mut exes = Vec::new();
    for collector in ["tinox", "boehm"] {
        let exe = dir.join(collector);
        let build = Command::new(env!("CARGO_BIN_EXE_tinox"))
            .env("TINOX_GC", collector)
            .env_remove("TINOX_CFLAGS")
            .args(["build"])
            .arg(&source)
            .args(["--release", "-o"])
            .arg(&exe)
            .output()
            .unwrap();
        assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));
        exes.push(exe);
    }
    let run = |exe: &Path, workload: &str, scale: &str, threads: &str| {
        let out = Command::new("timeout")
            .arg("60")
            .arg(exe)
            .env("WORKLOAD", workload)
            .env("SCALE", scale)
            .env("THREADS", threads)
            .output()
            .unwrap();
        assert!(out.status.success(), "{workload}/{threads}: {:?} {}", out.status, String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.split_whitespace().find(|f| f.starts_with("check=")).unwrap_or_default().to_string()
    };
    for (workload, scale, threads) in [("threads", "200000", "2"), ("threads", "200000", "8"), ("threads", "200000", "16"), ("mapchurn", "100000", "1")] {
        for _ in 0..5 {
            let tinox = run(&exes[0], workload, scale, threads);
            assert!(!tinox.is_empty());
            assert_eq!(tinox, run(&exes[1], workload, scale, threads), "{workload} with {threads} threads");
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// #727: a thread returning from a large allocation holds only the block's header pointer for a
/// moment after it released the heap lock. A collection started by another thread right then used to
/// free the fresh block (use after free, crashes under load). Many processes at once give the
/// scheduler enough chances to stop a thread in that window.
#[test]
fn a_fresh_large_block_survives_a_collection_that_starts_as_it_is_returned() {
    let dir = std::env::temp_dir().join(format!("tinox-gc-large-return-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("stress");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmarks/gc_stress/Main.tnx");
    let build = Command::new(env!("CARGO_BIN_EXE_tinox"))
        .env("TINOX_GC", "tinox")
        .env_remove("TINOX_CFLAGS")
        .args(["build"])
        .arg(&source)
        .args(["--release", "-o"])
        .arg(&exe)
        .output()
        .unwrap();
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));
    for round in 0..10 {
        let children: Vec<_> = (0..32)
            .map(|_| {
                Command::new("timeout")
                    .arg("120")
                    .arg(&exe)
                    .env("WORKLOAD", "threads")
                    .env("SCALE", "500000")
                    .env("THREADS", "8")
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect();
        for child in children {
            let out = child.wait_with_output().unwrap();
            assert!(out.status.success(), "round {round}: {:?} {}", out.status, String::from_utf8_lossy(&out.stderr));
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}
