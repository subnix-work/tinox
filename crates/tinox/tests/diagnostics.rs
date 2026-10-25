//! CLI protocol tests exercise real command boundaries, including imported files.
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Project(PathBuf);
impl Project {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "tinox-diagnostics-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let project = Self(root);
        project.write(
            "tinox.toml",
            "[package]\ncompiler = \"2.7.0\"\nname=\"diagnostics\"\nversion=\"0.1.0\"\nentry=\"Main.tnx\"\n",
        );
        project.write(
            "Main.tnx",
            "class Main { fnc main() -> Int32 { return 0 } }",
        );
        project
    }
    fn write(&self, file: &str, source: &str) {
        let path = self.0.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }
    fn command(&self, action: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tinox"));
        command
            .current_dir(&self.0)
            .arg(action)
            .env("TINOX_FRONTEND_CACHE_DIR", self.0.join("cache"))
            .env("TINOX_BUILD_CACHE", "0")
            .env("TINOX_RUNTIME_CACHE", "0");
        command
    }
    fn json(&self, action: &str, success: bool) -> Value {
        let output = self.command(action).arg("--json").output().unwrap();
        assert_eq!(output.status.success(), success, "{output:?}");
        let report = parse(&output);
        assert_eq!(report["success"], success);
        assert_eq!(report["schema_version"], 1);
        report
    }
}
impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn parse(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("Invalid protocol: {e}: {output:?}"))
}

#[test]
fn enum_string_concatenation_is_rejected_before_codegen() {
    let project = Project::new();
    project.write("Status.tnx", "enum Status { STARTING\nFAILED }");
    project.write("Run.tnx", "import Status\nclass Run { var status: Status }");
    for expression in ["\"status: \" + run.status", "run.status + \"!\""] {
        project.write("Main.tnx", &format!(
            "import Run\nclass Main {{ fnc main() -> Int32 {{ const run = Run {{ status: Status::FAILED }}\nprintln({expression})\nreturn 0 }} }}"
        ));
        for action in ["check", "build"] {
            let output = project.command(action).output().unwrap();
            assert_eq!(output.status.code(), Some(1), "{output:?}");
            assert!(String::from_utf8_lossy(&output.stderr).contains("cannot concatenate String and enum 'Status'"), "{output:?}");
        }
    }
    project.write("Main.tnx", "import Run\nclass Main { fnc main() -> Int32 { const run = Run { status: Status::FAILED }\nprintln(\"status: \" + run.status.name)\nreturn 0 } }");
    let output = project.command("run").output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "status: FAILED");
}

#[test]
fn check_and_build_share_cross_file_diagnostics_and_text_meaning() {
    let project = Project::new();
    project.write(
        "Main.tnx",
        "import Helper\nclass Main { fnc main() -> Int32 { return 0 } }",
    );
    for (source, category) in [
        ("class Helper { fnc value( }", "syntax"),
        ("class Helper { fnc value() -> Int64 { return `; } }", "lex"),
        (
            "class Helper { fnc value() -> Int64 { return \"wrong\" } }",
            "type",
        ),
        ("@DoesNotExist class Helper {}", "annotation"),
        (
            "class Helper { @Transactional fn value() -> Int64 { return 0 } }",
            "annotation",
        ),
        ("import Missing\nclass Helper {}", "import"),
    ] {
        project.write("Helper.tnx", source);
        let check = project.json("check", false);
        let build = project.json("build", false);
        assert_eq!(check, build);
        // Repeat against a warm frontend cache; the file and details must survive.
        assert_eq!(check, project.json("check", false));
        let diagnostic = &check["diagnostics"][0];
        assert_eq!(diagnostic["category"], category);
        assert_eq!(diagnostic["severity"], "error");
        assert_eq!(
            diagnostic["file"],
            project.0.join("Helper.tnx").to_str().unwrap()
        );
        assert_eq!(diagnostic["range"]["start"]["line"], 1);
        assert!(diagnostic["range"]["start"]["column"].as_u64().unwrap() > 0);
        assert!(diagnostic["code"].as_str().unwrap().starts_with("TINOX-"));
        let text = project.command("check").output().unwrap();
        assert_eq!(text.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&text.stderr);
        assert!(
            stderr.contains(diagnostic["message"].as_str().unwrap()),
            "{stderr}"
        );
        assert!(stderr.contains("Helper.tnx"));
        if category == "type" {
            assert_eq!(diagnostic["code"], "TINOX-TYPE-MISMATCH");
            assert_eq!(diagnostic["expected"], "Int64");
            assert_eq!(diagnostic["actual"], "String");
        }
    }
}

