//! Regression coverage for #157: `tinox install`/`add` used to resolve
//! only the dependencies declared directly in the project's own
//! `tinox.toml` — a dependency that itself shipped a `tinox.toml`
//! declaring further dependencies had those silently ignored.
//!
//! `install_follows_a_dependencys_own_tinox_toml_transitively` exercises
//! the actual built `tinox` binary end-to-end against a real (local,
//! in-process) HTTP server serving real tar.gz artifacts — not just the
//! resolver logic in isolation — per this project's own "verify against
//! real, independent systems" testing convention (CLAUDE.md): a package
//! manager is a network-facing feature, and a version of this test that
//! only pre-populated `.tinox/deps/` by hand would never exercise the
//! actual download → extract → discover a nested `tinox.toml` →
//! download-again path.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

fn tinox() -> &'static str {
    env!("CARGO_BIN_EXE_tinox")
}

fn build_tar_gz(dir: &Path, files: &[(&str, &str)]) -> Vec<u8> {
    use flate2::{write::GzEncoder, Compression};
    use tar::Builder;
    for (name, content) in files {
        std::fs::write(dir.join(name), content).expect("write fixture file");
    }
    let gz = GzEncoder::new(Vec::new(), Compression::default());
    let mut builder = Builder::new(gz);
    for (name, _) in files {
        builder.append_path_with_name(dir.join(name), name).expect("add to tar");
    }
    builder.into_inner().expect("finish tar").finish().expect("finish gz")
}

fn handle_request(stream: &mut TcpStream, routes: &[(String, Vec<u8>)]) {
    let mut buf = [0u8; 4096];
    let n = match stream.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return,
    };
    let request = String::from_utf8_lossy(&buf[..n]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_string();

    if let Some((_, body)) = routes.iter().find(|(p, _)| *p == path) {
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(header.as_bytes());
        let _ = stream.write_all(body);
    } else {
        let header = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let _ = stream.write_all(header.as_bytes());
    }
}

/// A minimal single-threaded-per-connection HTTP/1.1 server serving a
/// fixed set of `(path, bytes)` responses — just enough for
/// `ureq::get(url)` (what `pm::install_dep` uses) to download a tarball
/// by exact path. Returns the bound port immediately; routes can be
/// populated (or replaced) after the fact via the returned handle, since
/// building a fixture's tarball may itself need to know the port first
/// (to embed a `http://127.0.0.1:<port>/...` URL in its own tinox.toml).
type Routes = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

fn serve() -> (u16, Routes) {
    let routes: Routes = Arc::new(Mutex::new(Vec::new()));
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("local_addr").port();
    let routes_for_thread = routes.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let routes = routes_for_thread.lock().unwrap().clone();
            std::thread::spawn(move || handle_request(&mut stream, &routes));
        }
    });
    (port, routes)
}

