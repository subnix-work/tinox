//! Issue #545: AUTH for RedisClient/RedisPool against a real password-protected redis-server.
#![cfg(unix)]

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

const MAIN: &str = r#"import tinox.core.redis

class Main
{
    fnc main() -> Int32 {
        // Without credentials the server refuses commands.
        const open: RedisPool = RedisPool::create("127.0.0.1", __PORT__, 2)
        const c0: RedisClient = open.acquire(1000)
        const denied: RedisReply = c0.command(["SET", "k", "v"])
        println("noauth=" + (denied.isError() && denied.text.startsWith("NOAUTH")).toString())
        open.release(c0)

        // Correct password: every pooled connection authenticates itself.
        const pool: RedisPool = RedisPool::create("127.0.0.1", __PORT__, 2).authenticate("", "s3cret")
        const c1: RedisClient = pool.acquire(1000)
        println("set=" + c1.command(["SET", "k", "v"]).isValue().toString())
        println("get=" + c1.getReply("k").text)
        pool.release(c1)

        // ACL user form.
        const acl: RedisPool = RedisPool::create("127.0.0.1", __PORT__, 1).authenticate("alice", "alicepw")
        const c2: RedisClient = acl.acquire(1000)
        println("acl=" + c2.getReply("k").text)
        acl.release(c2)

        // Wrong password: the acquire fails visibly and the permit is not leaked.
        const wrong: RedisPool = RedisPool::create("127.0.0.1", __PORT__, 1).authenticate("", "nope")
        const c3: RedisClient = wrong.acquire(1000)
        println("wrong=" + (c3.conn <= 0 && c3.errorMessage.startsWith("AUTH failed")).toString())
        const c4: RedisClient = wrong.acquire(1000)
        println("again=" + (c4.errorMessage.startsWith("AUTH failed")).toString())

        // Plain client.
        const direct: RedisClient = RedisClient::connect("127.0.0.1", __PORT__)
        println("direct=" + direct.auth("", "s3cret").toString() + " bad=" + direct.auth("", "x").toString())
        return 0
    }
}
"#;

#[test]
fn auth_works_against_a_password_protected_redis() {
    if Command::new("redis-server").arg("--version").stdout(Stdio::null()).status().is_err() {
        eprintln!("SKIP: redis-server not installed");
        return;
    }
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let server = Command::new("redis-server")
        .args(["--port", &port.to_string(), "--save", "", "--appendonly", "no", "--bind", "127.0.0.1", "--requirepass", "s3cret"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn redis-server");
    let _guard = Server(server);
    for _ in 0..50 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // An ACL user, created with an independent client (redis-cli).
    let acl = Command::new("redis-cli")
        .args(["-p", &port.to_string(), "-a", "s3cret", "ACL", "SETUSER", "alice", "on", ">alicepw", "~*", "+@all"])
        .output()
        .expect("redis-cli");
    assert!(String::from_utf8_lossy(&acl.stdout).contains("OK"), "{acl:?}");

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let dir = std::env::temp_dir().join(format!("tinox-redis-auth-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let version = std::fs::read_to_string(root.join("crates/tinox-core-ext/redis/tinox.toml"))
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("version = ").map(|v| v.trim_matches('"').to_string()))
        .unwrap();
    std::fs::write(
        dir.join("tinox.toml"),
        format!(
            "[package]\ncompiler = \"2.8.0\"\nname = \"redisauth\"\nversion = \"0.1.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"redis\"\nversion = \"{version}\"\n\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"redis\"\npath = \"{}\"\n",
            root.join("crates/tinox-core-ext/redis").display()
        ),
    )
    .unwrap();
    std::fs::write(dir.join("Main.tnx"), MAIN.replace("__PORT__", &port.to_string())).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_tinox")).args(["run", "Main.tnx"]).current_dir(&dir).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    for expected in ["noauth=true", "set=true", "get=v", "acl=v", "wrong=true", "again=true", "direct=true bad=false"] {
        assert!(stdout.lines().any(|l| l == expected), "missing `{expected}` in:\n{stdout}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

const MAIN_TLS: &str = r#"import tinox.core.redis

class Main
{
    fnc main() -> Int32 {
        // Self-signed certificate: verification must refuse it, the explicit opt-out must work.
        const strict: RedisClient = RedisClient::connectTls("localhost", __PORT__, true)
        println("verified=" + (strict.conn > 0).toString() + " err=" + strict.errorMessage)
        const c: RedisClient = RedisClient::connectTls("localhost", __PORT__, false)
        println("insecure=" + (c.conn > 0).toString())
        c.auth("", "s3cret")
        println("set=" + c.command(["SET", "tlskey", "tlsvalue"]).isValue().toString())
        println("get=" + c.getReply("tlskey").text)
        const pool: RedisPool = RedisPool::create("localhost", __PORT__, 2).useTls(false).authenticate("", "s3cret")
        const p: RedisClient = pool.acquire(1000)
        println("pool=" + p.getReply("tlskey").text)
        pool.release(p)
        return 0
    }
}
"#;

#[test]
fn tls_connections_verify_certificates_and_work_when_opted_out() {
    if Command::new("redis-server").arg("--version").stdout(Stdio::null()).status().is_err() {
        eprintln!("SKIP: redis-server not installed");
        return;
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let server = Command::new("redis-server")
        .args(["--port", "0", "--tls-port", &port.to_string(), "--save", "", "--appendonly", "no", "--bind", "127.0.0.1", "--requirepass", "s3cret", "--tls-auth-clients", "no"])
        .arg("--tls-cert-file")
        .arg(root.join("tests/fixtures/tls/selfsigned_cert.pem"))
        .arg("--tls-key-file")
        .arg(root.join("tests/fixtures/tls/selfsigned_key.pem"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn redis-server");
    let _guard = Server(server);
    for _ in 0..50 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let dir = std::env::temp_dir().join(format!("tinox-redis-tls-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let version = std::fs::read_to_string(root.join("crates/tinox-core-ext/redis/tinox.toml"))
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("version = ").map(|v| v.trim_matches('"').to_string()))
        .unwrap();
    std::fs::write(
        dir.join("tinox.toml"),
        format!(
            "[package]\ncompiler = \"2.8.0\"\nname = \"redistls\"\nversion = \"0.1.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"redis\"\nversion = \"{version}\"\n\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"redis\"\npath = \"{}\"\n",
            root.join("crates/tinox-core-ext/redis").display()
        ),
    )
    .unwrap();
    std::fs::write(dir.join("Main.tnx"), MAIN_TLS.replace("__PORT__", &port.to_string())).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_tinox")).args(["run", "Main.tnx"]).current_dir(&dir).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    for expected in ["verified=false err=TLS handshake failed", "insecure=true", "set=true", "get=tlsvalue", "pool=tlsvalue"] {
        assert!(stdout.lines().any(|l| l == expected), "missing `{expected}` in:\n{stdout}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
