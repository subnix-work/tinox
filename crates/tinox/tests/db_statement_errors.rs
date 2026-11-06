//! #608: statement errors unwind normally, release pool slots and roll back real PostgreSQL work.
use std::{fs, path::PathBuf, process::{Command, Output, Stdio}, time::Duration};

mod pg_common;
use pg_common::{repo_root, tool_available, PgContainer};
#[allow(dead_code)]
mod common;

fn text(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

struct Project(PathBuf);
impl Drop for Project {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

impl Project {
    fn run(&self, mode: &str) -> Output {
        let mut child = Command::new(self.0.join("app")).current_dir(&self.0).env("MODE", mode)
            .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        common::wait_timeout(&mut child, Duration::from_secs(20))
            .expect("database error handling leaked a pool connection or hung");
        child.wait_with_output().unwrap()
    }
}

#[test]
fn postgres_statement_errors_are_catchable_and_preserve_transaction_outcomes() {
    if !tool_available("docker", &["--version"]) || !tool_available("psql", &["--version"]) {
        eprintln!("SKIP database statement errors (docker/psql unavailable)");
        return;
    }
    let pg = PgContainer::start("postgres:16-alpine");
    pg.psql("CREATE TABLE items (id SERIAL PRIMARY KEY, name TEXT NOT NULL, amount BIGINT CHECK (amount >= 0));
        INSERT INTO items (name, amount) VALUES ('original', 7);
        CREATE TABLE item_refs (item BIGINT REFERENCES items(id)); INSERT INTO item_refs VALUES (1);
        CREATE TABLE memberships (org BIGINT, member BIGINT, amount BIGINT CHECK (amount >= 0), PRIMARY KEY (org, member));
        INSERT INTO memberships VALUES (1, 1, 9);
        CREATE TABLE member_refs (org BIGINT, member BIGINT, FOREIGN KEY (org, member) REFERENCES memberships(org, member));
        INSERT INTO member_refs VALUES (1, 1);");
    let project = Project(std::env::temp_dir().join(format!("tinox-db-errors-{}", std::process::id())));
    fs::create_dir_all(project.0.join("src")).unwrap();
    for name in ["Main", "Service", "Item", "Membership", "Missing"] {
        fs::copy(repo_root().join(format!("tests/fixtures/db_statement_errors/{name}.tnx")),
            project.0.join(format!("src/{name}.tnx"))).unwrap();
    }
    fs::write(project.0.join("tinox.toml"), format!(
        "[package]\ncompiler=\"2.9.2\"\nname=\"db_errors\"\nversion=\"0.0.0\"\n\
        [database]\ndriver=\"postgres\"\nurl=\"{}\"\npool=1\n\
        [[dependencies]]\ngroup=\"tinox.core\"\nartifactId=\"db\"\nversion=\"1.2.5\"\n\
        [[overrides]]\ngroup=\"tinox.core\"\nartifactId=\"db\"\npath=\"{}\"\n",
        pg.url(), repo_root().join("crates/tinox-core-ext/db").display())).unwrap();
    for args in [&["install"][..], &["build", "src/Main.tnx", "-o", "app"][..]] {
        let output = Command::new(env!("CARGO_BIN_EXE_tinox")).current_dir(&project.0).args(args).output().unwrap();
        assert!(output.status.success(), "{}", text(&output));
    }
    let run = project.run("statements");
    let out = text(&run);
    assert!(run.status.success(), "{out}");
    assert!(run.stderr.is_empty(), "caught errors should not be printed to stderr: {out}");
    assert_eq!(out.matches("raw cleanup\n").count(), 8, "{out}");
    assert_eq!(out.matches("caught json: ").count(), 8, "{out}");
    for marker in ["insert", "update", "delete", "list", "first", "count", "composite insert",
        "composite update", "composite delete", "composite lookup", "transaction", "rollback"] {
        assert_eq!(out.matches(&format!("caught {marker}: ")).count(), 1, "{out}");
    }
    assert!(out.contains("invalid input syntax for type json"), "driver detail lost: {out}");
    assert!(out.contains("violates check constraint") && out.contains("violates foreign key constraint"), "{out}");
    assert!(out.contains("transaction rolled back after a failed statement"), "{out}");
    assert!(out.ends_with("healthy rows: 1\nstill alive\n"), "{out}");
    assert!(!out.contains("unreachable"), "{out}");
    // Independent client verifies no partial writes, including an error caught inside the transaction.
    assert_eq!(pg.psql("SELECT name || ':' || amount FROM items ORDER BY id"), "original:7");
    assert_eq!(pg.psql("SELECT org || ':' || member || ':' || amount FROM memberships"), "1:1:9");

    let disconnected = project.run("disconnect");
    let out = text(&disconnected);
    assert!(disconnected.status.success(), "{out}");
    assert!(disconnected.stderr.is_empty(), "{out}");
    assert!(out.contains("caught disconnect: ") && out.contains("begin recovered"), "{out}");
    assert!(!out.contains("caught begin: "), "the pool must reconnect: {out}");
    assert!(out.ends_with("still alive\n"), "{out}");
    assert_eq!(pg.psql("SELECT amount FROM items WHERE id = 1"), "7");
}
