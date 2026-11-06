//! Bug 462: the HTTP/2 server against h2spec (summerwind/h2spec), an independent conformance suite for
//! RFC 7540/9113 and RFC 7541 (HPACK). The server is built from the in-tree `http2_server`/`hpack`
//! sources through `[[overrides]]`, so an unpublished fix is covered without any network access.
//!
//! Skipped (loudly) when `h2spec` is not installed, unless `TINOX_REQUIRE_H2SPEC=1`, which CI sets so a
//! missing tool cannot hide missing coverage. Install: `go install github.com/summerwind/h2spec/cmd/h2spec@v2.2.1`.
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

fn have(tool: &str) -> bool {
    Command::new(tool).arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok()
}

#[test]
fn http2_server_passes_h2spec() {
    if !have("h2spec") || !have("curl") {
        assert!(
            std::env::var("TINOX_REQUIRE_H2SPEC").as_deref() != Ok("1"),
            "TINOX_REQUIRE_H2SPEC=1 but h2spec/curl is not installed"
        );
        eprintln!("SKIP: h2spec (and curl) are required");
        return;
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let dir = std::env::temp_dir().join(format!("tinox-h2spec-{}", std::process::id()));
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
    let mut toml = String::from("[package]\ncompiler = \"2.9.2\"\nname = \"h2conf\"\nversion = \"0.0.0\"\ndescription = \"\"\n");
    for m in ["http2_server", "hpack", "http_server", "json"] {
        toml.push_str(&format!("\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"{m}\"\nversion = \"{}\"\n", version(m)));
    }
    for m in ["http2_server", "http_server", "hpack"] {
        toml.push_str(&format!("\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"{m}\"\npath = \"{}\"\n", ext.join(m).display()));
    }
    std::fs::write(dir.join("tinox.toml"), toml).unwrap();
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reservation.local_addr().unwrap().port();
    std::fs::write(
        dir.join("Main.tnx"),
        format!(
            "import tinox.core.http2_server\n\nclass Main {{\n    fnc main() -> Int32 {{\n        const server: Http2Server = Http2Server::new({port})\n        server.get(\"/\", ctx => {{ ctx.response.text(\"hello\") }})\n        server.post(\"/\", ctx => {{ ctx.response.text(\"posted\") }})\n        server.listen()\n        return 0\n    }}\n}}\n"
        ),
    )
    .unwrap();
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let install = Command::new(tinox).arg("install").current_dir(&dir).output().unwrap();
    assert!(install.status.success(), "{}", String::from_utf8_lossy(&install.stderr));
    let exe = dir.join("h2conf");
    let build = Command::new(tinox).args(["build", "Main.tnx", "-o"]).arg(&exe).current_dir(&dir).output().unwrap();
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));
    drop(reservation);
    let server = Command::new(&exe).current_dir(&dir).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let _guard = Kill(server);

    let url = format!("http://127.0.0.1:{port}/");
    let mut ready = false;
    for _ in 0..50 {
        let out = Command::new("curl").args(["-s", "--http2-prior-knowledge", &url]).output().unwrap();
        if String::from_utf8_lossy(&out.stdout) == "hello" {
            ready = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(ready, "the HTTP/2 test server did not answer a plain request");

    let out = Command::new("h2spec").args(["-h", "127.0.0.1", "-p", &port.to_string(), "-o", "3"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let summary = text.lines().rev().find(|l| l.contains(" tests, ")).unwrap_or("").to_string();
    assert!(out.status.success() && summary.contains(" 0 failed"), "h2spec reported failures: {summary}\n{text}");
    let total: u32 = summary.trim().split(' ').next().and_then(|n| n.parse().ok()).unwrap_or(0);
    assert!(total >= 145, "h2spec ran only {total} cases: {summary}");
    let _ = std::fs::remove_dir_all(&dir);
}
