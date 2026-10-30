//! Issue #525: instance-scoped HttpClient with connect/read/total deadlines, no header leakage
//! between callers on one thread, and failures distinguishable from empty responses. Peers are
//! independent raw-socket servers.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A listener whose accept queue is already full, so a further connect() hangs.
fn saturated_listener() -> (TcpListener, Vec<TcpStream>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    unsafe { libc::listen(listener.as_raw_fd(), 0) };
    let port = listener.local_addr().unwrap().port();
    let mut fill = Vec::new();
    for _ in 0..8 {
        let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_NONBLOCK, 0) };
        let addr = libc::sockaddr_in {
            sin_family: libc::AF_INET as u16,
            sin_port: port.to_be(),
            sin_addr: libc::in_addr { s_addr: u32::from_ne_bytes([127, 0, 0, 1]) },
            sin_zero: [0; 8],
        };
        unsafe { libc::connect(sock, &addr as *const _ as *const libc::sockaddr, std::mem::size_of::<libc::sockaddr_in>() as u32) };
        fill.push(unsafe { std::os::unix::io::FromRawFd::from_raw_fd(sock) });
    }
    (listener, fill)
}

const MAIN: &str = r#"import tinox.core.http

class Main
{
    fnc probe(label: String, client: HttpClient, url: String) -> Nothing {
        const t0 = now()
        const r: HttpClientResponse = client.get(url)
        const ms = now() - t0
        println(label + " failed=" + r.failed().toString() + " ms=" + ms.toString() + " status=" + r.statusCode().toString() + " err=" + r.error())
    }

    fnc main() -> Int32 {
        // Peer accepts the connection but never answers: bounded by the idle-read deadline.
        Main::probe("read", HttpClient::create().timeouts(2000, 300, 0), "http://127.0.0.1:__STALL__/x")
        // Peer trickles bytes forever: each read is fine, the total deadline stops it.
        Main::probe("total", HttpClient::create().timeouts(2000, 2000, 500), "http://127.0.0.1:__TRICKLE__/x")
        // Connect never completes: bounded by the connect deadline.
        Main::probe("connect", HttpClient::create().timeouts(300, 2000, 0), "http://127.0.0.1:__BLACKHOLE__/x")
        // Unresolvable name fails fast and visibly.
        Main::probe("dns", HttpClient::create().timeouts(2000, 2000, 5000), "http://does-not-exist.invalid/x")
        // An enclosing task deadline also stops a stalled request.
        taskSetDeadline(300)
        Main::probe("task", HttpClient::create().timeouts(0, 0, 0), "http://127.0.0.1:__STALL__/x")
        return 0
    }
}
"#;

const MAIN_HEADERS: &str = r#"import tinox.core.http

class Main
{
    fnc main() -> Int32 {
        const a: HttpClient = HttpClient::create().header("X-Who", "client-a").timeouts(2000, 2000, 5000)
        const b: HttpClient = HttpClient::create().header("X-Who", "client-b").timeouts(2000, 2000, 5000)
        Http::setHeader("X-Who", "legacy-thread-state")
        a.get("http://127.0.0.1:__ECHO__/a")
        b.get("http://127.0.0.1:__ECHO__/b")
        const ok: HttpClientResponse = b.get("http://127.0.0.1:__ECHO__/ok")
        println("ok failed=" + ok.failed().toString() + " status=" + ok.statusCode().toString() + " body=" + ok.body())
        Http::clearHeaders()
        const plain: HttpClientResponse = a.post("http://127.0.0.1:__ECHO__/post", "payload")
        println("post body=" + plain.body())
        return 0
    }
}
"#;

fn project(name: &str, main: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let dir = std::env::temp_dir().join(format!("tinox-http-client-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let version = std::fs::read_to_string(root.join("crates/tinox-core-ext/http/tinox.toml"))
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("version = ").map(|v| v.trim_matches('"').to_string()))
        .unwrap();
    std::fs::write(
        dir.join("tinox.toml"),
        format!(
            "[package]\ncompiler = \"2.8.0\"\nname = \"hc\"\nversion = \"0.1.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"http\"\nversion = \"{version}\"\n\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"http\"\npath = \"{}\"\n",
            root.join("crates/tinox-core-ext/http").display()
        ),
    )
    .unwrap();
    std::fs::write(dir.join("Main.tnx"), main).unwrap();
    dir
}

fn run(dir: &PathBuf) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_tinox")).args(["run", "Main.tnx"]).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn field(line: &str, key: &str) -> String {
    line.split_whitespace().find_map(|p| p.strip_prefix(key)).unwrap_or("").to_string()
}

