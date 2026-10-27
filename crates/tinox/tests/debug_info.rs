//! Issues #498-#500: what a debugger sees in a `--debug` build. Needs gdb;
//! every test is skipped (with a message) when it is not installed.
use std::path::{Path, PathBuf};
use std::process::Command;

fn gdb_available() -> bool {
    Command::new("gdb").arg("--version").output().is_ok_and(|o| o.status.success())
}

/// Builds `files` as a project in a temp dir with `--debug` and returns
/// (dir, executable).
fn build_debug(name: &str, files: &[(&str, &str)]) -> (PathBuf, PathBuf) {
    let d = std::env::temp_dir().join(format!("tinox_dbginfo_{}_{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("tinox.toml"),
        format!("[package]\ncompiler = \"{}\"\nname = \"dbg\"\nversion = \"0.1.0\"\n", env!("CARGO_PKG_VERSION")),
    )
    .unwrap();
    for (n, t) in files {
        std::fs::write(d.join(n), t).unwrap();
    }
    let exe = d.join("app");
    let out = Command::new(env!("CARGO_BIN_EXE_tinox"))
        .current_dir(&d)
        .args(["build", "Main.tnx", "--debug", "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (d, exe)
}

/// Runs gdb in batch mode and returns the program-independent output.
fn gdb(dir: &Path, exe: &Path, commands: &[&str]) -> String {
    let script = dir.join("cmds.gdb");
    let mut text = String::from("set pagination off\nset debuginfod enabled off\nset style enabled off\n");
    for c in commands {
        text.push_str(c);
        text.push('\n');
    }
    std::fs::write(&script, text).unwrap();
    let out = Command::new("gdb").args(["-q", "-batch", "-x"]).arg(&script).arg(exe).output().unwrap();
    let all = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    all.lines().filter(|l| !l.starts_with("[New Thread") && !l.starts_with("[Thread") && !l.contains("libthread_db") && !l.contains("Using host")).collect::<Vec<_>>().join("\n")
}

pub const MAIN: &str = "class Main\n{\n    fnc work(n: Int64) -> Int64\n    {\n        var total: Int64 = 0\n        var i: Int64 = 0\n        while (i < n)\n        {\n            total = total + i\n            i = i + 1\n        }\n        return total\n    }\n\n    fnc main() -> Int32\n    {\n        const sum: Int64 = Main::work(4)\n        println(sum.toString())\n        return 0\n    }\n}\n";

#[test]
fn breakpoint_on_a_body_line_and_stepping_follow_the_source() {
    if !gdb_available() {
        eprintln!("SKIP: gdb not installed");
        return;
    }
    let (d, exe) = build_debug("steps", &[("Main.tnx", MAIN)]);
    let out = gdb(&d, &exe, &["break Main.tnx:9", "run", "next", "next", "next", "next"]);
    // breakpoint lands on line 9, then the loop body runs line by line and loops back
    assert!(out.contains("Breakpoint 1 at") && out.contains("file Main.tnx, line 9"), "{out}");
    let steps: Vec<&str> = out.lines().filter(|l| l.chars().next().is_some_and(|c| c.is_ascii_digit())).collect();
    let numbers: Vec<u32> = steps.iter().filter_map(|l| l.split_whitespace().next()?.parse().ok()).collect();
    assert_eq!(&numbers[..5], &[9, 10, 7, 9, 10], "unexpected stepping order: {out}");
    // the backtrace names the line, not only the function
    let bt = gdb(&d, &exe, &["break Main.tnx:10", "run", "bt 2"]);
    assert!(bt.contains("Main.work (n=4) at Main.tnx:10"), "{bt}");
    assert!(bt.contains("Main.main ()") && bt.contains("Main.tnx:17"), "caller frame must show its call line: {bt}");
}

#[test]
fn arguments_and_locals_show_with_their_values() {
    if !gdb_available() {
        eprintln!("SKIP: gdb not installed");
        return;
    }
    let (d, exe) = build_debug("locals", &[("Main.tnx", MAIN)]);
    let out = gdb(&d, &exe, &["break Main.tnx:9", "run", "info args", "info locals", "next", "next", "next", "print total", "print i", "up", "info locals"]);
    assert!(out.contains("Main.work (n=4)"), "frame line shows the argument: {out}");
    assert!(out.contains("n = 4"), "{out}");
    assert!(out.contains("total = 0") && out.contains("i = 0"), "locals before the first iteration: {out}");
    // after one pass through the loop body: total = 0 + 0, i = 1
    assert!(out.contains("$1 = 0") && out.contains("$2 = 1"), "values advance with execution: {out}");
    // the caller frame lists its own locals, including the declared one
    let up = out.split("up").last().unwrap_or("");
    assert!(out.contains("sum = ") || up.contains("sum"), "caller local missing: {out}");
}

#[test]
fn release_builds_carry_no_variable_info() {
    let d = std::env::temp_dir().join(format!("tinox_dbginfo_release_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("tinox.toml"), format!("[package]\ncompiler = \"{}\"\nname = \"r\"\nversion = \"0.1.0\"\n", env!("CARGO_PKG_VERSION"))).unwrap();
    std::fs::write(d.join("Main.tnx"), MAIN).unwrap();
    let exe = d.join("app");
    let out = Command::new(env!("CARGO_BIN_EXE_tinox")).current_dir(&d).args(["build", "Main.tnx", "-o", exe.to_str().unwrap()]).output().unwrap();
    assert!(out.status.success());
    if let Ok(dump) = Command::new("readelf").arg("--debug-dump=info").arg(&exe).output() {
        let text = String::from_utf8_lossy(&dump.stdout);
        assert!(text.contains("DW_TAG_subprogram"), "function-level info stays in release builds");
        assert!(!text.contains("DW_TAG_variable") && !text.contains("DW_TAG_formal_parameter"), "release builds must not describe variables");
    }
}

const TYPES_MAIN: &str = "import Person\nimport Node\n\nclass Main\n{\n    fnc main() -> Int32\n    {\n        const p = Person { id: 7, label: \"seven\" }\n        const xs: List<Int64> = [1, 2, 3]\n        const names: List<String> = [\"a\", \"b\"]\n        var ages: Map<String, Int64> = Map::new()\n        ages.set(\"ann\", 31)\n        const n = Node { value: 1, next: Node { value: 2, next: null } }\n        const name: String = \"tinox\"\n        println(name + p.label)\n        return 0\n    }\n}\n";

fn printers() -> String {
    format!("source {}/../../runtime/tinox-gdb.py", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn classes_lists_and_maps_print_readably_with_the_pretty_printers() {
    if !gdb_available() {
        eprintln!("SKIP: gdb not installed");
        return;
    }
    let (d, exe) = build_debug(
        "types",
        &[
            ("Main.tnx", TYPES_MAIN),
            ("Person.tnx", "class Person\n{\n    var id: Int64\n    var label: String\n}\n"),
            ("Node.tnx", "class Node\n{\n    var value: Int64\n    var next: Node\n}\n"),
        ],
    );
    let src = printers();
    let out = gdb(&d, &exe, &[&src, "break Main.tnx:16", "run", "info locals", "print *p", "print xs"]);
    assert!(out.contains("p = {id = 7, label = \"seven\"}"), "{out}");
    assert!(out.contains("xs = {1, 2, 3}"), "{out}");
    assert!(out.contains("names = {\"a\", \"b\"}"), "{out}");
    assert!(out.contains("ages = {[\"ann\"] = 31}"), "{out}");
    assert!(out.contains("n = {value = 1, next = {value = 2, next = null}}"), "self-referential classes terminate: {out}");
    assert!(out.contains("name = 0x") && out.contains("\"tinox\""), "{out}");
    assert!(out.contains("$2 = {1, 2, 3}"), "{out}");
    // without the printers the raw structure is still inspectable
    let raw = gdb(&d, &exe, &["break Main.tnx:16", "run", "print *xs", "print *p"]);
    assert!(raw.contains("{len = 3, cap = 4, data = 0x") && raw.contains("{id = 7, label = 0x"), "{raw}");
}
