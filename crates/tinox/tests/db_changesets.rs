//! Declarative schema changesets (issue #410): `tinox db update/status/validate/update-sql`
//! and `migrate_on_startup`, checked from outside with an independent client (`sqlite3` /
//! `psql` / `mysql` in the container) against the real databases, not against the engine's own
//! report. PostgreSQL and MySQL run in docker; those parts SKIP gracefully when docker is missing.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

fn tool_available(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

struct PgContainer {
    name: String,
    port: u16,
}

impl PgContainer {
    fn start() -> Self {
        let name = format!("tinox-pg-changesets-test-{}", std::process::id());
        let _ = Command::new("docker").args(["rm", "-f", &name]).output();
        let status = Command::new("docker")
            .args([
                "run", "-d", "--rm",
                "--name", &name,
                "-e", "POSTGRES_HOST_AUTH_METHOD=trust",
                "-P", // publish exposed ports to random host ports -- no
                      // hardcoded literal, same "no manually-picked port"
                      // convention CLAUDE.md's e2e fixtures already use.
                "postgres:16-alpine",
            ])
            .stdout(Stdio::null())
            .status()
            .expect("spawn docker run");
        assert!(status.success(), "docker run postgres failed");

        let port_out = Command::new("docker")
            .args(["port", &name, "5432/tcp"])
            .output()
            .expect("docker port");
        assert!(port_out.status.success(), "docker port failed");
        let port_str = String::from_utf8_lossy(&port_out.stdout);
        // "0.0.0.0:54321\n[::]:54321\n" -- take the first line's port.
        let port: u16 = port_str
            .lines()
            .next()
            .and_then(|l| l.rsplit(':').next())
            .and_then(|p| p.trim().parse().ok())
            .unwrap_or_else(|| panic!("could not parse docker port output: {port_str}"));

        let container = PgContainer { name, port };
        container.wait_ready();
        container
    }

    fn wait_ready(&self) {
        // Probes the way this test itself will actually connect -- a real
        // query, from the HOST, over the MAPPED port.
        //
        // `docker exec pg_isready` (what this used to do) is not enough,
        // and that gap made this test genuinely flaky in CI: it failed
        // once with "psql: server closed the connection unexpectedly ...
        // server terminated abnormally before or while processing the
        // request" while the very same commit passed on a rerun. The
        // official postgres image runs a TEMPORARY server during
        // initdb/init-script processing, deliberately started with
        // `listen_addresses=''` so it's reachable only over the
        // container's own unix socket -- which is exactly what an
        // in-container pg_isready talks to. So pg_isready reports "ready"
        // during a phase where the published TCP port isn't serving the
        // final server at all; the entrypoint then shuts that temporary
        // server down and starts the real one. A host-side connect
        // landing in that window gets accepted and then dropped, which is
        // precisely the observed error.
        //
        // Checking for an answered query (not just an accepted TCP
        // connection) is what closes the window completely: it cannot
        // succeed against the temporary server (not listening on TCP),
        // and it cannot succeed during the restart gap either.
        //
        // Mechanism verified directly against a real PostgreSQL 18.6
        // rather than inferred from the image's entrypoint script: with a
        // server started `-c listen_addresses=''` (exactly what the
        // temporary init server uses), `pg_isready` over the unix socket
        // reports "accepting connections" and exits 0, while a host-side
        // TCP connect at the same moment gets "Connection refused". The
        // old check returned on precisely that signal.
        for _ in 0..120 {
            if self.try_query("SELECT 1").is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        panic!(
            "postgres container never became ready on 127.0.0.1:{}",
            self.port
        );
    }

    /// Non-panicking counterpart to `psql` below: returns `None` while the
    /// server isn't answering (yet), rather than failing the test. Only
    /// used for readiness polling -- every real assertion goes through
    /// `psql`, which still treats a failure as fatal.
    fn try_query(&self, sql: &str) -> Option<String> {
        let out = Command::new("psql")
            .args([
                "-h", "127.0.0.1",
                "-p", &self.port.to_string(),
                "-U", "postgres",
                "-d", "postgres",
                "-t", "-A",
                "-c", sql,
            ])
            .env("PGPASSWORD", "postgres")
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn psql(&self, sql: &str) -> String {
        let out = Command::new("psql")
            .args([
                "-h", "127.0.0.1",
                "-p", &self.port.to_string(),
                "-U", "postgres",
                "-d", "postgres",
                "-t", "-A", // tuples-only, unaligned -- easy to parse
                "-c", sql,
            ])
            .env("PGPASSWORD", "postgres")
            .output()
            .expect("spawn psql");
        assert!(
            out.status.success(),
            "psql failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn url(&self) -> String {
        format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", self.port)
    }
}

impl Drop for PgContainer {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "-f", &self.name]).output();
    }
}

const CHANGELOG: &str = r#"databaseChangeLog:
  - changeSet:
      id: 1
      author: tino
      changes:
        - createTable:
            tableName: person
            columns:
              - column: { name: id, type: bigint, autoIncrement: true, constraints: { primaryKey: true } }
              - column: { name: name, type: varchar(100), constraints: { nullable: false } }
  - include: { file: more.yaml }
"#;

const MORE: &str = r#"databaseChangeLog:
  - changeSet:
      id: 2
      author: tino
      changes:
        - addColumn:
            tableName: person
            columns:
              - column: { name: status, type: varchar(20), defaultValue: ACTIVE }
        - createIndex: { tableName: person, indexName: idx_person_name, columns: [name] }
        - insert: { tableName: person, values: { name: seeded } }
"#;

const FAILING: &str = r#"
  - changeSet:
      id: 3
      author: tino
      changes:
        - insert: { tableName: person, values: { name: halfway } }
        - sql: { dbms: "sqlite, postgres", statements: "INSERT INTO no_such_table VALUES (1)" }
"#;

const MAIN: &str = "class Main\n{\n    fnc main() -> Int32\n    {\n        println(\"started\");\n        return 0;\n    }\n}\n";

struct Project {
    dir: PathBuf,
}

impl Project {
    fn new(tag: &str, driver: &str, url: &str, extra: &str) -> Project {
        let dir = std::env::temp_dir().join(format!("tinox-changesets-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("migrations")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("data")).unwrap();
        std::fs::write(
            dir.join("tinox.toml"),
            format!(
                "[package]\nname = \"cs\"\nversion = \"0.1.0\"\nentry = \"src/Main.tnx\"\n\n[database]\ndriver = \"{driver}\"\nurl = \"{url}\"\n{extra}"
            ),
        )
        .unwrap();
        std::fs::write(dir.join("migrations/changelog.yaml"), CHANGELOG).unwrap();
        std::fs::write(dir.join("migrations/more.yaml"), MORE).unwrap();
        std::fs::write(dir.join("src/Main.tnx"), MAIN).unwrap();
        Project { dir }
    }

    fn tinox(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tinox"))
            .args(args)
            .current_dir(&self.dir)
            .output()
            .expect("spawn tinox")
    }

    fn write(&self, rel: &str, text: &str) {
        std::fs::write(self.dir.join(rel), text).unwrap();
    }

    fn append(&self, rel: &str, text: &str) {
        let p = self.dir.join(rel);
        let mut s = std::fs::read_to_string(&p).unwrap();
        s.push_str(text);
        std::fs::write(p, s).unwrap();
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn sqlite(db: &Path, sql: &str) -> String {
    let out = Command::new("sqlite3").arg(db).arg(sql).output().expect("spawn sqlite3");
    assert!(out.status.success(), "sqlite3 failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn changesets_sqlite_apply_validate_and_roll_back() {
    if !tool_available("sqlite3", &["--version"]) {
        eprintln!("SKIP changesets_sqlite (sqlite3 not installed)");
        return;
    }
    let p = Project::new("sqlite", "sqlite", "data/test.db", "");
    let db = p.dir.join("data/test.db");

    let v = p.tinox(&["db", "validate"]);
    assert!(v.status.success(), "{}", text(&v));
    let sql = p.tinox(&["db", "update-sql"]);
    assert!(text(&sql).contains("CREATE TABLE \"person\""), "{}", text(&sql));

    let st = p.tinox(&["db", "status"]);
    assert!(text(&st).contains("2 pending"), "{}", text(&st));

    let up = p.tinox(&["db", "update"]);
    assert!(up.status.success(), "{}", text(&up));
    assert_eq!(sqlite(&db, "SELECT name || ':' || status FROM person"), "seeded:ACTIVE");
    assert_eq!(sqlite(&db, "SELECT COUNT(*) FROM tinox_changelog"), "2");
    assert_eq!(sqlite(&db, "SELECT COUNT(*) FROM sqlite_master WHERE name = 'idx_person_name'"), "1");

    // Second run is a no-op.
    let again = p.tinox(&["db", "update"]);
    assert!(again.status.success() && text(&again).contains("up to date"), "{}", text(&again));
    assert_eq!(sqlite(&db, "SELECT COUNT(*) FROM person"), "1");

    // An applied changeSet that was edited is a hard error and nothing else runs.
    p.write("migrations/changelog.yaml", &CHANGELOG.replace("varchar(100)", "varchar(120)"));
    p.append("migrations/more.yaml", FAILING);
    let bad = p.tinox(&["db", "update"]);
    assert!(!bad.status.success(), "{}", text(&bad));
    assert!(text(&bad).contains("was modified after it was applied"), "{}", text(&bad));
    assert_eq!(sqlite(&db, "SELECT COUNT(*) FROM tinox_changelog"), "2");
    p.write("migrations/changelog.yaml", CHANGELOG);

    // A failing changeSet is rolled back as a whole (its first statement must not survive).
    let fail = p.tinox(&["db", "update"]);
    assert!(!fail.status.success(), "{}", text(&fail));
    assert!(text(&fail).contains("no_such_table"), "{}", text(&fail));
    assert_eq!(sqlite(&db, "SELECT COUNT(*) FROM person WHERE name = 'halfway'"), "0");
    assert_eq!(sqlite(&db, "SELECT COUNT(*) FROM tinox_changelog"), "2");
    assert_eq!(sqlite(&db, "SELECT COUNT(*) FROM tinox_changelog_lock"), "0", "lock must be released after a failure");
}

#[test]
fn changesets_baseline_adopts_an_existing_schema_without_running_it() {
    if !tool_available("sqlite3", &["--version"]) {
        eprintln!("SKIP changesets_baseline (sqlite3 not installed)");
        return;
    }
    let p = Project::new("baseline", "sqlite", "data/test.db", "");
    let db = p.dir.join("data/test.db");
    // A database that already has the schema (created some other way) and one row of real data.
    sqlite(&db, "CREATE TABLE person (id INTEGER PRIMARY KEY AUTOINCREMENT, name VARCHAR(100) NOT NULL, status VARCHAR(20) DEFAULT 'ACTIVE'); INSERT INTO person (name) VALUES ('mine');");

    let plain = p.tinox(&["db", "update"]);
    assert!(!plain.status.success(), "creating an existing table must fail:\n{}", text(&plain));

    let base = p.tinox(&["db", "baseline"]);
    assert!(base.status.success(), "{}", text(&base));
    assert_eq!(sqlite(&db, "SELECT COUNT(*) FROM tinox_changelog"), "2");
    assert_eq!(sqlite(&db, "SELECT name FROM person"), "mine", "baseline must not execute the changeSets (no seed row)");

    let st = p.tinox(&["db", "status"]);
    assert!(text(&st).contains("0 pending"), "{}", text(&st));
}

#[test]
fn changesets_validate_rejects_bad_changelogs() {
    let p = Project::new("invalid", "sqlite", "data/test.db", "");
    for (name, yaml, needle) in [
        ("unknown change", "databaseChangeLog:\n  - changeSet:\n      id: 1\n      author: a\n      changes:\n        - frobnicate: {}\n", "unsupported change type"),
        ("duplicate id", "databaseChangeLog:\n  - changeSet: {id: 1, author: a, changes: [{dropTable: {tableName: t}}]}\n  - changeSet: {id: 1, author: a, changes: [{dropTable: {tableName: t}}]}\n", "duplicate changeSet"),
        ("unknown type", "databaseChangeLog:\n  - changeSet: {id: 1, author: a, changes: [{createTable: {tableName: t, columns: [{column: {name: c, type: nonsense}}]}}]}\n", "unsupported type"),
        ("bad identifier", "databaseChangeLog:\n  - changeSet: {id: 1, author: a, changes: [{dropTable: {tableName: \"t; DROP\"}}]}\n", "invalid identifier"),
    ] {
        p.write("migrations/changelog.yaml", yaml);
        let o = p.tinox(&["db", "validate"]);
        assert!(!o.status.success(), "{name}: expected failure\n{}", text(&o));
        assert!(text(&o).contains(needle), "{name}: expected '{needle}' in\n{}", text(&o));
    }
}

#[test]
fn changesets_migrate_on_startup_runs_before_main() {
    if !tool_available("sqlite3", &["--version"]) {
        eprintln!("SKIP changesets_migrate_on_startup (sqlite3 not installed)");
        return;
    }
    let p = Project::new("startup", "sqlite", "data/test.db", "migrate_on_startup = true\n");
    let build = p.tinox(&["build"]);
    assert!(build.status.success(), "{}", text(&build));
    let exe = p.dir.join("cs");
    let run = Command::new(&exe).current_dir(&p.dir).output().expect("run program");
    assert!(run.status.success(), "{}", text(&run));
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "started", "stdout must stay clean");
    assert_eq!(sqlite(&p.dir.join("data/test.db"), "SELECT COUNT(*) FROM person"), "1");
}

#[test]
fn changesets_postgres_apply_and_concurrent_starters() {
    if !tool_available("docker", &["--version"]) || !tool_available("psql", &["--version"]) {
        eprintln!("SKIP changesets_postgres (docker or psql not installed)");
        return;
    }
    let pg = PgContainer::start();
    let p = Project::new("pg", "postgres", &pg.url(), "migrate_on_startup = true\n");

    let up = p.tinox(&["db", "update"]);
    assert!(up.status.success(), "{}", text(&up));
    assert_eq!(pg.psql("SELECT name || ':' || status FROM person"), "seeded:ACTIVE");
    assert_eq!(pg.psql("SELECT COUNT(*) FROM pg_indexes WHERE indexname = 'idx_person_name'"), "1");
    assert_eq!(pg.psql("SELECT data_type FROM information_schema.columns WHERE table_name = 'person' AND column_name = 'id'"), "bigint");

    p.append("migrations/more.yaml", FAILING);
    let fail = p.tinox(&["db", "update"]);
    assert!(!fail.status.success(), "{}", text(&fail));
    assert_eq!(pg.psql("SELECT COUNT(*) FROM person WHERE name = 'halfway'"), "0", "failed changeSet must roll back");
    assert_eq!(pg.psql("SELECT COUNT(*) FROM tinox_changelog"), "2");
    p.write("migrations/more.yaml", MORE);

    // Several instances starting at once on a fresh schema: every changeSet runs exactly once.
    pg.psql("DROP TABLE person, tinox_changelog, tinox_changelog_lock");
    let build = p.tinox(&["build"]);
    assert!(build.status.success(), "{}", text(&build));
    let exe = p.dir.join("cs");
    for round in 0..5 {
        pg.psql("DROP TABLE IF EXISTS person, tinox_changelog, tinox_changelog_lock");
        let kids: Vec<_> = (0..3)
            .map(|_| {
                Command::new(&exe)
                    .current_dir(&p.dir)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .expect("spawn instance")
            })
            .collect();
        for mut k in kids {
            assert!(k.wait().unwrap().success(), "round {round}: an instance failed to start");
        }
        assert_eq!(pg.psql("SELECT COUNT(*) FROM tinox_changelog"), "2", "round {round}");
        assert_eq!(pg.psql("SELECT COUNT(*) FROM person"), "1", "round {round}: seed row must exist exactly once");
    }
}

#[test]
fn changesets_rollback_by_count_and_tag_refuses_irreversible() {
    if !tool_available("sqlite3", &["--version"]) {
        eprintln!("SKIP changesets_rollback (sqlite3 not installed)");
        return;
    }
    let p = Project::new("rollback", "sqlite", "data/rb.db", "");
    let db = p.dir.join("data/rb.db");
    let base = r#"databaseChangeLog:
  - changeSet:
      id: 1
      author: tino
      tag: base
      changes:
        - createTable: { tableName: t, columns: [ { column: { name: a, type: int } } ] }
  - changeSet:
      id: 2
      author: tino
      changes:
        - insert: { tableName: t, values: { a: 7 } }
  - changeSet:
      id: 3
      author: tino
      changes:
        - addColumn: { tableName: t, columns: [ { column: { name: b, type: int } } ] }
        - createIndex: { tableName: t, indexName: idx_t_a, columns: [a] }
"#;
    p.write("migrations/changelog.yaml", base);
    let u = p.tinox(&["db", "update"]);
    assert!(u.status.success(), "{}", text(&u));

    let v = p.tinox(&["db", "validate", "--rollback"]);
    assert!(!v.status.success() && text(&v).contains("'insert' cannot be undone"), "{}", text(&v));
    assert!(!p.tinox(&["db", "rollback"]).status.success(), "needs --count or --tag");

    // Newest changeSet: derived rollback (index + column), history row removed with it.
    let r = p.tinox(&["db", "rollback", "--count", "1"]);
    assert!(r.status.success(), "{}", text(&r));
    assert_eq!(sqlite(&db, "SELECT COUNT(*) FROM sqlite_master WHERE name = 'idx_t_a'"), "0");
    assert_eq!(sqlite(&db, "SELECT COUNT(*) FROM pragma_table_info('t') WHERE name = 'b'"), "0");
    assert_eq!(sqlite(&db, "SELECT COUNT(*) FROM tinox_changelog"), "2");

    // Back to the tag: changeSet 2 is irreversible -> hard failure, nothing touched.
    let r = p.tinox(&["db", "rollback", "--tag", "base"]);
    assert!(!r.status.success() && text(&r).contains("nothing was rolled back"), "{}", text(&r));
    assert_eq!(sqlite(&db, "SELECT COUNT(*) FROM t"), "1");
    assert_eq!(sqlite(&db, "SELECT COUNT(*) FROM tinox_changelog"), "2");
    assert_eq!(sqlite(&db, "SELECT COUNT(*) FROM tinox_changelog_lock"), "0", "lock must be released");

    // Too many / unknown tag are refused as well.
    assert!(!p.tinox(&["db", "rollback", "--count", "9"]).status.success());
    assert!(!p.tinox(&["db", "rollback", "--tag", "nope"]).status.success());

    // An explicit rollback block (not part of the checksum) makes it reversible.
    p.write("migrations/changelog.yaml", &base.replace(
        "        - insert: { tableName: t, values: { a: 7 } }\n",
        "        - insert: { tableName: t, values: { a: 7 } }\n      rollback:\n        - sql: { dbms: sqlite, statements: \"DELETE FROM t WHERE a = 7\" }\n",
    ));
    let r = p.tinox(&["db", "rollback", "--tag", "base"]);
    assert!(r.status.success(), "{}", text(&r));
    assert_eq!(sqlite(&db, "SELECT COUNT(*) FROM t"), "0");
    assert_eq!(sqlite(&db, "SELECT COUNT(*) FROM tinox_changelog"), "1");

    // Re-applying works after a rollback.
    assert!(p.tinox(&["db", "update"]).status.success());
    assert_eq!(sqlite(&db, "SELECT COUNT(*) FROM t"), "1");
    assert_eq!(sqlite(&db, "SELECT COUNT(*) FROM tinox_changelog"), "3");
}

#[test]
fn changesets_migrate_on_test_is_opt_in() {
    if !tool_available("sqlite3", &["--version"]) {
        eprintln!("SKIP changesets_migrate_on_test (sqlite3 not installed)");
        return;
    }
    const TEST_SRC: &str = "class SchemaTest\n{\n    @Test\n    fnc ok() -> Bool\n    {\n        return true;\n    }\n}\n";
    // Default: tinox test never migrates, even with migrate_on_startup.
    let p = Project::new("ontest-off", "sqlite", "data/test.db", "migrate_on_startup = true\n");
    p.write("src/SchemaTest.tnx", TEST_SRC);
    let off = p.tinox(&["test"]);
    assert!(off.status.success(), "{}", text(&off));
    assert!(!p.dir.join("data/test.db").exists() || sqlite(&p.dir.join("data/test.db"), "SELECT COUNT(*) FROM sqlite_master WHERE name = 'person'") == "0");
    // Opt-in: the schema exists after the test run.
    let p = Project::new("ontest-on", "sqlite", "data/test.db", "migrate_on_test = true\n");
    p.write("src/SchemaTest.tnx", TEST_SRC);
    let on = p.tinox(&["test"]);
    assert!(on.status.success(), "{}", text(&on));
    assert_eq!(sqlite(&p.dir.join("data/test.db"), "SELECT COUNT(*) FROM person"), "1");
}

struct MysqlContainer {
    name: String,
    port: u16,
}

impl MysqlContainer {
    fn start() -> Self {
        let name = format!("tinox-mysql-changesets-test-{}", std::process::id());
        let _ = Command::new("docker").args(["rm", "-f", &name]).output();
        let status = Command::new("docker")
            .args([
                "run", "-d", "--rm", "--name", &name,
                "-e", "MYSQL_ROOT_PASSWORD=tinox",
                "-e", "MYSQL_DATABASE=tinox",
                "-P",
                "mysql:8.4",
            ])
            .stdout(Stdio::null())
            .status()
            .expect("spawn docker run");
        assert!(status.success(), "docker run mysql failed");
        let port_out = Command::new("docker")
            .args(["port", &name, "3306/tcp"])
            .output()
            .expect("docker port");
        assert!(port_out.status.success(), "docker port failed");
        let port_str = String::from_utf8_lossy(&port_out.stdout);
        let port: u16 = port_str
            .lines()
            .next()
            .and_then(|l| l.rsplit(':').next())
            .and_then(|p| p.trim().parse().ok())
            .unwrap_or_else(|| panic!("could not parse docker port output: {port_str}"));
        let c = MysqlContainer { name, port };
        c.wait_ready();
        c
    }

    fn exec(&self, sql: &str) -> Option<String> {
        // `-h 127.0.0.1` forces TCP: the image's temporary init server is skip-networking
        // (unix socket only), so a TCP answer means the final server is up.
        let out = Command::new("docker")
            .args(["exec", &self.name, "mysql", "-h127.0.0.1", "-uroot", "-ptinox", "-N", "-B", "-D", "tinox", "-e", sql])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn wait_ready(&self) {
        for _ in 0..180 {
            if self.exec("SELECT 1").is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        panic!("mysql container never became ready");
    }

    fn query(&self, sql: &str) -> String {
        self.exec(sql).unwrap_or_else(|| panic!("mysql query failed: {sql}"))
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
fn changesets_mysql_apply_validate_partial_and_concurrent_starters() {
    if !tool_available("docker", &["--version"]) {
        eprintln!("SKIP changesets_mysql (docker not installed)");
        return;
    }
    let my = MysqlContainer::start();
    let p = Project::new("mysql", "mysql", &my.url(), "migrate_on_startup = true\n");

    let v = p.tinox(&["db", "validate"]);
    assert!(v.status.success(), "{}", text(&v));

    let up = p.tinox(&["db", "update"]);
    assert!(up.status.success(), "{}", text(&up));
    assert_eq!(my.query("SELECT CONCAT(name, ':', status) FROM person"), "seeded:ACTIVE");
    assert_eq!(my.query("SELECT COUNT(*) FROM information_schema.statistics WHERE table_schema = 'tinox' AND index_name = 'idx_person_name'"), "1");
    assert_eq!(my.query("SELECT data_type FROM information_schema.columns WHERE table_schema = 'tinox' AND table_name = 'person' AND column_name = 'id'"), "bigint");
    assert_eq!(my.query("SELECT COUNT(*) FROM tinox_changelog"), "2");
    assert_eq!(my.query("SELECT COUNT(*) FROM tinox_changelog_lock"), "0", "lock must be released");

    // Re-running is a no-op.
    let again = p.tinox(&["db", "update"]);
    assert!(again.status.success(), "{}", text(&again));
    assert_eq!(my.query("SELECT COUNT(*) FROM person"), "1");

    // Checksum mismatch: nothing is applied, exit is non-zero, the lock is released.
    p.write("migrations/more.yaml", &MORE.replace("seeded", "tampered"));
    let bad = p.tinox(&["db", "update"]);
    assert!(!bad.status.success(), "{}", text(&bad));
    assert!(text(&bad).contains("modified after it was applied"), "{}", text(&bad));
    assert_eq!(my.query("SELECT COUNT(*) FROM tinox_changelog_lock"), "0");
    let st = p.tinox(&["db", "status"]);
    assert!(!st.status.success() && text(&st).contains("CHANGED"), "{}", text(&st));
    p.write("migrations/more.yaml", MORE);

    // Partial application: DDL is not transactional on MySQL. The first statement's effect
    // stays, the changeSet has no history row, and the error says so.
    p.append(
        "migrations/more.yaml",
        r#"
  - changeSet:
      id: 3
      author: tino
      changes:
        - createTable: { tableName: half_done, columns: [ { column: { name: id, type: int } } ] }
        - sql: { dbms: mysql, statements: "INSERT INTO no_such_table VALUES (1)" }
"#,
    );
    let fail = p.tinox(&["db", "update"]);
    assert!(!fail.status.success(), "{}", text(&fail));
    assert!(text(&fail).contains("cannot roll back DDL"), "{}", text(&fail));
    assert_eq!(my.query("SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = 'tinox' AND table_name = 'half_done'"), "1", "partial DDL stays");
    assert_eq!(my.query("SELECT COUNT(*) FROM tinox_changelog"), "2", "failed changeSet must not be recorded");
    assert_eq!(my.query("SELECT COUNT(*) FROM tinox_changelog_lock"), "0");
    p.write("migrations/more.yaml", MORE);

    // Several instances starting at once on a fresh schema: every changeSet runs exactly once.
    let build = p.tinox(&["build"]);
    assert!(build.status.success(), "{}", text(&build));
    let exe = p.dir.join("cs");
    for round in 0..5 {
        my.query("DROP TABLE IF EXISTS person, half_done, tinox_changelog, tinox_changelog_lock");
        let kids: Vec<_> = (0..3)
            .map(|_| {
                Command::new(&exe)
                    .current_dir(&p.dir)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .expect("spawn instance")
            })
            .collect();
        for mut k in kids {
            assert!(k.wait().unwrap().success(), "round {round}: an instance failed to start");
        }
        assert_eq!(my.query("SELECT COUNT(*) FROM tinox_changelog"), "2", "round {round}");
        assert_eq!(my.query("SELECT COUNT(*) FROM person"), "1", "round {round}: seed row must exist exactly once");
    }
}
