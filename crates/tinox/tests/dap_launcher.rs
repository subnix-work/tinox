//! Issue #501: `tinox dap` runs gdb's DAP server with the Tinox defaults.
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

fn tinox() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tinox"))
}

fn gdb_available() -> bool {
    Command::new("gdb").arg("--version").output().is_ok_and(|o| o.status.success())
}

#[test]
fn check_reports_the_command_or_a_clear_error() {
    let out = tinox().args(["dap", "--check"]).output().unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if gdb_available() {
        // gdb may be older than 14 on some machines: then the error says so
        assert!(out.status.success() && text.contains("--interpreter=dap") || text.contains("too old"), "{text}");
    } else {
        assert!(!out.status.success() && text.contains("gdb not found"), "{text}");
    }
}

#[test]
fn missing_gdb_is_an_actionable_error() {
    let empty = std::env::temp_dir().join(format!("tinox_dap_nopath_{}", std::process::id()));
    std::fs::create_dir_all(&empty).unwrap();
    let out = tinox().args(["dap", "--check"]).env("PATH", &empty).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("gdb not found") && err.contains("14"), "{err}");
}

#[test]
fn launch_json_names_the_adapter_and_the_program() {
    let out = tinox().args(["dap", "--launch-json", "/tmp/definitely-not-built"]).env("PATH", std::env::var("PATH").unwrap_or_default()).output().unwrap();
    // needs a usable gdb to get this far
    if !gdb_available() || !out.status.success() {
        return;
    }
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["adapter"]["command"], "tinox");
    assert_eq!(v["adapter"]["args"][0], "dap");
    assert_eq!(v["request"], "launch");
    assert!(v["arguments"]["program"].as_str().unwrap().ends_with("definitely-not-built"));
    assert!(v["note"].as_str().unwrap().contains("build it"));
}

struct Dap {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    rx: Receiver<Value>,
    seq: i64,
    events: Vec<Value>,
}

impl Dap {
    fn start() -> Dap {
        let mut child = tinox().arg("dap").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut r = BufReader::new(stdout);
            loop {
                let mut len = 0usize;
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let line = line.trim();
                    if line.is_empty() {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                }
                let mut buf = vec![0u8; len];
                if r.read_exact(&mut buf).is_err() {
                    return;
                }
                if tx.send(serde_json::from_slice(&buf).unwrap()).is_err() {
                    return;
                }
            }
        });
        Dap { child, stdin, rx, seq: 0, events: Vec::new() }
    }

    fn send(&mut self, command: &str, arguments: Value) -> i64 {
        self.seq += 1;
        let body = json!({"seq": self.seq, "type": "request", "command": command, "arguments": arguments}).to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
        self.stdin.flush().unwrap();
        self.seq
    }

    /// Waits for the message satisfying `pred`, stashing everything else as events.
    fn wait(&mut self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        if let Some(i) = self.events.iter().position(&pred) {
            return self.events.remove(i);
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let m = self.rx.recv_timeout(left).unwrap_or_else(|_| panic!("timeout waiting for {what}; pending: {:?}", self.events));
            if pred(&m) {
                return m;
            }
            self.events.push(m);
        }
    }

    fn request(&mut self, command: &str, arguments: Value) -> Value {
        let id = self.send(command, arguments);
        let r = self.wait(command, |m| m["type"] == "response" && m["request_seq"] == id);
        assert_eq!(r["success"], true, "{command} failed: {r}");
        r
    }
}

#[test]
fn a_dap_client_can_stop_at_a_breakpoint_and_read_pretty_printed_variables() {
    if !gdb_available() || tinox().args(["dap", "--check"]).output().map(|o| !o.status.success()).unwrap_or(true) {
        eprintln!("SKIP: gdb 14+ not available");
        return;
    }
    let d: PathBuf = std::env::temp_dir().join(format!("tinox_dap_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("tinox.toml"), format!("[package]\ncompiler = \"{}\"\nname = \"app\"\nversion = \"0.1.0\"\n", env!("CARGO_PKG_VERSION"))).unwrap();
    let src = "class Main\n{\n    fnc work(n: Int64) -> Int64\n    {\n        const xs: List<Int64> = [1, 2, 3]\n        var total: Int64 = n\n        total = total + xs.len()\n        return total\n    }\n\n    fnc main() -> Int32\n    {\n        println(Main::work(4).toString())\n        return 0\n    }\n}\n";
    std::fs::write(d.join("Main.tnx"), src).unwrap();
    let build = tinox().current_dir(&d).args(["build", "Main.tnx", "--debug", "-o", "app"]).output().unwrap();
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));

    let mut dap = Dap::start();
    dap.request("initialize", json!({"adapterID": "tinox", "linesStartAt1": true}));
    // launch's response only arrives after configurationDone: do not wait for it yet
    let launch_id = dap.send("launch", json!({"program": d.join("app").to_string_lossy()}));
    dap.wait("initialized event", |m| m["event"] == "initialized");
    // gdb reports the breakpoint as pending until the program is loaded; it resolves on launch
    let bp = dap.request("setBreakpoints", json!({"source": {"path": d.join("Main.tnx").to_string_lossy()}, "breakpoints": [{"line": 7}]}));
    assert_eq!(bp["body"]["breakpoints"].as_array().unwrap().len(), 1, "{bp}");
    dap.request("configurationDone", json!({}));
    dap.wait("launch response", |m| m["type"] == "response" && m["request_seq"] == launch_id);
    let stopped = dap.wait("stopped", |m| m["event"] == "stopped");
    assert_eq!(stopped["body"]["reason"], "breakpoint", "{stopped}");

    let thread = stopped["body"]["threadId"].clone();
    let stack = dap.request("stackTrace", json!({"threadId": thread}));
    let top = &stack["body"]["stackFrames"][0];
    assert_eq!(top["name"], "Main.work", "{stack}");
    assert_eq!(top["line"], 7);
    let scopes = dap.request("scopes", json!({"frameId": top["id"]}));
    // arguments and locals live in separate scopes; registers are of no interest
    let mut all: Vec<Value> = Vec::new();
    for scope in scopes["body"]["scopes"].as_array().unwrap() {
        if scope["name"].as_str().unwrap_or("").starts_with("Reg") {
            continue;
        }
        let vars = dap.request("variables", json!({"variablesReference": scope["variablesReference"]}));
        all.extend(vars["body"]["variables"].as_array().unwrap().iter().cloned());
    }
    let find = |name: &str| all.iter().find(|v| v["name"] == name).unwrap_or_else(|| panic!("{name} missing in {all:?}")).clone();
    assert_eq!(find("n")["value"], "4");
    assert_eq!(find("total")["value"], "4");
    let xs = find("xs");
    assert_eq!(xs["value"], "[1, 2, 3] (len=3)", "the Tinox pretty-printer must be active and DAP-friendly: {xs}");
    assert_eq!(xs["indexedVariables"], 3, "elements stay expandable");

    let _ = dap.send("disconnect", json!({"terminateDebuggee": true}));
    let _ = dap.child.kill();
    let _ = dap.child.wait();
}
