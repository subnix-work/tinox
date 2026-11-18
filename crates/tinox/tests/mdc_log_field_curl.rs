//! `@LogField` (MDC) on a real compiled REST server, driven by the system's own curl.
//!
//! A `@LogField` parameter puts its raw bound value into the log context for the request, so a
//! log record written by the handler carries it as a JSON field. The context must not survive
//! into the next request on a reused epoll worker, and a request rejected with 400 (the handler
//! never runs) must not leave a field behind either. Records are read from the server's stdout.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root")
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

const PORT: &str = "18100";

#[test]
fn log_field_reaches_log_records_and_does_not_leak() {
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let workdir = std::env::temp_dir().join(format!("tinox-mdc-log-field-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(&workdir).expect("mkdir workdir");
    let version = extended_module_version("http_server");
    std::fs::write(
        workdir.join("tinox.toml"),
        format!(
            "[package]\ncompiler = \"{}\"\nname = \"mdc_log_field\"\nversion = \"0.1.0\"\ndescription = \"\"\n\n\
             [[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"http_server\"\nversion = \"{version}\"\n",
            env!("CARGO_PKG_VERSION")
        ),
    )
    .expect("write tinox.toml");
    std::fs::write(
        workdir.join("tinox.overrides.yaml"),
        format!(
            "overrides:\n  \"tinox.core:http_server:{version}\": \"{}\"\n",
            repo_root().join("crates/tinox-core-ext/http_server").display()
        ),
    )
    .expect("write overrides");
    std::fs::write(
        workdir.join("Ctrl.tnx"),
        r#"import tinox.core.http_server
import tinox.core.logger

class Ctrl
{
    @GET
    @Path("/orders/:orderId")
    @Produces("application/json")
    fn order(@PathParam @LogField orderId: Int64, @QueryParam @LogField("customer") c: String) -> String
    {
        Logger::info(Logger::new("Ctrl"), "order requested")
        return "\"ok\""
    }

    @GET
    @Path("/plain")
    @Produces("application/json")
    fn plain() -> String
    {
        Logger::info(Logger::new("Ctrl"), "plain request")
        return "\"ok\""
    }
}
"#,
    )
    .expect("write Ctrl.tnx");
    std::fs::write(
        workdir.join("Main.tnx"),
        "import Ctrl\n\nclass Main\n{\n    fnc main() -> Int32\n    {\n        return 0\n    }\n}\n",
    )
    .expect("write Main.tnx");

    let install = Command::new(tinox).arg("install").current_dir(&workdir).output().expect("spawn install");
    assert!(install.status.success(), "install failed: {}", String::from_utf8_lossy(&install.stderr));
    let exe = workdir.join("mdc_server");
    let build = Command::new(tinox)
        .args(["build"])
        .arg(workdir.join("Main.tnx"))
        .arg("-o")
        .arg(&exe)
        .env("TINOX_PORT", PORT)
        .current_dir(&workdir)
        .output()
        .expect("spawn build");
    assert!(
        build.status.success(),
        "build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let log_path = workdir.join("server.log");
    let log = std::fs::File::create(&log_path).expect("create log");
    let child = Command::new(&exe)
        .current_dir(&workdir)
        .env("TINOX_LOG_JSON", "true")
        .stdout(Stdio::from(log))
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server");
    let _guard = KillOnDrop(child);
    std::thread::sleep(Duration::from_millis(500));

    let status = |url: &str| -> String {
        for attempt in 0..5 {
            let out = Command::new("curl")
                .args(["-s", "-o", "/dev/null", "-w", "%{http_code}", url])
                .output()
                .expect("spawn curl");
            let code = String::from_utf8_lossy(&out.stdout).to_string();
            if (!code.is_empty() && code != "000") || attempt == 4 {
                return code;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        unreachable!()
    };
    let base = format!("http://127.0.0.1:{PORT}");
    // Alternate requests with and without fields so a leaking worker would show up.
    for round in 0..6 {
        assert_eq!(status(&format!("{base}/orders/{}?c=acme{round}", 100 + round)), "200");
        assert_eq!(status(&format!("{base}/plain")), "200");
    }
    // A request rejected with 400 never reaches the handler, and the next plain request is clean.
    assert_eq!(status(&format!("{base}/orders/abc?c=x")), "400");
    assert_eq!(status(&format!("{base}/plain")), "200");
    std::thread::sleep(Duration::from_millis(300));

    let text = std::fs::read_to_string(&log_path).expect("read log");
    let records: Vec<&str> = text.lines().filter(|l| l.contains("\"logger\":\"Ctrl\"")).collect();
    let orders: Vec<&&str> = records.iter().filter(|l| l.contains("order requested")).collect();
    let plains: Vec<&&str> = records.iter().filter(|l| l.contains("plain request")).collect();
    assert_eq!(orders.len(), 6, "log:\n{text}");
    assert_eq!(plains.len(), 7, "log:\n{text}");
    for round in 0..6 {
        let want_id = format!("\"orderId\":\"{}\"", 100 + round);
        let want_customer = format!("\"customer\":\"acme{round}\"");
        assert!(
            orders.iter().any(|l| l.contains(&want_id) && l.contains(&want_customer)),
            "missing {want_id} {want_customer} in:\n{text}"
        );
    }
    for line in &plains {
        assert!(
            !line.contains("orderId") && !line.contains("customer"),
            "MDC leaked into a later request: {line}"
        );
    }
    let _ = std::fs::remove_dir_all(&workdir);
}
