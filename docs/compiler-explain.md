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
`request_context`. Startup providers run eagerly before the entry point;
application providers are lazy. Request providers share one concrete instance
throughout each annotated REST dispatch, including repeated injections and
diamond-shaped dependency graphs. Concurrent requests have separate caches.
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
  controllers cannot retain request-scoped dependencies; captive lifetimes are
  compile errors, including non-injected nullable/container fields.
- Application/startup-scoped WebSocket endpoints use their declared DI provider;
  unscoped WebSocket and UI instances are allocated per connection. AMQP handler
  instances are allocated per consumer connection. HTTP request scope is rejected
  on WebSocket endpoints.
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

## Qualified compile-time providers

Scoped classes are concrete DI providers. `@Provider("name")` gives a
provider a unique, non-empty application-wide name; it does not introduce
provider methods, factories written by the application, or runtime lookup.
`@Inject @Qualifier("name")` selects that provider for a field. The field's
static type remains the declared interface or concrete class. An interface
provider must directly declare that interface in its `implements` list.

```tinox
// Store.tnx
interface Store { fn value() -> Int64 }

// FastStore.tnx
import Store
@ApplicationComponent @Provider("fast")
class FastStore implements Store {
    fn value() -> Int64 { return 1 }
}

// SafeStore.tnx
import Store
@ApplicationComponent @Provider("safe")
class SafeStore implements Store {
    fn value() -> Int64 { return 2 }
}

// Application.tnx
import Store
import FastStore
import SafeStore
@Startup
class Application {
    @Inject @Qualifier("safe") var store: Store
}
```

Without a qualifier, injection requires exactly one compatible scoped
provider. Existing concrete-class injection remains valid. An absent or
ambiguous provider, duplicate provider name, malformed qualifier, and any
cycle through selected providers are frontend errors in `check`, `build`
and `explain`; no backend or running application is required. Names are
case-sensitive. `@Qualifier` requires `@Inject` on the same field, and
`@Provider` requires an existing component scope annotation on a non-generic
class without inheritance. Its interfaces are declared directly with
`implements`; implicit inherited contracts are not provider candidates.
Generic injected types are rejected rather than silently skipped.

`explain --bootstrap --json` reports the declared field type in `target`,
the selected concrete class in `provider_class`, the qualifier in
`qualifier`, and the generated function and selected scope in `provider`
and `target_scope`. Its text form prints the same concrete generated
function. `describe @Provider --json` and `describe @Qualifier --json` expose
both annotation contracts through the compiler's annotation registry. Code generation and lifecycle dependency ordering
consume this same resolved provider graph.

Ownership follows the selected class's existing scope: application and
startup providers share one GC-managed instance; request providers follow
the request scope contract. DI constructs zero-initialized objects,
applies supported literal/empty-container field defaults, initializes the
interface vtable and injects selected dependencies before publishing the
singleton. It does not call a user constructor or an implicit initializer.
Consequently construction has no application-defined throwing factory;
allocation failure retains the runtime's hard failure behavior. The
singleton construction lock is released before returning the initialized
instance. With `@Service`, initialization work that can fail belongs in
`@OnStart`: its failure prevents dependent startup hooks and `Main.main`,
marks startup failed and runs managed cleanup. There is no successful
startup with a partially initialized provider. Recoverable arbitrary
provider factory methods and custom disposal ownership are outside this
initial class-provider model.


### Request context ownership (#522)

`@HttpRequestScoped` now means one instance per concrete provider per synchronous
annotated REST request, rather than allocation per factory call. TCP and HTTP/3
route wrappers establish the context before authentication, binding and dispatch,
and release its GC roots after the body returns. Early 400/401/403/415 responses,
normal serialization, propagated exceptions and cooperative cancellation all pass
through that outer cleanup boundary. The cache is thread-local, explicitly
registered with the Boehm GC and never inherited by another thread. Ending a
request releases ownership deterministically; physical heap reclamation remains
GC-dependent. There is no implicit destructor or native-resource disposal hook.
Use `defer`/`finally` for resources and `@OnStop` for application ownership.
Process termination, forced native thread cancellation and fatal signals cannot
promise application cleanup.

Application/startup injection graphs and declared retaining fields must not reach
request-scoped classes, even through nullable/container/ordinary nested fields or
interfaces implemented by request providers. Qualified injections use their
resolved concrete provider, so an application implementation stays valid when a
request implementation also exists. Injected fields are provider-owned and cannot
be reassigned in Tinox code; this prevents later replacement of a safe provider
with request state. Replace a captive singleton owner
with a request-scoped owner that injects application services. `spawn` directly
inside request-scoped classes is a compile error. Calling an indirect helper that
spawns after a managed request instance exists fails visibly at runtime; this
conservative rule rejects even an apparently immutable capture. Routes without
managed request instances retain structured task support. Schedule detached work
outside request ownership using copied immutable data. Request caches do not
propagate to background threads.

This is a checked generated-provider ownership contract, not a general borrow
checker. Manually constructing annotated classes, bypassing providers with raw
FFI, or erasing references to `Any` does not create managed request ownership and
must not be used to retain request state. There is no supported request scope for
WebSocket/AMQP callbacks, CLI entry points, or manually called providers outside
annotated REST dispatch; provider access without an active context fails loudly.
Such code should use an application-scoped service or an explicit locally owned
object instead of treating the old per-call factory as a request context.

The pre-existing non-`@Service` scalar-handler exception response bug is tracked
in #573: such a throw may return success and leave a pending error. Request cache
cleanup still runs on that path; this scope change does not claim to repair its
HTTP status or error-consumption policy (broader error mapping: #524).
