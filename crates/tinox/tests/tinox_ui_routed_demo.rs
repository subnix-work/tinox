//! End-to-end test for the Tinox-UI Phase 5 example (issue #215): the
//! expanded widget library (heading/numberField/dropdown/progressBar/
//! spacer/textArea) and client-side routing (Component::link), both
//! built on top of Phase 4's @TinoxUIApp/@View annotation sugar. Also
//! serves as the live regression test for issue #218 (a closure-call
//! codegen bug that made calling a captured `fnc(Float64) -> Nothing`
//! closure from a lambda body generate invalid LLVM IR) -- the
//! numberField assertions below specifically exercise that exact shape.

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

/// Finds the `"id":"cN"` immediately preceding a given `"type":"X"`
/// marker's Nth occurrence (0-indexed) -- same backward-scan technique
/// tinox_ui_hello.rs's find_button_id already uses, generalized to
/// support more than one component of the same type (this example has
/// two Link components).
fn find_id_before_type(json: &str, type_name: &str, occurrence: usize) -> String {
    let marker = format!("\"type\":\"{type_name}\"");
    let mut search_from = 0;
    let mut type_pos = None;
    for _ in 0..=occurrence {
        let pos = json[search_from..].find(&marker).unwrap_or_else(|| {
            panic!("no (further) {type_name} component in render (json: {json})")
        });
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
fn tinox_ui_routed_demo_widgets_and_routing_end_to_end() {
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let root = repo_root();
    let src_dir = root.join("examples/tinox_ui_routed_demo");
    let workdir = std::env::temp_dir().join(format!(
        "tinox-ui-routed-demo-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(&workdir).expect("mkdir workdir");
    let exe = workdir.join("tinox_ui_routed_demo");

    let ui_src_dir = root.join("crates/tinox-core-ext/ui/tinox/core/ui");
    let staged_ui_dir = workdir.join("src/tinox/core/ui");
    std::fs::create_dir_all(&staged_ui_dir).expect("mkdir staged ui dir");
    for entry in std::fs::read_dir(&ui_src_dir).expect("read ui module dir") {
        let entry = entry.expect("dir entry");
        let dest = staged_ui_dir.join(entry.file_name());
        std::fs::copy(entry.path(), dest).expect("copy ui module file");
    }
    for name in ["RoutedDemoApp.tnx", "Main.tnx"] {
        std::fs::copy(src_dir.join(name), workdir.join("src").join(name))
            .unwrap_or_else(|e| panic!("copy {name}: {e}"));
    }
    let staged_toml = "[package]\nname = \"tinox_ui_routed_demo_test\"\nversion = \"0.1.0\"\ndescription = \"\"\n\n\
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

    // Port from RoutedDemoApp's own @TinoxUIApp(8288, 8289).
    let ws_port = 8289;
    let mut stream = None;
    for _ in 0..50 {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", ws_port)) {
            stream = Some(s);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut stream = stream.expect("connect to RoutedDemoApp's WS endpoint");
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();

    let req = "GET /__tinoxui HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n";
    stream.write_all(req.as_bytes()).expect("send handshake");
    let resp = read_handshake_response(&mut stream);
    assert!(resp.contains("101"), "expected 101 response, got: {resp}");

    let init = read_text_frame(&mut stream);
    assert!(init.contains("\"kind\":\"init\""), "expected init message, got: {init}");
    assert!(init.contains("\"type\":\"Heading\""), "expected a Heading widget, got: {init}");
    assert!(init.contains("\"type\":\"NumberField\""), "expected a NumberField widget, got: {init}");
    assert!(init.contains("\"type\":\"Select\""), "expected a Select (dropdown) widget, got: {init}");
    assert!(init.contains("\"type\":\"ProgressBar\""), "expected a ProgressBar widget, got: {init}");
    assert!(init.contains("\"options\":\"red|green|blue\""), "expected dropdown options, got: {init}");

    // Set the number field to 42 -- exercises the exact closure shape
    // fixed by issue #218 (Component::numberField wraps a
    // fnc(Float64)->Nothing closure).
    let numfield_id = find_id_before_type(&init, "NumberField", 0);
    let ev1 = format!("{{\"kind\":\"event\",\"id\":\"{numfield_id}\",\"value\":\"42\"}}");
    send_masked_text_frame(&mut stream, ev1.as_bytes());
    let upd1 = read_text_frame(&mut stream);
    assert!(upd1.contains("Quantity: 42"), "expected quantity 42 after setting numberField, got: {upd1}");
    assert!(upd1.contains("\"value\":\"42\""), "expected ProgressBar value 42, got: {upd1}");

    // Navigate to About via the second Link ("About", index 1).
    let about_link_id = find_id_before_type(&upd1, "Link", 1);
    let ev2 = format!("{{\"kind\":\"event\",\"id\":\"{about_link_id}\",\"value\":\"/about\"}}");
    send_masked_text_frame(&mut stream, ev2.as_bytes());
    let upd2 = read_text_frame(&mut stream);
    assert!(upd2.contains("\"text\":\"About\""), "expected the About view, got: {upd2}");
    assert!(!upd2.contains("\"type\":\"NumberField\""), "About view should not contain the home view's widgets, got: {upd2}");

    // Navigate back Home via the first Link ("Home", index 0) -- state
    // (quantity=42) must have survived the round trip.
    let home_link_id = find_id_before_type(&upd2, "Link", 0);
    let ev3 = format!("{{\"kind\":\"event\",\"id\":\"{home_link_id}\",\"value\":\"/\"}}");
    send_masked_text_frame(&mut stream, ev3.as_bytes());
    let upd3 = read_text_frame(&mut stream);
    assert!(upd3.contains("Quantity: 42"), "expected quantity to survive the About/Home round trip, got: {upd3}");

    let _ = std::fs::remove_dir_all(&workdir);
}
