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
    assert!(bt.contains("Main.work () at Main.tnx:10"), "{bt}");
    assert!(bt.contains("Main.main ()") && bt.contains("Main.tnx:17"), "caller frame must show its call line: {bt}");
}
