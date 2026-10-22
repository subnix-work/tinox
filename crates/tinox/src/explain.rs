//! Explain generated wiring without invoking the backend or executing a program.
use super::*;
use serde::Serialize;
use tinox_codegen::BootstrapPlan;

#[derive(Serialize)]
struct Explanation {
    schema_version: u32,
    kind: &'static str,
    success: bool,
    plan: BootstrapPlan,
    call_graph: Vec<String>,
}
fn explain(args: &[String]) -> Result<Explanation, Failure> {
    let mut files = Vec::new();
    let mut bootstrap = false;
    let mut opt = OptLevel::Release;
    for arg in args {
        match arg.as_str() {
            "--bootstrap" => bootstrap = true,
            "--json" => {}
            "--debug" => opt = OptLevel::Debug,
            "--release" => opt = OptLevel::Release,
            _ if arg.starts_with('-') => {
                return Err(Failure::new(
                    "driver",
                    None,
                    None,
                    format!("Unknown explain option: {arg}"),
                ))
            }
            _ => files.push(arg.clone()),
        }
    }
    if !bootstrap || files.len() > 1 {
        return Err(Failure::new(
            "driver",
            None,
            None,
            "Usage: tinox explain --bootstrap [file] [--json] [--debug|--release]",
        ));
    }
    let input = resolve_entry_file(&files).ok_or_else(|| {
        Failure::new(
            "driver",
            None,
            None,
            "No entry source found; specify a file or a project with tinox.toml",
        )
    })?;
    let codegen = generate_program(&input, opt, false)?;
    Ok(Explanation {
        schema_version: 1,
        kind: "bootstrap_explanation",
        success: true,
        plan: codegen.bootstrap_plan(),
        call_graph: vec!["tinox".into(), "graph".into(), input],
    })
}
fn text(explanation: &Explanation) -> String {
    use std::fmt::Write;
    let plan = &explanation.plan;
    let mut output = format!(
        "Bootstrap: {}\nEntry: {}\n",
        plan.mode,
        plan.entry_point.as_deref().unwrap_or("none")
    );
    if plan.lifecycle.enabled {
        writeln!(
            output,
            "Lifecycle: startup {} ms; shutdown {} ms",
            plan.lifecycle.startup_timeout_ms, plan.lifecycle.shutdown_timeout_ms
        )
        .unwrap();
        writeln!(output, "  States: {}", plan.lifecycle.states.join(" -> ")).unwrap();
        for hook in &plan.lifecycle.hooks {
            writeln!(
                output,
                "  Hook: @{} {}.{}",
                hook.kind, hook.class, hook.method
            )
            .unwrap();
        }
        writeln!(output, "  Readiness: {}", plan.lifecycle.readiness).unwrap();
        for step in &plan.lifecycle.shutdown {
            writeln!(output, "  Shutdown: {step}").unwrap();
        }
    }
    for init in &plan.initializers {
        writeln!(
            output,
            "Initialize: {} (priority {}, {}) {}",
            init.name,
            init.priority,
            init.components.join(", "),
            init.configuration
        )
        .unwrap();
    }
    for task in &plan.background_tasks {
        let endpoint = &task.endpoint;
        writeln!(
            output,
            "Spawn: {} {} -> {} ({} {}:{}; {}, {})",
            task.kind,
            task.owner.as_deref().unwrap_or(""),
            task.function,
            endpoint.direction,
            endpoint.host.as_deref().unwrap_or("default bind address"),
            endpoint.port.configured,
            endpoint.port.source,
            task.instance_lifetime
        )
        .unwrap();
        if let Some(path) = &endpoint.path {
            writeln!(output, "  Path: {path}").unwrap();
        }
        if let Some(destination) = &endpoint.destination {
            writeln!(output, "  Destination: {destination}; credentials redacted").unwrap();
        }
        writeln!(
            output,
            "  Concurrency: {}; callbacks: {}",
            task.concurrency,
            task.callbacks.join(", ")
        )
        .unwrap();
    }
    for route in &plan.routes {
        writeln!(
            output,
            "Route: {} {} -> {} [{}; {}; auth required: {}]",
            route.method,
            route.path,
            route.handler,
            route.transport,
            route.controller_allocation,
            route.auth_required
        )
        .unwrap();
    }
    for component in &plan.components {
        writeln!(
            output,
            "Component: {} [{}; {}]",
            component.class, component.scope, component.initialization
        )
        .unwrap();
        for injection in &component.injections {
            writeln!(
                output,
                "  Inject: {} -> {} via {} [{}; {}]",
                injection.field,
                injection.target,
                injection.provider,
                injection.target_scope,
                injection.resolution
            )
            .unwrap();
        }
    }
    for binding in &plan.configuration {
        writeln!(
            output,
            "Config: {}.{} <- {} ({}; value redacted; {})",
            binding.class, binding.field, binding.key, binding.resolution, binding.application
        )
        .unwrap();
    }
    for step in &plan.steps {
        writeln!(
            output,
            "{}: {} ({})",
            step.action, step.target, step.ordering
        )
        .unwrap();
    }
    for note in &plan.notes {
        writeln!(output, "Note: {note}").unwrap();
    }
    writeln!(
        output,
        "Call graph: {}",
        explanation
            .call_graph
            .iter()
            .map(|s| format!("{s:?}"))
            .collect::<Vec<_>>()
            .join(" ")
    )
    .unwrap();
    output
}
pub fn run(args: &[String]) {
    match explain(args) {
        Ok(explanation) => {
            if diagnostics::json(args) {
                println!("{}", serde_json::to_string(&explanation).unwrap());
            } else {
                print!("{}", text(&explanation));
            }
        }
        Err(failure) => {
            if diagnostics::json(args) {
                println!(
                    "{}",
                    serde_json::json!({ "schema_version": 1, "success": false, "diagnostics": failure.0 })
                );
            } else {
                // Do not echo entire annotation source lines: they can contain
                // passwords alongside an unrelated validation error.
                for diagnostic in failure.0 {
                    eprintln!("error[{}]: {}", diagnostic.code, diagnostic.message);
                }
            }
            std::process::exit(1);
        }
    }
}