#[test]
fn stalled_peers_cannot_exceed_the_deadline_contract() {
    // read stall
    let stall = TcpListener::bind("127.0.0.1:0").unwrap();
    let stall_port = stall.local_addr().unwrap().port();
    let held = Arc::new(Mutex::new(Vec::new()));
    let held2 = held.clone();
    std::thread::spawn(move || {
        for conn in stall.incoming().flatten() {
            held2.lock().unwrap().push(conn); // accept, read nothing, answer nothing
        }
    });
    // trickle
    let trickle = TcpListener::bind("127.0.0.1:0").unwrap();
    let trickle_port = trickle.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut conn in trickle.incoming().flatten() {
            std::thread::spawn(move || {
                let mut buf = [0u8; 1024];
                let _ = conn.read(&mut buf);
                let _ = conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\n\r\n");
                for _ in 0..200 {
                    if conn.write_all(b"x").is_err() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            });
        }
    });
    let (blackhole, _fill) = saturated_listener();
    let blackhole_port = blackhole.local_addr().unwrap().port();

    let dir = project(
        "deadlines",
        &MAIN
            .replace("__STALL__", &stall_port.to_string())
            .replace("__TRICKLE__", &trickle_port.to_string())
            .replace("__BLACKHOLE__", &blackhole_port.to_string()),
    );
    let out = run(&dir);
    let line = |label: &str| out.lines().find(|l| l.starts_with(&format!("{label} "))).unwrap_or_else(|| panic!("no `{label}` line in:\n{out}")).to_string();
    let ms = |l: &str| field(l, "ms=").parse::<u64>().unwrap();

    let read = line("read");
    assert_eq!(field(&read, "failed="), "true", "{read}");
    assert!((250..900).contains(&ms(&read)), "read stall must end near 300 ms: {read}");
    assert!(read.contains("read timed out"), "{read}");

    let total = line("total");
    assert_eq!(field(&total, "failed="), "true", "{total}");
    assert!((450..1200).contains(&ms(&total)), "trickle must be cut off near 500 ms: {total}");
    assert!(total.contains("total deadline"), "{total}");

    let connect = line("connect");
    assert_eq!(field(&connect, "failed="), "true", "{connect}");
    assert!(ms(&connect) < 1500, "connect stall must end near 300 ms: {connect}");
    assert!(connect.contains("connect timed out") || connect.contains("connect failed"), "{connect}");

    let dns = line("dns");
    assert_eq!(field(&dns, "failed="), "true", "{dns}");
    assert!(dns.contains("DNS lookup"), "{dns}");

    let task = line("task");
    assert_eq!(field(&task, "failed="), "true", "{task}");
    assert!(ms(&task) < 1500 && task.contains("cancelled"), "task deadline must stop the request: {task}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn headers_stay_with_their_client_and_a_success_is_not_a_failure() {
    let echo = TcpListener::bind("127.0.0.1:0").unwrap();
    let echo_port = echo.local_addr().unwrap().port();
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        for mut conn in echo.incoming().flatten() {
            let mut buf = [0u8; 4096];
            let n = conn.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            seen2.lock().unwrap().push(req.clone());
            let body = format!("saw:{}", req.lines().find(|l| l.to_lowercase().starts_with("x-who")).unwrap_or("none"));
            let _ = conn.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).as_bytes());
        }
    });
    let dir = project("headers", &MAIN_HEADERS.replace("__ECHO__", &echo_port.to_string()));
    let out = run(&dir);
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 4, "{requests:?}");
    let who = |r: &str| r.lines().filter(|l| l.to_lowercase().starts_with("x-who")).map(str::to_string).collect::<Vec<_>>();
    assert_eq!(who(&requests[0]), vec!["X-Who: client-a"], "client a must not carry the legacy thread-local header");
    assert_eq!(who(&requests[1]), vec!["X-Who: client-b"]);
    assert!(out.contains("ok failed=false status=200 body=saw:X-Who: client-b"), "{out}");
    assert!(out.contains("post body=saw:X-Who: client-a"), "{out}");
    assert!(requests[3].contains("payload"), "{requests:?}");
    let _ = std::fs::remove_dir_all(dir);
}
