//! `DB.raw(sql, params)` (ai-vault prerequisite: a raw-SQL escape hatch for
//! `tinox.core.db`, added because the `filter()/orderBy()/...` ORM DSL can't
//! express a pgvector `ORDER BY embedding <-> $1` query), verified against a
//! REAL Postgres (docker), not a simulated/mocked driver -- same "verify
//! against real, independent systems" philosophy as postgres_transactional.rs,
//! whose `PgContainer`/`tool_available`/`repo_root` helpers this test reuses
//! via `common` rather than duplicating.
//!
//! Checked, independently of the compiled program's own stdout via a
//! separate `psql` seed/query:
//!   1. multiple rows come back, in `ORDER BY` order, with the bound `$1`
//!      parameter actually filtering (proves params: List<String> -> $N
//!      binding works, not just a hardcoded/ignored query string);
//!   2. a NULL column is simply absent from its row's Map (`.contains`
//!      returns false), not a sentinel value;
//!   3. a query matching zero rows comes back as an empty list, not an
//!      error.

use std::process::Command;

mod pg_common;
use pg_common::{repo_root, tool_available, PgContainer};

#[test]
fn raw_query_binds_params_and_omits_null_columns() {
    if !tool_available("docker", &["--version"]) {
        eprintln!("SKIP raw_query_binds_params_and_omits_null_columns (docker not installed)");
        return;
    }
    if !tool_available("psql", &["--version"]) {
        eprintln!("SKIP raw_query_binds_params_and_omits_null_columns (psql not installed)");
        return;
    }

    let pg = PgContainer::start("postgres:16-alpine");
    pg.psql("CREATE TABLE items (id SERIAL PRIMARY KEY, name TEXT NOT NULL, price FLOAT8 NOT NULL, note TEXT);");
    // 'widget' has a note, 'gadget' does not (NULL) -- both above the $1
    // threshold the fixture queries with; a third, cheaper row must be
    // excluded by the WHERE, proving the param actually filters.
    pg.psql("INSERT INTO items (name, price, note) VALUES ('widget', 9.99, 'first'), ('gadget', 19.99, NULL), ('cheap', 1.00, 'excluded');");

    let tinox = env!("CARGO_BIN_EXE_tinox");
    let root = repo_root();
    let fixture = root.join("tests/fixtures/postgres_raw_query");
    let workdir = std::env::temp_dir().join(format!("tinox-pg-raw-query-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(workdir.join("src")).expect("mkdir workdir/src");

    std::fs::copy(fixture.join("Main.tnx"), workdir.join("src/Main.tnx")).expect("copy Main.tnx");

    std::fs::write(
        workdir.join("tinox.toml"),
        format!(
            r#"[package]
name = "postgres_raw_query"
version = "0.1.0"
description = "DB.raw e2e fixture"

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

    // Both qualifying rows, in id order (proves $1 filtered out 'cheap' and
    // ORDER BY held).
    assert!(stdout.contains("rows: 2"), "stdout: {stdout}");
    let widget_pos = stdout.find("row id=1 name=widget hasNote=true").unwrap_or_else(|| panic!("missing widget row:\n{stdout}"));
    let gadget_pos = stdout.find("row id=2 name=gadget hasNote=false").unwrap_or_else(|| panic!("missing gadget row (NULL note must be absent, not a sentinel):\n{stdout}"));
    assert!(widget_pos < gadget_pos, "rows out of ORDER BY id order:\n{stdout}");

    // A WHERE matching nothing must come back as an empty list, not an error.
    assert!(stdout.contains("none: 0"), "stdout: {stdout}");

    // Independent verification against real Postgres state, not just the
    // program's own claims: the seed data is exactly as inserted (DB.raw is
    // read-only here, nothing should have changed it).
    let count = pg.psql("SELECT count(*) FROM items;");
    assert_eq!(count, "3", "psql: {count}");

    let _ = std::fs::remove_dir_all(&workdir);
}
