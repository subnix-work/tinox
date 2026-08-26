//! End-to-end test for the Tinox-UI "hello world" example (issue #215,
//! Phase 1 MVP): drives the compiled app's actual WebSocket protocol over
//! a raw TCP socket (same low-level approach ws_annotations.rs already
//! uses for the RFC 6455 handshake) -- confirms the init render shows
//! "Clicks: 0", and that sending a click event produces an update render
//! showing "Clicks: 1", i.e. the whole component-tree build / id-
//! assignment / @DoNotSerialize-excluded-handler dispatch / automatic-
//! re-render pipeline works end to end against the real compiled binary,
//! not just in isolation.
//!
//! Does NOT verify the client-side ui.js DOM rendering (that needs a
//! real browser -- see docs/tinox-ui/PLAN.md's own testing-strategy
//! section) -- this only exercises the server side of the protocol.

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
    // Payloads here are always short (small JSON event messages), so the
    // single-byte length form is enough for the client->server direction.
    assert!(payload.len() < 126, "test event payload unexpectedly large");
    frame.push(0x80 | payload.len() as u8);
    frame.extend_from_slice(&mask);
    for (i, b) in payload.iter().enumerate() {
        frame.push(b ^ mask[i % 4]);
    }
    stream.write_all(&frame).expect("send text frame");
}

/// Reads one unmasked (server->client) text frame, handling the extended
/// 16-bit length form (0x7E marker) -- unlike the small fixed strings the
/// existing ws_*.rs tests exchange, Tinox-UI's init/update payloads are a
/// full serialized Component tree and comfortably exceed the 125-byte
/// single-byte length encoding.
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

/// Pulls `"id":"cN"` for the first Button component out of a raw JSON
/// render message -- deliberately a small hand-rolled scan (no serde_json
/// dependency in this crate) rather than a real JSON parse, matching the
/// existing ws_*.rs tests' own "no new deps, simple substring/scan
/// checks" convention.
fn find_button_id(json: &str) -> String {
    let marker = "\"type\":\"Button\"";
    let type_pos = json.find(marker).expect("no Button component in render");
    // The id field is emitted before type in Component's own field order
    // (id, type, text, ...) -- scan backward from the Button's "type" key
    // for the nearest preceding "id":"...".
    let before = &json[..type_pos];
    let id_key = before.rfind("\"id\":\"").expect("no id field before Button's type");
    let after_key = &before[id_key + "\"id\":\"".len()..];
    let end = after_key.find('"').expect("unterminated id string");
    after_key[..end].to_string()
}

#[test]
fn tinox_ui_hello_click_counter_end_to_end() {
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let root = repo_root();
    let src_dir = root.join("examples/tinox_ui_hello");
    let workdir = std::env::temp_dir().join(format!(
        "tinox-ui-hello-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(&workdir).expect("mkdir workdir");
    let exe = workdir.join("tinox_ui_hello");

    // tinox.core:ui isn't published to tinox-central yet (issue #215's
    // Phase 6) -- `tinox install` can't fetch it. Stage the module's own
    // source directly into workdir/src/tinox/core/ui/ instead, alongside
    // copying HelloApp.tnx/Main.tnx there too, and install ONLY the
    // already-published deps (http_server/websocket/json) for real. Once
    // Phase 6 publishes `ui`, this keeps working unchanged -- local src/
    // resolution takes priority regardless, so the test stays hermetic
    // either way rather than depending on publish order or network
    // access to tinox-central.
    let ui_src_dir = root.join("crates/tinox-core-ext/ui/tinox/core/ui");
    let staged_ui_dir = workdir.join("src/tinox/core/ui");
    std::fs::create_dir_all(&staged_ui_dir).expect("mkdir staged ui dir");
    for entry in std::fs::read_dir(&ui_src_dir).expect("read ui module dir") {
        let entry = entry.expect("dir entry");
        let dest = staged_ui_dir.join(entry.file_name());
        std::fs::copy(entry.path(), dest).expect("copy ui module file");
    }
    for name in ["HelloApp.tnx", "Main.tnx"] {
        std::fs::copy(src_dir.join(name), workdir.join("src").join(name))
            .unwrap_or_else(|e| panic!("copy {name}: {e}"));
    }
    // Deliberately NOT parsed out of the example's own tinox.toml (fragile
    // to do reliably with plain text munging) -- just the same three
    // already-published deps written directly, kept in sync by hand with
    // examples/tinox_ui_hello/tinox.toml.
    let staged_toml = "[package]\nname = \"tinox_ui_hello_test\"\nversion = \"0.1.0\"\ndescription = \"\"\n\n\
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

    // Port from HelloApp's own @WebsocketEndpoint("/__tinoxui", 8281).
    let port = 8281;
    let mut stream = None;
    for _ in 0..50 {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", port)) {
            stream = Some(s);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut stream = stream.expect("connect to tinox_ui_hello's WS endpoint");
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

    let update1 = read_text_frame(&mut stream);
    assert!(update1.contains("\"kind\":\"update\""), "expected update message, got: {update1}");
    assert!(update1.contains("Clicks: 1"), "expected click count 1 after one click, got: {update1}");

    // A second click, re-reading the button's id from the update (rather
    // than reusing button_id) -- this example's tree shape never changes
    // between renders so the id happens to stay "c3" every time either
    // way, but re-reading it here matches how a real client always would
    // (ids are NOT guaranteed stable across renders in general, see
    // Component.tnx's own doc comment) and confirms handleEvent is using
    // each render's own freshly-rebuilt handler map, not a stale one.
    let button_id2 = find_button_id(&update1);
    let event2 = format!("{{\"kind\":\"event\",\"id\":\"{button_id2}\",\"value\":\"\"}}");
    send_masked_text_frame(&mut stream, event2.as_bytes());
    let update2 = read_text_frame(&mut stream);
    assert!(update2.contains("Clicks: 2"), "expected click count 2 after two clicks, got: {update2}");

    let _ = std::fs::remove_dir_all(&workdir);
}
