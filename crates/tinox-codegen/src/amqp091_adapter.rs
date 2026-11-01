//! AMQP 0-9-1 annotation adapter. This module owns only the generated receive
//! loop; the frontend owns annotation validation, SemanticFacts owns source
//! identity/types, and CodeGen owns physical layouts and bootstrap registration.
use super::bootstrap::{BackgroundTask, Endpoint};
use super::{Amqp091ConsumerEntry, CodeGen};
use std::fmt::Write;
use tinox_typecheck::ValueType;

/// Prepared emission input, never inferred from generated LLVM text.
struct Amqp091Input {
    instance_bytes: i64,
    run_symbol: String,
    service_enabled: bool,
}
impl Amqp091Input {
    fn prepare(codegen: &CodeGen, consumer: &Amqp091ConsumerEntry, index: usize) -> Self {
        // Annotation processing has checked the callback signature. Validate
        // that its checked declaration identity survived the lowering boundary.
        // Synthetic, unchecked CodeGen clients retain their existing behavior.
        if codegen.checked_semantics {
            if let Some(callback) = &consumer.on_message {
                let symbol = format!("{}_{}", consumer.class_name, callback);
                assert!(
                    codegen.semantics.declarations.contains_key(&symbol),
                    "AMQP091 adapter invariant: missing checked callback {symbol}"
                );
            }
            for (owner, field, slot, expected) in [
                ("AmqpConnection091", "conn", 0, ValueType::Int),
                ("AmqpChannel091", "chanId", 1, ValueType::Int),
                ("AmqpMessage091", "deliveryTag", 0, ValueType::Int),
                ("AmqpMessage091", "ok", 5, ValueType::Bool),
            ] {
                let actual = codegen
                    .semantics
                    .field_types
                    .get(owner)
                    .and_then(|fields| fields.get(field));
                assert_eq!(
                    actual,
                    Some(&expected),
                    "AMQP091 adapter invariant: checked field {owner}.{field}"
                );
                assert_eq!(
                    codegen
                        .struct_layouts
                        .get(owner)
                        .and_then(|fields| fields.get(slot))
                        .map(String::as_str),
                    Some(field),
                    "AMQP091 adapter invariant: layout slot {owner}.{field}"
                );
            }
        }
        Self {
            instance_bytes: codegen
                .struct_layouts
                .get(&consumer.class_name)
                .map(|fields| (fields.len().max(1) * 8) as i64)
                .unwrap_or(8),
            run_symbol: format!("__tinox_run_amqp091_{index}"),
            service_enabled: codegen.service_enabled(),
        }
    }
}

