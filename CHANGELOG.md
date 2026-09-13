# Changelog

All notable changes to Tinox are documented in this file.

## [2.2.0] - 2026-09-11

### Breaking

- **`Component::fileUpload(uploadUrl, showProgress, onChange)`** (`tinox.core:ui`,
  bumped to 1.0.11) replaces v1's `fileUpload(onChange)`. It now really
  transfers the picked file's bytes (in 32 KB chunks, base64-encoded,
  POSTed sequentially to an app-provided companion `HttpServer` route)
  instead of only reporting the file's name, and can show a live
  percent + estimated-time-remaining readout driven by the browser's
  real `xhr.upload.onprogress` events. `uploadUrl=""` keeps v1's exact
  old behavior (no network transfer). The 32 KB chunk size isn't
  arbitrary: `tinox.core:http_server` hard-rejects any body over 4 MB,
  and measurement showed the cost of a large single request isn't
  linear anyway — a Boehm GC-triggered stop-the-world pause on the
  receive buffer's growth makes anything much larger than that a
  multi-second-to-a-minute stall per request under real concurrent
  load. `tinox.core:http_server` gained `HttpServer::options(path,
  handler)` (bumped to 1.0.4) to answer the resulting CORS preflight —
  an upload-progress listener disqualifies a request from CORS
  "simple" status regardless of headers/body size.
- **REST handler parameters now require an explicit
  `@PathParam`/`@QueryParam`/`@PostParam`/`@HttpContext` annotation.**
  The old implicit single-unannotated-`ctx`-parameter shape is a hard
  compile error, with no compatibility fallback. A handler's return
  type decides the response: `-> HttpContext` keeps manual
  response-building, anything else (a `@JsonSerializable` class,
  `List<class>`, `String`, `Int64`, `Bool`, ...) is auto-serialized as
  the JSON body.
- **Cross-namespace imports must be explicit** (issue #194, both
  phases): a name pulled in only because some *other* file in the
  program happened to import it no longer resolves — every file needs
  its own `import` for anything outside its own namespace. A file
  needs no import at all for a name in its own namespace (same
  directory), matching how the stdlib already behaved.
