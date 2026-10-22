//! Independent HTTP peers exercise the shared runtime used by generated routes.
#[test]
fn request_task_dependency_context_and_bounded_otlp_export() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("timeout")
        .args(["60", "python3"])
        .arg(root.join("scripts/check-observability.py"))
        .output()
        .expect("run observability protocol checks");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
}
