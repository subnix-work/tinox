//! Issue #530: OidcWebApp.installWithSessions end to end -- login redirect, callback with a real
//! (PyJWT-issued, RS256) id_token verified against a JWKS, two cookies in one response, managed
//! session gating and logout. The identity provider is an independent raw-socket server.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct Kill(Child);
impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `const <name> = "..."` string literal (escapes undone) from the e2e fixture that holds the
/// independently issued token and key set.
fn fixture_const(source: &str, name: &str) -> String {
    let start = source.find(&format!("const {name} = \"")).unwrap_or_else(|| panic!("{name}")) + format!("const {name} = \"").len();
    let rest = &source[start..];
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(chars.next().unwrap()),
            '"' => break,
            c => out.push(c),
        }
    }
    out
}

const MAIN: &str = r#"import tinox.core.http_server
import tinox.core.oidc
import tinox.core.json

class Main
{
    fnc main() -> Int32
    {
        const client: OidcClient = OidcClient::new("https://issuer.example.com", "http://127.0.0.1:__IDP__/authorize", "http://127.0.0.1:__IDP__/token", "http://127.0.0.1:__IDP__/jwks.json", "my-client-id", "client-secret", "http://127.0.0.1:__APP__/callback")
        const app: OidcWebApp = OidcWebApp::new(client, "pending-cookie-secret-123")
        const sessions: SessionManager = SessionManager::create(["session-secret-session-secret"]).secure(false).lifetime(600).idle(300)
        const server: HttpServer = HttpServer::new(__APP__)
        app.installWithSessions(server, sessions)
        server.get("/", ctx => {
            if (!app.requireSession(ctx, sessions)) { return }
            ctx.response.text("hello " + sessions.load(ctx).claims["sub"].getString())
        })
        server.get("/logout", ctx => { app.logoutSession(ctx, sessions) })
        server.listen()
        return 0
    }
}
"#;

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}
impl Reply {
    fn all(&self, name: &str) -> Vec<String> {
        self.headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone()).collect()
    }
}

fn get(port: u16, path: &str, cookies: &[&str]) -> Reply {
    for attempt in 0..15 {
        let mut cmd = Command::new("curl");
        cmd.args(["-s", "-i", &format!("http://127.0.0.1:{port}{path}")]);
        if !cookies.is_empty() {
            cmd.args(["-H", &format!("Cookie: {}", cookies.join("; "))]);
        }
        let text = String::from_utf8_lossy(&cmd.output().unwrap().stdout).to_string();
        if !text.is_empty() || attempt == 14 {
            let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
            let mut lines = head.lines();
            let status = lines.next().and_then(|l| l.split_whitespace().nth(1)).and_then(|s| s.parse().ok()).unwrap_or(0);
            let headers = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.to_string(), v.trim().to_string())).collect();
            return Reply { status, headers, body: body.to_string() };
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    unreachable!()
}

fn cookie_pair(set_cookie: &str) -> String {
    set_cookie.split(';').next().unwrap().to_string()
}

