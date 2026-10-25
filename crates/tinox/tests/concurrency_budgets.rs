//! Issue #302: container-aware concurrency budgets and overload handling.
//!
//! Everything here drives REAL compiled programs and independent clients
//! (system curl, raw TCP sockets, `taskset`), not calls into the runtime's
//! helpers: the budgets are only meaningful if the compiled binary honours
//! them end to end.
//!
//! * the `Limits:` startup line reports the effective values (explicit
//!   `TINOX_*` overrides, CPU affinity, cgroup v1/v2 quota via a fixture
//!   tree) and malformed overrides hard-fail,
//! * HTTP load above `TINOX_MAX_CONNECTIONS` is answered with a visible
//!   `503 Service Unavailable` + `Retry-After` (distinguishable from an
//!   unrelated failure) and the server recovers once load drops,
//! * `spawn` above `TINOX_MAX_TASKS` yields a task that re-throws a
//!   budget error on `await` instead of growing threads without bound.

use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// Current published version of an extended-tier module (see
/// http_server_gc_stress_curl.rs for why this is not hardcoded).
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
    let dir = std::env::temp_dir().join(format!("tinox-budgets-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir workdir");
    dir
}

fn build(dir: &Path, exe: &str, port: Option<u16>) -> PathBuf {
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let out = dir.join(exe);
    let mut cmd = Command::new(tinox);
    cmd.arg("build").arg(dir.join("Main.tnx")).arg("-o").arg(&out).current_dir(dir);
    if let Some(p) = port {
        cmd.env("TINOX_PORT", p.to_string());
    }
    let b = cmd.output().expect("spawn build");
    assert!(
        b.status.success(),
        "build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&b.stdout),
        String::from_utf8_lossy(&b.stderr)
    );
    out
}

