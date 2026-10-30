//! Issue #528: repeated response headers and first-class cookies, observed with the system's curl.
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

fn curl_head(port: u16, path: &str) -> (String, String) {
    for attempt in 0..10 {
        let out = Command::new("curl").args(["-s", "-i", &format!("http://127.0.0.1:{port}{path}")]).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        if !text.is_empty() || attempt == 9 {
            let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
            return (head.to_string(), body.to_string());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    unreachable!()
}

#[test]
fn repeated_headers_and_cookies_reach_an_independent_client() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let dir = std::env::temp_dir().join(format!("tinox-response-headers-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let read_version = |module: &str| {
        std::fs::read_to_string(root.join("crates/tinox-core-ext").join(module).join("tinox.toml"))
            .unwrap()
            .lines()
            .find_map(|l| l.strip_prefix("version = ").map(|v| v.trim_matches('"').to_string()))
            .unwrap()
    };
    std::fs::write(
        dir.join("tinox.toml"),
        format!(
            "[package]\ncompiler = \"2.8.0\"\nname = \"hdrs\"\nversion = \"0.0.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"http_server\"\nversion = \"{}\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"json\"\nversion = \"{}\"\n\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"http_server\"\npath = \"{}\"\n",
            read_version("http_server"),
            read_version("json"),
            root.join("crates/tinox-core-ext/http_server").display()
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("Ctrl.tnx"),
        r#"import tinox.core.http_server

class Ctrl
{
    @GET("/cookies")
    fnc cookies(@HttpContext ctx: HttpContext) -> HttpContext
    {
        var session: Cookie = Cookie::create("session", "abc123")
        session.path = "/"
        session.httpOnly = true
        session.secure = true
        session.sameSite = "Lax"
        session.maxAge = 3600
        var theme: Cookie = Cookie::create("theme", "dark")
        ctx.response.setCookie(session).setCookie(theme).addHeader("X-Multi", "a").addHeader("X-Multi", "b").json("{}")
        return ctx
    }

    @GET("/replace")
    fnc replace(@HttpContext ctx: HttpContext) -> HttpContext
    {
        ctx.response.addHeader("X-Multi", "a").addHeader("X-Multi", "b").header("X-Multi", "only").json("{}")
        return ctx
    }

    @GET("/remove")
    fnc remove(@HttpContext ctx: HttpContext) -> HttpContext
    {
        ctx.response.removeCookie("session", "/", "").json("{}")
        return ctx
    }

    @GET("/invalid")
    fnc invalid(@HttpContext ctx: HttpContext) -> HttpContext
    {
        ctx.response.addHeader("X-Evil", "a\r\nSet-Cookie: pwned=1").json("{}")
        return ctx
    }

    @GET("/raw-injection")
    fnc rawInjection(@HttpContext ctx: HttpContext) -> HttpContext
    {
        ctx.response.header("X-Evil", "a\r\nSet-Cookie: pwned=1").json("{}")
        return ctx
    }

    @GET("/badcookie")
    fnc badCookie(@HttpContext ctx: HttpContext) -> HttpContext
    {
        var c: Cookie = Cookie::create("a b", "v")
        ctx.response.setCookie(c).json("{}")
        return ctx
    }
}
"#,
    )
    .unwrap();
    std::fs::write(dir.join("Main.tnx"), "import Ctrl\nclass Main {\n    fnc main() -> Int32 {\n        return 0\n    }\n}\n").unwrap();
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let install = Command::new(env!("CARGO_BIN_EXE_tinox")).arg("install").current_dir(&dir).output().unwrap();
    assert!(install.status.success(), "{install:?}");
    let exe = dir.join("hdrs");
    let build = Command::new(env!("CARGO_BIN_EXE_tinox"))
        .args(["build", "Main.tnx", "-o"])
        .arg(&exe)
        .env("TINOX_PORT", port.to_string())
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));
    let server = Command::new(&exe).current_dir(&dir).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let _guard = Kill(server);
    std::thread::sleep(Duration::from_millis(600));

    let (head, _) = curl_head(port, "/cookies");
    let lower = head.to_lowercase();
    let cookies: Vec<&str> = head.lines().filter(|l| l.to_lowercase().starts_with("set-cookie:")).collect();
    assert_eq!(cookies.len(), 2, "two separate Set-Cookie lines expected:\n{head}");
    assert!(cookies.contains(&"Set-Cookie: session=abc123; Path=/; Max-Age=3600; Secure; HttpOnly; SameSite=Lax"), "{head}");
    assert!(cookies.contains(&"Set-Cookie: theme=dark"), "{head}");
    assert_eq!(lower.matches("x-multi:").count(), 2, "{head}");

    let (head, _) = curl_head(port, "/replace");
    assert_eq!(head.to_lowercase().matches("x-multi:").count(), 1, "replace must drop appended values:\n{head}");
    assert!(head.contains("X-Multi: only"), "{head}");

    let (head, _) = curl_head(port, "/remove");
    assert!(head.contains("Set-Cookie: session=; Path=/; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT"), "{head}");

    // Invalid input never reaches the wire as a smuggled header.
    for path in ["/invalid", "/raw-injection", "/badcookie"] {
        let (head, _) = curl_head(port, path);
        assert!(head.starts_with("HTTP/1.1 500"), "{path} must be a 500:\n{head}");
        assert!(!head.to_lowercase().contains("pwned"), "{path} leaked an injected header:\n{head}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
