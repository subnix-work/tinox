//! #560: opt-in permessage-deflate context takeover (`tinox.websocket.compression = context`),
//! verified against an independent client (the Python `websockets` library): negotiation of the
//! client's parameters, byte-exact echoes over many messages on one context, a clearly smaller wire
//! size than the stateless default, state released again after many short connections, and a hard
//! error for an unknown configuration value.

use std::io::Read;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const CLIENT: &str = r#"
import asyncio, json, random, sys
import websockets

port, mode = int(sys.argv[1]), sys.argv[2]
random.seed(560)
WORDS = "the quick brown fox jumps over lazy dog build test deploy service request response".split()
def msg():
    return json.dumps({"user": "u%d" % random.randint(1, 40), "text": " ".join(random.choice(WORDS) for _ in range(random.randint(4, 14)))})

async def counted(extensions):
    reader, writer = None, None
    # byte-counting proxy between client and server
    counts = {"down": 0}
    async def pipe(r, w, count):
        try:
            while data := await r.read(65536):
                if count: counts["down"] += len(data)
                w.write(data); await w.drain()
        except ConnectionError:
            pass
        finally:
            w.close()
    async def handle(cr, cw):
        sr, sw = await asyncio.open_connection("127.0.0.1", port)
        await asyncio.gather(pipe(cr, sw, False), pipe(sr, cw, True))
    proxy = await asyncio.start_server(handle, "127.0.0.1", 0)
    pport = proxy.sockets[0].getsockname()[1]
    async with websockets.connect("ws://127.0.0.1:%d/echo" % pport, extensions=extensions) as ws:
        neg = ws.response.headers.get("Sec-WebSocket-Extensions", "")
        ok = True
        for _ in range(300):
            m = msg()
            await ws.send(m)
            ok &= (await ws.recv()) == m
        # a large message (> 64 KiB window) and an empty one on the same context
        big = "x" * 100000 + msg()
        await ws.send(big); ok &= (await ws.recv()) == big
        await ws.send(""); ok &= (await ws.recv()) == ""
    proxy.close(); await proxy.wait_closed()
    return neg, ok, counts["down"]

async def main():
    from websockets.extensions.permessage_deflate import ClientPerMessageDeflateFactory as F
    if mode == "defaults":
        neg, ok, down = await counted([F()])
        print(json.dumps({"neg": neg, "ok": ok, "down": down}))
    elif mode == "params":
        out = []
        for kw in ({"client_no_context_takeover": True}, {"server_no_context_takeover": True}, {"server_max_window_bits": 10},
                   {"client_no_context_takeover": True, "server_no_context_takeover": True, "server_max_window_bits": 9}):
            neg, ok, down = await counted([F(**kw)])
            out.append({"offer": kw, "neg": neg, "ok": ok})
        print(json.dumps(out))
    elif mode == "churn":
        for _ in range(150):
            async with websockets.connect("ws://127.0.0.1:%d/echo" % port, extensions=[F()]) as ws:
                for _ in range(5):
                    m = msg(); await ws.send(m); assert (await ws.recv()) == m
        print("ok")
asyncio.run(main())
"#;

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn python_has_websockets() -> bool {
    Command::new("python3").args(["-I", "-c", "import websockets"]).output().map(|o| o.status.success()).unwrap_or(false)
}

// The annotation's port comes from TINOX_PORT at COMPILE time, so each server is built for its own port.
fn build_server(dir: &PathBuf, port: u16) -> PathBuf {
    let ext = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tinox-core-ext/websocket").canonicalize().unwrap();
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("tinox.toml"),
        format!(
            "[package]\ncompiler = \"{}\"\nname = \"ctx\"\nversion = \"0.0.0\"\n\n[startup]\nbanner = false\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"websocket\"\nversion = \"1.1.6\"\n\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"websocket\"\npath = \"{}\"\n",
            env!("CARGO_PKG_VERSION"),
            ext.display()
        ),
    )
    .unwrap();
    std::fs::write(dir.join("Main.tnx"), "import EchoEndpoint\n\nclass Main\n{\n    fnc main() -> Int32\n    {\n        return 0\n    }\n}\n").unwrap();
    std::fs::write(
        dir.join("EchoEndpoint.tnx"),
        "import tinox.core.websocket\n\n@WebsocketEndpoint(\"/echo\")\nclass EchoEndpoint\n{\n    @OnOpen\n    fn onOpen(conn: Int64) -> Nothing\n    {\n    }\n\n    @OnMessage\n    fn onMessage(conn: Int64, msg: String) -> Nothing\n    {\n        Ws::sendText(conn, msg)\n    }\n\n    @OnClose\n    fn onClose(conn: Int64) -> Nothing\n    {\n    }\n}\n",
    )
    .unwrap();
    let exe = dir.join("server");
    let out = Command::new(env!("CARGO_BIN_EXE_tinox")).current_dir(dir).env("TINOX_PORT", port.to_string()).args(["build", "Main.tnx", "-o"]).arg(&exe).output().unwrap();
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    exe
}

