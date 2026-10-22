//! CLI rendering is deliberately separate from diagnostic production.
use tinox_common::diagnostic::{Diagnostic, Failure, SCHEMA_VERSION};

pub fn json(args: &[String]) -> bool {
    args.iter()
        .any(|arg| arg == "--json" || arg == "--error-format=json")
        || args
            .windows(2)
            .any(|pair| pair == ["--error-format", "json"])
}

pub fn finish(args: &[String], result: Result<(), Failure>) {
    finish_with_message(args, result, "No errors");
}

pub fn validate_format(args: &[String]) -> Result<(), Failure> {
    for (index, arg) in args.iter().enumerate() {
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
    if json(args) {
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
