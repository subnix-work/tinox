//! Issue #654: `HttpServer::listenTls` used to accept, handshake and serve one connection at a
//! time, so every client that connected and never sent a ClientHello stalled the server for the
//! 5 s receive timeout (slow-loris). Real compiled server (local http_server source via an
//! override), idle raw TCP connections, a real TLS request through the system's curl.

use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

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
        .expect("module version")
}

struct KillOnDrop(Child);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn idle_connections_do_not_delay_a_tls_request() {
    if Command::new("curl").arg("--version").output().is_err() {
        eprintln!("SKIP idle_connections_do_not_delay_a_tls_request (curl not installed)");
        return;
    }
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let workdir = std::env::temp_dir().join(format!("tinox-https-concurrent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(&workdir).unwrap();
    let version = extended_module_version("http_server");
    std::fs::write(
        workdir.join("tinox.toml"),
        format!(
            "[package]\ncompiler = \"{}\"\nname = \"https_concurrent\"\nversion = \"0.1.0\"\n\n\
             [[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"http_server\"\nversion = \"{version}\"\n",
            env!("CARGO_PKG_VERSION")
        ),
    )
    .unwrap();
    std::fs::write(
        workdir.join("tinox.overrides.yaml"),
        format!(
            "overrides:\n  \"tinox.core:http_server:{version}\": \"{}\"\n",
            repo_root().join("crates/tinox-core-ext/http_server").display()
        ),
    )
    .unwrap();
    for f in ["selfsigned_cert.pem", "selfsigned_key.pem"] {
        std::fs::copy(repo_root().join("tests/fixtures/tls").join(f), workdir.join(f)).unwrap();
    }
    std::fs::write(
        workdir.join("Main.tnx"),
        format!(
            "import tinox.core.http_server\n\nclass Main\n{{\n    fnc main() -> Int32\n    {{\n        const server: HttpServer = HttpServer::new({port})\n        server.get(\"/hello\", ctx => {{\n            ctx.response.text(\"Hello over TLS!\")\n        }})\n        server.listenTls(\"selfsigned_cert.pem\", \"selfsigned_key.pem\")\n        return 0\n    }}\n}}\n"
        ),
    )
    .unwrap();

    let install = Command::new(tinox).arg("install").current_dir(&workdir).output().unwrap();
    assert!(install.status.success(), "{}", String::from_utf8_lossy(&install.stderr));
    let exe = workdir.join("server");
    let build = Command::new(tinox).args(["build", "Main.tnx", "-o"]).arg(&exe).current_dir(&workdir).output().unwrap();
    assert!(build.status.success(), "build failed:\n{}", String::from_utf8_lossy(&build.stderr));

    let child = Command::new(&exe)
        .current_dir(&workdir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server");
    let _guard = KillOnDrop(child);
    let mut up = false;
    for _ in 0..50 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            up = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(up, "server did not start");

    // Three clients that connect and never speak TLS.
    let _idle: Vec<TcpStream> = (0..3).map(|_| TcpStream::connect(("127.0.0.1", port)).unwrap()).collect();
    std::thread::sleep(Duration::from_millis(300));

    let start = Instant::now();
    let out = Command::new("curl")
        .args(["-sk", "--max-time", "20", &format!("https://127.0.0.1:{port}/hello")])
        .output()
        .unwrap();
    let elapsed = start.elapsed();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "Hello over TLS!", "curl: {:?}", out);
    // Serially, the three idle sockets cost 3 x 5 s before this request is even looked at.
    assert!(elapsed < Duration::from_secs(3), "TLS request took {elapsed:?} behind idle connections");
    let _ = std::fs::remove_dir_all(&workdir);
}
