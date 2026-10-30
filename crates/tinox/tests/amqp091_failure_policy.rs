//! @MessageFailure on an @Amqp091Consumer (issue #537), against a REAL RabbitMQ with independent
//! observers (rabbitmqctl for queue depths, the management HTTP API for publishing and reading the
//! dead-letter queue): transient failure is retried, permanent failure is dead-lettered and never
//! acknowledged as handled, a queue's own DLX still works without a policy, a killed consumer's
//! delivery comes back redelivered, and duplicates can be suppressed with the idempotency window.

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

const ORDERS: &str = r#"import tinox.core.amqp091
import Ledger

@Amqp091Consumer("127.0.0.1", PORT, "/", "tinox", "tinox", "orders")
class Orders
{
    var flaky: Int64
    var started: Int64
    var window: IdempotencyWindow091
    var txTries: Int64

    @OnMessage
    @MessageFailure(3, 50, "orders.dlq")
    fn onMessage(msg: AmqpMessage091) -> Nothing
    {
        if (this.started == 0) {
            this.window = IdempotencyWindow091::create(100)
            this.started = 1
        }
        var text: String = ""
        for i in 0..msg.body.len() {
            text = text + fromCharCode(msg.body[i])
        }
        if (text.startsWith("dup")) {
            if (!this.window.firstSeen(msg.messageId)) {
                println("duplicate " + msg.messageId)
                return
            }
        }
        println("handle " + text + " redelivered=" + msg.redelivered.toString())
        if (text.startsWith("poison")) {
            throw "cannot process " + text
        }
        if (text.startsWith("flaky")) {
            this.flaky = this.flaky + 1
            if (this.flaky < 3) {
                throw "transient"
            }
        }
        if (text.startsWith("hang")) {
            // A handler timeout is the handler's own deadline: an exceeded deadline is an ordinary failure.
            scope {
                taskSetDeadline(200)
                sleep(5000)
            }
        }
        if (text.startsWith("tx-")) {
            // The write is @Transactional: a failing attempt rolls back, the retry starts a fresh transaction.
            this.txTries = this.txTries + 1
            new Ledger().record(text, this.txTries < 3)
        }
        if (text == "slow" && !msg.redelivered) {
            sleep(30000)
        }
        println("done " + text)
    }
}
"#;

const LEDGER: &str = "import tinox.core.db\nimport Entry\n\nclass Ledger\n{\n    @Transactional\n    fn record(text: String, fail: Bool) -> Nothing\n    {\n        DB::of(Entry).save(Entry { id: 0, text: text })\n        if (fail) {\n            throw \"ledger unavailable\"\n        }\n    }\n}\n";
const ENTRY: &str = "@Entity\n@Table(\"entries\")\nclass Entry\n{\n    @Id @GeneratedValue\n    var id: Int64\n    @Column(\"text\")\n    var text: String\n}\n";

const PLAIN: &str = r#"import tinox.core.amqp091

@Amqp091Consumer("127.0.0.1", PORT, "/", "tinox", "tinox", "plain")
class Plain
{
    @OnMessage
    fn onMessage(msg: AmqpMessage091) -> Nothing
    {
        println("plain failing")
        throw "always"
    }
}
"#;

fn start(dir: &Path, log: &str) -> Kill {
    let out = std::fs::File::create(dir.join(log)).unwrap();
    Kill(Command::new(dir.join("app")).current_dir(dir).stdout(Stdio::from(out)).stderr(Stdio::null()).spawn().unwrap())
}

fn log(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap_or_default()
}

