//! End-to-end test for the Tinox-UI signup-form example (issue #215,
//! Phase 2): a form combining TextField + Checkbox + conditional Button/
//! success-Label, exercising more of the v1 widget set together than
//! tinox_ui_hello's single button does, and confirming instance-method
//! calls through a captured `this`-alias (not just field mutation) work
//! from inside a nested lambda (SignupApp.build()'s `fn(v) { app.trySubmit(); }`).
//!
//! Drives the real validation flow over the wire: submit empty -> "Name is
//! required", fill name and resubmit -> "You must agree to the terms",
//! check the box and resubmit -> success message, button gone.

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
    let mask = [7u8, 6, 5, 4];
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

/// Finds `"id":"cN"` for the first component of the given `type` in a raw
/// JSON render message -- same small hand-rolled scan tinox_ui_hello.rs
/// uses (no serde_json dependency in this crate).
fn find_id(json: &str, component_type: &str) -> String {
    let marker = format!("\"type\":\"{component_type}\"");
    let type_pos = json.find(&marker).unwrap_or_else(|| panic!("no {component_type} component in render: {json}"));
    let before = &json[..type_pos];
    let id_key = before.rfind("\"id\":\"").expect("no id field before type");
    let after_key = &before[id_key + "\"id\":\"".len()..];
    let end = after_key.find('"').expect("unterminated id string");
    after_key[..end].to_string()
}

#[test]
fn tinox_ui_signup_validation_flow_end_to_end() {
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let root = repo_root();
    let src_dir = root.join("examples/tinox_ui_signup");
    let workdir = std::env::temp_dir().join(format!(
        "tinox-ui-signup-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(&workdir).expect("mkdir workdir");
    let exe = workdir.join("tinox_ui_signup");

    // Same local-staging approach as tinox_ui_hello.rs -- tinox.core:ui
    // isn't published yet (issue #215 Phase 6).
    let ui_src_dir = root.join("crates/tinox-core-ext/ui/tinox/core/ui");
    let staged_ui_dir = workdir.join("src/tinox/core/ui");
    std::fs::create_dir_all(&staged_ui_dir).expect("mkdir staged ui dir");
    for entry in std::fs::read_dir(&ui_src_dir).expect("read ui module dir") {
        let entry = entry.expect("dir entry");
        let dest = staged_ui_dir.join(entry.file_name());
        std::fs::copy(entry.path(), dest).expect("copy ui module file");
    }
    for name in ["SignupApp.tnx", "Main.tnx"] {
        std::fs::copy(src_dir.join(name), workdir.join("src").join(name))
            .unwrap_or_else(|e| panic!("copy {name}: {e}"));
    }
    let staged_toml = "[package]\nname = \"tinox_ui_signup_test\"\nversion = \"0.1.0\"\ndescription = \"\"\n\n\
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

    // Port from SignupApp's own @WebsocketEndpoint("/__tinoxui", 8283).
    let port = 8283;
    let mut stream = None;
    for _ in 0..50 {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", port)) {
            stream = Some(s);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut stream = stream.expect("connect to tinox_ui_signup's WS endpoint");
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();

    let req = "GET /__tinoxui HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n";
    stream.write_all(req.as_bytes()).expect("send handshake");
    let resp = read_handshake_response(&mut stream);
    assert!(resp.contains("101"), "expected 101 response, got: {resp}");

    let init = read_text_frame(&mut stream);
    assert!(init.contains("\"kind\":\"init\""), "expected init message, got: {init}");

    // 1) Submit with an empty name -> validation error.
    let button_id = find_id(&init, "Button");
    send_masked_text_frame(&mut stream, format!("{{\"kind\":\"event\",\"id\":\"{button_id}\",\"value\":\"\"}}").as_bytes());
    let u1 = read_text_frame(&mut stream);
    assert!(u1.contains("Name is required"), "expected name-required error, got: {u1}");

    // 2) Fill in the name, submit again -> agreement error.
    let field_id = find_id(&u1, "TextField");
    send_masked_text_frame(&mut stream, format!("{{\"kind\":\"event\",\"id\":\"{field_id}\",\"value\":\"Ada\"}}").as_bytes());
    let u2 = read_text_frame(&mut stream);
    let button_id2 = find_id(&u2, "Button");
    send_masked_text_frame(&mut stream, format!("{{\"kind\":\"event\",\"id\":\"{button_id2}\",\"value\":\"\"}}").as_bytes());
    let u3 = read_text_frame(&mut stream);
    assert!(u3.contains("You must agree to the terms"), "expected agreement error, got: {u3}");

    // 3) Check the box, submit again -> success, no Button left in the tree.
    let checkbox_id = find_id(&u3, "Checkbox");
    send_masked_text_frame(&mut stream, format!("{{\"kind\":\"event\",\"id\":\"{checkbox_id}\",\"value\":\"true\"}}").as_bytes());
    let u4 = read_text_frame(&mut stream);
    let button_id3 = find_id(&u4, "Button");
    send_masked_text_frame(&mut stream, format!("{{\"kind\":\"event\",\"id\":\"{button_id3}\",\"value\":\"\"}}").as_bytes());
    let u5 = read_text_frame(&mut stream);
    assert!(u5.contains("Thanks, Ada!"), "expected success message, got: {u5}");
    assert!(!u5.contains("\"type\":\"Button\""), "expected the Button to be gone after a successful submit, got: {u5}");

    let _ = std::fs::remove_dir_all(&workdir);
}
