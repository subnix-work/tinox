//! @Transactional + the Postgres connection pool (issue #191), verified
//! against a REAL Postgres (docker), not a simulated/mocked driver --
//! per this repo's "verify against real, independent systems" philosophy
//! (CLAUDE.md). Two things are checked independently of the compiled
//! program's own stdout, via a separate `psql` client querying the
//! database directly afterward:
//!
//!   1. A successful @Transactional method's changes are actually
//!      committed (both UPDATEs inside it land).
//!   2. A failing @Transactional method's changes are fully rolled back
//!      (neither UPDATE inside it survives, not just the one nearest the
//!      throw) -- the whole point of wrapping BOTH saves in one BEGIN.
//!
//! SKIPs gracefully (matching e2e.rs's own sqlite3-not-installed SKIP
//! pattern) if `docker` or `psql` aren't available -- this is real
//! infrastructure the dev/CI machine must provide, not something to fake.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn tool_available(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

struct PgContainer {
    name: String,
    port: u16,
}

impl PgContainer {
    fn start() -> Self {
        let name = format!("tinox-pg-transactional-test-{}", std::process::id());
        let _ = Command::new("docker").args(["rm", "-f", &name]).output();
        let status = Command::new("docker")
            .args([
                "run", "-d", "--rm",
                "--name", &name,
                "-e", "POSTGRES_HOST_AUTH_METHOD=trust",
                "-P", // publish exposed ports to random host ports -- no
                      // hardcoded literal, same "no manually-picked port"
                      // convention CLAUDE.md's e2e fixtures already use.
                "postgres:16-alpine",
            ])
            .stdout(Stdio::null())
            .status()
            .expect("spawn docker run");
        assert!(status.success(), "docker run postgres failed");

        let port_out = Command::new("docker")
            .args(["port", &name, "5432/tcp"])
            .output()
            .expect("docker port");
        assert!(port_out.status.success(), "docker port failed");
        let port_str = String::from_utf8_lossy(&port_out.stdout);
        // "0.0.0.0:54321\n[::]:54321\n" -- take the first line's port.
        let port: u16 = port_str
            .lines()
            .next()
            .and_then(|l| l.rsplit(':').next())
            .and_then(|p| p.trim().parse().ok())
            .unwrap_or_else(|| panic!("could not parse docker port output: {port_str}"));

        let container = PgContainer { name, port };
        container.wait_ready();
        container
    }

    fn wait_ready(&self) {
        // Probes the way this test itself will actually connect -- a real
        // query, from the HOST, over the MAPPED port.
        //
        // `docker exec pg_isready` (what this used to do) is not enough,
        // and that gap made this test genuinely flaky in CI: it failed
        // once with "psql: server closed the connection unexpectedly ...
        // server terminated abnormally before or while processing the
        // request" while the very same commit passed on a rerun. The
        // official postgres image runs a TEMPORARY server during
        // initdb/init-script processing, deliberately started with
        // `listen_addresses=''` so it's reachable only over the
        // container's own unix socket -- which is exactly what an
        // in-container pg_isready talks to. So pg_isready reports "ready"
        // during a phase where the published TCP port isn't serving the
        // final server at all; the entrypoint then shuts that temporary
        // server down and starts the real one. A host-side connect
        // landing in that window gets accepted and then dropped, which is
        // precisely the observed error.
        //
        // Checking for an answered query (not just an accepted TCP
        // connection) is what closes the window completely: it cannot
        // succeed against the temporary server (not listening on TCP),
        // and it cannot succeed during the restart gap either.
        //
        // Mechanism verified directly against a real PostgreSQL 18.6
        // rather than inferred from the image's entrypoint script: with a
        // server started `-c listen_addresses=''` (exactly what the
        // temporary init server uses), `pg_isready` over the unix socket
        // reports "accepting connections" and exits 0, while a host-side
        // TCP connect at the same moment gets "Connection refused". The
        // old check returned on precisely that signal.
        for _ in 0..120 {
            if self.try_query("SELECT 1").is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        panic!(
            "postgres container never became ready on 127.0.0.1:{}",
            self.port
        );
    }

    /// Non-panicking counterpart to `psql` below: returns `None` while the
    /// server isn't answering (yet), rather than failing the test. Only
    /// used for readiness polling -- every real assertion goes through
    /// `psql`, which still treats a failure as fatal.
    fn try_query(&self, sql: &str) -> Option<String> {
        let out = Command::new("psql")
            .args([
                "-h", "127.0.0.1",
                "-p", &self.port.to_string(),
                "-U", "postgres",
                "-d", "postgres",
                "-t", "-A",
                "-c", sql,
            ])
            .env("PGPASSWORD", "postgres")
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn psql(&self, sql: &str) -> String {
        let out = Command::new("psql")
            .args([
                "-h", "127.0.0.1",
                "-p", &self.port.to_string(),
                "-U", "postgres",
                "-d", "postgres",
                "-t", "-A", // tuples-only, unaligned -- easy to parse
                "-c", sql,
            ])
            .env("PGPASSWORD", "postgres")
            .output()
            .expect("spawn psql");
        assert!(
            out.status.success(),
            "psql failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn url(&self) -> String {
        format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", self.port)
    }
}

impl Drop for PgContainer {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "-f", &self.name]).output();
    }
}

