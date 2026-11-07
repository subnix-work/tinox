//! Issue #313: `[[overrides]]` local path overrides. Runs the real built
//! `tinox` binary. Two lanes:
//!  * workspace-source lane -- `workspace_crypto_*` consumes the extended-tier
//!    module straight from `crates/tinox-core-ext/crypto` (no registry, no
//!    install), so local edits to the module are what the test observes;
//!  * synthetic fixtures -- prove an unpublished module change is visible,
//!    that the installed copy is shadowed only when an override is present,
//!    and that every misconfiguration is a hard error.
//!
//! The registry/artifact lane (real published archives) stays with
//! `stdlib_smoke.rs` and friends, which keep resolving installed packages.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn tinox() -> &'static str {
    env!("CARGO_BIN_EXE_tinox")
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root")
}

fn fresh_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("tinox-local-override-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("mkdir");
    d
}

/// A tinox.core-style package `localdemo` whose `Greeter.greet()` returns `greeting`.
fn write_localdemo(dir: &Path, greeting: &str) {
    let src = dir.join("tinox/core/localdemo");
    std::fs::create_dir_all(&src).expect("mkdir pkg");
    std::fs::write(dir.join("tinox.toml"), "[package]\ncompiler = \"2.9.3\"\nname = \"localdemo\"\nversion = \"9.9.9\"\ndescription = \"\"\ngroup = \"tinox.core\"\n").expect("pkg toml");
    std::fs::write(
        src.join("Greeter.tnx"),
        format!("class Greeter {{\n    fn greet() -> String {{ return \"{greeting}\" }}\n}}\n"),
    )
    .expect("Greeter.tnx");
}

const MAIN: &str = "import tinox.core.localdemo\nclass Main {\n    fnc main() -> Int32 {\n        println(Greeter::greet())\n        return 0\n    }\n}\n";

fn project(name: &str, extra_toml: &str) -> PathBuf {
    let p = fresh_dir(name);
    std::fs::create_dir_all(p.join("src")).expect("mkdir src");
    std::fs::write(
        p.join("tinox.toml"),
        format!("[package]\ncompiler = \"2.9.3\"\nname = \"overridetest\"\nversion = \"0.1.0\"\ndescription = \"\"\nentry = \"src/Main.tnx\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"localdemo\"\nversion = \"1.0.0\"\n{extra_toml}"),
    )
    .expect("toml");
    std::fs::write(p.join("src/Main.tnx"), MAIN).expect("Main");
    p
}

fn override_toml(path: &str) -> String {
    format!("\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"localdemo\"\npath = \"{path}\"\n")
}

fn run(dir: &Path) -> Output {
    Command::new(tinox()).arg("run").current_dir(dir).output().expect("tinox run")
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

#[test]
fn override_makes_an_unpublished_module_change_observable() {
    let pkg = fresh_dir("pkg-observe");
    write_localdemo(&pkg, "v1");
    let proj = project("observe", &override_toml(pkg.to_str().unwrap()));

    let out = run(&proj);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("v1"));
    assert!(stderr(&out).contains("resolved from local override"), "provenance missing: {}", stderr(&out));

    // Edit the local module without publishing anything: the next run sees it.
    write_localdemo(&pkg, "v2");
    let out = run(&proj);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("v2"));

    std::fs::remove_dir_all(&pkg).ok();
    std::fs::remove_dir_all(&proj).ok();
}

#[test]
fn without_override_the_unpublished_module_is_not_found() {
    let proj = project("no-override", "");
    let out = run(&proj);
    assert!(!out.status.success(), "an undeclared-source module must not resolve");
    std::fs::remove_dir_all(&proj).ok();
}

#[test]
fn install_skips_overridden_coordinates_and_does_not_lock_them() {
    let pkg = fresh_dir("pkg-install");
    write_localdemo(&pkg, "v1");
    let proj = project("install", &override_toml(pkg.to_str().unwrap()));
    let out = Command::new(tinox()).arg("install").current_dir(&proj).output().expect("install");
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("local override"));
    let lock = std::fs::read_to_string(proj.join("tinox.lock")).unwrap_or_default();
    assert!(!lock.contains("localdemo"), "local source leaked into tinox.lock: {lock}");
    std::fs::remove_dir_all(&pkg).ok();
    std::fs::remove_dir_all(&proj).ok();
}

