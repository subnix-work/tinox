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