#[test]
fn flags_success_and_entry_errors_are_valid_json() {
    let project = Project::new();
    for flags in [
        vec!["--json"],
        vec!["--error-format=json"],
        vec!["--error-format", "json", "Main.tnx"],
    ] {
        let output = project.command("check").args(flags).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(parse(&output)["diagnostics"], serde_json::json!([]));
    }
    project.write("Main.tnx", "class Main { fnc main( }");
    assert_eq!(
        project.json("check", false)["diagnostics"][0]["category"],
        "syntax"
    );
    fs::remove_file(project.0.join("Main.tnx")).unwrap();
    for action in ["check", "build"] {
        assert_eq!(
            project.json(action, false)["diagnostics"][0]["category"],
            "driver"
        );
        let output = project
            .command(action)
            .args(["--json", "Absent.tnx"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert_eq!(parse(&output)["diagnostics"][0]["category"], "io");
    }
}

#[cfg(unix)]
#[test]
fn verifier_and_toolchain_failures_are_distinct_and_stdout_is_reserved() {
    use std::os::unix::fs::PermissionsExt;
    let project = Project::new();
    let tools = project.0.join("tools");
    fs::create_dir(&tools).unwrap();
    let script = |name: &str, body: &str| {
        let path = tools.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    };
    script("opt", "echo verifier-stdout; echo invalid-ir >&2; exit 1");
    let path = format!("{}:{}", tools.display(), std::env::var("PATH").unwrap());
    let output = project
        .command("build")
        .args(["--json", "--debug"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(parse(&output)["diagnostics"][0]["category"], "internal");
    script("opt", "exit 0");
    script("llc", "echo llc-stdout; echo failed-tool >&2; exit 1");
    let output = project
        .command("build")
        .args(["--json", "--debug"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report = parse(&output);
    assert_eq!(report["diagnostics"][0]["category"], "toolchain");
    assert!(report["diagnostics"][0]["message"]
        .as_str()
        .unwrap()
        .contains("failed-tool"));
}

#[test]
fn successful_build_protocol_and_executable_are_usable() {
    let project = Project::new();
    assert_eq!(
        project.json("build", true)["diagnostics"],
        serde_json::json!([])
    );
    assert!(Command::new(project.0.join("diagnostics"))
        .status()
        .unwrap()
        .success());
}

#[test]
fn nested_source_identity_and_distinct_ranges_survive_graph_merging() {
    let project = Project::new();
    project.write(
        "Main.tnx",
        "import model.Helper\nclass Main { fnc main() -> Int32 { return 0 } }",
    );
    project.write("model/Helper.tnx", "namespace model {\nclass Helper {\nfnc one() -> Int64 { return firstMissing }\nfnc two() -> Int64 { return secondMissing }\n}\n}\n");
    let report = project.json("check", false);
    let diagnostics = report["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 2);
    for (diagnostic, line) in diagnostics.iter().zip([3, 4]) {
        assert_eq!(
            diagnostic["file"],
            project.0.join("model/Helper.tnx").to_str().unwrap()
        );
        assert_eq!(diagnostic["range"]["start"]["line"], line);
        assert_eq!(diagnostic["range"]["start"]["column"], 29);
        assert!(
            diagnostic["range"]["end"]["offset"].as_u64().unwrap()
                > diagnostic["range"]["start"]["offset"].as_u64().unwrap()
        );
    }
    assert_eq!(report, project.json("build", false));
}
