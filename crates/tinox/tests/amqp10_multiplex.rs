//! Issue #539: one connection-level dispatcher routes AMQP 1.0 frames by session/link into bounded
//! inboxes. Against a REAL RabbitMQ 4 (independent observer: rabbitmqctl queue depths): three
//! receivers on three links (two of them sharing one session) block at the same time while a
//! publisher interleaves messages; nothing is misattributed. A broker-side connection kill wakes
//! every waiter with an error.

use std::path::PathBuf;
use std::process::Command;

mod rabbit_common;
use rabbit_common::Broker;

const MAIN: &str = r#"import tinox.core.amqp10
import tinox.core.encoding

class Main
{
    // Receives `count` messages from one link; every body must start with the link's own prefix.
    async fnc consumer(link: Amqp10Link, prefix: String, count: Int64) -> Int64 {
        link.grantCredit(count)
        var good: Int64 = 0
        var i: Int64 = 0
        while i < count {
            const m: Amqp10Message = link.nextMessage()
            if (!m.ok) { return -1 }
            var text: String = ""
            for j in 0..m.body.len() {
                text = text + fromCharCode(m.body[j])
            }
            if (text.startsWith(prefix)) { good = good + 1 }
            link.ack(m.deliveryId)
            i = i + 1
        }
        return good
    }

    async fnc waiter(link: Amqp10Link) -> Int64 {
        link.grantCredit(5)
        const m: Amqp10Message = link.nextMessage()
        if (m.ok) { return 0 }
        return 1
    }

    fnc main() -> Int32 {
        const conn: Amqp10Connection = Amqp10Connection::connect("127.0.0.1", __PORT__, "tinox", "tinox")
        println("connected=" + (conn.conn > 0).toString())
        const s1: Amqp10Session = Amqp10Session::begin(conn)
        const s2: Amqp10Session = Amqp10Session::begin(conn)
        const s3: Amqp10Session = Amqp10Session::begin(conn)
        println("sessions=" + (s1.errorMessage == "" && s2.errorMessage == "" && s3.errorMessage == "").toString())
        const r1: Amqp10Link = Amqp10Link::attach(s1, "r1", true, "/queues/c1")
        const r2: Amqp10Link = Amqp10Link::attach(s2, "r2", true, "/queues/c2")
        const r3: Amqp10Link = Amqp10Link::attach(s2, "r3", true, "/queues/c3")
        println("links=" + (r1.errorMessage == "" && r2.errorMessage == "" && r3.errorMessage == "").toString())
        const t1 = spawn Main::consumer(r1, "c1-", __COUNT__)
        const t2 = spawn Main::consumer(r2, "c2-", __COUNT__)
        const t3 = spawn Main::consumer(r3, "c3-", __COUNT__)

        // The publisher interleaves all three queues over its own session while the consumers wait.
        const p1: Amqp10Link = Amqp10Link::attach(s3, "p1", false, "/queues/c1")
        const p2: Amqp10Link = Amqp10Link::attach(s3, "p2", false, "/queues/c2")
        const p3: Amqp10Link = Amqp10Link::attach(s3, "p3", false, "/queues/c3")
        var i: Int64 = 0
        while i < __COUNT__ {
            p1.publish(Encoding::toUtf8("c1-" + i.toString()), "text/plain")
            p2.publish(Encoding::toUtf8("c2-" + i.toString()), "text/plain")
            p3.publish(Encoding::toUtf8("c3-" + i.toString()), "text/plain")
            i = i + 1
        }
        println("publish_errors=" + (p1.errorMessage + p2.errorMessage + p3.errorMessage).len().toString())
        println("c1=" + (await t1).toString())
        println("c2=" + (await t2).toString())
        println("c3=" + (await t3).toString())
        p1.detach()
        p2.detach()
        p3.detach()
        r1.detach()
        r2.detach()
        r3.detach()
        s1.end()
        s2.end()
        s3.end()
        conn.close()

        // Broker-side kill: every waiter on every session/link is woken with an error.
        const k: Amqp10Connection = Amqp10Connection::connect("127.0.0.1", __PORT__, "tinox", "tinox")
        const ks1: Amqp10Session = Amqp10Session::begin(k)
        const ks2: Amqp10Session = Amqp10Session::begin(k)
        const w1: Amqp10Link = Amqp10Link::attach(ks1, "w1", true, "/queues/quiet1")
        const w2: Amqp10Link = Amqp10Link::attach(ks2, "w2", true, "/queues/quiet2")
        const wt1 = spawn Main::waiter(w1)
        const wt2 = spawn Main::waiter(w2)
        println("kill_ready=" + (w1.errorMessage == "" && w2.errorMessage == "").toString())
        println("kill_woken_1=" + (await wt1).toString())
        println("kill_woken_2=" + (await wt2).toString())
        return 0
    }
}
"#;

