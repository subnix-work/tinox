//! Exercise the public offline CLI and execute its SQL with an independent SQLite client.
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn tinox(root: &Path, sub: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tinox"))
        .current_dir(root)
        .args(["db", sub])
        .output()
        .unwrap()
}
fn success(out: Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn generate_cli_and_apply_sqlite_schema() {
    let root = std::env::temp_dir().join(format!("tinox-db-generate-cli-{}", std::process::id()));
    fs::create_dir_all(root.join("src/models")).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(root.clone());
    fs::write(
        root.join("tinox.toml"),
        r#"
[package]
name = "entity-test"
version = "0.1.0"
[database]
driver = "sqlite"
url = "test.db"
changelog = "schema/changes.yaml"
"#,
    )
    .unwrap();
    let entity = root.join("src/models/Person.tnx");
    let model = r#"
@Entity
@Table("people")
class Person {
    @Id @GeneratedValue
    id: Int64
    @Column("full_name") @NotNull
    name: String
    state: State
}
"#;
    fs::write(&entity, model).unwrap();
    fs::write(
        root.join("src/models/State.tnx"),
        "enum State { ACTIVE\nINACTIVE }",
    )
    .unwrap();
    assert!(success(tinox(&root, "generate")).contains("generated changeSet"));
    success(tinox(&root, "validate"));
    let changelog = root.join("schema/changes.yaml");
    let original = fs::read(&changelog).unwrap();
    assert!(success(tinox(&root, "generate")).contains("up to date"));
    assert_eq!(fs::read(&changelog).unwrap(), original);
    let initial_sql = success(tinox(&root, "update-sql"));
    fs::write(
        &entity,
        model.replace("state: State", "state: State\n    email: String"),
    )
    .unwrap();
    success(tinox(&root, "generate"));
    assert!(fs::read(&changelog).unwrap().starts_with(&original));
    success(tinox(&root, "validate"));
    let all_sql = success(tinox(&root, "update-sql"));
    assert!(all_sql.starts_with(&initial_sql));

    if Command::new("sqlite3").arg("--version").output().is_err() {
        eprintln!("SKIP SQLite execution: sqlite3 not installed (CLI assertions passed)");
        return;
    }
    let db = root.join("test.db");
    let script = root.join("generated.sql");
    fs::write(&script, &all_sql).unwrap();
    success(
        Command::new("sqlite3")
            .arg(&db)
            .stdin(fs::File::open(script).unwrap())
            .output()
            .unwrap(),
    );
    let rows = success(Command::new("sqlite3").arg(&db).arg(
        "INSERT INTO people(full_name,state,email) VALUES ('Ada','ACTIVE','ada@example.org'); SELECT id,full_name,state,email FROM people;"
    ).output().unwrap());
    assert_eq!(rows.trim(), "1|Ada|ACTIVE|ada@example.org");
    let cols = success(
        Command::new("sqlite3")
            .arg(&db)
            .arg("PRAGMA table_info(people)")
            .output()
            .unwrap(),
    );
    assert!(cols.lines().any(|l| l.contains("|state|TEXT|")), "{cols}");
    assert!(cols.lines().any(|l| l.contains("|email|TEXT|")), "{cols}");
}