#[test]
fn transactional_commits_on_success_and_rolls_back_on_error() {
    if !tool_available("docker", &["--version"]) {
        eprintln!("SKIP transactional_commits_on_success_and_rolls_back_on_error (docker not installed)");
        return;
    }
    if !tool_available("psql", &["--version"]) {
        eprintln!("SKIP transactional_commits_on_success_and_rolls_back_on_error (psql not installed)");
        return;
    }

    let pg = PgContainer::start();
    pg.psql("CREATE TABLE accounts (id SERIAL PRIMARY KEY, name TEXT NOT NULL, balance BIGINT NOT NULL);");
    pg.psql("INSERT INTO accounts (name, balance) VALUES ('Alice', 1000), ('Bob', 200);");

    let tinox = env!("CARGO_BIN_EXE_tinox");
    let root = repo_root();
    let fixture = root.join("tests/fixtures/postgres_transactional");
    let workdir = std::env::temp_dir().join(format!(
        "tinox-pg-transactional-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(workdir.join("src")).expect("mkdir workdir/src");

    for f in ["Account.tnx", "BankService.tnx", "Main.tnx"] {
        std::fs::copy(fixture.join(f), workdir.join("src").join(f))
            .unwrap_or_else(|e| panic!("copy {f}: {e}"));
    }

    std::fs::write(
        workdir.join("tinox.toml"),
        format!(
            r#"[package]
name = "postgres_transactional"
version = "0.1.0"
description = "issue #191 e2e fixture"

[database]
driver = "postgres"
url = "{}"
pool = 3

[[dependencies]]
group = "tinox.core"
artifactId = "db"
version = "1.0.0"
"#,
            pg.url()
        ),
    )
    .expect("write tinox.toml");

    let install = Command::new(tinox)
        .arg("install")
        .current_dir(&workdir)
        .output()
        .expect("spawn install");
    assert!(
        install.status.success(),
        "tinox install failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&install.stdout),
        String::from_utf8_lossy(&install.stderr)
    );

    let exe = workdir.join("app");
    let build = Command::new(tinox)
        .arg("build")
        .arg(workdir.join("src/Main.tnx"))
        .arg("-o")
        .arg(&exe)
        .current_dir(&workdir)
        .output()
        .expect("spawn build");
    assert!(
        build.status.success(),
        "build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let run = Command::new(&exe)
        .current_dir(&workdir)
        .output()
        .expect("spawn compiled program");
    assert!(
        run.status.success(),
        "program failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains("transfer1 done"), "stdout: {stdout}");
    assert!(stdout.contains("transfer2 rolled back: insufficient funds"), "stdout: {stdout}");
    assert!(stdout.contains("balance after touch: 501"), "stdout: {stdout}");

    // Independent verification against real Postgres state, not just the
    // program's own stdout: transfer1 (500) must be committed, transfer2
    // (999999, overdraws Alice) must be FULLY rolled back -- both saves,
    // not just the one nearest the throw -- and getBalanceAndTouch's +1
    // (an @Transactional method that ends in an explicit `return`, unlike
    // transfer's implicit fall-through) must ALSO be committed, not
    // silently lost the way the explicit-return-skips-commit bug this
    // fixture was extended to catch would have left it.
    let alice_balance = pg.psql("SELECT balance FROM accounts WHERE name = 'Alice';");
    let bob_balance = pg.psql("SELECT balance FROM accounts WHERE name = 'Bob';");
    assert_eq!(alice_balance, "501", "Alice's balance should reflect the committed transfer1 (500) plus getBalanceAndTouch's committed +1");
    assert_eq!(bob_balance, "700", "Bob's balance should reflect only the committed transfer1");

    let _ = std::fs::remove_dir_all(&workdir);
}
