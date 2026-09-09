//! Regression test for issue #140: `tinox_HttpServer_listen`'s epoll worker
//! threads used to crash inside GC-managed memory after a handful of
//! requests to an allocation-heavy `@GET` route (root cause: several
//! `static __thread` buffers hold pointers to GC-managed memory, but Boehm
//! GC does not automatically scan `__thread`/TLS storage as roots -- fixed
//! by explicitly `GC_add_roots`-registering them once per thread, see
//! `tinox_gc_register_thread_roots` in runtime.c).
//!
//! Drives a real compiled server with the SYSTEM'S OWN curl over many
//! sequential real HTTP requests (not a simulated in-process client), per
//! this project's "verify against a real, independent implementation"
//! philosophy -- this exact bug was previously invisible to any
//! self-consistent/simulated test and was only found via live curl load.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// Reads the current published version out of the module's own
/// `crates/tinox-core-ext/<module>/tinox.toml` instead of a hardcoded
/// string -- see amqp10_consumer_annotation.rs's copy of this helper for
/// why a hardcoded version here is a latent bug, not just style.
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

/// tinox_HttpServer_listen always binds port 8080 with no configuration
/// knob on this path, so this test has no way to dodge a port collision the
/// way the httpServerCreate(0)-based e2e fixtures do. Found live (issue
/// #226): an unrelated process already holding 0.0.0.0:8080 makes every
/// curl request in the loop below land on THAT process instead of the
/// compiled test server (which, before the #226 fix, failed its own bind
/// silently and kept running) -- collapsing the success count to exactly
/// 0/300, indistinguishable at a glance from the real GC-crash regression
/// (#140) this test exists to catch. Check for that up front and SKIP with
/// an unambiguous reason instead of asserting a false "server likely
/// crashed" -- matches postgres_transactional.rs's own SKIP-on-missing-
/// prerequisite pattern.
fn port_8080_available() -> bool {
    std::net::TcpListener::bind("0.0.0.0:8080").is_ok()
}

#[test]
fn http_server_survives_heavy_allocation_load() {
    if !port_8080_available() {
        eprintln!(
            "SKIP http_server_survives_heavy_allocation_load (port 8080 is already in use by another process on this machine -- not a tinox regression, see issue #226)"
        );
        return;
    }

    let tinox = env!("CARGO_BIN_EXE_tinox");
    let workdir = std::env::temp_dir().join(format!(
        "tinox-http-gc-stress-curl-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(&workdir).expect("mkdir workdir");

    // A route that allocates heavily (many small String concatenations,
    // each a fresh GC allocation) before ever touching ctx.response --
    // reproduces the crash without needing the Map-based header-setting
    // path, isolating the GC-root issue itself.
    std::fs::write(
        workdir.join("Ctrl.tnx"),
        r#"import tinox.core.http_server;

class Ctrl
{
    @GET("/heavy")
    fnc heavy(@HttpContext ctx: HttpContext) -> HttpContext
    {
        var s: String = "";
        var i: Int64 = 0;
        while i < 3000
        {
            s = s + fromCharCode(65 + (i % 26));
            i = i + 1;
        }
        ctx.response.status(200).json("{\"len\":" + s.len().toString() + "}");
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

    // Core/extended stdlib split: Ctrl.tnx imports tinox.core.http_server
    // (extended-tier), so it needs a declared+installed dependency now.
    std::fs::write(
        workdir.join("tinox.toml"),
        format!(
            "[package]\nname = \"heavy_server\"\nversion = \"0.0.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"http_server\"\nversion = \"{}\"\n",
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

    let exe = workdir.join("heavy_server");
    let build = Command::new(tinox)
        .arg("build")
        .arg(workdir.join("Main.tnx"))
        .arg("-o")
        .arg(&exe)
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

    // tinox_HttpServer_listen always binds port 8080 (no port-configuration
    // knob on this path -- matches the annotation-driven server's actual
    // behavior, not a test-specific choice).
    let port = 8080;

    let mut success = 0u32;
    for _ in 0..300 {
        let out = Command::new("curl")
            .args([
                "-s",
                "-o",
                "/dev/null",
                "-w",
                "%{http_code}",
                &format!("http://127.0.0.1:{port}/heavy"),
            ])
            .output()
            .expect("spawn curl");
        if String::from_utf8_lossy(&out.stdout) == "200" {
            success += 1;
        }
    }

    // A handful of transient connection hiccups from a tight sequential
    // curl loop are tolerated; a crash (the actual bug) collapses this to
    // near-zero successes for the remainder of the run, not a handful of
    // isolated misses.
    assert!(
        success >= 290,
        "expected almost all 300 requests to succeed, got {success} -- server likely crashed (issue #140)"
    );

    // The process must still be alive and responsive after the load, not
    // just "some requests happened to land before it died".
    let final_check = Command::new("curl")
        .args(["-s", &format!("http://127.0.0.1:{port}/heavy")])
        .output()
        .expect("spawn final curl");
    assert!(
        String::from_utf8_lossy(&final_check.stdout).contains("\"len\":3000"),
        "server not responsive after load: {final_check:?}"
    );

    let _ = std::fs::remove_dir_all(&workdir);
}
