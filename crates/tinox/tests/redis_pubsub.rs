//! Issue #546: RedisSubscriber (separate Pub/Sub connection) against a real redis-server.
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
    fnc listen(sub: RedisSubscriber, expected: Int64) -> String {
        var seen: String = ""
        var got: Int64 = 0
        while got < expected {
            const m: RedisMessage = sub.next(5000)
            if (m.isMessage()) {
                seen = seen + m.topic + ":" + m.payload + ","
                got = got + 1
            } else if (m.isClosed()) {
                return seen + "CLOSED"
            }
        }
        return seen
    }

    fnc blocked(sub: RedisSubscriber) -> String {
        const m: RedisMessage = sub.next(30000)
        return m.isClosed().toString()
    }

    fnc main() -> Int32 {
        const publisher: RedisClient = RedisClient::connect("127.0.0.1", __PORT__)
        const sub: RedisSubscriber = RedisSubscriber::connect("127.0.0.1", __PORT__)
        println("subscribed=" + sub.subscribe(["a", "b"]).toString())

        // Timeout consumes nothing and keeps the subscription alive.
        println("timeout=" + sub.next(100).isTimeout().toString())

        // Ordered delivery of published messages, including an empty payload.
        const t = spawn Main::listen(sub, 4)
        sleep_ms(100)
        publisher.command(["PUBLISH", "a", "one"])
        publisher.command(["PUBLISH", "b", "two"])
        publisher.command(["PUBLISH", "a", ""])
        publisher.command(["PUBLISH", "b", "four"])
        println("seen=" + await t)

        // After unsubscribing from `a`, only `b` is delivered.
        println("unsub=" + sub.unsubscribe(["a"]).toString())
        publisher.command(["PUBLISH", "a", "ignored"])
        publisher.command(["PUBLISH", "b", "kept"])
        const m: RedisMessage = sub.next(2000)
        println("after=" + m.topic + ":" + m.payload)

        // The command connection is unaffected by subscriber traffic.
        println("ping=" + publisher.ping().toString())

        // Server-side disconnect: next() reports closed, reconnect() restores the subscription.
        publisher.command(["CLIENT", "KILL", "TYPE", "pubsub"])
        println("killed=" + sub.next(2000).isClosed().toString())
        println("reconnect=" + sub.reconnect().toString())
        publisher.command(["PUBLISH", "b", "again"])
        const r: RedisMessage = sub.next(2000)
        println("resumed=" + r.topic + ":" + r.payload)

        // close() from another thread wakes a blocked next().
        const waiter = spawn Main::blocked(sub)
        sleep_ms(200)
        sub.close()
        println("woken=" + await waiter)
        publisher.close()
        return 0
    }
}
"#;

#[test]
fn subscriber_orders_unsubscribes_reconnects_and_wakes_on_close() {
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
    let dir = std::env::temp_dir().join(format!("tinox-redis-pubsub-{}", std::process::id()));
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
            "[package]\ncompiler = \"2.8.0\"\nname = \"redispubsub\"\nversion = \"0.1.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"redis\"\nversion = \"{version}\"\n\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"redis\"\npath = \"{}\"\n",
            root.join("crates/tinox-core-ext/redis").display()
        ),
    )
    .unwrap();
    std::fs::write(dir.join("Main.tnx"), MAIN.replace("__PORT__", &port.to_string())).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_tinox")).args(["run", "Main.tnx"]).current_dir(&dir).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    for expected in [
        "subscribed=true", "timeout=true", "seen=a:one,b:two,a:,b:four,", "unsub=true", "after=b:kept",
        "ping=true", "killed=true", "reconnect=true", "resumed=b:again", "woken=true",
    ] {
        assert!(stdout.lines().any(|l| l == expected), "missing `{expected}` in:\n{stdout}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
