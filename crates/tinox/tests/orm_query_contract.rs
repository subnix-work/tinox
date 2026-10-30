//! ORM query contract (issue #533): deterministic pages, explicit absence, and compile errors for
//! anything that cannot be lowered to SQL (it used to be silently dropped). Rows are cross-checked
//! with an independent `sqlite3`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

struct Project {
    dir: PathBuf,
}

impl Project {
    fn new(tag: &str) -> Project {
        let dir = std::env::temp_dir().join(format!("tinox-ormq-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let from = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/orm_paging");
        for f in ["Item.tnx", "Main.tnx"] {
            std::fs::copy(from.join(f), dir.join("src").join(f)).unwrap();
        }
        let db = dir.join("data.db");
        let out = Command::new("sqlite3")
            .arg(&db)
            .arg("CREATE TABLE items (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, grp INTEGER NOT NULL);")
            .output()
            .expect("sqlite3");
        assert!(out.status.success(), "{}", text(&out));
        std::fs::write(
            dir.join("tinox.toml"),
            format!("[package]\ncompiler = \"2.8.0\"\nname = \"ormq\"\nversion = \"0.1.0\"\n\n[database]\ndriver = \"sqlite\"\nurl = \"{}\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"db\"\nversion = \"1.1.0\"\n", db.display()),
        )
        .unwrap();
        let install = Command::new(env!("CARGO_BIN_EXE_tinox")).arg("install").current_dir(&dir).output().unwrap();
        assert!(install.status.success(), "{}", text(&install));
        Project { dir }
    }

    fn build(&self) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tinox")).args(["build", "src/Main.tnx", "-o", "app"]).current_dir(&self.dir).output().unwrap()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn have_sqlite3() -> bool {
    Command::new("sqlite3").arg("--version").output().is_ok()
}

#[test]
fn pages_are_deterministic_and_absence_is_explicit() {
    if !have_sqlite3() {
        eprintln!("SKIP orm query contract (sqlite3 not installed)");
        return;
    }
    let p = Project::new("pages");
    let build = p.build();
    assert!(build.status.success(), "{}", text(&build));
    let run = Command::new(p.dir.join("app")).current_dir(&p.dir).output().unwrap();
    let out = text(&run);
    assert!(run.status.success(), "{out}");
    for expected in [
        "empty count: 0",
        "empty first: []",
        "page0: a b ",
        "page1: c d ",
        "page2: e ",
        "page3: \n",
        "desc: e d ",
        "first: e",
        "missing: []",
        "none: 0",
    ] {
        assert!(out.contains(expected), "missing {expected:?} in:\n{out}");
    }
    let db = p.dir.join("data.db");
    let rows = Command::new("sqlite3").arg(&db).arg("SELECT group_concat(name, '') FROM (SELECT name FROM items ORDER BY id)").output().unwrap();
    assert_eq!(String::from_utf8_lossy(&rows.stdout).trim(), "abcde");
}

fn broken(tag: &str, query: &str, needle: &str) {
    let p = Project::new(tag);
    let main = std::fs::read_to_string(p.dir.join("src/Main.tnx")).unwrap();
    let start = main.find("    fnc main()").unwrap();
    let patched = format!("{}    fnc main() -> Int32\n    {{\n        {query}\n        return 0\n    }}\n}}\n", &main[..start]);
    std::fs::write(p.dir.join("src/Main.tnx"), patched).unwrap();
    let build = p.build();
    assert!(!build.status.success(), "{tag}: must not compile");
    let msg = text(&build);
    assert!(msg.contains(needle), "{tag}: expected {needle:?} in:\n{msg}");
}

#[test]
fn unsupported_queries_fail_at_compile_time() {
    if !have_sqlite3() {
        return;
    }
    broken("unknown-field", "println(DB::of(Item).filter(i => i.nope == 1).count().toString())", "has no field 'nope'");
    broken("unsupported-expr", "println(DB::of(Item).filter(i => i.name.len() > 2).count().toString())", "cannot be translated to SQL");
    broken("order-unknown", "println(DB::of(Item).orderBy(i => i.nope).count().toString())", "unknown field 'nope'");
    broken("limit-var", "const n = 3\n        println(DB::of(Item).orderBy(i => i.name).limit(n).list().len().toString())", "limit(...) must be an integer literal");
    broken("limit-big", "println(DB::of(Item).orderBy(i => i.name).limit(100000).list().len().toString())", "page size");
    broken("offset-alone", "println(DB::of(Item).orderBy(i => i.name).offset(2).list().len().toString())", "needs limit");
}
