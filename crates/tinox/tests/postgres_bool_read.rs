//! `Bool` entity fields read from PostgreSQL `boolean` columns (issue #604),
//! verified against a REAL Postgres (docker). PostgreSQL sends booleans as
//! `t`/`f`, which the row mapper used to parse with `atoll()` (always 0).

use std::process::Command;

mod pg_common;
use pg_common::{repo_root, tool_available, PgContainer};

#[test]
fn bool_entity_fields_read_back_from_postgres_boolean_columns() {
    if !tool_available("docker", &["--version"]) {
        eprintln!("SKIP bool_entity_fields_read_back_from_postgres_boolean_columns (docker not installed)");
        return;
    }
    if !tool_available("psql", &["--version"]) {
        eprintln!("SKIP bool_entity_fields_read_back_from_postgres_boolean_columns (psql not installed)");
        return;
    }

    let pg = PgContainer::start("postgres:16-alpine");
    pg.psql("CREATE TABLE flags (id SERIAL PRIMARY KEY, name TEXT NOT NULL, enabled BOOLEAN NOT NULL);");
    pg.psql("INSERT INTO flags (name, enabled) VALUES ('on', TRUE), ('off', FALSE);");

    let tinox = env!("CARGO_BIN_EXE_tinox");
    let root = repo_root();
    let fixture = root.join("tests/fixtures/postgres_bool_read");
    let workdir = std::env::temp_dir().join(format!("tinox-pg-bool-read-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(workdir.join("src")).expect("mkdir workdir/src");

    for f in ["Flag.tnx", "Main.tnx"] {
        std::fs::copy(fixture.join(f), workdir.join("src").join(f)).unwrap_or_else(|e| panic!("copy {f}: {e}"));
    }

    std::fs::write(
        workdir.join("tinox.toml"),
        format!(
            r#"[package]
compiler = "2.9.4"
name = "postgres_bool_read"
version = "0.1.0"
description = "issue #604 fixture"

[database]
driver = "postgres"
url = "{}"
pool = 3

[[dependencies]]
group = "tinox.core"
artifactId = "db"
version = "1.2.8"

[[overrides]]
group = "tinox.core"
artifactId = "db"
path = "{db_dir}"
"#,
            pg.url(),
            db_dir = repo_root().join("crates/tinox-core-ext/db").display()
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

    assert!(stdout.contains("on=true"), "TRUE column must read as true:\n{stdout}");
    assert!(stdout.contains("off=false"), "FALSE column must read as false:\n{stdout}");
    assert!(stdout.contains("roundtrip=true"), "saved Bool must read back as true:\n{stdout}");

    // Independent verification against real Postgres state.
    let saved = pg.psql("SELECT enabled FROM flags WHERE name = 'saved';");
    assert_eq!(saved, "t", "psql: {saved}");

    let _ = std::fs::remove_dir_all(&workdir);
}
