//! Issue #359: `TINOX_BIND_ADDRESS` restricts every listener to one address.
//! Drives a real compiled program and independent TCP clients.

use std::io::{BufRead, BufReader};
use std::net::{SocketAddr, TcpStream};
use std::process::{Command, Stdio};
use std::time::Duration;

const PROGRAM: &str = r#"class Main
{
    fnc main() -> Int32
    {
        const srv = httpServerCreate(0)
        println(httpServerBoundPort(srv).toString())
        sleep_ms(3000)
        return 0
    }
}
"#;

fn run(env: Option<&str>) -> (std::process::Child, u16) {
    let dir = std::env::temp_dir().join(format!("tinox-bind-{}-{}", std::process::id(), env.unwrap_or("none").replace('.', "_")));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Main.tnx"), PROGRAM).unwrap();
    let exe = dir.join("bind_prog");
    let b = Command::new(env!("CARGO_BIN_EXE_tinox"))
        .arg("build").arg(dir.join("Main.tnx")).arg("-o").arg(&exe).current_dir(&dir)
        .output().unwrap();
    assert!(b.status.success(), "build failed: {}", String::from_utf8_lossy(&b.stderr));
    let mut cmd = Command::new(&exe);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(v) = env {
        cmd.env("TINOX_BIND_ADDRESS", v);
    }
    let mut child = cmd.spawn().unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.as_mut().unwrap()).read_line(&mut line).unwrap();
    let port = line.trim().parse().unwrap_or_else(|_| panic!("no port printed: {line:?}"));
    (child, port)
}

fn reachable(ip: &str, port: u16) -> bool {
    let addr: SocketAddr = format!("{ip}:{port}").parse().unwrap();
    TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok()
}

#[test]
fn loopback_only_when_env_set() {
    let (mut child, port) = run(Some("127.0.0.1"));
    assert!(reachable("127.0.0.1", port));
    assert!(!reachable("127.0.0.2", port), "127.0.0.2 must not reach a 127.0.0.1-only listener");
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn default_binds_all_interfaces() {
    let (mut child, port) = run(None);
    assert!(reachable("127.0.0.1", port));
    assert!(reachable("127.0.0.2", port));
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn invalid_address_fails_hard() {
    let dir = std::env::temp_dir().join(format!("tinox-bind-bad-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Main.tnx"), PROGRAM).unwrap();
    let exe = dir.join("bind_prog");
    let b = Command::new(env!("CARGO_BIN_EXE_tinox"))
        .arg("build").arg(dir.join("Main.tnx")).arg("-o").arg(&exe).current_dir(&dir)
        .output().unwrap();
    assert!(b.status.success());
    let out = Command::new(&exe).env("TINOX_BIND_ADDRESS", "not-an-ip").output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid TINOX_BIND_ADDRESS"));
}
