//! Core/extended stdlib tier split: `tinox.core.*` imports for a CORE_MODULES
//! entry always resolve with zero tinox.toml, while an extended-tier module
//! (e.g. json, not in CORE_MODULES) requires a real, installed dependency —
//! see `resolve_imports`'s branch-3 gating in `crates/tinox/src/main.rs`.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn tinox_bin() -> &'static str {
    env!("CARGO_BIN_EXE_tinox")
}

fn workdir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tinox-stdlib-tier-gating-{}-{}",
        std::process::id(),
        label
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir workdir");
    dir
}

#[test]
fn core_module_import_resolves_with_zero_tinox_toml() {
    let dir = workdir("core");
    fs::write(
        dir.join("Main.tnx"),
        "import tinox.core.string\nclass Main {\n    fnc main() -> Int32 {\n        println(Strings::toUpperCase(\"hi\"))\n        return 0\n    }\n}\n",
    )
    .unwrap();

    let out = Command::new(tinox_bin())
        .arg("build")
        .arg(dir.join("Main.tnx"))
        .arg("-o")
        .arg(dir.join("out"))
        .output()
        .expect("spawn build");
    assert!(
        out.status.success(),
        "core-module build should succeed with no tinox.toml at all:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let run = Command::new(dir.join("out")).output().expect("run");
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "HI");

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn extended_module_import_with_no_tinox_toml_fails_with_declare_it_error() {
    let dir = workdir("undeclared");
    fs::write(
        dir.join("Main.tnx"),
        "import tinox.core.json\nclass Main {\n    fnc main() -> Int32 {\n        let v: JsonValue = Json::parse(\"1\")\n        println(v.getInt())\n        return 0\n    }\n}\n",
    )
    .unwrap();

    let out = Command::new(tinox_bin())
        .arg("build")
        .arg(dir.join("Main.tnx"))
        .arg("-o")
        .arg(dir.join("out"))
        .output()
        .expect("spawn build");
    assert!(!out.status.success(), "should fail to compile: an extended module with no dependency declared");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("extended-tier stdlib module"), "{stderr}");
    assert!(stderr.contains("group = \"tinox.core\""), "{stderr}");
    assert!(stderr.contains("artifactId = \"json\""), "{stderr}");
    assert!(stderr.contains("tinox install"), "{stderr}");

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn extended_module_declared_but_not_installed_fails_with_run_install_error() {
    let dir = workdir("declared-not-installed");
    fs::write(
        dir.join("tinox.toml"),
        "[package]\nname = \"gatetest\"\nversion = \"0.1.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"json\"\nversion = \"1.1.0\"\n",
    )
    .unwrap();
    fs::write(
        dir.join("Main.tnx"),
        "import tinox.core.json\nclass Main {\n    fnc main() -> Int32 {\n        let v: JsonValue = Json::parse(\"1\")\n        println(v.getInt())\n        return 0\n    }\n}\n",
    )
    .unwrap();

    // TINOX_HOME points at an empty, never-installed-into global cache —
    // the dependency IS declared but has never been `tinox install`ed here.
    let tinox_home = workdir("declared-not-installed-home");
    let out = Command::new(tinox_bin())
        .arg("build")
        .arg(dir.join("Main.tnx"))
        .arg("-o")
        .arg(dir.join("out"))
        .env("TINOX_HOME", &tinox_home)
        .output()
        .expect("spawn build");
    assert!(!out.status.success(), "should fail: dependency declared but never installed");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("tinox.toml declares tinox.core:json:1.1.0"), "{stderr}");
    assert!(stderr.contains("isn't installed"), "{stderr}");
    assert!(stderr.contains("tinox install"), "{stderr}");
    // Must NOT be conflated with the "never declared" message from the
    // previous test — these are deliberately distinct diagnostics.
    assert!(!stderr.contains("not part of the always-available core"), "{stderr}");

    fs::remove_dir_all(&dir).ok();
    fs::remove_dir_all(&tinox_home).ok();
}
