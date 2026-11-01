//! Shared helpers for the real-Postgres-via-docker test binaries
//! (postgres_transactional.rs, postgres_raw_query.rs) — deliberately kept
//! separate from `common/mod.rs` rather than added there: `common`'s
//! existing items are all reachable from `parse_case`/`run_case`, which
//! EVERY binary that declares `mod common;` already uses, so nothing in it
//! is ever dead code in any of those crates. `PgContainer`/`tool_available`
//! have no such relationship to the golden-test harness — a binary that
//! only needs the sqlite-based `run_case` machinery (e2e.rs, matrix.rs, ...)
//! would never reach them, and this workspace's clippy gate is `-D
//! warnings` (0 unused-item allowance) per binary. A separate module that
//! only the docker-based binaries declare keeps both fully used, everywhere
//! they're declared.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Deliberately NOT imported from `common::repo_root` (identical
/// definition) — pulling in `common` here would pull in exactly one used
/// item (`repo_root`) alongside a dozen golden-test-harness items
/// (`parse_case`/`run_case`/`Case`/...) this module's own consumers never
/// reach, which is dead code under this workspace's `-D warnings` clippy
/// gate. Five duplicated lines beats that cross-module coupling.
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// Whether `cmd args` exits successfully — used to SKIP (not fail) a test
/// when required real infrastructure (docker, psql) isn't installed on this
/// machine, matching this harness's existing sqlite3-not-installed SKIP.
pub fn tool_available(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// A real, ephemeral Postgres container (issue #191's "verify against real,
/// independent systems" philosophy — no mock driver). `-P` publishes to a
/// random host port (no hardcoded literal, per this repo's e2e-fixture
/// convention); `Drop` always removes the container, even on panic.
pub struct PgContainer {
    name: String,
    port: u16,
}

impl PgContainer {
    /// `image`, e.g. `"postgres:16-alpine"` (generic SQL) or
    /// `"pgvector/pgvector:pg16"` (needs `CREATE EXTENSION vector`).
    pub fn start(image: &str) -> Self {
        let name = format!("tinox-pg-test-{}", std::process::id());
        let _ = Command::new("docker").args(["rm", "-f", &name]).output();
        let status = Command::new("docker")
            .args([
                "run", "-d", "--rm",
                "--name", &name,
                "-e", "POSTGRES_HOST_AUTH_METHOD=trust",
                "-P",
                image,
            ])
            .stdout(Stdio::null())
            .status()
            .expect("spawn docker run");
        assert!(status.success(), "docker run {image} failed");

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

    pub fn psql(&self, sql: &str) -> String {
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

    /// True when the server REJECTS the statement (constraint violations and the like).
    #[allow(dead_code)]
    pub fn psql_rejects(&self, sql: &str) -> bool {
        let out = Command::new("psql")
            .args(["-h", "127.0.0.1", "-p", &self.port.to_string(), "-U", "postgres", "-d", "postgres", "-c", sql])
            .env("PGPASSWORD", "postgres")
            .output()
            .expect("spawn psql");
        !out.status.success()
    }

    pub fn url(&self) -> String {
        format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", self.port)
    }
}

impl Drop for PgContainer {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "-f", &self.name]).output();
    }
}
