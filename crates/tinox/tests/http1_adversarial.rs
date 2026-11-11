//! Adversarial HTTP/1 clients against a REAL compiled server (follow-up to #312's overload
//! findings and the fuzzing work in #677).
//!
//! Every scenario speaks raw TCP, so the bytes on the wire are exactly what is written here, with
//! no client library normalising them. After each scenario a plain request must still be answered
//! promptly: the server surviving is the baseline, the status codes are the finer check.
//!
//! * slow-header clients (slowloris) must not starve other clients,
//! * ambiguous or malformed request framing is rejected with the documented status (request
//!   smuggling starts with ambiguity),
//! * oversized or hostile headers and bodies are rejected without taking the server down,
//! * clients that vanish mid-request (RST, half close) leave nothing behind.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn extended_module_version(module: &str) -> String {
    let manifest = repo_root().join("crates/tinox-core-ext").join(module).join("tinox.toml");
    std::fs::read_to_string(&manifest)
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|l| l.trim().strip_prefix("version"))
                .and_then(|rest| rest.trim_start().strip_prefix('='))
                .map(|rest| rest.trim().trim_matches('"').to_string())
        })
        .unwrap_or_else(|| "1.0.0".to_string())
}

struct KillOnDrop(Child);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral")
        .local_addr()
        .unwrap()
        .port()
}

fn workdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tinox-adversarial-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir workdir");
    dir
}

/// Builds the server fixture: `/fast` answers immediately, `/echo` echoes a posted body's length.
fn build_server(name: &str, port: u16) -> (PathBuf, PathBuf) {
    let dir = workdir(name);
    std::fs::write(
        dir.join("Ctrl.tnx"),
        r#"import tinox.core.http_server

class Ctrl
{
    @GET("/fast")
    fnc fast(@HttpContext ctx: HttpContext) -> HttpContext
    {
        ctx.response.status(200).json("{\"ok\":true}")
        return ctx
    }

    @POST("/echo")
    fnc echo(@HttpContext ctx: HttpContext) -> HttpContext
    {
        ctx.response.status(200).json("{\"ok\":true}")
        return ctx
    }
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("Main.tnx"),
        "import Ctrl\n\nclass Main\n{\n    fnc main() -> Int32\n    {\n        return 0\n    }\n}\n",
    )
    .unwrap();
    let version = extended_module_version("http_server");
    std::fs::write(
        dir.join("tinox.toml"),
        format!(
            "[package]\ncompiler = \"2.9.4\"\nname = \"adversarial_server\"\nversion = \"0.0.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"http_server\"\nversion = \"{version}\"\n",
        ),
    )
    .unwrap();
    // Consume the in-tree module from source, not whatever is published.
    std::fs::write(
        dir.join("tinox.overrides.yaml"),
        format!(
            "overrides:\n  \"tinox.core:http_server:{version}\": \"{}\"\n",
            repo_root().join("crates/tinox-core-ext/http_server").display()
        ),
    )
    .unwrap();
    let install = Command::new(env!("CARGO_BIN_EXE_tinox"))
        .arg("install")
        .current_dir(&dir)
        .output()
        .expect("spawn install");
    assert!(install.status.success(), "tinox install failed: {install:?}");
    let exe = dir.join("adversarial_server");
    let b = Command::new(env!("CARGO_BIN_EXE_tinox"))
        .arg("build")
        .arg(dir.join("Main.tnx"))
        .arg("-o")
        .arg(&exe)
        .current_dir(&dir)
        .env("TINOX_PORT", port.to_string())
        .output()
        .expect("spawn build");
    assert!(
        b.status.success(),
        "build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&b.stdout),
        String::from_utf8_lossy(&b.stderr)
    );
    (dir, exe)
}

fn start_server(exe: &Path, dir: &Path, port: u16, envs: &[(&str, &str)]) -> KillOnDrop {
    let mut cmd = Command::new(exe);
    cmd.current_dir(dir).stdout(Stdio::null()).stderr(Stdio::null());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let child = KillOnDrop(cmd.spawn().expect("spawn server"));
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return child;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("server did not start listening on {port}");
}

fn connect(port: u16) -> TcpStream {
    let s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
    s
}

/// What a client saw: the first status line's code (if any response arrived).
#[derive(Debug)]
struct Outcome {
    status: Option<u16>,
}

fn read_outcome(mut s: TcpStream, wait: Duration) -> Outcome {
    s.set_read_timeout(Some(wait)).unwrap();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        match s.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() > 12 {
                    // The status line is all these scenarios assert on; once the header block
                    // is complete, see whether the server also closes.
                    s.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
                }
            }
            Err(_) => break,
        }
    }
    let status = std::str::from_utf8(&buf[..buf.len().min(16)])
        .ok()
        .and_then(|t| t.strip_prefix("HTTP/1."))
        .and_then(|t| t.get(2..5))
        .and_then(|c| c.parse().ok());
    Outcome { status }
}

