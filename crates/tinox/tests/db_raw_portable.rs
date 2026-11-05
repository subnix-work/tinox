//! `DB::raw(sql, params)` on SQLite and MySQL (issue #562), checked against independent
//! clients: the `sqlite3` CLI and the `mysql` CLI inside a real MySQL container read the tables
//! the compiled program wrote. The same fixture program runs on both drivers, so the contract
//! (parameters, column-name keys, NULL columns absent, empty results, write outcomes, long
//! values, visible failures) is the same one the PostgreSQL gate already pins.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

fn tool_available(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd).args(args).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

struct Project {
    dir: PathBuf,
}

impl Project {
    fn new(tag: &str, driver: &str, url: &str) -> Project {
        let dir = std::env::temp_dir().join(format!("tinox-dbraw-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::copy(repo_root().join("tests/fixtures/db_raw_portable/Main.tnx"), dir.join("src/Main.tnx")).unwrap();
        std::fs::write(
            dir.join("tinox.toml"),
            format!(
                "[package]\ncompiler = \"2.9.1\"\nname = \"dbraw\"\nversion = \"0.1.0\"\n\n[database]\ndriver = \"{driver}\"\nurl = \"{url}\"\npool = 2\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"db\"\nversion = \"1.2.5\"\n\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"db\"\npath = \"{}\"\n",
                repo_root().join("crates/tinox-core-ext/db").display()
            ),
        )
        .unwrap();
        let install = Command::new(env!("CARGO_BIN_EXE_tinox")).arg("install").current_dir(&dir).output().unwrap();
        assert!(install.status.success(), "install failed: {}", text(&install));
        let build = Command::new(env!("CARGO_BIN_EXE_tinox")).args(["build", "src/Main.tnx", "-o", "app"]).current_dir(&dir).output().unwrap();
        assert!(build.status.success(), "build failed: {}", text(&build));
        Project { dir }
    }

    fn run(&self, mode: &str) -> Output {
        Command::new(self.dir.join("app")).current_dir(&self.dir).env("MODE", mode).output().unwrap()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn assert_contract(out: &str) {
    let expected = [
        "rows: 2",
        "row id=1 name=widget hasNote=true",
        "row id=2 name=gadget hasNote=false", // NULL note: the key is absent
        "none: 0",
        "write: 0",
        "after: 1 name=x'); DROP TABLE items; --",
        "swapped: 1",
        "long: 6000",
    ];
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines, expected, "unexpected program output:\n{out}");
}

#[test]
fn sqlite_raw_binds_parameters_and_reports_rows_nulls_and_writes() {
    if !tool_available("sqlite3", &["--version"]) {
        eprintln!("SKIP sqlite raw (sqlite3 not installed)");
        return;
    }
    let tmp = std::env::temp_dir().join(format!("tinox-dbraw-sqlite-db-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let db = tmp.join("data.db");
    let seed = Command::new("sqlite3")
        .arg(&db)
        .arg("CREATE TABLE items (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, price REAL NOT NULL, note TEXT); \
              INSERT INTO items (name, price, note) VALUES ('widget', 9.99, 'first'), ('gadget', 19.99, NULL), ('cheap', 1.00, 'excluded');")
        .output()
        .unwrap();
    assert!(seed.status.success(), "{}", text(&seed));

    let p = Project::new("sqlite", "sqlite", db.to_str().unwrap());
    let run = p.run("ok");
    assert!(run.status.success(), "program failed:\n{}", text(&run));
    assert_contract(&String::from_utf8_lossy(&run.stdout));

    // Independent check: the hostile value is stored as data and the table survived.
    let q = |sql: &str| {
        let o = Command::new("sqlite3").arg(&db).arg(sql).output().unwrap();
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    assert_eq!(q("SELECT count(*) FROM items"), "5");
    assert_eq!(q("SELECT name FROM items WHERE price = 50"), "x'); DROP TABLE items; --");
    assert_eq!(q("SELECT length(note) FROM items WHERE name = 'long'"), "6000");

    let caught = p.run("catch");
    let out = text(&caught);
    assert!(caught.status.success() && caught.stderr.is_empty(), "{out}");
    assert_eq!(out.matches("caught: ORM query failed for table 'DB::raw': ").count(), 8, "{out}");
    assert!(out.contains("no_such_table") && out.ends_with("healthy rows: 5\n"), "{out}");
    assert!(out.contains("constraint: ORM query failed") && out.contains("price"), "{out}");

    let fail = p.run("fail");
    assert!(!fail.status.success(), "an uncaught database error must terminate the program:\n{}", text(&fail));
    assert!(text(&fail).contains("no_such_table") || text(&fail).contains("no such table"), "{}", text(&fail));
    let _ = std::fs::remove_dir_all(&tmp);
}

struct MysqlContainer {
    name: String,
    port: u16,
}

impl MysqlContainer {
    fn start() -> Self {
        let name = format!("tinox-mysql-dbraw-test-{}", std::process::id());
        let _ = Command::new("docker").args(["rm", "-f", &name]).output();
        let status = Command::new("docker")
            .args(["run", "-d", "--rm", "--name", &name, "-e", "MYSQL_ROOT_PASSWORD=tinox", "-e", "MYSQL_DATABASE=tinox", "-P", "mysql:8.4"])
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "docker run mysql failed");
        let port_out = Command::new("docker").args(["port", &name, "3306/tcp"]).output().unwrap();
        let s = String::from_utf8_lossy(&port_out.stdout);
        let port: u16 = s.lines().next().and_then(|l| l.rsplit(':').next()).and_then(|p| p.trim().parse().ok()).unwrap_or_else(|| panic!("port: {s}"));
        let c = MysqlContainer { name, port };
        for _ in 0..180 {
            if c.exec("SELECT 1").is_some() {
                return c;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        panic!("mysql container never became ready");
    }

    fn exec(&self, sql: &str) -> Option<String> {
        let out = Command::new("docker")
            .args(["exec", &self.name, "mysql", "-h127.0.0.1", "-uroot", "-ptinox", "-N", "-B", "-D", "tinox", "-e", sql])
            .output()
            .ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn url(&self) -> String {
        format!("mysql://root:tinox@127.0.0.1:{}/tinox", self.port)
    }
}

impl Drop for MysqlContainer {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "-f", &self.name]).output();
    }
}

#[test]
fn mysql_raw_binds_parameters_and_reports_rows_nulls_and_writes() {
    if !tool_available("docker", &["--version"]) {
        eprintln!("SKIP mysql raw (docker not installed)");
        return;
    }
    let my = MysqlContainer::start();
    my.exec("CREATE TABLE items (id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(255) NOT NULL, price DOUBLE NOT NULL, note LONGTEXT)").expect("create");
    my.exec("INSERT INTO items (name, price, note) VALUES ('widget', 9.99, 'first'), ('gadget', 19.99, NULL), ('cheap', 1.00, 'excluded')").expect("seed");

    let p = Project::new("mysql", "mysql", &my.url());
    let run = p.run("ok");
    assert!(run.status.success(), "program failed:\n{}", text(&run));
    assert_contract(&String::from_utf8_lossy(&run.stdout));

    assert_eq!(my.exec("SELECT count(*) FROM items").unwrap(), "5");
    assert_eq!(my.exec("SELECT name FROM items WHERE price = 50").unwrap(), "x'); DROP TABLE items; --");
    assert_eq!(my.exec("SELECT CHAR_LENGTH(note) FROM items WHERE name = 'long'").unwrap(), "6000");

    let caught = p.run("catch");
    let out = text(&caught);
    assert!(caught.status.success() && caught.stderr.is_empty(), "{out}");
    assert_eq!(out.matches("caught: ORM query failed for table 'DB::raw': ").count(), 8, "{out}");
    assert!(out.contains("no_such_table") && out.ends_with("healthy rows: 5\n"), "{out}");
    assert!(out.contains("constraint: ORM query failed") && out.contains("price"), "{out}");

    let fail = p.run("fail");
    assert!(!fail.status.success(), "an uncaught database error must terminate the program:\n{}", text(&fail));
    assert!(text(&fail).contains("no_such_table"), "{}", text(&fail));
}
