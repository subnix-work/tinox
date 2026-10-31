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

const SCOPES_MAIN: &str = "class Main\n{\n    fnc main() -> Int32\n    {\n        var outer: Int64 = 10\n        if (outer > 0)\n        {\n            var t: Int64 = 2\n            var inner: Int64 = 20\n            println(t.toString() + inner.toString())\n        }\n        if (outer > 0)\n        {\n            var t: String = \"second\"\n            println(t)\n        }\n        var after: Int64 = 30\n        println(after.toString())\n        return 0\n    }\n}\n";

/// Issue #556: a local is visible from its declaration to the end of its block; a name reused in a
/// sibling block is a different variable, and a breakpoint on a block's line resolves once.
#[test]
fn locals_follow_lexical_blocks_and_declaration_order() {
    if !gdb_available() {
        eprintln!("SKIP: gdb not installed");
        return;
    }
    let (d, exe) = build_debug("scopes", &[("Main.tnx", SCOPES_MAIN)]);
    let out = gdb(&d, &exe, &["break Main.tnx:8", "break Main.tnx:10", "break Main.tnx:14", "break Main.tnx:18", "run", "info locals", "c", "info locals", "c", "info locals", "c", "info locals"]);
    assert!(!out.contains("locations)"), "each line resolves to one location: {out}");
    let sections: Vec<&str> = out.split("hit Breakpoint").collect();
    let at = |line: &str| sections.iter().find(|s| s.contains(&format!("Main.tnx:{}\n", line.trim()))).copied().unwrap_or_else(|| panic!("no stop at line {line}: {out}"));
    // before `var t` is declared only `outer` exists
    let s8 = at("8");
    assert!(s8.contains("outer = 10") && !s8.contains("t =") && !s8.contains("inner") && !s8.contains("after"), "{s8}");
    // inside the first block: its own `t` (an integer) and `inner`, not the other block's or `after`
    let s10 = at("10");
    assert!(s10.contains("t = 2") && s10.contains("inner = 20") && s10.contains("outer = 10") && !s10.contains("after"), "{s10}");
    // the sibling block's `t` is a String and the first block's names are gone
    let s14 = at("14");
    assert!(s14.contains("outer = 10") && !s14.contains("inner") && !s14.contains("t = 2") && !s14.contains("after"), "{s14}");
    let s18 = at("18");
    assert!(s18.contains("after = 30") && s18.contains("outer = 10") && !s18.contains("inner") && !s18.contains("t ="), "{s18}");
}

const GENERIC_MAIN: &str = "import Box\nimport Color\nimport Shape\n\nclass Main\n{\n    fnc main() -> Int32\n    {\n        const a: Box<Int64> = Box<Int64> { value: 5, count: 1 }\n        const b: Box<String> = Box<String> { value: \"hi\", count: 2 }\n        const c: Color = Color::Green\n        const s: Shape = Shape::Square(3, 4)\n        const t: Shape = Shape::Named(\"ann\", 2.5)\n        const e: Shape = Shape::Empty\n        const ch: Char = 'x'\n        println(a.count.toString() + b.value)\n        return 0\n    }\n}\n";

/// Issue #556: concrete generic instantiations, enums and `Char` print accurately, and the raw
/// values stay reachable.
#[test]
fn generic_instantiations_enums_and_chars_print_accurately() {
    if !gdb_available() {
        eprintln!("SKIP: gdb not installed");
        return;
    }
    let (d, exe) = build_debug(
        "generic",
        &[
            ("Main.tnx", GENERIC_MAIN),
            ("Box.tnx", "class Box<T>\n{\n    var value: T\n    var count: Int64\n}\n"),
            ("Color.tnx", "enum Color\n{\n    Red\n    Green\n    Blue\n}\n"),
            ("Shape.tnx", "enum Shape\n{\n    Circle(Int64)\n    Square(Int64, Int64)\n    Named(String, Float64)\n    Empty\n}\n"),
        ],
    );
    let src = printers();
    let out = gdb(&d, &exe, &[&src, "break Main.tnx:17", "run", "info locals", "print/d c", "ptype b"]);
    assert!(out.contains("a = {value = 5, count = 1}"), "Box<Int64>: {out}");
    assert!(out.contains("b = {value = \"hi\", count = 2}"), "Box<String>: {out}");
    assert!(out.contains("c = Color::Green"), "{out}");
    assert!(out.contains("s = Shape::Square(3, 4)"), "{out}");
    assert!(out.contains("t = Shape::Named(\"ann\", 2.5)"), "{out}");
    assert!(out.contains("e = Shape::Empty"), "{out}");
    assert!(out.contains("ch = 'x' (120)"), "{out}");
    assert!(out.contains("$1 = ") && !out.contains("$1 = Color"), "raw value stays reachable with /d: {out}");
    // the type is described under its Tinox name, not as a mangled specialization
    assert!(out.contains("Box<String>"), "{out}");
    // without the printers the enumerators are still named
    let raw = gdb(&d, &exe, &["break Main.tnx:17", "run", "print c", "print e"]);
    assert!(raw.contains("$1 = Green") && raw.contains("$2 = Empty"), "{raw}");
}
