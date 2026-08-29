//! End-to-end test for the Tinox-UI widget-library showcase example
//! (issue #215 follow-up): DataGrid<T> (generic, DTO-driven), DatePicker,
//! RadioGroup, FileUpload, Tabs, Dialog, Notification, Accordion, all
//! exercised together in one app built on Phase 4's @TinoxUIApp/@View
//! sugar. Drives the real WebSocket protocol over a raw TCP socket, same
//! low-level approach every other tinox_ui_*.rs test already uses --
//! plain substring/scan checks rather than a real JSON parse (no
//! serde_json dependency in this crate, matching the existing
//! tinox_ui_hello.rs/tinox_ui_routed_demo.rs convention).

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

/// Finds the `"id":"cN"` immediately preceding the Nth (0-indexed)
/// occurrence of a given `"type":"X"` marker -- same backward-scan
/// technique tinox_ui_routed_demo.rs's find_id_before_type already uses.
fn find_id_before_type(json: &str, type_name: &str, occurrence: usize) -> String {
    let marker = format!("\"type\":\"{type_name}\"");
    let mut search_from = 0;
    let mut type_pos = None;
    for _ in 0..=occurrence {
        let pos = json[search_from..]
            .find(&marker)
            .unwrap_or_else(|| panic!("no (further) {type_name} component in render (json: {json})"));
        type_pos = Some(search_from + pos);
        search_from = search_from + pos + marker.len();
    }
    let type_pos = type_pos.unwrap();
    let before = &json[..type_pos];
    let id_key = before.rfind("\"id\":\"").expect("no id field before type");
    let after_key = &before[id_key + "\"id\":\"".len()..];
    let end = after_key.find('"').expect("unterminated id string");
    after_key[..end].to_string()
}