#[test]
fn misconfigured_overrides_are_hard_errors() {
    // missing path
    let proj = project("missing-path", &override_toml("/nonexistent/tinox-override-path"));
    let out = run(&proj);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("override for tinox.core:localdemo"), "{}", stderr(&out));

    // package name mismatch
    let pkg = fresh_dir("pkg-mismatch");
    write_localdemo(&pkg, "v1");
    std::fs::write(pkg.join("tinox.toml"), "[package]\ncompiler = \"2.9.3\"\nname = \"other\"\nversion = \"1.0.0\"\ndescription = \"\"\n").unwrap();
    let proj2 = project("name-mismatch", &override_toml(pkg.to_str().unwrap()));
    let out = run(&proj2);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("expected 'localdemo'"), "{}", stderr(&out));

    // duplicate coordinate
    write_localdemo(&pkg, "v1");
    let dup = format!("{}{}", override_toml(pkg.to_str().unwrap()), override_toml(pkg.to_str().unwrap()));
    let proj3 = project("duplicate", &dup);
    let out = run(&proj3);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("more than once"), "{}", stderr(&out));

    // override matching nothing in the graph
    let proj4 = project("unused", &format!("{}{}", override_toml(pkg.to_str().unwrap()), "\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"ghost\"\npath = \".\"\n"));
    let out = run(&proj4);
    assert!(!out.status.success());

    for d in [pkg, proj, proj2, proj3, proj4] {
        std::fs::remove_dir_all(d).ok();
    }
}

#[test]
fn package_refuses_to_archive_a_manifest_with_overrides() {
    let pkg = fresh_dir("pkg-publish");
    write_localdemo(&pkg, "v1");
    std::fs::create_dir_all(pkg.join("src/tinox/core/localdemo")).unwrap();
    std::fs::copy(pkg.join("tinox/core/localdemo/Greeter.tnx"), pkg.join("src/tinox/core/localdemo/Greeter.tnx")).unwrap();
    let toml = std::fs::read_to_string(pkg.join("tinox.toml")).unwrap();
    std::fs::write(pkg.join("tinox.toml"), format!("{toml}{}", override_toml("../somewhere"))).unwrap();
    let out = Command::new(tinox()).arg("package").current_dir(&pkg).output().expect("package");
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), stderr(&out));
    assert!(combined.contains("[[overrides]]"), "expected a refusal mentioning [[overrides]]: {combined}");
    assert!(!pkg.join("localdemo-9.9.9.tar.gz").exists());
    std::fs::remove_dir_all(&pkg).ok();
}

/// Workspace-source lane: the real in-tree extended-tier `crypto` module,
/// consumed with no install and no registry access.
#[test]
fn workspace_crypto_resolves_from_the_checkout() {
    let proj = fresh_dir("workspace-crypto");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    let crypto = repo_root().join("crates/tinox-core-ext/crypto");
    std::fs::write(
        proj.join("tinox.toml"),
        format!("[package]\ncompiler = \"2.9.3\"\nname = \"wscrypto\"\nversion = \"0.1.0\"\ndescription = \"\"\nentry = \"src/Main.tnx\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"crypto\"\nversion = \"0.0.0-workspace\"\n\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"crypto\"\npath = \"{}\"\n", crypto.display()),
    )
    .unwrap();
    std::fs::write(
        proj.join("src/Main.tnx"),
        "import tinox.core.crypto\nclass Main {\n    fnc main() -> Int32 {\n        let c = new Crypto()\n        println(c.md5(\"abc\"))\n        return 0\n    }\n}\n",
    )
    .unwrap();
    let out = run(&proj);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("900150983cd24fb0d6963f7d28e17f72"));
    assert!(stderr(&out).contains(crypto.to_str().unwrap()), "provenance should name the workspace path: {}", stderr(&out));
    std::fs::remove_dir_all(&proj).ok();
}

/// Issue #621: `Ws::readUpgrade` / `requestHeader` / `rejectUpgrade` /
/// `acceptUpgrade` let a server inspect Cookie/Origin before accepting the
/// upgrade. New API, so it can only be exercised from the workspace source
/// (the e2e harness resolves published packages).
#[test]
fn workspace_websocket_exposes_upgrade_request_headers() {
    let proj = fresh_dir("workspace-websocket-upgrade");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    let ws = repo_root().join("crates/tinox-core-ext/websocket");
    std::fs::write(
        proj.join("tinox.toml"),
        format!("[package]\ncompiler = \"2.9.3\"\nname = \"wsupgrade\"\nversion = \"0.1.0\"\ndescription = \"\"\nentry = \"src/Main.tnx\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"websocket\"\nversion = \"0.0.0-workspace\"\n\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"websocket\"\npath = \"{}\"\n", ws.display()),
    )
    .unwrap();
    std::fs::write(proj.join("src/Main.tnx"), include_str!("fixtures/ws_upgrade_inspect/Main.tnx")).unwrap();
    let out = run(&proj);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "cookie=sid=abc\norigin=https://evil.example\nmissing=[]\nok-rejected\nok-accepted\n"
    );
    std::fs::remove_dir_all(&proj).ok();
}