#[test]
fn oidc_login_creates_a_managed_session_and_logout_ends_it() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let fixture = std::fs::read_to_string(root.join("tests/e2e/oidc_id_token_verification/Main.tnx")).unwrap();
    let id_token = fixture_const(&fixture, "idToken");
    let jwks = fixture_const(&fixture, "jwksBody");

    // Independent identity provider.
    let idp = TcpListener::bind("127.0.0.1:0").unwrap();
    let idp_port = idp.local_addr().unwrap().port();
    let token_requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = token_requests.clone();
    std::thread::spawn(move || {
        for mut conn in idp.incoming().flatten() {
            let mut buf = vec![0u8; 8192];
            let n = conn.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = if req.starts_with("POST /token") {
                seen.lock().unwrap().push(req.clone());
                format!("{{\"access_token\":\"access-1\",\"token_type\":\"Bearer\",\"expires_in\":3600,\"id_token\":\"{id_token}\"}}")
            } else {
                jwks.clone()
            };
            let _ = conn.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).as_bytes());
        }
    });

    let dir = std::env::temp_dir().join(format!("tinox-oidc-web-{}", std::process::id()));
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
    let mut toml = String::from("[package]\ncompiler = \"2.8.0\"\nname = \"web\"\nversion = \"0.0.0\"\ndescription = \"\"\n");
    for m in ["oidc", "http_server", "json", "crypto", "jwt", "oauth2", "http"] {
        toml.push_str(&format!("\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"{m}\"\nversion = \"{}\"\n", version(m)));
    }
    for m in ["oidc", "http_server", "http"] {
        toml.push_str(&format!("\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"{m}\"\npath = \"{}\"\n", ext.join(m).display()));
    }
    std::fs::write(dir.join("tinox.toml"), toml).unwrap();
    let app_port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    std::fs::write(dir.join("Main.tnx"), MAIN.replace("__IDP__", &idp_port.to_string()).replace("__APP__", &app_port.to_string())).unwrap();
    let install = Command::new(env!("CARGO_BIN_EXE_tinox")).arg("install").current_dir(&dir).output().unwrap();
    assert!(install.status.success(), "{}", String::from_utf8_lossy(&install.stderr));
    let exe = dir.join("web");
    let build = Command::new(env!("CARGO_BIN_EXE_tinox")).args(["build", "Main.tnx", "-o"]).arg(&exe).current_dir(&dir).output().unwrap();
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));
    let server = Command::new(&exe).current_dir(&dir).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let _guard = Kill(server);
    std::thread::sleep(Duration::from_millis(600));

    // Not logged in: redirected to /login.
    let r = get(app_port, "/", &[]);
    assert_eq!(r.status, 302);
    assert_eq!(r.all("Location"), vec!["/login"]);

    // /login redirects to the IdP and sets the pending-login cookie.
    let r = get(app_port, "/login", &[]);
    assert_eq!(r.status, 302, "{:?}", r.headers);
    let location = r.all("Location").pop().unwrap();
    assert!(location.starts_with(&format!("http://127.0.0.1:{idp_port}/authorize?")), "{location}");
    let state = location.split(['?', '&']).find_map(|p| p.strip_prefix("state=")).expect("state in authorize URL").to_string();
    let pending = r.all("Set-Cookie").into_iter().find(|c| c.starts_with("tinox_oidc_pending=")).expect("pending cookie");
    assert!(pending.contains("HttpOnly") && pending.contains("SameSite=Lax") && pending.contains("Max-Age=300"), "{pending}");
    let pending_pair = cookie_pair(&pending);

    // A callback with the wrong state or without the pending cookie never creates a session.
    assert_eq!(get(app_port, "/callback?code=abc&state=forged", &[&pending_pair]).status, 400);
    assert_eq!(get(app_port, &format!("/callback?code=abc&state={state}"), &[]).status, 400);

    // The real callback: token exchange, id_token verification, two Set-Cookie headers.
    let r = get(app_port, &format!("/callback?code=abc&state={state}"), &[&pending_pair]);
    assert_eq!(r.status, 302, "{} {:?}", r.body, r.headers);
    let cookies = r.all("Set-Cookie");
    assert_eq!(cookies.len(), 2, "{cookies:?} all={:?}", r.headers);
    assert!(cookies.iter().any(|c| c.starts_with("tinox_oidc_pending=") && c.contains("Max-Age=0")), "pending cookie must be cleared: {cookies:?}");
    let session = cookies.iter().find(|c| c.starts_with("tinox_session=")).expect("session cookie");
    assert!(session.contains("HttpOnly"), "{session}");
    let session_pair = cookie_pair(session);
    let token_req = token_requests.lock().unwrap().last().unwrap().clone();
    assert!(token_req.contains("code=abc") && token_req.contains("code_verifier="), "{token_req}");

    // The session gates the page; garbage and logout do not.
    let r = get(app_port, "/", &[&session_pair]);
    assert_eq!((r.status, r.body.as_str()), (200, "hello user42"));
    assert_eq!(get(app_port, "/", &["tinox_session=junk"]).status, 302);
    let r = get(app_port, "/logout", &[&session_pair]);
    assert_eq!(r.status, 302);
    assert!(r.all("Set-Cookie").iter().any(|c| c.starts_with("tinox_session=") && c.contains("Max-Age=0")), "{:?}", r.headers);
    assert_eq!(get(app_port, "/", &[&session_pair]).status, 302, "a logged-out cookie must not work again");
    let _ = std::fs::remove_dir_all(&dir);
}