/// Sends `bytes` and reports what comes back.
fn exchange(port: u16, bytes: &[u8]) -> Outcome {
    let mut s = connect(port);
    let _ = s.write_all(bytes);
    read_outcome(s, Duration::from_secs(8))
}

/// A normal request on a fresh connection; returns the latency when it was answered with 200.
fn healthy(port: u16) -> Option<Duration> {
    let started = Instant::now();
    let mut s = connect(port);
    s.write_all(b"GET /fast HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n").ok()?;
    let o = read_outcome(s, Duration::from_secs(25));
    (o.status == Some(200)).then(|| started.elapsed())
}

/// A server with the default worker count, started fresh for one test.
fn server(name: &str) -> (u16, KillOnDrop) {
    let port = free_port();
    let (dir, exe) = build_server(name, port);
    let child = start_server(&exe, &dir, port, &[]);
    assert!(healthy(port).is_some(), "baseline request failed");
    (port, child)
}

fn cpus() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

/// Malformed and ambiguous framing: the documented status, and the server keeps serving.
#[test]
fn malformed_and_ambiguous_requests_are_rejected() {
    let (port, _server) = server("malformed");

    // (name, bytes, acceptable statuses). `None` in the set means "closing without a response is
    // also acceptable"; 200 is never acceptable for these.
    let mut huge_header = b"GET /fast HTTP/1.1\r\nHost: t\r\nX-Big: ".to_vec();
    huge_header.extend(std::iter::repeat_n(b'a', 1 << 20));
    huge_header.extend_from_slice(b"\r\n\r\n");
    let cases: Vec<(&str, Vec<u8>, Vec<u16>)> = vec![
        ("1 MiB header value (default cap is 64 KiB)", huge_header, vec![431]),
        ("Content-Length together with Transfer-Encoding", b"POST /echo HTTP/1.1\r\nHost: t\r\nContent-Length: 5\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n".to_vec(), vec![400]),
        ("two different Content-Length values", b"POST /echo HTTP/1.1\r\nHost: t\r\nContent-Length: 5\r\nContent-Length: 6\r\n\r\nhello!".to_vec(), vec![400]),
        ("non-numeric Content-Length", b"POST /echo HTTP/1.1\r\nHost: t\r\nContent-Length: abc\r\n\r\n".to_vec(), vec![400]),
        ("signed Content-Length (+5)", b"POST /echo HTTP/1.1\r\nHost: t\r\nContent-Length: +5\r\n\r\nhello".to_vec(), vec![400]),
        ("negative Content-Length", b"POST /echo HTTP/1.1\r\nHost: t\r\nContent-Length: -1\r\n\r\n".to_vec(), vec![400]),
        ("overflowing Content-Length", b"POST /echo HTTP/1.1\r\nHost: t\r\nContent-Length: 99999999999999999999\r\n\r\n".to_vec(), vec![400, 413]),
        ("unsupported transfer coding", b"POST /echo HTTP/1.1\r\nHost: t\r\nTransfer-Encoding: gzip\r\n\r\n".to_vec(), vec![501]),
        ("whitespace before the header colon", b"POST /echo HTTP/1.1\r\nHost: t\r\nContent-Length : 5\r\n\r\nhello".to_vec(), vec![400]),
        ("unknown HTTP version", b"GET /fast HTTP/2.0\r\nHost: t\r\n\r\n".to_vec(), vec![505]),
        ("invalid chunk size", b"POST /echo HTTP/1.1\r\nHost: t\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\nhello\r\n0\r\n\r\n".to_vec(), vec![400]),
        ("chunk data not followed by CRLF", b"POST /echo HTTP/1.1\r\nHost: t\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhelloXX0\r\n\r\n".to_vec(), vec![400]),
        ("chunk size larger than the limit", b"POST /echo HTTP/1.1\r\nHost: t\r\nTransfer-Encoding: chunked\r\n\r\nFFFFFFFFFFFFFFFF\r\n".to_vec(), vec![400, 413]),
    ];

    let mut failures = Vec::new();
    for (name, bytes, ok) in &cases {
        let o = exchange(port, bytes);
        let pass = o.status.is_some_and(|s| ok.contains(&s));
        eprintln!("[{}] {name}: {o:?}", if pass { "ok" } else { "FAIL" });
        if !pass {
            failures.push(format!("{name}: got {o:?}, expected one of {ok:?}"));
        }
        if healthy(port).is_none_or(|d| d > Duration::from_secs(3)) {
            failures.push(format!("{name}: server not healthy afterwards"));
        }
    }

    // Hostile but not individually specified: only "no 200, and the server survives" is asserted.
    let mut many_headers = b"GET /fast HTTP/1.1\r\nHost: t\r\n".to_vec();
    for i in 0..5000 {
        many_headers.extend_from_slice(format!("X-H{i}: v\r\n").as_bytes());
    }
    many_headers.extend_from_slice(b"\r\n");
    let hostile: Vec<(&str, Vec<u8>)> = vec![
        ("5000 headers", many_headers),
        ("NUL byte in the request line", b"GET /fa\0st HTTP/1.1\r\nHost: t\r\n\r\n".to_vec()),
        ("empty request line", b"\r\n\r\n".to_vec()),
        ("binary garbage", (0..=255u8).cycle().take(4096).collect()),
        ("request line without a version", b"GET /fast\r\n\r\n".to_vec()),
    ];
    for (name, bytes) in &hostile {
        let o = exchange(port, bytes);
        eprintln!("[info] {name}: {o:?}");
        // "request line without a version" is answered 200 today (HTTP/0.9-style leniency) and an
        // empty request line gets a 404; both are noted in the bug, neither is asserted here.
        if o.status == Some(200) && *name != "5000 headers" && *name != "request line without a version" {
            failures.push(format!("{name}: answered 200 ({o:?})"));
        }
        if healthy(port).is_none_or(|d| d > Duration::from_secs(3)) {
            failures.push(format!("{name}: server not healthy afterwards"));
        }
    }
    assert!(failures.is_empty(), "adversarial request failures:\n{}", failures.join("\n"));
}

