# Bootstrap explanation

`tinox explain --bootstrap` reports the effective generated service wiring without
running the application. It uses the same resolved sources, annotation validation
and in-memory code generation as `tinox build`, stopping before LLVM verification,
native tools, linking or executable publication. Dependencies must already be
installed; inspection never installs packages or connects to services. Normal
compiler frontend caches may be populated.

```sh
tinox explain --bootstrap                         # Project entry from tinox.toml
tinox explain --bootstrap src/Main.tnx
tinox explain --bootstrap src/Main.tnx --json
tinox explain --bootstrap --debug                  # Inspect the debug configuration
```

The default is the release configuration, matching `tinox build`. Project settings
and compiler environment variables have the same meaning as for builds. In
particular, `TINOX_PORT` is resolved by the compiler for the plain REST listener and
WebSocket endpoints without an explicit annotation port. It is not a runtime
configuration lookup. UI ports come from annotation processing; HTTP/3 ports come
from `@Http3RestController`.

The text output lists the entry point, eager initializers, background tasks,
listeners or broker destinations, routes, component scopes, injection providers,
configuration bindings, and spawn/call/join ordering. The JSON success envelope is:

```json
{
  "schema_version": 1,
  "kind": "bootstrap_explanation",
  "success": true,
  "plan": {
    "mode": "bootstrap",
    "entry_point": "Main::main",
    "banner": false,
    "initializers": [],
    "background_tasks": [],
    "components": [],
    "routes": [],
    "configuration": [],
    "steps": [{"action": "call", "target": "Main::main", "ordering": "after issuing all background spawns"}],
    "notes": []
  },
  "call_graph": ["tinox", "graph", "src/Main.tnx"]
}
```

The example omits the explanatory notes that real output always includes. The
`call_graph` array is an argument vector for the existing call-graph tool, not a
shell command to concatenate. Inspection does not create its diagram.

Each background task includes its generated function, owner, instance lifetime,
concurrency model, callbacks and endpoint. Endpoints report protocol, direction,
configured port, configuration source, path or destination where applicable.
`port.bound` stays `null`: actual binding, ephemeral port selection, connection
success and readiness are runtime facts. `port.phase` identifies compile-time
resolution of the configured number. Broker usernames/passwords are never copied
into this model. URI-shaped host/destination labels containing userinfo, query
strings or fragments are redacted conservatively. Database URLs, configuration
values, field defaults, certificates and key paths are not printed. Configuration
keys and ordinary endpoint paths/destinations remain visible.

Component scopes are `application_singleton`, `startup_singleton`, or
`request_factory`. Startup providers run eagerly before the entry point;
application providers are lazy, and request providers allocate per factory call.
Dependencies resolve recursively before the owner is published. Missing component
providers and cyclic injection graphs fail through the common validation path in
`check`, `build`, and `explain` (see #330).

The output describes current behavior, including its limits:

- Database initialization has constructor priority 10; eager DI startup has
  priority 65535. Both share one LLVM constructor table (#331).
- Background tasks spawn in registration order, followed by `Main::main`, then
  joins in registration order. Actual thread execution is concurrent. Spawning
  does not establish readiness before Main, and joins can wait indefinitely.
- REST handlers use the reported controller allocation policy. Singleton REST
  controllers refresh request-scoped injection fields during dispatch.
- WebSocket/UI instances are allocated per connection; AMQP handler instances
  are allocated per consumer connection. These paths allocate directly rather
  than obtaining an instance from a declared DI scope.
- `@Config` bindings remain unresolved and redacted. The generated struct-literal
  construction path reads them; DI and endpoint allocation do not automatically
  perform that lookup.
- Singleton getters do not add synchronization. Shared mutable state still
  requires explicit coordination (#303). No coordinated cancellation/shutdown
  contract is implied; lifecycle details can extend this model with #304.

Errors exit nonzero. With `--json`, errors use the versioned
[compiler diagnostic envelope](compiler-diagnostics.md), with `success: false` and
`diagnostics`. Text errors omit source-line excerpts to avoid echoing credentials
from an annotation sharing the failing line. Success output contains selected
public metadata, never a generated LLVM dump.
