//! @MessageFailure on an @Amqp10Consumer (issue #537), against a REAL RabbitMQ speaking AMQP 1.0, observed with
//! independent tools (rabbitmqctl queue depths, management API): transient failure is retried, permanent failure is
//! rejected and dead-lettered by the queue's own DLX (never accepted as handled), a consumer without a policy gets one
//! attempt, and a consumer killed mid-handler leaves the delivery for redelivery.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

mod rabbit_common;
use rabbit_common::Broker;

struct Kill(Child);
impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

const ORDERS: &str = r#"import tinox.core.amqp10
import tinox.core.env

@Amqp10Consumer("127.0.0.1", PORT, "tinox", "tinox", "/queues/q10")
class Orders10
{
    var flaky: Int64

    @OnMessage
    @MessageFailure(3, 50)
    fn onMessage(msg: Amqp10Message) -> Nothing
    {
        var text: String = ""
        for i in 0..msg.body.len() {
            text = text + fromCharCode(msg.body[i])
        }
        println("handle " + text)
        if (text.startsWith("poison")) {
            throw "cannot process " + text
        }
        if (text.startsWith("flaky")) {
            this.flaky = this.flaky + 1
            if (this.flaky < 3) {
                throw "transient"
            }
        }
        if (text == "slow" && Env::getVar("RESTARTED") == "") {
            sleep(30000)
        }
        println("done " + text)
    }
}
"#;

const PLAIN: &str = r#"import tinox.core.amqp10

@Amqp10Consumer("127.0.0.1", PORT, "tinox", "tinox", "/queues/plain10")
class Plain10
{
    @OnMessage
    fn onMessage(msg: Amqp10Message) -> Nothing
    {
        println("plain failing")
        throw "always"
    }
}
"#;

fn start(dir: &Path, log: &str, restarted: bool) -> Kill {
    let out = std::fs::File::create(dir.join(log)).unwrap();
    let mut cmd = Command::new(dir.join("app"));
    cmd.current_dir(dir).stdout(Stdio::from(out)).stderr(Stdio::null());
    if restarted {
        cmd.env("RESTARTED", "1");
    }
    Kill(cmd.spawn().unwrap())
}

fn log(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap_or_default()
}

#[test]
fn amqp10_failure_policy_against_real_rabbitmq() {
    if Command::new("docker").arg("--version").output().is_err() {
        eprintln!("SKIP amqp10 failure policy (docker not installed)");
        return;
    }
    let broker = Broker::start();
    let dead = "{\"x-dead-letter-exchange\":\"\",\"x-dead-letter-routing-key\":\"KEY\"}";
    broker.declare("q10", &dead.replace("KEY", "q10.dead"));
    broker.declare("q10.dead", "{}");
    broker.declare("plain10", &dead.replace("KEY", "plain10.dead"));
    broker.declare("plain10.dead", "{}");

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let dir = std::env::temp_dir().join(format!("tinox-amqp10-policy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let version = std::fs::read_to_string(root.join("crates/tinox-core-ext/amqp10/tinox.toml"))
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("version = ").map(|v| v.trim_matches('"').to_string()))
        .unwrap();
    std::fs::write(
        dir.join("tinox.toml"),
        format!(
            "[package]\ncompiler = \"2.8.0\"\nname = \"policy10\"\nversion = \"0.0.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"amqp10\"\nversion = \"{version}\"\n\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"amqp10\"\npath = \"{}\"\n",
            root.join("crates/tinox-core-ext/amqp10").display()
        ),
    )
    .unwrap();
    let port = broker.amqp.to_string();
    std::fs::write(dir.join("Orders10.tnx"), ORDERS.replace("PORT", &port)).unwrap();
    std::fs::write(dir.join("Plain10.tnx"), PLAIN.replace("PORT", &port)).unwrap();
    std::fs::write(dir.join("Main.tnx"), "import Orders10\nimport Plain10\n\nclass Main\n{\n    fnc main() -> Int32 {\n        return 0\n    }\n}\n").unwrap();
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let install = Command::new(tinox).arg("install").current_dir(&dir).output().unwrap();
    assert!(install.status.success(), "{}", String::from_utf8_lossy(&install.stderr));
    let build = Command::new(tinox).args(["build", "Main.tnx", "-o", "app"]).current_dir(&dir).output().unwrap();
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));

    let first = start(&dir, "run1.log", false);
    std::thread::sleep(Duration::from_secs(2));
    broker.publish("q10", "flaky-1", "m-flaky");
    broker.publish("q10", "poison-1", "m-poison");
    broker.publish("q10", "ok-1", "m-ok");
    broker.publish("plain10", "bad-1", "m-bad");
    broker.wait("queues drained, poison dead-lettered by the broker", || {
        broker.depth("q10") == (0, 0) && broker.depth("q10.dead") == (1, 0) && broker.depth("plain10") == (0, 0) && broker.depth("plain10.dead") == (1, 0)
    });
    let out = log(&dir, "run1.log");
    assert_eq!(out.matches("handle flaky-1").count(), 3, "two transient failures then success:\n{out}");
    assert_eq!(out.matches("done flaky-1").count(), 1, "{out}");
    assert_eq!(out.matches("handle poison-1").count(), 3, "bounded retries:\n{out}");
    assert_eq!(out.matches("done poison-1").count(), 0, "{out}");
    assert_eq!(out.matches("done ok-1").count(), 1, "{out}");
    assert_eq!(out.matches("plain failing").count(), 1, "no policy: a single attempt:\n{out}");
    let dead_q = broker.api("POST", "queues/%2F/q10.dead/get", Some("{\"count\":5,\"ackmode\":\"ack_requeue_false\",\"encoding\":\"auto\"}")).unwrap();
    assert!(dead_q.contains("poison-1"), "{dead_q}");
    let plain_dead = broker.api("POST", "queues/%2F/plain10.dead/get", Some("{\"count\":5,\"ackmode\":\"ack_requeue_false\",\"encoding\":\"auto\"}")).unwrap();
    assert!(plain_dead.contains("bad-1"), "{plain_dead}");

    // A consumer killed while handling leaves the delivery unsettled: the broker redelivers it to the next one.
    broker.publish("q10", "slow", "m-slow");
    broker.wait("the slow message being handled", || log(&dir, "run1.log").contains("handle slow"));
    assert_eq!(broker.depth("q10"), (0, 1), "unsettled while in flight");
    drop(first);
    let _second = start(&dir, "run2.log", true);
    broker.wait("redelivery and completion", || log(&dir, "run2.log").contains("done slow"));
    broker.wait("q10 empty after restart", || broker.depth("q10") == (0, 0));
    let _ = std::fs::remove_dir_all(&dir);
}
