//! Issue #548: the exported spans and the automatic message-boundary adapters, checked from what the
//! official OpenTelemetry Collector (and, for AMQP, a real RabbitMQ 4) decoded. Each script skips
//! itself when Docker is not installed.
fn run(script: &str) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("timeout")
        .args(["600", "python3"])
        .arg(root.join("scripts").join(script))
        .output()
        .expect("run collector interoperability check");
    assert!(
        output.status.success(),
        "{script}\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
}

#[test]
fn http_spans_form_parent_child_graphs_in_the_official_collector() {
    run("check-observability-collector.py");
}

#[test]
fn websocket_messages_become_separate_consumer_traces() {
    run("check-observability-adapters.py");
}

#[test]
fn amqp_consumers_become_separate_consumer_traces() {
    run("check-observability-amqp.py");
}
