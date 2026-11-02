//! `@Blocking(workers, queue)` routes (issue #552): a slow handler runs on its own bounded pool, so it cannot
//! occupy the HTTP epoll worker that serves other routes; a full queue is a visible 503; the offloaded
//! response closes the connection. Checked from outside with `curl`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct App {
    child: Child,
    port: u16,
}
impl Drop for App {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn curl_available() -> bool {
    Command::new("curl").arg("--version").output().is_ok()
}

fn project(tag: &str, api: Option<&str>) -> (Dir, PathBuf) {
    let dir = Dir(std::env::temp_dir().join(format!("tinox-blocking-{tag}-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/blocking_route");
    std::fs::create_dir_all(&dir.0).unwrap();
    for f in ["Main.tnx", "Api.tnx", "tinox.toml"] {
        std::fs::copy(fixture.join(f), dir.0.join(f)).unwrap();
    }
    if let Some(api) = api {
        std::fs::write(dir.0.join("Api.tnx"), api).unwrap();
    }
    let install = Command::new(env!("CARGO_BIN_EXE_tinox")).arg("install").current_dir(&dir.0).output().unwrap();
    assert!(install.status.success(), "{}", String::from_utf8_lossy(&install.stderr));
    let app = dir.0.join("app");
    (dir, app)
}

fn build(dir: &Dir, port: u16) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tinox"))
        .args(["build", "Main.tnx", "-o", "app"])
        .env("TINOX_PORT", port.to_string())
        .current_dir(&dir.0)
        .output()
        .unwrap()
}

/// One HTTP worker, so a handler that blocks it starves every other connection of that worker.
fn start(dir: &Dir) -> App {
    start_with(dir, &[])
}

fn start_with(dir: &Dir, envs: &[(&str, &str)]) -> App {
    let port = free_port();
    let b = build(dir, port);
    assert!(b.status.success(), "{}{}", String::from_utf8_lossy(&b.stdout), String::from_utf8_lossy(&b.stderr));
    let child = Command::new(dir.0.join("app")).env("TINOX_HTTP_WORKERS", "1").envs(envs.iter().copied()).current_dir(&dir.0).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let app = App { child, port };
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(10) {
        if curl(&app, &["-s", "-o", "/dev/null", &path(&app, "/fast")]).is_some() {
            return app;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("app never answered");
}

fn path(app: &App, p: &str) -> String {
    format!("http://127.0.0.1:{}{p}", app.port)
}

fn curl(_app: &App, args: &[&str]) -> Option<String> {
    let out = Command::new("curl").args(args).args(["--max-time", "15"]).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

/// (status, seconds) of one request.
fn timed(app: &App, p: &str) -> (u16, f64) {
    let out = curl(app, &["-s", "-o", "/dev/null", "-w", "%{http_code} %{time_total}", &path(app, p)]).expect("curl");
    let mut parts = out.split_whitespace();
    (parts.next().unwrap().parse().unwrap(), parts.next().unwrap().parse().unwrap())
}

fn spawn_curl(app: &App, p: &str) -> Child {
    Command::new("curl").args(["-s", "-o", "/dev/null", "-w", "%{http_code}", "--max-time", "20", &path(app, p)]).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap()
}

fn finish(mut c: Child) -> String {
    let mut out = String::new();
    c.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    let _ = c.wait();
    out
}

#[test]
fn a_blocking_route_does_not_starve_other_routes() {
    if !curl_available() {
        eprintln!("SKIP blocking routes (curl not installed)");
        return;
    }
    let (dir, _) = project("starve", None);
    let app = start(&dir);

    // Without @Blocking, three slow requests occupy the single HTTP worker: a fast route waits behind them.
    let inline: Vec<Child> = (0..3).map(|_| spawn_curl(&app, "/slow-inline")).collect();
    std::thread::sleep(Duration::from_millis(200));
    let (status, starved) = timed(&app, "/fast");
    assert_eq!(status, 200);
    assert!(starved > 1.5, "an inline slow handler should block the worker, fast route took {starved}s");
    for c in inline {
        assert_eq!(finish(c), "200");
    }

    // With @Blocking(2, 4) the same load runs on the route's pool and the fast route answers at once.
    let blocking: Vec<Child> = (0..3).map(|_| spawn_curl(&app, "/slow")).collect();
    std::thread::sleep(Duration::from_millis(200));
    let (status, fast) = timed(&app, "/fast");
    assert_eq!(status, 200);
    assert!(fast < 0.7, "the fast route must not wait for blocking work, took {fast}s");
    for c in blocking {
        assert_eq!(finish(c), "200", "accepted blocking requests complete");
    }
}

#[test]
fn a_full_queue_is_a_visible_503_and_the_pool_stays_bounded() {
    if !curl_available() {
        return;
    }
    let (dir, _) = project("overload", None);
    let app = start(&dir);
    // 2 workers + 4 queue slots accept 6 of 12 simultaneous requests; the rest are refused immediately.
    let clients: Vec<Child> = (0..12).map(|_| spawn_curl(&app, "/slow")).collect();
    let statuses: Vec<String> = clients.into_iter().map(finish).collect();
    let ok = statuses.iter().filter(|s| *s == "200").count();
    let refused = statuses.iter().filter(|s| *s == "503").count();
    assert_eq!(ok + refused, 12, "{statuses:?}");
    assert!((6..=8).contains(&ok), "expected about 6 accepted, got {ok}: {statuses:?}");
    assert!(refused >= 4, "{statuses:?}");

    // The refusal is fast, says why, and tells the client when to retry.
    let blockers: Vec<Child> = (0..6).map(|_| spawn_curl(&app, "/slow")).collect();
    std::thread::sleep(Duration::from_millis(300));
    let started = Instant::now();
    let headers = curl(&app, &["-s", "-i", &path(&app, "/slow")]).unwrap();
    assert!(started.elapsed() < Duration::from_millis(700), "refusal took {:?}", started.elapsed());
    assert!(headers.starts_with("HTTP/1.1 503"), "{headers}");
    assert!(headers.to_lowercase().contains("retry-after: 1"), "{headers}");
    assert!(headers.contains("overloaded"), "{headers}");
    for c in blockers {
        finish(c);
    }
    // Once the queue drains the route accepts again.
    let (status, _) = timed(&app, "/slow");
    assert_eq!(status, 200);
}

#[test]
fn offloaded_requests_keep_their_body_and_close_the_connection() {
    if !curl_available() {
        return;
    }
    let (dir, _) = project("body", None);
    let app = start(&dir);
    let body = "{\"name\":\"Ada\",\"n\":[1,2,3],\"text\":\"ünïcode ✓\"}";
    let reply = curl(&app, &["-s", "-i", "-X", "POST", "-H", "Content-Type: application/json", "-d", body, &path(&app, "/echo")]).unwrap();
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    assert!(reply.ends_with(body), "{reply}");
    assert!(reply.to_lowercase().contains("connection: close"), "an offloaded response closes the connection:\n{reply}");
    // An ordinary route on the same server still keeps the connection alive.
    let fast = curl(&app, &["-s", "-i", &path(&app, "/fast")]).unwrap();
    assert!(fast.to_lowercase().contains("connection: keep-alive"), "{fast}");
}

#[test]
fn request_ids_survive_the_pool_boundary() {
    if !curl_available() {
        return;
    }
    let (dir, _) = project("trace", None);
    let app = start_with(&dir, &[("TINOX_TELEMETRY_ENABLED", "true")]);
    // The telemetry context of an offloaded request is created on the pool thread from the request's headers.
    for (method, p) in [("GET", "/fast"), ("POST", "/echo")] {
        let reply = curl(&app, &["-s", "-i", "-X", method, "-H", "X-Request-Id: req-abc-123", "-H", "Traceparent: 00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01", "-d", "{}", &path(&app, p)]).unwrap();
        assert!(reply.contains("X-Request-Id: req-abc-123"), "{p} lost the request id:\n{reply}");
    }
}

#[test]
fn invalid_blocking_annotations_are_compile_errors() {
    let bad_args = "import tinox.core.http_server\n\nclass Api\n{\n    @GET\n    @Path(\"/x\")\n    @Blocking(0, 4)\n    fn x(@HttpContext ctx: HttpContext) -> HttpContext\n    {\n        return ctx\n    }\n}\n";
    let no_verb = "import tinox.core.http_server\n\nclass Api\n{\n    @Blocking(2, 4)\n    fn x(@HttpContext ctx: HttpContext) -> HttpContext\n    {\n        return ctx\n    }\n}\n";
    for (tag, api, expect) in [("badargs", bad_args, "workers must be 1..256"), ("noverb", no_verb, "has no effect without")] {
        let (dir, _) = project(tag, Some(api));
        let b = build(&dir, free_port());
        assert!(!b.status.success(), "{tag} must be rejected");
        let text = format!("{}{}", String::from_utf8_lossy(&b.stdout), String::from_utf8_lossy(&b.stderr));
        assert!(text.contains(expect), "{tag}: expected {expect:?} in\n{text}");
    }
}

fn copy_tree(source: &Path, target: &Path) {
    std::fs::create_dir_all(target).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target.join(entry.file_name()));
        } else {
            std::fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
        }
    }
}

/// A `@Service` app with request-scoped injection: draining waits for blocking work that is running or
/// queued, queued requests are answered 503 once the service is stopping, and the process exits cleanly.
#[test]
fn draining_waits_for_running_and_queued_blocking_requests() {
    if !curl_available() {
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = Dir(std::env::temp_dir().join(format!("tinox-blocking-service-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    // Local copies of the extended modules stand in for installed packages (as the lifecycle tests do).
    let mut manifest = "[package]\ncompiler = \"2.9.0\"\nname=\"blocking_service\"\nversion=\"0.0.0\"\n[startup]\nbanner=false\n".to_string();
    for module in ["http_server", "json"] {
        manifest.push_str(&format!("[[dependencies]]\ngroup=\"tinox.core\"\nartifactId=\"{module}\"\nversion=\"9.9.9\"\n"));
        copy_tree(&root.join("crates/tinox-core-ext").join(module).join("tinox"), &dir.0.join(format!(".tinox/repository/central/tinox.core/{module}/9.9.9/tinox")));
    }
    std::fs::write(dir.0.join("tinox.toml"), manifest).unwrap();
    copy_tree(&root.join("tests/fixtures/blocking_service"), &dir.0);
    let b = Command::new(env!("CARGO_BIN_EXE_tinox")).current_dir(&dir.0).env("TINOX_HOME", &dir.0).env("TINOX_PORT", "0").args(["build", "Main.tnx", "-o", "service"]).output().unwrap();
    assert!(b.status.success(), "{}{}", String::from_utf8_lossy(&b.stdout), String::from_utf8_lossy(&b.stderr));

    let mut child = Command::new(dir.0.join("service")).current_dir(&dir.0).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut reader = std::io::BufReader::new(child.stdout.take().unwrap());
    // The service prints "init" (its @OnStart hook) before the bound port.
    let port: u16 = (0..10)
        .find_map(|_| {
            let mut line = String::new();
            std::io::BufRead::read_line(&mut reader, &mut line).ok()?;
            line.trim().parse().ok()
        })
        .expect("the service never printed its port");
    let app = App { child, port };

    let (status, _) = timed(&app, "/fast");
    assert_eq!(status, 200);
    let body = curl(&app, &["-s", &path(&app, "/slow")]).unwrap();
    assert_eq!(body.trim().trim_matches('"'), "complete false", "the pool thread reaches the request-scoped state");

    // One request runs (1 worker), two wait in the queue; the stop arrives while they are in flight.
    let running = spawn_curl(&app, "/slow");
    std::thread::sleep(Duration::from_millis(100));
    let queued: Vec<Child> = (0..2).map(|_| spawn_curl(&app, "/slow")).collect();
    std::thread::sleep(Duration::from_millis(150));
    let pid = app.child.id().to_string();
    let _ = Command::new("kill").args(["-TERM", &pid]).status();
    assert_eq!(finish(running), "200", "the running request completes");
    for c in queued {
        let status = finish(c);
        assert!(status == "503" || status == "200", "a queued request is served or refused, never dropped: {status}");
    }
    let mut app = app;
    let started = Instant::now();
    let exit = loop {
        if let Some(status) = app.child.try_wait().unwrap() {
            break status;
        }
        assert!(started.elapsed() < Duration::from_secs(10), "the service did not exit after draining");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(exit.success(), "{exit:?}");
}