#[test]
fn sessions_and_links_wait_concurrently_on_one_connection() {
    if Command::new("docker").arg("--version").output().is_err() {
        eprintln!("SKIP: docker not installed");
        return;
    }
    let broker = Broker::start();
    for q in ["c1", "c2", "c3", "quiet1", "quiet2"] {
        broker.declare(q, "{}");
    }
    let count = 100;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let version = std::fs::read_to_string(root.join("crates/tinox-core-ext/amqp10/tinox.toml"))
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("version = ").map(|v| v.trim_matches('"').to_string()))
        .unwrap();
    let dir = std::env::temp_dir().join(format!("tinox-amqp10-mux-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("tinox.toml"),
        format!(
            "[package]\ncompiler = \"2.8.0\"\nname = \"mux\"\nversion = \"0.0.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"amqp10\"\nversion = \"{version}\"\n\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"amqp10\"\npath = \"{}\"\n",
            root.join("crates/tinox-core-ext/amqp10").display()
        ),
    )
    .unwrap();
    std::fs::write(dir.join("Main.tnx"), MAIN.replace("__PORT__", &broker.amqp.to_string()).replace("__COUNT__", &count.to_string())).unwrap();
    // Once the waiters are attached (the program reports nothing until they are woken), kill the
    // second connection from the broker side as soon as a consumer is registered on quiet2.
    let out = std::thread::scope(|scope| {
        let killer = scope.spawn(|| {
            for _ in 0..900 {
                let q = broker.api("GET", "queues/%2F/quiet2", None).unwrap_or_default();
                if q.contains("\"consumers\":1") || q.contains("\"consumers\": 1") {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    for _ in 0..100 {
                        let list = broker.api("GET", "connections", None).unwrap_or_default();
                        let mut killed = false;
                        for name in list.split("\"name\":\"").skip(1) {
                            let name = name.split('"').next().unwrap().replace(' ', "%20").replace('>', "%3E");
                            killed |= broker.api("DELETE", &format!("connections/{name}"), None).is_some();
                        }
                        if killed {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(300));
                    }
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        });
        let out = Command::new(env!("CARGO_BIN_EXE_tinox")).args(["run", "Main.tnx"]).current_dir(&dir).output().unwrap();
        killer.join().unwrap();
        out
    });
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    for expected in [
        "connected=true",
        "sessions=true",
        "links=true",
        "publish_errors=0",
        &format!("c1={count}"),
        &format!("c2={count}"),
        &format!("c3={count}"),
        "kill_ready=true",
        "kill_woken_1=1",
        "kill_woken_2=1",
    ] {
        assert!(stdout.lines().any(|l| l == expected), "missing `{expected}` in:\n{stdout}");
    }
    broker.wait("queues drained", || broker.depth("c1") == (0, 0) && broker.depth("c2") == (0, 0) && broker.depth("c3") == (0, 0));
    let _ = std::fs::remove_dir_all(&dir);
}
