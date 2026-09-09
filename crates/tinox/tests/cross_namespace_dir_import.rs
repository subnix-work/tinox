//! Regression coverage for two gaps found live while migrating the
//! external `demo` project (Postgres/REST layered app, see CLAUDE.md) to
//! issue #194 Phase 1's mandatory explicit imports:
//!
//! 1. A project-local dotted import (`import demo.model.Person;`) only
//!    ever resolved relative to the IMPORTING file's own directory, with
//!    no fallback to the project root's `src/`/`tests/` — so a file NOT
//!    at the project root (e.g. `src/demo/dao/PersonDao.tnx`) had no
//!    syntactically valid way to import a type from a DIFFERENT
//!    namespace-mirrored directory at all. Fixed by trying the project
//!    root as a fallback once the direct relative-to-self lookup fails.
//! 2. `check_explicit_imports` (#194's own validation pass) unconditionally
//!    wrapped EVERY typecheck error it collected in its "must be
//!    explicitly imported" trailer, even ones with nothing to do with
//!    imports (e.g. "missing return statement", a separate, unrelated
//!    typechecker gap: return-completeness analysis doesn't look inside
//!    try/catch bodies) — actively misleading. Fixed by letting that one
//!    specific, known-unrelated error category pass through uncaught, so
//!    the real compile pipeline's own typecheck reports it cleanly
//!    afterward instead.

use std::process::Command;

fn tinox() -> &'static str {
    env!("CARGO_BIN_EXE_tinox")
}

fn setup_project(name: &str) -> std::path::PathBuf {
    let workdir = std::env::temp_dir().join(format!("tinox-cross-ns-dir-import-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(workdir.join("src/demo/model")).expect("mkdir src/demo/model");
    std::fs::create_dir_all(workdir.join("src/demo/dao")).expect("mkdir src/demo/dao");
    std::fs::write(
        workdir.join("tinox.toml"),
        "[package]\nname = \"crossnstest\"\nversion = \"0.1.0\"\ndescription = \"\"\nentry = \"src/Main.tnx\"\n",
    )
    .expect("write tinox.toml");
    std::fs::write(
        workdir.join("src/demo/model/Person.tnx"),
        "namespace demo.model {\n    class Person {\n        var id: Int64;\n    }\n}\n",
    )
    .expect("write Person.tnx");
    // Full dotted path, written from a file that is NOT the project root
    // and NOT a sibling of Person.tnx -- this is the exact shape that
    // previously had no valid resolution at all.
    std::fs::write(
        workdir.join("src/demo/dao/PersonDao.tnx"),
        "import demo.model.Person;\n\nnamespace demo.dao {\n    class PersonDao {\n        fn get() -> Person {\n            return Person { id: 1 };\n        }\n    }\n}\n",
    )
    .expect("write PersonDao.tnx");
    std::fs::write(
        workdir.join("src/Main.tnx"),
        "import demo.dao.PersonDao;\n\nclass Main {\n    fnc main() -> Int32 {\n        let d = new PersonDao();\n        println(d.get().id);\n        return 0;\n    }\n}\n",
    )
    .expect("write Main.tnx");
    workdir
}

#[test]
fn full_dotted_import_resolves_from_project_root_when_not_relative_to_self() {
    let workdir = setup_project("root-fallback");

    let output = Command::new(tinox()).arg("run").current_dir(&workdir).output().expect("run tinox run");
    assert!(
        output.status.success(),
        "expected tinox run to succeed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains('1'));

    std::fs::remove_dir_all(&workdir).ok();
}

#[test]
fn missing_return_statement_is_not_misreported_as_a_missing_import() {
    let workdir = std::env::temp_dir().join(format!(
        "tinox-cross-ns-dir-import-test-missing-return-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(workdir.join("src")).expect("mkdir src");
    std::fs::write(
        workdir.join("tinox.toml"),
        "[package]\nname = \"missingrettest\"\nversion = \"0.1.0\"\ndescription = \"\"\nentry = \"src/Main.tnx\"\n",
    )
    .expect("write tinox.toml");
    // Both try/catch branches return, but the (separate, pre-existing)
    // return-completeness checker doesn't look inside try/catch bodies --
    // this file has no cross-namespace reference at all, so a "missing
    // import" trailer on its error would be pure noise.
    std::fs::write(
        workdir.join("src/Main.tnx"),
        "class Main {\n    fnc f() -> Bool {\n        try {\n            return true;\n        } catch (e: String) {\n            return false;\n        }\n    }\n    fnc main() -> Int32 {\n        println(f());\n        return 0;\n    }\n}\n",
    )
    .expect("write Main.tnx");

    let output = Command::new(tinox()).arg("run").current_dir(&workdir).output().expect("run tinox run");
    assert!(!output.status.success(), "expected tinox run to fail on a missing return statement");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("missing return statement"), "unexpected stderr: {stderr}");
    assert!(
        !stderr.contains("explicitly imported"),
        "missing-return error should not be wrapped in the explicit-imports trailer: {stderr}"
    );

    std::fs::remove_dir_all(&workdir).ok();
}
