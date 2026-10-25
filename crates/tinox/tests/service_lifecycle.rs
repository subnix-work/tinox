//! Lifecycle integration against independent clients and real process signals.
#[allow(dead_code)]
mod common;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn success(command: &mut Command) {
    let output = command.output().unwrap();
    assert!(output.status.success(), "{command:?}\n{output:?}");
}
fn copy_tree(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target.join(entry.file_name()));
        } else {
            fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
        }
    }
}
fn project(name: &str, modules: &[&str]) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory =
        std::env::temp_dir().join(format!("tinox-lifecycle-{name}-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let mut manifest =
        "[package]\ncompiler = \"2.7.0\"\nname=\"lifecycle\"\nversion=\"0.0.0\"\n[startup]\nbanner=false\n".to_string();
    for module in modules {
        manifest.push_str(&format!(
            "[[dependencies]]\ngroup=\"tinox.core\"\nartifactId=\"{module}\"\nversion=\"9.9.9\"\n"
        ));
        copy_tree(
            &root
                .join("crates/tinox-core-ext")
                .join(module)
                .join("tinox"),
            &directory.join(format!(
                ".tinox/repository/central/tinox.core/{module}/9.9.9/tinox"
            )),
        );
    }
    fs::write(directory.join("tinox.toml"), manifest).unwrap();
    directory
}
fn build(directory: &Path) {
    success(
        Command::new(env!("CARGO_BIN_EXE_tinox"))
            .current_dir(directory)
            .env("TINOX_HOME", directory)
            .env("TINOX_PORT", "0")
            .args(["build", "Main.tnx", "-o", "service"]),
    );
}
#[test]
fn signals_drain_http_and_websocket_work_before_disposal() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (name, modules) in [
        ("http", vec!["http_server", "json"]),
        ("websocket", vec!["websocket"]),
        ("http3", vec!["http3_server", "http_server", "json"]),
    ] {
        if name == "http3" {
            let libraries = Command::new("pkg-config")
                .args([
                    "--exists",
                    "libngtcp2",
                    "libngtcp2_crypto_ossl",
                    "libnghttp3",
                ])
                .status()
                .is_ok_and(|status| status.success());
            let curl = Command::new("curl")
                .arg("-V")
                .output()
                .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains("HTTP3"));
            if !libraries || !curl {
                eprintln!("SKIP lifecycle HTTP/3 (native libraries or HTTP/3 curl unavailable)");
                continue;
            }
        }
        let directory = project(name, &modules);
        if name == "http3" {
            fs::copy(
                root.join("tests/fixtures/tls/selfsigned_cert.pem"),
                directory.join("cert.pem"),
            )
            .unwrap();
            fs::copy(
                root.join("tests/fixtures/tls/selfsigned_key.pem"),
                directory.join("key.pem"),
            )
            .unwrap();
        }
        copy_tree(
            &root.join("tests/fixtures/service_lifecycle").join(name),
            &directory,
        );
        if name == "http3" {
            success(
                Command::new(env!("CARGO_BIN_EXE_tinox"))
                    .current_dir(&directory)
                    .env("TINOX_HOME", &directory)
                    .env("TINOX_HTTP3", "1")
                    .args(["build", "Main.tnx", "-o", "service"]),
            );
        } else {
            build(&directory);
        }
        let mut client = Command::new("python3")
            .arg(root.join("crates/tinox/tests/common/service_lifecycle.py"))
            .arg(name)
            .arg(directory.join("service"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        common::wait_timeout(&mut client, Duration::from_secs(100))
            .expect("lifecycle clients timed out");
        let output = client.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        fs::remove_dir_all(directory).unwrap();
    }
}
#[test]
fn common_deadlines_bound_unready_startup_and_blocked_cleanup() {
    for (name, source, expected_error) in [
        ("startup", "@Service(150, 1000) class Main { fnc main() -> Int32 { serviceAwaitShutdown()\nreturn 0 } }", "startup/readiness deadline exceeded"),
        ("shutdown", "@Service(1000, 150) class Main { fnc main() -> Int32 { return 0 } }", "shutdown deadline exceeded"),
    ] {
        let directory = project(name, &[]);
        fs::write(directory.join("Main.tnx"), format!("import State\n{source}")).unwrap();
        let hook = if name == "startup" { "@Readiness fn ready() -> Bool { return false }" } else { "@OnStop fn stop() -> Nothing { while (true) { sleep(10) } }" };
        fs::write(directory.join("State.tnx"), format!("@Startup class State {{ {hook} }}")).unwrap();
        build(&directory);
        for _ in 0..20 {
            let start = Instant::now();
            let mut child = Command::new(directory.join("service")).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
            common::wait_timeout(&mut child, Duration::from_secs(3)).expect("common lifecycle watchdog failed");
            let output = child.wait_with_output().unwrap();
            assert_eq!(output.status.code(), Some(1), "{output:?}");
            assert!(String::from_utf8_lossy(&output.stderr).contains(expected_error), "{output:?}");
            assert!(start.elapsed() < Duration::from_secs(2));
        }
        fs::remove_dir_all(directory).unwrap();
    }
}

#[test]
fn blocked_health_hook_cannot_delay_the_signal_deadline() {
    use std::io::{BufRead, BufReader};
    let directory = project("blocked-health", &[]);
    fs::write(directory.join("Main.tnx"), "import State\n@Service(5000, 150) class Main { fnc main() -> Int32 { serviceAwaitShutdown()\nreturn 0 } }").unwrap();
    fs::write(directory.join("State.tnx"), "@Startup class State { @Readiness fn ready() -> Bool { println(\"checking\")\nwhile (true) {} return false } }").unwrap();
    build(&directory);
    for _ in 0..20 {
        let mut child = Command::new(directory.join("service"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(line, "checking\n");
        let start = Instant::now();
        success(Command::new("kill").args(["-TERM", &child.id().to_string()]));
        common::wait_timeout(&mut child, Duration::from_secs(2))
            .expect("blocked health delayed SIGTERM");
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("shutdown deadline exceeded"),
            "{output:?}"
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn hooks_without_managed_entrypoint_fail_visibly() {
    let directory = project("unmanaged-hooks", &[]);
    fs::write(
        directory.join("Main.tnx"),
        "import State\nclass Main { fnc main() -> Int32 { return 0 } }",
    )
    .unwrap();
    fs::write(
        directory.join("State.tnx"),
        "@Startup class State { @OnStart fn start() -> Nothing {} }",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tinox"))
        .current_dir(&directory)
        .args(["check", "Main.tnx"])
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Lifecycle hooks require @Service"),
        "{output:?}"
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn database_initialization_failure_prevents_main_and_reports_service_failure() {
    let directory = project("database-failure", &[]);
    let manifest = format!(
        "{}\n[database]\ndriver=\"sqlite\"\nurl=\"{}/missing/database.sqlite\"\n",
        fs::read_to_string(directory.join("tinox.toml")).unwrap(),
        directory.display()
    );
    fs::write(directory.join("tinox.toml"), manifest).unwrap();
    fs::write(directory.join("Main.tnx"), "@Service(2000, 1000) class Main { fnc main() -> Int32 { println(\"Main must not run\")\nreturn 0 } }").unwrap();
    build(&directory);
    for _ in 0..20 {
        let output = Command::new(directory.join("service")).output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("tinox: service failed: SQLite initialization failed"),
            "{output:?}"
        );
    }
    fs::remove_dir_all(directory).unwrap();
}
