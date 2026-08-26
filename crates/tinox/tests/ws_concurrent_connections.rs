//! Regression test for the @WebsocketEndpoint multi-connection fix
//! (Tinox-UI issue #215, Phase 0): emit_ws_code's original accept loop
//! ran a connection's entire conn_open/msg_loop/conn_end sequence INLINE,
//! so WsServer_accept was never called again until the current connection
//! closed -- a second client could not connect at all while the first was
//! still open. Fixed by spawning each accepted connection onto its own
//! detached worker thread (tinox_task_spawn_detached) so the accept loop
//! goes straight back to accepting.
//!
//! Same "drive a real handshake over a raw TCP socket" approach as
//! ws_annotations.rs, but with TWO connections opened before EITHER sends
//! a message -- the previous, single-connection version of this codegen
//! would have hung forever on the second connection's handshake, since
//! WsServer_accept would never even be called for it.

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
    let mask = [9u8, 8, 7, 6];
    let mut frame = vec![0x81u8, 0x80 | payload.len() as u8];
    frame.extend_from_slice(&mask);
    for (i, b) in payload.iter().enumerate() {
        frame.push(b ^ mask[i % 4]);
    }
    stream.write_all(&frame).expect("send text frame");
}

fn read_text_frame(stream: &mut TcpStream) -> String {
    let hdr = read_n(stream, 2);
    assert_eq!(hdr[0], 0x81, "expected FIN+text opcode byte");
    let plen = (hdr[1] & 0x7f) as usize;
    let body = read_n(stream, plen);
    String::from_utf8_lossy(&body).into_owned()
}

#[test]
fn ws_annotated_endpoint_serves_two_concurrent_connections() {
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let root = repo_root();
    let src = root.join("examples/ws_echo_annotated/Main.tnx");
    let workdir = std::env::temp_dir().join(format!(
        "tinox-ws-concurrent-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(&workdir).expect("mkdir workdir");
    let exe = workdir.join("EchoEndpoint");

    let install = Command::new(tinox)
        .arg("install")
        .current_dir(src.parent().expect("src has a parent dir"))
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
        .arg(&src)
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

    let port = 8793;
    let connect = || -> TcpStream {
        let mut stream = None;
        for _ in 0..50 {
            if let Ok(s) = TcpStream::connect(("127.0.0.1", port)) {
                stream = Some(s);
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let s = stream.expect("connect to annotated WS server");
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s
    };

    // Open connection A and complete its handshake, but do NOT send a
    // message yet -- keep it open and idle in msg_loop, exactly the state
    // that used to block WsServer_accept from ever being called again.
    let req = "GET /echo HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n";
    let mut a = connect();
    a.write_all(req.as_bytes()).expect("send handshake A");
    let resp_a = read_handshake_response(&mut a);
    assert!(resp_a.contains("101"), "connection A: expected 101 response, got: {resp_a}");

    // Now open connection B WHILE A is still open. With the old,
    // single-connection-at-a-time accept loop this would hang until A
    // closed (and this test would fail on TcpStream::connect's own
    // retry budget above, or on the read timeout below).
    let mut b = connect();
    b.write_all(req.as_bytes()).expect("send handshake B");
    let resp_b = read_handshake_response(&mut b);
    assert!(resp_b.contains("101"), "connection B: expected 101 response, got: {resp_b}");

    // Both connections independently echo their own, DIFFERENT payloads --
    // proves they're not somehow sharing one server-side loop/instance.
    send_masked_text_frame(&mut a, b"from-a");
    send_masked_text_frame(&mut b, b"from-b");
    let echoed_a = read_text_frame(&mut a);
    let echoed_b = read_text_frame(&mut b);
    assert_eq!(echoed_a, "echo: from-a", "connection A got the wrong echo");
    assert_eq!(echoed_b, "echo: from-b", "connection B got the wrong echo");

    // A second round-trip on A, confirming it's still alive and correctly
    // routed after B connected and exchanged its own message in between.
    send_masked_text_frame(&mut a, b"from-a-again");
    let echoed_a2 = read_text_frame(&mut a);
    assert_eq!(echoed_a2, "echo: from-a-again", "connection A's second message got the wrong echo");

    let _ = std::fs::remove_dir_all(&workdir);
}
