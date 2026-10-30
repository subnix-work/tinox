//! @Cacheable + @Transactional @CacheEvict (issue #543) with a real SQLite database: eviction happens
//! only after a successful commit, a failed update leaves cache and row alone, and concurrent readers
//! never keep serving the old value after the update. The row is cross-checked with `sqlite3`.

use std::path::Path;
use std::process::Command;

#[test]
fn eviction_follows_the_commit_and_concurrent_readers_see_the_new_value() {
    if Command::new("sqlite3").arg("--version").output().is_err() {
        eprintln!("SKIP cache transactions (sqlite3 not installed)");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tinox-cache-tx-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let from = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/cache_tx");
    for f in ["Item.tnx", "Catalog.tnx", "Main.tnx"] {
        std::fs::copy(from.join(f), dir.join("src").join(f)).unwrap();
    }
    let db = dir.join("data.db");
    let create = Command::new("sqlite3").arg(&db).arg("CREATE TABLE items (id INTEGER PRIMARY KEY AUTOINCREMENT, price INTEGER NOT NULL);").output().unwrap();
    assert!(create.status.success());
    std::fs::write(
        dir.join("tinox.toml"),
        format!("[package]\ncompiler = \"2.8.0\"\nname = \"cachetx\"\nversion = \"0.1.0\"\n\n[database]\ndriver = \"sqlite\"\nurl = \"{}\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"db\"\nversion = \"1.1.0\"\n", db.display()),
    )
    .unwrap();
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let install = Command::new(tinox).arg("install").current_dir(&dir).output().unwrap();
    assert!(install.status.success(), "{}", String::from_utf8_lossy(&install.stderr));
    let build = Command::new(tinox).args(["build", "src/Main.tnx", "-o", "app"]).current_dir(&dir).output().unwrap();
    assert!(build.status.success(), "{}{}", String::from_utf8_lossy(&build.stdout), String::from_utf8_lossy(&build.stderr));
    for round in 0..10 {
        let reset = Command::new("sqlite3").arg(&db).arg("DELETE FROM items; DELETE FROM sqlite_sequence;").output().unwrap();
        assert!(reset.status.success());
        let run = Command::new(dir.join("app")).current_dir(&dir).output().unwrap();
        let out = format!("{}{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
        assert!(run.status.success(), "round {round}:\n{out}");
        assert!(out.contains("initial 1\n"), "{out}");
        assert!(out.contains("failed update: rolled back"), "{out}");
        assert!(out.contains("after failed update 1\n"), "the rollback must not change or evict anything:\n{out}");
        assert!(out.contains("after update 2\n"), "round {round}: a reader kept the stale value:\n{out}");
        let price = Command::new("sqlite3").arg(&db).arg("SELECT price FROM items").output().unwrap();
        assert_eq!(String::from_utf8_lossy(&price.stdout).trim(), "2");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
