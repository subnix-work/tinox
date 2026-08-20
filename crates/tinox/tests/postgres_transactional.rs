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

use std::process::Command;

mod pg_common;
use pg_common::{repo_root, tool_available, PgContainer};

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

    let pg = PgContainer::start("postgres:16-alpine");
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
compiler = "2.7.0"
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
version = "1.1.0"
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
    assert_eq!(stdout.matches("missing: 0::0\n").count(), 3, "stdout: {stdout}");
    assert_eq!(stdout.matches("missing list: 0\n").count(), 3, "stdout: {stdout}");
    assert_eq!(stdout.matches("missing count: 0\n").count(), 3, "stdout: {stdout}");
    assert!(stdout.contains("missing id: 0\nmissing name: []\nmissing balance: 0\nempty list: 0\nempty count: 0\n"), "stdout: {stdout}");
    assert!(run.stderr.is_empty(), "unexpected stderr: {}", String::from_utf8_lossy(&run.stderr));
    assert!(stdout.contains("transfer1 done"), "stdout: {stdout}");
    assert!(stdout.contains("transfer2 rolled back: insufficient funds"), "stdout: {stdout}");
    assert!(stdout.contains("balance after touch: 501"), "stdout: {stdout}");

    assert_eq!(stdout.matches("cancelled rollback: Task cancelled").count(), 20, "stdout: {stdout}");

    assert_eq!(stdout.matches("guard released: 501").count(), 20, "stdout: {stdout}");

    // All twenty cancelled tasks must have rolled back before their joins
    // completed; otherwise these independent balances include +10000 changes.
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
