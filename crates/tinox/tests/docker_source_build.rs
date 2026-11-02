//! `tinox docker --source-build` against a real Docker daemon (issue #309).
//!
//! Opt-in: it needs docker, network access (`tinox install` inside the builder) and builds
//! the builder image from `docker/builder/Dockerfile` when it is missing, so it only runs
//! with `TINOX_DOCKER_E2E=1` (`make docker-e2e`). Without it the test reports SKIP.
//!
//! Checked from outside with independent clients (`docker`, `curl`): the image builds from
//! a minimal reviewed context, runs as the configured non-root user with a read-only /app,
//! serves REST backed by SQLite migrated on startup, keeps its data in the /app/data volume
//! across container restarts, and stops promptly under `--init`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn docker(args: &[&str]) -> Output {
    Command::new("docker").args(args).output().expect("spawn docker")
}

fn curl(args: &[&str]) -> Option<String> {
    let out = Command::new("curl").args(["-s", "--max-time", "2"]).args(args).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

struct Cleanup {
    containers: Vec<String>,
    image: String,
    volume: String,
    dir: PathBuf,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        for c in &self.containers {
            let _ = docker(&["rm", "-f", c]);
        }
        let _ = docker(&["rmi", "-f", &self.image]);
        let _ = docker(&["volume", "rm", "-f", &self.volume]);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn start(name: &str, image: &str, volume: &str) -> String {
    let run = docker(&["run", "-d", "--init", "--name", name, "-p", "127.0.0.1::8080", "-v", &format!("{volume}:/app/data"), image]);
    assert!(run.status.success(), "docker run failed: {}", text(&run));
    let port_out = docker(&["port", name, "8080/tcp"]);
    let s = String::from_utf8_lossy(&port_out.stdout).to_string();
    let port = s.lines().next().and_then(|l| l.rsplit(':').next()).map(|p| p.trim().to_string()).unwrap_or_else(|| panic!("port: {s}"));
    let url = format!("http://127.0.0.1:{port}/notes");
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(30) {
        if curl(&[&url]).is_some() {
            return url;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let logs = docker(&["logs", name]);
    panic!("container never became ready:\n{}", text(&logs));
}

#[test]
fn source_build_image_runs_non_root_serves_rest_and_keeps_data() {
    if std::env::var("TINOX_DOCKER_E2E").as_deref() != Ok("1") {
        eprintln!("SKIP docker source-build e2e (set TINOX_DOCKER_E2E=1 to run it)");
        return;
    }
    let root = repo_root();
    let version = env!("CARGO_PKG_VERSION");
    let builder = format!("ghcr.io/subnix-work/tinox-builder:{version}-trixie");
    if !docker(&["image", "inspect", &builder]).status.success() {
        let build = Command::new("docker")
            .args(["build", "-f", "docker/builder/Dockerfile", "-t", &builder, "."])
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(build.status.success(), "builder image build failed: {}", text(&build));
    }

    let id = std::process::id();
    let dir = std::env::temp_dir().join(format!("tinox-docker-e2e-{id}"));
    let _ = std::fs::remove_dir_all(&dir);
    let fixture = root.join("tests/fixtures/docker_source_build");
    let status = Command::new("cp").arg("-r").arg(&fixture).arg(&dir).status().unwrap();
    assert!(status.success());
    let image = format!("tinox-docker-e2e-{id}:test");
    let volume = format!("tinox-docker-e2e-data-{id}");
    let (c1, c2) = (format!("tinox-docker-e2e-a-{id}"), format!("tinox-docker-e2e-b-{id}"));
    let _cleanup = Cleanup { containers: vec![c1.clone(), c2.clone()], image: image.clone(), volume: volume.clone(), dir: dir.clone() };

    let build = Command::new(env!("CARGO_BIN_EXE_tinox"))
        .args(["docker", "--source-build", "--tag", &image])
        .current_dir(&dir)
        .output()
        .unwrap();
    let out = text(&build);
    assert!(build.status.success(), "tinox docker --source-build failed:\n{out}");
    assert!(out.contains("Verifying linked libraries resolve inside the image... ok"), "{out}");
    assert!(out.contains("Container user: uid 10001"), "{out}");
    // The reviewed build context lists the opted-in migrations.
    assert!(out.contains("migrations/changelog.yaml"), "{out}");

    let size = docker(&["image", "inspect", &image, "--format", "{{.Size}}"]);
    let bytes: u64 = String::from_utf8_lossy(&size.stdout).trim().parse().unwrap();
    assert!(bytes < 200 * 1024 * 1024, "image unexpectedly large: {bytes} bytes");

    // Migrated on startup, served over HTTP, written through the ORM.
    let url = start(&c1, &image, &volume);
    assert_eq!(curl(&[&url]).unwrap().trim(), "[]");
    let created = curl(&["-X", "POST", "-H", "Content-Type: application/json", "-d", r#"{"text":"hello"}"#, &url]).unwrap();
    assert!(created.contains("\"text\":\"hello\""), "{created}");

    // Unprivileged user, read-only application directory.
    let id_out = docker(&["exec", &c1, "id", "-u"]);
    assert_eq!(String::from_utf8_lossy(&id_out.stdout).trim(), "10001");
    assert!(!docker(&["exec", &c1, "touch", "/app/not-writable"]).status.success());
    assert!(docker(&["exec", &c1, "test", "-f", "/app/build-info.json"]).status.success());

    // Stops promptly (PID 1 would ignore SIGTERM without --init).
    let stopping = Instant::now();
    assert!(docker(&["stop", &c1]).status.success());
    assert!(stopping.elapsed() < Duration::from_secs(5), "stop took {:?}", stopping.elapsed());

    // Data lives in the volume, not in the container.
    let url2 = start(&c2, &image, &volume);
    assert!(curl(&[&url2]).unwrap().contains("\"text\":\"hello\""));
}


// ---- Database and TLS images (#309): the runtime libraries the build selects must really be in the image ----

struct Net(String);
impl Drop for Net {
    fn drop(&mut self) {
        let _ = docker(&["network", "rm", &self.0]);
    }
}

fn opt_in() -> bool {
    if std::env::var("TINOX_DOCKER_E2E").as_deref() != Ok("1") {
        eprintln!("SKIP docker source-build e2e (set TINOX_DOCKER_E2E=1 to run it)");
        return false;
    }
    true
}

fn ensure_builder() {
    let builder = format!("ghcr.io/subnix-work/tinox-builder:{}-trixie", env!("CARGO_PKG_VERSION"));
    if !docker(&["image", "inspect", &builder]).status.success() {
        let build = Command::new("docker").args(["build", "-f", "docker/builder/Dockerfile", "-t", &builder, "."]).current_dir(repo_root()).output().unwrap();
        assert!(build.status.success(), "builder image build failed: {}", text(&build));
    }
}

/// Copies the notes fixture, swaps its [database] section and builds the image.
fn build_variant(tag: &str, database: &str) -> (PathBuf, String) {
    let id = std::process::id();
    let dir = std::env::temp_dir().join(format!("tinox-docker-e2e-{tag}-{id}"));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(Command::new("cp").arg("-r").arg(repo_root().join("tests/fixtures/docker_source_build")).arg(&dir).status().unwrap().success());
    let manifest = std::fs::read_to_string(dir.join("tinox.toml")).unwrap();
    let swapped = manifest.replace("driver = \"sqlite\"\nurl = \"data/notes.db\"", database);
    assert_ne!(manifest, swapped, "fixture [database] section changed");
    std::fs::write(dir.join("tinox.toml"), swapped).unwrap();
    let image = format!("tinox-docker-e2e-{tag}-{id}:test");
    let out = Command::new(env!("CARGO_BIN_EXE_tinox")).args(["docker", "--source-build", "--tag", &image]).current_dir(&dir).output().unwrap();
    let t = text(&out);
    assert!(out.status.success(), "{tag}: source build failed:\n{t}");
    assert!(t.contains("Verifying linked libraries resolve inside the image... ok"), "{tag}: {t}");
    assert!(t.contains("Container user: uid 10001"), "{tag}: {t}");
    (dir, image)
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(60) {
        if ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    panic!("{what} never became ready");
}

fn run_app_on_network(name: &str, image: &str, network: &str) -> String {
    let run = docker(&["run", "-d", "--init", "--name", name, "--network", network, "-p", "127.0.0.1::8080", image]);
    assert!(run.status.success(), "{}", text(&run));
    let port_out = docker(&["port", name, "8080/tcp"]);
    let s = String::from_utf8_lossy(&port_out.stdout).to_string();
    let port = s.lines().next().and_then(|l| l.rsplit(':').next()).map(|p| p.trim().to_string()).unwrap_or_else(|| panic!("port: {s}"));
    let url = format!("http://127.0.0.1:{port}/notes");
    let mut last = String::new();
    wait_for("the app", || {
        last = text(&docker(&["logs", name]));
        curl(&[&url]).is_some()
    });
    let _ = last;
    url
}

#[test]
fn source_build_postgres_image_migrates_and_writes() {
    if !opt_in() {
        return;
    }
    ensure_builder();
    let id = std::process::id();
    let net = Net(format!("tinox-e2e-net-pg-{id}"));
    assert!(docker(&["network", "create", &net.0]).status.success());
    let pg = format!("tinox-e2e-pg-{id}");
    let app = format!("tinox-e2e-pgapp-{id}");
    let (dir, image) = build_variant("pg", &format!("driver = \"postgres\"\nurl = \"postgres://postgres@{pg}/postgres\""));
    let _cleanup = Cleanup { containers: vec![app.clone(), pg.clone()], image: image.clone(), volume: String::new(), dir };
    assert!(docker(&["run", "-d", "--name", &pg, "--network", &net.0, "-e", "POSTGRES_HOST_AUTH_METHOD=trust", "postgres:16-alpine"]).status.success());
    wait_for("postgres", || docker(&["exec", &pg, "pg_isready", "-U", "postgres"]).status.success());
    let url = run_app_on_network(&app, &image, &net.0);
    let created = curl(&["-X", "POST", "-H", "Content-Type: application/json", "-d", r#"{"text":"from-postgres"}"#, &url]).unwrap();
    assert!(created.contains("from-postgres"), "{created}");
    // Independent client: the row is really in PostgreSQL, in the table the changelog created.
    let rows = docker(&["exec", &pg, "psql", "-U", "postgres", "-At", "-c", "SELECT text FROM notes"]);
    assert_eq!(String::from_utf8_lossy(&rows.stdout).trim(), "from-postgres", "{}", text(&rows));
}

#[test]
fn source_build_mysql_image_migrates_and_writes() {
    if !opt_in() {
        return;
    }
    ensure_builder();
    let id = std::process::id();
    let net = Net(format!("tinox-e2e-net-my-{id}"));
    assert!(docker(&["network", "create", &net.0]).status.success());
    let my = format!("tinox-e2e-my-{id}");
    let app = format!("tinox-e2e-myapp-{id}");
    let (dir, image) = build_variant("my", &format!("driver = \"mysql\"\nurl = \"mysql://root:tinox@{my}:3306/tinox\""));
    let _cleanup = Cleanup { containers: vec![app.clone(), my.clone()], image: image.clone(), volume: String::new(), dir };
    assert!(docker(&["run", "-d", "--name", &my, "--network", &net.0, "-e", "MYSQL_ROOT_PASSWORD=tinox", "-e", "MYSQL_DATABASE=tinox", "mysql:8.4"]).status.success());
    wait_for("mysql", || docker(&["exec", &my, "mysql", "-h127.0.0.1", "-uroot", "-ptinox", "-e", "SELECT 1"]).status.success());
    let url = run_app_on_network(&app, &image, &net.0);
    let created = curl(&["-X", "POST", "-H", "Content-Type: application/json", "-d", r#"{"text":"from-mysql"}"#, &url]).unwrap();
    assert!(created.contains("from-mysql"), "{created}");
    let rows = docker(&["exec", &my, "mysql", "-h127.0.0.1", "-uroot", "-ptinox", "-N", "-B", "-D", "tinox", "-e", "SELECT text FROM notes"]);
    assert_eq!(String::from_utf8_lossy(&rows.stdout).trim(), "from-mysql", "{}", text(&rows));
}

#[test]
fn source_build_tls_image_serves_https() {
    if !opt_in() {
        return;
    }
    ensure_builder();
    let id = std::process::id();
    let dir = std::env::temp_dir().join(format!("tinox-docker-e2e-tls-{id}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("tinox.toml"),
        "[package]\ncompiler = \"2.9.0\"\nname = \"tlsapp\"\nversion = \"0.1.0\"\n\n[docker]\nports = [8443]\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"http_server\"\nversion = \"1.2.2\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/Main.tnx"),
        "import tinox.core.http_server\n\nclass Main\n{\n    fnc main() -> Int32\n    {\n        const server: HttpServer = HttpServer::new(8443)\n        server.get(\"/hello\", ctx => {\n            ctx.response.body = \"secure hello\"\n        })\n        server.listenTls(\"/app/data/certs/cert.pem\", \"/app/data/certs/key.pem\")\n        return 0\n    }\n}\n",
    )
    .unwrap();
    let image = format!("tinox-docker-e2e-tls-{id}:test");
    let out = Command::new(env!("CARGO_BIN_EXE_tinox")).args(["docker", "--source-build", "--tag", &image]).current_dir(&dir).output().unwrap();
    assert!(out.status.success(), "{}", text(&out));
    let app = format!("tinox-docker-e2e-tlsapp-{id}");
    let certs = dir.join("certs");
    std::fs::create_dir_all(&certs).unwrap();
    for (from, to) in [("selfsigned_cert.pem", "cert.pem"), ("selfsigned_key.pem", "key.pem")] {
        std::fs::copy(repo_root().join("tests/fixtures/tls").join(from), certs.join(to)).unwrap();
    }
    let _cleanup = Cleanup { containers: vec![app.clone()], image: image.clone(), volume: String::new(), dir: dir.clone() };
    // Key material arrives as a read-only mount; the image itself contains none.
    let run = docker(&["run", "-d", "--init", "--name", &app, "-p", "127.0.0.1::8443", "-v", &format!("{}:/app/data/certs:ro", certs.display()), &image]);
    assert!(run.status.success(), "{}", text(&run));
    let port_out = docker(&["port", &app, "8443/tcp"]);
    let s = String::from_utf8_lossy(&port_out.stdout).to_string();
    let port = s.lines().next().and_then(|l| l.rsplit(':').next()).map(|p| p.trim().to_string()).unwrap();
    let url = format!("https://127.0.0.1:{port}/hello");
    wait_for("the TLS app", || curl(&["-k", &url]).is_some());
    assert_eq!(curl(&["-k", &url]).unwrap().trim(), "secure hello");
    // The handshake is real TLS: certificate verification against nothing trusted fails, plain HTTP is refused.
    assert!(curl(&[&url]).is_none(), "a self-signed certificate must not verify without -k");
    assert!(curl(&[&format!("http://127.0.0.1:{port}/hello")]).is_none(), "plain HTTP must not be served on the TLS port");
    let id_out = docker(&["exec", &app, "id", "-u"]);
    assert_eq!(String::from_utf8_lossy(&id_out.stdout).trim(), "10001");
    assert!(!docker(&["exec", &app, "sh", "-c", "ls /app/data/certs/*.pem >/dev/null 2>&1 && test -w /app/data/certs"]).status.success(), "the certificate mount is read-only");
}

#[test]
fn source_build_without_network_fails_visibly_when_dependencies_are_needed() {
    if !opt_in() {
        return;
    }
    ensure_builder();
    let (dir, image) = build_variant("offline", "driver = \"sqlite\"\nurl = \"data/offline.db\"");
    let _cleanup = Cleanup { containers: vec![], image: image.clone(), volume: String::new(), dir: dir.clone() };
    // The staged context does not carry the host's package cache, so the same Dockerfile cannot build offline:
    // `tinox install` inside the builder fails with a clear message instead of reusing anything from the host.
    let context = dir.join(".tinox/docker/ctx");
    let offline = Command::new("docker")
        .args(["build", "--network", "none", "--no-cache", "-f"])
        .arg(context.join("Dockerfile"))
        .arg(&context)
        .args(["-t", &format!("{image}-offline")])
        .output()
        .unwrap();
    let t = text(&offline);
    assert!(!offline.status.success(), "an offline build of a project with dependencies must fail:\n{t}");
    assert!(t.contains("Download failed") && t.contains("0 installed"), "{t}");
    assert!(!docker(&["image", "inspect", &format!("{image}-offline")]).status.success(), "no half-built image may exist");
}

#[test]
fn source_build_http3_is_an_explicit_unsupported_contract() {
    if !opt_in() {
        return;
    }
    ensure_builder();
    let id = std::process::id();
    let dir = std::env::temp_dir().join(format!("tinox-docker-e2e-h3-{id}"));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(Command::new("cp").arg("-r").arg(repo_root().join("tests/fixtures/docker_source_build")).arg(&dir).status().unwrap().success());
    let image = format!("tinox-docker-e2e-h3-{id}:test");
    let _cleanup = Cleanup { containers: vec![], image: image.clone(), volume: String::new(), dir: dir.clone() };
    let out = Command::new(env!("CARGO_BIN_EXE_tinox")).args(["docker", "--source-build", "--tag", &image]).env("TINOX_HTTP3", "1").current_dir(&dir).output().unwrap();
    let t = text(&out);
    // The stock builder has no ngtcp2/nghttp3: the build fails in the builder, naming the missing header, and the
    // tool says so up front. No image is produced.
    assert!(!out.status.success(), "TINOX_HTTP3=1 must not produce an image with the stock builder:\n{t}");
    assert!(t.contains("ngtcp2/ngtcp2.h"), "{t}");
    assert!(t.contains("stock builder image has no ngtcp2/nghttp3"), "{t}");
    assert!(!docker(&["image", "inspect", &image]).status.success());
}
