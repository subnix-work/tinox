//! Issue #527: @Retry / @CircuitBreaker around real HttpClient calls against an independent
//! raw-socket service that fails on demand.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const API: &str = r#"import tinox.core.http

class Api
{
    var client: HttpClient

    @Retry(5, 20, ["UNAVAILABLE"], 200, 0, 5000)
    @Idempotent
    fn read(url: String) -> String
    {
        const r: HttpClientResponse = this.client.get(url)
        if (r.failed()) { throw "UNAVAILABLE: " + r.error() }
        if (r.statusCode() == 503) { throw "UNAVAILABLE: status 503" }
        return r.body()
    }

    fn write(url: String) -> String
    {
        const r: HttpClientResponse = this.client.post(url, "payload")
        if (r.statusCode() == 503) { throw "UNAVAILABLE: status 503" }
        return r.body()
    }

    @CircuitBreaker(2, 400, ["UNAVAILABLE"])
    fn guarded(url: String) -> String
    {
        const r: HttpClientResponse = this.client.get(url)
        if (r.failed() || r.statusCode() == 503) { throw "UNAVAILABLE: down" }
        return r.body()
    }
}
"#;

const MAIN: &str = r#"import Api
import tinox.core.http

class Main
{
    fnc main() -> Int32
    {
        const api = Api { client: HttpClient::create().timeouts(500, 500, 2000) }
        println("read=" + api.read("http://127.0.0.1:__PORT__/flaky"))
        try { api.write("http://127.0.0.1:__PORT__/flaky-post") } catch (e: String) { println("write failed: " + e) }
        try { api.guarded("http://127.0.0.1:__PORT__/dead") } catch (e: String) { println("g1 " + e) }
        try { api.guarded("http://127.0.0.1:__PORT__/dead") } catch (e: String) { println("g2 " + e) }
        try { api.guarded("http://127.0.0.1:__PORT__/dead") } catch (e: String) { println("g3 " + e) }
        sleep_ms(450)
        println("recovered=" + api.guarded("http://127.0.0.1:__PORT__/ok"))
        return 0
    }
}
"#;

#[test]
fn retry_and_breaker_against_a_failing_service() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let flaky_hits = Arc::new(AtomicUsize::new(0));
    let post_hits = Arc::new(AtomicUsize::new(0));
    let dead_hits = Arc::new(AtomicUsize::new(0));
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let (f, p, d, l) = (flaky_hits.clone(), post_hits.clone(), dead_hits.clone(), log.clone());
    std::thread::spawn(move || {
        for mut conn in listener.incoming().flatten() {
            let (f, p, d, l) = (f.clone(), p.clone(), d.clone(), l.clone());
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let n = conn.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let line = req.lines().next().unwrap_or("").to_string();
                l.lock().unwrap().push(line.clone());
                let status = if line.contains("/flaky-post") {
                    p.fetch_add(1, Ordering::SeqCst);
                    "503 Service Unavailable"
                } else if line.contains("/flaky") {
                    if f.fetch_add(1, Ordering::SeqCst) < 2 { "503 Service Unavailable" } else { "200 OK" }
                } else if line.contains("/dead") {
                    d.fetch_add(1, Ordering::SeqCst);
                    "503 Service Unavailable"
                } else {
                    "200 OK"
                };
                let body = if status.starts_with("200") { "fine" } else { "no" };
                let _ = conn.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes());
            });
        }
    });

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let dir = std::env::temp_dir().join(format!("tinox-resilience-http-{}", std::process::id()));
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
            "[package]\ncompiler = \"2.8.0\"\nname = \"res\"\nversion = \"0.1.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"http\"\nversion = \"{version}\"\n\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"http\"\npath = \"{}\"\n",
            root.join("crates/tinox-core-ext/http").display()
        ),
    )
    .unwrap();
    std::fs::write(dir.join("Api.tnx"), API).unwrap();
    std::fs::write(dir.join("Main.tnx"), MAIN.replace("__PORT__", &port.to_string())).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_tinox")).args(["run", "Main.tnx"]).current_dir(&dir).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));

    assert!(stdout.contains("read=fine"), "{stdout}");
    assert_eq!(flaky_hits.load(Ordering::SeqCst), 3, "2 failures + 1 success: {:?}", log.lock().unwrap());
    // The write is not annotated: exactly one request, failure surfaces.
    assert_eq!(post_hits.load(Ordering::SeqCst), 1, "{:?}", log.lock().unwrap());
    assert!(stdout.contains("write failed: UNAVAILABLE: status 503"), "{stdout}");
    // Breaker: two real failures, the third call is rejected without a request; later it recovers.
    assert_eq!(dead_hits.load(Ordering::SeqCst), 2, "{:?}", log.lock().unwrap());
    assert!(stdout.contains("g3 CIRCUIT_OPEN: Api.guarded rejected the call"), "{stdout}");
    assert!(stdout.contains("recovered=fine"), "{stdout}");
    let _ = std::fs::remove_dir_all(dir);
}
