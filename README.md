# Tinox Programming Language

**Tinox** is a native, statically typed programming language with an LLVM backend, garbage collection, and concurrency support.

> Named after **Tino** + **Linux**/Unix – a language for modern, readable software.

## Status

**Phase:** V2 – Feature-Complete Core (In Development)

| Component    | Status      | Notes                                 |
|--------------|-------------|----------------------------------------|
| Lexer        | ✅ Done     | Unicode, string interpolation, ranges  |
| Parser       | ✅ Done     | Full AST                               |
| Type Checker | ✅ Done     | Base types, classes, enums, generics, annotations |
| Code Gen     | ✅ Done     | LLVM IR backend, `@inline` support     |
| Runtime      | ✅ Done     | C runtime (pthread-based)              |
| CLI          | ✅ Done     | build, run, dev, test, doc, check, fmt, repl, install |

## Installation

```bash
git clone https://github.com/subnix-work/tinox.git
cd tinox
cargo build --release
# Binary: target/release/tinox
```

Requirements: `clang`, `llc` (LLVM tools)

**Platform: Linux only, by design.** The C runtime's HTTP/WebSocket/HTTP2
event loop is epoll-based (no kqueue/IOCP fallback), it relies on Linux's
`MSG_NOSIGNAL` socket flag, and crash backtraces + the Boehm GC's
stop-the-world suspend assume a glibc/ELF target. Compiling
`runtime/runtime.c` on another OS fails immediately with a clear error
rather than a confusing cascade of missing-header errors. Tracked as
[#113](https://github.com/subnix-work/tinox/issues/113) if you're
interested in what porting this would take.

## Usage

```bash
tinox new <name>                             # Scaffold a new project (writes tinox.toml)
tinox build [file] [--force]                 # Compile to an executable (uses tinox.toml if no file)
tinox run   [file] [--force]                 # Compile and run (uses tinox.toml if no file)
tinox dev   [file]                           # Dev mode: hot-reload on file changes
tinox test  [file]                           # Run all @Test-annotated methods
tinox test --watch                           # Re-run tests on file changes (TDD mode)
tinox doc   [--open]                         # Generate HTML documentation in docs/
tinox check program.tnx                      # Type-check only, no compilation
tinox fmt program.tnx                        # Format (writes to stdout)
tinox fmt --write program.tnx                # Format and overwrite the file
tinox repl                                   # Start the interactive REPL
tinox install                                # Download and install all dependencies (tinox.toml)
tinox add <group> <artifact> <version> <url> # Add + install a dependency
tinox package                                # Pack src/ into <name>-<version>.tar.gz
tinox publish [--repository <id>]            # Pack and upload to a registry
tinox search <query> [--repository <id>]     # Search a registry's package catalog
```

Run `tinox help` for the same list from the CLI itself.

Use `tinox describe <Symbol>` for compact, checked API signatures and imports
from the current project's resolved sources. `--member`, `--signatures-only`,
`--limit`, and `--json` support focused queries; see the
[symbol-description protocol](docs/compiler-describe.md).

Use `tinox explain --bootstrap [file]` to inspect generated components, routes,
scopes, injection and startup/join ordering without starting the application.
`--json` exposes the same plan for tooling; see the
[bootstrap explanation protocol](docs/compiler-explain.md).

`tinox check --json` and `tinox build --json` provide versioned structured
diagnostics for tooling. See the [diagnostics protocol](docs/compiler-diagnostics.md)
for the schema, stable codes, source ranges, and LSP mapping.

### Testing

Methods annotated `@Test` (optionally `@Test("description")`) are discovered and
run by `tinox test`:

```tinox
class MathSuite
{
    @Test("addition works")
    fn testAdd() -> Nothing
    {
        assert(1 + 1 == 2);
    }
}
```

```bash
tinox test              # run once
tinox test --watch      # re-run on file changes
```

Tests are discovered recursively under `tests/` and `src/` of the nearest
`tinox.toml` project. Hidden directories, `target`, `build`, `dist`,
`node_modules`, and symlinks are skipped. Discovery/collection errors, failing
tests, and suites without any `@Test` methods exit unsuccessfully. Watch mode
keeps running after failures and discovers newly added nested files.

### Application build reuse

`tinox build` and `tinox run` reuse an unchanged executable after validating a
versioned build fingerprint and the executable's content and execute permission.
`--force` rebuilds explicitly; `TINOX_BUILD_CACHE=0` disables application reuse.
`TINOX_CACHE_TRACE=1` reports hits, invalidation and conservative bypasses.
Disabling the runtime cache also disables application reuse.

The fingerprint covers source/import and manifest bytes actually read by the
frontend (including installed dependency sources), generated IR, compiler/tool
contents, target/optimization configuration, the preprocessed runtime/headers,
C/link flags, environment, native tools' shared libraries and effective
compiler/linker configuration. Actual
link inputs and loaded shared libraries are checked by content and selection.
Unsupported native flags or an incomplete native dependency inventory bypass
reuse. This currently targets the existing Linux native toolchain.

Native tool digests are memoized under the runtime cache only for files with
precise change times older than the hash operation's safety window. Inode,
size, mode, modification time and change time guard reuse, so restoring mtime
after an update cannot preserve a stale tool digest. Recent/coarse-time files
are hashed directly, and memo records are checksummed.

Imports are resolved on every invocation; frontend queries reuse exact parsed
sources and semantic results as described below. IR generation still runs;
a proven application hit skips LLVM verification/optimization, object generation
and linking. An unsuccessful frontend or backend clears the success marker.

Build metadata and immutable executable artifacts live beside the output in
`.<output-name>.tinox-build/`. Ignore `*.tinox-build/` in existing projects (new
projects include this rule). Remove that directory to clear its cache when no
builds or runs use it. Interrupted staging directories can be removed the same
way. Different outputs have separate locks; publication uses private staging
files and atomic renames. Immutable artifacts keep concurrent runs isolated
from later rebuilds. Old artifacts remain until the directory is cleared.

`tinox run` stores its output under the project's ignored `.tinox/run/`, or the
user's Tinox cache for standalone files, and preserves the program's exit status
and working directory.

Measure the no-op path against a warm-runtime build with
`python3 scripts/benchmark-build-cache.py --compiler target/release/tinox`.
`--debug` selects debug application builds; `--methods 500` adds a synthetic
backend workload. The script verifies each program's output and records raw
samples, host/compiler identity and whether a warm application hit occurred.

### Incremental frontend queries

`tinox build`, `run`, `dev` and `check` reuse immutable parsed sources, per-file
visibility/typechecking results, and whole-program expression types. The LSP
uses the same parse and per-file semantic query layer for buffer diagnostics
and imported project sources. Resolution remains owned by each frontend;
this does not change the LSP's existing import-resolution capabilities.

- `TINOX_FRONTEND_CACHE=0` disables all frontend reuse, independently of the
  application/runtime caches. Use this to compare a clean frontend run.
- `TINOX_FRONTEND_CACHE_DIR=/path` selects storage (default:
  `$XDG_CACHE_HOME/tinox/frontend-v1` or `~/.cache/tinox/frontend-v1`).
- `TINOX_CACHE_TRACE=1` reports parse, visibility and program query hits/misses.
- `TINOX_TIMINGS=1` reports parsing, import resolution, per-file visibility,
  typechecking, annotation processing and build backend milliseconds, including
  failed queries. Import/visibility/total scopes include nested stages; their
  timings must not be added together.

Parsed sources are keyed by content and a schema fingerprint covering frontend
compiler sources, manifests, dependency lockfile, target and Rust toolchain.
They contain no resolved imports, generated accessors or assigned node IDs.
Each caller gets its own AST, stamps canonical file origins, checks file and
namespace rules, resolves explicit/implied/sibling imports afresh, and assigns
IDs after merging in the existing declaration order.

Per-file semantic queries depend on the canonical file identity and the exact
ordered ASTs of that file and its resolved prelude closure, including annotations
and spans. Editing a leaf reuses unrelated parsing and per-file checking;
changing a signature or annotation invalidates every dependent query. Dependency
bodies also invalidate users conservatively: `this` usage currently affects
method-call semantics, so a signature-only approximation would be unsafe.
Whole-program typechecking reruns after graph changes; its cached expression
table is reused only with the *exact same AST and node numbering*. This is
incrementality at file/query boundaries, not incremental LLVM generation or
method-level semantic recomputation.

Directory membership, deletions, ambiguity, namespace checks and import cycles
are never inferred from cached filesystem metadata. Diagnostics are stored as
structured spans/messages and rendered by the caller. Entries are size-limited,
checksummed against their query key and atomically published; missing, damaged
or unwritable caches recompute normally. Independent writers can safely race.
There is no automatic disk eviction yet: remove the selected frontend cache to
reclaim space; abandoned `.tmp-*` files from killed writers can be removed too.

Reproduce the synthetic scale measurements (clean/cold/warm/leaf edits) with
`python3 scripts/benchmark-frontend.py --compiler target/release/tinox`.
Wall-time samples, query counts and nested stage summaries live in
`benchmarks/frontend_cache_results.json`.

Measured medians on the recorded Linux machine (three runs, synthetic 100-statement
leaf methods; milliseconds for `tinox check`):

| Files | Cache disabled | Cold | Warm | One leaf edited |
| --- | ---: | ---: | ---: | ---: |
| 21 | 12.3 | 13.2 | 4.4 | 7.8 |
| 101 | 54.6 | 60.1 | 19.7 | 37.2 |
| 301 | 165.1 | 166.9 | 56.5 | 114.1 |

The 301-file leaf edit reparses one source and reuses 299 independent semantic
queries; the entry and edited leaf recheck. Whole-program checking remains a
material part of edit latency. Cold caching can cost more than a disabled run;
these synthetic timings do not predict full application or service latency.

### Runtime object cache

Builds share runtime C objects under `$XDG_CACHE_HOME/tinox/runtime-v1` (or
`~/.cache/tinox/runtime-v1`). The key includes the preprocessed runtime and
headers, C compiler and tool contents/version/target, effective flags and include
configuration. Linking still runs against the current host libraries.

- `TINOX_CACHE_TRACE=1 tinox build` reports the cache key and hit/miss/bypass.
- `TINOX_RUNTIME_CACHE=0 tinox build` compiles the runtime without caching.
- `TINOX_RUNTIME_CACHE_DIR=/path tinox build` selects a separate cache.
- To clear it, remove the selected cache directory while no builds are running.

ASan, checked, TLS, database and HTTP/3 configurations have separate keys.
Unknown C flags (including plugins, response files and native CPU selection)
bypass caching conservatively. Objects are checksummed; a damaged entry rebuilds.
Per-key kernel locks and atomic publication prevent concurrent or interrupted
builds from exposing partially compiled objects. Interrupted builds can leave
unused `.tmp-*` directories; clearing the cache removes them too.

Measure disabled, cold and warm builds (including output verification) with
`python3 scripts/benchmark-runtime-cache.py --compiler target/release/tinox`.
Use `--debug` for debug application builds. Results include raw timings and host/
compiler metadata; they describe this machine, not a service-readiness guarantee.

### Package Manager

`tinox install`/`tinox add` resolve dependencies declared in a project's
`tinox.toml`, as one `[[dependencies]]` table per dependency:

```toml
[package]
name = "my-project"
version = "0.1.0"

# Coordinate-only: resolved against the default registry
# (https://central.tinox-lang.de) — no url needed.
[[dependencies]]
group = "someorg"
artifactId = "somelib"
version = "1.0.0"

# Explicit-url form still works for anything not published to a registry.
[[dependencies]]
group = "otherorg"
artifactId = "otherlib"
version = "0.3.0"
url = "https://example.com/otherlib.tnx"
```

A coordinate-only dependency (no `url`) resolves against a registry:
`https://central.tinox-lang.de` by default, or a project-configured
`[[repositories]]` entry referenced by `repository = "id"` (e.g. a private/
self-hosted index):

```toml
[[repositories]]
id = "internal"
url = "https://pkg.example.com"

[[dependencies]]
group = "internal.tools"
artifactId = "widgets"
version = "2.0.0"
repository = "internal"
```

`tinox add <group> <artifact> <version> <url>` appends an explicit-url
`[[dependencies]]` table and installs it in one step.

`tinox publish` packs the current project (same archive `tinox package`
builds) and uploads it to a registry — `POST /api/v1/{group}/{artifactId}/
{version}` — resolved the same way as a coordinate-only dependency
(`--repository <id>`, else the default registry). Requires `[package]
group` in `tinox.toml` (alongside the existing `name`/`version`) and a
`TINOX_CENTRAL_ADMIN_KEY` bearer token for the target registry — the
public registry's publish endpoint is admin-key-gated, not yet a general
per-user auth model.

`tinox search <query>` lists every `group:artifactId` (and its latest
version) on a registry whose group or artifactId contains `query`
(case-insensitive).

## Hello World

```tinox
class Main
{
    fnc main() -> Int32
    {
        println("Hello, World!");
        return 0;
    }
}
```

## Syntax Overview

### Variables

```tinox
const x: Int64 = 42;          // immutable
var y: Float64 = 3.14;      // mutable
const name = "Tino";          // type inference
const msg = "Hi ${name}!";    // string interpolation
```

### Namespaces & Static Methods

All functions live inside classes. Static methods (no object needed) use `fnc`,
and are always called with `::` — `Utils::add(3, 4)`. Writing `Utils.add(3, 4)`
is a compile error naming the correct form; `.` stays reserved for calls through
a value (`point.distanceTo(other)`). One spelling per kind of call, so a
codebase can't drift between them:

```tinox
namespace math {
    class Utils {
        fnc add(a: Int64, b: Int64) -> Int64
        {
            return a + b;
        }

        fnc square(x: Int64) -> Int64
        {
            return x * x;
        }
    }
}

class Main
{
    fnc main() -> Int32
    {
        println(Utils::add(3, 4));    // 7
        println(Utils::square(5));    // 25
        return 0;
    }
}
```

Importing the whole namespace or a single class:

```tinox
import math;          // all classes from math
import math.Utils;    // only Utils
```

### Classes & Inheritance

Instance methods use `fn` and have access to `this`. Constructors are defined with `fnc new()`:

```tinox
class Animal
{
    name: String;

    fn speak() -> String
    {
        return "...";
    }
}

class Dog extends Animal
{
    fn speak() -> String
    {
        return "Woof! I am ${this.name}";
    }
}
```

Classes with a constructor:

```tinox
class Point
{
    x: Int64;
    y: Int64;

    fnc new(x: Int64, y: Int64) -> Point
    {
        return Point { x: x, y: y };
    }

    fn distanceTo(other: Point) -> Float64
    {
        const dx = this.x - other.x;
        const dy = this.y - other.y;
        return Math::sqrt((dx * dx + dy * dy).toFloat64());
    }
}

const p = Point::new(3, 4);
```

### Interfaces

```tinox
interface Printable
{
    fn toString() -> String;
}

class Point implements Printable
{
    x: Int64;
    y: Int64;

    fn toString() -> String
    {
        return "(${this.x}, ${this.y})";
    }
}
```

### Enums & Pattern Matching

```tinox
enum Direction
{
    North;
    South;
    East;
    West;
}

namespace nav {
    class DirUtils {
        fnc turn(d: Direction) -> Direction
        {
            match d
            {
                North => return East;
                East  => return South;
                South => return West;
                West  => return North;
            }
        }
    }
}
```

### Generics

```tinox
class Box<T>
{
    value: T;

    fn get() -> T
    {
        return this.value;
    }
}

class Main
{
    fnc identity<T>(x: T) -> T
    {
        return x;
    }

    fnc main() -> Int32
    {
        const b = new Box<Int64>(42);
        println(b.get());
        return 0;
    }
}
```

### Function Types

Functions as values and parameters use `fnc(T1, T2) -> R` as their type:

```tinox
class Main
{
    fnc apply(x: Int64, f: fnc(Int64) -> Int64) -> Int64
    {
        return f(x);
    }

    fnc main() -> Int32
    {
        const doubled = apply(21, n => n * 2);
        println(doubled);   // 42
        return 0;
    }
}
```

### Tuples

```tinox
const point = (10, 20);
println(point.0 + point.1);

const nested = ((1, 2), 3);
println(nested.0.1);        // 2
```

### Arrays & Builtins

```tinox
const arr = [1, 2, 3, 4, 5];
arr.push(6);
println(arr.len());         // 6
println(arr.first());       // 1
println(arr.last());        // 6

const s = "hello";
println(s.toUpper());       // HELLO
println(s.contains("ell")); // true

const parts = "a,b,c".split(",");
println(parts.len());       // 3
println(parts.join(" - ")); // a - b - c
```

### Maps

```tinox
const m = @{"one" => 1, "two" => 2};
m.insert("three", 3);
println(m.get("one"));      // 1
println(m.contains("two")); // true
println(m.len());           // 3
m.remove("one");

// Type-annotated map
const headers: Map<String, String> = Map::new();
headers["Content-Type"] = "application/json";
```

### Ranges & Loops

```tinox
for i in 0..5               // 0, 1, 2, 3, 4 (exclusive)
{
    println(i);
}

for i in 0...5              // 0, 1, 2, 3, 4, 5 (inclusive)
{
    println(i);
}

for ch in "hello"           // character iteration
{
    print(ch);
}
```

### Async / Concurrency

```tinox
class Main
{
    async fnc fetchData(id: Int64) -> Int64
    {
        return id * 2;
    }

    fnc main() -> Int32
    {
        const handle = spawn fetchData(21);
        const result = await handle;      // 42
        println(result);

        const ch = channel;
        send ch -> 99;
        const v = recv ch;
        println(v);
        return 0;
    }
}
```

### File I/O

```tinox
// Writing
const f = open("output.txt", "w");
f.write("Hello Tinox!\n");
f.close();

// Reading (entire contents)
const f = open("output.txt");
const content = f.read();
f.close();
println(content);

// Reading line by line
const f = open("log.txt");
while !f.eof()
{
    const line = f.readLine();
    println(line);
}
f.close();

// Helper functions
println(fileExists("output.txt")); // true
deleteFile("output.txt");
```

Modes: `"r"` (read, default), `"w"` (write), `"a"` (append), `"rb"` / `"wb"` (binary)

### Defer

```tinox
class Main
{
    fnc readFile(path: String) -> String
    {
        const f = open(path);
        defer { f.close(); }   // runs automatically at the end of the function

        return f.read();
    }
}
```

Multiple `defer`s run in reverse order (LIFO):

```tinox
defer { println("3"); }
defer { println("2"); }
defer { println("1"); }
// Output on return: 1, 2, 3
```

### Try / Catch

```tinox
try
{
    riskyOperation();
}
catch e: RuntimeError
{
    println("An error occurred");
}
finally
{
    cleanup();
}
```

`throw` raises an exception:

```tinox
class Main
{
    fnc divide(a: Int64, b: Int64) -> Int64
    {
        if b == 0
        {
            throw "Division by zero";
        }
        return a / b;
    }
}
```

### Annotations

Tinox supports annotations with `@Name` or `@Name(args)` syntax on classes, methods, functions, and fields:

```tinox
class Main
{
    @inline
    fnc fastCalc(x: Int64) -> Int64
    {
        return x * x + 1;
    }

    @deprecated("Use newApi instead")
    fnc oldApi()
    {
        println("legacy");
    }
}
```

The compiler validates annotations (unknown annotations or invalid placement are errors). The following annotations are recognized:

| Annotation     | Targets              | Description                            |
|----------------|-----------------------|-----------------------------------------|
| `@inline`      | Function, Method     | Emits LLVM `alwaysinline`               |
| `@deprecated`  | Function, Method, Class | Warns on use                          |
| `@GET`          | Method               | REST: GET endpoint                      |
| `@POST`         | Method               | REST: POST endpoint                     |
| `@PUT`          | Method               | REST: PUT endpoint                      |
| `@PATCH`        | Method               | REST: PATCH endpoint                    |
| `@DELETE`       | Method               | REST: DELETE endpoint                   |
| `@Path`         | Class, Method        | URL path                                |
| `@Produces`    | Method               | Response content type                   |
| `@Consumes`    | Method               | Expected request content type           |
| `@StatusCode`  | Method               | Default HTTP status code                |
| `@Auth`         | Method, Class        | Authentication ("bearer"/"basic")       |
| `@WebsocketEndpoint("/path"[, port])` | Class    | WebSocket: generates an accept/message loop as `main` |
| `@OnOpen`       | Method               | WebSocket: called on new connection     |
| `@OnMessage`    | Method               | WebSocket: called per text message      |
| `@OnClose`      | Method               | WebSocket: called on connection close   |

Both styles are equivalent:

```tinox
class UserController
{
    @GET("/users")              // path directly in the annotation
    fnc listUsers(...) { ... }
}

class UserController
{
    @GET                        // or separated with @Path
    @Path("/users")
    fnc listUsers(...) { ... }
}
```

### HTTP Server & REST Framework

The standard library includes an HTTP server (`http_server`) and an annotation-driven REST framework. The `mini_http` module is a lightweight in-process counterpart:

```tinox
module mini_http;

class HttpRequest
{
    var method: String;
    var path: String;
    var body: String;
    var headers: Map<String, String>;
    var params: Map<String, String>;

    fnc new(method: String, path: String, ...) -> HttpRequest { ... }
}

class HttpResponse
{
    var statusCode: Int64;
    var headers: Map<String, String>;
    var body: String;

    fnc new() -> HttpResponse { ... }
}

class HttpServer
{
    var port: Int64;

    fnc new(port: Int64) -> HttpServer { ... }

    fn get(path: String, handler: fnc(HttpContext)) -> HttpServer { ... }
    fn post(path: String, handler: fnc(HttpContext)) -> HttpServer { ... }
    fn listen() { ... }
}
```

REST controller with annotations:

```tinox
import mini_http;

class UserController
{
    @GET
    @Path("/users")
    @Produces("application/json")
    @StatusCode(200)
    fnc listUsers(ctx: HttpContext)
    {
        ctx.response.body = "[{\"id\":1,\"name\":\"Alice\"}]";
    }

    @POST
    @Path("/users")
    @Consumes("application/json")
    @StatusCode(201)
    fnc createUser(ctx: HttpContext)
    {
        ctx.response.body = "{\"id\":2}";
    }

    @GET
    @Path("/users/:id")
    fnc getUser(ctx: HttpContext)
    {
        const id: String = ctx.request.params["id"];
        ctx.response.body = "{\"id\":${id}}";
    }

    @DELETE
    @Path("/users/:id")
    @Auth("bearer")
    @StatusCode(204)
    fnc deleteUser(ctx: HttpContext)
    {
        ctx.response.statusCode = 204;
    }
}
```

### WebSocket Server

The standard library includes an RFC 6455 WebSocket server (`websocket`), built on top of the HTTP server's connection-handle layer. v1 is an explicit loop-based API (no lambda handler), serving one connection at a time:

```tinox
import tinox.core.websocket;

const srv: Int64 = WsServer::listen(8790);

while true {
    const conn: Int64 = WsServer::accept(srv);   // includes handshake
    if conn <= 0 { continue; }

    while true {
        const f: WsFrame = Ws::readMessage(conn); // ping/pong + close handled automatically
        if f.opcode == 1 {
            Ws::sendText(conn, "echo: " + Ws::text(f));
            continue;
        }
        break; // close (8), EOF (-1), or protocol error (-2)
    }
    Ws::close(conn);
}
```

Known v1 gaps: no fragmentation, no client, no permessage-deflate.

`wss://` (TLS) is also supported via `WsServer::listenTls(port, certPath, keyPath)` + `WsServer::acceptTls(srv)` (otherwise identical API). OpenSSL is linked by default, no extra flag required (opt out with `TINOX_TLS=0` if OpenSSL isn't available):

```tinox
const srv = WsServer::listenTls(8791, "cert.pem", "key.pem");
const conn = WsServer::acceptTls(srv);   // includes TLS + WS handshake
```

Alternatively, annotation-driven (`@WebsocketEndpoint`/`@OnOpen`/`@OnMessage`/`@OnClose`): the compiler generates the entire loop as `main` — no handshake/readMessage code needed. Only applies when the file has no `main` of its own and contains exactly one `@WebsocketEndpoint` class (more than one is a compile error):

```tinox
import tinox.core.websocket;

@WebsocketEndpoint("/echo", 8793)
class EchoEndpoint
{
    @OnMessage
    fn onMessage(conn: Int64, msg: String) -> Nothing
    {
        Ws::sendText(conn, "echo: " + msg);
    }
}
```

### AMQP-0-9-1 Client

The standard library includes an AMQP-0-9-1 **client** (`amqp091`, no broker) for message queue brokers such as RabbitMQ. v1 is an explicit publish/consume API (no lambda handler), with a fixed channel per connection. `amqps://` (TLS) is supported:

```tinox
import tinox.core.amqp091;

const conn = AmqpConnection091::connect("127.0.0.1", 5672, "/", "guest", "guest");
const ch = AmqpChannel091::open(conn);
const queueName = ch.declareQueue("my-queue", true, false, false);

var body: List<Int64> = [];
for i in 0..3 { body.push("abc".charCodeAt(i)); }
ch.publish("", queueName, body, "text/plain");

ch.consume(queueName);
const m = ch.nextMessage();       // blocking pull
if m.ok {
    ch.ack(m.deliveryTag);
}
conn.close();
```

`amqps://` (TLS) uses `AmqpConnection091::connectTls(host, port, vhost, user, pass, verify)` instead of `connect` (otherwise identical API). OpenSSL is linked by default, no extra flag required; `verify=true` checks the broker's certificate chain and hostname against the system CA stores, `verify=false` is a deliberate opt-out for self-signed test certificates:

```tinox
const conn = AmqpConnection091::connectTls("broker.example.com", 5671, "/", "guest", "guest", true);
```

Heartbeats (§4.2.7) can be sent on a background thread, same explicit-opt-in pattern as `amqp10` — `conn.heartbeat` is the broker's proposed interval in seconds (from `connection.tune`, informational only until you start sending):

```tinox
conn.startHeartbeat(20000);   // send a heartbeat frame every 20s, in the background
// ...
conn.stopHeartbeat();         // or just conn.close(), which stops it for you
```

Known v1 gaps: no multi-channel, no `exchange.declare` (only the default exchange plus broker-predefined exchanges), no publisher confirms, no annotation-driven consumer API, no auto-reconnect. AMQP 1.0 is a separate, later roadmap phase (different type system) — details and architecture in the [GitHub issues](https://github.com/subnix-work/tinox/issues?q=is%3Aissue+%22AMQP-0-9-1-Client%22) (feature history, marked done there).

### AMQP-1.0 Client

The standard library additionally includes a standalone AMQP-1.0 **client** (`amqp10`, no shared code with `amqp091` — a completely different type system and a three-tier Connection→Session→Link hierarchy with credit-based flow control instead of 0-9-1's Connection→Channel model):

```tinox
import tinox.core.amqp10;

const conn = Amqp10Connection::connect("127.0.0.1", 5672, "guest", "guest");
const session = Amqp10Session::begin(conn);
var sender = Amqp10Link::attach(session, "my-sender", false, "/queues/my-queue");

var body: List<Int64> = [];
for i in 0..3 { body.push("abc".charCodeAt(i)); }
sender.publish(body, "text/plain");
sender.detach();

var receiver = Amqp10Link::attach(session, "my-receiver", true, "/queues/my-queue");
receiver.grantCredit(10);
const m = receiver.nextMessage();       // blocking pull, waits for a transfer
if m.ok {
    receiver.ack(m.deliveryId);
}
conn.close();
```

Beyond the base client, `amqp10` also supports multiple sessions/links per connection, SASL SCRAM-SHA-256 (in addition to PLAIN), delivery states beyond `accepted` (`rejected`/`released`/`modified`), transactions (`txn-id` declare/discharge), link recovery/resumption, and heartbeat/auto-reconnect. Details and architecture in the [GitHub issues](https://github.com/subnix-work/tinox/issues?q=is%3Aissue+%22AMQP-1.0-Client%22) (feature history, marked done there).

An annotation-driven consumer API is also available, analogous to the WebSocket module's `@OnMessage`: the compiler generates the connect/begin/attach/grantCredit/nextMessage/ack loop as `main`. Only valid when the file defines no `main` and has exactly one `@Amqp10Consumer` class:

```tinox
import tinox.core.amqp10;

@Amqp10Consumer("127.0.0.1", 5672, "guest", "guest", "/queues/my-queue")
class MyConsumer
{
    @OnMessage
    fn onMessage(msg: Amqp10Message) -> Nothing
    {
        println(msg.body);
    }
}
```

## Feature Overview

| Feature                       | Status         |
|--------------------------------|----------------|
| Variables (const/var)          | ✅ Done        |
| Namespaces                     | ✅ Done        |
| Classes + inheritance          | ✅ Done        |
| Constructors (`fnc new()`)     | ✅ Done        |
| Static methods (`fnc`)         | ✅ Done        |
| Interfaces + vtable            | ✅ Done        |
| Enums + pattern matching       | ✅ Done        |
| Generics (monomorphization)    | ✅ Done        |
| Tuples                         | ✅ Done        |
| Arrays + builtins              | ✅ Done        |
| String interpolation           | ✅ Done        |
| Ranges (.. / ...)              | ✅ Done        |
| Lambdas / closures             | ✅ Done        |
| Function types (`fnc(T)->R`)   | ✅ Done        |
| Async / spawn / await          | ✅ Done        |
| Channels + select              | ✅ Done        |
| Try / catch / finally          | ✅ Done        |
| `throw` statement              | ✅ Done        |
| Import system                  | ✅ Done        |
| Float32 / Float64               | ✅ Done        |
| Map / dict type                | ✅ Done        |
| `defer` statement              | ✅ Done        |
| Annotations                    | ✅ Done        |
| HTTP server (stdlib)           | ✅ Done        |
| REST framework (stdlib)        | ✅ Done        |
| WebSocket server (stdlib)      | ✅ Done (v1)   |
| AMQP-0-9-1 client (stdlib)     | ✅ Done (v1)   |
| AMQP-1.0 client (stdlib)       | ✅ Done (v1)   |
| LSP (tinox-lsp)                | ✅ Done        |
| Eclipse plugin                 | ✅ Done        |
| File I/O                       | ✅ Done        |
| Formatter (`tinox fmt`)        | ✅ Done        |
| REPL (`tinox repl`)             | ✅ Done        |
| Test runner (`@Test`, `tinox test`) | ✅ Done   |
| Dev mode / hot-reload (`tinox dev`) | ✅ Done   |
| HTML docs (`tinox doc`)        | ✅ Done        |
| Package manager (`tinox install`/`add`/`package`/`publish`/`search`, central registry) | ✅ Done |

## Project Structure

```
tinox/
├── Cargo.toml
├── crates/
│   ├── tinox-common/       # Shared types (Span, Error)
│   ├── tinox-lexer/        # Lexer / tokenizer
│   ├── tinox-parser/       # Parser + AST
│   ├── tinox-typecheck/    # Type checker + annotation processing
│   ├── tinox-codegen/      # LLVM IR code generation
│   ├── tinox-lsp/          # Language Server Protocol
│   ├── tinox/              # CLI binary
│   └── tinox-core/         # Standard library (.tnx modules)
├── examples/               # Example programs (.tnx)
└── runtime/                # C runtime (tinox_alloc, threading, channels)
```

## Standard Library (tinox-core)

50+ modules as `.tnx` files:

| Category     | Modules                                              |
|--------------|-----------------------------------------------------|
| HTTP         | `http_server`, `rest.client`, `rest.server`, `mini_http`, `websocket` |
| Messaging    | `amqp091`, `amqp10`                                  |
| Data         | `json`, `csv`, `xml`, `regex`                        |
| Security     | `crypto`, `jwt`, `bcrypt`                            |
| Collections  | `collections`, `queue`, `stack`, `linkedlist`        |
| System       | `fs`, `io`, `env`, `process`, `os`                   |
| Utilities    | `math`, `mathf`, `string_utils`, `date`, `uuid`      |
| Async        | `cron`, `events`, `pool`, `cache`, `pubsub`          |

## Garbage Collection

The runtime uses the [Boehm GC](https://www.hboehm.info/gc/) in its
default conservative, **stop-the-world, non-generational, non-incremental**
configuration — every collection is a full mark-sweep over the entire
live heap, pausing all threads (`spawn` is real pthreads) simultaneously.
Measured pause times scale with live heap size, not total heap size:
roughly 90 µs at 10k live objects (0.6 MB) up to ~31 ms at 5M live
objects (~300 MB) on the dev machine — see
[`benchmarks/gc_pause_results.md`](benchmarks/gc_pause_results.md) for
full numbers and methodology (`benchmarks/bench_gc_pause/Main.tnx`). No
tuning knobs are currently exposed beyond
`tinox.core.debug.Debug::gcCollect()`/`::memoryUsage()`; if a workload's
live set is large enough for this to matter, that's the number to watch.

## Changelog

See [CHANGELOG.md](CHANGELOG.md) for release notes.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for build instructions, coding conventions, and how to submit changes.

## Security

See [SECURITY.md](SECURITY.md) for how to report a vulnerability.

## License

MIT OR Apache-2.0

For unpublished package development, see [local dependency overrides and stdlib test lanes](docs/local-dependencies.md).
