//! Shared RabbitMQ container helper for the broker integration tests: a real broker, observed through
//! independent channels (rabbitmqctl for queue depths, the management HTTP API for publishing/reading).
#![allow(dead_code)]
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub struct Broker {
    pub name: String,
    pub amqp: u16,
    pub mgmt: u16,
}

impl Broker {
    pub fn start() -> Broker {
        let name = format!("tinox-rabbit-policy-{}", std::process::id());
        let _ = Command::new("docker").args(["rm", "-f", &name]).output();
        let status = Command::new("docker")
            .args(["run", "-d", "--rm", "--name", &name, "-e", "RABBITMQ_DEFAULT_USER=tinox", "-e", "RABBITMQ_DEFAULT_PASS=tinox", "-P", "rabbitmq:4-management"])
            .stdout(Stdio::null())
            .status()
            .expect("docker run");
        assert!(status.success(), "docker run rabbitmq failed");
        let port = |p: &str| -> u16 {
            let out = Command::new("docker").args(["port", &name, p]).output().unwrap();
            let s = String::from_utf8_lossy(&out.stdout);
            s.lines().next().and_then(|l| l.rsplit(':').next()).and_then(|v| v.trim().parse().ok()).unwrap_or_else(|| panic!("port {p}: {s}"))
        };
        let b = Broker { amqp: port("5672/tcp"), mgmt: port("15672/tcp"), name };
        let deadline = Instant::now() + Duration::from_secs(120);
        while Instant::now() < deadline {
            if b.api("GET", "overview", None).is_some() {
                return b;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        panic!("rabbitmq never became ready");
    }

    pub fn api(&self, method: &str, path: &str, body: Option<&str>) -> Option<String> {
        let mut cmd = Command::new("curl");
        cmd.args(["-s", "-f", "-u", "tinox:tinox", "-X", method, "-H", "content-type: application/json"]);
        if let Some(b) = body {
            cmd.args(["-d", b]);
        }
        cmd.arg(format!("http://127.0.0.1:{}/api/{path}", self.mgmt));
        let out = cmd.output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
    }

    pub fn declare(&self, queue: &str, args: &str) {
        self.api("PUT", &format!("queues/%2F/{queue}"), Some(&format!("{{\"durable\":true,\"auto_delete\":false,\"arguments\":{args}}}"))).unwrap_or_else(|| panic!("declare {queue}"));
    }

    pub fn publish(&self, queue: &str, payload: &str, message_id: &str) {
        let body = format!("{{\"properties\":{{\"message_id\":\"{message_id}\",\"content_type\":\"text/plain\"}},\"routing_key\":\"{queue}\",\"payload\":\"{payload}\",\"payload_encoding\":\"string\"}}");
        let r = self.api("POST", "exchanges/%2F/amq.default/publish", Some(&body)).expect("publish");
        assert!(r.contains("\"routed\":true"), "{r}");
    }

    /// (ready, unacknowledged) from the broker itself (not the possibly stale management stats).
    pub fn depth(&self, queue: &str) -> (u64, u64) {
        let out = Command::new("docker").args(["exec", &self.name, "rabbitmqctl", "-q", "list_queues", "name", "messages_ready", "messages_unacknowledged"]).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        for line in text.lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() == 3 && f[0] == queue {
                return (f[1].parse().unwrap(), f[2].parse().unwrap());
            }
        }
        (0, 0)
    }

    pub fn wait(&self, what: &str, mut ok: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            if ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        panic!("timed out waiting for {what}");
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "-f", &self.name]).output();
    }
}

