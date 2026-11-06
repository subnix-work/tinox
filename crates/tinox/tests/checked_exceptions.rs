//! `[language] checked_exceptions` (#524, stage 3): `tinox check` reports `@Error` classes a call
//! can raise that are neither caught nor listed in `@Throws`.

use std::path::Path;
use std::process::Command;

fn project(name: &str, mode: &str, service: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("tinox_checked_exc_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("tinox.toml"),
        format!("[package]\ncompiler = \"2.9.2\"\nname = \"ce\"\nversion = \"0.1.0\"\ncompiler = \"{}\"\n\n[language]\nchecked_exceptions = \"{mode}\"\n", env!("CARGO_PKG_VERSION")),
    )
    .unwrap();
    let w = |f: &str, s: &str| std::fs::write(dir.join("src").join(f), s).unwrap();
    w("NotFound.tnx", "@Error(\"NOT_FOUND\", 404)\nclass NotFound { var message: String }\n");
    w("Svc.tnx", service);
    w("Main.tnx", "import Svc\n\nclass Main {\n    fnc main() -> Int32 {\n        println(Svc::safe().toString())\n        return 0\n    }\n}\n");
    dir
}

fn check(dir: &Path) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_tinox"))
        .arg("check")
        .arg("src/Main.tnx")
        .current_dir(dir)
        .output()
        .unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

const LEAKY: &str = "import NotFound\n\nclass Svc {\n    @Throws(NotFound)\n    fnc find() -> Int64 {\n        throw NotFound { message: \"x\" }\n    }\n    fnc safe() -> Int64 {\n        return Svc::find()\n    }\n}\n";
const FIXED: &str = "import NotFound\n\nclass Svc {\n    @Throws(NotFound)\n    fnc find() -> Int64 {\n        throw NotFound { message: \"x\" }\n    }\n    fnc safe() -> Int64 {\n        try {\n            return Svc::find()\n        } catch (e: NotFound) {\n            return 0\n        }\n    }\n}\n";

#[test]
fn error_mode_rejects_an_unhandled_call() {
    let (ok, text) = check(&project("error", "error", LEAKY));
    assert!(!ok, "{text}");
    assert!(text.contains("call to Svc.find may raise NotFound"), "{text}");
}

#[test]
fn warn_mode_reports_but_succeeds() {
    let (ok, text) = check(&project("warn", "warn", LEAKY));
    assert!(ok, "{text}");
    assert!(text.contains("warning:") && text.contains("may raise NotFound"), "{text}");
}

#[test]
fn a_catch_satisfies_the_checker() {
    let (ok, text) = check(&project("fixed", "error", FIXED));
    assert!(ok, "{text}");
    assert!(!text.contains("may raise"), "{text}");
}

#[test]
fn the_checker_is_off_by_default() {
    let (ok, text) = check(&project("off", "off", LEAKY));
    assert!(ok, "{text}");
    assert!(!text.contains("may raise"), "{text}");
}
