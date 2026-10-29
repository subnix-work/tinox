//! One managed service combines protocols, shared state, tasks and real transactions.
#[allow(dead_code)]
mod common;
mod pg_common;
use pg_common::{repo_root, tool_available, PgContainer};
use std::{
    fs,
    os::unix::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

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
fn success(command: &mut Command) {
    let output = command.output().unwrap();
    assert!(output.status.success(), "{command:?}\n{output:?}");
}
#[test]
fn independent_clients_verify_composed_service_recovery_and_draining() {
    if !tool_available("docker", &["--version"]) || !tool_available("psql", &["--version"]) {
        assert!(
            std::env::var_os("TINOX_REQUIRE_COMPOSITION").is_none()
                && std::env::var("GITHUB_ACTIONS").as_deref() != Ok("true"),
            "required composition infrastructure unavailable"
        );
        eprintln!("SKIP service composition: real Docker PostgreSQL and psql required");
        return;
    }
    let root = repo_root();
    let pg = PgContainer::start("postgres:16-alpine");
    pg.psql("CREATE TABLE accounts (id SERIAL PRIMARY KEY, balance BIGINT NOT NULL); INSERT INTO accounts(balance) VALUES (0);");
    for fresh in [false, true] {
        let directory =
            std::env::temp_dir().join(format!("tinox-composition-{}-{fresh}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        copy_tree(&root.join("tests/fixtures/service_composition"), &directory);
        let mut manifest = format!("[package]\ncompiler=\"2.8.0\"\nname=\"composition\"\nversion=\"0.0.0\"\n[startup]\nbanner=false\n[database]\ndriver=\"postgres\"\nurl=\"{}\"\npool=2\n", pg.url());
        for module in ["http_server", "json", "websocket", "db", "http"] {
            let version = if !fresh {
                "9.9.9"
            } else if module == "db" {
                "1.2.2"
            } else {
                "1.1.1"
            };
            manifest.push_str(&format!("[[dependencies]]\ngroup=\"tinox.core\"\nartifactId=\"{module}\"\nversion=\"{version}\"\n"));
            if !fresh {
                copy_tree(
                    &root
                        .join("crates/tinox-core-ext")
                        .join(module)
                        .join("tinox"),
                    &directory.join(format!(
                        ".tinox/repository/central/tinox.core/{module}/{version}/tinox"
                    )),
                );
            }
        }
        fs::write(directory.join("tinox.toml"), manifest).unwrap();
        if fresh {
            assert!(!directory.join(".tinox").exists());
            success(
                Command::new(env!("CARGO_BIN_EXE_tinox"))
                    .current_dir(&directory)
                    .env("TINOX_HOME", &directory)
                    .arg("install"),
            );
        }
        success(
            Command::new(env!("CARGO_BIN_EXE_tinox"))
                .current_dir(&directory)
                .env("TINOX_HOME", &directory)
                .env("TINOX_PORT", "0")
                .args(["build", "Main.tnx", "-o", "service"]),
        );
        let mut client = Command::new("python3")
            .arg(root.join("crates/tinox/tests/common/service_composition.py"))
            .arg(directory.join("service"))
            .arg(pg.url())
            .process_group(0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if common::wait_timeout(&mut client, Duration::from_secs(180)).is_none() {
            // Kill the whole scenario group, including its service subprocess.
            let _ = Command::new("kill")
                .args(["-KILL", "--", &format!("-{}", client.id())])
                .status();
            panic!("composed service scenario timed out; workdir={directory:?}");
        }
        let output = client.wait_with_output().unwrap();
        assert!(output.status.success(), "fresh={fresh}: {output:?}");
        eprintln!("{}", String::from_utf8_lossy(&output.stdout));
        fs::remove_dir_all(directory).unwrap();
    }
}
