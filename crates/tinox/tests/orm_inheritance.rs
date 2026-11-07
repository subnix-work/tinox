//! Entities inheriting columns from a non-entity base class (issue #563): the generated schema and the ORM
//! agree on column order and types, checked with independent `sqlite3` reads; unsupported hierarchy shapes are
//! explicit compile errors that change nothing.

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

fn project(tag: &str, files: &[(&str, &str)]) -> (Dir, PathBuf) {
    let dir = Dir(std::env::temp_dir().join(format!("tinox-orminh-{tag}-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(dir.0.join("src")).unwrap();
    std::fs::create_dir_all(dir.0.join("migrations")).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/orm_inheritance");
    for f in ["Auditable.tnx", "Invoice.tnx", "Main.tnx"] {
        std::fs::copy(fixture.join(f), dir.0.join("src").join(f)).unwrap();
    }
    for (name, body) in files {
        std::fs::write(dir.0.join("src").join(name), body).unwrap();
    }
    let db = dir.0.join("data.db");
    std::fs::write(
        dir.0.join("tinox.toml"),
        format!(
            "[package]\ncompiler = \"2.9.4\"\nname = \"orminh\"\nversion = \"0.1.0\"\n\n[database]\ndriver = \"sqlite\"\nurl = \"{}\"\nmigrate_on_startup = true\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"db\"\nversion = \"1.1.0\"\n",
            db.display()
        ),
    )
    .unwrap();
    let install = Command::new(env!("CARGO_BIN_EXE_tinox")).arg("install").current_dir(&dir.0).output().unwrap();
    assert!(install.status.success(), "{}", text(&install));
    (dir, db)
}

fn tinox(dir: &Dir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tinox")).args(args).current_dir(&dir.0).output().unwrap()
}

#[test]
fn inherited_columns_are_generated_and_round_trip_through_the_orm() {
    if Command::new("sqlite3").arg("--version").output().is_err() {
        eprintln!("SKIP entity inheritance (sqlite3 not installed)");
        return;
    }
    let (dir, db) = project("ok", &[]);
    let generate = tinox(&dir, &["db", "generate"]);
    assert!(generate.status.success(), "{}", text(&generate));
    let log = std::fs::read_to_string(dir.0.join("migrations/changelog.yaml")).unwrap();
    // Inherited and own columns are all in the table (the generator lists columns alphabetically).
    for name in ["name: id", "name: created_by", "name: amount", "name: memo"] {
        assert!(log.contains(name), "{name} missing:\n{log}");
    }

    let build = tinox(&dir, &["build", "src/Main.tnx", "-o", "app"]);
    assert!(build.status.success(), "{}", text(&build));
    let out = Command::new(dir.0.join("app")).current_dir(&dir.0).output().unwrap();
    let stdout = text(&out);
    assert!(out.status.success(), "{stdout}");
    assert!(stdout.contains("count: 2"), "{stdout}");
    assert!(stdout.contains("invoice 1 by ann amount 120 memo first"), "{stdout}");
    assert!(stdout.contains("invoice 2 by bob amount 80 memo second"), "{stdout}");
    assert!(stdout.contains("updated: 95 second"), "{stdout}");

    // Independent client: the schema has the inherited columns, and the rows hold what the ORM wrote.
    let cols = sqlite(&db, "SELECT group_concat(name, ',') FROM pragma_table_info('invoices')");
    let mut sorted: Vec<&str> = cols.split(',').collect();
    sorted.sort();
    assert_eq!(sorted, ["amount", "created_by", "id", "memo"]);
    assert_eq!(sqlite(&db, "SELECT group_concat(id || ':' || created_by || ':' || amount || ':' || memo, '|') FROM (SELECT * FROM invoices ORDER BY id)"), "1:ann:120:first|2:bob:95:second");
    let null = Command::new("sqlite3").arg(&db).arg("INSERT INTO invoices (amount, memo) VALUES (1, 'x')").output().unwrap();
    assert!(!null.status.success(), "the inherited @NotNull must be a real NOT NULL constraint");
    // Generating again is a no-op: the hierarchy maps to a stable schema.
    assert!(text(&tinox(&dir, &["db", "generate"])).contains("up to date"));
}

fn rejected(tag: &str, files: &[(&str, &str)], expect: &str, build_names_it: bool) {
    let (dir, _) = project(tag, files);
    let generate = tinox(&dir, &["db", "generate"]);
    assert!(!generate.status.success(), "{tag}: schema generation must reject it:\n{}", text(&generate));
    assert!(text(&generate).contains(expect), "{tag}: expected {expect:?} in:\n{}", text(&generate));
    // The compiler refuses the program too (with this message, or an earlier type error for generics).
    let build = tinox(&dir, &["build", "src/Main.tnx", "-o", "app"]);
    assert!(!build.status.success(), "{tag}: build must be rejected:\n{}", text(&build));
    if build_names_it {
        assert!(text(&build).contains(expect), "{tag}: the build must name the problem too:\n{}", text(&build));
    }
    assert!(!dir.0.join("migrations/changelog.yaml").exists() || std::fs::read_to_string(dir.0.join("migrations/changelog.yaml")).unwrap().trim().is_empty(), "{tag}: a rejected model must not write a changelog");
}

#[test]
fn unsupported_hierarchies_are_explicit_errors() {
    if Command::new("sqlite3").arg("--version").output().is_err() {
        return;
    }
    // An entity extending an entity (single-table / joined inheritance).
    rejected(
        "entity-base",
        &[("Auditable.tnx", "@Entity\n@Table(\"audits\")\nclass Auditable\n{\n    @Id\n    @GeneratedValue\n    var id: Int64\n    @Column(\"created_by\")\n    var createdBy: String\n}\n")],
        "inheritance between entities",
        true,
    );
    // A subclass redeclaring an inherited field.
    rejected(
        "redeclared",
        &[("Invoice.tnx", "import Auditable\n\n@Entity\n@Table(\"invoices\")\nclass Invoice extends Auditable\n{\n    var createdBy: String\n    var amount: Int64\n    var memo: String\n}\n")],
        "redeclaring an inherited field",
        true,
    );
    // A generic entity.
    rejected(
        "generic",
        &[("Invoice.tnx", "import Auditable\n\n@Entity\n@Table(\"invoices\")\nclass Invoice<T> extends Auditable\n{\n    var amount: Int64\n    var memo: String\n}\n")],
        "generic entities are unsupported",
        false,
    );
    // A generic base class.
    rejected(
        "generic-base",
        &[("Auditable.tnx", "class Auditable<T>\n{\n    @Id\n    @GeneratedValue\n    var id: Int64\n    @Column(\"created_by\")\n    var createdBy: String\n}\n")],
        "is generic",
        true,
    );
}
