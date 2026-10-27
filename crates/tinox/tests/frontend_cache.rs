//! Exercise frontend queries through separate real CLI processes, comparing
//! incremental diagnostics and executable output with cache-disabled runs.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Project {
    root: PathBuf,
}
impl Project {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "tinox-frontend-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let p = Self { root };
        p.write(
            "tinox.toml",
            "[package]\ncompiler = \"2.8.0\"\nname=\"frontend\"\nversion=\"0.1.0\"\n",
        );
        p.write("Main.tnx", "import Left\nimport Right\nclass Main { fnc main() -> Int32 { println(Left::get().toString())\nreturn 0 } }");
        p.write(
            "Left.tnx",
            "import Leaf\nclass Left { fnc get() -> Int64 { return Leaf::get() } }",
        );
        p.write(
            "Right.tnx",
            "class Right { fnc get() -> Int64 { return 9 } }",
        );
        p.leaf(42);
        p
    }
    fn write(&self, name: &str, body: &str) {
        let path = self.root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
    fn leaf(&self, n: i32) {
        self.write(
            "Leaf.tnx",
            &format!("class Leaf {{ fnc get() -> Int64 {{ return {n} }} }}"),
        );
    }
    fn command(&self, args: &[&str], enabled: bool) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tinox"));
        cmd.args(args)
            .current_dir(&self.root)
            .env("TINOX_FRONTEND_CACHE_DIR", self.root.join("cache"))
            .env("TINOX_RUNTIME_CACHE_DIR", self.root.join("runtime"))
            .env("TINOX_FRONTEND_CACHE", if enabled { "1" } else { "0" })
            .env("TINOX_CACHE_TRACE", "1")
            .env_remove("TINOX_TIMINGS");
        cmd
    }
    fn check(&self, enabled: bool) -> Output {
        self.command(&["check", "Main.tnx"], enabled)
            .output()
            .unwrap()
    }
    fn equivalent(&self, success: bool) -> String {
        let incremental = self.check(true);
        let clean = self.check(false);
        assert_eq!(
            incremental.status.success(),
            success,
            "{}",
            stderr(&incremental)
        );
        assert_eq!(incremental.status.code(), clean.status.code());
        assert_eq!(incremental.stdout, clean.stdout);
        assert_eq!(diagnostics(&incremental), diagnostics(&clean));
        stderr(&incremental)
    }
}
impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
fn diagnostics(output: &Output) -> String {
    stderr(output)
        .lines()
        .filter(|line| !line.starts_with("[frontend-"))
        .collect::<Vec<_>>()
        .join("\n")
}
fn count(trace: &str, query: &str, outcome: &str) -> usize {
    trace
        .matches(&format!("[frontend-cache] {query} {outcome} "))
        .count()
}

#[test]
fn leaf_edit_reuses_unrelated_queries_and_invalidates_transitive_users() {
    let p = Project::new();
    p.equivalent(true);
    let warm = p.equivalent(true);
    assert_eq!(count(&warm, "parse", "miss"), 0);
    assert_eq!(count(&warm, "visibility", "hit"), 4);
    assert_eq!(count(&warm, "program", "hit"), 1);
    p.leaf(43);
    let edited = p.equivalent(true);
    assert_eq!(count(&edited, "parse", "miss"), 1);
    assert_eq!(count(&edited, "visibility", "hit"), 1);
    assert_eq!(count(&edited, "visibility", "miss"), 3);
    assert_eq!(count(&edited, "program", "miss"), 1);
}

#[test]
fn signature_annotations_imports_and_error_recovery_match_clean() {
    let p = Project::new();
    p.equivalent(true);
    p.write(
        "Leaf.tnx",
        "class Leaf { fnc get() -> String { return \"changed\" } }",
    );
    assert!(p.equivalent(false).contains("visibility miss"));
    p.leaf(42);
    p.equivalent(true);
    p.write(
        "Leaf.tnx",
        "@DoesNotExist class Leaf { fnc get() -> Int64 { return 42 } }",
    );
    assert!(p.equivalent(false).contains("unknown annotation"));
    p.leaf(42);
    p.write(
        "Left.tnx",
        "class Left { fnc get() -> Int64 { return Leaf::get() } }",
    );
    assert!(p.equivalent(false).contains("Import error:"));
    p.write(
        "Left.tnx",
        "import Leaf\nclass Left { fnc get() -> Int64 { return Leaf::get() } }",
    );
    p.equivalent(true);
}

#[test]
fn deletions_parse_failures_and_file_rules_are_checked_after_warming() {
    let p = Project::new();
    p.equivalent(true);
    fs::remove_file(p.root.join("Leaf.tnx")).unwrap();
    p.equivalent(false);
    for source in [
        "class Leaf {",
        "class Leaf { fnc get() -> Int64 { return `; } }",
        "class WrongName {}",
    ] {
        p.write("Leaf.tnx", source);
        p.equivalent(false);
        p.equivalent(false);
    }
    p.leaf(42);
    p.equivalent(true);
}

