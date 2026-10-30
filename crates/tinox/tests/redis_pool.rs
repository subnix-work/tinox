//! Issue #544: RedisClient reply kinds and RedisPool, verified against a real redis-server.
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
    fnc worker(pool: RedisPool, rounds: Int64) -> Int64 {
        var done: Int64 = 0
        var i: Int64 = 0
        while i < rounds {
            const c: RedisClient = pool.acquire(5000)
            if (c.conn > 0) {
                const r: RedisReply = c.command(["INCR", "counter"])
                if (r.isValue()) { done = done + 1 }
                pool.release(c)
            }
            i = i + 1
        }
        return done
    }

    fnc main() -> Int32 {
        const pool: RedisPool = RedisPool::create("127.0.0.1", __PORT__, 2)

        // Distinct reply kinds without a shared flag.
        const c: RedisClient = pool.acquire(1000)
        c.command(["DEL", "missing", "empty", "text"])
        c.command(["SET", "empty", ""])
        c.command(["SET", "text", "hello"])
        println("nil=" + c.getReply("missing").isNil().toString())
        const e: RedisReply = c.getReply("empty")
        println("empty=" + (e.isValue() && e.text == "").toString())
        println("text=" + c.getReply("text").text)
        println("error=" + c.command(["INCR", "text"]).isError().toString())
        pool.release(c)

        // Ordering and exclusivity under concurrency: 6 workers share 2 connections.
        c.command(["DEL", "counter"])
        const t1 = spawn Main::worker(pool, 50)
        const t2 = spawn Main::worker(pool, 50)
        const t3 = spawn Main::worker(pool, 50)
        const t4 = spawn Main::worker(pool, 50)
        const t5 = spawn Main::worker(pool, 50)
        const t6 = spawn Main::worker(pool, 50)
        const total: Int64 = await t1 + await t2 + await t3 + await t4 + await t5 + await t6
        println("increments=" + total.toString())
        const check: RedisClient = pool.acquire(1000)
        println("counter=" + check.getReply("counter").text)
        pool.release(check)

        // Saturation: both permits held, a third acquire times out without blocking forever.
        const a: RedisClient = pool.acquire(1000)
        const b: RedisClient = pool.acquire(1000)
        const third: RedisClient = pool.acquire(100)
        println("exhausted=" + (third.conn <= 0 && third.errorMessage == "pool exhausted").toString())
        pool.release(a)
        pool.release(b)

        // A failed request poisons only its own connection: it is discarded and the next
        // borrower gets a clean one.
        const bad: RedisClient = pool.acquire(1000)
        bad.command(["QUIT"])
        const dead: RedisReply = bad.command(["GET", "text"])
        println("failure=" + dead.isFailure().toString())
        println("unhealthy=" + (!bad.healthy).toString())
        pool.release(bad)
        const next: RedisClient = pool.acquire(1000)
        println("recovered=" + next.getReply("text").text)
        pool.release(next)

        pool.close()
        const after: RedisClient = pool.acquire(100)
        println("closed=" + (after.errorMessage == "pool closed").toString())
        return 0
    }
}
"#;

#[test]
fn pool_and_reply_kinds_behave_against_a_real_redis() {
    if Command::new("redis-server").arg("--version").stdout(Stdio::null()).status().is_err() {
        eprintln!("SKIP: redis-server not installed");
        return;
    }
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let server = Command::new("redis-server")
        .args(["--port", &port.to_string(), "--save", "", "--appendonly", "no", "--bind", "127.0.0.1"])
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

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let dir = std::env::temp_dir().join(format!("tinox-redis-pool-{}", std::process::id()));
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
            "[package]\ncompiler = \"2.8.0\"\nname = \"redispool\"\nversion = \"0.1.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"redis\"\nversion = \"{version}\"\n\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"redis\"\npath = \"{}\"\n",
            root.join("crates/tinox-core-ext/redis").display()
        ),
    )
    .unwrap();
    std::fs::write(dir.join("Main.tnx"), MAIN.replace("__PORT__", &port.to_string())).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_tinox")).args(["run", "Main.tnx"]).current_dir(&dir).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    for expected in [
        "nil=true", "empty=true", "text=hello", "error=true", "increments=300", "counter=300",
        "exhausted=true", "failure=true", "unhealthy=true", "recovered=hello", "closed=true",
    ] {
        assert!(stdout.lines().any(|l| l == expected), "missing `{expected}` in:\n{stdout}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
