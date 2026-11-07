//! Issue #308: @Config values are typed, strict and overridable per deployment.
#![cfg(unix)]

use std::fs;
use std::process::Command;

fn build(name: &str, properties: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("tinox-config-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("tinox.toml"),
        format!("[package]\ncompiler = \"2.9.3\"\nname = \"cfg\"\nversion = \"0.1.0\"\ncompiler = \"{}\"\n", env!("CARGO_PKG_VERSION")),
    )
    .unwrap();
    fs::write(
        root.join("Settings.tnx"),
        "class Settings {\n    @Config(\"app.port\") var port: Int64\n    @Config(\"app.name\") var name: String = \"fallback\"\n    @Config(\"app.debug\") var debug: Bool = false\n}\n",
    )
    .unwrap();
    fs::write(
        root.join("Main.tnx"),
        "import Settings\nclass Main {\n    fnc main() -> Int32 {\n        const s: Settings = Settings { port: 0, name: \"fallback\", debug: false }\n        println(s.port.toString() + \" \" + s.name + \" \" + s.debug.toString())\n        return 0\n    }\n}\n",
    )
    .unwrap();
    fs::write(root.join("application.properties"), properties).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_tinox")).args(["build", "Main.tnx", "-o", "cfg"]).current_dir(&root).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    (root.clone(), root.join("cfg"))
}

fn run(root: &std::path::Path, exe: &std::path::Path, envs: &[(&str, &str)]) -> (bool, String, String) {
    let out = Command::new(exe).current_dir(root).envs(envs.iter().copied()).output().unwrap();
    (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string(), String::from_utf8_lossy(&out.stderr).to_string())
}

#[test]
fn defaults_files_and_environment_resolve_in_a_fixed_order() {
    let (root, exe) = build("order", "app.port=8080\napp.name=fromfile\n");
    assert_eq!(run(&root, &exe, &[]).1, "8080 fromfile false");
    // The environment overrides the file; absent keys keep their declared default.
    assert_eq!(run(&root, &exe, &[("APP_PORT", "9090"), ("APP_DEBUG", "yes")]).1, "9090 fromfile true");
    // The same binary with a different deployment configuration.
    let (root2, exe2) = build("order2", "app.port=1\n");
    assert_eq!(run(&root2, &exe2, &[]).1, "1 fallback false");
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(root2);
}