#[test]
fn failure_policy_against_real_rabbitmq() {
    if Command::new("docker").arg("--version").output().is_err() {
        eprintln!("SKIP amqp failure policy (docker not installed)");
        return;
    }
    let broker = Broker::start();
    broker.declare("orders", "{}");
    broker.declare("orders.dlq", "{}");
    broker.declare("plain", "{\"x-dead-letter-exchange\":\"\",\"x-dead-letter-routing-key\":\"plain.dead\"}");
    broker.declare("plain.dead", "{}");

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let dir = std::env::temp_dir().join(format!("tinox-amqp-policy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let version = std::fs::read_to_string(root.join("crates/tinox-core-ext/amqp091/tinox.toml"))
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("version = ").map(|v| v.trim_matches('"').to_string()))
        .unwrap();
    let db = dir.join("ledger.db");
    let created = Command::new("sqlite3").arg(&db).arg("CREATE TABLE entries (id INTEGER PRIMARY KEY AUTOINCREMENT, text TEXT NOT NULL);").output().unwrap();
    assert!(created.status.success(), "sqlite3 is required: {created:?}");
    let db_version = std::fs::read_to_string(root.join("crates/tinox-core-ext/db/tinox.toml")).unwrap().lines().find_map(|l| l.strip_prefix("version = ").map(|v| v.trim_matches('"').to_string())).unwrap();
    std::fs::write(
        dir.join("tinox.toml"),
        format!(
            "[package]\ncompiler = \"2.8.0\"\nname = \"policy\"\nversion = \"0.0.0\"\ndescription = \"\"\n\n[database]\ndriver = \"sqlite\"\nurl = \"{}\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"amqp091\"\nversion = \"{version}\"\n\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"amqp091\"\npath = \"{}\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"db\"\nversion = \"{db_version}\"\n\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"db\"\npath = \"{}\"\n",
            db.display(),
            root.join("crates/tinox-core-ext/amqp091").display(),
            root.join("crates/tinox-core-ext/db").display()
        ),
    )
    .unwrap();
    std::fs::write(dir.join("Ledger.tnx"), LEDGER).unwrap();
    std::fs::write(dir.join("Entry.tnx"), ENTRY).unwrap();
    std::fs::write(dir.join("Orders.tnx"), ORDERS.replace("PORT", &broker.amqp.to_string())).unwrap();
    std::fs::write(dir.join("Plain.tnx"), PLAIN.replace("PORT", &broker.amqp.to_string())).unwrap();
    std::fs::write(dir.join("Main.tnx"), "import Orders\nimport Plain\nimport Entry\n\nclass Main\n{\n    fnc main() -> Int32 {\n        return 0\n    }\n}\n").unwrap();
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let install = Command::new(tinox).arg("install").current_dir(&dir).output().unwrap();
    assert!(install.status.success(), "{}", String::from_utf8_lossy(&install.stderr));
    let build = Command::new(tinox).args(["build", "Main.tnx", "-o", "app"]).current_dir(&dir).output().unwrap();
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));

    // ---- transient failure, permanent failure, plain DLX, duplicate ----
    let first = start(&dir, "run1.log");
    std::thread::sleep(Duration::from_secs(2));
    broker.publish("orders", "flaky-1", "m-flaky");
    broker.publish("orders", "poison-1", "m-poison");
    broker.publish("orders", "ok-1", "m-ok");
    broker.publish("orders", "dup-1", "same-id");
    broker.publish("orders", "dup-1", "same-id");
    broker.publish("plain", "bad-1", "m-bad");
    broker.publish("orders", "hang-1", "m-hang");
    broker.publish("orders", "tx-1", "m-tx");
    broker.wait("orders drained and poison dead-lettered", || {
        broker.depth("orders") == (0, 0) && broker.depth("orders.dlq") == (2, 0) && broker.depth("plain") == (0, 0) && broker.depth("plain.dead") == (1, 0)
    });
    let out = log(&dir, "run1.log");
    assert_eq!(out.matches("handle flaky-1").count(), 3, "two transient failures then success:\n{out}");
    assert_eq!(out.matches("done flaky-1").count(), 1, "{out}");
    assert_eq!(out.matches("handle poison-1").count(), 3, "bounded retries, not forever:\n{out}");
    assert_eq!(out.matches("done poison-1").count(), 0, "{out}");
    assert_eq!(out.matches("done ok-1").count(), 1, "{out}");
    assert_eq!(out.matches("handle dup-1").count(), 1, "the duplicate is suppressed:\n{out}");
    assert_eq!(out.matches("duplicate same-id").count(), 1, "{out}");
    assert_eq!(out.matches("plain failing").count(), 1, "no policy: one attempt, then reject without requeue:\n{out}");
    // A handler that overruns its own deadline fails like any other and ends up dead-lettered.
    assert_eq!(out.matches("handle hang-1").count(), 3, "{out}");
    assert_eq!(out.matches("done hang-1").count(), 0, "{out}");
    // Two transactional attempts rolled back, the third committed: exactly one row, written once.
    assert_eq!(out.matches("handle tx-1").count(), 3, "{out}");
    assert_eq!(out.matches("done tx-1").count(), 1, "{out}");
    let rows = Command::new("sqlite3").arg(dir.join("ledger.db")).arg("SELECT group_concat(text, ',') FROM entries").output().unwrap();
    assert_eq!(String::from_utf8_lossy(&rows.stdout).trim(), "tx-1", "failed attempts must not leave rows");
    // The dead letters are the original payloads and keep the message-id, read by an independent client.
    let dead = broker.api("POST", "queues/%2F/orders.dlq/get", Some("{\"count\":5,\"ackmode\":\"ack_requeue_false\",\"encoding\":\"auto\"}")).unwrap();
    assert!(dead.contains("\"payload\":\"poison-1\"") && dead.contains("\"message_id\":\"m-poison\""), "{dead}");
    assert!(dead.contains("\"payload\":\"hang-1\""), "{dead}");
    let plain_dead = broker.api("POST", "queues/%2F/plain.dead/get", Some("{\"count\":5,\"ackmode\":\"ack_requeue_false\",\"encoding\":\"auto\"}")).unwrap();
    assert!(plain_dead.contains("\"payload\":\"bad-1\""), "{plain_dead}");

    // ---- consumer killed mid-handling: the delivery is redelivered, not lost ----
    broker.publish("orders", "slow", "m-slow");
    broker.wait("the slow message being handled", || log(&dir, "run1.log").contains("handle slow redelivered=false"));
    assert_eq!(broker.depth("orders"), (0, 1), "unacknowledged while in flight");
    drop(first); // SIGKILL
    let _second = start(&dir, "run2.log");
    broker.wait("redelivery and completion", || log(&dir, "run2.log").contains("done slow"));
    let out2 = log(&dir, "run2.log");
    assert!(out2.contains("handle slow redelivered=true"), "{out2}");
    broker.wait("orders empty after restart", || broker.depth("orders") == (0, 0));
    let _ = std::fs::remove_dir_all(&dir);
}
