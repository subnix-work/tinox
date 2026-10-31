//! Issue #541: deliveries, publisher confirms and command replies of ONE AMQP 0-9-1 channel are routed
//! to separate inboxes. Against a REAL RabbitMQ (independent observer: rabbitmqctl queue depths):
//! a consumer and a confirm waiter run concurrently on the same channel without misclassifying frames.

use std::path::PathBuf;
use std::process::Command;

mod rabbit_common;
use rabbit_common::Broker;

const MAIN: &str = r#"import tinox.core.amqp091
import tinox.core.encoding

class Main
{
    // Publishes on the SAME channel the main task consumes from and collects every confirm.
    async fnc publisher(ch: AmqpChannel091, count: Int64) -> Int64 {
        var i: Int64 = 0
        var lastTag: Int64 = 0
        while i < count {
            lastTag = ch.publish("", "demux", Encoding::toUtf8("m" + i.toString()), "text/plain")
            i = i + 1
        }
        var confirmed: Int64 = 0
        var nacked: Int64 = 0
        var highest: Int64 = 0
        while highest < lastTag {
            const c: AmqpConfirmResult091 = ch.waitForConfirm()
            if (!c.ok) { return -1 }
            if (!c.ack) { nacked = nacked + 1 }
            confirmed = confirmed + (c.deliveryTag - highest)
            highest = c.deliveryTag
        }
        if (nacked > 0) { return -2 }
        return confirmed
    }

    async fnc deliveryWaiter(ch: AmqpChannel091) -> Int64 {
        const m: AmqpMessage091 = ch.nextMessage()
        if (m.ok) { return 0 }
        return 1
    }

    async fnc confirmWaiter(ch: AmqpChannel091) -> Int64 {
        const c: AmqpConfirmResult091 = ch.waitForConfirm()
        if (c.ok) { return 0 }
        return 1
    }

    fnc main() -> Int32 {
        const conn: AmqpConnection091 = AmqpConnection091::connect("127.0.0.1", __PORT__, "/", "tinox", "tinox")
        println("connected=" + (conn.conn > 0).toString())
        const ch: AmqpChannel091 = AmqpChannel091::open(conn)
        println("declared=" + (ch.declareQueue("demux", true, false, false) == "demux").toString())
        println("confirms=" + ch.confirmSelect().toString())
        println("consuming=" + (ch.consume("demux") != "").toString())
        const task = spawn Main::publisher(ch, __COUNT__)
        var tags: List<Int64> = []
        var ordered: Bool = true
        var i: Int64 = 0
        while i < __COUNT__ {
            const m: AmqpMessage091 = ch.nextMessage()
            if (!m.ok) {
                println("delivery_error=" + m.errorMessage)
                return 1
            }
            if (m.body.len() == 0 || m.body.len() > 8) { ordered = false }
            tags.push(m.deliveryTag)
            i = i + 1
        }
        const confirmed = await task
        println("received=" + tags.len().toString())
        println("confirmed=" + confirmed.toString())
        // Acks go out only after the publisher is done: interleaving them with a publish on one
        // channel would break the method/header/body frame sequence on the wire.
        for k in 0..tags.len() {
            ch.ack(tags[k])
        }
        // A broker-initiated channel.close wakes the consumer AND the confirm waiter of that channel.
        const ch2: AmqpChannel091 = AmqpChannel091::open(conn)
        println("declared2=" + (ch2.declareQueue("quiet", true, false, false) == "quiet").toString())
        println("confirms2=" + ch2.confirmSelect().toString())
        const w1 = spawn Main::deliveryWaiter(ch2)
        const w2 = spawn Main::confirmWaiter(ch2)
        sleep_ms(300)
        // Redeclaring with a different auto-delete flag is a channel-level PRECONDITION_FAILED.
        println("mismatch_rejected=" + (ch2.declareQueue("quiet", true, false, true) == "").toString())
        println("delivery_waiter_woken=" + (await w1).toString())
        println("confirm_waiter_woken=" + (await w2).toString())
        // The first channel is unaffected.
        println("first_channel_alive=" + (ch.declareQueue("demux", true, false, false) == "demux").toString())
        conn.close()

        // The broker closes a whole connection (management API: CONNECTION_FORCED): every blocked
        // waiter on every channel is woken with an error instead of hanging.
        const conn2: AmqpConnection091 = AmqpConnection091::connect("127.0.0.1", __PORT__, "/", "tinox", "tinox")
        const c3: AmqpChannel091 = AmqpChannel091::open(conn2)
        const c4: AmqpChannel091 = AmqpChannel091::open(conn2)
        println("phase3_ready=" + (c3.declareQueue("phase3", true, false, false) == "phase3").toString())
        const d1 = spawn Main::deliveryWaiter(c4)
        const d2 = spawn Main::confirmWaiter(c4)
        println("conn_close_delivery_woken=" + (await d1).toString())
        println("conn_close_confirm_woken=" + (await d2).toString())
        println("conn_close_command_fails=" + (c3.declareQueue("phase3", true, false, false) == "").toString())
        return 0
    }
}
"#;

#[test]
fn deliveries_and_confirms_share_one_channel() {
    if Command::new("docker").arg("--version").output().is_err() {
        eprintln!("SKIP: docker not installed");
        return;
    }
    let broker = Broker::start();
    let count = 200;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let version = std::fs::read_to_string(root.join("crates/tinox-core-ext/amqp091/tinox.toml"))
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("version = ").map(|v| v.trim_matches('"').to_string()))
        .unwrap();
    let dir = std::env::temp_dir().join(format!("tinox-amqp091-demux-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("tinox.toml"),
        format!(
            "[package]\ncompiler = \"2.8.0\"\nname = \"demux\"\nversion = \"0.0.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"amqp091\"\nversion = \"{version}\"\n\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"amqp091\"\npath = \"{}\"\n",
            root.join("crates/tinox-core-ext/amqp091").display()
        ),
    )
    .unwrap();
    std::fs::write(dir.join("Main.tnx"), MAIN.replace("__PORT__", &broker.amqp.to_string()).replace("__COUNT__", &count.to_string())).unwrap();
    // Once the program reports its phase-3 queue, force-close its connections from the broker side.
    let out = std::thread::scope(|scope| {
        let killer = scope.spawn(|| {
            for _ in 0..600 {
                if broker.api("GET", "queues/%2F/phase3", None).is_some() {
                    // Connection statistics reach the management API with a delay: retry until listed.
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
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        });
        let out = Command::new(env!("CARGO_BIN_EXE_tinox")).args(["run", "Main.tnx"]).current_dir(&dir).output().unwrap();
        killer.join().unwrap();
        out
    });
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    for expected in ["connected=true", "declared=true", "confirms=true", "consuming=true", &format!("received={count}"), &format!("confirmed={count}"), "declared2=true", "confirms2=true", "mismatch_rejected=true", "delivery_waiter_woken=1", "confirm_waiter_woken=1", "first_channel_alive=true", "phase3_ready=true", "conn_close_delivery_woken=1", "conn_close_confirm_woken=1", "conn_close_command_fails=true"] {
        assert!(stdout.lines().any(|l| l == expected), "missing `{expected}` in:\n{stdout}");
    }
    // The broker agrees: everything was acknowledged, nothing left behind.
    broker.wait("queue drained", || broker.depth("demux") == (0, 0));
    let _ = std::fs::remove_dir_all(&dir);
}
