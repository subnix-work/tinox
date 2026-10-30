//! Issue #529: @Cors / @Csrf enforced at the HTTP/1 request boundary, exercised with curl as an
//! independent browser-style client (preflight, credentialed requests, origin rejection, CSRF).
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

fn request(port: u16, method: &str, path: &str, headers: &[&str]) -> (String, String) {
    for attempt in 0..10 {
        let mut cmd = Command::new("curl");
        cmd.args(["-s", "-i", "-X", method, &format!("http://127.0.0.1:{port}{path}")]);
        for h in headers {
            cmd.args(["-H", h]);
        }
        if method == "POST" {
            cmd.args(["-d", "x=1"]);
        }
        let text = String::from_utf8_lossy(&cmd.output().unwrap().stdout).to_string();
        if !text.is_empty() || attempt == 9 {
            let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
            return (head.to_string(), body.to_string());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    unreachable!()
}

fn status(head: &str) -> u16 {
    head.split_whitespace().nth(1).unwrap().parse().unwrap()
}
fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

#[test]
fn cors_and_csrf_policies_hold_for_a_browser_style_client() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let dir = std::env::temp_dir().join(format!("tinox-browser-security-{}", std::process::id()));
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
            "[package]\ncompiler = \"2.8.0\"\nname = \"sec\"\nversion = \"0.0.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"http_server\"\nversion = \"{}\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"json\"\nversion = \"{}\"\n\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"http_server\"\npath = \"{}\"\n",
            read_version("http_server"),
            read_version("json"),
            root.join("crates/tinox-core-ext/http_server").display()
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("Policy.tnx"),
        "@Cors([\"https://app.example.com\"], true, 600, [\"Content-Type\", \"X-Custom\"])\n@Csrf([\"https://admin.example.com\"])\nclass Policy {\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("Ctrl.tnx"),
        r#"import tinox.core.http_server

class Ctrl
{
    @GET("/api/data")
    fnc read(@HttpContext ctx: HttpContext) -> HttpContext
    {
        ctx.response.json("{\"ran\":\"read\"}")
        return ctx
    }

    @POST("/api/data")
    fnc write(@HttpContext ctx: HttpContext) -> HttpContext
    {
        ctx.response.json("{\"ran\":\"write\"}")
        return ctx
    }
}
"#,
    )
    .unwrap();
    std::fs::write(dir.join("Main.tnx"), "import Ctrl\nimport Policy\nclass Main {\n    fnc main() -> Int32 {\n        return 0\n    }\n}\n").unwrap();
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let install = Command::new(env!("CARGO_BIN_EXE_tinox")).arg("install").current_dir(&dir).output().unwrap();
    assert!(install.status.success(), "{install:?}");
    let exe = dir.join("sec");
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

    let app = "Origin: https://app.example.com";
    let same = format!("Origin: http://127.0.0.1:{port}");

    // Preflight: allowed origin/method/header.
    let (head, body) = request(port, "OPTIONS", "/api/data", &[app, "Access-Control-Request-Method: POST", "Access-Control-Request-Headers: content-type"]);
    assert_eq!(status(&head), 204, "{head}");
    assert_eq!(header(&head, "Access-Control-Allow-Origin"), Some("https://app.example.com"), "{head}");
    assert_eq!(header(&head, "Access-Control-Allow-Credentials"), Some("true"));
    assert_eq!(header(&head, "Access-Control-Allow-Methods"), Some("POST"));
    assert_eq!(header(&head, "Access-Control-Allow-Headers"), Some("content-type"));
    assert_eq!(header(&head, "Access-Control-Max-Age"), Some("600"));
    assert_eq!(header(&head, "Vary"), Some("Origin"));
    assert!(body.is_empty(), "{body}");

    // Preflight refusals use the standard error envelope and never leak allow-headers.
    let refusals: [(&[&str], &str); 3] = [
        (&["Origin: https://evil.example", "Access-Control-Request-Method: POST"], "CORS_ORIGIN_DENIED"),
        (&[app, "Access-Control-Request-Method: POST", "Access-Control-Request-Headers: x-secret"], "CORS_HEADER_DENIED"),
        (&[app, "Access-Control-Request-Method: DELETE"], "CORS_METHOD_DENIED"),
    ];
    for (headers, code) in refusals {
        let (head, body) = request(port, "OPTIONS", "/api/data", headers);
        assert_eq!(status(&head), 403, "{code}: {head}");
        assert!(body.contains(code) && body.contains("correlationId"), "{code}: {body}");
        assert!(header(&head, "Access-Control-Allow-Origin").is_none(), "{code}: {head}");
    }

    // Credentialed actual request from the allowed origin.
    let (head, body) = request(port, "GET", "/api/data", &[app, "Cookie: session=abc"]);
    assert_eq!(status(&head), 200, "{head}");
    assert_eq!(header(&head, "Access-Control-Allow-Origin"), Some("https://app.example.com"));
    assert_eq!(header(&head, "Access-Control-Allow-Credentials"), Some("true"));
    assert!(body.contains("read"), "{body}");

    // A foreign origin is rejected before the handler runs, with or without credentials.
    let (head, body) = request(port, "GET", "/api/data", &["Origin: https://evil.example"]);
    assert_eq!(status(&head), 403, "{head}");
    assert!(body.contains("CORS_ORIGIN_DENIED") && !body.contains("read"), "{body}");
    let (head, _) = request(port, "GET", "/api/data", &["Origin: null"]);
    assert_eq!(status(&head), 403, "opaque origin must not pass: {head}");

    // Same-origin and non-browser (no Origin) requests carry no CORS headers.
    let (head, _) = request(port, "GET", "/api/data", &[&same]);
    assert_eq!(status(&head), 200);
    assert!(header(&head, "Access-Control-Allow-Origin").is_none(), "{head}");
    let (head, _) = request(port, "GET", "/api/data", &[]);
    assert_eq!(status(&head), 200);

    // CSRF: unsafe requests carrying cookies need a trusted Origin/Referer.
    let cookie = "Cookie: session=abc";
    let accepted: [(&[&str], &str); 5] = [
        (&[cookie, app], "allowed CORS origin"),
        (&[cookie, &same], "same origin"),
        (&[cookie, "Referer: https://admin.example.com/page"], "trusted referer"),
        (&[cookie, "Origin: https://admin.example.com"], "trusted csrf origin"),
        (&[], "no cookie (token-style client)"),
    ];
    for (headers, why) in accepted {
        let (head, body) = request(port, "POST", "/api/data", headers);
        assert_eq!(status(&head), 200, "{why}: {head}");
        assert!(body.contains("write"), "{why}: {body}");
    }
    let rejected: [(&[&str], &str, &str); 4] = [
        (&[cookie, "Origin: https://evil.example"], "CORS_ORIGIN_DENIED", "foreign origin"),
        (&[cookie], "CSRF_REJECTED", "cookie without Origin/Referer"),
        (&[cookie, "Referer: https://evil.example/x"], "CSRF_REJECTED", "foreign referer"),
        (&[cookie, "Referer: not-a-url"], "CSRF_REJECTED", "garbled referer"),
    ];
    for (headers, code, why) in rejected {
        let (head, body) = request(port, "POST", "/api/data", headers);
        assert_eq!(status(&head), 403, "{why}: {head}");
        assert!(body.contains(code) && !body.contains("write"), "{why}: {body}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