/// Issue #627: the `ui` shell loads no third-party fonts unless `TINOX_UI_FONTS=google`;
/// an unknown value is a hard error. Workspace source (new behaviour, not yet in a published package).
#[test]
fn workspace_ui_shell_uses_system_fonts_by_default() {
    let proj = fresh_dir("workspace-ui-fonts");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    let ext = repo_root().join("crates/tinox-core-ext");
    let mut toml = String::from("[package]\ncompiler = \"2.9.3\"\nname = \"uifonts\"\nversion = \"0.1.0\"\ndescription = \"\"\nentry = \"src/Main.tnx\"\n");
    for m in ["ui", "json", "http_server", "websocket", "http"] {
        toml.push_str(&format!("\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"{m}\"\nversion = \"0.0.0-workspace\"\n\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"{m}\"\npath = \"{}\"\n", ext.join(m).display()));
    }
    std::fs::write(proj.join("tinox.toml"), toml).unwrap();
    std::fs::write(proj.join("src/Main.tnx"), include_str!("fixtures/ui_shell_fonts/Main.tnx")).unwrap();
    let go = |value: Option<&str>| {
        let mut c = Command::new(tinox());
        c.arg("run").current_dir(&proj).env_remove("TINOX_UI_FONTS");
        if let Some(v) = value { c.env("TINOX_UI_FONTS", v); }
        c.output().unwrap()
    };
    let expect = |o: &Output, google: &str| {
        assert!(o.status.success(), "stderr: {}", stderr(o));
        assert_eq!(String::from_utf8_lossy(&o.stdout), format!("google={google}\nplaceholder=false\nport=true\n"));
    };
    expect(&go(None), "false");
    expect(&go(Some("system")), "false");
    expect(&go(Some("google")), "true");
    let bad = go(Some("comic-sans"));
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stdout).contains("TINOX_UI_FONTS must be"), "{:?}", bad);
    std::fs::remove_dir_all(&proj).ok();
}

#[test]
fn add_uses_local_overrides_without_installing_or_locking_the_coordinate() {
    let pkg = fresh_dir("pkg-add");
    write_localdemo(&pkg, "added locally");
    let proj = fresh_dir("add");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    std::fs::write(proj.join("src/Main.tnx"), MAIN).unwrap();
    std::fs::write(
        proj.join("tinox.toml"),
        format!("[package]\ncompiler = \"2.9.3\"\nname = \"overridetest\"\nversion = \"0.1.0\"\n{}", override_toml(pkg.to_str().unwrap())),
    ).unwrap();
    let output = Command::new(tinox()).args(["add", "tinox.core", "localdemo", "1.0.0", "http://127.0.0.1:1/unreachable"])
        .current_dir(&proj).env("TINOX_HOME", proj.join("empty-home"))
        .output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(String::from_utf8_lossy(&output.stdout).contains("local override"), "{:?}", output);
    if let Ok(lock) = std::fs::read_to_string(proj.join("tinox.lock")) {
        assert!(!lock.contains("localdemo"), "local coordinate was pinned: {lock}");
    }
    let output = run(&proj);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(String::from_utf8_lossy(&output.stdout).contains("added locally"));
    std::fs::remove_dir_all(&pkg).ok();
    std::fs::remove_dir_all(&proj).ok();
}

/// #605: `Json::deserialize` used to return a null object for JSON `null` / unparseable text, so the
/// first field access crashed the process even inside `try`. It now throws, which `catch` handles.
#[test]
fn workspace_json_deserialize_throws_on_null_and_invalid_text() {
    let proj = fresh_dir("workspace-json-deserialize");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    let json = repo_root().join("crates/tinox-core-ext/json");
    std::fs::write(
        proj.join("tinox.toml"),
        format!("[package]\ncompiler = \"2.9.3\"\nname = \"wsjson\"\nversion = \"0.1.0\"\ndescription = \"\"\nentry = \"src/Main.tnx\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"json\"\nversion = \"0.0.0-workspace\"\n\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"json\"\npath = \"{}\"\n", json.display()),
    )
    .unwrap();
    std::fs::write(
        proj.join("src/Item.tnx"),
        "import tinox.core.json\n\n@JsonSerializable\nclass Item {\n    var name: String\n}\n",
    )
    .unwrap();
    std::fs::write(
        proj.join("src/Main.tnx"),
        "import tinox.core.json\nimport Item\n\nclass Main {\n    fnc probe(body: String) -> Int64 {\n        var item: Item = Item { name: \"\" }\n        try {\n            item = Json::deserialize<Item>(body)\n        } catch (e: String) {\n            println(\"caught\")\n            return -1\n        }\n        return item.name.len()\n    }\n\n    fnc main() -> Int32 {\n        println(probe(\"{\\\"name\\\":\\\"abc\\\"}\"))\n        println(probe(\"null\"))\n        println(probe(\"abc\"))\n        println(probe(\"\"))\n        println(probe(\"{}\"))\n        return 0\n    }\n}\n",
    )
    .unwrap();
    let out = run(&proj);
    assert!(out.status.success(), "process must not crash; stderr: {}", stderr(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "3\ncaught\n-1\ncaught\n-1\ncaught\n-1\n0");
    std::fs::remove_dir_all(&proj).ok();
}