#[test]
fn missing_required_and_malformed_values_fail_startup_without_echoing_the_value() {
    let (root, exe) = build("strict", "app.name=x\n");
    let (ok, _, err) = run(&root, &exe, &[]);
    assert!(!ok);
    assert!(err.contains("'app.port' is required"), "{err}");
    for bad in ["12garbage", "", "99999999999999999999", "abc"] {
        let (ok, out, err) = run(&root, &exe, &[("APP_PORT", bad)]);
        assert!(!ok, "APP_PORT={bad:?} must fail, printed {out:?}");
        assert!(err.contains("not a valid integer"), "{err}");
        assert!(bad.is_empty() || !err.contains(bad), "value leaked into error: {err}");
    }
    let (ok, _, err) = run(&root, &exe, &[("APP_PORT", "1"), ("APP_DEBUG", "maybe")]);
    assert!(!ok);
    assert!(err.contains("not a valid boolean"), "{err}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn all_problems_are_reported_together_before_anything_runs() {
    // Main never constructs Settings, yet the bad configuration still stops startup, and every
    // problem is listed (not just the first).
    let root = std::env::temp_dir().join(format!("tinox-config-agg-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("tinox.toml"), format!("[package]\ncompiler = \"2.9.3\"\nname = \"cfg\"\nversion = \"0.1.0\"\ncompiler = \"{}\"\n", env!("CARGO_PKG_VERSION"))).unwrap();
    fs::write(
        root.join("Settings.tnx"),
        "class Settings {\n    @Config(\"app.port\") var port: Int64\n    @Config(\"app.limit\") var limit: Int64 = 5\n    @Config(\"app.debug\") var debug: Bool = false\n    @Config(\"app.secret\") var secret: String\n}\n",
    )
    .unwrap();
    fs::write(root.join("Main.tnx"), "import Settings\nclass Main {\n    fnc main() -> Int32 {\n        println(\"started\")\n        return 0\n    }\n}\n").unwrap();
    fs::write(root.join("application.properties"), "app.limit=oops\napp.debug=maybe\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_tinox")).args(["build", "Main.tnx", "-o", "cfg"]).current_dir(&root).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let (ok, stdout, err) = run(&root, &root.join("cfg"), &[]);
    assert!(!ok && !stdout.contains("started"), "{stdout} {err}");
    for needle in ["'app.port' is required", "'app.secret' is required", "'app.limit' is not a valid integer", "'app.debug' is not a valid boolean", "4 configuration problem(s)"] {
        assert!(err.contains(needle), "missing `{needle}` in:\n{err}");
    }
    assert!(!err.contains("oops") && !err.contains("maybe"), "values must not be echoed: {err}");
    // Fixing the configuration lets it start.
    let (ok, stdout, err) = run(&root, &root.join("cfg"), &[("APP_PORT", "1"), ("APP_SECRET", "s"), ("APP_LIMIT", "3"), ("APP_DEBUG", "true")]);
    assert!(ok && stdout == "started", "{stdout} {err}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn properties_values_expand_environment_placeholders() {
    // ${VAR} / ${VAR:default}: the variable name is free (unlike the key-derived override).
    let props = "app.port=${TEST_PORT_VAR:8080}\napp.name=host-${TEST_HOST_VAR:localhost}:${TEST_NAME_PORT:5432}/db\n";
    let (root, exe) = build("expand", props);
    assert_eq!(run(&root, &exe, &[]).1, "8080 host-localhost:5432/db false");
    assert_eq!(run(&root, &exe, &[("TEST_PORT_VAR", "9090"), ("TEST_HOST_VAR", "db1")]).1, "9090 host-db1:5432/db false");
    // A set-but-empty variable counts as set and wins over the default.
    assert_eq!(run(&root, &exe, &[("TEST_HOST_VAR", "")]).1, "8080 host-:5432/db false");
    // The key-derived override still beats the (expanded) file value.
    assert_eq!(run(&root, &exe, &[("TEST_PORT_VAR", "9090"), ("APP_PORT", "7")]).1, "7 host-localhost:5432/db false");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn unset_placeholder_without_default_and_malformed_placeholders_fail_startup() {
    let (root, exe) = build("expand-missing", "app.port=${TEST_REQUIRED_VAR}\napp.name=${BROKEN\napp.debug=${:x}\n");
    let (ok, _, err) = run(&root, &exe, &[]);
    assert!(!ok);
    assert!(err.contains("key 'app.port' refers to environment variable 'TEST_REQUIRED_VAR', which is not set and has no default"), "{err}");
    assert!(err.contains("key 'app.name' has an unterminated"), "{err}");
    assert!(err.contains("key 'app.debug' has a ${...} placeholder with an empty"), "{err}");
    assert!(err.contains("3 configuration problem(s)"), "{err}");
    // Once the variable is set the unrelated malformed entries are the only blockers.
    let (root2, exe2) = build("expand-secret", "app.port=${TEST_REQUIRED_VAR}\napp.name=${TEST_NAME_VAR}\n");
    let (ok, _, err) = run(&root2, &exe2, &[("TEST_REQUIRED_VAR", "1")]);
    assert!(!ok && err.contains("'TEST_NAME_VAR'"), "{err}");
    // Values (possibly secrets) never appear in the error text.
    let (ok, _, err) = run(&root2, &exe2, &[("TEST_REQUIRED_VAR", "topsecret-not-int")]);
    assert!(!ok && !err.contains("topsecret-not-int"), "{err}");
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(root2);
}

#[test]
fn escaped_placeholder_stays_literal_and_expansion_is_not_rescanned() {
    let (root, exe) = build("expand-escape", "app.port=1\napp.name=a\\${NOT_EXPANDED}b-${TEST_RESCAN_VAR}\n");
    // The variable's own value contains `${...}` and must not be expanded a second time.
    assert_eq!(run(&root, &exe, &[("TEST_RESCAN_VAR", "${X}")]).1, "1 a${NOT_EXPANDED}b-${X} false");
    let _ = fs::remove_dir_all(root);
}