#[test]
fn install_follows_a_dependencys_own_tinox_toml_transitively() {
    let workdir = std::env::temp_dir().join(format!("tinox-transitive-deps-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(workdir.join("fixtures/e")).expect("mkdir fixtures/e");
    std::fs::create_dir_all(workdir.join("fixtures/f")).expect("mkdir fixtures/f");
    std::fs::create_dir_all(workdir.join("project/src")).expect("mkdir project/src");

    let (port, routes) = serve();

    // F: a leaf dependency, no tinox.toml of its own.
    let f_tar = build_tar_gz(&workdir.join("fixtures/f"), &[("LibF.tnx", "class LibF {}\n")]);

    // E: depends on F via its OWN tinox.toml, embedding the now-known
    // server port — this is the tarball's tinox.toml, not the outer
    // project's, and is what `install_dep_transitively` should discover
    // and follow after extracting E.
    let e_dir = workdir.join("fixtures/e");
    let e_tar = build_tar_gz(
        &e_dir,
        &[
            ("LibE.tnx", "class LibE {}\n"),
            (
                "tinox.toml",
                &format!("[[dependencies]]\ngroup = \"com.example\"\nartifactId = \"F\"\nversion = \"1.0.0\"\nurl = \"http://127.0.0.1:{port}/F.tar.gz\"\n"),
            ),
        ],
    );

    *routes.lock().unwrap() = vec![("/E.tar.gz".to_string(), e_tar), ("/F.tar.gz".to_string(), f_tar)];

    let project = workdir.join("project");
    std::fs::write(
        project.join("tinox.toml"),
        format!("[package]\ncompiler = \"2.6.0\"\nname = \"freshtest\"\nversion = \"0.1.0\"\ndescription = \"\"\nentry = \"src/Main.tnx\"\n\n[[dependencies]]\ngroup = \"com.example\"\nartifactId = \"E\"\nversion = \"1.0.0\"\nurl = \"http://127.0.0.1:{port}/E.tar.gz\"\n"),
    )
    .expect("write project tinox.toml");

    let output = Command::new(tinox()).arg("install").current_dir(&project).output().expect("run tinox install");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "tinox install failed:\nstdout: {stdout}\nstderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(stdout.contains("2 installed, 0 failed"), "expected E AND F to be installed transitively: {stdout}");
    assert!(project.join(".tinox/deps/com.example/E/1.0.0/LibE.tnx").is_file(), "E not installed");
    assert!(project.join(".tinox/deps/com.example/F/1.0.0/LibF.tnx").is_file(), "F not installed transitively");

    let lock = std::fs::read_to_string(project.join("tinox.lock")).expect("read tinox.lock");
    assert!(lock.contains("artifactId: E"), "{lock}");
    assert!(lock.contains("artifactId: F"), "{lock}");

    std::fs::remove_dir_all(&workdir).ok();
}

#[test]
fn a_dependency_cycle_terminates_instead_of_hanging() {
    let workdir = std::env::temp_dir().join(format!("tinox-transitive-cycle-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(workdir.join("src")).expect("mkdir src");
    std::fs::create_dir_all(workdir.join(".tinox/deps/com.example/A/1.0.0")).expect("mkdir A");
    std::fs::create_dir_all(workdir.join(".tinox/deps/com.example/B/1.0.0")).expect("mkdir B");

    std::fs::write(
        workdir.join("tinox.toml"),
        "[package]\ncompiler = \"2.6.0\"\nname = \"cycletest\"\nversion = \"0.1.0\"\ndescription = \"\"\nentry = \"src/Main.tnx\"\n\n[[dependencies]]\ngroup = \"com.example\"\nartifactId = \"A\"\nversion = \"1.0.0\"\nurl = \"https://example.invalid/A.tar.gz\"\n",
    )
    .expect("write tinox.toml");
    std::fs::write(
        workdir.join(".tinox/deps/com.example/A/1.0.0/tinox.toml"),
        "[[dependencies]]\ngroup = \"com.example\"\nartifactId = \"B\"\nversion = \"1.0.0\"\nurl = \"https://example.invalid/B.tar.gz\"\n",
    )
    .expect("write A's tinox.toml");
    std::fs::write(
        workdir.join(".tinox/deps/com.example/B/1.0.0/tinox.toml"),
        "[[dependencies]]\ngroup = \"com.example\"\nartifactId = \"A\"\nversion = \"1.0.0\"\nurl = \"https://example.invalid/A.tar.gz\"\n",
    )
    .expect("write B's tinox.toml (cycle back to A)");
    std::fs::write(workdir.join(".tinox/deps/com.example/A/1.0.0/LibA.tnx"), "class LibA {}\n").unwrap();
    std::fs::write(workdir.join(".tinox/deps/com.example/B/1.0.0/LibB.tnx"), "class LibB {}\n").unwrap();

    // Both A and B are already "installed" on disk, so this run never
    // needs the network — install_dep short-circuits on an existing
    // install dir, but the transitive WALK still has to traverse the
    // cycle without infinite-looping.
    let output = Command::new(tinox())
        .arg("install")
        .current_dir(&workdir)
        .output()
        .expect("run tinox install");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "tinox install failed:\nstdout: {stdout}\nstderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(stdout.contains("2 installed, 0 failed"), "expected A and B each counted exactly once: {stdout}");

    std::fs::remove_dir_all(&workdir).ok();
}
