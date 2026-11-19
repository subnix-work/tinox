//! Issue #670: `tinox install` against an already populated cache must still pin the archive
//! checksum in the project's `tinox.lock`. Real local HTTP server, real tar.gz artifact.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

fn tar_gz(dir: &Path, name: &str, content: &str) -> Vec<u8> {
    use flate2::{write::GzEncoder, Compression};
    std::fs::write(dir.join(name), content).unwrap();
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
    builder.append_path_with_name(dir.join(name), name).unwrap();
    builder.into_inner().unwrap().finish().unwrap()
}

fn serve(body: Arc<Mutex<Vec<u8>>>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let body = body.lock().unwrap().clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            });
        }
    });
    port
}

fn install(dir: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tinox")).arg("install").current_dir(dir).output().unwrap()
}

#[test]
fn warm_cache_install_still_pins_the_archive_checksum() {
    let dir = std::env::temp_dir().join(format!("tinox-warm-lock-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("fixture")).unwrap();
    let archive = tar_gz(&dir.join("fixture"), "Lib.tnx", "class Lib {}\n");
    let body = Arc::new(Mutex::new(archive));
    let port = serve(body.clone());
    std::fs::write(
        dir.join("tinox.toml"),
        format!(
            "[package]\ncompiler = \"{}\"\nname = \"warm\"\nversion = \"0.1.0\"\n\n[[dependencies]]\ngroup = \"com.example\"\nartifactId = \"Lib\"\nversion = \"1.0.0\"\nurl = \"http://127.0.0.1:{port}/Lib.tar.gz\"\n",
            env!("CARGO_PKG_VERSION")
        ),
    )
    .unwrap();

    // Cold install creates the lock with the archive hash.
    let cold = install(&dir);
    assert!(cold.status.success(), "{}", String::from_utf8_lossy(&cold.stderr));
    let lock = std::fs::read_to_string(dir.join("tinox.lock")).expect("cold install must write tinox.lock");
    let pinned = lock.lines().find(|l| l.contains("sha256")).expect("sha256 in lock").to_string();

    // Warm cache, no lock (a second project sharing the cache): the pin must come back, unchanged.
    std::fs::remove_file(dir.join("tinox.lock")).unwrap();
    let warm = install(&dir);
    assert!(warm.status.success(), "{}", String::from_utf8_lossy(&warm.stderr));
    assert!(String::from_utf8_lossy(&warm.stdout).contains("already installed"));
    let relocked = std::fs::read_to_string(dir.join("tinox.lock")).expect("warm install must write tinox.lock");
    assert_eq!(relocked.lines().find(|l| l.contains("sha256")).unwrap(), pinned);

    // A cache entry without a recorded hash (created before this fix): fetch and hash the archive.
    std::fs::remove_file(dir.join("tinox.lock")).unwrap();
    let sidecar = dir.join(".tinox/deps/com.example/Lib/1.0.0/.archive.sha256");
    assert!(sidecar.exists(), "cold install must record the archive hash");
    std::fs::remove_file(sidecar).unwrap();
    let legacy = install(&dir);
    assert!(legacy.status.success(), "{}", String::from_utf8_lossy(&legacy.stderr));
    let again = std::fs::read_to_string(dir.join("tinox.lock")).expect("legacy cache install must write tinox.lock");
    assert_eq!(again.lines().find(|l| l.contains("sha256")).unwrap(), pinned);
    let _ = std::fs::remove_dir_all(&dir);
}
