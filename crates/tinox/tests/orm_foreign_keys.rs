//! @ForeignKey (issue #534): validated at compile time, emitted by `tinox db generate`, enforced
//! by the database. Independent `sqlite3` reads the real constraint and the rows.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn sqlite(db: &Path, sql: &str) -> Output {
    Command::new("sqlite3").arg(db).arg(sql).output().expect("sqlite3")
}

struct Project {
    dir: PathBuf,
}

impl Project {
    fn new(tag: &str) -> Project {
        let dir = std::env::temp_dir().join(format!("tinox-fk-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("migrations")).unwrap();
        let from = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/orm_foreign_key");
        for f in ["Author.tnx", "Book.tnx", "Main.tnx"] {
            std::fs::copy(from.join(f), dir.join("src").join(f)).unwrap();
        }
        std::fs::write(
            dir.join("tinox.toml"),
            format!("[package]\ncompiler = \"2.8.0\"\nname = \"fk\"\nversion = \"0.1.0\"\n\n[database]\ndriver = \"sqlite\"\nurl = \"{}\"\nmigrate_on_startup = true\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"db\"\nversion = \"1.1.0\"\n", dir.join("data.db").display()),
        )
        .unwrap();
        let p = Project { dir };
        let install = p.tinox(&["install"]);
        assert!(install.status.success(), "{}", text(&install));
        p
    }

    fn tinox(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tinox")).args(args).current_dir(&self.dir).output().unwrap()
    }

    fn build(&self) -> Output {
        self.tinox(&["build", "src/Main.tnx", "-o", "app"])
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
fn foreign_key_is_generated_enforced_and_gives_a_bounded_list_detail() {
    if !have_sqlite3() {
        eprintln!("SKIP foreign keys (sqlite3 not installed)");
        return;
    }
    let p = Project::new("ok");
    let generate = p.tinox(&["db", "generate"]);
    assert!(generate.status.success(), "{}", text(&generate));
    let log = std::fs::read_to_string(p.dir.join("migrations/changelog.yaml")).unwrap();
    assert!(log.contains("references: authors(id)"), "{log}");
    let build = p.build();
    assert!(build.status.success(), "{}", text(&build));
    let run = Command::new(p.dir.join("app")).current_dir(&p.dir).output().unwrap();
    let out = text(&run);
    assert!(run.status.success(), "{out}");
    assert!(out.contains("Ada has 2 books") && out.contains(" - Engine") && out.contains(" - Notes") && !out.contains("Other"), "{out}");

    // The real constraint exists...
    let db = p.dir.join("data.db");
    let fk = sqlite(&db, "SELECT \"table\" || '.' || \"to\" FROM pragma_foreign_key_list('books')");
    assert_eq!(String::from_utf8_lossy(&fk.stdout).trim(), "authors.id");
    // ... and a second process cannot create an orphan or delete a referenced parent.
    let orphan = sqlite(&db, "PRAGMA foreign_keys = ON; INSERT INTO books (title, author_id) VALUES ('x', 999)");
    assert!(!orphan.status.success(), "orphan row must be rejected");
    let parent = sqlite(&db, "PRAGMA foreign_keys = ON; DELETE FROM authors WHERE name = 'Ada'");
    assert!(!parent.status.success(), "referenced parent must not be deletable");
    let counts = sqlite(&db, "SELECT (SELECT COUNT(*) FROM authors) || '/' || (SELECT COUNT(*) FROM books)");
    assert_eq!(String::from_utf8_lossy(&counts.stdout).trim(), "2/3");
}

#[test]
fn the_runtime_enforces_the_constraint_for_orm_writes() {
    if !have_sqlite3() {
        return;
    }
    let p = Project::new("orphan");
    assert!(p.tinox(&["db", "generate"]).status.success());
    std::fs::write(
        p.dir.join("src/Main.tnx"),
        "import tinox.core.db\nimport Author\nimport Book\n\nclass Main\n{\n    fnc main() -> Int32\n    {\n        DB::of(Book).save(Book { id: 0, title: \"Orphan\", authorId: 999 })\n        println(\"saved\")\n        return 0\n    }\n}\n",
    )
    .unwrap();
    let build = p.build();
    assert!(build.status.success(), "{}", text(&build));
    let run = Command::new(p.dir.join("app")).current_dir(&p.dir).output().unwrap();
    assert!(!run.status.success(), "{}", text(&run));
    assert!(!text(&run).contains("saved"), "{}", text(&run));
    let rows = sqlite(&p.dir.join("data.db"), "SELECT COUNT(*) FROM books");
    assert_eq!(String::from_utf8_lossy(&rows.stdout).trim(), "0");
}

const MINIMAL_MAIN: &str = "import tinox.core.db\nimport Author\nimport Book\n\nclass Main\n{\n    fnc main() -> Int32\n    {\n        return 0\n    }\n}\n";

fn broken(tag: &str, book: &str, needle: &str) {
    let p = Project::new(tag);
    std::fs::write(p.dir.join("src/Book.tnx"), book).unwrap();
    std::fs::write(p.dir.join("src/Main.tnx"), MINIMAL_MAIN).unwrap();
    let build = p.build();
    assert!(!build.status.success(), "{tag}: must not compile");
    assert!(text(&build).contains(needle), "{tag}: expected {needle:?} in\n{}", text(&build));
}

#[test]
fn unsupported_mappings_fail_visibly() {
    if !have_sqlite3() {
        return;
    }
    let book = |fk: &str, ty: &str| format!("@Entity\n@Table(\"books\")\nclass Book\n{{\n    @Id @GeneratedValue\n    var id: Int64\n    @Column(\"title\")\n    var title: String\n    @Column(\"author_id\")\n    @ForeignKey({fk})\n    var authorId: {ty}\n}}\n");
    broken("unknown-table", &book("\"people\", \"id\"", "Int64"), "not an @Entity table");
    broken("wrong-column", &book("\"authors\", \"name\"", "Int64"), "primary key column");
    broken("type-mismatch", &book("\"authors\", \"id\"", "String"), "does not match");
    // A reference cycle: authors -> books -> authors.
    let p = Project::new("cycle");
    std::fs::write(p.dir.join("src/Main.tnx"), MINIMAL_MAIN).unwrap();
    std::fs::write(
        p.dir.join("src/Author.tnx"),
        "@Entity\n@Table(\"authors\")\nclass Author\n{\n    @Id @GeneratedValue\n    var id: Int64\n    @Column(\"name\")\n    var name: String\n    @Column(\"fav\")\n    @ForeignKey(\"books\", \"id\")\n    var fav: Int64\n}\n",
    )
    .unwrap();
    let build = p.build();
    assert!(!build.status.success());
    assert!(text(&build).contains("form a cycle"), "{}", text(&build));
}

#[test]
fn new_tables_are_generated_parents_first() {
    if !have_sqlite3() {
        return;
    }
    let p = Project::new("order");
    // "zoo_books" sorts after "authors" alphabetically; make the PARENT sort last instead.
    for (f, from, to) in [("Author.tnx", "\"authors\"", "\"zauthors\""), ("Book.tnx", "\"authors\"", "\"zauthors\"")] {
        let path = p.dir.join("src").join(f);
        let text = std::fs::read_to_string(&path).unwrap().replace(from, to);
        std::fs::write(path, text).unwrap();
    }
    let generate = p.tinox(&["db", "generate"]);
    assert!(generate.status.success(), "{}", text(&generate));
    let log = std::fs::read_to_string(p.dir.join("migrations/changelog.yaml")).unwrap();
    let parent = log.find("tableName: zauthors").expect("parent table");
    let child = log.find("tableName: books").expect("child table");
    assert!(parent < child, "parent must be created before the child:\n{log}");
}
