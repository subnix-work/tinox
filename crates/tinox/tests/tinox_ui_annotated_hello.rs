//! End-to-end test for the Tinox-UI @TinoxUIApp/@View annotation-sugar
//! example (issue #215, Phase 4): confirms the compiler-generated HTTP
//! shell/client-JS server and WebSocket accept loop (emit_tinoxui_code,
//! crates/tinox-codegen/src/codegen.rs) work identically to Phase 1's
//! hand-wired shape (see tinox_ui_hello.rs) even though this example's
//! own source never calls TinoxUIRuntime/HttpServer/WsServer at all --
//! all of that is synthesized purely from the @TinoxUIApp(httpPort,
//! wsPort) class annotation and a single @View method.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

struct KillOnDrop(Child);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn read_n(stream: &mut TcpStream, n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    stream.read_exact(&mut buf).expect("read_exact");
    buf
}

fn read_handshake_response(stream: &mut TcpStream) -> String {
    let mut resp = Vec::new();
    let mut byte = [0u8; 1];
    while !resp.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).expect("read handshake response");
        resp.push(byte[0]);
    }
    String::from_utf8_lossy(&resp).into_owned()
}

fn send_masked_text_frame(stream: &mut TcpStream, payload: &[u8]) {
    let mask = [5u8, 4, 3, 2];
    let mut frame = vec![0x81u8];
    assert!(payload.len() < 126, "test event payload unexpectedly large");
    frame.push(0x80 | payload.len() as u8);
    frame.extend_from_slice(&mask);
    for (i, b) in payload.iter().enumerate() {
        frame.push(b ^ mask[i % 4]);
    }
    stream.write_all(&frame).expect("send text frame");
}

fn read_text_frame(stream: &mut TcpStream) -> String {
    let hdr = read_n(stream, 2);
    assert_eq!(hdr[0], 0x81, "expected FIN+text opcode byte");
    let len_byte = hdr[1] & 0x7f;
    let plen: usize = if len_byte == 126 {
        let ext = read_n(stream, 2);
        u16::from_be_bytes([ext[0], ext[1]]) as usize
    } else if len_byte == 127 {
        let ext = read_n(stream, 8);
        u64::from_be_bytes(ext.try_into().unwrap()) as usize
    } else {
        len_byte as usize
    };
    let body = read_n(stream, plen);
    String::from_utf8_lossy(&body).into_owned()
}

fn find_button_id(json: &str) -> String {
    let marker = "\"type\":\"Button\"";
    let type_pos = json.find(marker).expect("no Button component in render");
    let before = &json[..type_pos];
    let id_key = before.rfind("\"id\":\"").expect("no id field before Button's type");
    let after_key = &before[id_key + "\"id\":\"".len()..];
    let end = after_key.find('"').expect("unterminated id string");
    after_key[..end].to_string()
}

