//! Real, curl-based regression coverage for issue #261: `[metrics] enabled
//! = true` together with a real `@GET` route used to fail codegen with
//! `opt: ... invalid redefinition of function 'tinox_metrics_prometheus'`
//! -- `emit_route_code`'s `/metrics` shim re-declared both
//! `tinox_metrics_prometheus` (already declared unconditionally in the
//! module preamble) and `tinox_HttpServer_new` (already declared a few
//! lines above in the same function) on top of an existing declaration,
//! which `opt` hard-rejects even for an identical signature. Compiles a
//! project combining both features and drives it with a real curl, per
//! this project's "verify against real, independent systems" rule -- a
//! compile-only check wouldn't confirm the `/metrics` endpoint the fix is
//! actually supposed to preserve still works.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

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
fn metrics_enabled_with_real_routes_compiles_and_serves() {
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let workdir = std::env::temp_dir().join(format!(
        "tinox-metrics-with-routes-curl-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(&workdir).expect("mkdir workdir");

    std::fs::write(
        workdir.join("Ctrl.tnx"),
        r#"import tinox.core.http_server;

class Ctrl
{
    @GET
    @Path("/hello")
    fn hello() -> String { return "hi"; }
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
            "[package]\nname = \"metrics_with_routes\"\nversion = \"0.0.0\"\ndescription = \"\"\n\n\
             [metrics]\nenabled = true\n\n\
             [[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"http_server\"\nversion = \"{}\"\n",
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

    let exe = workdir.join("metrics_with_routes");
    // Own, unclaimed port -- see rest_param_binding_curl.rs's own comment
    // on why TINOX_PORT is needed at *build* time and why every curl-based
    // integration test here must claim a distinct one.
    let build = Command::new(tinox)
        .arg("build")
        .arg(workdir.join("Main.tnx"))
        .arg("-o")
        .arg(&exe)
        .env("TINOX_PORT", "18201")
        .current_dir(&workdir)
        .output()
        .expect("spawn build");
    assert!(
        build.status.success(),
        "build failed (this is the exact failure mode of issue #261 if it regresses):\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&build.stdout).contains("invalid redefinition"),
        "regression: duplicate LLVM declare reappeared"
    );

    let child = Command::new(&exe)
        .current_dir(&workdir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server");
    let _guard = KillOnDrop(child);
    std::thread::sleep(Duration::from_millis(500));

    let base = "http://127.0.0.1:18201";
    let curl_body = |url: &str| -> String {
        for attempt in 0..5 {
            let out = Command::new("curl").args(["-s", url]).output().expect("spawn curl");
            let body = String::from_utf8_lossy(&out.stdout).to_string();
            if !body.is_empty() || attempt == 4 {
                return body;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        unreachable!()
    };

    // The route added specifically to trigger the bug's duplicate-declare
    // guard (emit_route_code only reaches the metrics block when routes
    // are non-empty) still works.
    assert_eq!(curl_body(&format!("{base}/hello")), "\"hi\"");

    // The /metrics endpoint itself is actually wired up (200, not the
    // 404/connection-refused it would give if the shim never got
    // registered) -- no @Counter/@Histogram/@Gauge is declared in this
    // fixture, so an empty body is the correct real output, not a masked
    // failure.
    let curl_status = |url: &str| -> String {
        for attempt in 0..5 {
            let out = Command::new("curl")
                .args(["-s", "-o", "/dev/null", "-w", "%{http_code}", url])
                .output()
                .expect("spawn curl");
            let status = String::from_utf8_lossy(&out.stdout).to_string();
            if (!status.is_empty() && status != "000") || attempt == 4 {
                return status;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        unreachable!()
    };
    assert_eq!(curl_status(&format!("{base}/metrics")), "200");
    assert_eq!(curl_body(&format!("{base}/metrics")), "");

    let _ = std::fs::remove_dir_all(&workdir);
}