- **A namespace's physical folder layout must mirror its dotted path**
  (issue #185): `namespace a.b.c { ... }` must now live under a
  path ending in `a/b/c/`, hard-enforced at compile time.
- **`if` conditions must be parenthesized**, `if (cond) { ... }`,
  now hard-enforced by the parser for both statement and expression
  forms (issue #233). Scoped to `if` only — `while`/`for`/match-guard
  `if` are unaffected.

### Added

- **Tinox-UI** (`tinox.core:ui`, new extended-tier module, issue #215,
  all 8 phases): a server-driven, Vaadin/Phoenix-LiveView-style web UI
  framework — write the whole UI as a `Component` tree on the server,
  a generic client JS (baked into every compiled app, no separate
  asset to forget shipping) renders it and ships events back over a
  persistent WebSocket.
  - `@TinoxUIApp(httpPort, wsPort)` + `@View`/`@Route("/path/:param")`
    annotation sugar generates the entire HTTP shell + WebSocket
    accept loop; a hand-wired lower-level API remains available.
  - Diff-based rendering (`TinoxUIRuntime::diff`) sends only the
    changed patch ops on each event instead of the whole tree, with
    stable component ids across renders; automatic reconnect with
    exponential backoff (0.5s–10s) on a dropped connection.
  - Over 40 widgets: layout (`VBox`/`HBox`/`Spacer`), typed inputs
    (`TextField`/`NumberField`/`IntField`/`PasswordField`/
    `EmailField`/`DatePicker`/`TimePicker`/`DateTimePicker`/`Slider`/
    `ComboBox`), `Checkbox`/`CheckboxGroup`/`RadioGroup`/`Dropdown`,
    `FileUpload` (real chunked content + progress, see Breaking),
    generic `DataGrid<T>`/`GridColumn<T>` (including
    component-per-cell, sortable/hideable columns), `Tabs`/`Dialog`/
    `Accordion`/`Notification`/`ProgressBar`, `Avatar`/`Badge`/`Card`/
    `Details`/`ConfirmDialog`/`Breadcrumb`/`MenuBar`/`withTooltip`,
    `Component::jsComponent` (embed an arbitrary custom element/JS
    module) and `Component::html` (raw markup, including executable
    `<script>`), plus `Component::link`-based client-side routing.
  - A tinox-central-styled default theme (dark, glass surfaces,
    cyan/purple accents) ships for every app with no opt-in.
  - `Component::withStyle`/`withTooltip` and a client-side
    `window.tinoxSendEvent` (for a `Component::html()`-embedded
    `<script>` to trigger a real server round trip, e.g. a polling
    timer) round out the v1 escape hatches.
- **`@Getter`/`@Setter`** (Lombok-style): generates `get<Field>()`/
  `set<Field>(value)` for every field lacking a hand-written method of
  that exact name (class-level), or for a single field (field-level);
  a hand-written accessor always wins over a synthesized one.
- **`@Transactional`** (class- or method-level) wraps a method body in
  a real `BEGIN`/`COMMIT`-or-`ROLLBACK` against a new Postgres
  connection pool (`[database] pool` in `tinox.toml` now actually does
  something); propagation is `REQUIRED` (joins an already-open
  transaction on the same thread). Postgres-only for now — SQLite/MySQL
  keep their existing single-connection auto-commit behavior.
- **Real thread-safety for `Mutex`/`Semaphore`/`RWLock`**
  (`tinox.core.semaphore`): all three now wrap real POSIX primitives
  (`pthread_mutex_t`/`sem_t`/`pthread_rwlock_t`) instead of a
  non-atomic check-then-act that was never actually safe under
  contention; `lock()`/`acquire()` now genuinely block instead of
  throwing on contention.
- **`Process::run(argv, timeoutMs, stdin)`** (`tinox.core.process`):
  argv-based subprocess execution (never a shell — no injection risk)
  with captured stdout/stderr, a wall-clock timeout, and stdin
  piping. **`InteractiveProcess`**: a long-lived subprocess driven
  incrementally via `writeStdin`/`readOutput` for its whole session
  (e.g. `kubectl exec -i`), backed by a real PTY (not plain pipes), so
  interactive line-discipline (a lone `\r` for Enter) works correctly.
- **`tinox.core.kubernetes`** (new extended-tier module): a Kubernetes
  REST API client — kubeconfig/in-cluster auth including TLS
  client-cert mTLS, explicit multi-context support, a generic dynamic
  resource client covering any Group/Version/Kind (including CRDs)
  plus 19 typed accessors, typed models for the common built-in kinds,
  live `?watch=true` streaming, and Server-Side Apply.
- **`tinox graph`**: writes a Mermaid call-graph flowchart seeded from
  every auto-run entry point (REST, WebSocket, AMQP 0-9-1/1.0
  consumers, CLI `@Command`).
- **`tinox publish [--repository <id>]`/`tinox search <query>`**:
  publish the current project to a tinox-central-shaped registry
  (needs `[package] group` + `TINOX_CENTRAL_ADMIN_KEY`) and search a
  registry's package catalog, from the CLI.
- **Dev UI**: component full-state dump (real DI singleton field
  values, not just name/scope), a `/tests/run` endpoint backing a new
  Tests view (runs `tinox test` in the connected project and streams
  real PASS/FAIL output), and `/routes` now reports each route's
  param bindings plus example request/response JSON for the "Try it
  out" dialog. `tinox dev` now defaults to the published
  `ghcr.io/subnix-work/tinox-devui` image (no local Docker build
  needed first) and has a real Ctrl-C cleanup handler.
- **VS Code extension** (`editors/vscode/`): an LSP client over the
  existing `tinox-lsp` binary, same role the Eclipse plugin already
  played. **Eclipse plugin**: a real "Import Existing Tinox Project"
  wizard and distinct `src`/`tests` folder icons.
- `RouteMatcher::param()` (`tinox.core:http_server`) as a single-value
  convenience wrapper around `extractParams()`.

### Fixed

- **Runtime/concurrency**, all found under real concurrent load, not
  by inspection:
  - A fresh connection could be force-closed by the SAME event-loop
    iteration's own stale-connection scan, before ever being read once
    — an unsigned clock-read underflow from timestamping it against a
    slightly-later coarse-clock read than the iteration's own `now_ms`
    (issue #175).
  - The multi-threaded HTTP response-send loop didn't retry on
    `EINTR`, silently truncating a response whenever the GC's
    stop-the-world signal landed mid-`send()` (Bug 180).
  - An oversized request body was silently clamped and truncated
    instead of rejected — now a clean 413, both for the manually-wired
    and annotation-driven `HttpServer` paths (issue #174).
  - A WebSocket connection was force-closed after every ~5s of
    legitimate idle time (a "slow-loris" guard on `accept()` stayed
    active for the connection's entire lifetime instead of just the
    handshake) (issue #221).
  - `HttpServer::listen()` logged and silently kept running on a
    failed `bind()`, serving nothing forever with no visible error
    (issues #226, #234, #235).
  - `tinox test` silently hung instead of running, for any test file
    importing a class with its own auto-run annotations.
- **Codegen/typecheck**:
  - A subclass instance was rejected as a base-class-typed
    method-call argument or variable initializer (issue #173).
  - A closure called through an object field (not a local variable)
    always reported `i64` as its LLVM return type regardless of its
    real declared type (issue #218); the same class of bug for two
    *independently* generic-specialized closures could also collide on
    the same synthesized `__lambda_N` name (issue #222).
  - `Float64.toInt()` as a method call (not `x as Int64`) generated
    invalid LLVM IR (issue #223).
  - `ORM` `.contains()`/`.startsWith()`/`.endsWith()` silently dropped
    the entire filter (matching every row) for any non-literal
    argument — only a literal string worked before (issue #238).
  - Three Tinox-UI runtime bugs: a per-connection instance allocated
    via raw `GC_malloc` left `String` fields as an invalid null
    pointer instead of an empty string; a client-side `DIV` update
    patch wiped a `Dialog`'s whole rendered subtree instead of leaving
    structural widgets alone; an invisible placeholder component never
    got a patchable id (issues #228, #230, #231).
  - `tinox doc` silently excluded generic classes (`class Foo<T>`)
    from a module's generated docs page.
  - `return` inside a `try` (or an `@Transactional` method) skipped
    its own `finally`/commit entirely (issue #193, and the
    `@Transactional`-specific case fixed ahead of it).
- **Tooling**: `tinox-lsp` failed to resolve a project-local import
  (only ever resolved `tinox.core.*`), and separately never embedded
  `tinox.core.db` at all; the Eclipse plugin's nature/decorator
  registration silently no-op'd on a malformed `plugin.xml`, and its
  `plugin.xml` XML comments couldn't contain a bare `--` anywhere,
  which silently killed every extension registration, not just the
  malformed comment; the VS Code extension's `require`d dependency
  wasn't bundled into the packaged `.vsix`, so activation silently
  never ran.

## [2.1.0] - 2026-08-11

### Breaking

- **`class Main` is now required in every program**, including ones that
  previously relied on an auto-run annotation
  (`@GET`/`@Http3RestController`/`@WebsocketEndpoint`/`@Amqp10Consumer`/
  `@Amqp091Consumer`) alone. Each auto-run kind used to generate its own
  `@tinox_main` directly, so at most one could exist per program, they
  could never be combined, and an explicit `class Main` silently
  pre-empted all of them with no error. Every registered kind now spawns
  on its own thread from a single, unified bootstrap that also calls
  `Main.main()` — so REST + WebSocket + AMQP consumers can now coexist in
  one program, but an annotation-only file without `class Main` needs one
  added. `@Command` CLI programs and `tinox test` stay exempt.
- **The stdlib is split into a core tier (always bundled) and an
  extended tier (explicit dependency + `tinox install`)**. 41
  fundamental modules (types, collections, math, string, I/O, logging,
  concurrency primitives) stay built into the compiler and resolve
  unconditionally; 31 protocol/vertical modules (REST, AMQP 0-9-1/1.0,
  WebSocket, crypto, OAuth2/OIDC, JSON/YAML/TOML/XML/msgpack, HTTP/2/3,
  DB, …) moved to `crates/tinox-core-ext/` and must now be declared as a
  `group:artifactId:version` dependency in `tinox.toml` and
  `tinox install`ed before a program can import them — a program that
  imported one of these without declaring it now gets a hard,
  actionable error instead of silently resolving against a bundled
  copy. `tinox.toml` gains `[[repositories]]` (defaulting to
  `https://central.tinox-lang.de`), and installed coordinate
  dependencies land in a new global, Maven-style cache
  (`~/.tinox/repository/`, `TINOX_HOME`-overridable) shared across every
  project on the machine.

### Added

- **`tinox docker`**: compiles the project and packages the binary into
  a minimal Docker image. A new `[docker]` section in `tinox.toml`
  declares the ports to `EXPOSE`, plus optional `image`/`base`/
  `extra_packages` overrides (default base `debian:trixie-slim`). Only
  installs the runtime shared libraries actually linked, and runs `ldd`
  inside the freshly built image afterward — hard-fails with the exact
  missing library instead of silently shipping a broken image.
- **Startup banner**: every compiled program with at least one auto-run
  endpoint now prints an ASCII banner, the `tinox.core` modules declared
  in `tinox.toml`, each endpoint's protocol + port, and the bootstrap's
  own elapsed time — on by default, no `import tinox.core.logger;` or
  annotation needed. Opt out per project with `[startup]`/
  `banner = false` in `tinox.toml`.
- **Five new stdlib modules**: `tinox.core.compress` (gzip/raw-DEFLATE,
  [#132](https://github.com/subnix-work/tinox/issues/132)),
  `tinox.core.sse` (Server-Sent Events,
  [#133](https://github.com/subnix-work/tinox/issues/133)),
  `tinox.core.smtp` (SMTP client incl. STARTTLS/AUTH PLAIN,
  [#134](https://github.com/subnix-work/tinox/issues/134)),
  `tinox.core.redis` (RESP2 client,
  [#135](https://github.com/subnix-work/tinox/issues/135)), and
  `tinox.core.msgpack` (MessagePack codec,
  [#136](https://github.com/subnix-work/tinox/issues/136)).
- **Transitive dependency resolution** for `tinox install`/`add`: a
  dependency's own declared dependencies are now discovered and
  installed too, not just the direct list
  ([#157](https://github.com/subnix-work/tinox/issues/157)). An import
  resolving against two different installed dependencies at once is now
  a hard, actionable error instead of a silent first-match
  ([#156](https://github.com/subnix-work/tinox/issues/156)).
  `tinox package` now archives `tinox.toml` itself, so a published
  package's own dependencies are discoverable by its consumers.
- **`tinox doc`**: generated pages gain Description (from `tinox.toml`),
  Dependencies (linked to sibling doc pages), and Examples sections
  ahead of the existing auto-extracted API reference; published doc
  pages now exist for all 72 tinox-core modules.
- **`docs.html`/`docs_en.html`** redesigned to match tinox-central's
  dark, glassy theme and use the full page width; every per-module
  stdlib reference section now embeds the corresponding
  `docs/tinox-core/<module>/<version>/docs.html` page via `<iframe>`
  instead of duplicating its content by hand.
- **Fuzzing**: a dedicated HTTP/2 frame-parsing target
  ([#111](https://github.com/subnix-work/tinox/issues/111) follow-up),
  a `make fuzz` target running all seven harnesses against their seed
  corpus, and weekly CI wiring.

### Fixed

- **Generics/typecheck/codegen**: own-type-param generic instance
  methods (e.g. `Option<T>.map<U>`) had no instance-call dispatch path
  and ICE'd ([#153](https://github.com/subnix-work/tinox/issues/153)),
  including when chained with no intermediate `let`
  ([#158](https://github.com/subnix-work/tinox/issues/158)).
  `Class<T>::method(...)` (type args before `::`) discarded them
  entirely, ICE'ing on codegen
  ([#166](https://github.com/subnix-work/tinox/issues/166)). An
  arrow-sugar lambda (`n => ...`) passed to a generic instance method
  silently mis-specialized its type param to `Int64`
  ([#165](https://github.com/subnix-work/tinox/issues/165)). Namespace-
  wrapped generic classes (essentially all of the stdlib) were missing
  call-site type-argument unification entirely, breaking any bare-
  chained factory call with no `let`
  ([#161](https://github.com/subnix-work/tinox/issues/161)). Interface
  vtable dispatch hardcoded `i64` as every method's return type
  regardless of its real declared type, corrupting `String`/`Bool`-
  returning interface calls. The same symbol declared via `extern fn` in
  more than one file merged into one program produced a duplicate LLVM
  `declare` ([#168](https://github.com/subnix-work/tinox/issues/168)).
- **`List<Int64>.join()`** segfaulted immediately — no element-type
  check, reinterpreted raw integers as string pointers; now a
  compile-time error
  ([#164](https://github.com/subnix-work/tinox/issues/164)).
- **`@inline`** ICE'd on any function/method with a non-void return
  type — `alwaysinline` was emitted in the LLVM IR return-value-
  attribute position instead of the function-attribute position.
- **O(n²) string building** (`s = s + fromCharCode(...)` in a loop) was
  quadratic across several stdlib modules (WebSocket, HPACK, AMQP
  0-9-1/1.0, HTTP/2, Redis, Base64/Hex, JWT, URI, Crypto) — replaced
  with a single-pass byte-collect-then-convert
  ([#167](https://github.com/subnix-work/tinox/issues/167)).
- **HTTP route params**: `route_matches()` reused a single stack buffer
  across `:param` names, so every parameter but a route's last one
  silently resolved to `""`
  ([#176](https://github.com/subnix-work/tinox/issues/176)).
- **Package manager**: dependencies were read from a separate
  `tinox.yaml` that no other subcommand consulted, so `tinox add`/
  `install` never actually took effect — moved into `tinox.toml` itself
  ([#154](https://github.com/subnix-work/tinox/issues/154)). `tinox
  new`'s scaffold failed both `tinox run` and `tinox test` out of the
  box ([#155](https://github.com/subnix-work/tinox/issues/155)/
  [#159](https://github.com/subnix-work/tinox/issues/159)).
  `tinox install` now understands tinox-central's base64-JSON artifact
  envelope and exits non-zero on partial failure.
- **`docs.html`/`docs_en.html`/`README.md`**: every code example still
  used the pre-2.0.0 bare top-level `fn main()` shape and failed to
  compile as written; all ~70 examples fixed and individually
  re-verified against the real compiler
  ([#160](https://github.com/subnix-work/tinox/issues/160)/
  [#163](https://github.com/subnix-work/tinox/issues/163)).
- **`tinox docker`**'s default base image bumped from
  `debian:bookworm-slim` to `debian:trixie-slim` — the older default's
  glibc (2.36) proved too old for a host-compiled binary from any
  reasonably current dev machine, tripping the post-build `ldd` check
  on the very first real-world use rather than being a theoretical edge
  case.

### Changed

- `tinox.core.metrics.Stopwatch` renamed to `MetricsStopwatch` to avoid
  a bare-name collision with `tinox.core.time.Stopwatch`
  ([#170](https://github.com/subnix-work/tinox/issues/170) — the
  underlying lack of module-qualified symbol keys in tinox-typecheck is
  a larger, cross-cutting gap left open for a future pass).

## [2.0.0] - 2026-08-02

### Breaking

- **Mandatory class-qualified function calls** ([#149](https://github.com/subnix-work/tinox/issues/149)).
  A top-level `fn`/`fnc` declaration with a body is now a hard compile
  error (`extern fn` FFI declarations stay legal). Every program needs an
  entry point shaped exactly `class Main { fnc main() -> Int32 }`, living
  in a file named `Main.tnx` (per the existing one-class-per-file rule).
  A same-class bare call (`helper()` from another method of the same
  class) is resolved automatically; a call into a *different* class still
  needs the `ClassName::method()` form. Existing programs using top-level
  `fn` need migrating — wrap the entry point and any free helper
  functions in a class.

### Fixed

- **Memory safety**: `tinox_HttpServer_listen`'s epoll worker threads
  could crash inside GC-managed memory under allocation-heavy
  annotation-driven (`@GET`/`@POST`/…) routes
  ([#140](https://github.com/subnix-work/tinox/issues/140)). Root cause:
  several `static __thread` runtime buffers hold pointers to GC-managed
  memory, but Boehm GC does not automatically scan thread-local storage
  as roots — now explicitly registered via `GC_add_roots()` on every
  thread that can run Tinox code.
- `Map<K, V>` with a non-`String` key (`Int64`, `Bool`, …) segfaulted
  immediately on `insert`/`get`/`contains`/`remove` and on `m[key]`
  indexing — the key's raw bit pattern was reinterpreted as a pointer
  instead of being stringified
  ([#129](https://github.com/subnix-work/tinox/issues/129)).
- A struct literal omitting a declared field (or naming an unknown one)
  silently left the field as uninitialized garbage instead of failing to
  compile
  ([#130](https://github.com/subnix-work/tinox/issues/130)).
- Two classes sharing a bare name across different imported modules
  silently corrupted each other's codegen field layout, surfacing as a
  confusing "field not in layout of typed class" internal error; now a
  clear compile-time diagnostic
  ([#139](https://github.com/subnix-work/tinox/issues/139)).
- An `Int32`-returning call result used in a binary expression and
  stored into an `Int32` local generated invalid LLVM IR (an `i64`/`i32`
  type mismatch)
  ([#150](https://github.com/subnix-work/tinox/issues/150)).
- `resolve_entry_file` ignored `tinox.toml`'s `[package] entry` field,
  always falling back to the hardcoded `src/main.tnx`
  ([#152](https://github.com/subnix-work/tinox/issues/152)).
- `fuzz/{hpack,amqp091,amqp10}/build.sh` failed to link, missing `-lz`
  ([#151](https://github.com/subnix-work/tinox/issues/151)).
- Verified the `@Auth` annotation's credential-validation fix (default-
  deny without a registered `AuthValidator`) actually holds
  ([#141](https://github.com/subnix-work/tinox/issues/141)).

## [1.0.2] - 2026-07-28

Security and robustness hardening. 23 issues found by an automated
security review and independently verified against current source
before fixing — see [#86](https://github.com/subnix-work/tinox/issues/86)
through [#108](https://github.com/subnix-work/tinox/issues/108) for full
root-cause/fix/verification details on each. No breaking changes.

### Fixed

- **AMQP 1.0 / 0-9-1**: `Amqp10Reader`/`AmqpReader091` had no bounds
  checking, so a malformed or truncated broker frame (including a
  heartbeat reused as a SASL frame) crashed the client
  ([#86](https://github.com/subnix-work/tinox/issues/86),
  [#89](https://github.com/subnix-work/tinox/issues/89)). SCRAM-SHA-256's
  broker-supplied iteration count had no upper bound, enabling a CPU-DoS
  during connect
  ([#87](https://github.com/subnix-work/tinox/issues/87)).
  `Amqp10Connection::connect()` had no TLS variant, so SASL
  credentials went out in cleartext — added `connectTls()`
  ([#88](https://github.com/subnix-work/tinox/issues/88)). Failed dials
  leaked the socket fd
  ([#90](https://github.com/subnix-work/tinox/issues/90)).
- **WebSocket / HTTP / HTTP/2**: TLS accept had no receive timeout, so
  one stalled client could hang the whole HTTPS/WSS server
  ([#91](https://github.com/subnix-work/tinox/issues/91)). WebSocket
  control frames (Ping/Pong/Close) didn't enforce RFC 6455's
  FIN/length limits
  ([#92](https://github.com/subnix-work/tinox/issues/92)). An HTTP/2
  stream's header block/body could grow without bound across
  CONTINUATION/DATA frames
  ([#94](https://github.com/subnix-work/tinox/issues/94)). The response
  header buffer could overflow on the stack
  ([#95](https://github.com/subnix-work/tinox/issues/95)), and
  Content-Length had no cap
  ([#96](https://github.com/subnix-work/tinox/issues/96)).
- **Runtime memory safety**: an integer overflow in the array allocator
  could corrupt the heap for large peer-controlled lengths
  ([#93](https://github.com/subnix-work/tinox/issues/93)). The JSON
  parser could loop forever on malformed input
  ([#97](https://github.com/subnix-work/tinox/issues/97)). The ZIP
  reader trusted an entry's uncompressed size independent of its
  compressed size, allowing an out-of-bounds read from a crafted archive
  ([#98](https://github.com/subnix-work/tinox/issues/98)). The
  Prometheus metrics formatter could overflow its output buffer with
  long metric names
  ([#99](https://github.com/subnix-work/tinox/issues/99)).
- **Package manager**: `tinox install` used a dependency's
  group/artifactId/version directly as filesystem path components,
  allowing a malicious `tinox.yaml` to write files outside
  `.tinox/deps` (critical)
  ([#100](https://github.com/subnix-work/tinox/issues/100)).
- **Concurrency**: the cross-function exception slot
  (`@__tinox_err`) was a process-wide global instead of thread-local,
  so concurrent HTTP request handlers could race on each other's
  thrown/caught values — now `thread_local`
  ([#101](https://github.com/subnix-work/tinox/issues/101)). The
  SQLite statement cache and the Postgres connection had no locking
  despite the HTTP server running handlers concurrently
  ([#102](https://github.com/subnix-work/tinox/issues/102),
  [#103](https://github.com/subnix-work/tinox/issues/103)).
- **Auth / REST framework**: `@Auth("bearer"/"basic")` only checked
  the Authorization header's scheme prefix and never validated the
  actual credential — it now fails closed by default and requires the
  application to register a real validator via the new
  `RestApi::setAuthValidator()`
  ([#104](https://github.com/subnix-work/tinox/issues/104)).
  `Jwt::verify()`/`decode()` ignored `exp`/`nbf` claims, accepting
  expired tokens
  ([#105](https://github.com/subnix-work/tinox/issues/105)).
  `Http::setHeader()` had no CRLF validation, allowing header
  injection from `RestClient`/`RequestBuilder`
  ([#106](https://github.com/subnix-work/tinox/issues/106)). Generated
  `toString()`/`toJson()` keyed the `@Sensitive`/`@Masked`/
  `@DoNotSerialize` skip-set by the wrong class, so an inherited
  sensitive field leaked in a subclass's output
  ([#107](https://github.com/subnix-work/tinox/issues/107)).
- **CI**: the `jgrep-tinox` dogfood checkout was unpinned and the
  workflow had no explicit least-privilege `permissions:` — now pinned
  to a commit SHA with `contents: read`
  ([#108](https://github.com/subnix-work/tinox/issues/108)).

### Added

- `Amqp10Connection::connectTls(host, port, user, pass, verify)` — TLS
  variant of `connect()`.
- `RestApi::setAuthValidator(fnc(String, String) -> Bool)` — registers
  the credential validator used by `@Auth`-protected routes.

## [1.0.1] - 2026-07-27

Packaging fix, no language/stdlib changes.

### Fixed

- The `tinox` binary was not relocatable: `tinox build`/`tinox run`
  located `runtime.c` via an absolute path baked in at Rust compile time
  (`CARGO_MANIFEST_DIR`), with no fallback or override. On any machine
  other than the one it was built on, every build failed
  ("Runtime compilation failed"). Standard library imports
  (`import tinox.core.*` — used by nearly every non-trivial program) had
  a partial fix already (`TINOX_PATH` env var) but no system-install
  fallback either, failing with `"TINOX_PATH not set and dev path not
  found"`.

  Both now additionally fall back to a fixed system path
  (`/usr/share/tinox/runtime.c` and `/usr/share/tinox/core`) after the
  existing `TINOX_PATH`/dev-checkout checks, so a distro-packaged
  `tinox` binary (e.g. the AUR `tinox-bin` package, which installs to
  those paths) works standalone. See
  [#85](https://github.com/subnix-work/tinox/issues/85).

## [1.0.0] - 2026-07-27

First official release. Tinox is a native, statically typed programming
language with an LLVM backend, garbage collection, and concurrency
support.

### Language

- Lexer with Unicode support, string interpolation, and ranges (`..` / `...`).
- Full recursive-descent parser producing a complete AST.
- Static type checker: base types, classes, interfaces, enums, generics
  (monomorphized), function types, annotations.
- LLVM IR code generator, including `@inline` support and a typed
  value bridge between typecheck and codegen.
- C runtime (pthread-based): `spawn` starts a real POSIX thread, not a
  cooperative coroutine.
- Classes with inheritance, interfaces with vtable dispatch, enums with
  pattern matching, generics, tuples, arrays, lambdas/closures, `Map`,
  `defer`, `try`/`catch`/`finally`/`throw`, an import system, and an
  annotation system (`@Name`/`@Name(args)`) validated by the compiler.
- Async/concurrency: `spawn`/`await`, channels, `select`.
- **Hard compiler rule (new in this release):** a `.tnx` file may contain
  at most one top-level `class`/`interface`/`enum`; if it has one, the
  filename must match the type name exactly. Multi-type modules are
  directories with one file per type.

### CLI

`tinox build` / `run` / `check` / `fmt` / `fmt --write`. `--checked` build
mode enables a heap-kind registry that guards array/map runtime functions
against dispatch-on-wrong-type bugs.

### Standard Library (`tinox-core`, 50+ modules)

- **HTTP:** `http_server` (incl. `listenTls` for HTTPS), `rest_framework`
  (annotation-driven REST controllers: `@GET`/`@POST`/`@PUT`/`@PATCH`/
  `@DELETE`, `@Path`, `@Produces`, `@Consumes`, `@StatusCode`, `@Auth`),
  `mini_http`.
- **WebSocket (v1):** RFC 6455 server (`websocket`), `wss://` (TLS), and
  an annotation-driven form (`@WebsocketEndpoint`/`@OnOpen`/`@OnMessage`/
  `@OnClose`). Known v1 gaps: no fragmentation, no client, no
  permessage-deflate.
- **AMQP-0-9-1 client (v1):** `amqp091` for brokers such as RabbitMQ,
  including `amqps://` (TLS). Known v1 gaps: no multi-channel, no
  `exchange.declare` (default/broker-predefined exchanges only), no
  publisher confirms, no annotation-driven consumer API, no
  heartbeat/auto-reconnect.
- **AMQP-1.0 client:** `amqp10`, a separate implementation (own type
  system, Connection→Session→Link hierarchy with credit-based flow
  control). Covers multiple sessions/links per connection, SASL PLAIN
  and SCRAM-SHA-256, delivery states beyond `accepted` (`rejected`/
  `released`/`modified`), transactions, link recovery/resumption,
  heartbeat/auto-reconnect, and an annotation-driven consumer API
  (`@Amqp10Consumer`/`@OnMessage`).
- **Data:** `json`, `csv`, `xml`, `yaml`, `toml`, `regex`, `base64`, `hex`.
- **Security:** `crypto` (incl. AES, PBKDF2/HMAC-SHA-256), `jwt`.
- **Persistence:** SQLite-backed ORM (`db`, `@Entity`), CRUD example app.
- **Collections/utilities:** `collections`, `queue`, `stack`, `heap`,
  `trie`, `graph`, `bitmap`, `set`, `cache`/`LruCache`, `math`/`mathf`/
  `mathx`, `string`, `time`/`duration`, `uuid`, `random`, `decimal`,
  `complex`.
- **System/async:** `fs`, `io`, `env`, `process`, `socket`, `cron`,
  `events` (EventEmitter), `pool`, `semaphore` (Mutex/RWLock),
  `ratelimit`, `metrics`.
- **Protocol/format internals:** `hpack` (HTTP/2 header compression),
  `http2_server`.

### Tooling

- Language Server Protocol implementation (`tinox-lsp`).
- Eclipse plugin.
- `tinox fmt` formatter.
- REPL: not yet implemented (planned).

### Testing & CI

- Golden end-to-end tests (`tests/e2e/*.tnx`), a generated context
  matrix, a boundary-value suite, and a stdlib smoke gate exercising
  every `tinox-core` module.
- `make check` (clippy with zero warnings, unit tests, e2e/matrix/
  boundary/stdlib_smoke, and a dogfood pass building examples/
  benchmarks and a downstream project) gates every commit via a
  pre-push hook and GitHub Actions.
- `make asan` (AddressSanitizer, plain malloc) and `make checked`
  (heap-kind registry) as periodic/pre-release deep-verification passes,
  outside the default `make check` gate.

### Known limitations in this release

- [#84](https://github.com/subnix-work/tinox/issues/84): `DB.of(EntityClass).save(...)`
  / `.delete(...)` are not recognized as ORM query-chain terminals and
  currently fail to compile (invalid LLVM IR) rather than silently
  misbehaving. Affects `examples/crud` only; not exercised by any
  `tests/e2e/orm_sqlite_*.tnx` test, which use `filter`/`count`/`first`/
  `list` instead.
- See the README sections on the WebSocket and AMQP-0-9-1 clients above
  for their documented v1 scope limitations.

### Project history

This release consolidates over 250 commits and 80+ tracked issues
(bug fixes and feature work) since the initial compiler bring-up. Full
history: [GitHub issues](https://github.com/subnix-work/tinox/issues?q=is%3Aissue).
