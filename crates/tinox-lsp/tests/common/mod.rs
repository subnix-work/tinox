//! Minimal blocking LSP client for black-box tests of the `tinox-lsp` binary.
#![allow(dead_code)]
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

pub struct Lsp {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    next_id: i64,
    /// Server->client notifications and requests seen while waiting for responses.
    pub inbox: Vec<Value>,
    /// The server's `initialize` result.
    pub init: Value,
}

impl Lsp {
    pub fn start(root: &Path, rename_files: bool) -> Lsp {
        Lsp::start_with_env(root, rename_files, &[])
    }

    pub fn start_with_env(root: &Path, rename_files: bool, env: &[(&str, &str)]) -> Lsp {
        let mut child = Command::new(env!("CARGO_BIN_EXE_tinox-lsp"))
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || read_loop(stdout, tx));
        let mut lsp = Lsp { child, stdin, rx, next_id: 1, inbox: Vec::new(), init: Value::Null };
        let ops = if rename_files { json!(["rename"]) } else { json!([]) };
        lsp.init = lsp.request(
            "initialize",
            json!({
                "processId": null,
                "rootUri": url(root),
                "capabilities": { "workspace": { "workspaceEdit": { "resourceOperations": ops } } }
            }),
        );
        lsp.notify("initialized", json!({}));
        lsp
    }

    fn write(&mut self, msg: &Value) {
        let body = msg.to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
        self.stdin.flush().unwrap();
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        self.write(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Sends a request and returns the full response object (`result` or `error`).
    pub fn request_raw(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let msg = self.rx.recv_timeout(left).unwrap_or_else(|_| panic!("timeout waiting for response to {method}"));
            if msg.get("id") == Some(&json!(id)) && msg.get("method").is_none() {
                return msg;
            }
            // answer server->client requests (e.g. workspace/configuration) with null
            if msg.get("id").is_some() && msg.get("method").is_some() {
                let rid = msg["id"].clone();
                self.write(&json!({"jsonrpc": "2.0", "id": rid, "result": null}));
            }
            self.inbox.push(msg);
        }
    }

    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let r = self.request_raw(method, params);
        assert!(r.get("error").is_none(), "{method} failed: {r}");
        r["result"].clone()
    }

    pub fn open(&mut self, path: &Path, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": url(path), "languageId": "tinox", "version": 1, "text": text}}),
        );
    }

    /// Waits for a `publishDiagnostics` notification for `path` satisfying `pred`.
    pub fn diagnostics_for(&mut self, path: &Path, pred: impl Fn(&Vec<Value>) -> bool) -> Vec<Value> {
        let uri = url(path);
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut checked = 0;
        loop {
            while checked < self.inbox.len() {
                let m = &self.inbox[checked];
                checked += 1;
                if m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == uri {
                    let d: Vec<Value> = m["params"]["diagnostics"].as_array().cloned().unwrap_or_default();
                    if pred(&d) {
                        return d;
                    }
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(m) => self.inbox.push(m),
                Err(_) => panic!("timeout waiting for diagnostics of {}", path.display()),
            }
        }
    }

    /// Collects everything the server sends during the next `ms` milliseconds.
    pub fn drain(&mut self, ms: u64) {
        let deadline = Instant::now() + Duration::from_millis(ms);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(m) => self.inbox.push(m),
                Err(_) => return,
            }
        }
    }

    /// All `publishDiagnostics` payloads for `path` received so far, oldest first.
    pub fn published(&self, path: &Path) -> Vec<Vec<Value>> {
        let uri = url(path);
        self.inbox
            .iter()
            .filter(|m| m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == uri)
            .map(|m| m["params"]["diagnostics"].as_array().cloned().unwrap_or_default())
            .collect()
    }

    pub fn shutdown(mut self) {
        let _ = self.request_raw("shutdown", json!(null));
        self.notify("exit", json!(null));
        // The server's runtime waits for its blocking stdin read: close the pipe.
        let Lsp { mut child, stdin, .. } = self;
        drop(stdin);
        let _ = child.wait();
    }
}

fn read_loop(stdout: ChildStdout, tx: std::sync::mpsc::Sender<Value>) {
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
            if let Some(v) = line.strip_prefix("Content-Length:") {
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
}

pub fn url(p: &Path) -> String {
    format!("file://{}", p.display())
}

pub fn temp_project(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("tinox_lsp_{}_{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("tinox.toml"),
        format!("[package]\ncompiler = \"{}\"\nname = \"lsp_demo\"\nversion = \"0.1.0\"\n", env!("CARGO_PKG_VERSION")),
    )
    .unwrap();
    d
}

pub fn pos(line: u32, character: u32) -> Value {
    json!({"line": line, "character": character})
}
