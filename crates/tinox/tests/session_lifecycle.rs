//! Issue #530: SessionManager lifecycle (login, sliding renewal, expiry, rotation, logout, key
//! rotation, revocation) observed through curl as an independent HTTP client.
#![cfg(unix)]

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct Kill(Child);
impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

const MAIN: &str = r#"import tinox.core.http_server
import tinox.core.oidc
import tinox.core.json

class Main
{
    fnc main() -> Int32
    {
        const a: SessionManager = SessionManager::create(["old-secret-old-secret"]).secure(false).lifetime(5).idle(2).renewAfter(1)
        const b: SessionManager = SessionManager::create(["new-secret-new-secret", "old-secret-old-secret"]).secure(false).lifetime(5).idle(2).renewAfter(1)
        const server: HttpServer = HttpServer::new(__PORT__)
        server.post("/login", ctx => {
            a.start(ctx, Json::parse("{\"user\":\"ada\",\"role\":\"admin\"}").asTable())
            ctx.response.text("logged in")
        })
        server.get("/me", ctx => {
            const s: Session = a.touch(ctx)
            if (!s.valid) { ctx.response.status(401).text("no:" + s.reason) } else { ctx.response.text("yes:" + s.claims["user"].getString() + ":" + s.renewed.toString() + ":" + s.id) }
        })
        server.get("/peek", ctx => {
            const s: Session = a.load(ctx)
            if (!s.valid) { ctx.response.status(401).text("no:" + s.reason) } else { ctx.response.text("yes:" + s.id) }
        })
        server.get("/bme", ctx => {
            const s: Session = b.touch(ctx)
            if (!s.valid) { ctx.response.status(401).text("no:" + s.reason) } else { ctx.response.text("yes:" + s.claims["user"].getString() + ":" + s.renewed.toString()) }
        })
        server.post("/rotate", ctx => {
            const s: Session = a.rotate(ctx)
            if (!s.valid) { ctx.response.status(401).text("no:" + s.reason) } else { ctx.response.text("rotated:" + s.id) }
        })
        server.post("/logout", ctx => {
            a.end(ctx)
            ctx.response.text("bye")
        })
        server.post("/logout-all", ctx => {
            a.endAll()
            ctx.response.text("all")
        })
        server.listen()
        return 0
    }
}
"#;

struct Reply {
    status: u16,
    set_cookies: Vec<String>,
    body: String,
}