/// Kills the server when a failing assertion unwinds, so no stray process keeps the port.
struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl std::ops::Deref for Server {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}

fn start(exe: &PathBuf, port: u16, mode: Option<&str>) -> Server {
    let mut cmd = Command::new(exe);
    cmd.stdout(Stdio::null()).stderr(Stdio::piped());
    if let Some(m) = mode {
        cmd.env("TINOX_WEBSOCKET_COMPRESSION", m);
    }
    let child = cmd.spawn().unwrap();
    let child = Server(child);
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return child;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("server did not start");
}

fn client(port: u16, mode: &str) -> String {
    let out = Command::new("python3").args(["-I", "-c", CLIENT, &port.to_string(), mode]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

fn rss_kib(pid: u32) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    status.lines().find_map(|l| l.strip_prefix("VmRSS:")).unwrap().trim().trim_end_matches("kB").trim().parse().unwrap()
}

#[test]
fn context_takeover_negotiates_shrinks_the_wire_and_releases_its_state() {
    if !python_has_websockets() {
        eprintln!("SKIP: python3 with the websockets package is not available");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tinox_ws_ctx_{}", std::process::id()));
    let port = free_port();
    let exe = build_server(&dir, port);

    // stateless default: unchanged negotiation, baseline size
    let server = start(&exe, port, None);
    let base: serde_json::Value = serde_json::from_str(&client(port, "defaults")).unwrap();
    drop(server);
    assert_eq!(base["ok"], true);
    assert_eq!(base["neg"], "permessage-deflate; client_no_context_takeover; server_no_context_takeover");

    // context takeover: no forced parameters, much smaller, still byte-exact
    let server = start(&exe, port, Some("context"));
    let ctx: serde_json::Value = serde_json::from_str(&client(port, "defaults")).unwrap();
    assert_eq!(ctx["ok"], true);
    assert_eq!(ctx["neg"], "permessage-deflate");
    let (b, c) = (base["down"].as_u64().unwrap(), ctx["down"].as_u64().unwrap());
    assert!(c * 2 < b, "context takeover should at least halve the wire bytes: {c} vs {b}");

    // the client's parameters are honoured and echoed, every message stays byte-exact
    let params: Vec<serde_json::Value> = serde_json::from_str(&client(port, "params")).unwrap();
    let neg: Vec<&str> = params.iter().map(|p| p["neg"].as_str().unwrap()).collect();
    assert!(params.iter().all(|p| p["ok"] == true), "{params:?}");
    assert_eq!(neg[0], "permessage-deflate; client_no_context_takeover");
    assert_eq!(neg[1], "permessage-deflate; server_no_context_takeover");
    assert_eq!(neg[2], "permessage-deflate; server_max_window_bits=10");
    assert_eq!(neg[3], "permessage-deflate; client_no_context_takeover; server_no_context_takeover; server_max_window_bits=9");

    // state is released when a connection closes: 150 connections must not accumulate ~99 KiB each
    let _ = client(port, "churn"); // warm up allocator
    let before = rss_kib(server.id());
    assert_eq!(client(port, "churn").trim(), "ok");
    let after = rss_kib(server.id());
    assert!(after < before + 8192, "RSS grew from {before} to {after} KiB over 150 closed connections");
    drop(server);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unknown_compression_mode_is_a_hard_startup_error() {
    let dir = std::env::temp_dir().join(format!("tinox_ws_ctx_bad_{}", std::process::id()));
    if !python_has_websockets() {
        eprintln!("SKIP: python3 with the websockets package is not available");
        return;
    }
    let port = free_port();
    let exe = build_server(&dir, port);
    let mut cmd = Command::new(&exe);
    cmd.env("TINOX_WEBSOCKET_COMPRESSION", "bogus").stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    // the configuration is read on the first handshake
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = Command::new("python3")
        .args(["-I", "-c", "import asyncio, websockets, sys\nasync def m():\n    try:\n        async with websockets.connect('ws://127.0.0.1:%d/echo' % int(sys.argv[1])) as w: await w.send('x'); await w.recv()\n    except Exception: pass\nasyncio.run(m())", &port.to_string()])
        .output();
    let status = child.wait().unwrap();
    let mut err = String::new();
    child.stderr.take().unwrap().read_to_string(&mut err).unwrap();
    assert!(!status.success(), "server must exit with an error");
    assert!(err.contains("tinox.websocket.compression must be 'stateless' or 'context'"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}
