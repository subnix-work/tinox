//! Composite @Id keys (issue #535): DDL generated from the entity, ORM save/delete on every key
//! component, verified by independent `sqlite3` reads of the schema and the rows.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn sqlite(db: &Path, sql: &str) -> String {
    let out = Command::new("sqlite3").arg(db).arg(sql).output().expect("sqlite3");
    assert!(out.status.success(), "sqlite3: {}", text(&out));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn composite_primary_key_ddl_and_crud_use_every_component() {
    if Command::new("sqlite3").arg("--version").output().is_err() {
        eprintln!("SKIP composite keys (sqlite3 not installed)");
        return;
    }
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/orm_composite");
    let dir = Dir(std::env::temp_dir().join(format!("tinox-composite-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(dir.0.join("src")).unwrap();
    std::fs::create_dir_all(dir.0.join("migrations")).unwrap();
    for f in ["Membership.tnx", "Main.tnx"] {
        std::fs::copy(fixture.join(f), dir.0.join("src").join(f)).unwrap();
    }
    let db = dir.0.join("data.db");
    std::fs::write(
        dir.0.join("tinox.toml"),
        format!(
            "[package]\ncompiler = \"2.8.0\"\nname = \"composite\"\nversion = \"0.1.0\"\n\n[database]\ndriver = \"sqlite\"\nurl = \"{}\"\nmigrate_on_startup = true\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"db\"\nversion = \"1.1.0\"\n",
            db.display()
        ),
    )
    .unwrap();
    let run = |args: &[&str]| Command::new(tinox).args(args).current_dir(&dir.0).output().unwrap();

    let install = run(&["install"]);
    assert!(install.status.success(), "{}", text(&install));
    // Schema from the entity model: a two-column key in declaration order.
    let generate = run(&["db", "generate"]);
    assert!(generate.status.success(), "{}", text(&generate));
    let log = std::fs::read_to_string(dir.0.join("migrations/changelog.yaml")).unwrap();
    assert!(log.contains("primaryKey:") && log.contains("- org") && log.contains("- member"), "{log}");
    assert!(log.find("- org").unwrap() < log.find("- member").unwrap(), "key order is declaration order:\n{log}");

    let build = run(&["build", "src/Main.tnx", "-o", "app"]);
    assert!(build.status.success(), "{}", text(&build));
    let out = Command::new(dir.0.join("app")).current_dir(&dir.0).output().unwrap();
    let stdout = text(&out);
    assert!(out.status.success(), "{stdout}");
    assert!(stdout.contains("count: 3") && stdout.contains("acme/2: lead") && stdout.contains("after delete: 2"), "{stdout}");

    // Independent checks: the real constraint, and exactly the unrelated rows left.
    let ddl = sqlite(&db, "SELECT sql FROM sqlite_master WHERE name = 'memberships'");
    assert!(ddl.contains("PRIMARY KEY (org, member)") || ddl.contains("PRIMARY KEY (\"org\", \"member\")"), "{ddl}");
    let rows = sqlite(&db, "SELECT group_concat(org || '/' || member || '=' || role, ',') FROM (SELECT * FROM memberships ORDER BY org, member)");
    assert_eq!(rows, "acme/1=owner,globex/1=dev");
    let dup = Command::new("sqlite3").arg(&db).arg("INSERT INTO memberships (org, member, role) VALUES ('acme', 1, 'x')").output().unwrap();
    assert!(!dup.status.success(), "the database must reject a duplicate key");
}
