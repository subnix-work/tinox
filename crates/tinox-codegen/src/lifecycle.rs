//! Explicit service policy shared by lowering and bootstrap inspection.
use super::CodeGen;
use serde::Serialize;
use std::{collections::BTreeSet, fmt::Write};
use tinox_parser::{AnnotationArg, Class, Literal};

#[derive(Clone, Debug)]
pub struct ServiceOptions {
    pub startup_ms: i64,
    pub shutdown_ms: i64,
}
#[derive(Clone, Debug, Serialize)]
pub struct Hook {
    pub class: String,
    pub method: String,
    pub kind: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct LifecyclePlan {
    pub enabled: bool,
    pub startup_timeout_ms: i64,
    pub shutdown_timeout_ms: i64,
    pub states: Vec<&'static str>,
    pub hooks: Vec<Hook>,
    pub readiness: &'static str,
    pub shutdown: Vec<&'static str>,
    pub health_paths: Vec<&'static str>,
}
impl CodeGen {
    pub(super) fn configure_lifecycle(&mut self, classes: &[Class]) {
        for class in classes {
            if let Some(annotation) = class.annotations.iter().find(|a| a.name == "Service") {
                let duration = |index: usize, default| match annotation.args.get(index) {
                    Some(AnnotationArg::Literal(Literal::Integer(value))) => *value,
                    _ => default,
                };
                self.service_options = Some(ServiceOptions {
                    startup_ms: duration(0, 30000),
                    shutdown_ms: duration(1, 5000),
                });
            }
            for method in &class.methods {
                for annotation in &method.annotations {
                    if matches!(
                        annotation.name.as_str(),
                        "OnStart" | "OnStop" | "Readiness" | "Liveness"
                    ) {
                        self.lifecycle_hooks.push(Hook {
                            class: class.name.clone(),
                            method: method.name.clone(),
                            kind: annotation.name.clone(),
                        });
                    }
                }
            }
        }
        let owners: BTreeSet<_> = self
            .lifecycle_hooks
            .iter()
            .map(|h| h.class.clone())
            .collect();
        fn visit(
            name: &str,
            components: &[super::DiComponentInfo],
            seen: &mut BTreeSet<String>,
            order: &mut Vec<String>,
        ) {
            if !seen.insert(name.into()) {
                return;
            }
            if let Some(component) = components.iter().find(|c| c.class_name == name) {
                let mut dependencies: Vec<_> = component
                    .inject_fields
                    .iter()
                    .map(|f| f.provider_class.as_str())
                    .collect();
                dependencies.sort_unstable();
                for dependency in dependencies {
                    visit(dependency, components, seen, order);
                }
            }
            order.push(name.into());
        }
        let mut order = Vec::new();
        let mut seen = BTreeSet::new();
        for owner in &owners {
            visit(owner, &self.di_components, &mut seen, &mut order);
        }
        self.lifecycle_hooks.sort_by(|a, b| {
            (order.iter().position(|n| n == &a.class), &a.method, &a.kind).cmp(&(
                order.iter().position(|n| n == &b.class),
                &b.method,
                &b.kind,
            ))
        });
    }
    pub(super) fn service_enabled(&self) -> bool {
        self.service_options.is_some() && self.test_entries.is_empty()
    }
    pub fn lifecycle_plan(&self) -> LifecyclePlan {
        LifecyclePlan {
            enabled: self.service_enabled(),
            startup_timeout_ms: self.service_options.as_ref().map_or(30000, |o| o.startup_ms),
            shutdown_timeout_ms: self.service_options.as_ref().map_or(5000, |o| o.shutdown_ms),
            states: vec!["initializing", "starting", "ready", "unready", "draining", "stopping", "stopped", "failed"],
            hooks: self.lifecycle_hooks.clone(),
            readiness: "after explicit initializers, OnStart hooks, all required listeners/consumer handshakes and successful Readiness contributions; Main is not a readiness barrier",
            shutdown: vec!["mark unready and stop admission", "drain admitted callbacks and acknowledgements", "join listeners and connection workers", "cancel and join remaining structured children", "OnStop hooks in reverse owner/method order", "close database pools", "exit nonzero on failure or force process termination at the common deadline"],
            health_paths: vec!["/health/live", "/health/ready"],
        }
    }
    pub(super) fn emit_service_declarations(&mut self) {
        for (name, signature) in [
            ("tinox_service_begin", "void (i64, i64, i64)"),
            ("tinox_service_hook_begin", "void (i8*)"),
            ("tinox_service_hook_end", "void ()"),
            ("tinox_service_initialized", "void ()"),
            ("tinox_service_task_enter", "void (i64)"),
            ("tinox_service_task_exit", "void (i64)"),
            ("tinox_service_listener_ready", "void (i64)"),
            ("tinox_service_main_returned", "void (i64)"),
            ("tinox_service_finish", "i64 (i64)"),
            ("tinox_service_work_enter", "i64 ()"),
            ("tinox_service_existing_work_enter", "void ()"),
            ("tinox_service_work_exit", "void ()"),
            ("tinox_service_worker_start", "i64 ()"),
            ("tinox_service_worker_end", "void ()"),
            ("tinox_service_check_error", "i64 (i8*)"),
            ("tinox_service_cleanup_error", "void (i8*)"),
            ("tinox_service_cleanup_enter", "void ()"),
            ("tinox_service_failed", "i64 ()"),
            ("tinox_service_close_connection", "void (i64)"),
            ("tinox_service_unavailable", "void (i64)"),
            ("tinox_service_request_done", "void (i64)"),
            ("tinox_service_health_response", "void (i64, i64)"),
            ("serviceIsReady", "i1 ()"),
            ("serviceIsLive", "i1 ()"),
            ("serviceIsStopping", "i1 ()"),
            ("serviceState", "i8* ()"),
            ("serviceStatusJson", "i8* ()"),
            ("serviceStartedMs", "i64 ()"),
            ("serviceReadyMs", "i64 ()"),
            ("serviceShutdownMs", "i64 ()"),
            ("serviceBoundPort", "i64 (i64)"),
            ("serviceAwaitReady", "i1 (i64)"),
            ("serviceRequestShutdown", "void ()"),
            ("serviceAwaitShutdown", "void ()"),
        ] {
            let (result, args) = signature.split_once(' ').unwrap();
            writeln!(&mut self.ir, "declare {result} @{name}{args}").unwrap();
        }
    }
    pub(super) fn emit_lifecycle_hooks(&mut self) {
        if !self.service_enabled() {
            return;
        }
        let hooks = self.lifecycle_hooks.clone();
        let mut owners: Vec<String> = Vec::new();
        for hook in &hooks {
            if !owners.contains(&hook.class) {
                owners.push(hook.class.clone());
            }
        }
        for owner in &owners {
            writeln!(
                &mut self.lambda_ir,
                "@__service_{owner}_initialized = internal global i1 false"
            )
            .unwrap();
        }
        writeln!(&mut self.lambda_ir, "define i64 @__tinox_service_start_hooks() {{\nentry.tnx:\n  %already_stopping = call i1 @serviceIsStopping()\n  br i1 %already_stopping, label %failed, label %initialize\ninitialize:").unwrap();
        for owner in &owners {
            writeln!(
                &mut self.lambda_ir,
                "  %owner_{owner} = call i64* @{owner}_di_get()"
            )
            .unwrap();
            let label = self
                .emit_lambda_string_literal(&format!("Initialize lifecycle owner {owner} failed"));
            writeln!(&mut self.lambda_ir, "  %owner_ok_{owner} = call i64 @tinox_service_check_error(i8* {label})\n  %owner_valid_{owner} = icmp ne i64 %owner_ok_{owner}, 0\n  br i1 %owner_valid_{owner}, label %owner_next_{owner}, label %failed\nowner_next_{owner}:").unwrap();
            writeln!(
                &mut self.lambda_ir,
                "  store i1 true, i1* @__service_{owner}_initialized"
            )
            .unwrap();
        }
        for (index, hook) in hooks.iter().filter(|h| h.kind == "OnStart").enumerate() {
            let label = self.emit_lambda_string_literal(&format!(
                "OnStart {}.{} failed",
                hook.class, hook.method
            ));
            writeln!(&mut self.lambda_ir, "  call void @tinox_service_hook_begin(i8* {label})\n  call void @{}_{}(i64* %owner_{})\n  call void @tinox_service_hook_end()", hook.class, hook.method, hook.class).unwrap();
            writeln!(&mut self.lambda_ir, "  %ok_{index} = call i64 @tinox_service_check_error(i8* {label})\n  %success_{index} = icmp ne i64 %ok_{index}, 0\n  br i1 %success_{index}, label %next_{index}, label %failed\nnext_{index}:").unwrap();
        }
        writeln!(&mut self.lambda_ir, "  %stopping_after_hooks = call i1 @serviceIsStopping()\n  %start_result = select i1 %stopping_after_hooks, i64 0, i64 1\n  ret i64 %start_result\nfailed:\n  ret i64 0\n}}").unwrap();
        writeln!(
            &mut self.lambda_ir,
            "define void @__tinox_service_stop_hooks() {{\nentry.tnx:"
        )
        .unwrap();
        for (index, hook) in hooks
            .iter()
            .rev()
            .filter(|h| h.kind == "OnStop")
            .enumerate()
        {
            let owner = &hook.class;
            let label = self.emit_lambda_string_literal(&format!(
                "OnStop {}.{} failed",
                hook.class, hook.method
            ));
            writeln!(&mut self.lambda_ir, "  %initialized_{index} = load i1, i1* @__service_{owner}_initialized\n  br i1 %initialized_{index}, label %stop_{index}, label %next_{index}\nstop_{index}:\n  %owner_{index} = call i64* @{owner}_di_get()\n  call void @tinox_service_hook_begin(i8* {label})\n  call void @{}_{}(i64* %owner_{index})\n  call void @tinox_service_hook_end()", hook.class, hook.method).unwrap();
            writeln!(&mut self.lambda_ir, "  call void @tinox_service_cleanup_error(i8* {label})\n  br label %next_{index}\nnext_{index}:").unwrap();
        }
        writeln!(&mut self.lambda_ir, "  ret void\n}}").unwrap();
        for (kind, name) in [("Readiness", "ready"), ("Liveness", "live")] {
            writeln!(
                &mut self.lambda_ir,
                "define i64 @__tinox_service_{name}_checks() {{\nentry.tnx:"
            )
            .unwrap();
            for (index, hook) in hooks.iter().filter(|h| h.kind == kind).enumerate() {
                writeln!(&mut self.lambda_ir, "  %owner_{index} = call i64* @{}_di_get()\n  %check_{index} = call i1 @{}_{}(i64* %owner_{index})\n  %error_{index} = load i64, i64* @__tinox_err\n  %no_error_{index} = icmp eq i64 %error_{index}, 0\n  %ok_{index} = and i1 %check_{index}, %no_error_{index}\n  br i1 %ok_{index}, label %next_{index}, label %unhealthy\nnext_{index}:", hook.class, hook.class, hook.method).unwrap();
            }
            writeln!(
                &mut self.lambda_ir,
                "  ret i64 1\nunhealthy:\n  ret i64 0\n}}"
            )
            .unwrap();
        }
        // These symbols have the closure signature HTTP/3 route registration uses.
        for (name, ready) in [("live", 0), ("ready", 1)] {
            writeln!(&mut self.lambda_ir, "define void @__tinox_service_{name}_route(i64 %ctx, i64* %env) {{\nentry.tnx:\n  call void @tinox_service_health_response(i64 %ctx, i64 {ready})\n  ret void\n}}").unwrap();
            writeln!(
                &mut self.ir,
                "@__service_{name}_path = private constant [{} x i8] c\"/health/{name}\\00\"",
                name.len() + 9
            )
            .unwrap();
        }
    }
    pub(super) fn emit_managed_route_wrapper(&mut self, name: &str, closure: bool) {
        let request = self.has_request_scope();
        if !self.service_enabled() && !request { return; }
        let extra = if closure { ", i64* %env" } else { "" };
        writeln!(&mut self.lambda_ir, "define void @{name}(i64 %ctx_i64{extra}) {{\nentry.tnx:").unwrap();
        if self.service_enabled() {
            writeln!(&mut self.lambda_ir, "  %admitted = call i64 @tinox_service_work_enter()\n  %ok = icmp ne i64 %admitted, 0\n  br i1 %ok, label %run, label %unavailable\nrun:").unwrap();
        }
        if request { writeln!(&mut self.lambda_ir, "  call void @tinox_request_begin()").unwrap(); }
        writeln!(&mut self.lambda_ir, "  call void @{name}__body(i64 %ctx_i64{extra})").unwrap();
        // This outer boundary survives every early response and propagated
        // exception/cancellation from the body, including DTO validation.
        if request { writeln!(&mut self.lambda_ir, "  call void @tinox_request_end()").unwrap(); }
        if self.service_enabled() { writeln!(&mut self.lambda_ir, "  call void @tinox_service_request_done(i64 %ctx_i64)").unwrap(); }
        writeln!(&mut self.lambda_ir, "  ret void").unwrap();
        if self.service_enabled() { writeln!(&mut self.lambda_ir, "unavailable:\n  call void @tinox_service_unavailable(i64 %ctx_i64)\n  ret void").unwrap(); }
        writeln!(&mut self.lambda_ir, "}}\n").unwrap();
    }
    pub(super) fn emit_service_bootstrap_start(&mut self, expected: usize) {
        if !self.service_enabled() {
            return;
        }
        let options = self.service_options.clone().unwrap();
        writeln!(
            &mut self.lambda_ir,
            "  call void @tinox_service_begin(i64 {expected}, i64 {}, i64 {})",
            options.startup_ms, options.shutdown_ms
        )
        .unwrap();
        for (index, initializer) in self.bootstrap_plan().initializers.iter().enumerate() {
            let label = self
                .emit_lambda_string_literal(&format!("Initializer {} failed", initializer.name));
            writeln!(&mut self.lambda_ir, "  call void @{}()\n  %initializer_ok_{index} = call i64 @tinox_service_check_error(i8* {label})", initializer.function).unwrap();
            writeln!(&mut self.lambda_ir, "  %init_failed_{index} = call i64 @tinox_service_failed()\n  %init_ok_{index} = icmp eq i64 %init_failed_{index}, 0\n  br i1 %init_ok_{index}, label %init_next_{index}, label %service_start_failed\ninit_next_{index}:").unwrap();
        }
        writeln!(&mut self.lambda_ir, "  %hooks_ok = call i64 @__tinox_service_start_hooks()\n  %started = icmp ne i64 %hooks_ok, 0\n  br i1 %started, label %service_started, label %service_start_failed\nservice_start_failed:\n  %failure = call i64 @tinox_service_finish(i64 0)\n  %failure32 = trunc i64 %failure to i32\n  ret i32 %failure32\nservice_started:\n  call void @tinox_service_initialized()").unwrap();
    }
}
