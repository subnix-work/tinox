//! Issue #526: HttpClient keep-alive pooling, verified against an independent Python HTTP/1.1
//! server (plain and TLS) that logs every accepted connection and request.
#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

struct Server {
    child: Child,
    port: u16,
    lines: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn start(mode: &str, extra: &[String]) -> Server {
        let root = repo_root();
        let mut child = Command::new("python3")
            .arg(root.join("tests/fixtures/http_keepalive/server.py"))
            .arg(mode)
            .args(extra)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let mut first = String::new();
        reader.read_line(&mut first).unwrap();
        let port = first.trim().strip_prefix("PORT ").unwrap_or_else(|| panic!("server said {first:?}")).parse().unwrap();
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        std::thread::spawn(move || {
            for l in reader.lines().map_while(Result::ok) {
                sink.lock().unwrap().push(l);
            }
        });
        Server { child, port, lines }
    }
    fn conns(&self) -> usize {
        self.lines.lock().unwrap().iter().filter(|l| l.starts_with("CONN ")).count()
    }
    fn reqs(&self) -> Vec<String> {
        self.lines.lock().unwrap().iter().filter(|l| l.starts_with("REQ ")).cloned().collect()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn project(name: &str, main: &str) -> PathBuf {
    let root = repo_root();
    let dir = std::env::temp_dir().join(format!("tinox-http-ka-{name}-{}", std::process::id()));
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
            "[package]\ncompiler = \"2.8.0\"\nname = \"ka\"\nversion = \"0.1.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"http\"\nversion = \"{version}\"\n\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"http\"\npath = \"{}\"\n",
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

fn expected(n: usize) -> String {
    "0123456789abcdef".repeat(n / 16 + 1)[..n].to_string()
}

const MAIN: &str = r#"import tinox.core.http

class Main
{
    fnc check(label: String, r: HttpClientResponse, want: String) -> Int64 {
        if (r.failed() || r.body() != want) {
            println("BAD " + label + " failed=" + r.failed().toString() + " err=" + r.error() + " len=" + r.body().len().toString())
            return 0
        }
        return 1
    }

    fnc worker(c: HttpClient, base: String, n: Int64) -> Int64 {
        var ok: Int64 = 0
        var i: Int64 = 0
        while (i < n) {
            ok = ok + Main::check("w", c.get(base + "/len/3000"), "__LEN3000__")
            i = i + 1
        }
        return ok
    }

    fnc main() -> Int32 {
        const base = "__BASE__"
        const c: HttpClient = HttpClient::create().timeouts(2000, 2000, 8000).__TLS__keepAlive(3, 60000, 0)
        var good: Int64 = 0
        // Different framings back to back on what must stay one connection.
        good = good + Main::check("small", c.get(base + "/len/5"), "01234")
        good = good + Main::check("zero", c.get(base + "/len/0"), "")
        good = good + Main::check("big", c.get(base + "/len/70000"), "__LEN70000__")
        good = good + Main::check("chunked", c.get(base + "/chunked/5000"), "__LEN5000__")
        good = good + Main::check("echo", c.post(base + "/echo", "hello"), "echo:hello")
        good = good + Main::check("empty", c.get(base + "/empty"), "")
        const head: HttpClientResponse = c.request("HEAD", base + "/x", "")
        good = good + Main::check("head", head, "")
        good = good + Main::check("after-head", c.get(base + "/len/7"), "0123456")
        println("serial good=" + good.toString() + " opened=" + c.connectionsOpened().toString() + " reused=" + c.connectionsReused().toString() + " idle=" + c.idleConnections().toString())

        // Connection: close is honoured; the next request opens a new connection.
        good = good + Main::check("close", c.get(base + "/close"), "closing")
        good = good + Main::check("after-close", c.get(base + "/len/4"), "0123")
        println("close opened=" + c.connectionsOpened().toString())

        // Server hangs up silently after a response: GET is replayed, POST fails visibly.
        good = good + Main::check("drop", c.get(base + "/drop"), "dropped")
        const again: HttpClientResponse = c.get(base + "/len/4")
        println("drop-get failed=" + again.failed().toString() + " body=" + again.body())
        c.get(base + "/drop")
        const post: HttpClientResponse = c.post(base + "/echo", "once")
        println("drop-post failed=" + post.failed().toString() + " err=" + post.error() + " body=" + post.body())

        // Concurrency: 8 tasks x 10 requests through a pool of at most 3 connections.
        const d: HttpClient = HttpClient::create().timeouts(2000, 2000, 20000).__TLS__keepAlive(3, 60000, 0)
        const t1 = spawn Main::worker(d, base, 10)
        const t2 = spawn Main::worker(d, base, 10)
        const t3 = spawn Main::worker(d, base, 10)
        const t4 = spawn Main::worker(d, base, 10)
        const t5 = spawn Main::worker(d, base, 10)
        const t6 = spawn Main::worker(d, base, 10)
        const t7 = spawn Main::worker(d, base, 10)
        const t8 = spawn Main::worker(d, base, 10)
        const total = (await t1) + (await t2) + (await t3) + (await t4) + (await t5) + (await t6) + (await t7) + (await t8)
        println("load ok=" + total.toString() + " opened=" + d.connectionsOpened().toString() + " busy=" + d.busyConnections().toString())

        // Lifetime: a connection older than lifetimeMs is not reused.
        const e: HttpClient = HttpClient::create().timeouts(2000, 2000, 8000).__TLS__keepAlive(2, 60000, 100)
        e.get(base + "/len/1")
        sleep_ms(250)
        e.get(base + "/len/1")
        println("lifetime opened=" + e.connectionsOpened().toString())

        c.close()
        const closed: HttpClientResponse = c.get(base + "/len/1")
        println("closed failed=" + closed.failed().toString() + " err=" + closed.error())
        return 0
    }
}
"#;

fn render(base: &str, tls: &str) -> String {
    MAIN.replace("__BASE__", base)
        .replace("__TLS__", tls)
        .replace("__LEN3000__", &expected(3000))
        .replace("__LEN70000__", &expected(70000))
        .replace("__LEN5000__", &expected(5000))
}

fn field(out: &str, line: &str, key: &str) -> i64 {
    let l = out.lines().find(|l| l.starts_with(line)).unwrap_or_else(|| panic!("no `{line}` line in:\n{out}"));
    l.split_whitespace().find_map(|p| p.strip_prefix(key)).unwrap_or_else(|| panic!("no {key} in {l}")).parse().unwrap()
}

fn verify(out: &str, server: &Server, label: &str) {
    assert!(!out.contains("BAD "), "{label}: {out}");
    // 8 serial requests (incl. HEAD/204/chunked/big) shared a single connection.
    assert_eq!(field(out, "serial", "good="), 8, "{out}");
    assert_eq!(field(out, "serial", "opened="), 1, "{label}: one connection must serve all framings\n{out}");
    assert_eq!(field(out, "serial", "reused="), 7, "{out}");
    assert_eq!(field(out, "serial", "idle="), 1, "{out}");
    assert_eq!(field(out, "close", "opened="), 2, "Connection: close must end reuse\n{out}");
    assert!(out.contains("drop-get failed=false body=0123"), "GET after a silent hang-up is replayed:\n{out}");
    // Either the dead connection is noticed before the POST is written (fresh connection) or the
    // hang-up wins the race (visible failure). Never a silent replay: at most one POST reaches the server.
    assert!(
        out.contains("drop-post failed=false err= body=echo:once") || (out.contains("drop-post failed=true") && out.contains("not retried automatically")),
        "{out}"
    );
    let posts = server.reqs().iter().filter(|r| r.starts_with("REQ POST /echo")).count();
    // One POST from the serial block plus at most one from the hang-up case.
    assert!(posts <= 2, "POST must never be replayed: {:?}", server.reqs());
    assert_eq!(field(out, "load", "ok="), 80, "{out}");
    assert!(field(out, "load", "opened=") <= 3, "pool limit must hold: {out}");
    assert_eq!(field(out, "load", "busy="), 0, "{out}");
    assert_eq!(field(out, "lifetime", "opened="), 2, "{out}");
    assert!(out.contains("closed failed=true err=HTTP client is closed"), "{out}");
    // Independent confirmation from the server: far fewer connections than requests.
    let reqs = server.reqs().len();
    let conns = server.conns();
    assert!(conns * 3 < reqs, "{label}: {conns} connections for {reqs} requests");
    // The server never saw two requests interleaved on one connection (ids per request are valid).
    for r in server.reqs() {
        assert!(r.split_whitespace().count() == 4, "{r}");
    }
}

#[test]
fn keepalive_reuses_connections_over_http() {
    let server = Server::start("http", &[]);
    let dir = project("http", &render(&format!("http://127.0.0.1:{}", server.port), ""));
    let out = run(&dir);
    verify(&out, &server, "http");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn keepalive_reuses_connections_over_tls() {
    let tmp = std::env::temp_dir().join(format!("tinox-http-ka-cert-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let (cert, key) = (tmp.join("cert.pem"), tmp.join("key.pem"));
    let status = Command::new("openssl")
        .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "2", "-subj", "/CN=localhost", "-addext", "subjectAltName=DNS:localhost"])
        .arg("-keyout")
        .arg(&key)
        .arg("-out")
        .arg(&cert)
        .output()
        .unwrap();
    assert!(status.status.success(), "{}", String::from_utf8_lossy(&status.stderr));
    let server = Server::start("https", &[cert.display().to_string(), key.display().to_string()]);
    let tls = format!("tlsCaCertFile(\"{}\").", cert.display());
    let dir = project("https", &render(&format!("https://localhost:{}", server.port), &tls));
    let out = run(&dir);
    verify(&out, &server, "https");
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(tmp);
}

const MAIN_RACE: &str = r#"import tinox.core.http

class Main
{
    fnc main() -> Int32 {
        const base = "http://127.0.0.1:__PORT__"
        const c: HttpClient = HttpClient::create().timeouts(2000, 2000, 8000).keepAlive(2, 60000, 0)
        const first: HttpClientResponse = c.get(base + "/a")
        println("first failed=" + first.failed().toString() + " body=" + first.body())
        sleep_ms(200)
        const second: HttpClientResponse = c.__SECOND__
        println("second failed=" + second.failed().toString() + " err=" + second.error() + " body=" + second.body() + " opened=" + c.connectionsOpened().toString())
        return 0
    }
}
"#;

/// Server that answers the first request on a connection and then hangs up in the very moment
/// the next request arrives (after it was read), without ever answering it: the classic
/// keep-alive race. Returns the requests it saw.
fn racing_server() -> (u16, Arc<Mutex<Vec<String>>>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        for mut conn in listener.incoming().flatten() {
            let seen = seen2.clone();
            std::thread::spawn(move || {
                let mut count = 0;
                let mut buf = [0u8; 4096];
                loop {
                    let n = conn.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    seen.lock().unwrap().push(req.lines().next().unwrap_or("").to_string());
                    count += 1;
                    if count == 1 {
                        let _ = conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
                    } else {
                        return; // close without answering
                    }
                }
            });
        }
    });
    (port, seen)
}

#[test]
fn stale_connection_is_replayed_only_for_idempotent_methods() {
    // The liveness probe at checkout would normally catch a closed idle connection, so the server
    // here keeps the socket open until the second request is on the wire (the race window).
    for (second, expect_failed) in [("get(base + \"/b\")", false), ("post(base + \"/b\", \"payload\")", true)] {
        let (port, seen) = racing_server();
        let dir = project("race", &MAIN_RACE.replace("__PORT__", &port.to_string()).replace("__SECOND__", second));
        let out = run(&dir);
        let requests = seen.lock().unwrap().clone();
        if expect_failed {
            assert!(out.contains("second failed=true") && out.contains("not retried automatically"), "{out}");
            assert_eq!(requests.iter().filter(|r| r.starts_with("POST")).count(), 1, "POST must not be replayed: {requests:?}");
        } else {
            // The replay on a fresh connection is answered by the (new) connection's first request.
            assert!(out.contains("second failed=false") && out.contains("body=ok") && out.contains("opened=2"), "{out}");
            assert_eq!(requests.iter().filter(|r| r.starts_with("GET /b")).count(), 2, "GET replayed once: {requests:?}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
