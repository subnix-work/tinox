//! Issue #676: a managed `@Amqp091Consumer` that is idle (no delivery pending) must stop cleanly on
//! SIGTERM. Against a real RabbitMQ; repeated, because the crash was intermittent. The queue is
//! observed independently through rabbitmqctl.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod rabbit_common;
use rabbit_common::Broker;

const CONSUMER: &str = r#"import tinox.core.amqp091

@Amqp091Consumer("127.0.0.1", PORT, "/", "tinox", "tinox", "tasks")
class Tasks
{
    @OnMessage
    fn onMessage(msg: AmqpMessage091) -> Nothing
    {
        var text: String = ""
        for i in 0..msg.body.len() {
            text = text + fromCharCode(msg.body[i])
        }
        println("handled " + text)
    }
}
"#;

const MAIN: &str = r#"import Tasks

@Service(8000, 4000)
class Main
{
    fnc main() -> Int32
    {
        while (!serviceIsReady())
        {
            sleep(20)
        }
        println("READY")
        serviceAwaitShutdown()
        return 0
    }
}
"#;

#[test]
fn idle_consumer_stops_cleanly_on_sigterm() {
    if Command::new("docker").arg("--version").output().is_err() {
        eprintln!("SKIP amqp091 managed shutdown (docker not installed)");
        return;
    }
    let broker = Broker::start();
    broker.declare("tasks", "{}");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let dir = std::env::temp_dir().join(format!("tinox-amqp-shutdown-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let version = std::fs::read_to_string(root.join("crates/tinox-core-ext/amqp091/tinox.toml"))
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("version = ").map(|v| v.trim_matches('"').to_string()))
        .unwrap();
    std::fs::write(
        dir.join("tinox.toml"),
        format!(
            "[package]\ncompiler = \"{}\"\nname = \"shutdown\"\nversion = \"0.0.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"amqp091\"\nversion = \"{version}\"\n\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"amqp091\"\npath = \"{}\"\n",
            env!("CARGO_PKG_VERSION"),
            root.join("crates/tinox-core-ext/amqp091").display()
        ),
    )
    .unwrap();
    std::fs::write(dir.join("Tasks.tnx"), CONSUMER.replace("PORT", &broker.amqp.to_string())).unwrap();
    std::fs::write(dir.join("Main.tnx"), MAIN).unwrap();
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let install = Command::new(tinox).arg("install").current_dir(&dir).output().unwrap();
    assert!(install.status.success(), "{}", String::from_utf8_lossy(&install.stderr));
    let build = Command::new(tinox).args(["build", "Main.tnx", "-o", "app"]).current_dir(&dir).output().unwrap();
    assert!(build.status.success(), "build failed:\n{}", String::from_utf8_lossy(&build.stderr));

    // One message is published and consumed first, so the consumer is proven to work; the queue is
    // drained before every signal, i.e. the consumer is idle in its blocking receive.
    broker.publish("tasks", "first", "m-1");
    let mut statuses = Vec::new();
    for round in 0..15 {
        let mut child = Command::new(dir.join("app")).current_dir(&dir).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut ready = false;
        while Instant::now() < deadline {
            match lines.next() {
                Some(Ok(l)) if l == "READY" => {
                    ready = true;
                    break;
                }
                Some(Ok(_)) => continue,
                _ => break,
            }
        }
        assert!(ready, "round {round}: the service never reported READY");
        broker.wait("queue drained", || broker.depth("tasks") == (0, 0));
        std::thread::sleep(Duration::from_millis(300));
        Command::new("kill").args(["-TERM", &child.id().to_string()]).status().unwrap();
        let start = Instant::now();
        let status = loop {
            if let Some(s) = child.try_wait().unwrap() {
                break s;
            }
            assert!(start.elapsed() < Duration::from_secs(15), "round {round}: no exit after SIGTERM");
            std::thread::sleep(Duration::from_millis(20));
        };
        statuses.push(status.code());
    }
    // Signal termination (-11 on SIGSEGV) shows up as `None` here; a clean stop has an exit code.
    assert!(statuses.iter().all(|c| matches!(c, Some(0))), "exit codes after SIGTERM: {statuses:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
