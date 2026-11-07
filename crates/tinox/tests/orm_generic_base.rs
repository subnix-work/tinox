//! Entities extending a concrete specialization of a generic mapped superclass (`extends Meta<String>`, issue #563):
//! the schema and the ORM see one concrete field list per specialization, checked with independent `sqlite3`
//! reads; unsupported shapes are explicit errors that change nothing.

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
    let dir = Dir(std::env::temp_dir().join(format!("tinox-ormgen-{tag}-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(dir.0.join("src")).unwrap();
    std::fs::create_dir_all(dir.0.join("migrations")).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/orm_generic_base");
    for f in ["Meta.tnx", "Invoice.tnx", "Counter.tnx", "Main.tnx"] {
        std::fs::copy(fixture.join(f), dir.0.join("src").join(f)).unwrap();
    }
    for (name, body) in files {
        std::fs::write(dir.0.join("src").join(name), body).unwrap();
    }
    let db = dir.0.join("data.db");
    std::fs::write(
        dir.0.join("tinox.toml"),
        format!(
            "[package]\ncompiler = \"2.9.3\"\nname = \"ormgen\"\nversion = \"0.1.0\"\n\n[database]\ndriver = \"sqlite\"\nurl = \"{}\"\nmigrate_on_startup = true\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"db\"\nversion = \"1.1.0\"\n",
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

fn have_sqlite() -> bool {
    Command::new("sqlite3").arg("--version").output().is_ok()
}

#[test]
fn specializations_map_to_concrete_columns_and_round_trip() {
    if !have_sqlite() {
        eprintln!("SKIP generic mapped superclass (sqlite3 not installed)");
        return;
    }
    let (dir, db) = project("ok", &[]);
    let generate = tinox(&dir, &["db", "generate"]);
    assert!(generate.status.success(), "{}", text(&generate));

    let build = tinox(&dir, &["build", "src/Main.tnx", "-o", "app"]);
    assert!(build.status.success(), "{}", text(&build));
    let out = Command::new(dir.0.join("app")).current_dir(&dir.0).output().unwrap();
    let stdout = text(&out);
    assert!(out.status.success(), "{stdout}");
    assert!(stdout.contains("invoice paid 120"), "{stdout}");
    assert!(stdout.contains("counter 8 3"), "{stdout}");

    // Independent client: each table has the column type of its own specialization.
    assert_eq!(sqlite(&db, "SELECT type FROM pragma_table_info('invoices') WHERE name = 'tag'"), "TEXT");
    assert_eq!(sqlite(&db, "SELECT type FROM pragma_table_info('counters') WHERE name = 'tag'"), "INTEGER");
    assert_eq!(sqlite(&db, "SELECT tag || ':' || amount FROM invoices"), "paid:120");
    assert_eq!(sqlite(&db, "SELECT (tag + 1) || ':' || hits FROM counters"), "8:3");
    let null = Command::new("sqlite3").arg(&db).arg("INSERT INTO invoices (amount) VALUES (1)").output().unwrap();
    assert!(!null.status.success(), "@NotNull on the generic base field must be a real constraint");
    // The hierarchy maps to a stable schema: a second generate has nothing to do.
    assert!(text(&tinox(&dir, &["db", "generate"])).contains("up to date"));

    // Re-specializing the base (String -> Int64) changes the type of an existing column: the generator reports it
    // for a manual changeSet and neither renames nor drops anything.
    let before = std::fs::read_to_string(dir.0.join("migrations/changelog.yaml")).unwrap();
    let invoice = std::fs::read_to_string(dir.0.join("src/Invoice.tnx")).unwrap();
    std::fs::write(dir.0.join("src/Invoice.tnx"), invoice.replace("Meta<String>", "Meta<Int64>")).unwrap();
    let changed = tinox(&dir, &["db", "generate"]);
    assert!(text(&changed).contains("manual changeSet"), "{}", text(&changed));
    assert_eq!(std::fs::read_to_string(dir.0.join("migrations/changelog.yaml")).unwrap(), before, "the changelog must be untouched");
    assert_eq!(sqlite(&db, "SELECT group_concat(name, ',') FROM (SELECT name FROM pragma_table_info('invoices') ORDER BY name)"), "amount,id,tag");
}

fn rejected(tag: &str, files: &[(&str, &str)], expect: &str) {
    let (dir, _) = project(tag, files);
    let generate = tinox(&dir, &["db", "generate"]);
    assert!(!generate.status.success(), "{tag}: schema generation must reject it:\n{}", text(&generate));
    assert!(text(&generate).contains(expect), "{tag}: expected {expect:?} in:\n{}", text(&generate));
    let build = tinox(&dir, &["build", "src/Main.tnx", "-o", "app"]);
    assert!(!build.status.success(), "{tag}: build must be rejected:\n{}", text(&build));
    assert!(text(&build).contains(expect), "{tag}: the build must name the problem too:\n{}", text(&build));
    let log = dir.0.join("migrations/changelog.yaml");
    assert!(!log.exists() || std::fs::read_to_string(log).unwrap().trim().is_empty(), "{tag}: a rejected model must not write a changelog");
}

#[test]
fn unsupported_specializations_are_explicit_errors() {
    if !have_sqlite() {
        return;
    }
    let invoice = |header: &str| format!("import Meta\n\n@Entity\n@Table(\"invoices\")\n{header}\n{{\n    @Column(\"amount\")\n    var amount: Int64\n}}\n");
    rejected("arity", &[("Invoice.tnx", &invoice("class Invoice extends Meta<String, Int64>"))], "1 type parameter(s) but 2 type argument(s)");
    rejected("compound", &[("Invoice.tnx", &invoice("class Invoice extends Meta<List<String>>"))], "must be a scalar or named type");
    rejected(
        "not-generic",
        &[
            ("Meta.tnx", "class Meta\n{\n    @Id\n    @GeneratedValue\n    var id: Int64\n}\n"),
            ("Invoice.tnx", &invoice("class Invoice extends Meta<String>")),
        ],
        "is not a generic class",
    );
    rejected(
        "methods",
        &[(
            "Meta.tnx",
            "class Meta<T>\n{\n    @Id\n    @GeneratedValue\n    var id: Int64\n    var tag: T\n    fnc describe() -> String\n    {\n        return \"x\"\n    }\n}\n",
        )],
        "declares methods",
    );
    rejected(
        "entity-base",
        &[("Meta.tnx", "@Entity\n@Table(\"metas\")\nclass Meta<T>\n{\n    @Id\n    @GeneratedValue\n    var id: Int64\n    var tag: T\n}\n")],
        "inheritance between entities",
    );
    // Type arguments on a class that is not an entity.
    rejected(
        "plain-class",
        &[("Invoice.tnx", "import Meta\n\nclass Invoice extends Meta<String>\n{\n    var amount: Int64\n}\n"), ("Counter.tnx", "class Counter\n{\n    var hits: Int64\n}\n")],
        "only supported for entities",
    );
}
