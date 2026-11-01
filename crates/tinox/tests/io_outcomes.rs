//! Recoverable operational errors retain context; independent OS checks verify effects.
#![cfg(target_os = "linux")]
#[allow(dead_code)]
mod common;

use std::{fs, path::PathBuf, process::Command, time::Duration};

struct Tree(PathBuf);
impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn checked_io_preserves_data_and_releases_failed_operations() {
    let tree = Tree(std::env::temp_dir().join(format!("tinox-io-{}", std::process::id())));
    fs::create_dir_all(&tree.0).unwrap();
    fs::write(tree.0.join("empty"), b"").unwrap();
    fs::write(tree.0.join("partial"), b"abcdef").unwrap();
    fs::write(tree.0.join("close-failure"), b"abcdef").unwrap();
    let shim = tree.0.join("faults.so");
    let compiled = Command::new("cc")
        .args(["-shared", "-fPIC"])
        .arg(common::repo_root().join("tests/fixtures/io_outcomes/faults.c"))
        .args(["-ldl", "-o"])
        .arg(&shim)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let binary: Vec<u8> = (0..200_000).map(|i| (i % 256) as u8).collect();
    fs::write(tree.0.join("binary"), &binary).unwrap();
    fs::hard_link(tree.0.join("binary"), tree.0.join("alias")).unwrap();
    let mut build = Command::new(env!("CARGO_BIN_EXE_tinox"));
    build
        .current_dir(common::repo_root())
        .args(["build", "tests/fixtures/io_outcomes/Main.tnx", "-o"])
        .arg(tree.0.join("probe"));
    let (status, output) = common::run_captured(build, Duration::from_secs(90)).unwrap();
    assert!(status.is_some_and(|s| s.success()), "{output}");
    let mut run = Command::new(tree.0.join("probe"));
    run.env("TINOX_IO_ROOT", &tree.0).env("LD_PRELOAD", &shim);
    let (status, output) = common::run_captured(run, Duration::from_secs(30)).unwrap();
    assert!(status.is_some_and(|s| s.success()), "{output}");
    assert_eq!(output.trim(), "io outcomes ok");
    assert_eq!(fs::read(tree.0.join("binary")).unwrap(), binary);
    assert_eq!(fs::read(tree.0.join("copied")).unwrap(), binary);
    assert!(!tree.0.join("text").exists());
    assert!(!tree.0.join("moved").exists());
}