impl CodeGen {
    /// Generates an auto-run loop for each `@Amqp091Consumer`-annotated
    /// class (Issue #126): connect/open/qos/consume/nextMessage/ack loop,
    /// calling the class's `@OnMessage` method directly by its mangled
    /// `{Class}_{method}` symbol — same hand-emitted-IR technique as
    /// `emit_amqp10_consumer_code`, calling the already-compiled
    /// `AmqpConnection091`/`AmqpChannel091` static/instance methods from
    /// `tinox.core.amqp091` (which the file must import). The handler
    /// receives the whole `AmqpMessage091` pointer (not a decoded body) so
    /// no string-building loop needs to be hand-rolled in raw IR. Prefetch
    /// is hardcoded to 1 (mirrors amqp10's per-message `grantCredit(1)`) —
    /// no annotation argument for it in v1.
    ///
    /// Fires once per `@Amqp091Consumer` class found (no upper limit — each
    /// gets its own uniquely-named `__tinox_run_amqp091_<idx>()`, spawned on
    /// its own thread by `emit_tinox_main_bootstrap`; multiple consumers
    /// against the same broker/port but different queues is a normal,
    /// expected shape, unlike WS where the same port would collide).
    pub(super) fn emit_amqp091_consumer_code(&mut self) {
        if self.amqp091_consumers.is_empty() || self.has_main {
            return;
        }

        if !self.class_named_types.contains("AmqpMessage091") {
            panic!("@Amqp091Consumer requires `import tinox.core.amqp091;` (AmqpMessage091 type not found)");
        }

        let consumers = self.amqp091_consumers.clone();
        for (idx, c) in consumers.iter().enumerate() {
            let input = Amqp091Input::prepare(self, c, idx);
            let inst_size = input.instance_bytes;

            // The connect/receive loop lives in __tinox_run_amqp091_<idx>(),
            // a plain callee (not @tinox_main directly), so
            // emit_tinox_main_bootstrap can run it on its own thread instead
            // of tail-calling it inline.
            let run_fn = input.run_symbol;
            writeln!(&mut self.lambda_ir, "define i64 @{run_fn}() {{").unwrap();
            writeln!(&mut self.lambda_ir, "entry.tnx:").unwrap();

            let host_ptr = self.emit_lambda_string_literal(&c.host);
            let vhost_ptr = self.emit_lambda_string_literal(&c.vhost);
            let user_ptr = self.emit_lambda_string_literal(&c.user);
            let pass_ptr = self.emit_lambda_string_literal(&c.pass);
            let queue_ptr = self.emit_lambda_string_literal(&c.queue);

            writeln!(&mut self.lambda_ir, "  %conn_obj = call i64* @AmqpConnection091_connect(i64* null, i8* {host_ptr}, i64 {port}, i8* {vhost_ptr}, i8* {user_ptr}, i8* {pass_ptr})", port = c.port).unwrap();
            writeln!(&mut self.lambda_ir, "  %conn_field = getelementptr %class.AmqpConnection091, ptr %conn_obj, i32 0, i32 0").unwrap();
            writeln!(
                &mut self.lambda_ir,
                "  %conn_val = load i64, i64* %conn_field"
            )
            .unwrap();
            writeln!(
                &mut self.lambda_ir,
                "  %conn_bad = icmp sle i64 %conn_val, 0"
            )
            .unwrap();
            writeln!(
                &mut self.lambda_ir,
                "  br i1 %conn_bad, label %connect_fail, label %do_open"
            )
            .unwrap();

            writeln!(&mut self.lambda_ir, "connect_fail:").unwrap();
            writeln!(&mut self.lambda_ir, "  ret i64 1").unwrap();

            writeln!(&mut self.lambda_ir, "do_open:").unwrap();
            writeln!(
                &mut self.lambda_ir,
                "  %chan_obj = call i64* @AmqpChannel091_open(i64* null, i64* %conn_obj)"
            )
            .unwrap();
            writeln!(&mut self.lambda_ir, "  %chanid_field = getelementptr %class.AmqpChannel091, ptr %chan_obj, i32 0, i32 1").unwrap();
            writeln!(
                &mut self.lambda_ir,
                "  %chanid_val = load i64, i64* %chanid_field"
            )
            .unwrap();
            writeln!(
                &mut self.lambda_ir,
                "  %chan_bad = icmp eq i64 %chanid_val, 0"
            )
            .unwrap();
            writeln!(
                &mut self.lambda_ir,
                "  br i1 %chan_bad, label %open_fail, label %do_qos"
            )
            .unwrap();

            writeln!(&mut self.lambda_ir, "open_fail:").unwrap();
            if input.service_enabled {
                writeln!(
                    &mut self.lambda_ir,
                    "  call void @tinox_service_close_connection(i64 %conn_val)"
                )
                .unwrap();
            }
            writeln!(&mut self.lambda_ir, "  ret i64 2").unwrap();

            writeln!(&mut self.lambda_ir, "do_qos:").unwrap();
            writeln!(
                &mut self.lambda_ir,
                "  %qos_ok = call i1 @AmqpChannel091_qos(i64* %chan_obj, i64 1)"
            )
            .unwrap();
            writeln!(
                &mut self.lambda_ir,
                "  br i1 %qos_ok, label %do_consume, label %qos_fail"
            )
            .unwrap();

            writeln!(&mut self.lambda_ir, "qos_fail:").unwrap();
            if input.service_enabled {
                writeln!(
                    &mut self.lambda_ir,
                    "  call void @tinox_service_close_connection(i64 %conn_val)"
                )
                .unwrap();
            }
            writeln!(&mut self.lambda_ir, "  ret i64 3").unwrap();

            writeln!(&mut self.lambda_ir, "do_consume:").unwrap();
            writeln!(
                &mut self.lambda_ir,
                "  %tag_ptr = call i8* @AmqpChannel091_consume(i64* %chan_obj, i8* {queue_ptr})"
            )
            .unwrap();
            writeln!(
                &mut self.lambda_ir,
                "  %tag_len = call i64 @tinox_string_length(i8* %tag_ptr)"
            )
            .unwrap();
            writeln!(&mut self.lambda_ir, "  %tag_bad = icmp eq i64 %tag_len, 0").unwrap();
            writeln!(
                &mut self.lambda_ir,
                "  br i1 %tag_bad, label %consume_fail, label %consumer_ready"
            )
            .unwrap();

            writeln!(&mut self.lambda_ir, "consume_fail:").unwrap();
            if input.service_enabled {
                writeln!(
                    &mut self.lambda_ir,
                    "  call void @tinox_service_close_connection(i64 %conn_val)"
                )
                .unwrap();
            }
            writeln!(&mut self.lambda_ir, "  ret i64 4").unwrap();

            writeln!(&mut self.lambda_ir, "consumer_ready:").unwrap();
            writeln!(
                &mut self.lambda_ir,
                "  %raw = call i8* @tinox_alloc(i64 {inst_size})"
            )
            .unwrap();
            writeln!(&mut self.lambda_ir, "  %inst = bitcast i8* %raw to i64*").unwrap();
            self.emit_field_defaults(&c.class_name, "%inst");
            writeln!(&mut self.lambda_ir, "  br label %recv_loop").unwrap();

            writeln!(&mut self.lambda_ir, "recv_loop:").unwrap();
            if input.service_enabled {
                writeln!(&mut self.lambda_ir, "  %stopping = call i1 @serviceIsStopping()\n  br i1 %stopping, label %service_close, label %recv_next\nrecv_next:").unwrap();
            }
            if input.service_enabled {
                writeln!(
                    &mut self.lambda_ir,
                    "  call void @tinox_service_listener_ready(i64 -1)"
                )
                .unwrap();
            }
            writeln!(
                &mut self.lambda_ir,
                "  %msg_obj = call i64* @AmqpChannel091_nextMessage(i64* %chan_obj)"
            )
            .unwrap();
            writeln!(
                &mut self.lambda_ir,
                "  %ok_field = getelementptr %class.AmqpMessage091, ptr %msg_obj, i32 0, i32 5"
            )
            .unwrap();
            writeln!(&mut self.lambda_ir, "  %ok_val = load i64, i64* %ok_field").unwrap();
            writeln!(&mut self.lambda_ir, "  %is_ok = icmp ne i64 %ok_val, 0").unwrap();
            writeln!(
                &mut self.lambda_ir,
                "  br i1 %is_ok, label %handle_msg, label %recv_loop"
            )
            .unwrap();

            writeln!(&mut self.lambda_ir, "handle_msg:").unwrap();
            if input.service_enabled {
                writeln!(&mut self.lambda_ir, "  %message_admitted = call i64 @tinox_service_work_enter()\n  %message_ok = icmp ne i64 %message_admitted, 0\n  br i1 %message_ok, label %message_next, label %service_close\nmessage_next:").unwrap();
            }
            // Failure policy (issue #537): the delivery is acknowledged only when the handler
            // returned normally. A failed attempt is retried with backoff up to maxAttempts, then
            // dead-lettered or rejected by MessageFailure091.onFailure -- never acknowledged as handled.
            let (max_attempts, backoff_ms, dead_letter) = c.failure.clone().unwrap_or((1, 0, String::new()));
            let handler_label = self.emit_lambda_string_literal(&format!("{}.{}", c.class_name, c.on_message.clone().unwrap_or_default()));
            let dlq_ptr = self.emit_lambda_string_literal(&dead_letter);
            writeln!(&mut self.lambda_ir, "  %attempt_slot = alloca i64\n  store i64 1, i64* %attempt_slot\n  br label %attempt_loop\nattempt_loop:").unwrap();
            if let Some(ref on_message) = c.on_message {
                let span_name = self.emit_lambda_string_literal("amqp091.consume");
                writeln!(&mut self.lambda_ir, "  %msg_span = call i64 @tinox_telemetry_message_begin(i8* {span_name})").unwrap();
                writeln!(
                    &mut self.lambda_ir,
                    "  call void @{}_{}(i64* %inst, i64* %msg_obj)",
                    c.class_name, on_message
                )
                .unwrap();
                writeln!(&mut self.lambda_ir, "  call void @tinox_telemetry_message_end(i64 %msg_span)").unwrap();
            }
            writeln!(&mut self.lambda_ir, "  %handler_err = load i64, i64* @__tinox_err").unwrap();
            writeln!(&mut self.lambda_ir, "  %handler_failed = call i64 @tinox_message_handler_failed(i8* {handler_label}, i64 %handler_err)").unwrap();
            writeln!(&mut self.lambda_ir, "  %did_fail = icmp ne i64 %handler_failed, 0").unwrap();
            writeln!(&mut self.lambda_ir, "  br i1 %did_fail, label %on_fail, label %ack_message").unwrap();
            writeln!(&mut self.lambda_ir, "on_fail:").unwrap();
            if input.service_enabled {
                // Shutting down: leave the delivery unacknowledged so the broker redelivers it.
                writeln!(&mut self.lambda_ir, "  %fail_stopping = call i1 @serviceIsStopping()\n  br i1 %fail_stopping, label %fail_abandon, label %fail_settle\nfail_abandon:\n  call void @tinox_service_work_exit()\n  br label %service_close\nfail_settle:").unwrap();
            }
            writeln!(&mut self.lambda_ir, "  %attempt = load i64, i64* %attempt_slot").unwrap();
            writeln!(&mut self.lambda_ir, "  %settled = call i64 @MessageFailure091_onFailure(i64* %chan_obj, i64* %msg_obj, i64 %attempt, i64 {max_attempts}, i64 {backoff_ms}, i8* {dlq_ptr})").unwrap();
            writeln!(&mut self.lambda_ir, "  %retry = icmp eq i64 %settled, 0\n  br i1 %retry, label %retry_attempt, label %settled_done").unwrap();
            writeln!(&mut self.lambda_ir, "retry_attempt:\n  %next_attempt = add i64 %attempt, 1\n  store i64 %next_attempt, i64* %attempt_slot\n  br label %attempt_loop").unwrap();
            writeln!(&mut self.lambda_ir, "settled_done:").unwrap();
            if input.service_enabled {
                writeln!(&mut self.lambda_ir, "  call void @tinox_service_work_exit()").unwrap();
            }
            writeln!(&mut self.lambda_ir, "  br label %recv_loop").unwrap();
            writeln!(&mut self.lambda_ir, "ack_message:").unwrap();
            writeln!(
                &mut self.lambda_ir,
                "  %tag_field = getelementptr %class.AmqpMessage091, ptr %msg_obj, i32 0, i32 0"
            )
            .unwrap();
            writeln!(
                &mut self.lambda_ir,
                "  %tag_val = load i64, i64* %tag_field"
            )
            .unwrap();
            writeln!(
                &mut self.lambda_ir,
                "  call void @AmqpChannel091_ack(i64* %chan_obj, i64 %tag_val)"
            )
            .unwrap();
            if input.service_enabled {
                writeln!(
                    &mut self.lambda_ir,
                    "  call void @tinox_service_work_exit()"
                )
                .unwrap();
            }
            writeln!(&mut self.lambda_ir, "  br label %recv_loop").unwrap();

            if input.service_enabled {
                writeln!(&mut self.lambda_ir, "service_close:\n  call void @tinox_service_cleanup_enter()\n  call void @AmqpConnection091_close(i64* %conn_obj)\n  call void @tinox_service_close_connection(i64 %conn_val)\n  ret i64 0").unwrap();
            }

            writeln!(&mut self.lambda_ir, "}}").unwrap();
            writeln!(&mut self.lambda_ir).unwrap();

            self.register_background_task(BackgroundTask::new(
                "amqp091",
                Some(&c.class_name),
                &run_fn,
                "per_consumer_connection",
                "sequential receive/handler/ack loop",
                Endpoint::connect("AMQP 0-9-1", c.port, &c.host, &c.queue),
            ));
            self.startup_endpoints.push((
                "AMQP 0-9-1 (consumer)".to_string(),
                format!("{}:{} (queue: {})", c.host, c.port, c.queue),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tinox_lexer::Lexer;
    use tinox_parser::Parser;

    fn consumer() -> Amqp091ConsumerEntry {
        Amqp091ConsumerEntry {
            class_name: "Consumer".into(),
            host: "host".into(),
            port: 5672,
            vhost: "/".into(),
            user: "user".into(),
            pass: "secret".into(),
            queue: "queue".into(),
            on_message: Some("receive".into()),
            failure: None,
        }
    }
    fn checked_generator() -> CodeGen {
        let source = r#"
class AmqpConnection091 { var conn: Int64 }
class AmqpChannel091 { var conn: Int64
 var chanId: Int64 }
class AmqpMessage091 {
    var deliveryTag: Int64
 var exchange: String
 var routingKey: String
    var contentType: String
 var body: List<Int64>
 var ok: Bool
}
class Consumer { var marker: Int64 = 42
 fn receive(message: AmqpMessage091) -> Nothing {} }
"#;
        let ast = Parser::new(Lexer::new(source).tokenize().unwrap())
            .parse()
            .unwrap();
        let checked = tinox_typecheck::cache::check_program_cached(&ast, None).unwrap();
        let mut codegen = CodeGen::new();
        codegen.set_semantic_facts(checked.semantics);
        codegen.set_expr_value_types(checked.expr_types);
        codegen.gen(&ast).unwrap();
        codegen
    }
    #[test]
    fn prepared_input_uses_checked_identity_and_physical_layout() {
        let codegen = checked_generator();
        let input = Amqp091Input::prepare(&codegen, &consumer(), 3);
        assert_eq!(input.instance_bytes, 8);
        assert_eq!(input.run_symbol, "__tinox_run_amqp091_3");
        assert!(!input.service_enabled);
    }
    #[test]
    #[should_panic(expected = "AMQP091 adapter invariant: layout slot")]
    fn layout_drift_is_rejected_before_ir_emission() {
        let mut codegen = checked_generator();
        codegen
            .struct_layouts
            .get_mut("AmqpMessage091")
            .unwrap()
            .swap(0, 5);
        Amqp091Input::prepare(&codegen, &consumer(), 0);
    }
    #[test]
    #[should_panic(expected = "AMQP091 adapter invariant: checked field")]
    fn semantic_type_drift_is_rejected_before_ir_emission() {
        let mut codegen = checked_generator();
        codegen
            .semantics
            .field_types
            .get_mut("AmqpMessage091")
            .unwrap()
            .insert("ok".into(), ValueType::Int);
        Amqp091Input::prepare(&codegen, &consumer(), 0);
    }
    #[test]
    #[should_panic(expected = "missing checked callback")]
    fn missing_callback_identity_is_rejected_before_ir_emission() {
        let mut codegen = checked_generator();
        codegen.semantics.declarations.remove("Consumer_receive");
        Amqp091Input::prepare(&codegen, &consumer(), 0);
    }
    #[test]
    fn service_adapter_balances_work_and_preserves_failure_cleanup() {
        let mut codegen = checked_generator();
        codegen.service_options = Some(super::super::lifecycle::ServiceOptions {
            startup_ms: 1000,
            shutdown_ms: 1000,
        });
        codegen.amqp091_consumers = vec![consumer(), consumer()];
        codegen.emit_amqp091_consumer_code();
        let ir = &codegen.lambda_ir;
        assert_eq!(ir.matches("define i64 @__tinox_run_amqp091_").count(), 2);
        assert_eq!(
            ir.matches("call void @tinox_service_close_connection")
                .count(),
            8
        );
        // acknowledged / settled / abandoned-on-shutdown paths, per consumer
        assert_eq!(
            ir.matches("call void @tinox_service_work_exit()").count(),
            6
        );
        assert_eq!(ir.matches("call void @AmqpConnection091_close").count(), 2);
        assert_eq!(codegen.background_tasks.len(), 2);
        assert!(
            ir.find("call void @Consumer_receive").unwrap()
                < ir.find("call void @AmqpChannel091_ack").unwrap()
        );
        // A failed handler goes through the failure policy and is never acknowledged on that path.
        assert_eq!(ir.matches("@MessageFailure091_onFailure").count(), 2);
        assert_eq!(ir.matches("call void @AmqpChannel091_ack").count(), 2);
        assert!(
            ir.contains("i64 42"),
            "consumer field defaults must still be emitted"
        );
    }
    #[test]
    fn explicit_entrypoint_still_suppresses_adapter_emission() {
        let mut codegen = CodeGen::new();
        codegen.has_main = true;
        codegen.amqp091_consumers = vec![consumer()];
        codegen.emit_amqp091_consumer_code();
        assert!(codegen.lambda_ir.is_empty());
        assert!(codegen.background_tasks.is_empty());
    }
}