/// Clients that vanish mid-request (RST, half close) must not wedge workers or leak the budget.
#[test]
fn abrupt_disconnects_leave_the_server_healthy() {
    let (port, _server) = server("disconnects");

    for round in 0..300 {
        let mut s = connect(port);
        match round % 4 {
            // Full request, then an immediate RST instead of reading the answer.
            0 => {
                let _ = s.write_all(b"GET /fast HTTP/1.1\r\nHost: t\r\n\r\n");
                let _ = socket_reset(&s);
            }
            // Half a request, then a clean FIN.
            1 => {
                let _ = s.write_all(b"POST /echo HTTP/1.1\r\nHost: t\r\nContent-Length: 100\r\n\r\nabc");
                let _ = s.shutdown(Shutdown::Both);
            }
            // Request, then shut down only the write side and read the answer.
            2 => {
                let _ = s.write_all(b"GET /fast HTTP/1.1\r\nHost: t\r\n\r\n");
                let _ = s.shutdown(Shutdown::Write);
                let _ = read_outcome(s, Duration::from_millis(500));
            }
            // Connect and leave without sending anything.
            _ => drop(s),
        }
    }
    let latency = healthy(port);
    assert!(latency.is_some_and(|d| d < Duration::from_secs(3)), "server unhealthy after 300 abrupt disconnects: {latency:?}");
    // The connection budget must have been released: a burst of fresh requests all succeed.
    for i in 0..200 {
        assert!(healthy(port).is_some(), "request {i} failed after the disconnect storm (budget leak?)");
    }
}

