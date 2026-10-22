//! Consumer-local overrides must visibly exercise edits without touching installs.
use std::{fs, path::PathBuf, process::{Command, Output}, sync::atomic::{AtomicU64, Ordering}};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Project(PathBuf);
impl Project {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("tinox-overrides-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&root).unwrap();
        let p = Self(root);
        p.write("tinox.toml", "[package]\nname=\"consumer\"\nversion=\"1.0.0\"\ngroup=\"demo\"\nentry=\"src/Main.tnx\"\n[[dependencies]]\ngroup=\"demo\"\nartifactId=\"counter\"\nversion=\"1.0.0\"\nurl=\"https://invalid.example/counter.tar.gz\"\n");
        p.write("src/Main.tnx", "import demo.counter; class Main { fnc main() -> Int32 { println(Counter::value()); return 0; } }");
        p.package("local", "counter", 2);
        p.package(".tinox/deps/demo/counter/1.0.0", "counter", 1);
        p.enable();
        p
    }
    fn write(&self, path: &str, content: &str) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    fn package(&self, path: &str, name: &str, value: i32) {
        self.write(&format!("{path}/tinox.toml"), &format!("[package]\ngroup=\"demo\"\nname=\"{name}\"\nversion=\"1.0.0\"\n"));
        self.write(&format!("{path}/src/demo/{name}/Counter.tnx"), &format!("namespace demo.{name} {{ class Counter {{ fnc value() -> Int64 {{ return {value}; }} }} }}"));
        if path.starts_with(".tinox") {
            let src = self.0.join(format!("{path}/src/demo"));
            fs::rename(src, self.0.join(format!("{path}/demo"))).unwrap();
            fs::remove_dir(self.0.join(format!("{path}/src"))).unwrap();
        }
    }
    fn enable(&self) {
        self.write("tinox.overrides.yaml", "overrides:\n  'demo:counter:1.0.0': local\n");
    }
    fn cli(&self, args: &[&str], success: bool) -> Output {
        let out = Command::new(env!("CARGO_BIN_EXE_tinox")).args(args).current_dir(&self.0)
            .env("TINOX_HOME", &self.0).env("TINOX_FRONTEND_CACHE_DIR", self.0.join("frontend-cache"))
            .output().unwrap();
        assert_eq!(out.status.success(), success, "{args:?}: {out:?}");
        out
    }
    fn build_value(&self, expected: &str) {
        self.cli(&["build", "src/Main.tnx", "-o", "app"], true);
        let out = Command::new(self.0.join("app")).current_dir(&self.0).output().unwrap();
        assert!(out.status.success(), "{out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), expected);
    }
}
impl Drop for Project { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }

#[test]
fn unpublished_edits_rebuild_and_removing_override_restores_installed_source() {
    let p = Project::new();
    p.cli(&["install"], true);
    let lock = fs::read_to_string(p.0.join("tinox.lock")).unwrap();
    assert!(lock.contains("local_sources") && lock.contains("demo:counter:1.0.0"), "{lock}");
    assert!(!lock.contains(p.0.to_str().unwrap()), "paths must not leak into locks");
    let provenance = p.cli(&["sources"], true);
    assert!(String::from_utf8_lossy(&provenance.stdout).contains("local override: demo:counter:1.0.0"));
    p.build_value("2");
    p.package("local", "counter", 3);
    p.build_value("3");
    fs::remove_file(p.0.join("tinox.overrides.yaml")).unwrap();
    p.cli(&["install"], true);
    let lock = fs::read_to_string(p.0.join("tinox.lock")).unwrap();
    assert!(!lock.contains("local_sources"), "{lock}");
    p.build_value("1");
}

#[test]
fn transitive_overrides_and_cycles_use_the_consumer_configuration() {
    let p = Project::new();
    p.package("leaf", "leaf", 9);
    let mut local = fs::read_to_string(p.0.join("local/tinox.toml")).unwrap();
    local.push_str("[[dependencies]]\ngroup=\"demo\"\nartifactId=\"leaf\"\nversion=\"1.0.0\"\n");
    p.write("local/tinox.toml", &local);
    p.write("leaf/tinox.toml", "[package]\nname=\"leaf\"\nversion=\"1.0.0\"\n[[dependencies]]\ngroup=\"demo\"\nartifactId=\"counter\"\nversion=\"1.0.0\"\n");
    p.write("local/tinox.overrides.yaml", "overrides:\n  'demo:leaf:1.0.0': missing\n");
    p.write("tinox.overrides.yaml", "overrides:\n  'demo:counter:1.0.0': local\n  'demo:leaf:1.0.0': leaf\n");
    p.cli(&["install"], true);
    let out = p.cli(&["sources"], true);
    let out = String::from_utf8_lossy(&out.stdout);
    assert!(out.contains("local override: demo:leaf:1.0.0"), "{out}");
    let description = p.cli(&["describe", "demo.counter.Counter", "--json"], true);
    assert!(String::from_utf8_lossy(&description.stdout).contains("local"));
}

#[test]
fn invalid_sources_fail_without_falling_back_to_installed_copies() {
    let p = Project::new();
    for (config, error) in [
        ("overrides:\n  'demo:counter:1.0.0': missing\n", "missing"),
        ("overrides:\n  'demo:counter:2.0.0': local\n", "does not match"),
        ("overrides:\n  '../demo:counter:1.0.0': local\n", "invalid dependency"),
        ("overrides:\n  'demo:counter': local\n", "invalid override coordinate"),
        ("override: {}\n", "unknown field"),
        ("overrides:\n  'demo:counter:1.0.0': local\n  'demo:counter:1.0.0': missing\n", "duplicate override"),
    ] {
        p.write("tinox.overrides.yaml", config);
        let out = p.cli(&["check", "src/Main.tnx"], false);
        assert!(String::from_utf8_lossy(&out.stderr).contains(error), "{out:?}");
        p.cli(&["install"], false);
    }
    p.enable();
    p.write("local/tinox.toml", "[package]\nname=\"counter\"\nversion=\"1.0.0\"\ngroup=\"wrong\"\n");
    p.cli(&["sources"], false);
    p.package("local", "counter", 2);
    fs::rename(p.0.join("local/src/demo"), p.0.join("local/src/wrong")).unwrap();
    let out = p.cli(&["install"], false);
    assert!(String::from_utf8_lossy(&out.stderr).contains("namespace layout"));
}

#[test]
fn conflicting_requested_versions_are_rejected_and_publication_is_blocked() {
    let p = Project::new();
    for command in ["package", "publish"] {
        let out = p.cli(&[command], false);
        assert!(String::from_utf8_lossy(&out.stderr).contains("before packaging or publishing"));
        assert!(!p.0.join("consumer-1.0.0.tar.gz").exists());
    }
    let manifest = fs::read_to_string(p.0.join("tinox.toml")).unwrap();
    p.write("tinox.toml", &(manifest + "[[dependencies]]\ngroup=\"demo\"\nartifactId=\"counter\"\nversion=\"2.0.0\"\n"));
    let out = p.cli(&["install"], false);
    assert!(String::from_utf8_lossy(&out.stderr).contains("version conflict"));
    p.cli(&["check", "src/Main.tnx"], false);
}

#[test]
fn contract_generation_uses_local_sources_and_propagates_override_errors() {
    let p = Project::new();
    p.write("local/src/demo/counter/Counter.tnx",
        "namespace demo.counter { class Counter { fnc unpublished() -> Int64 { return 42; } } }");
    p.write("src/Main.tnx",
        "import demo.counter; class Main { fnc main() -> Int32 { println(Counter::unpublished()); return 0; } }");
    let out = p.cli(&["contract", "src/Main.tnx"], true);
    assert!(String::from_utf8_lossy(&out.stdout).contains("openapi:"));
    p.write("tinox.overrides.yaml", "overrides:\n  'demo:counter:1.0.0': missing\n");
    let out = p.cli(&["contract", "src/Main.tnx"], false);
    assert!(String::from_utf8_lossy(&out.stderr).contains("missing"));
    fs::remove_file(p.0.join("tinox.overrides.yaml")).unwrap();
    p.cli(&["contract", "src/Main.tnx"], false);
}