#[test]
fn cycles_directory_membership_and_namespace_rules_match_clean() {
    let p = Project::new();
    p.write(
        "Leaf.tnx",
        "import Left\nclass Leaf { fnc get() -> Int64 { return 42 } }",
    );
    p.equivalent(true);
    p.equivalent(true);
    p.write("Main.tnx", "import demo\nclass Main { fnc main() -> Int32 { println(A::get().toString())\nreturn 0 } }");
    p.write(
        "demo/A.tnx",
        "namespace demo { class A { fnc get() -> Int64 { return 42 } } }",
    );
    p.equivalent(true);
    p.write(
        "demo/B.tnx",
        "namespace demo { class B { fnc get() -> Int64 { return \"invalid\" } } }",
    );
    p.equivalent(false);
    fs::remove_file(p.root.join("demo/B.tnx")).unwrap();
    p.equivalent(true);
    p.write(
        "demo/A.tnx",
        "namespace wrong { class A { fnc get() -> Int64 { return 42 } } }",
    );
    p.equivalent(false);
}

#[test]
fn newly_ambiguous_installed_import_is_never_hidden_by_cached_queries() {
    let p = Project::new();
    p.write("Main.tnx", "import Util\nclass Main { fnc main() -> Int32 { println(Util::get().toString())\nreturn 0 } }");
    let dep = |name| {
        format!("\n[[dependencies]]\ngroup=\"example\"\nartifactId=\"{name}\"\nversion=\"1.0.0\"\nurl=\"https://example.com/{name}.tar.gz\"\n")
    };
    let manifest = format!(
        "[package]\ncompiler = \"2.8.0\"\nname=\"frontend\"\nversion=\"0.1.0\"\n{}",
        dep("one")
    );
    p.write("tinox.toml", &manifest);
    p.write(
        ".tinox/deps/example/one/1.0.0/Util.tnx",
        "class Util { fnc get() -> Int64 { return 42 } }",
    );
    p.equivalent(true);
    p.write("tinox.toml", &(manifest + &dep("two")));
    p.write(
        ".tinox/deps/example/two/1.0.0/Util.tnx",
        "class Util { fnc get() -> Int64 { return 43 } }",
    );
    assert!(p.equivalent(false).contains("Ambiguous import"));
}

fn entries(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .unwrap()
        .flat_map(|e| {
            let p = e.unwrap().path();
            if p.is_dir() {
                entries(&p)
            } else {
                vec![p]
            }
        })
        .collect()
}

#[test]
fn corrupt_misplaced_and_unwritable_cache_entries_fall_back() {
    let p = Project::new();
    p.equivalent(true);
    let files = entries(&p.root.join("cache"));
    assert!(!files.is_empty());
    // A valid record moved under another query key must not be accepted.
    let record = fs::read(&files[0]).unwrap();
    for path in &files {
        fs::write(path, &record).unwrap();
    }
    p.equivalent(true);
    for path in &files {
        fs::write(path, "truncated").unwrap();
    }
    let trace = p.equivalent(true);
    assert!(count(&trace, "program", "miss") > 0);
    for path in entries(&p.root.join("cache")) {
        // A valid envelope containing invalid serialized data must also miss.
        use sha2::{Digest, Sha256};
        let bad = b"invalid serialization";
        let mut hash = Sha256::new();
        hash.update(path.file_name().unwrap().to_string_lossy().as_bytes());
        hash.update(bad);
        let mut bytes = hash.finalize().to_vec();
        bytes.extend(bad);
        fs::write(path, bytes).unwrap();
    }
    p.equivalent(true);
    fs::remove_dir_all(p.root.join("cache")).unwrap();
    p.write("cache", "not a directory");
    p.equivalent(true);
}

#[test]
fn changed_node_numbering_and_inferred_types_produce_clean_executables() {
    let p = Project::new();
    for (body, expected) in [
        ("return 42", "42\n"),
        (
            "var text: String = \"changed\"\nprintln(text)\nreturn 43",
            "changed\n43\n",
        ),
    ] {
        p.write(
            "Leaf.tnx",
            &format!("class Leaf {{ fnc get() -> Int64 {{ {body} }} }}"),
        );
        for enabled in [true, true, false] {
            let built = p
                .command(
                    &["build", "Main.tnx", "--debug", "--force", "-o", "app"],
                    enabled,
                )
                .output()
                .unwrap();
            assert!(built.status.success(), "{}", stderr(&built));
            let run = Command::new(p.root.join("app")).output().unwrap();
            assert!(run.status.success());
            assert_eq!(String::from_utf8_lossy(&run.stdout), expected);
        }
    }
}

#[test]
fn concurrent_publication_repeats_without_partial_results() {
    let p = Project::new();
    for n in 0..20 {
        p.leaf(n);
        let mut a = p
            .command(&["check", "Main.tnx"], true)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut b = p
            .command(&["check", "Main.tnx"], true)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        assert!(a.wait().unwrap().success());
        assert!(b.wait().unwrap().success());
        p.equivalent(true);
    }
}