#[test]
fn tinox_ui_annotated_hello_click_counter_end_to_end() {
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let root = repo_root();
    let src_dir = root.join("examples/tinox_ui_annotated_hello");
    let workdir = std::env::temp_dir().join(format!(
        "tinox-ui-annotated-hello-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(&workdir).expect("mkdir workdir");
    let exe = workdir.join("tinox_ui_annotated_hello");

    // tinox.core:ui isn't published to tinox-central yet (issue #215's
    // Phase 6) -- stage the module's own source directly, same approach
    // every other tinox_ui_*.rs test already uses.
    let ui_src_dir = root.join("crates/tinox-core-ext/ui/tinox/core/ui");
    let staged_ui_dir = workdir.join("src/tinox/core/ui");
    std::fs::create_dir_all(&staged_ui_dir).expect("mkdir staged ui dir");
    for entry in std::fs::read_dir(&ui_src_dir).expect("read ui module dir") {
        let entry = entry.expect("dir entry");
        let dest = staged_ui_dir.join(entry.file_name());
        std::fs::copy(entry.path(), dest).expect("copy ui module file");
    }
    for name in ["AnnotatedHelloApp.tnx", "Main.tnx"] {
        std::fs::copy(src_dir.join(name), workdir.join("src").join(name))
            .unwrap_or_else(|e| panic!("copy {name}: {e}"));
    }
    let staged_toml = "[package]\nname = \"tinox_ui_annotated_hello_test\"\nversion = \"0.1.0\"\ndescription = \"\"\n\n\
        [[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"http_server\"\nversion = \"1.0.1\"\n\n\
        [[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"websocket\"\nversion = \"1.0.1\"\n\n\
        [[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"json\"\nversion = \"1.0.0\"\n";
    std::fs::write(workdir.join("tinox.toml"), staged_toml).expect("write staged tinox.toml");

    let install = Command::new(tinox)
        .arg("install")
        .current_dir(&workdir)
        .output()
        .expect("spawn install");
    assert!(
        install.status.success(),
        "tinox install failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&install.stdout),
        String::from_utf8_lossy(&install.stderr)
    );

    let build = Command::new(tinox)
        .arg("build")
        .arg(workdir.join("src/Main.tnx"))
        .arg("-o")
        .arg(&exe)
        .current_dir(&workdir)
        .output()
        .expect("spawn build");
    assert!(
        build.status.success(),
        "build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let child = Command::new(&exe)
        .current_dir(&workdir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server");
    let _guard = KillOnDrop(child);

    // HTTP shell/client-JS server also comes purely from the annotation --
    // confirm both routes work, not just the WS side below.
    let http_port = 8286u16;
    let mut connected = false;
    for _ in 0..50 {
        if TcpStream::connect(("127.0.0.1", http_port)).is_ok() {
            connected = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(connected, "HTTP shell server never came up on :{http_port}");

    let shell = reqwest_like_get(http_port, "/");
    assert!(shell.contains("<!doctype html>") || shell.contains("<!DOCTYPE html>"), "expected shell HTML, got: {shell}");
    let js = reqwest_like_get(http_port, "/ui.js");
    assert!(js.contains("function") || js.contains("=>"), "expected client JS body, got first 200 chars: {}", &js[..js.len().min(200)]);

    // WS port from AnnotatedHelloApp's own @TinoxUIApp(8286, 8287).
    let ws_port = 8287;
    let mut stream = None;
    for _ in 0..50 {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", ws_port)) {
            stream = Some(s);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut stream = stream.expect("connect to AnnotatedHelloApp's WS endpoint");
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();

    let req = "GET /__tinoxui HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n";
    stream.write_all(req.as_bytes()).expect("send handshake");
    let resp = read_handshake_response(&mut stream);
    assert!(resp.contains("101"), "expected 101 response, got: {resp}");

    let init = read_text_frame(&mut stream);
    assert!(init.contains("\"kind\":\"init\""), "expected init message, got: {init}");
    assert!(init.contains("Clicks: 0"), "expected initial click count 0, got: {init}");

    let button_id = find_button_id(&init);
    let event = format!("{{\"kind\":\"event\",\"id\":\"{button_id}\",\"value\":\"\"}}");
    send_masked_text_frame(&mut stream, event.as_bytes());

    // issue #225: @TinoxUIApp now renders via TinoxUIRuntime::diff/
    // sendPatch (stable ids across renders) instead of Phase 1's
    // full-tree resend -- a click sends a "patch" message (an `ops`
    // array), not a whole new "update" tree, and the label/button ids
    // stay the SAME across renders since neither moved position.
    let patch1 = read_text_frame(&mut stream);
    assert!(patch1.contains("\"kind\":\"patch\""), "expected patch message, got: {patch1}");
    assert!(patch1.contains("Clicks: 1"), "expected click count 1 after one click, got: {patch1}");

    // The button's id is stable now -- reusing the SAME id (rather than
    // re-parsing it from the response, which no longer carries a full
    // Button node at all for an unrelated-label-only patch) is itself
    // part of what this test is confirming: a second event dispatch
    // against that stable id still finds its handler after the server
    // re-ran TinoxUIRuntime::collectHandlers.
    let event2 = format!("{{\"kind\":\"event\",\"id\":\"{button_id}\",\"value\":\"\"}}");
    send_masked_text_frame(&mut stream, event2.as_bytes());
    let patch2 = read_text_frame(&mut stream);
    assert!(patch2.contains("\"kind\":\"patch\""), "expected patch message, got: {patch2}");
    assert!(patch2.contains("Clicks: 2"), "expected click count 2 after two clicks, got: {patch2}");

    let _ = std::fs::remove_dir_all(&workdir);
}

/// Minimal blocking GET over a raw TCP socket -- this test crate has no
/// HTTP client dependency, matching the existing tinox_ui_*.rs tests' own
/// "no new deps" convention (they hand-roll the WS handshake the same way).
fn reqwest_like_get(port: u16, path: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect for GET");
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).expect("send GET");
    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).expect("read GET response");
    String::from_utf8_lossy(&resp).into_owned()
}
