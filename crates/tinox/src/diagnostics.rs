//! CLI rendering is deliberately separate from diagnostic production.
use tinox_common::diagnostic::{Diagnostic, Failure, SCHEMA_VERSION};

pub fn json(args: &[String]) -> bool {
    args.iter()
        .any(|arg| arg == "--json" || arg == "--error-format=json")
        || args
            .windows(2)
            .any(|pair| pair == ["--error-format", "json"])
        || ndjson(args)
}

/// `--message-format=json` (also `--message-format json`): a stream of one
/// JSON object per line instead of the single `--json` document.
pub fn ndjson(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--message-format=json")
        || args.windows(2).any(|pair| pair == ["--message-format", "json"])
}

/// Build artifact reported by the closing `build-finished` event.
static ARTIFACT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn set_artifact(path: &str) {
    if let Ok(mut a) = ARTIFACT.lock() {
        *a = Some(path.to_string());
    }
}

/// One line of the `--message-format=json` stream. Every event carries
/// `reason` and `schema_version`.
pub fn emit(reason: &str, mut fields: serde_json::Value) {
    if let Some(obj) = fields.as_object_mut() {
        obj.insert("reason".into(), reason.into());
        obj.insert("schema_version".into(), SCHEMA_VERSION.into());
    }
    println!("{fields}");
}

pub fn finish(args: &[String], result: Result<(), Failure>) {
    finish_with_message(args, result, "No errors");
}

pub fn validate_format(args: &[String]) -> Result<(), Failure> {
    for (index, arg) in args.iter().enumerate() {
        let message_format = if arg == "--message-format" {
            Some(args.get(index + 1).map(String::as_str).unwrap_or(""))
        } else {
            arg.strip_prefix("--message-format=")
        };
        if let Some(value) = message_format {
            if !matches!(value, "json" | "text" | "human") {
                return Err(Failure::new("driver", None, None, "--message-format requires json, text or human"));
            }
        }
        let value = if arg == "--error-format" {
            Some(args.get(index + 1).map(String::as_str).unwrap_or(""))
        } else {
            arg.strip_prefix("--error-format=")
        };
        if let Some(value) = value {
            if !matches!(value, "json" | "text") {
                return Err(Failure::new(
                    "driver",
                    None,
                    None,
                    "--error-format requires json or text",
                ));
            }
        }
    }
    Ok(())
}

pub fn finish_with_message(args: &[String], result: Result<(), Failure>, success_message: &str) {
    let success = result.is_ok();
    let diagnostics = result.err().map(|e| e.0).unwrap_or_default();
    if ndjson(args) {
        for d in &diagnostics {
            emit("compiler-message", serde_json::json!({ "diagnostic": d }));
        }
        let artifact = if success { ARTIFACT.lock().ok().and_then(|a| a.clone()) } else { None };
        emit(
            "build-finished",
            serde_json::json!({
                "command": std::env::args().nth(1).unwrap_or_default(),
                "success": success,
                "exit_code": if success { 0 } else { 1 },
                "artifact": artifact,
            }),
        );
    } else if json(args) {
        println!(
            "{}",
            serde_json::json!({
                "schema_version": SCHEMA_VERSION,
                "success": success,
                "diagnostics": diagnostics,
            })
        );
    } else if success {
        println!("{success_message}");
    } else {
        for diagnostic in &diagnostics {
            render(diagnostic);
        }
    }
    std::process::exit(if success { 0 } else { 1 });
}

fn render(diagnostic: &Diagnostic) {
    if let (Some(file), Some(range)) = (&diagnostic.file, diagnostic.range) {
        let source = std::fs::read_to_string(file).unwrap_or_default();
        super::print_error(
            file,
            &source.lines().collect::<Vec<_>>(),
            range,
            &diagnostic.message,
        );
    } else {
        eprintln!("error[{}]: {}", diagnostic.code, diagnostic.message);
    }
    for note in &diagnostic.notes {
        eprintln!("note: {note}");
    }
}

/// Capture native compiler output so even noisy wrappers cannot corrupt stdout.
pub fn tool_output(tool: &str, output: std::process::Output) -> Result<(), Failure> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        eprint!("{stdout}{stderr}");
        Ok(())
    } else {
        Err(Failure::new(
            "toolchain",
            None,
            None,
            format!("{tool} failed ({}):\n{stdout}{stderr}", output.status),
        ))
    }
}
