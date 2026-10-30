//! Transaction propagation and savepoints (issue #532) against a REAL Postgres. The program's
//! own output is cross-checked with a separate `psql` client reading the final table.

use std::process::Command;

mod pg_common;
use pg_common::{repo_root, tool_available, PgContainer};

#[test]
fn propagation_modes_and_savepoints_behave_against_real_postgres() {
    if !tool_available("docker", &["--version"]) || !tool_available("psql", &["--version"]) {
        eprintln!("SKIP propagation test (docker/psql not installed)");
        return;
    }
    let pg = PgContainer::start("postgres:16-alpine");
    pg.psql("CREATE TABLE notes (id SERIAL PRIMARY KEY, tag TEXT NOT NULL);");

    let tinox = env!("CARGO_BIN_EXE_tinox");
    let fixture = repo_root().join("tests/fixtures/postgres_propagation");
    let workdir = std::env::temp_dir().join(format!("tinox-pg-propagation-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(workdir.join("src")).expect("mkdir");
    for f in ["Note.tnx", "Flows.tnx", "Main.tnx"] {
        std::fs::copy(fixture.join(f), workdir.join("src").join(f)).unwrap();
    }
    std::fs::write(
        workdir.join("tinox.toml"),
        format!(
            r#"[package]
compiler = "2.8.0"
name = "postgres_propagation"
version = "0.1.0"
description = "issue #532 e2e fixture"

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
    .unwrap();
    let install = Command::new(tinox).arg("install").current_dir(&workdir).output().unwrap();
    assert!(install.status.success(), "install failed: {}", String::from_utf8_lossy(&install.stderr));
    let exe = workdir.join("app");
    let build = Command::new(tinox)
        .arg("build")
        .arg(workdir.join("src/Main.tnx"))
        .arg("-o")
        .arg(&exe)
        .current_dir(&workdir)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "build failed:\n{}{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(&exe).current_dir(&workdir).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "program failed:\n{stdout}\n{stderr}");

    assert!(stdout.contains("caught: nested failed"), "{stdout}");
    assert!(stdout.contains("returned: 7"), "{stdout}");
    assert!(stdout.contains("refused: transaction rolled back"), "{stdout}");
    assert!(stdout.contains("outer: outer failed"), "{stdout}");
    assert!(stdout.contains("caught: independent failed"), "{stdout}");
    // The independent transaction runs on its own connection and cannot see uncommitted rows.
    assert!(stdout.contains("independent sees outer: 0"), "{stdout}");

    let tags = pg.psql("SELECT string_agg(tag, ',' ORDER BY tag) FROM notes;");
    assert_eq!(
        tags,
        "i-inner-kept,j-outer-1,j-outer-2,m-outer,n-inner-ok,n-inner-returned,n-outer-1,n-outer-2,s-nested-standalone",
        "stdout: {stdout}\nstderr: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&workdir);
}
