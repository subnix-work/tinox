//! Message translation (issue #588): `tr`/`trc`/`trn` with gettext catalogs compiled by `msgfmt`, `%N`
//! placeholders that a translation can reorder, plural rules evaluated from the catalog header, and
//! `tinox i18n extract` checked against `msgmerge` (an independent gettext tool).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn available(tool: &str) -> bool {
    Command::new(tool).arg("--version").output().is_ok()
}

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/i18n")
}

fn build(source: &Path, out: &Path) {
    let b = Command::new(env!("CARGO_BIN_EXE_tinox")).arg("build").arg(source).arg("-o").arg(out).output().unwrap();
    assert!(b.status.success(), "{}", text(&b));
}

fn run(app: &Path, envs: &[(&str, &str)]) -> String {
    let out = Command::new(app).env_remove("LANGUAGE").env_remove("LC_ALL").env_remove("LC_MESSAGES").env_remove("LANG").envs(envs.iter().copied()).output().unwrap();
    assert!(out.status.success(), "{}", text(&out));
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn compile_catalog(po: &Path, dir: &Path, lang: &str, domain: &str) {
    let target = dir.join(lang).join("LC_MESSAGES");
    std::fs::create_dir_all(&target).unwrap();
    let c = Command::new("msgfmt").arg(po).arg("-o").arg(target.join(format!("{domain}.mo"))).output().unwrap();
    assert!(c.status.success(), "{}", text(&c));
}

#[test]
fn catalogs_placeholders_context_and_plurals() {
    if !available("msgfmt") {
        eprintln!("SKIP i18n (gettext msgfmt not installed)");
        return;
    }
    let dir = Dir(std::env::temp_dir().join(format!("tinox-i18n-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(&dir.0).unwrap();
    let app = dir.0.join("app");
    let plural = dir.0.join("plural");
    build(&fixtures().join("Main.tnx"), &app);
    build(&fixtures().join("plural/Main.tnx"), &plural);
    let locale = dir.0.join("locale");
    compile_catalog(&fixtures().join("de.po"), &locale, "de", "messages");
    compile_catalog(&fixtures().join("ru.po"), &locale, "ru", "messages");
    let loc = locale.to_str().unwrap();

    // No language, "C" or an unknown language: the English source text, plural rule n != 1.
    let english = "Hello\nSaved 3 files to disk\nOpen\n1 file\n5 files\n";
    assert_eq!(run(&app, &[("TINOX_LOCALE_DIR", loc)]), english);
    assert_eq!(run(&app, &[("TINOX_LOCALE_DIR", loc), ("LANG", "C")]), english);
    assert_eq!(run(&app, &[("TINOX_LOCALE_DIR", loc), ("LANGUAGE", "fr")]), english);

    // German: the translation reorders the placeholders (%2 before %1); context and plural forms resolve.
    let german = "Hallo\nAuf disk wurden 3 Dateien gespeichert\nÖffnen\n1 Datei\n5 Dateien\n";
    assert_eq!(run(&app, &[("TINOX_LOCALE_DIR", loc), ("LANGUAGE", "de")]), german);
    // LANG with region, encoding and modifier falls back to the language.
    assert_eq!(run(&app, &[("TINOX_LOCALE_DIR", loc), ("LANG", "de_DE.UTF-8")]), german);
    assert_eq!(run(&app, &[("TINOX_LOCALE_DIR", loc), ("LC_ALL", "de_AT@euro")]), german);
    // LANGUAGE is a list; the first language with a catalog wins.
    assert_eq!(run(&app, &[("TINOX_LOCALE_DIR", loc), ("LANGUAGE", "fr:de:ru")]), german);

    // A catalog next to the executable is found without any environment setting.
    std::fs::rename(&locale, dir.0.join("locale-moved")).unwrap();
    std::fs::rename(dir.0.join("locale-moved"), dir.0.join("locale")).unwrap();
    assert_eq!(run(&app, &[("LANGUAGE", "de")]), german);

    // A wrong text domain finds nothing.
    assert_eq!(run(&app, &[("LANGUAGE", "de"), ("TINOX_TEXTDOMAIN", "other")]), english);

    // Three plural forms from the Plural-Forms header expression.
    let ru = run(&plural, &[("LANGUAGE", "ru")]);
    assert_eq!(ru.trim(), "1 файл; 2 файла; 5 файлов; 11 файлов; 21 файл; 22 файла; 25 файлов; 101 файл; 111 файлов;");
    let de = run(&plural, &[("LANGUAGE", "de")]);
    assert!(de.starts_with("1 Datei; 2 Dateien; 5 Dateien;"), "{de}");
}

#[test]
fn extraction_matches_the_translators_catalog() {
    if !available("msgmerge") {
        eprintln!("SKIP i18n extraction (gettext msgmerge not installed)");
        return;
    }
    let dir = Dir(std::env::temp_dir().join(format!("tinox-i18n-extract-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(&dir.0).unwrap();
    let pot = dir.0.join("messages.pot");
    let extract = Command::new(env!("CARGO_BIN_EXE_tinox"))
        .args(["i18n", "extract", "--out"])
        .arg(&pot)
        .arg(fixtures().join("Main.tnx"))
        .output()
        .unwrap();
    assert!(extract.status.success(), "{}", text(&extract));
    let template = std::fs::read_to_string(&pot).unwrap();
    for expected in ["msgid \"Hello\"", "msgctxt \"menu\"\nmsgid \"Open\"", "msgid \"%1 file\"\nmsgid_plural \"%1 files\"", "msgid \"Saved %1 files to %2\""] {
        assert!(template.contains(expected), "missing {expected:?} in\n{template}");
    }
    assert!(template.contains("#: ") && template.contains("Main.tnx:"), "{template}");

    // The independent tool accepts the template and maps every one of the translator's entries onto it:
    // nothing is obsolete, nothing is untranslated.
    let merged = dir.0.join("merged.po");
    let merge = Command::new("msgmerge").args(["--no-fuzzy-matching", "-q"]).arg(fixtures().join("de.po")).arg(&pot).arg("-o").arg(&merged).output().unwrap();
    assert!(merge.status.success(), "{}", text(&merge));
    let merged_text = std::fs::read_to_string(&merged).unwrap();
    assert!(!merged_text.contains("\n#~"), "obsolete entries after merging:\n{merged_text}");
    assert!(!merged_text.contains("#, fuzzy"), "{merged_text}");
    // Extraction is deterministic.
    let again = dir.0.join("again.pot");
    let second = Command::new(env!("CARGO_BIN_EXE_tinox")).args(["i18n", "extract", "--out"]).arg(&again).arg(fixtures().join("Main.tnx")).output().unwrap();
    assert!(second.status.success());
    assert_eq!(std::fs::read(&pot).unwrap(), std::fs::read(&again).unwrap());
}

#[test]
fn texts_must_be_literals() {
    let dir = Dir(std::env::temp_dir().join(format!("tinox-i18n-literal-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(&dir.0).unwrap();
    let main = dir.0.join("Main.tnx");
    std::fs::write(&main, "class Main {\n    fnc main() -> Int32 {\n        const name = \"x\"\n        println(tr(name))\n        println(tr(\"a\" + name))\n        return 0\n    }\n}\n").unwrap();
    let build = Command::new(env!("CARGO_BIN_EXE_tinox")).arg("build").arg(&main).arg("-o").arg(dir.0.join("app")).output().unwrap();
    assert!(!build.status.success());
    assert!(text(&build).contains("needs string literals"), "{}", text(&build));
    // The extractor reports the same calls with their positions and fails.
    let extract = Command::new(env!("CARGO_BIN_EXE_tinox")).args(["i18n", "extract", "--out", "-"]).arg(&main).output().unwrap();
    assert!(!extract.status.success());
    assert!(text(&extract).contains("Main.tnx:4") && text(&extract).contains("Main.tnx:5"), "{}", text(&extract));
}

#[test]
fn a_classs_own_tr_method_keeps_its_meaning() {
    let dir = Dir(std::env::temp_dir().join(format!("tinox-i18n-shadow-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(&dir.0).unwrap();
    let main = dir.0.join("Main.tnx");
    std::fs::write(&main, "class Main {\n    fnc tr(s: String) -> String {\n        return \"mine:\" + s\n    }\n    fnc main() -> Int32 {\n        const name = \"x\"\n        println(tr(name))\n        println(Main::tr(\"lit\"))\n        return 0\n    }\n}\n").unwrap();
    let app = dir.0.join("app");
    build(&main, &app);
    assert_eq!(run(&app, &[]), "mine:x\nmine:lit\n");
}
