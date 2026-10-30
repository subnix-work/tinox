//! Issue #545: Sentinel master discovery and failover against real redis/valkey processes
//! (one master, one replica, one sentinel).
#![cfg(unix)]

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct Proc(Child);
impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn wait_port(port: u16) {
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("port {port} never opened");
}

fn server(port: u16, extra: &[&str]) -> Proc {
    Proc(
        Command::new("redis-server")
            .args(["--port", &port.to_string(), "--save", "", "--appendonly", "no", "--bind", "127.0.0.1"])
            .args(extra)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn redis-server"),
    )
}

const MAIN: &str = r#"import tinox.core.redis

class Main
{
    fnc main() -> Int32 {
        // A dead sentinel first: discovery falls through to the next one.
        const sentinels: List<String> = ["127.0.0.1:__DEAD__", "127.0.0.1:__SENTINEL__"]
        const first: String = RedisSentinel::resolveMaster(sentinels, "mymaster")
        println("first=" + first)
        const c: RedisClient = RedisSentinel::connectMaster(sentinels, "mymaster")
        println("connected=" + (c.conn > 0).toString())
        println("set=" + c.command(["SET", "k", "v1"]).isValue().toString())

        // Trigger a failover through the sentinel and wait for the promoted replica.
        const admin: RedisClient = RedisClient::connect("127.0.0.1", __SENTINEL__)
        println("failover=" + admin.command(["SENTINEL", "failover", "mymaster"]).isValue().toString())
        var moved: Bool = false
        var tries: Int64 = 0
        while !moved && tries < 150 {
            sleep_ms(200)
            moved = RedisSentinel::resolveMaster(sentinels, "mymaster") == "127.0.0.1:__REPLICA__"
            tries = tries + 1
        }
        println("moved=" + moved.toString())

        // The old connection is not silently reused for retries; the caller reconnects.
        const c2: RedisClient = RedisSentinel::connectMaster(sentinels, "mymaster")
        println("reconnected=" + (c2.conn > 0).toString())
        println("write=" + c2.command(["SET", "k", "v2"]).isValue().toString())
        println("role=" + c2.command(["INFO", "replication"]).text.contains("role:master").toString())
        println("none=" + (RedisSentinel::resolveMaster(["127.0.0.1:__DEAD__"], "mymaster") == "").toString())
        return 0
    }
}
"#;

#[test]
fn sentinel_discovery_follows_a_real_failover() {
    if Command::new("redis-server").arg("--version").stdout(Stdio::null()).status().is_err()
        || Command::new("redis-sentinel").arg("--version").stdout(Stdio::null()).status().is_err()
    {
        eprintln!("SKIP: redis-server/redis-sentinel not installed");
        return;
    }
    let (master, replica, sentinel, dead) = (free_port(), free_port(), free_port(), free_port());
    let _m = server(master, &[]);
    wait_port(master);
    let _r = server(replica, &["--replicaof", "127.0.0.1", &master.to_string()]);
    wait_port(replica);

    let dir = std::env::temp_dir().join(format!("tinox-redis-sentinel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let conf = dir.join("sentinel.conf");
    std::fs::write(
        &conf,
        format!(
            "port {sentinel}\nbind 127.0.0.1\ndir {}\nsentinel monitor mymaster 127.0.0.1 {master} 1\nsentinel down-after-milliseconds mymaster 1000\nsentinel failover-timeout mymaster 5000\nsentinel parallel-syncs mymaster 1\n",
            dir.display()
        ),
    )
    .unwrap();
    let _s = Proc(
        Command::new("redis-sentinel").arg(&conf).stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("spawn sentinel"),
    );
    wait_port(sentinel);
    // Let the sentinel learn the replica before asking it to fail over.
    std::thread::sleep(Duration::from_secs(3));

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let version = std::fs::read_to_string(root.join("crates/tinox-core-ext/redis/tinox.toml"))
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("version = ").map(|v| v.trim_matches('"').to_string()))
        .unwrap();
    std::fs::write(
        dir.join("tinox.toml"),
        format!(
            "[package]\ncompiler = \"2.8.0\"\nname = \"redissentinel\"\nversion = \"0.1.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"redis\"\nversion = \"{version}\"\n\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"redis\"\npath = \"{}\"\n",
            root.join("crates/tinox-core-ext/redis").display()
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("Main.tnx"),
        MAIN.replace("__SENTINEL__", &sentinel.to_string())
            .replace("__REPLICA__", &replica.to_string())
            .replace("__DEAD__", &dead.to_string()),
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_tinox")).args(["run", "Main.tnx"]).current_dir(&dir).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    for expected in [
        format!("first=127.0.0.1:{master}"),
        "connected=true".into(),
        "set=true".into(),
        "failover=true".into(),
        "moved=true".into(),
        "reconnected=true".into(),
        "write=true".into(),
        "role=true".into(),
        "none=true".into(),
    ] {
        assert!(stdout.lines().any(|l| l == expected), "missing `{expected}` in:\n{stdout}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
