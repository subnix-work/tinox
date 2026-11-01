# Framework lowering adapters (#520)

The first extraction isolates **AMQP 0-9-1 consumer loops** in
`crates/tinox-codegen/src/amqp091_adapter.rs`. It deliberately preserves the
existing language API, annotation validation and generated IR. It introduces no
new type registry, runtime ABI, protocol behavior or scheduling policy.

## Boundary inventory

| Boundary | Current owner | Next extraction constraint |
| --- | --- | --- |
| AMQP 0-9-1 generated consumer loop | `amqp091_adapter.rs` | First extracted adapter; contract below. |
| AMQP 1.0 consumer loop | `CodeGen::emit_amqp10_consumer_code` | Keep session/link credit and acknowledgement ordering explicit. |
| WebSocket endpoints | `CodeGen::emit_ws_code` | Preserve callback ABI, connection ownership and service admission. |
| TCP and HTTP/3 routes | `emit_route_code`, `emit_http3_route_code` | Share checked route inputs; retain protocol-specific listener/response cleanup. |
| DI and configuration | `emit_di_code` | Preserve scope ownership, construction order and lifecycle hooks. |
| ORM | `emit_entity_code`, `gen_orm_query`, `gen_orm_save_delete`, `gen_orm_raw` | Retain checked entity metadata and acquire/release/transaction obligations. |
| CLI, JSON and UI | `emit_cli_code`, JSON emitters, `emit_tinoxui_code` | Separate registration metadata from serialization and dispatch emission. |
| Process bootstrap and service lifecycle | `bootstrap.rs`, `lifecycle.rs` | Existing shared boundaries; remain the authority for startup and shutdown. |

These are follow-on boundaries, not migrations performed by this change. A
shared trait or universal adapter registry is unnecessary for one extraction.

## AMQP 0-9-1 input and linkage contract

The frontend supplies validated `Amqp091ConsumerEntry` records. Codegen's normal
checked-program path supplies the existing `SemanticFacts`; the adapter checks
that the callback's declaration identity is present, and that the checked
protocol fields have the expected types. Physical layout remains CodeGen's
responsibility. The adapter verifies the four ABI slots it dereferences:

| Source field | Checked type | Physical word slot |
| --- | --- | --- |
| `AmqpConnection091.conn` | `Int64` | 0 |
| `AmqpChannel091.chanId` | `Int64` | 1 |
| `AmqpMessage091.deliveryTag` | `Int64` | 0 |
| `AmqpMessage091.ok` | `Bool` | 5 |

These checks detect an internal frontend/layout integration error before emitting
the loop. Synthetic unchecked CodeGen callers preserve their previous behavior.
No source type is inferred from LLVM strings. A private prepared input fixes the
instance allocation size, unique run symbol and service policy before emission.

Each consumer defines `i64 @__tinox_run_amqp091_<index>()` with internal generated
naming and registers precisely one background task and one startup endpoint.
The shared bootstrap owns thread spawning/joining and entry-point generation.
The adapter never creates a competing `tinox_main`.

The imported module must define `AmqpConnection091_connect`/`close` and
`AmqpChannel091_open`/`qos`/`consume`/`nextMessage`/`ack`, with the existing compiled
method ABI. The callback receives the complete message object. Allocation and
string length use shared runtime declarations; service helper declarations remain
owned by `lifecycle.rs`. The adapter adds definitions and calls, not duplicate
extern declarations. The normal undefined-callee and LLVM verification gates
continue to check final module linkage.

## Ownership and cleanup contract

Each generated consumer owns one connection, channel and initialized consumer
instance. Prefetch remains one, receive/handler/ack remain sequential. Connect,
open, QoS and consume failures retain return codes 1, 2, 3 and 4. With service
lifecycle enabled, post-connect setup failures close the registered connection.
Admitted callbacks leave service work accounting on both success and error;
acknowledgement occurs after the callback succeeds. Shutdown performs the
existing cleanup-enter, protocol close and connection-close sequence. Without
service lifecycle, all prior loop and failure behavior remains unchanged.

This refactor does not claim to add reconnect, nack/retry, concurrency, deadlines
or a new non-service cleanup policy. Those remain separate behavior changes.

## Maintenance and verification evidence

At baseline `ac96a66`, central `codegen.rs` contains 19,968 lines. The extraction
removes 163 lines and adds two module-registration lines: **161 fewer lines**
(about 0.81%) in the central emitter. Consumer-loop changes can now be reviewed in
one protocol-specific module with six independent contract tests, without paging
through routing, ORM or DI implementations. This is a measured locality benefit,
not a runtime performance claim; additional contract checks run at compile time.

Six unit tests cover prepared metadata, mutated semantic types/layout slots,
missing callback identity, lifecycle cleanup/accounting and entrypoint suppression.
The native composition test compiles four consumers plus Main in debug, release
and checked modes. Deterministic protocol stand-ins take all four setup failure
edges; each executable runs 20 times and must produce precisely the expected
output despite thread ordering. A negative test checks frontend diagnostics with
backend tools absent. These stand-ins test compiler composition, not AMQP wire
conformance; existing real-broker/protocol suites retain that responsibility.

Baseline comparison of the four generated consumer function bodies is
byte-for-byte equal. The original missing-import guard and frontend diagnostics
remain in place. Full project validation is performed on the integrated branch
before committing, alongside independently implemented framework changes.
