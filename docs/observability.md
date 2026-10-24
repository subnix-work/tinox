# Request correlation, logging and tracing

This first increment of #305 builds on `@Log`, `@Timed` and `@Counted`.
It uses [W3C Trace Context](https://www.w3.org/TR/trace-context/) for HTTP
propagation and [OTLP/HTTP JSON](https://opentelemetry.io/docs/specs/otlp/)
for span export. It adds no proprietary tracing wire format and publishes
no extended package.

## Configuration

Read once at process startup, before application threads run. Environment
variables override `application.properties`. Invalid booleans, sampling
rates, service names and endpoints stop startup with an English diagnostic
that names the setting without printing its value.

| Environment variable | Property | Default |
| --- | --- | --- |
| `TINOX_TELEMETRY_ENABLED` | `tinox.telemetry.enabled` | `false` |
| `TINOX_TELEMETRY_SAMPLE` | `tinox.telemetry.sample` | `10000` |
| `TINOX_TELEMETRY_SERVICE` | `tinox.telemetry.service` | `tinox` |
| `TINOX_TELEMETRY_ENDPOINT` | `tinox.telemetry.endpoint` | empty (no exporter) |

Enabled accepts `true` or `false`. Sampling is an integer in 0..10000,
representing the fraction of new root traces sampled in ten-thousandths.
Children inherit their parent's sampled decision, including remote parents.
An unsampled trace still propagates correlation and records metrics.
Service names are 1..64 ASCII letters, digits, dots, underscores or hyphens.

An endpoint must have the form `http://127.0.0.1:4318/v1/traces` (numeric
IPv4, explicit port, fixed path). Use a local OpenTelemetry collector to
forward over TLS, authenticate, retry or store spans durably. The runtime
exporter intentionally supports this bounded local-agent transport only.
Enabling telemetry without an endpoint still enables correlation, logs
and metrics.

## Automatic behavior

* The runtime route dispatcher used by `@GET`/`@POST` and other HTTP/1.1
  route annotations creates a server span and request ID, emits JSON
  `request.start` and `request.end` records, and restores the previous
  context after the handler. Unmatched routes are included. Response
  `X-Request-Id` carries the correlation ID.
* `spawn` captures the current context by value. The spawned operation
  gets its own child span, records queue delay and execution duration,
  and passes context through further spawned work. Parent and sibling
  contexts remain independent. Joinable and detached tasks share this path.
* `Http::get/post/put/delete/patch` and their corresponding runtime
  builtins create client spans and inject `traceparent` and `x-request-id`.
  Client spans close on both transport success and failure. Active tracing
  owns these headers; manual setters cannot add duplicates or stale
  `tracestate`. Both plain HTTP and HTTPS use this client path.
* `@Log` loggers automatically select JSON when telemetry is enabled.
  Existing `TINOX_LOG_JSON=true` remains available with telemetry disabled.
  JSON records carry `timestamp`, `level`, `logger`, `message`, `request_id`,
  `trace_id` and `span_id`. Correlation fields are empty outside a context.
  Each structured record uses one stdio operation to prevent line interleaving.
* Process start and orderly stop emit `service.start` / `service.stop`
  events and increment service lifecycle counters.

Incoming `traceparent` validates lowercase hexadecimal, separators, version,
length and nonzero trace/span IDs. Version 00 has exactly 55 bytes; future
versions with the known prefix are accepted and emitted as version 00.
Invalid or duplicate parents start a new trace. Request IDs must be 1..64
ASCII token bytes, using the same character set as service names. Invalid,
oversized, missing or duplicate request IDs are replaced with the trace ID.
Provider-specific `tracestate` and baggage are discarded; neither is required
to preserve the distributed trace identity.

No URL, query, body, authorization header, exception text, or application
object is captured automatically. Existing `@Sensitive` and `@Masked`
conversions remain in place before the logger receives its message. Literal
secrets passed directly as message strings remain the application's
responsibility. JSON escaping covers every control byte; message text is
bounded to 4096 bytes and logger names to 128 bytes, without splitting UTF-8
characters at the truncation boundary. These limits also apply when only
`TINOX_LOG_JSON` is enabled. Plain text logging is unchanged.

Automatic server coverage currently targets the runtime HTTP/1.1 route
server. Manual Tinox server loops, HTTP/2, HTTP/3, WebSocket message handlers
and broker consumers can use the explicit API; automatic adapters for those
boundaries are future increments. Log output to stdout retains the operating
system's normal backpressure behavior.

## Explicit API

Import `tinox.core.logger`. These functions are also available for message
boundaries and custom operations:

```tinox
const token = telemetryBegin("message.process", receivedTraceparent, receivedRequestId)
// Application work; spawn and outgoing HTTP inherit this context.
const outgoingParent = telemetryTraceparent()
const requestId = telemetryRequestId()
telemetryEnd(token)
```

Names use the bounded ASCII token character set, with `operation` as a
fallback. Empty parent continues the active context, or creates a new root
when none is active. Empty request ID inherits the active request ID for
local work. End tokens in LIFO order, including error paths. `telemetryEnd`
returns 0 for an out-of-order token without modifying context, otherwise 1.
There are at most 32 nested scopes; a rejected begin returns 0 and increments
`tinox_telemetry_scope_rejected_total`. Ending token 0 is harmless.
Disabled mode returns token 0 and creates no IDs, clocks or exporter thread.

`telemetryFlush()` is an optional best-effort drain with a 250ms maximum
wait. It returns 1 if the queue is empty and no send is in flight, otherwise
0. It does not guarantee successful delivery. Orderly process exit attempts
the same bounded drain; abnormal exit can lose pending spans.

## Resource bounds and metrics

The exporter has one worker and a fixed queue of 128 value-only spans.
Producers hold a short mutex only to copy a span; network I/O happens outside
that mutex. A full queue drops the newest span and increments
`tinox_telemetry_dropped_total`. Each export has a single 200ms absolute
connect/write/status deadline, uses no DNS, and never retries. Failed
connections, timeouts and non-200 HTTP responses increment
`tinox_telemetry_export_failed_total`. Successful HTTP acknowledgements
increment `tinox_telemetry_exported_total`; collector partial-success bodies
are not interpreted. No exporter traffic creates additional spans.

Existing Prometheus metrics infrastructure records
`tinox_http_server_duration_ns`, `tinox_http_client_duration_ns`,
`tinox_operation_duration_ns`, and `tinox_task_queue_duration_ns` histograms.
All automatic metric names are fixed: request IDs, trace IDs, URLs, operation
names, errors and service names never become metric labels. `@Timed` and
`@Counted` keep their existing behavior and metric names.

## Example and verification

See [the annotated example](../examples/observability/src/Probe.tnx) and
[its configuration](../examples/observability/application.properties).
Start an independent dependency on port 8081, then run `tinox install` and
`tinox run` from `examples/observability`. Call `/probe` on port 8080 with a
W3C `traceparent` and `x-request-id` header. Handler and child logs share the
request/trace IDs and have distinct span IDs; outbound HTTP carries the
client span ID.

Run `cargo test --release -p tinox --test observability -- --nocapture`, or
`python3 scripts/check-observability.py`. The test uses independent Python
HTTP clients, a dependency service and an OTLP JSON receiver, checks 23
request/task/outbound-call chains on a reused worker, validates span
relationships and JSON escaping, and exercises invalid metadata, sampling,
queue saturation, a stalled exporter (15 repetitions) and connection refusal. It prints
baseline and instrumented nanoseconds per operation with telemetry disabled
and enabled. The full gate includes a Tinox fixture for redaction and the
explicit API. Numeric timing results must be collected on the target system;
the implementation environment has no C/Rust compiler or make, so no timing
results or green gate are claimed here. A real OpenTelemetry Collector has
not been exercised here either.
