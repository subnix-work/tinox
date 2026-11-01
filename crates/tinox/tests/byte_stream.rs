//! Bounded TCP/TLS streaming checked against independent Python clients.
#![cfg(target_os = "linux")]
#[allow(dead_code)]
mod common;
use std::{fs, process::Command, time::Duration};

#[test]
fn bounded_streams_cancel_timeout_and_preserve_binary_payloads() {
    let root = common::repo_root();
    let work = std::env::temp_dir().join(format!("tinox-byte-stream-{}", std::process::id()));
    fs::create_dir_all(&work).unwrap();
    let exe = work.join("probe");
    let mut build = Command::new(env!("CARGO_BIN_EXE_tinox"));
    build
        .current_dir(&root)
        .args(["build", "tests/fixtures/byte_stream/Main.tnx", "-o"])
        .arg(&exe);
    let (status, output) = common::run_captured(build, Duration::from_secs(90)).unwrap();
    assert!(status.is_some_and(|s| s.success()), "{output}");
    let mut check = Command::new("python3");
    check
        .arg(root.join("tests/fixtures/byte_stream/check.py"))
        .arg(&exe)
        .arg(&root);
    let (status, output) = common::run_captured(check, Duration::from_secs(150)).unwrap();
    let _ = fs::remove_dir_all(work);
    assert!(status.is_some_and(|s| s.success()), "{output}");
}
