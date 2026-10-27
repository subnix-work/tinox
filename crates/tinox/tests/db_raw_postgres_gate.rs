//! Compile-time "postgres-only" gates: `DB::raw(...)` (new) and
//! `@Transactional` (pre-existing, added in issue #191 — found while
//! researching `DB::raw` that it had no dedicated test of its own, despite
//! being a hard compile error since 2026-08-14; fixed here alongside the
//! new feature that needed the same kind of check). Both fire before
//! codegen/linking ever runs, so unlike postgres_transactional.rs /
//! postgres_raw_query.rs this needs no docker/psql -- just `tinox build`
//! against a project with no (or a non-postgres) `[database] driver`.

use std::path::PathBuf;
use std::process::Command;

// Deliberately private and undeclared as a shared module: this binary is
// the only one of the three DB::raw/@Transactional-adjacent test binaries
// that needs no docker/psql machinery at all (it's a pure compile-time
// check), so it doesn't share `pg_common.rs` (whose `tool_available`/
// `PgContainer` would then be dead code here under `-D warnings`).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn build_fixture(fixture_dir: &str, workdir_name: &str, database_toml: &str) -> std::process::Output {
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let root = repo_root();
    let fixture = root.join("tests/fixtures").join(fixture_dir);
    let workdir = std::env::temp_dir().join(format!("tinox-{workdir_name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(workdir.join("src")).expect("mkdir workdir/src");

    for entry in std::fs::read_dir(&fixture).expect("read fixture dir") {
        let entry = entry.expect("dir entry");
        let name = entry.file_name();
        std::fs::copy(entry.path(), workdir.join("src").join(&name))
            .unwrap_or_else(|e| panic!("copy {name:?}: {e}"));
    }

    let db_dir = root.join("crates/tinox-core-ext/db").display().to_string();
    std::fs::write(
        workdir.join("tinox.toml"),
        format!(
            r#"[package]
compiler = "2.7.0"
name = "{workdir_name}"
version = "0.1.0"
{database_toml}
[[dependencies]]
group = "tinox.core"
artifactId = "db"
version = "1.1.0"

[[overrides]]
group = "tinox.core"
artifactId = "db"
path = "{db_dir}"
"#
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

    let out = Command::new(tinox)
        .arg("build")
        .arg(workdir.join("src/Main.tnx"))
        .arg("-o")
        .arg(workdir.join("app"))
        .current_dir(&workdir)
        .output()
        .expect("spawn build");

    let _ = std::fs::remove_dir_all(&workdir);
    out
}

#[test]
fn db_raw_without_postgres_driver_is_a_compile_error() {
    let out = build_fixture("db_raw_gate", "db-raw-gate", "");
    assert!(!out.status.success(), "expected build to fail with no [database] configured");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("DB::raw"), "stderr: {stderr}");
    assert!(stderr.contains("Thing.doit"), "error should name the offending class.method:\n{stderr}");
}

#[test]
fn transactional_without_postgres_driver_is_a_compile_error() {
    // Pre-existing check (issue #191), previously untested end-to-end --
    // reuses the postgres_transactional.rs fixture's classes, just built
    // with no [database] driver instead of a running Postgres.
    let out = build_fixture("postgres_transactional", "transactional-gate", "");
    assert!(!out.status.success(), "expected build to fail with no [database] configured");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("@Transactional"), "stderr: {stderr}");
    assert!(stderr.contains("only supported for driver"), "stderr: {stderr}");
}
