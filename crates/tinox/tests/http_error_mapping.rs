//! Issue #524: `@ErrorMap` maps "CODE: message" handler errors to a status and a stable error
//! envelope; everything else is a safe, correlated 500. Observed with curl.
#![cfg(unix)]

use serde_json::Value;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

struct Kill(Child);
impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn get(port: u16, path: &str, extra: &[&str]) -> (u16, String, Value) {
    for attempt in 0..10 {
        let out = Command::new("curl")
            .args(["-s", "-i"])
            .args(extra)
            .arg(format!("http://127.0.0.1:{port}{path}"))
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
            return (status, head.to_string(), serde_json::from_str(body).unwrap_or(Value::Null));
        }
        if attempt == 9 {
            panic!("no response for {path}");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    unreachable!()
}

#[test]
fn mapped_codes_become_envelopes_and_unknown_failures_are_safe_500s() {
    let dir = std::env::temp_dir().join(format!("tinox-error-mapping-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let version = |m: &str| {
        std::fs::read_to_string(root.join("crates/tinox-core-ext").join(m).join("tinox.toml"))
            .unwrap()
            .lines()
            .find_map(|l| l.strip_prefix("version = ").map(|v| v.trim_matches('"').to_string()))
            .unwrap()
    };
    std::fs::write(
        dir.join("tinox.toml"),
        format!(
            "[package]\ncompiler = \"2.8.0\"\nname = \"errmap\"\nversion = \"0.0.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"http_server\"\nversion = \"{}\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"json\"\nversion = \"{}\"\n\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"http_server\"\npath = \"{}\"\n",
            version("http_server"),
            version("json"),
            root.join("crates/tinox-core-ext/http_server").display()
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("Errors.tnx"),
        "@ErrorMap(\"USER_NOT_FOUND\", 404)\n@ErrorMap(\"ORDER_CONFLICT\", 409)\nclass Errors { var unused: Int64 }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("Ctrl.tnx"),
        r#"import tinox.core.http_server
import Errors

class Ctrl
{
    @GET("/missing")
    fnc missing(@HttpContext ctx: HttpContext) -> HttpContext
    {
        throw "USER_NOT_FOUND: user \"5\" does not exist"
        return ctx
    }

    @GET("/missing-scalar")
    fnc missingScalar() -> Int64
    {
        throw "USER_NOT_FOUND: user 6 does not exist"
        return 0
    }

    @GET("/conflict")
    fnc conflict(@HttpContext ctx: HttpContext) -> HttpContext
    {
        throw "ORDER_CONFLICT: order already shipped"
        return ctx
    }

    @GET("/secret")
    fnc secret(@HttpContext ctx: HttpContext) -> HttpContext
    {
        throw "connection to db failed password=hunter2"
        return ctx
    }

    @GET("/unregistered")
    fnc unregistered(@HttpContext ctx: HttpContext) -> HttpContext
    {
        throw "SOMETHING_ELSE: nope"
        return ctx
    }

    @GET("/fine")
    fnc fine(@HttpContext ctx: HttpContext) -> HttpContext
    {
        ctx.response.status(200).json("{\"ok\":true}")
        return ctx
    }
}
"#,
    )
    .unwrap();
    std::fs::write(dir.join("Main.tnx"), "import Ctrl\nclass Main {\n    fnc main() -> Int32 {\n        return 0\n    }\n}\n").unwrap();
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    assert!(Command::new(env!("CARGO_BIN_EXE_tinox")).arg("install").current_dir(&dir).output().unwrap().status.success());
    let exe = dir.join("errmap");
    let build = Command::new(env!("CARGO_BIN_EXE_tinox"))
        .args(["build", "Main.tnx", "-o"])
        .arg(&exe)
        .env("TINOX_PORT", port.to_string())
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));
    let log = std::fs::File::create(dir.join("server.log")).unwrap();
    let server = Command::new(&exe).current_dir(&dir).stdout(log.try_clone().unwrap()).stderr(log).spawn().unwrap();
    let _guard = Kill(server);
    std::thread::sleep(Duration::from_millis(600));

    for path in ["/missing", "/missing-scalar"] {
        let (status, _, body) = get(port, path, &[]);
        assert_eq!(status, 404, "{path}");
        assert_eq!(body["error"]["code"], "USER_NOT_FOUND", "{path}");
        assert!(body["error"]["message"].as_str().unwrap().contains("does not exist"), "{body}");
        assert!(body["error"]["correlationId"].as_str().unwrap().starts_with("req-"), "{body}");
    }
    let (status, _, body) = get(port, "/conflict", &["-H", "X-Request-Id: trace-42"]);
    assert_eq!((status, body["error"]["code"].as_str(), body["error"]["correlationId"].as_str()), (409, Some("ORDER_CONFLICT"), Some("trace-42")));
    for path in ["/secret", "/unregistered"] {
        let (status, head, body) = get(port, path, &[]);
        assert_eq!(status, 500, "{path}");
        assert_eq!(body["error"]["code"], "INTERNAL");
        assert_eq!(body["error"]["message"], "Internal error");
        assert!(!head.contains("hunter2") && !body.to_string().contains("hunter2"), "internal text leaked");
    }
    // A failure does not poison the next request.
    let (status, _, body) = get(port, "/fine", &[]);
    assert_eq!((status, body["ok"].as_bool()), (200, Some(true)));
    let _ = std::fs::remove_dir_all(&dir);
}
