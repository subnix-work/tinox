//! Issue #540: real AMQP 1.0 link-credit and session-window accounting. Against a REAL RabbitMQ 4
//! (independent observer: rabbitmqctl queue depths): a sender that pushes far more messages than the
//! broker's initial credit and session window stalls and resumes on every flow; a receiver with a
//! small credit window never has more than that many unread transfers outstanding, and keeps going
//! past the advertised session window because consumption reopens it; serial arithmetic wraps mod 2^32.

use std::path::PathBuf;
use std::process::Command;

mod rabbit_common;
use rabbit_common::Broker;

const MAIN: &str = r#"import tinox.core.amqp10
import tinox.core.encoding

class Main
{
    fnc main() -> Int32 {
        println("wrap_a=" + Amqp10Session::serialDiff(5, 4294967290).toString())
        println("wrap_b=" + Amqp10Session::serialDiff(3, 4).toString())
        println("wrap_c=" + Amqp10Session::serialDiff(10, 10).toString())
        println("wrap_d=" + Amqp10Session::serialDiff(0, 4294967295).toString())

        const conn: Amqp10Connection = Amqp10Connection::connect("127.0.0.1", __PORT__, "tinox", "tinox")
        const s: Amqp10Session = Amqp10Session::begin(conn)
        const p: Amqp10Link = Amqp10Link::attach(s, "p", false, "/queues/fc")
        println("attached=" + (s.errorMessage == "" && p.errorMessage == "").toString())
        var i: Int64 = 0
        while i < __COUNT__ {
            p.publish(Encoding::toUtf8("m" + i.toString()), "text/plain")
            i = i + 1
        }
        println("publish_errors=" + p.errorMessage.len().toString())
        println("sent=" + p.deliveryCount.toString())
        println("window_updates_seen=" + (s.inbox.windowSeq > 1).toString())
        println("credit_updates_seen=" + (p.inbox.flowSeq > 1).toString())
        p.detach()
        s.end()

        const s2: Amqp10Session = Amqp10Session::begin(conn)
        const r: Amqp10Link = Amqp10Link::attach(s2, "r", true, "/queues/fc")
        r.consumeWithWindow(10)
        sleep_ms(1500)
        const outstanding = s2.inbox.received - s2.router.takenOn(s2.chanId)
        println("outstanding_ok=" + (outstanding <= 10 && outstanding > 0).toString())
        var ordered: Int64 = 0
        var n: Int64 = 0
        while n < __COUNT__ {
            const m: Amqp10Message = r.nextMessage()
            if (!m.ok) {
                println("recv_error=" + m.errorMessage)
                n = __COUNT__
            } else {
                var text: String = ""
                for j in 0..m.body.len() {
                    text = text + fromCharCode(m.body[j])
                }
                if (text == "m" + n.toString()) { ordered = ordered + 1 }
                r.ack(m.deliveryId)
                n = n + 1
            }
        }
        println("ordered=" + ordered.toString())
        println("max_window_reopened=" + (s2.advertisedEdge > Amqp10Session::incomingWindow()).toString())
        r.detach()
        s2.end()
        conn.close()
        return 0
    }
}
"#;

#[test]
fn credit_and_window_are_accounted_against_a_real_broker() {
    if Command::new("docker").arg("--version").output().is_err() {
        eprintln!("SKIP: docker not installed");
        return;
    }
    let broker = Broker::start();
    broker.declare("fc", "{}");
    let count = 3000;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let version = std::fs::read_to_string(root.join("crates/tinox-core-ext/amqp10/tinox.toml"))
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("version = ").map(|v| v.trim_matches('"').to_string()))
        .unwrap();
    let dir = std::env::temp_dir().join(format!("tinox-amqp10-flow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("tinox.toml"),
        format!(
            "[package]\ncompiler = \"2.8.0\"\nname = \"flow\"\nversion = \"0.0.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"amqp10\"\nversion = \"{version}\"\n\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"amqp10\"\npath = \"{}\"\n",
            root.join("crates/tinox-core-ext/amqp10").display()
        ),
    )
    .unwrap();
    std::fs::write(dir.join("Main.tnx"), MAIN.replace("__PORT__", &broker.amqp.to_string()).replace("__COUNT__", &count.to_string())).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_tinox")).args(["run", "Main.tnx"]).current_dir(&dir).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    for expected in [
        "wrap_a=11",
        "wrap_b=0",
        "wrap_c=0",
        "wrap_d=1",
        "attached=true",
        "publish_errors=0",
        &format!("sent={count}"),
        "window_updates_seen=true",
        "credit_updates_seen=true",
        "outstanding_ok=true",
        &format!("ordered={count}"),
        "max_window_reopened=true",
    ] {
        assert!(stdout.lines().any(|l| l == expected), "missing `{expected}` in:\n{stdout}");
    }
    broker.wait("queue drained", || broker.depth("fc") == (0, 0));
    let _ = std::fs::remove_dir_all(&dir);
}
