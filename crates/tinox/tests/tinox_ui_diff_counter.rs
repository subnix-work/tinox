//! End-to-end test for the Tinox-UI diff-based rendering example (issue
//! #215, Phase 3): drives the compiled app's actual WebSocket protocol over
//! a raw TCP socket, same low-level approach tinox_ui_hello.rs already
//! uses -- but this example uses TinoxUIRuntime::diff/sendPatch instead of
//! Phase 1's full-tree resend, so this test asserts on the DIFFERENT
//! "patch" message kind and confirms the patch is minimal (exactly one
//! "update" op per click, not a whole new tree) and that the changed
//! node's id stays the SAME across renders (diff() reuses the old id at a
//! matched tree position, unlike full-resend where every id is fresh on
//! every render).

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

/// Pulls the id of the Label whose text starts with "Clicks:" out of the
/// init render's serialized Component tree -- same hand-rolled-scan
/// convention as find_button_id, just keyed on the label's own text
/// instead of on a following sibling's type.
fn find_count_label_id(json: &str) -> String {
    let marker = "\"text\":\"Clicks: ";
    let text_pos = json.find(marker).expect("no 'Clicks: ' label in render");
    let before = &json[..text_pos];
    let id_key = before.rfind("\"id\":\"").expect("no id field before Clicks label's text");
    let after_key = &before[id_key + "\"id\":\"".len()..];
    let end = after_key.find('"').expect("unterminated id string");
    after_key[..end].to_string()
}

/// Counts top-level `"op":"` occurrences in a patch message's `ops` array
/// -- a plain substring count is enough here (no nested `"op":"` values can
/// occur inside a single TinoxUIPatchOp's own fields), avoiding a real JSON
/// parse dependency, matching this test file's existing scan-based style.
fn count_ops(json: &str) -> usize {
    json.matches("\"op\":\"").count()
}

#[test]
fn tinox_ui_diff_counter_end_to_end() {
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let root = repo_root();
    let src_dir = root.join("examples/tinox_ui_diff_counter");
    let workdir = std::env::temp_dir().join(format!(
        "tinox-ui-diff-counter-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(&workdir).expect("mkdir workdir");
    let exe = workdir.join("tinox_ui_diff_counter");

    // tinox.core:ui isn't published to tinox-central yet (issue #215's
    // Phase 6) -- stage the module's own source directly, same approach
    // tinox_ui_hello.rs/tinox_ui_signup.rs already use.
    let ui_src_dir = root.join("crates/tinox-core-ext/ui/tinox/core/ui");
    let staged_ui_dir = workdir.join("src/tinox/core/ui");
    std::fs::create_dir_all(&staged_ui_dir).expect("mkdir staged ui dir");
    for entry in std::fs::read_dir(&ui_src_dir).expect("read ui module dir") {
        let entry = entry.expect("dir entry");
        let dest = staged_ui_dir.join(entry.file_name());
        std::fs::copy(entry.path(), dest).expect("copy ui module file");
    }
    for name in ["DiffCounterApp.tnx", "Main.tnx"] {
        std::fs::copy(src_dir.join(name), workdir.join("src").join(name))
            .unwrap_or_else(|e| panic!("copy {name}: {e}"));
    }
    let staged_toml = "[package]\nname = \"tinox_ui_diff_counter_test\"\nversion = \"0.1.0\"\ndescription = \"\"\n\n\
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

    // Port from DiffCounterApp's own @WebsocketEndpoint("/__tinoxui", 8285).
    let port = 8285;
    let mut stream = None;
    for _ in 0..50 {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", port)) {
            stream = Some(s);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut stream = stream.expect("connect to tinox_ui_diff_counter's WS endpoint");
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();

    let req = "GET /__tinoxui HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n";
    stream.write_all(req.as_bytes()).expect("send handshake");
    let resp = read_handshake_response(&mut stream);
    assert!(resp.contains("101"), "expected 101 response, got: {resp}");

    let init = read_text_frame(&mut stream);
    assert!(init.contains("\"kind\":\"init\""), "expected init message, got: {init}");
    assert!(init.contains("Clicks: 0"), "expected initial click count 0, got: {init}");

    let button_id = find_button_id(&init);
    let count_label_id = find_count_label_id(&init);

    let event = format!("{{\"kind\":\"event\",\"id\":\"{button_id}\",\"value\":\"\"}}");
    send_masked_text_frame(&mut stream, event.as_bytes());

    let patch1 = read_text_frame(&mut stream);
    assert!(patch1.contains("\"kind\":\"patch\""), "expected patch message, got: {patch1}");
    assert_eq!(
        count_ops(&patch1),
        1,
        "expected exactly 1 op (only the count label changed), got: {patch1}"
    );
    assert!(patch1.contains("\"op\":\"update\""), "expected an update op, got: {patch1}");
    assert!(
        patch1.contains(&format!("\"id\":\"{count_label_id}\"")),
        "expected the update op to target the count label's OWN id ({count_label_id}), got: {patch1}"
    );
    assert!(patch1.contains("Clicks: 1"), "expected click count 1 after one click, got: {patch1}");

    // A second click: the count label's id must stay the SAME across
    // renders (diff() reuses the old id at a matched tree position) --
    // unlike full-resend (tinox_ui_hello.rs), where every id is thrown
    // away and rebuilt fresh on every single render.
    let event2 = format!("{{\"kind\":\"event\",\"id\":\"{button_id}\",\"value\":\"\"}}");
    send_masked_text_frame(&mut stream, event2.as_bytes());
    let patch2 = read_text_frame(&mut stream);
    assert_eq!(count_ops(&patch2), 1, "expected exactly 1 op on the second click too, got: {patch2}");
    assert!(
        patch2.contains(&format!("\"id\":\"{count_label_id}\"")),
        "expected the SAME count label id ({count_label_id}) reused on the second click, got: {patch2}"
    );
    assert!(patch2.contains("Clicks: 2"), "expected click count 2 after two clicks, got: {patch2}");

    let _ = std::fs::remove_dir_all(&workdir);
}
