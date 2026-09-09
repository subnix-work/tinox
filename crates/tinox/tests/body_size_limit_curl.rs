//! Regression test for issue #174: a request body over `TINOX_MAX_BODY`
//! (4 MB, `runtime.c`'s `conn_read_request`) used to be silently CLAMPED
//! and handed to the application as a truncated body -- no error, no
//! signal to the handler or client that anything was cut (e.g. a 150 MB
//! upload silently became an unmarked ~4 MB prefix, and a handler
//! computing a checksum over it returned success with a checksum of
//! garbage). Fixed to reject up front with a hard `413 Payload Too Large`
//! instead of quietly corrupting the request, per this project's "no
//! silent garbage" philosophy.
//!
//! Drives a real compiled server with the system's own curl over a real
//! TCP connection (not a simulated in-process client), per this project's
//! "verify against a real, independent implementation" philosophy --
//! exercises both `conn_read_request` call sites (the annotation-driven
//! `tinox_HttpServer_listen` path used here, and the manually-wired
//! `HttpServer.tnx` path, which shares the exact same runtime.c function).

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// See http_server_gc_stress_curl.rs's own copy of this helper for why a
/// hardcoded version string here would be a latent bug.
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

#[test]
fn oversized_body_rejected_with_413_not_silently_truncated() {
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let workdir = std::env::temp_dir().join(format!(
        "tinox-body-size-limit-curl-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(&workdir).expect("mkdir workdir");

    std::fs::write(
        workdir.join("Ctrl.tnx"),
        r#"import tinox.core.http_server;

class Ctrl
{
    @POST
    @Path("/upload")
    fn upload(@HttpContext ctx: HttpContext) -> HttpContext
    {
        ctx.response.status(200).json("{\"ok\":true,\"len\":" + ctx.request.body.len().toString() + "}");
        return ctx;
    }
}
"#,
    )
    .expect("write Ctrl.tnx");

    std::fs::write(
        workdir.join("Main.tnx"),
        r#"import Ctrl;

class Main
{
    fnc main() -> Int32
    {
        return 0;
    }
}
"#,
    )
    .expect("write Main.tnx");

    std::fs::write(
        workdir.join("tinox.toml"),
        format!(
            "[package]\nname = \"body_size_limit_server\"\nversion = \"0.0.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"http_server\"\nversion = \"{}\"\n",
            extended_module_version("http_server"),
        ),
    )
    .expect("write tinox.toml");
    let install = Command::new(tinox)
        .arg("install")
        .current_dir(&workdir)
        .output()
        .expect("spawn install");
    assert!(
        install.status.success(),
        "tinox install failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&install.stdout),
        String::from_utf8_lossy(&install.stderr)
    );

    let exe = workdir.join("body_size_limit_server");
    // Own port, distinct from http_server_gc_stress_curl.rs's default 8080
    // and rest_param_binding_curl.rs's 18099 -- see CLAUDE.md's port-
    // collision note (two test files claiming the same port makes them
    // flaky against each other when cargo runs the test binaries
    // concurrently).
    let build = Command::new(tinox)
        .arg("build")
        .arg(workdir.join("Main.tnx"))
        .arg("-o")
        .arg(&exe)
        .env("TINOX_PORT", "18174")
        .current_dir(&workdir)
        .output()
        .expect("spawn build");
    assert!(
        build.status.success(),
        "build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let child = Command::new(&exe)
        .current_dir(&workdir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server");
    let _guard = KillOnDrop(child);
    std::thread::sleep(Duration::from_millis(500));

    let base = "http://127.0.0.1:18174";

    let curl_status_and_body = |body: &[u8]| -> (String, String) {
        let bodyfile = workdir.join("req_body.bin");
        std::fs::write(&bodyfile, body).expect("write req body");
        for attempt in 0..5 {
            let out = Command::new("curl")
                .args([
                    "-s",
                    "-w",
                    "\n%{http_code}",
                    "-X",
                    "POST",
                    "-H",
                    "Content-Type: application/octet-stream",
                    "--data-binary",
                    &format!("@{}", bodyfile.display()),
                    &format!("{base}/upload"),
                ])
                .output()
                .expect("spawn curl");
            let full = String::from_utf8_lossy(&out.stdout).to_string();
            if let Some((body_part, status_part)) = full.rsplit_once('\n') {
                if !status_part.is_empty() && status_part != "000" {
                    return (status_part.to_string(), body_part.to_string());
                }
            }
            if attempt == 4 {
                return (String::new(), full);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        unreachable!()
    };

    // Normal small body: unaffected, handler runs, sees the real length.
    let (status, body) = curl_status_and_body(b"hello world");
    assert_eq!(status, "200");
    assert_eq!(body, r#"{"ok":true,"len":11}"#);

    // Exactly at the cap (4 MiB): still accepted, boundary is inclusive.
    let exact = vec![b'a'; 4 * 1024 * 1024];
    let (status, body) = curl_status_and_body(&exact);
    assert_eq!(status, "200");
    assert_eq!(body, r#"{"ok":true,"len":4194304}"#);

    // One byte over the cap: rejected with 413, not silently truncated
    // and handed to the handler as a "valid" (but corrupted) request.
    let over = vec![b'a'; 4 * 1024 * 1024 + 1];
    let (status, body) = curl_status_and_body(&over);
    assert_eq!(status, "413");
    assert!(!body.contains("\"ok\":true"), "handler must not have run on an over-cap body: {body}");

    // Well over the cap (5 MiB): same rejection, not a differently-sized
    // truncated success.
    let huge = vec![b'a'; 5 * 1024 * 1024];
    let (status, body) = curl_status_and_body(&huge);
    assert_eq!(status, "413");
    assert!(!body.contains("\"ok\":true"), "handler must not have run on an over-cap body: {body}");

    // Server must still be alive and correctly responsive after rejecting
    // oversized requests, not left in a broken/half-closed state.
    let (status, body) = curl_status_and_body(b"still alive");
    assert_eq!(status, "200");
    assert_eq!(body, r#"{"ok":true,"len":11}"#);

    let _ = std::fs::remove_dir_all(&workdir);
}
