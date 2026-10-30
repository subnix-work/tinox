//! Transactional outbox (issue #538) against a real PostgreSQL and a real RabbitMQ, with independent
//! observers (psql, rabbitmqctl, the management API): the outbox row commits/rolls back with the business
//! row, a committed record is never lost when the writer is gone before publishing, and a crash between the
//! broker's confirm and the "sent" mark leads to a repeated delivery with the SAME message-id (at-least-once).

use std::path::PathBuf;
use std::process::{Command, Output};

mod pg_common;
mod rabbit_common;
use pg_common::{repo_root, tool_available, PgContainer};
use rabbit_common::Broker;

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

const DDL: &str = "CREATE TABLE shop_orders (id BIGSERIAL PRIMARY KEY, code TEXT NOT NULL);
CREATE TABLE tinox_outbox (id BIGSERIAL PRIMARY KEY, destination TEXT NOT NULL, message_id TEXT NOT NULL, payload TEXT NOT NULL, status BIGINT NOT NULL, attempts BIGINT NOT NULL, last_error TEXT NOT NULL, created_at BIGINT NOT NULL, next_attempt_at BIGINT NOT NULL, sent_at BIGINT NOT NULL);";

#[test]
fn outbox_survives_crashes_with_at_least_once_delivery() {
    if !tool_available("docker", &["--version"]) || !tool_available("psql", &["--version"]) {
        eprintln!("SKIP outbox (docker/psql not installed)");
        return;
    }
    let pg = PgContainer::start("postgres:16-alpine");
    pg.psql(DDL);
    let broker = Broker::start();
    broker.declare("outbox.q", "{}");

    let root = repo_root();
    let dir: PathBuf = std::env::temp_dir().join(format!("tinox-outbox-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    for f in ["Shop.tnx", "Orders.tnx", "Main.tnx"] {
        std::fs::copy(root.join("tests/fixtures/outbox").join(f), dir.join("src").join(f)).unwrap();
    }
    let version = |m: &str| {
        std::fs::read_to_string(root.join("crates/tinox-core-ext").join(m).join("tinox.toml"))
            .unwrap()
            .lines()
            .find_map(|l| l.strip_prefix("version = ").map(|v| v.trim_matches('"').to_string()))
            .unwrap()
    };
    let mut toml = format!("[package]\ncompiler = \"2.8.0\"\nname = \"outboxapp\"\nversion = \"0.0.0\"\ndescription = \"\"\n\n[database]\ndriver = \"postgres\"\nurl = \"{}\"\npool = 3\n", pg.url());
    for m in ["db", "amqp091", "outbox"] {
        toml.push_str(&format!("\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"{m}\"\nversion = \"{}\"\n", version(m)));
        toml.push_str(&format!("\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"{m}\"\npath = \"{}\"\n", root.join("crates/tinox-core-ext").join(m).display()));
    }
    std::fs::write(dir.join("tinox.toml"), toml).unwrap();
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let install = Command::new(tinox).arg("install").current_dir(&dir).output().unwrap();
    assert!(install.status.success(), "{}", text(&install));
    let build = Command::new(tinox).args(["build", "src/Main.tnx", "-o", "app"]).current_dir(&dir).output().unwrap();
    assert!(build.status.success(), "{}", text(&build));
    let run = |mode: &str, refs: &str| -> Output {
        Command::new(dir.join("app"))
            .current_dir(&dir)
            .env("MODE", mode)
            .env("REFS", refs)
            .env("AMQP_PORT", broker.amqp.to_string())
            .output()
            .unwrap()
    };

    // 1. Business row + outbox record commit together; the failing order leaves neither behind.
    let w = run("write", "1,2,3");
    let out = text(&w);
    assert!(w.status.success(), "{out}");
    assert!(out.contains("rolled back 3: payment declined") && out.contains("backlog=2"), "{out}");
    assert_eq!(pg.psql("SELECT string_agg(code, ',' ORDER BY code) FROM shop_orders"), "1,2");
    assert_eq!(pg.psql("SELECT string_agg(message_id || ':' || status, ',' ORDER BY id) FROM tinox_outbox"), "order-1:0,order-2:0");
    assert_eq!(broker.depth("outbox.q"), (0, 0), "nothing is published by the writer");

    // 2. The writer is gone (it crashed or simply ended) before anything was published: a later dispatcher
    //    still delivers every committed record, marks it sent and the backlog drains.
    let d = run("dispatch", "");
    let out = text(&d);
    assert!(d.status.success(), "{out}");
    assert!(out.contains("backlog=0 parked=0"), "{out}");
    assert_eq!(broker.depth("outbox.q"), (2, 0));
    assert_eq!(pg.psql("SELECT string_agg(message_id || ':' || status, ',' ORDER BY id) FROM tinox_outbox"), "order-1:1,order-2:1");
    let got = broker.api("POST", "queues/%2F/outbox.q/get", Some("{\"count\":10,\"ackmode\":\"ack_requeue_false\",\"encoding\":\"auto\"}")).unwrap();
    assert!(got.contains("\"message_id\":\"order-1\"") && got.contains("\"message_id\":\"order-2\"") && got.contains("payload-1"), "{got}");

    // 3. Crash between the broker's confirm and the "sent" mark: the mark fails (a trigger stands in for the
    //    process dying right there), the record stays pending although the broker has the message ...
    pg.psql("CREATE FUNCTION tinox_block_mark() RETURNS trigger AS $$ BEGIN RAISE EXCEPTION 'simulated crash before the mark'; END $$ LANGUAGE plpgsql;
             CREATE TRIGGER tinox_block BEFORE UPDATE ON tinox_outbox FOR EACH ROW WHEN (NEW.status = 1) EXECUTE FUNCTION tinox_block_mark();");
    assert!(run("write", "4").status.success());
    let crashed = run("dispatch", "");
    assert!(!crashed.status.success(), "the dispatcher must die at the failing mark:\n{}", text(&crashed));
    assert_eq!(broker.depth("outbox.q"), (1, 0), "the broker already has the confirmed message");
    assert_eq!(pg.psql("SELECT status FROM tinox_outbox WHERE message_id = 'order-4'"), "0");
    // ... and after the restart it is published again with the same key (at-least-once, not exactly-once).
    pg.psql("DROP TRIGGER tinox_block ON tinox_outbox");
    let again = run("dispatch", "");
    assert!(again.status.success(), "{}", text(&again));
    assert_eq!(pg.psql("SELECT status FROM tinox_outbox WHERE message_id = 'order-4'"), "1");
    assert_eq!(broker.depth("outbox.q"), (2, 0));
    let dup = broker.api("POST", "queues/%2F/outbox.q/get", Some("{\"count\":10,\"ackmode\":\"ack_requeue_false\",\"encoding\":\"auto\"}")).unwrap();
    assert_eq!(dup.matches("\"message_id\":\"order-4\"").count(), 2, "same message-id twice, so a consumer can deduplicate:\n{dup}");
    let _ = std::fs::remove_dir_all(&dir);
}

const RELAY: &str = "import tinox.core.outbox\n\n@OutboxDispatcher(\"127.0.0.1\", PORT, \"/\", \"tinox\", \"tinox\", 3, 50)\nclass Relay\n{\n}\n";
const SERVICE_MAIN: &str = "import Relay\n\n@Service(5000, 5000)\nclass Main\n{\n    fnc main() -> Int32 {\n        return 0\n    }\n}\n";

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

#[test]
fn annotated_dispatcher_runs_with_the_service_parks_after_nacks_and_drains_on_sigterm() {
    if !tool_available("docker", &["--version"]) || !tool_available("psql", &["--version"]) {
        eprintln!("SKIP outbox lifecycle (docker/psql not installed)");
        return;
    }
    let pg = PgContainer::start("postgres:16-alpine");
    pg.psql(DDL);
    let broker = Broker::start();
    broker.declare("outbox.q", "{}");
    // A queue that holds one message and then nacks further publishes: a real, broker-made publish failure.
    broker.declare("tiny", "{\"x-max-length\":1,\"x-overflow\":\"reject-publish\"}");

    let root = repo_root();
    let dir: PathBuf = std::env::temp_dir().join(format!("tinox-outbox-service-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/Relay.tnx"), RELAY.replace("PORT", &broker.amqp.to_string())).unwrap();
    std::fs::write(dir.join("src/Main.tnx"), SERVICE_MAIN).unwrap();
    let version = |m: &str| {
        std::fs::read_to_string(root.join("crates/tinox-core-ext").join(m).join("tinox.toml"))
            .unwrap()
            .lines()
            .find_map(|l| l.strip_prefix("version = ").map(|v| v.trim_matches('"').to_string()))
            .unwrap()
    };
    let mut toml = format!("[package]\ncompiler = \"2.8.0\"\nname = \"relay\"\nversion = \"0.0.0\"\ndescription = \"\"\n\n[startup]\nbanner = false\n\n[database]\ndriver = \"postgres\"\nurl = \"{}\"\npool = 3\n", pg.url());
    for m in ["db", "amqp091", "outbox"] {
        toml.push_str(&format!("\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"{m}\"\nversion = \"{}\"\n", version(m)));
        toml.push_str(&format!("\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"{m}\"\npath = \"{}\"\n", root.join("crates/tinox-core-ext").join(m).display()));
    }
    std::fs::write(dir.join("tinox.toml"), toml).unwrap();
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let install = Command::new(tinox).arg("install").current_dir(&dir).output().unwrap();
    assert!(install.status.success(), "{}", text(&install));
    let build = Command::new(tinox).args(["build", "src/Main.tnx", "-o", "app"]).current_dir(&dir).output().unwrap();
    assert!(build.status.success(), "{}", text(&build));

    // Records written by an independent client (psql) before the service even starts ...
    let row = |dest: &str, id: &str| format!("INSERT INTO tinox_outbox (destination, message_id, payload, status, attempts, last_error, created_at, next_attempt_at, sent_at) VALUES ('{dest}', '{id}', 'body-{id}', 0, 0, '', 1, 0, 0);");
    pg.psql(&format!("{}{}{}{}", row("outbox.q", "svc-1"), row("outbox.q", "svc-2"), row("tiny", "tiny-1"), row("tiny", "tiny-2")));
    let log = std::fs::File::create(dir.join("service.log")).unwrap();
    let mut child = Command::new(dir.join("app")).current_dir(&dir).stdout(log.try_clone().unwrap()).stderr(log).spawn().unwrap();

    // ... are published by the annotated dispatcher without any code calling it.
    broker.wait("annotated dispatcher publishing", || broker.depth("outbox.q") == (2, 0) && broker.depth("tiny") == (1, 0));
    // The nacked record is retried with backoff and then parked instead of retrying forever.
    broker.wait("parking after the nacks", || pg.psql("SELECT status || ':' || attempts FROM tinox_outbox WHERE message_id = 'tiny-2'") == "2:3");
    assert_eq!(pg.psql("SELECT string_agg(message_id || ':' || status, ',' ORDER BY id) FROM tinox_outbox WHERE destination = 'outbox.q' OR message_id = 'tiny-1'"), "svc-1:1,svc-2:1,tiny-1:1");
    assert!(pg.psql("SELECT last_error FROM tinox_outbox WHERE message_id = 'tiny-2'").contains("nacked"));

    // New work arrives while it runs; SIGTERM then stops the service cleanly (exit 0) and nothing is lost.
    pg.psql(&row("outbox.q", "svc-3"));
    broker.wait("late record published", || broker.depth("outbox.q") == (3, 0));
    unsafe { kill(child.id() as i32, 15) };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(std::time::Instant::now() < deadline, "the service did not stop on SIGTERM:\n{}", std::fs::read_to_string(dir.join("service.log")).unwrap_or_default());
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    assert!(status.success(), "unclean stop: {status:?}\n{}", std::fs::read_to_string(dir.join("service.log")).unwrap_or_default());
    assert_eq!(pg.psql("SELECT count(*) FROM tinox_outbox WHERE status = 0"), "0");
    let _ = std::fs::remove_dir_all(&dir);
}
