//! Issue #449: `[package] compiler` must equal the running compiler version.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn project(dir: &Path, package_extra: &str) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("tinox.toml"),
        format!("[package]\nname = \"cv\"\nversion = \"0.1.0\"\n{package_extra}"),
    )
    .unwrap();
    fs::write(
        dir.join("src/Main.tnx"),
        "class Main\n{\n    fnc main() -> Int32\n    {\n        println(\"ok\")\n        return 0\n    }\n}\n",
    )
    .unwrap();
}

fn tinox(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tinox")).args(args).current_dir(dir).output().unwrap()
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("tinox_cv_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn matching_version_builds() {
    let d = tmp("match");
    project(&d, &format!("compiler = \"{VERSION}\"\n"));
    let out = tinox(&d, &["check"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn mismatching_version_is_a_hard_error() {
    let d = tmp("mismatch");
    project(&d, "compiler = \"0.0.1\"\n");
    let out = tinox(&d, &["check"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("requires compiler version 0.0.1") && err.contains(VERSION), "{err}");
}

#[test]
fn missing_field_is_a_hard_error_with_hint() {
    let d = tmp("missing");
    project(&d, "");
    let out = tinox(&d, &["check"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("does not declare the compiler version") && err.contains(&format!("compiler = \"{VERSION}\"")), "{err}");
}

#[test]
fn tinox_new_writes_the_field() {
    let d = tmp("new");
    let out = tinox(&d, &["new", "fresh"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let toml = fs::read_to_string(d.join("fresh/tinox.toml")).unwrap();
    assert!(toml.contains(&format!("compiler = \"{VERSION}\"")), "{toml}");
    let out = tinox(&d.join("fresh"), &["check"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn dependency_with_other_compiler_version_is_rejected() {
    let d = tmp("dep");
    project(
        &d,
        &format!(
            "compiler = \"{VERSION}\"\n[[dependencies]]\ngroup = \"example\"\nartifactId = \"dep\"\nversion = \"1.0.0\"\nurl = \"https://example.invalid/dep.tar.gz\"\n"
        ),
    );
    let dep = d.join(".tinox/deps/example/dep/1.0.0");
    fs::create_dir_all(&dep).unwrap();
    fs::write(dep.join("tinox.toml"), "[package]\nname = \"dep\"\nversion = \"1.0.0\"\ncompiler = \"0.0.1\"\n").unwrap();
    fs::write(dep.join("Dep.tnx"), "class Dep {}\n").unwrap();
    let out = tinox(&d, &["check"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("requires compiler version 0.0.1"), "{err}");
}
