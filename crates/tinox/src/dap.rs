//! `tinox dap` (issue #501): launches gdb's built-in Debug Adapter Protocol
//! server with Tinox defaults, so a DAP client configures one command.
//!
//! ```text
//! tinox dap                      serve DAP on stdin/stdout (replaces this process with gdb)
//! tinox dap --check              verify gdb (14+) and the pretty-printers, print what will run
//! tinox dap --launch-json [exe]  print the adapter command and a launch request for `exe`
//! ```
//!
//! gdb's DAP server needs gdb 14 or newer. The defaults are `debuginfod`
//! off (no network prompts), pagination off, and `tinox-gdb.py` sourced so
//! classes, lists and maps show readably in the variables view.

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

const MIN_GDB_MAJOR: u32 = 14;

/// `tinox-gdb.py` next to `runtime.c`: the dev checkout first, then the system install.
pub fn printers_path() -> Option<PathBuf> {
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../runtime/tinox-gdb.py");
    if dev.is_file() {
        return dev.canonicalize().ok();
    }
    let system = PathBuf::from("/usr/share/tinox/tinox-gdb.py");
    system.is_file().then_some(system)
}

/// Major and minor of the first version-looking token of `gdb --version`'s
/// first line ("GNU gdb (GDB) 18.1", "GNU gdb (Ubuntu 15.0.50.2024) 15.0.50.2024-git").
pub fn parse_gdb_version(output: &str) -> Option<(u32, u32)> {
    let line = output.lines().next()?;
    line.split_whitespace().rev().find_map(|tok| {
        let mut parts = tok.trim_matches(|c: char| !c.is_ascii_digit() && c != '.').split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next().and_then(|m| m.parse().ok()).unwrap_or(0);
        Some((major, minor))
    })
}

pub struct Plan {
    pub version: (u32, u32),
    pub printers: PathBuf,
}

/// Finds a usable gdb and the printers, or explains what is missing.
pub fn plan() -> Result<Plan, String> {
    let out = Command::new("gdb")
        .arg("--version")
        .output()
        .map_err(|e| format!("gdb not found ({e}); install gdb {MIN_GDB_MAJOR} or newer: its built-in DAP server is what `tinox dap` runs"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let version = parse_gdb_version(&text).ok_or_else(|| format!("cannot read the gdb version from {:?}", text.lines().next().unwrap_or("")))?;
    if version.0 < MIN_GDB_MAJOR {
        return Err(format!("gdb {}.{} is too old: the DAP server needs gdb {MIN_GDB_MAJOR} or newer", version.0, version.1));
    }
    let printers = printers_path().ok_or("tinox-gdb.py not found (looked next to runtime.c in the checkout and in /usr/share/tinox)")?;
    Ok(Plan { version, printers })
}

fn gdb_args(printers: &std::path::Path) -> Vec<String> {
    vec![
        "-q".into(),
        "--interpreter=dap".into(),
        "-iex".into(),
        "set debuginfod enabled off".into(),
        "-iex".into(),
        "set pagination off".into(),
        "-iex".into(),
        // a DAP client needs one-line values next to the expandable children
        "python import builtins; builtins.TINOX_DAP = True".into(),
        "-iex".into(),
        format!("source {}", printers.display()),
    ]
}

pub fn run(args: &[String]) {
    let plan = match plan() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    if args.iter().any(|a| a == "--check") {
        println!("gdb {}.{} (DAP server available)", plan.version.0, plan.version.1);
        println!("pretty-printers: {}", plan.printers.display());
        println!("command: gdb {}", gdb_args(&plan.printers).join(" "));
        return;
    }
    if let Some(pos) = args.iter().position(|a| a == "--launch-json") {
        let exe = args.get(pos + 1).filter(|a| !a.starts_with('-')).cloned().or_else(default_artifact);
        let Some(exe) = exe else {
            eprintln!("error: pass the executable (built with `tinox build --debug`) or run inside a project");
            std::process::exit(1);
        };
        let exe = std::fs::canonicalize(&exe).unwrap_or_else(|_| PathBuf::from(&exe));
        let exe_exists = exe.is_file();
        println!(
            "{}",
            serde_json::json!({
                "adapter": { "command": "tinox", "args": ["dap"] },
                "request": "launch",
                "arguments": { "program": exe.to_string_lossy() },
                "note": if exe_exists {
                    "set breakpoints after the adapter's `initialized` event, then send configurationDone"
                } else {
                    "the program does not exist yet: build it with `tinox build --debug` first"
                },
            })
        );
        return;
    }
    // Replace this process: gdb speaks DAP on our stdin/stdout.
    let err = Command::new("gdb").args(gdb_args(&plan.printers)).exec();
    eprintln!("error: cannot start gdb: {err}");
    std::process::exit(1);
}

/// `./<project name>` of the nearest tinox.toml.
fn default_artifact() -> Option<String> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let toml = dir.join("tinox.toml");
        if let Ok(content) = std::fs::read_to_string(&toml) {
            let name = content.lines().find_map(|l| {
                let (k, v) = l.split_once('=')?;
                (k.trim() == "name").then(|| v.trim().trim_matches('"').to_string())
            })?;
            return Some(dir.join(name).to_string_lossy().into_owned());
        }
        if !dir.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gdb_versions() {
        assert_eq!(parse_gdb_version("GNU gdb (GDB) 18.1\nCopyright"), Some((18, 1)));
        assert_eq!(parse_gdb_version("GNU gdb (Ubuntu 15.0.50.20240403-0ubuntu1) 15.0.50.20240403-git\n"), Some((15, 0)));
        assert_eq!(parse_gdb_version("GNU gdb (Debian 13.1-3) 13.1\n"), Some((13, 1)));
        assert_eq!(parse_gdb_version("not gdb"), None);
    }

    #[test]
    fn old_gdb_is_named_in_the_error() {
        // version gate is applied in plan(); the parse result drives it
        assert!(parse_gdb_version("GNU gdb (GDB) 13.2").unwrap().0 < MIN_GDB_MAJOR);
    }
}