#[test]
fn tinox_ui_widgets_showcase_end_to_end() {
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let root = repo_root();
    let src_dir = root.join("examples/tinox_ui_widgets_showcase");
    let workdir = std::env::temp_dir().join(format!(
        "tinox-ui-widgets-showcase-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(&workdir).expect("mkdir workdir");
    let exe = workdir.join("tinox_ui_widgets_showcase");

    let ui_src_dir = root.join("crates/tinox-core-ext/ui/tinox/core/ui");
    let staged_ui_dir = workdir.join("src/tinox/core/ui");
    std::fs::create_dir_all(&staged_ui_dir).expect("mkdir staged ui dir");
    for entry in std::fs::read_dir(&ui_src_dir).expect("read ui module dir") {
        let entry = entry.expect("dir entry");
        let dest = staged_ui_dir.join(entry.file_name());
        std::fs::copy(entry.path(), dest).expect("copy ui module file");
    }
    for name in ["Person.tnx", "ShowcaseApp.tnx", "Main.tnx"] {
        std::fs::copy(src_dir.join(name), workdir.join("src").join(name))
            .unwrap_or_else(|e| panic!("copy {name}: {e}"));
    }
    let staged_toml = "[package]\nname = \"tinox_ui_widgets_showcase_test\"\nversion = \"0.1.0\"\ndescription = \"\"\n\n\
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

    // Port from ShowcaseApp's own @TinoxUIApp(8290, 8291).
    let ws_port = 8291;
    let mut stream = None;
    for _ in 0..50 {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", ws_port)) {
            stream = Some(s);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut stream = stream.expect("connect to ShowcaseApp's WS endpoint");
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();

    let req = "GET /__tinoxui HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n";
    stream.write_all(req.as_bytes()).expect("send handshake");
    let resp = read_handshake_response(&mut stream);
    assert!(resp.contains("101"), "expected 101 response, got: {resp}");

    let init = read_text_frame(&mut stream);
    assert!(init.contains("\"kind\":\"init\""), "expected init message, got: {init}");

    // DataGrid<Person>: header + 2 real DTO rows.
    assert!(init.contains("\"type\":\"DataGrid\""), "expected a DataGrid, got: {init}");
    assert!(init.contains("\"text\":\"Name\"") && init.contains("\"text\":\"Age\""), "expected grid headers, got: {init}");
    assert!(init.contains("\"text\":\"Ada\"") && init.contains("\"text\":\"30\""), "expected Ada/30 row, got: {init}");
    assert!(init.contains("\"text\":\"Bob\"") && init.contains("\"text\":\"40\""), "expected Bob/40 row, got: {init}");

    // Dialog closed initially, Notification present.
    assert!(init.contains("\"type\":\"Dialog\""), "expected a Dialog, got: {init}");
    assert!(init.contains("\"open\":\"false\""), "expected dialog closed initially, got: {init}");
    assert!(init.contains("\"type\":\"Notification\"") && init.contains("Loaded 2 people"), "expected a Notification, got: {init}");

    // issue #225: @TinoxUIApp now renders via TinoxUIRuntime::diff/
    // sendPatch (stable ids across renders) instead of Phase 1's
    // full-tree resend. A "patch" message only carries the ops that
    // actually changed -- an `update` op on an existing node (e.g. the
    // Dialog's `open` prop flipping) does NOT re-include that node's own
    // `"type"` field, only `id`/`text`/`props` -- so every id this test
    // needs is captured ONCE from the initial full-tree `init` message
    // (ids are stable now, that's the whole point) rather than re-parsed
    // out of each response the way the old full-resend version of this
    // test did. A `replace`/`insert` op (a genuinely NEW subtree, e.g.
    // switching to the Form tab) DOES carry a full `node` with real
    // `"type"` markers for everything newly appeared at that position --
    // confirmed by actually running this example against the diff-based
    // renderer and inspecting the real wire messages before writing the
    // assertions below, not guessed from the protocol docs alone.

    // Click "Open Dialog" (the render tree's only Button).
    let open_btn_id = find_id_before_type(&init, "Button", 0);
    let dialog_id = find_id_before_type(&init, "Dialog", 0);
    let tabs_id = find_id_before_type(&init, "Tabs", 0);

    let ev1 = format!("{{\"kind\":\"event\",\"id\":\"{open_btn_id}\",\"value\":\"\"}}");
    send_masked_text_frame(&mut stream, ev1.as_bytes());
    let patch1 = read_text_frame(&mut stream);
    assert!(patch1.contains("\"kind\":\"patch\""), "expected patch message, got: {patch1}");
    assert!(patch1.contains("\"open\":\"true\""), "expected dialog open after button click, got: {patch1}");

    // Close it via the Dialog's own (stable) id (its onEvent = onClose).
    let ev2 = format!("{{\"kind\":\"event\",\"id\":\"{dialog_id}\",\"value\":\"\"}}");
    send_masked_text_frame(&mut stream, ev2.as_bytes());
    let patch2 = read_text_frame(&mut stream);
    assert!(patch2.contains("\"kind\":\"patch\""), "expected patch message, got: {patch2}");
    assert!(patch2.contains("\"open\":\"false\""), "expected dialog closed again, got: {patch2}");

    // Switch to the Form tab (index 1) via the Tabs component's (stable)
    // id -- this swaps the DataGrid subtree out for an entirely new one,
    // so unlike the two ops above, this DOES arrive as a `replace` op
    // carrying a full `node` (with real `"type"` markers) for everything
    // that just appeared.
    let ev3 = format!("{{\"kind\":\"event\",\"id\":\"{tabs_id}\",\"value\":\"1\"}}");
    send_masked_text_frame(&mut stream, ev3.as_bytes());
    let patch3 = read_text_frame(&mut stream);
    assert!(patch3.contains("\"kind\":\"patch\""), "expected patch message, got: {patch3}");
    assert!(patch3.contains("\"active\":\"1\""), "expected active tab 1, got: {patch3}");
    assert!(patch3.contains("\"type\":\"DatePicker\""), "expected DatePicker on Form tab, got: {patch3}");
    assert!(patch3.contains("\"type\":\"RadioGroup\""), "expected RadioGroup on Form tab, got: {patch3}");
    assert!(patch3.contains("\"type\":\"FileUpload\""), "expected FileUpload on Form tab, got: {patch3}");
    assert!(patch3.contains("\"type\":\"Accordion\"") && patch3.contains("\"type\":\"AccordionSection\""), "expected Accordion, got: {patch3}");
    assert!(patch3.contains("red|green|blue"), "expected RadioGroup options, got: {patch3}");

    // Simulate a file selection -- FileUpload is one of the freshly
    // inserted nodes above, so its id only exists from here on (it
    // wasn't part of `init`, unlike open_btn_id/dialog_id/tabs_id).
    let file_id = find_id_before_type(&patch3, "FileUpload", 0);
    let ev4 = format!("{{\"kind\":\"event\",\"id\":\"{file_id}\",\"value\":\"report.pdf\"}}");
    send_masked_text_frame(&mut stream, ev4.as_bytes());
    let upd4 = read_text_frame(&mut stream);
    assert!(upd4.contains("File: report.pdf"), "expected uploaded filename reported, got: {upd4}");

    let _ = std::fs::remove_dir_all(&workdir);
}