/// Annotation-driven HTTP server: `/fast` answers immediately, `/slow`
/// holds its connection for 200ms so concurrent requests overlap.
fn http_fixture(name: &str) -> (PathBuf, PathBuf, u16) {
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

    @GET("/slow")
    fnc slow(@HttpContext ctx: HttpContext) -> HttpContext
    {
        sleep_ms(200)
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
    std::fs::write(
        dir.join("tinox.toml"),
        format!(
            "[package]\ncompiler = \"2.6.0\"\nname = \"budget_server\"\nversion = \"0.0.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"http_server\"\nversion = \"{}\"\n",
            extended_module_version("http_server"),
        ),
    )
    .unwrap();
    let install = Command::new(env!("CARGO_BIN_EXE_tinox"))
        .arg("install")
        .current_dir(&dir)
        .output()
        .expect("spawn install");
    assert!(install.status.success(), "tinox install failed: {install:?}");
    let port = free_port();
    let exe = build(&dir, "budget_server", Some(port));
    (dir, exe, port)
}

/// Starts the server with `envs`, lets it print its banner, stops it and
/// returns what it wrote to stdout.
fn banner_of(exe: &Path, dir: &Path, wrapper: &[&str], envs: &[(&str, &str)]) -> String {
    let mut cmd = match wrapper.split_first() {
        Some((prog, rest)) => {
            let mut c = Command::new(prog);
            c.args(rest).arg(exe);
            c
        }
        None => Command::new(exe),
    };
    cmd.current_dir(dir).stdout(Stdio::piped()).stderr(Stdio::null());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn server");
    std::thread::sleep(Duration::from_millis(600));
    let _ = child.kill();
    let out = child.wait_with_output().expect("collect output");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn online_cpus() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

#[test]
fn limits_line_reports_overrides_affinity_and_cgroup_quota() {
    let (dir, exe, _port) = http_fixture("limits-line");

    // Explicit overrides are visible verbatim.
    let out = banner_of(
        &exe,
        &dir,
        &[],
        &[
            ("TINOX_HTTP_WORKERS", "2"),
            ("TINOX_MAX_CONNECTIONS", "7"),
            ("TINOX_MAX_TASKS", "9"),
            ("TINOX_MAX_WS_CONNECTIONS", "11"),
        ],
    );
    assert!(
        out.contains("http workers=2, max connections=7, max tasks=9, max websocket connections=11"),
        "overrides not reported:\n{out}"
    );

    // An explicit TINOX_CPUS wins over any detection and sizes the workers.
    let out = banner_of(&exe, &dir, &[], &[("TINOX_CPUS", "3")]);
    assert!(out.contains("cpus=3 (TINOX_CPUS), http workers=3"), "TINOX_CPUS not honoured:\n{out}");

    // CPU affinity (a real taskset mask) is respected.
    if Command::new("taskset").arg("--version").output().is_ok() {
        let out = banner_of(&exe, &dir, &["taskset", "-c", "0"], &[]);
        assert!(out.contains("cpus=1 (affinity), http workers=1"), "affinity not honoured:\n{out}");
    } else {
        eprintln!("SKIP affinity check (taskset not installed)");
    }

    // cgroup quota, v2 and v1, via a fixture tree. A 1-CPU quota only
    // lowers the count on a multi-core host.
    if online_cpus() > 1 {
        let v2 = dir.join("cg-v2");
        std::fs::create_dir_all(&v2).unwrap();
        std::fs::write(v2.join("cpu.max"), "100000 100000\n").unwrap();
        let out = banner_of(&exe, &dir, &[], &[("TINOX_CGROUP_ROOT", v2.to_str().unwrap())]);
        assert!(out.contains("cpus=1 (cgroup quota), http workers=1"), "cgroup v2 quota not honoured:\n{out}");

        let v1 = dir.join("cg-v1");
        std::fs::create_dir_all(v1.join("cpu")).unwrap();
        std::fs::write(v1.join("cpu/cpu.cfs_quota_us"), "50000\n").unwrap();
        std::fs::write(v1.join("cpu/cpu.cfs_period_us"), "100000\n").unwrap();
        let out = banner_of(&exe, &dir, &[], &[("TINOX_CGROUP_ROOT", v1.to_str().unwrap())]);
        assert!(out.contains("cpus=1 (cgroup quota), http workers=1"), "cgroup v1 quota not honoured:\n{out}");

        // "max" means no quota: falls back to the detected CPU count.
        let none = dir.join("cg-none");
        std::fs::create_dir_all(&none).unwrap();
        std::fs::write(none.join("cpu.max"), "max 100000\n").unwrap();
        let out = banner_of(&exe, &dir, &[], &[("TINOX_CGROUP_ROOT", none.to_str().unwrap())]);
        assert!(!out.contains("cgroup quota"), "unlimited cgroup must not be reported as a quota:\n{out}");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn malformed_limit_override_is_a_hard_failure() {
    let (dir, exe, _port) = http_fixture("bad-override");
    for (name, value) in [("TINOX_MAX_TASKS", "abc"), ("TINOX_MAX_CONNECTIONS", "0"), ("TINOX_HTTP_WORKERS", "-1"), ("TINOX_MAX_CONNECTIONS", "999999")] {
        let out = Command::new(&exe)
            .current_dir(&dir)
            .env(name, value)
            .stdin(Stdio::null())
            .output()
            .expect("run server");
        assert!(!out.status.success(), "{name}={value} must fail startup");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(&format!("invalid {name}=")), "unclear error for {name}={value}: {err}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn curl_status(port: u16, path: &str) -> (String, String) {
    let out = Command::new("curl")
        .args(["-s", "-i", "--max-time", "10", &format!("http://127.0.0.1:{port}{path}")])
        .output()
        .expect("spawn curl");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let status = text.lines().next().unwrap_or("").to_string();
    (status, text)
}

/// Socket inodes held open by `pid` (from `/proc/<pid>/fd`).
fn socket_inodes_of(pid: u32) -> std::collections::HashSet<u64> {
    let mut set = std::collections::HashSet::new();
    if let Ok(rd) = std::fs::read_dir(format!("/proc/{pid}/fd")) {
        for e in rd.flatten() {
            if let Ok(t) = std::fs::read_link(e.path()) {
                let t = t.to_string_lossy().into_owned();
                if let Some(n) = t.strip_prefix("socket:[").and_then(|r| r.strip_suffix(']')) {
                    if let Ok(n) = n.parse() {
                        set.insert(n);
                    }
                }
            }
        }
    }
    set
}

/// `(state, inode)` of every IPv4/IPv6 TCP socket whose LOCAL port is `port`,
/// read from `/proc/net/tcp{,6}` -- an independent view of the kernel's
/// socket table, not of the server's own bookkeeping.
fn tcp_sockets_on_port(port: u16) -> Vec<(String, u64)> {
    let mut rows = Vec::new();
    for file in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(text) = std::fs::read_to_string(file) else { continue };
        for line in text.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 10 {
                continue;
            }
            let local_port = f[1].rsplit(':').next().and_then(|p| u16::from_str_radix(p, 16).ok());
            if local_port == Some(port) {
                if let Ok(inode) = f[9].parse() {
                    rows.push((f[3].to_string(), inode));
                }
            }
        }
    }
    rows
}

/// Listening sockets on `port` that the server process `pid` does not own.
/// With `SO_REUSEPORT` a foreign listener on the same port silently joins
/// the server's accept group and steals connections (#442).
fn foreign_listeners(pid: u32, port: u16) -> Vec<u64> {
    let mine = socket_inodes_of(pid);
    tcp_sockets_on_port(port)
        .into_iter()
        .filter(|(st, ino)| st == "0A" && !mine.contains(ino))
        .map(|(_, ino)| ino)
        .collect()
}

/// Established connections on `port` that the server `pid` has accepted.
fn accepted_by_server(pid: u32, port: u16) -> usize {
    let mine = socket_inodes_of(pid);
    tcp_sockets_on_port(port)
        .into_iter()
        .filter(|(st, ino)| st == "01" && mine.contains(ino))
        .count()
}

#[test]
fn http_overload_is_rejected_with_503_and_server_recovers() {
    let (dir, exe, port) = http_fixture("http-overload");
    let child = Command::new(&exe)
        .current_dir(&dir)
        .env("TINOX_MAX_CONNECTIONS", "3")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server");
    let pid = child.id();
    let _guard = KillOnDrop(child);
    std::thread::sleep(Duration::from_millis(600));

    // The port was reserved by free_port() before the build, so another
    // process could have bound it since. Then idle connections would be
    // spread over foreign listeners and the budget never fills (#442).
    let foreign = foreign_listeners(pid, port);
    assert!(foreign.is_empty(), "port {port} is shared with listeners the server does not own (socket inodes {foreign:?}); fixture port was taken by another process");

    // Healthy before any load.
    let (status, _) = curl_status(port, "/fast");
    assert!(status.contains("200"), "baseline request failed: {status}");

    // The baseline curl has exited, but the server only frees its budget slot
    // once a worker has processed the EOF. Saturating before that makes the
    // third idle connection get rejected at accept (the stale slot still
    // counts), leaving the budget one short once the slot is freed -- the
    // next probe then gets 200 instead of 503 (#442). Wait until the kernel
    // shows no connection left on the port, only listeners.
    let deadline = Instant::now() + Duration::from_secs(10);
    while tcp_sockets_on_port(port).iter().any(|(st, _)| st != "0A" && st != "06") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let leftover: Vec<_> = tcp_sockets_on_port(port).into_iter().filter(|(st, _)| st != "0A" && st != "06").collect();
    assert!(leftover.is_empty(), "baseline connection still open after 10s: {leftover:?}");

    // Saturate the budget with idle connections the server has accepted.
    let idle: Vec<TcpStream> = (0..3)
        .map(|_| TcpStream::connect(("127.0.0.1", port)).expect("connect idle"))
        .collect();
    // Wait until the server has really accepted all three; a fixed sleep
    // races with worker scheduling on a loaded machine.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut accepted = accepted_by_server(pid, port);
    while accepted < 3 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
        accepted = accepted_by_server(pid, port);
    }
    assert!(accepted >= 3, "server accepted only {accepted} of 3 idle connections within 10s; sockets on port {port}: {:?}", tcp_sockets_on_port(port));

    // The next client is turned away visibly, and distinguishably.
    let (status, text) = curl_status(port, "/fast");
    assert!(status.contains("503"), "expected 503 over budget, got: {text}");
    assert!(text.to_lowercase().contains("retry-after: 1"), "missing Retry-After: {text}");
    assert!(text.contains("connection budget exhausted"), "overload not identifiable: {text}");

    // Dropping the load frees the budget: healthy requests recover.
    drop(idle);
    std::thread::sleep(Duration::from_millis(400));
    let (status, _) = curl_status(port, "/fast");
    assert!(status.contains("200"), "server did not recover after the burst: {status}");

    // Burst of concurrent requests: every answer is either a real 200 or
    // an explicit 503 -- never a hang, reset or other failure.
    let started = Instant::now();
    let handles: Vec<_> = (0..60)
        .map(|_| {
            std::thread::spawn(move || {
                let t = Instant::now();
                let (status, _) = curl_status(port, "/slow");
                (status, t.elapsed())
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let ok = results.iter().filter(|(s, _)| s.contains("200")).count();
    let rejected = results.iter().filter(|(s, _)| s.contains("503")).count();
    let worst = results.iter().map(|(_, d)| *d).max().unwrap();
    eprintln!(
        "burst: 60 requests in {:?}: {ok} served, {rejected} rejected (503), slowest {worst:?}",
        started.elapsed()
    );
    assert_eq!(ok + rejected, 60, "unexpected failures in burst: {results:?}");
    assert!(ok >= 1, "nothing served during the burst");

    // And the server is fully healthy again afterwards.
    std::thread::sleep(Duration::from_millis(500));
    let (status, _) = curl_status(port, "/fast");
    assert!(status.contains("200"), "server unhealthy after burst: {status}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn spawn_over_task_budget_rejects_instead_of_growing_threads() {
    let dir = workdir("task-budget");
    std::fs::write(
        dir.join("Main.tnx"),
        r#"class Main {
    async fnc hold(n: Int64) -> Int64 { sleep_ms(300)
    return n }

    fnc main() -> Int32 {
        var ok = 0
        var rejected = 0
        const t1 = spawn Main::hold(1)
        const t2 = spawn Main::hold(2)
        const t3 = spawn Main::hold(3)
        const t4 = spawn Main::hold(4)
        try { await t1
        ok = ok + 1 } catch (e: String) { rejected = rejected + 1 }
        try { await t2
        ok = ok + 1 } catch (e: String) { rejected = rejected + 1 }
        try { await t3
        ok = ok + 1 } catch (e: String) { println(e)
        rejected = rejected + 1 }
        try { await t4
        ok = ok + 1 } catch (e: String) { rejected = rejected + 1 }
        println("ok=" + ok.toString() + " rejected=" + rejected.toString())

        // Budget is released once the running tasks finish: healthy again.
        const t5 = spawn Main::hold(5)
        println("after=" + (await t5).toString())
        return 0
    }
}
"#,
    )
    .unwrap();
    let exe = build(&dir, "task_budget", None);

    let out = Command::new(&exe)
        .current_dir(&dir)
        .env("TINOX_MAX_TASKS", "2")
        .output()
        .expect("run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "program failed: {out:?}");
    assert!(stdout.contains("task budget exhausted"), "rejection not surfaced:\n{stdout}");
    assert!(stdout.contains("ok=2 rejected=2"), "unexpected admission split:\n{stdout}");
    assert!(stdout.contains("after=5"), "no recovery after tasks finished:\n{stdout}");

    // Default budget does not interfere with ordinary spawning.
    let out = Command::new(&exe).current_dir(&dir).output().expect("run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("ok=4 rejected=0"), "default budget rejected normal spawns:\n{stdout}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn limits_can_come_from_application_properties_and_env_wins() {
    let (dir, exe, _port) = http_fixture("props");
    std::fs::write(
        dir.join("application.properties"),
        "tinox.limits.max_tasks=7\ntinox.limits.max_connections=33\ntinox.limits.queue_wait_ms=250\n",
    )
    .unwrap();
    let out = banner_of(&exe, &dir, &[], &[]);
    assert!(out.contains("max connections=33, max tasks=7"), "properties ignored:\n{out}");
    assert!(out.contains("queue wait=250 ms"), "queue wait from properties ignored:\n{out}");

    // The environment variable takes precedence over the properties file.
    let out = banner_of(&exe, &dir, &[], &[("TINOX_MAX_TASKS", "9")]);
    assert!(out.contains("max connections=33, max tasks=9"), "env must win:\n{out}");

    // A malformed properties value is as fatal as a malformed env var.
    std::fs::write(dir.join("application.properties"), "tinox.limits.max_tasks=lots\n").unwrap();
    let r = Command::new(&exe).current_dir(&dir).stdin(Stdio::null()).output().unwrap();
    assert!(!r.status.success());
    assert!(String::from_utf8_lossy(&r.stderr).contains("invalid tinox.limits.max_tasks="));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dev_ui_limits_endpoint_and_metrics_report_queue_state() {
    let (dir, exe, port) = http_fixture("limits-endpoint");
    // Rebuild with the introspection API enabled on its own free port.
    let dev_port = free_port();
    let toml = std::fs::read_to_string(dir.join("tinox.toml")).unwrap();
    std::fs::write(dir.join("tinox.toml"), format!("{toml}\n[dev]\nenabled = true\nport = {dev_port}\n")).unwrap();
    let exe2 = build(&dir, "budget_server_dev", Some(port));
    let _ = exe;
    let child = Command::new(&exe2)
        .current_dir(&dir)
        .env("TINOX_MAX_CONNECTIONS", "5")
        .env("TINOX_QUEUE_WAIT_MS", "40")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server");
    let _guard = KillOnDrop(child);
    std::thread::sleep(Duration::from_millis(800));
    let (_, text) = curl_status(dev_port, "/limits");
    assert!(text.contains("\"maxConnections\":5"), "/limits missing budgets: {text}");
    assert!(text.contains("\"queueWaitMs\":40"), "/limits missing queue wait: {text}");
    assert!(text.contains("\"connectionsRejected\":0"), "/limits missing counters: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn queued_spawn_waits_for_a_slot_within_the_deadline() {
    let dir = workdir("task-queue");
    std::fs::write(
        dir.join("Main.tnx"),
        r#"class Main {
    async fnc hold(n: Int64) -> Int64 { sleep_ms(300)
    return n }

    fnc main() -> Int32 {
        var ok = 0
        var rejected = 0
        const t1 = spawn Main::hold(1)
        const t2 = spawn Main::hold(2)
        const t3 = spawn Main::hold(3)
        const t4 = spawn Main::hold(4)
        try { await t1
        ok = ok + 1 } catch (e: String) { rejected = rejected + 1 }
        try { await t2
        ok = ok + 1 } catch (e: String) { rejected = rejected + 1 }
        try { await t3
        ok = ok + 1 } catch (e: String) { rejected = rejected + 1 }
        try { await t4
        ok = ok + 1 } catch (e: String) { rejected = rejected + 1 }
        println("ok=" + ok.toString() + " rejected=" + rejected.toString())
        return 0
    }
}
"#,
    )
    .unwrap();
    let exe = build(&dir, "task_queue", None);
    let run = |wait: &str| {
        let t = Instant::now();
        let out = Command::new(&exe)
            .current_dir(&dir)
            .env("TINOX_MAX_TASKS", "2")
            .env("TINOX_QUEUE_WAIT_MS", wait)
            .output()
            .expect("run");
        (String::from_utf8_lossy(&out.stdout).into_owned(), t.elapsed())
    };
    // Generous deadline: the 3rd and 4th spawn queue until a slot frees up.
    let (out, took) = run("5000");
    assert!(out.contains("ok=4 rejected=0"), "queued spawns were not admitted:\n{out}");
    assert!(took >= Duration::from_millis(550), "spawns did not actually queue: {took:?}");
    // Deadline shorter than the running tasks: bounded wait, then rejection.
    let (out, took) = run("100");
    assert!(out.contains("ok=2 rejected=2"), "deadline not enforced:\n{out}");
    assert!(took < Duration::from_millis(1500), "rejection was not bounded: {took:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- WebSocket connection budget ----

fn ws_handshake(port: u16) -> TcpStream {
    use std::io::{Read, Write};
    let mut s = None;
    for _ in 0..50 {
        if let Ok(c) = TcpStream::connect(("127.0.0.1", port)) {
            s = Some(c);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut s = s.expect("connect to WS server");
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(b"GET /echo HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n").unwrap();
    let mut resp = Vec::new();
    let mut byte = [0u8; 1];
    while !resp.ends_with(b"\r\n\r\n") {
        s.read_exact(&mut byte).expect("handshake response");
        resp.push(byte[0]);
    }
    assert!(String::from_utf8_lossy(&resp).contains("101"), "no 101 upgrade");
    s
}

/// Sends one masked text frame and returns the first frame header byte the
/// server answers with (0x81 = echoed text, 0x88 = close) or None on EOF.
fn ws_ping_pong(s: &mut TcpStream) -> Option<u8> {
    use std::io::{Read, Write};
    let payload = b"hi";
    let mask = [1u8, 2, 3, 4];
    let mut frame = vec![0x81u8, 0x80 | payload.len() as u8];
    frame.extend_from_slice(&mask);
    for (i, b) in payload.iter().enumerate() {
        frame.push(b ^ mask[i % 4]);
    }
    let _ = s.write_all(&frame);
    let mut hdr = [0u8; 2];
    if s.read_exact(&mut hdr).is_err() {
        return None;
    }
    // Consume the (short, unmasked) payload so the next call starts on a
    // frame boundary.
    let mut payload = vec![0u8; (hdr[1] & 0x7f) as usize];
    let _ = s.read_exact(&mut payload);
    Some(hdr[0])
}

#[test]
fn websocket_connections_over_budget_are_closed_and_budget_recovers() {
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let src = repo_root().join("examples/ws_echo_annotated/Main.tnx");
    let dir = workdir("ws-budget");
    let exe = dir.join("EchoEndpoint");

    let install = Command::new(tinox)
        .arg("install")
        .current_dir(src.parent().unwrap())
        .output()
        .expect("spawn install");
    assert!(install.status.success(), "tinox install failed: {install:?}");
    let build = Command::new(tinox)
        .arg("build")
        .arg(&src)
        .arg("-o")
        .arg(&exe)
        .current_dir(&dir)
        .output()
        .expect("spawn build");
    assert!(build.status.success(), "build failed: {build:?}");

    let child = Command::new(&exe)
        .current_dir(&dir)
        .env("TINOX_MAX_WS_CONNECTIONS", "2")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server");
    let _guard = KillOnDrop(child);
    let port = 8793;

    let mut a = ws_handshake(port);
    let mut b = ws_handshake(port);
    assert_eq!(ws_ping_pong(&mut a), Some(0x81), "A should be served");
    assert_eq!(ws_ping_pong(&mut b), Some(0x81), "B should be served");

    // Third client is over budget: it is closed, never served.
    let mut c = ws_handshake(port);
    let answer = ws_ping_pong(&mut c);
    assert!(
        answer != Some(0x81),
        "connection over the WebSocket budget was served instead of closed"
    );

    // Admitted connections are unaffected by the rejection.
    assert_eq!(ws_ping_pong(&mut a), Some(0x81), "A broke after a rejection");

    // Freeing a slot lets a new client in.
    drop(b);
    std::thread::sleep(Duration::from_millis(500));
    let mut d = ws_handshake(port);
    assert_eq!(ws_ping_pong(&mut d), Some(0x81), "budget did not recover after a disconnect");

    let _ = std::fs::remove_dir_all(&dir);
}
