//! Workspace binary bridges, independently checked by curl's HTTP/2+HTTP/3 stacks.
#![cfg(target_os = "linux")]
#[allow(dead_code)]
mod common;
use std::{process::Command, time::Duration};

#[test]
fn binary_http2_http3_and_messaging_copy_adapters() {
    let root = common::repo_root();
    let libraries = Command::new("pkg-config")
        .args([
            "--exists",
            "libngtcp2",
            "libngtcp2_crypto_ossl",
            "libnghttp3",
        ])
        .status()
        .is_ok_and(|s| s.success());
    let curl = Command::new("curl").arg("-V").output().unwrap();
    if !libraries || !String::from_utf8_lossy(&curl.stdout).contains("HTTP3") {
        eprintln!("SKIP protocol bytes: HTTP/3 curl and development libraries required");
        return;
    }
    let mut check = Command::new("python3");
    check
        .arg(root.join("tests/fixtures/protocol_bytes/check.py"))
        .arg(env!("CARGO_BIN_EXE_tinox"))
        .arg(&root);
    let (status, output) = common::run_captured(check, Duration::from_secs(180)).unwrap();
    assert!(status.is_some_and(|s| s.success()), "{output}");
}
