//! Exercise the additive HTTP API from workspace sources, without installing
//! an older registry package that cannot contain the new methods.
// This suite uses only repo_root and run_captured from the shared e2e harness.
#[allow(dead_code)]
mod common;

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

struct Workdir(PathBuf);

impl Drop for Workdir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn http_absence_and_legacy_contracts() {
    let root = common::repo_root();
    let work = Workdir(std::env::temp_dir().join(format!(
        "tinox-api-contracts-{}",
        std::process::id()
    )));
    fs::create_dir_all(&work.0).expect("create workdir");
    fs::write(
        work.0.join("tinox.toml"),
        "[package]\ncompiler = \"2.8.0\"\nname = \"api_contracts\"\nversion = \"0.0.0\"\n",
    )
    .expect("write manifest");
    fs::copy(
        root.join("tests/fixtures/api_contracts/Main.tnx"),
        work.0.join("Main.tnx"),
    )
    .expect("copy fixture");

    // Mirror the real namespaces so project-local import resolution reads
    // these exact files, including their transitive imports, without network.
    for (module, files, tier) in [
        ("http_server", &["HttpRequest", "QueryString"][..], "tinox-core-ext/http_server"),
        ("json", &["Json", "JsonValue", "JsonField", "JsonSerializable"][..], "tinox-core-ext/json"),
        ("option", &["Option"][..], "tinox-core"),
        ("result", &["Result"][..], "tinox-core"),
    ] {
        let relative = PathBuf::from("tinox/core").join(module);
        fs::create_dir_all(work.0.join(&relative)).expect("create module directory");
        for file in files {
            let path = relative.join(format!("{file}.tnx"));
            fs::copy(root.join("crates").join(tier).join(&path), work.0.join(path))
                .expect("copy workspace source");
        }
    }

    let exe = work.0.join("contracts");
    let mut build = Command::new(env!("CARGO_BIN_EXE_tinox"));
    build.current_dir(&work.0).arg("build").arg("Main.tnx").arg("-o").arg(&exe);
    let (status, output) = common::run_captured(build, Duration::from_secs(60))
        .expect("run compiler");
    assert!(status.is_some_and(|s| s.success()), "build failed: {output}");
    let mut run = Command::new(&exe);
    run.current_dir(&work.0);
    let (status, output) = common::run_captured(run, Duration::from_secs(15))
        .expect("run contracts");
    assert!(status.is_some_and(|s| s.success()), "contracts failed: {output}");
    assert_eq!(output.trim(), "contracts ok");
}