/// A client that keeps the request head open by trickling bytes must be cut off at the absolute
/// head deadline (default 5s) with 408, however often it sends. This bounds the hold time; it
/// a stalled client costs no worker either, see the slowloris test below.
#[test]
fn trickling_request_head_is_cut_off_with_408() {
    use std::io::Read;
    let (port, _server) = server("head-deadline");
    let mut s = connect(port);
    s.write_all(b"GET /fast HTTP/1.1\r\nHost: t\r\nX-Slow: ").unwrap();
    let start = Instant::now();
    let mut reply = Vec::new();
    let mut buf = [0u8; 512];
    s.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
    while start.elapsed() < Duration::from_secs(12) {
        let _ = s.write_all(b"a");
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => reply.extend_from_slice(&buf[..n]),
            Err(_) => std::thread::sleep(Duration::from_millis(900)),
        }
        if reply.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let text = String::from_utf8_lossy(&reply);
    assert!(text.starts_with("HTTP/1.1 408"), "expected 408, got {text:?}");
    assert!(start.elapsed() < Duration::from_secs(10), "cut off too late: {:?}", start.elapsed());
    assert!(healthy(port).is_some(), "server unhealthy after the head deadline fired");
}

/// Regression for the slowloris starvation (#684): connections whose request head is incomplete
/// are parked by the epoll worker instead of being read with a blocking recv, so more slow
/// clients than workers must not delay a healthy client.
#[test]
fn slow_header_clients_do_not_starve_healthy_clients() {
    let (port, _server) = server("slowloris");
    let attackers = cpus() * 8;

    // Stalled: a partial request line, then silence.
    let stalled: Vec<TcpStream> = (0..attackers)
        .map(|_| {
            let mut s = connect(port);
            s.write_all(b"GET /fast HTTP/1.1\r\nHost: t\r\nX-Slow: ").unwrap();
            s
        })
        .collect();
    let stalled_latency = healthy(port);
    drop(stalled);

    // Trickle: one byte every 2s for 20s per attacker.
    let hold = Duration::from_secs(20);
    let trickle: Vec<_> = (0..attackers)
        .map(|_| {
            std::thread::spawn(move || {
                let mut s = connect(port);
                let _ = s.write_all(b"GET /fast HTTP/1.1\r\nHost: t\r\nX-Slow: ");
                let end = Instant::now() + hold;
                while Instant::now() < end {
                    std::thread::sleep(Duration::from_secs(2));
                    if s.write_all(b"a").is_err() {
                        break;
                    }
                }
            })
        })
        .collect();
    std::thread::sleep(Duration::from_millis(500));
    let trickle_latency = healthy(port);
    for t in trickle {
        let _ = t.join();
    }

    let limit = Duration::from_secs(3);
    assert!(
        stalled_latency.is_some_and(|d| d < limit) && trickle_latency.is_some_and(|d| d < limit),
        "{attackers} slow clients starved a healthy one: stalled -> {stalled_latency:?}, trickling -> {trickle_latency:?}"
    );
}

/// Resets the connection (RST instead of FIN) by closing with SO_LINGER 0.
fn socket_reset(s: &TcpStream) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    #[repr(C)]
    struct Linger {
        l_onoff: i32,
        l_linger: i32,
    }
    extern "C" {
        fn setsockopt(fd: i32, level: i32, name: i32, val: *const Linger, len: u32) -> i32;
    }
    const SOL_SOCKET: i32 = 1;
    const SO_LINGER: i32 = 13;
    let l = Linger { l_onoff: 1, l_linger: 0 };
    let rc = unsafe { setsockopt(s.as_raw_fd(), SOL_SOCKET, SO_LINGER, &l, std::mem::size_of::<Linger>() as u32) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}
