//! Adapter composition tests use the real verifier, linker and runtime. Protocol
//! stand-ins exercise lowering failure edges without depending on a live broker;
//! they do not claim wire-protocol conformance.
use std::{fs, path::PathBuf, process::Command};

struct Work(PathBuf);
impl Work {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("tinox-adapter-{name}-{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        fs::copy(fixture().join("tinox.toml"), path.join("tinox.toml")).unwrap();
        Self(path)
    }
    fn compiler(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tinox"));
        command
            .current_dir(&self.0)
            .env("TINOX_FRONTEND_CACHE_DIR", self.0.join("cache"));
        command
    }
}
impl Drop for Work {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/framework_adapter_amqp091")
}
#[test]
fn amqp091_failure_edges_compose_with_main_in_all_build_modes() {
    let work = Work::new("composition");
    for (mode, flags) in [
        ("debug", vec!["--debug"]),
        ("release", vec![]),
        ("checked", vec!["--checked"]),
    ] {
        let executable = work.0.join(mode);
        let build = work
            .compiler()
            .arg("build")
            .arg(fixture().join("Main.tnx"))
            .args(flags)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(build.status.success(), "{mode}: {build:?}");
        let ir = fs::read_to_string(executable.with_extension("ll")).unwrap();
        assert_eq!(ir.matches("define i64 @__tinox_run_amqp091_").count(), 4);
        assert_eq!(
            ir.matches("call i64* @AmqpConnection091_connect").count(),
            4
        );
        assert_eq!(ir.matches("call void @AmqpChannel091_ack").count(), 4);
        assert!(ir.contains("ret i64 4"), "consume failure remains explicit");
        for _ in 0..20 {
            let run = Command::new(&executable).output().unwrap();
            assert!(run.status.success(), "{mode}: {run:?}");
            let stdout = String::from_utf8(run.stdout).unwrap();
            let mut lines: Vec<_> = stdout.split_whitespace().collect();
            lines.sort_unstable();
            assert_eq!(
                lines,
                [
                    "connect-fail",
                    "consume-fail",
                    "main",
                    "open-fail",
                    "qos-fail"
                ]
            );
        }
    }
}
#[test]
fn invalid_handler_body_still_reports_a_frontend_error_without_backend_tools() {
    let work = Work::new("diagnostic");
    for entry in fs::read_dir(fixture()).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), work.0.join(entry.file_name())).unwrap();
    }
    let path = work.0.join("ConnectConsumer.tnx");
    let source = fs::read_to_string(&path).unwrap();
    fs::write(
        path,
        source.replace(
            "println(\"unexpected handler\")",
            "const invalid: String = 42",
        ),
    )
    .unwrap();
    for action in ["check", "build"] {
        let output = work
            .compiler()
            .args([action, "Main.tnx", "--json"])
            .env("PATH", "")
            .output()
            .unwrap();
        assert!(!output.status.success(), "{output:?}");
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(json["diagnostics"][0]["category"], "type", "{json}");
        assert!(json.to_string().contains("String"), "{json}");
    }
}
