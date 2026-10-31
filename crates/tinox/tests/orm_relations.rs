//! Relationship loading without a hidden N+1 (issue #534) against a real PostgreSQL: the schema comes from
//! `tinox db generate` + `tinox db update`, the foreign key is enforced by the server, joined results are correct and
//! the number of SELECTs per operation is read from the server's own statement log (not from the program).

use std::path::PathBuf;
use std::process::{Command, Output};

mod pg_common;
use pg_common::{repo_root, tool_available, PgContainer};

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

/// How many logged statements of the server mention `needle` (the log is the server's, not the program's).
fn logged(container: &str, needle: &str) -> usize {
    let out = Command::new("docker").args(["logs", container]).output().unwrap();
    text(&out).lines().filter(|l| l.contains("statement:") || l.contains("execute ")).filter(|l| l.contains(needle)).count()
}

#[test]
fn list_and_detail_loading_uses_a_bounded_number_of_queries() {
    if !tool_available("docker", &["--version"]) || !tool_available("psql", &["--version"]) {
        eprintln!("SKIP relations (docker/psql not installed)");
        return;
    }
    let pg = PgContainer::start("postgres:16-alpine");
    let container = format!("tinox-pg-test-{}", std::process::id());
    pg.psql("ALTER SYSTEM SET log_statement = 'all'");
    pg.psql("SELECT pg_reload_conf()");

    let root = repo_root();
    let dir: PathBuf = std::env::temp_dir().join(format!("tinox-relations-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    for f in ["Author.tnx", "Book.tnx", "Main.tnx"] {
        std::fs::copy(root.join("tests/fixtures/orm_relations").join(f), dir.join("src").join(f)).unwrap();
    }
    std::fs::write(
        dir.join("tinox.toml"),
        format!("[package]\ncompiler = \"2.8.0\"\nname = \"relations\"\nversion = \"0.0.0\"\n\n[database]\ndriver = \"postgres\"\nurl = \"{}\"\npool = 3\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"db\"\nversion = \"1.1.0\"\n", pg.url()),
    )
    .unwrap();
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let run = |args: &[&str]| Command::new(tinox).args(args).current_dir(&dir).output().unwrap();
    let install = run(&["install"]);
    assert!(install.status.success(), "{}", text(&install));
    // The schema is generated from the entities (parents first) and applied by the migration engine.
    let generate = run(&["db", "generate"]);
    assert!(generate.status.success(), "{}", text(&generate));
    let update = run(&["db", "update"]);
    assert!(update.status.success(), "{}", text(&update));
    let fk = pg.psql("SELECT confrelid::regclass::text FROM pg_constraint WHERE conrelid = 'books'::regclass AND contype = 'f'");
    assert_eq!(fk, "authors", "the generated schema carries the foreign key");

    pg.psql("INSERT INTO authors (name) VALUES ('Ada'), ('Bob'), ('Cy')");
    let mut values = Vec::new();
    for i in 1..=12 {
        values.push(format!("('book-{i:02}', {})", (i % 3) + 1));
    }
    pg.psql(&format!("INSERT INTO books (title, author_id) VALUES {}", values.join(",")));
    // Referential behaviour enforced by the server: no orphans, no deleting a referenced parent.
    assert!(pg.psql_rejects("INSERT INTO books (title, author_id) VALUES ('x', 999)"), "orphan insert must be rejected");
    assert!(pg.psql_rejects("DELETE FROM authors WHERE id = 1"), "a referenced parent must not be deletable");

    let build = run(&["build", "src/Main.tnx", "-o", "app"]);
    assert!(build.status.success(), "{}", text(&build));
    let exec = |mode: &str| Command::new(dir.join("app")).current_dir(&dir).env("MODE", mode).output().unwrap();

    let before = logged(&container, "FROM authors");
    let n = exec("nplus1");
    assert!(n.status.success(), "{}", text(&n));
    let n_queries = logged(&container, "FROM authors") - before;
    assert_eq!(n_queries, 12, "the naive loop is one query per book");

    let before = logged(&container, "FROM authors");
    let books_before = logged(&container, "FROM books");
    let b = exec("batch");
    let out = text(&b);
    assert!(b.status.success(), "{out}");
    assert_eq!(logged(&container, "FROM authors") - before, 1, "the batch loads all authors with ONE query");
    assert_eq!(logged(&container, "FROM books") - books_before, 1);
    // Joined results are right: book-01 has author_id 2 (Bob), book-02 -> 3 (Cy), book-03 -> 1 (Ada) ...
    for expected in ["book-01 by Bob", "book-02 by Cy", "book-03 by Ada", "book-12 by Ada"] {
        assert!(out.contains(expected), "missing {expected:?} in:\n{out}");
    }
    assert_eq!(out.lines().filter(|l| l.contains(" by ")).count(), 12);
    assert_eq!(text(&n).lines().filter(|l| l.contains(" by ")).collect::<Vec<_>>(), out.lines().filter(|l| l.contains(" by ")).collect::<Vec<_>>(), "both strategies return the same join");

    // The other direction: the books of several authors in one query.
    let before = logged(&container, "FROM books");
    let c = exec("children");
    let out = text(&c);
    assert!(c.status.success(), "{out}");
    assert_eq!(logged(&container, "FROM books") - before, 2, "one query for the listing in main, one for the children");
    let expected = pg.psql("SELECT string_agg(title, ',' ORDER BY id) FROM books WHERE author_id IN (1, 3)");
    assert_eq!(out.lines().filter(|l| l.starts_with("book-")).collect::<Vec<_>>().join(","), expected);
    let _ = std::fs::remove_dir_all(&dir);
}
