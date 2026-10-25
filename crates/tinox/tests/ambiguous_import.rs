//! Regression coverage for #156: an `import` that resolves against TWO
//! installed dependencies shipping a module at the same relative path
//! used to silently pick the first (manifest-declaration-order) match
//! with no diagnostic at all. Now a hard compile error instead — this
//! exercises the actual built `tinox` binary against a real
//! `.tinox/deps/` layout, not just the resolver function in isolation.

use std::path::Path;
use std::process::Command;

fn tinox() -> &'static str {
    env!("CARGO_BIN_EXE_tinox")
}

fn write_dep_util(project: &Path, group: &str, artifact: &str, greeting: &str) {
    let dir = project.join(".tinox/deps").join(group).join(artifact).join("1.0.0");
    std::fs::create_dir_all(&dir).expect("mkdir dep dir");
    std::fs::write(
        dir.join("Util.tnx"),
        format!("class Util {{\n    fnc greet() -> String {{ return \"{greeting}\" }}\n}}\n"),
    )
    .expect("write Util.tnx");
}

fn setup_project(name: &str, deps_toml: &str) -> std::path::PathBuf {
    let workdir = std::env::temp_dir().join(format!("tinox-ambiguous-import-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(workdir.join("src")).expect("mkdir src");
    std::fs::write(
        workdir.join("tinox.toml"),
        format!("[package]\ncompiler = \"2.6.0\"\nname = \"ambigtest\"\nversion = \"0.1.0\"\ndescription = \"\"\nentry = \"src/Main.tnx\"\n{deps_toml}"),
    )
    .expect("write tinox.toml");
    std::fs::write(
        workdir.join("src/Main.tnx"),
        "import Util\nclass Main {\n    fnc main() -> Int32 {\n        println(Util::greet())\n        return 0\n    }\n}\n",
    )
    .expect("write Main.tnx");
    workdir
}

#[test]
fn two_dependencies_shipping_the_same_module_path_is_a_hard_error() {
    let workdir = setup_project(
        "ambiguous",
        "\n[[dependencies]]\ngroup = \"com.example\"\nartifactId = \"lib1\"\nversion = \"1.0.0\"\nurl = \"https://example.com/lib1.tar.gz\"\n\n[[dependencies]]\ngroup = \"com.example\"\nartifactId = \"lib2\"\nversion = \"1.0.0\"\nurl = \"https://example.com/lib2.tar.gz\"\n",
    );
    write_dep_util(&workdir, "com.example", "lib1", "from lib1");
    write_dep_util(&workdir, "com.example", "lib2", "from lib2");

    let output = Command::new(tinox()).arg("run").current_dir(&workdir).output().expect("run tinox run");
    assert!(!output.status.success(), "expected tinox run to fail on an ambiguous import");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Ambiguous import"), "unexpected stderr: {stderr}");
    assert!(stderr.contains("com.example:lib1:1.0.0"), "unexpected stderr: {stderr}");
    assert!(stderr.contains("com.example:lib2:1.0.0"), "unexpected stderr: {stderr}");

    std::fs::remove_dir_all(&workdir).ok();
}

#[test]
fn single_matching_dependency_still_resolves_normally() {
    let workdir = setup_project(
        "single-match",
        "\n[[dependencies]]\ngroup = \"com.example\"\nartifactId = \"lib1\"\nversion = \"1.0.0\"\nurl = \"https://example.com/lib1.tar.gz\"\n",
    );
    write_dep_util(&workdir, "com.example", "lib1", "from lib1");

    let output = Command::new(tinox()).arg("run").current_dir(&workdir).output().expect("run tinox run");
    assert!(
        output.status.success(),
        "expected tinox run to succeed with a single matching dependency:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("from lib1"));

    std::fs::remove_dir_all(&workdir).ok();
}