fn call(port: u16, method: &str, path: &str, cookie: Option<&str>) -> Reply {
    for attempt in 0..15 {
        let mut cmd = Command::new("curl");
        cmd.args(["-s", "-i", "-X", method, &format!("http://127.0.0.1:{port}{path}")]);
        if let Some(c) = cookie {
            cmd.args(["-H", &format!("Cookie: tinox_session={c}")]);
        }
        let text = String::from_utf8_lossy(&cmd.output().unwrap().stdout).to_string();
        if !text.is_empty() || attempt == 14 {
            let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
            return Reply {
                status: head.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0),
                set_cookies: head.lines().filter(|l| l.to_lowercase().starts_with("set-cookie:")).map(|l| l[11..].trim().to_string()).collect(),
                body: body.to_string(),
            };
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    unreachable!()
}

fn cookie_value(r: &Reply) -> String {
    assert_eq!(r.set_cookies.len(), 1, "exactly one Set-Cookie expected: {:?}", r.set_cookies);
    let first = r.set_cookies[0].split(';').next().unwrap();
    first.strip_prefix("tinox_session=").unwrap_or_else(|| panic!("{first}")).to_string()
}

#[test]
fn session_lifecycle_is_enforced_for_an_independent_client() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let dir = std::env::temp_dir().join(format!("tinox-session-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let ext = root.join("crates/tinox-core-ext");
    let version = |m: &str| {
        std::fs::read_to_string(ext.join(m).join("tinox.toml"))
            .unwrap()
            .lines()
            .find_map(|l| l.strip_prefix("version = ").map(|v| v.trim_matches('"').to_string()))
            .unwrap()
    };
    let mut toml = String::from("[package]\ncompiler = \"2.8.0\"\nname = \"sess\"\nversion = \"0.0.0\"\ndescription = \"\"\n");
    for m in ["oidc", "http_server", "json", "crypto", "jwt", "oauth2", "http"] {
        toml.push_str(&format!("\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"{m}\"\nversion = \"{}\"\n", version(m)));
    }
    for m in ["oidc", "http_server", "http"] {
        toml.push_str(&format!("\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"{m}\"\npath = \"{}\"\n", ext.join(m).display()));
    }
    std::fs::write(dir.join("tinox.toml"), toml).unwrap();
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    std::fs::write(dir.join("Main.tnx"), MAIN.replace("__PORT__", &port.to_string())).unwrap();
    let install = Command::new(env!("CARGO_BIN_EXE_tinox")).arg("install").current_dir(&dir).output().unwrap();
    assert!(install.status.success(), "{}", String::from_utf8_lossy(&install.stderr));
    let exe = dir.join("sess");
    let build = Command::new(env!("CARGO_BIN_EXE_tinox")).args(["build", "Main.tnx", "-o"]).arg(&exe).current_dir(&dir).output().unwrap();
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));
    let server = Command::new(&exe).current_dir(&dir).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let _guard = Kill(server);
    std::thread::sleep(Duration::from_millis(600));

    // No cookie / garbage cookie.
    let r = call(port, "GET", "/me", None);
    assert_eq!((r.status, r.body.as_str()), (401, "no:missing"));
    let r = call(port, "GET", "/me", Some("not-a-sealed-value"));
    assert_eq!((r.status, r.body.as_str()), (401, "no:invalid"));

    // Login sets exactly one hardened cookie.
    let login = call(port, "POST", "/login", None);
    assert_eq!(login.status, 200, "{}", login.body);
    assert!(login.set_cookies[0].contains("HttpOnly") && login.set_cookies[0].contains("SameSite=Lax") && login.set_cookies[0].contains("Path=/"), "{:?}", login.set_cookies);
    let c1 = cookie_value(&login);
    let r = call(port, "GET", "/me", Some(&c1));
    assert!(r.body.starts_with("yes:ada:false:"), "fresh session is not renewed: {}", r.body);
    assert!(r.set_cookies.is_empty());
    let sid1 = r.body.rsplit(':').next().unwrap().to_string();

    // A tampered cookie is rejected.
    let mut tampered = c1.clone().into_bytes();
    let mid = tampered.len() / 2;
    tampered[mid] = if tampered[mid] == b'0' { b'1' } else { b'0' }; // the cookie is hex: pick a digit that really differs
    let r = call(port, "GET", "/me", Some(&String::from_utf8(tampered).unwrap()));
    assert_eq!((r.status, r.body.as_str()), (401, "no:invalid"));

    // Sliding renewal after renewAfter (1 s): a new cookie with the same session id.
    std::thread::sleep(Duration::from_millis(1200));
    let r = call(port, "GET", "/me", Some(&c1));
    assert!(r.body.starts_with("yes:ada:true:"), "{}", r.body);
    assert!(r.body.ends_with(&sid1), "renewal keeps the session id: {}", r.body);
    let c2 = cookie_value(&r);
    assert_ne!(c1, c2);

    // Rotation: new id, old cookie revoked, new one works.
    let rot = call(port, "POST", "/rotate", Some(&c2));
    assert!(rot.body.starts_with("rotated:"), "{}", rot.body);
    let c3 = cookie_value(&rot);
    let r = call(port, "GET", "/peek", Some(&c2));
    assert_eq!((r.status, r.body.as_str()), (401, "no:revoked"));
    let r = call(port, "GET", "/peek", Some(&c3));
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(!r.body.ends_with(&sid1), "rotation must change the id");

    // Key rotation: manager B (new key first, old key second) accepts A's cookie and re-seals it.
    let r = call(port, "GET", "/bme", Some(&c3));
    assert!(r.body.starts_with("yes:ada:true"), "older key re-sealed on touch: {}", r.body);
    let c4 = cookie_value(&r);
    let r = call(port, "GET", "/me", Some(&c4));
    assert_eq!((r.status, r.body.as_str()), (401, "no:invalid"), "manager A cannot open a cookie sealed with the new key");

    // Concurrent requests on one cookie all succeed.
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let c = c3.clone();
            std::thread::spawn(move || call(port, "GET", "/peek", Some(&c)).status)
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().unwrap(), 200);
    }

    // Logout clears the cookie and revokes the id.
    let out = call(port, "POST", "/logout", Some(&c3));
    assert!(out.set_cookies.iter().any(|c| c.contains("Max-Age=0")), "{:?}", out.set_cookies);
    let r = call(port, "GET", "/peek", Some(&c3));
    assert_eq!((r.status, r.body.as_str()), (401, "no:revoked"));

    // logout-all invalidates everything issued before.
    let login = call(port, "POST", "/login", None);
    let c5 = cookie_value(&login);
    assert_eq!(call(port, "GET", "/peek", Some(&c5)).status, 200);
    std::thread::sleep(Duration::from_millis(20));
    call(port, "POST", "/logout-all", None);
    let r = call(port, "GET", "/peek", Some(&c5));
    assert_eq!((r.status, r.body.as_str()), (401, "no:revoked"));

    // Idle expiry (2 s without a request) and absolute lifetime (5 s even when active).
    let login = call(port, "POST", "/login", None);
    let c6 = cookie_value(&login);
    std::thread::sleep(Duration::from_millis(2300));
    let r = call(port, "GET", "/me", Some(&c6));
    assert_eq!((r.status, r.body.as_str()), (401, "no:idle"));

    let login = call(port, "POST", "/login", None);
    let mut cookie = cookie_value(&login);
    let started = std::time::Instant::now();
    let mut verdict = String::new();
    while started.elapsed() < Duration::from_secs(8) {
        std::thread::sleep(Duration::from_millis(900));
        let r = call(port, "GET", "/me", Some(&cookie));
        if r.status != 200 {
            verdict = r.body;
            break;
        }
        if let Some(c) = r.set_cookies.first() {
            cookie = c.split(';').next().unwrap().strip_prefix("tinox_session=").unwrap().to_string();
        }
    }
    assert_eq!(verdict, "no:expired", "an active session still ends at the absolute lifetime");
    assert!(started.elapsed() >= Duration::from_millis(4500), "expired too early: {:?}", started.elapsed());
    let _ = std::fs::remove_dir_all(&dir);
}
