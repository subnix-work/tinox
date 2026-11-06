//! Bug 463: WebSocket protocol conformance (UTF-8 validation, close frames, masking, RSV bits,
//! control frames, fragmentation) checked with raw frames from an independent Python client against a
//! `tinox.core.websocket` echo server built from the in-tree sources through `[[overrides]]`.
#![cfg(unix)]

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct Kill(Child);
impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn websocket_server_passes_the_conformance_cases() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let dir = std::env::temp_dir().join(format!("tinox-ws-conformance-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let ext = root.join("crates/tinox-core-ext");
    let version = |m: &str| {
        std::fs::read_to_string(ext.join(m).join("tinox.toml"))
            .unwrap()
            .lines()
            .find_map(|l| l.strip_prefix("version = ").map(|v| v.trim_matches('"').to_string()))
            .unwrap()
    };
    let mut toml = String::from("[package]\ncompiler = \"2.9.2\"\nname = \"wsconf\"\nversion = \"0.0.0\"\ndescription = \"\"\n");
    for m in ["websocket", "http_server", "json"] {
        toml.push_str(&format!("\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"{m}\"\nversion = \"{}\"\n", version(m)));
    }
    for m in ["websocket", "http_server"] {
        toml.push_str(&format!("\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"{m}\"\npath = \"{}\"\n", ext.join(m).display()));
    }
    std::fs::write(dir.join("tinox.toml"), toml).unwrap();
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reservation.local_addr().unwrap().port();
    std::fs::write(
        dir.join("Main.tnx"),
        format!(
            "import tinox.core.websocket\n\nclass Main {{\n    fnc main() -> Int32 {{\n        const srv = WsServer::listen({port})\n        while true {{\n            const conn = WsServer::accept(srv)\n            if (conn > 0) {{\n                var open = true\n                while open {{\n                    const f = Ws::readMessage(conn)\n                    if (f.opcode == 1 || f.opcode == 2) {{\n                        Ws::writeFrame(conn, f.opcode, f.payload)\n                    }} else {{\n                        open = false\n                    }}\n                }}\n                httpConnClose(conn)\n            }}\n        }}\n        return 0\n    }}\n}}\n"
        ),
    )
    .unwrap();
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let install = Command::new(tinox).arg("install").current_dir(&dir).output().unwrap();
    assert!(install.status.success(), "{}", String::from_utf8_lossy(&install.stderr));
    let exe = dir.join("wsconf");
    let build = Command::new(tinox).args(["build", "Main.tnx", "-o"]).arg(&exe).current_dir(&dir).output().unwrap();
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));
    drop(reservation);
    let server = Command::new(&exe).current_dir(&dir).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let _guard = Kill(server);
    std::thread::sleep(Duration::from_millis(600));

    let script = root.join("tests/fixtures/ws_conformance/check.py");
    let out = Command::new("python3").arg(&script).arg(port.to_string()).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "WebSocket conformance failures:\n{text}\n{}", String::from_utf8_lossy(&out.stderr));
    let _ = std::fs::remove_dir_all(&dir);
}
