//! @Transactional for SQLite and MySQL (issue #531), checked from outside with independent
//! clients (`sqlite3`, the `mysql` CLI inside the container) reading the final tables.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

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
    fn new(tag: &str, fixture: &str, driver: &str, url: &str, pool: u32, main: Option<&str>) -> Project {
        let dir = std::env::temp_dir().join(format!("tinox-txdrv-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let from = repo_root().join("tests/fixtures").join(fixture);
        for f in ["Note.tnx", "Flows.tnx", "Main.tnx"] {
            std::fs::copy(from.join(f), dir.join("src").join(f)).unwrap();
        }
        if let Some(m) = main {
            std::fs::write(dir.join("src/Main.tnx"), m).unwrap();
        }
        std::fs::write(
            dir.join("tinox.toml"),
            format!(
                "[package]\ncompiler = \"2.8.0\"\nname = \"txdrv\"\nversion = \"0.1.0\"\n\n[database]\ndriver = \"{driver}\"\nurl = \"{url}\"\npool = {pool}\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"db\"\nversion = \"1.1.0\"\n"
            ),
        )
        .unwrap();
        let install = Command::new(env!("CARGO_BIN_EXE_tinox")).arg("install").current_dir(&dir).output().unwrap();
        assert!(install.status.success(), "install failed: {}", text(&install));
        Project { dir }
    }

    fn build(&self) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tinox"))
            .args(["build", "src/Main.tnx", "-o", "app"])
            .current_dir(&self.dir)
            .output()
            .unwrap()
    }

    fn run(&self, envs: &[(&str, &str)]) -> Output {
        Command::new(self.dir.join("app")).current_dir(&self.dir).envs(envs.iter().copied()).output().unwrap()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn sqlite_db(dir: &Path) -> PathBuf {
    let db = dir.join("data.db");
    let out = Command::new("sqlite3")
        .arg(&db)
        .arg("CREATE TABLE notes (id INTEGER PRIMARY KEY AUTOINCREMENT, tag TEXT NOT NULL);")
        .output()
        .expect("sqlite3");
    assert!(out.status.success(), "{}", text(&out));
    db
}

fn sqlite_tags(db: &Path) -> String {
    let out = Command::new("sqlite3").arg(db).arg("SELECT group_concat(tag, ',') FROM (SELECT tag FROM notes ORDER BY tag);").output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn sqlite_transactions_commit_roll_back_and_serialize() {
    if !tool_available("sqlite3", &["--version"]) {
        eprintln!("SKIP sqlite transactions (sqlite3 not installed)");
        return;
    }
    let tmp = std::env::temp_dir().join(format!("tinox-txdrv-sqlite-db-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let db = sqlite_db(&tmp);
    let p = Project::new("sqlite", "tx_sqlite", "sqlite", db.to_str().unwrap(), 1, None);
    let build = p.build();
    assert!(build.status.success(), "build failed: {}", text(&build));
    let run = p.run(&[]);
    let out = text(&run);
    assert!(run.status.success(), "program failed:\n{out}");
    assert!(out.contains("caught: nested failed"), "{out}");
    assert!(out.contains("returned: 7"), "{out}");
    assert!(out.contains("refused: transaction rolled back"), "{out}");
    assert_eq!(out.matches("failed ").count(), 4 + 1, "{out}"); // four odd tasks + "caught: nested failed" line
    assert_eq!(
        sqlite_tags(&db),
        "c-0,c-2,c-4,c-6,n-inner-ok,n-inner-returned,n-outer-1,n-outer-2,s-nested-standalone",
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn sqlite_waiting_for_a_transaction_is_bounded_and_visible() {
    if !tool_available("sqlite3", &["--version"]) {
        eprintln!("SKIP sqlite lock timeout (sqlite3 not installed)");
        return;
    }
    let tmp = std::env::temp_dir().join(format!("tinox-txdrv-sqlite-to-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let db = sqlite_db(&tmp);
    let p = Project::new("sqlite-timeout", "tx_sqlite_timeout", "sqlite", db.to_str().unwrap(), 1, None);
    let build = p.build();
    assert!(build.status.success(), "build failed: {}", text(&build));
    let started = Instant::now();
    let run = p.run(&[("TINOX_DB_LOCK_TIMEOUT_MS", "500")]);
    let out = text(&run);
    assert!(!run.status.success(), "must fail visibly:\n{out}");
    assert!(out.contains("waited 500 ms"), "{out}");
    assert!(started.elapsed() < Duration::from_secs(3), "wait was not bounded: {:?}", started.elapsed());
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn sqlite_rejects_requires_new_at_compile_time() {
    if !tool_available("sqlite3", &["--version"]) {
        return;
    }
    let tmp = std::env::temp_dir().join(format!("tinox-txdrv-sqlite-rn-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let p = Project::new("sqlite-rn", "tx_sqlite", "sqlite", tmp.join("x.db").to_str().unwrap(), 1, None);
    let flows = std::fs::read_to_string(p.dir.join("src/Flows.tnx")).unwrap().replace("@Transactional(\"NESTED\")\n    fn nested(", "@Transactional(\"REQUIRES_NEW\")\n    fn nested(");
    std::fs::write(p.dir.join("src/Flows.tnx"), flows).unwrap();
    let build = p.build();
    assert!(!build.status.success());
    assert!(text(&build).contains("SQLite cannot provide"), "{}", text(&build));
    let _ = std::fs::remove_dir_all(&tmp);
}

struct MysqlContainer {
    name: String,
    port: u16,
}

impl MysqlContainer {
    fn start() -> Self {
        let name = format!("tinox-mysql-txdrv-test-{}", std::process::id());
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
fn mysql_transactions_pool_savepoints_and_independent_transactions() {
    if !tool_available("docker", &["--version"]) {
        eprintln!("SKIP mysql transactions (docker not installed)");
        return;
    }
    let my = MysqlContainer::start();
    my.exec("CREATE TABLE notes (id INT AUTO_INCREMENT PRIMARY KEY, tag VARCHAR(100) NOT NULL) ENGINE=InnoDB").expect("create table");
    let p = Project::new("mysql", "postgres_propagation", "mysql", &my.url(), 3, None);
    let build = p.build();
    assert!(build.status.success(), "build failed: {}", text(&build));
    let run = p.run(&[]);
    let out = text(&run);
    assert!(run.status.success(), "program failed:\n{out}");
    assert!(out.contains("caught: nested failed"), "{out}");
    assert!(out.contains("returned: 7"), "{out}");
    assert!(out.contains("refused: transaction rolled back"), "{out}");
    assert!(out.contains("outer: outer failed"), "{out}");
    assert!(out.contains("independent sees outer: 0"), "{out}");
    let tags = my.exec("SELECT GROUP_CONCAT(tag ORDER BY tag) FROM notes").unwrap();
    assert_eq!(
        tags,
        "i-inner-kept,j-outer-1,j-outer-2,m-outer,n-inner-ok,n-inner-returned,n-outer-1,n-outer-2,s-nested-standalone",
        "{out}"
    );
}
