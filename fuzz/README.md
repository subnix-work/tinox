# Fuzzing

Coverage-guided (`libFuzzer`, via clang's `-fsanitize=fuzzer`) fuzz
harnesses for tinox stdlib functions that parse untrusted input. Added for
[#111](https://github.com/subnix-work/tinox/issues/111): the v1.0.2
security hardening pass (issues #86–#108) fixed ~20 bugs almost all in the
same shape — a parser trusted broker/client/file-controlled input without
bounds checks (malformed AMQP frames, an unbounded SASL iteration count, a
JSON parser infinite loop, a ZIP reader trusting a declared uncompressed
size independent of the compressed size). That bug class is exactly what
coverage-guided fuzzing finds automatically instead of via one-off manual
review.

No new tooling is required: `clang`/`clang++` already ship libFuzzer
support as part of compiler-rt, and tinox already requires clang. Verified
against clang 22.1.8.

## Targets

| Target | `fuzz/<target>/` | What it calls | Input bridging |
|---|---|---|---|
| JSON parser | `json/` | `jsonParse(const char*)` directly | none — `jsonParse` already takes a raw buffer |
| ZIP reader | `zip/` | `zipEntryCount(const char*)` → `tinox_zip_parse()` | writes each input to an anonymous `O_TMPFILE`, passes `/proc/self/fd/<n>` as the path (the zip\*() functions are path-based, not buffer-based) |
| HPACK decoder | `hpack/` | `Hpack::decode(List<Int64>, HpackDynTable)` (`crates/tinox-core/hpack/Hpack.tnx`) | `HpackDriver.tnx` imports the real module and adds a one-line `tinoxHpackDecode` wrapper method on `class HpackDriver`; `build.sh` compiles it via the real `tinox build` to LLVM IR, then recompiles that IR with ASan/coverage instrumentation — see "HPACK: calling compiled Tinox code" below |
| AMQP-0-9-1 frame reader | `amqp091/` | `Amqp091::readFrame(Int64)` (`crates/tinox-core/amqp091/Amqp091.tnx`) | `Amqp091Driver.tnx` wraps it same as HPACK; the fuzz bytes arrive via a pre-filled, write-shutdown `socketpair()` instead of a plain buffer — see "AMQP frame readers: calling compiled Tinox code that reads from a socket" below |
| AMQP-1.0 frame reader | `amqp10/` | `Amqp10::readFrame(Int64)` (`crates/tinox-core/amqp10/Amqp10.tnx`) | same socketpair bridging as `amqp091/` |
| HTTP/2 frame reader | `http2/` | `Http2Server::readFrame(Http2Conn)` (`crates/tinox-core/http2_server/Http2Server.tnx`) | `Http2Driver.tnx` wraps it same as AMQP — same socketpair bridging, but `Http2Conn::new(conn)`'s `handle` field can be the socketpair fd directly, since `httpServerReadRawBytes` reads off a raw fd rather than a `TinoxConn*` — see "HTTP/2: frame parsing without a connection" below |
| MessagePack decoder | `msgpack/` | `Msgpack::decode(List<Int64>)` (`crates/tinox-core/msgpack/Msgpack.tnx`) | `MsgpackDriver.tnx` wraps it exactly like HPACK — a pure `(List<Int64>) -> MsgpackValue` function with no socket dependency, same driver-module + recompiled-IR technique, no bridging needed beyond the `TinoxArray` handle itself |
| HTTP/1 framing helpers | `http1/` | `http_request_framing`, `http_chunked_end`, `http_dechunk`, `http_headers_has_chunked_encoding`, `http_header_value`, `http_parse_url` (all `static` in `runtime/runtime.c`) | `http1_fuzzer.c` `#include`s `runtime.c` into its own translation unit (the targets are `static`); first input byte selects the helper, the payload is a heap copy of exactly size+1 bytes with a trailing NUL, mirroring `conn_read_request`, so ASan flags any read past the terminator |
| WebSocket message reader | `websocket/` | `Ws::readMessageGeneric(conn, requireMasked, maskOutgoing)` (`crates/tinox-core-ext/websocket`) | `WsDriver.tnx` wraps it; same socketpair bridge as `http2/`. First input byte's low bit picks the role (server: frames must be masked / client: unmasked), the rest is the raw frame stream. `tinox.toml` uses `[[overrides]]` to fuzz the in-tree source instead of a published version. Covers frame header and length parsing, unmasking, fragment reassembly, control frames, UTF-8 and close-payload validation |

JSON and ZIP call straight into the real `runtime/runtime.c`; HPACK,
AMQP-0-9-1, AMQP-1.0, HTTP/2, and MessagePack call straight into the
real, compiled
`crates/tinox-core/{hpack,amqp091,amqp10,http2_server,msgpack}/*.tnx` —
none of the seven are a copy of the parsing logic, so a fix or a
regression in any of them is picked up automatically.

### HPACK: calling compiled Tinox code, not C

`amqp091`/`amqp10`/`http2_server`/`hpack` are all written in Tinox itself
(`crates/tinox-core/**/*.tnx`), not C — there's no `parse(bytes)`-shaped
function in `runtime.c` to call directly the way JSON/ZIP do. HPACK's
`Hpack::decode` turned out to be the tractable one of the four: it's a pure
`(List<Int64>, HpackDynTable) -> List<HpackHeader>` function with no socket
dependency, unlike AMQP/HTTP2 frame parsing (see "Extending to other
parsers" below).

To fuzz it: `fuzz/hpack/HpackDriver.tnx` imports `tinox.core.hpack` and
adds one wrapper method, `class HpackDriver { fnc tinoxHpackDecode(bytes:
List<Int64>) -> Int64 { ... } }`. `tinox build` compiles this down to
plain LLVM IR with a predictable exported symbol — since issue #149
(mandatory class-qualified functions, no top-level `fn`), that's the
mangled `@HpackDriver_tinoxHpackDecode` (`{ClassName}_{methodName}`, `tinox`'s
static-method mangling convention), `i64* -> i64` (a `List<Int64>` value is
just an `i64*` pointer to the `{len, cap, data}` handle `runtime.c`'s
`TinoxArray` uses). `tinox build` always tries to link a full executable
and fails at that final step (`HpackDriver.tnx` has no `class Main`, so
there's no `tinox_main` for `runtime.c`'s `main()` to call) — that failure
is expected and harmless; `build.sh` only needs the `.ll` it leaves behind
before the failing link. `build.sh` then recompiles that IR with
`-fsanitize=fuzzer-no-link,address` (`tinox build`'s own clang/opt/llc
pipeline adds neither ASan nor coverage instrumentation) and links it with
an ASan-instrumented `runtime.c` (renamed `main`, same as JSON/ZIP) plus
`hpack_fuzzer.cc`, which builds the input `TinoxArray` directly and calls
`HpackDriver_tinoxHpackDecode`.

This generalizes to any other buffer-in/buffer-out Tinox stdlib function,
not just HPACK — the driver-module + recompiled-IR pattern doesn't care
what the function does, only that it takes/returns primitives or handles
with a stable, C-callable ABI.

### AMQP frame readers: calling compiled Tinox code that reads from a socket

`Amqp091::readFrame`/`Amqp10::readFrame` are the "not standalone
functions" case flagged as future work in earlier revisions of this
README: they don't take a buffer, they read directly off a `conn` handle
(a `runtime.c` `TinoxConn*` wrapping a socket fd) via `httpConnReadN`,
which loops on `conn_recv()` until it gets N bytes or hits EOF (checked
in the source, not assumed, before relying on it here). So the driver
can't be a plain `(List<Int64>) -> X` wrapper like `tinoxHpackDecode` —
`Amqp091Driver.tnx`/`Amqp10Driver.tnx` instead wrap
`fnc tinoxAmqpXXXReadFrame(conn: Int64) -> Int64` as a static method of
`class Amqp091Driver`/`class Amqp10Driver`, taking an already-open conn
handle and returning just the `frameType`.

The fuzzer harness (`amqp091_fuzzer.cc`/`amqp10_fuzzer.cc`) builds that
conn handle from a `socketpair(AF_UNIX, SOCK_STREAM, ...)`: write the
whole fuzz input into one end, `shutdown(SHUT_WR)` that end so the kernel
signals EOF once the buffered bytes are drained, then call
`httpConnFromFd()` (a plain `runtime.c` function, called directly from
C++ same as the compiled driver symbol) on the read end and pass the
resulting conn handle to `tinoxAmqpXXXReadFrame`. No feeder thread
needed — the whole input is already sitting in the kernel's socket
buffer before `readFrame` ever calls `conn_recv`, and `httpConnReadN`'s
short-read handling means an input shorter than whatever frame length it
declares terminates the read loop instead of blocking.

One thing this technique requires closing explicitly that the buffer-only
targets don't: `close()` both ends of the socketpair every iteration. The
heap allocations this `-DTINOX_NO_GC` harness makes are leaked on
purpose (see below) — file descriptors are not, and a typical `ulimit -n`
(1024) is exhausted in well under a second of fuzzing otherwise, long
before the memory-based OOM these harnesses otherwise rely on as their
natural restart point.

This socketpair bridging works for any Tinox function whose input arrives
by reading off a conn/fd handle rather than a pre-built buffer — the
driver-module + recompiled-IR part is identical to HPACK's.

### HTTP/2: frame parsing without a connection

Earlier revisions of this README deferred a dedicated HTTP/2 target as
"meaningfully bigger" than the others, reasoning that `Http2Server::
readFrame` is an *instance* method needing a constructed `Http2Server`
(routes, middleware, socket state) plus an `Http2Conn` (streams map,
HPACK dynamic tables), and that driving a realistic amount of the
surrounding connection state machine to reach deeper frame handling
would be real work.

Revisiting it: that's true for exercising the *connection* state
machine (SETTINGS negotiation, HPACK-decoded headers, stream
multiplexing — `Http2Server::handleConnection`, still not fuzzed here),
but it overstated what `readFrame` itself needs. `Http2Server::new(port)`
and `Http2Conn::new(handle)` are both plain struct literals with no I/O
side effects, and `readFrame` only touches `this.readBytes`, a thin
wrapper over `httpServerReadRawBytes`. So `Http2Driver.tnx` builds both
with throwaway values (`Http2Server::new(0)`, `Http2Conn::new(conn)`)
and calls `server.readFrame(c)` — same driver-module + recompiled-IR
technique as HPACK, same socketpair bridging as AMQP, just entered one
layer up (an instance method instead of a free function).

One simplification versus AMQP: `httpServerReadRawBytes(fd, count)`
(`runtime.c`) calls `read()` directly on the given fd, unlike
`httpConnReadN`, which goes through a `TinoxConn*` built via
`httpConnFromFd()`. So `Http2Conn::new(conn)`'s `handle` field can be the
socketpair fd itself — no `httpConnFromFd()` step needed at all.

`readFrame` doesn't check for the RFC 7540 §3.5 connection preface
(`handleConnection` does that one layer up, before ever calling
`readFrame`) — so `fuzz/http2/seeds/` are raw 9-byte-header(+payload)
frames (SETTINGS, PING, HEADERS, DATA, GOAWAY, RST_STREAM, a padded/
prioritized HEADERS, a truncated header, and a frame with a declared
length far exceeding its actual body), not full connections.

## Building and running

```bash
fuzz/json/build.sh    && fuzz/json/json_fuzzer     fuzz/json/corpus/    fuzz/json/seeds/
fuzz/zip/build.sh     && fuzz/zip/zip_fuzzer       fuzz/zip/corpus/     fuzz/zip/seeds/
fuzz/hpack/build.sh   && fuzz/hpack/hpack_fuzzer   fuzz/hpack/corpus/   fuzz/hpack/seeds/
fuzz/amqp091/build.sh && fuzz/amqp091/amqp091_fuzzer fuzz/amqp091/corpus/ fuzz/amqp091/seeds/
fuzz/amqp10/build.sh  && fuzz/amqp10/amqp10_fuzzer  fuzz/amqp10/corpus/  fuzz/amqp10/seeds/
fuzz/http2/build.sh   && fuzz/http2/http2_fuzzer    fuzz/http2/corpus/   fuzz/http2/seeds/
fuzz/msgpack/build.sh && fuzz/msgpack/msgpack_fuzzer fuzz/msgpack/corpus/ fuzz/msgpack/seeds/
```

`corpus/` is the fuzzer's working corpus (gitignored — it grows large and
is reproducible from `seeds/` plus a fuzzing run); `seeds/` is a small,
curated set of valid/edge-case inputs checked into git as a starting
point. Create `corpus/` before the first run (`mkdir -p fuzz/<target>/corpus`);
libFuzzer does not create its output directory for you.

A crashing/hanging/leaking input gets written to the current directory by
default (or under `-artifact_prefix=fuzz/<target>/crashes/`, also
gitignored). Reproduce and debug one with:

```bash
fuzz/json/json_fuzzer fuzz/json/crashes/crash-<hash>
```

### `make fuzz`: all seven targets, CI-wired

`make fuzz` (repo root) builds and briefly runs all seven targets against
their checked-in seeds in one command — `bash fuzz/<t>/build.sh` followed
by a short `-fork=4` run per target, `FUZZ_SECONDS` (default 60) apiece.
It's wired into `.github/workflows/deep-checks.yml` alongside `asan`/
`checked`, so it runs weekly (and on `workflow_dispatch`) with no extra
CI setup — the same clang/llvm install those two targets already need
covers `-fsanitize=fuzzer` too.

Two libFuzzer quirks, both hit and root-caused while wiring this target
up, are worth knowing about before "fixing" what looks like a fuzz
failure but isn't:

- **A non-zero exit code alone does not mean a real finding.** Because
  every harness here builds `runtime.c` with `-DTINOX_NO_GC` (leaks are
  intentional, see below), a `-fork=4` worker will eventually hit
  `-rss_limit_mb` from pure accumulation — no bug required, just enough
  fuzzer iterations. When that happens, libFuzzer's `-fork` driver exits
  non-zero (observed: `71`) for the *whole run*, even with
  `-ignore_ooms=1` passed explicitly (verified: that flag only affects
  whether the coordinator keeps scheduling new jobs after an OOM, not the
  final exit code).
- **`slow-unit-*` artifacts can be a false alarm for the same reason.**
  As a single long-lived worker's never-freed heap grows into the
  hundreds of MB/GB over a run, glibc `malloc`'s own bookkeeping overhead
  grows with it — so an input executed late in the run can take
  measurably longer in wall-clock time than an equally-cheap input
  executed early, without being algorithmically slower at all. Verified
  by replaying several `slow-unit-*` files this surfaced (each well under
  500 bytes) standalone in a **fresh** process: every one parsed in
  ~0.1–0.2s, nothing like a hang.

So `make fuzz` doesn't trust the raw exit code: each target run passes
`-artifact_prefix=fuzz/<t>/artifacts/` (a fresh, empty directory per
run), and afterward the Makefile inspects what actually landed there.
`oom-*` and `slow-unit-*` files are exactly the two harmless cases above
and are logged, not failed. Only a `crash-*`/`timeout-*`/`leak-*` file
(an actual libFuzzer/ASan finding) — or a non-zero exit with *no*
artifact at all to explain it — fails the target. See the extensive
comment directly above the `fuzz:` target in the `Makefile` for the exact
logic.

## All targets run in `-DTINOX_NO_GC` mode — leaks are intentional

Every harness builds `runtime.c` with `-DTINOX_NO_GC` (see each
`build.sh`), the same mode `make asan` uses: plain `malloc`/`calloc`
instead of the Boehm GC, so ASan can see every allocation and no
`GC_INIT()` call is required. As the existing comment on that `#ifdef` in
`runtime.c` says, nothing is ever freed in this mode — that's deliberate,
matching `make asan`'s `ASAN_OPTIONS="detect_leaks=0"` (see `Makefile`).
**Always pass `-detect_leaks=0` to the fuzzer binary** (both as an
`ASAN_OPTIONS` env var and a libFuzzer flag — the flag alone isn't
enough, LeakSanitizer runs as part of the ASan runtime) or every run will
immediately report a "leak" that is not a bug.

Because nothing is freed, RSS grows monotonically over a single-process
run — empirically this hits libFuzzer's default 2048 MB `-rss_limit_mb`
after roughly 230k executions (~15s of wall time on this repo's dev
machine, ~50k exec/s). That OOM is an artifact of the harness, not a
finding. For a run longer than a couple of minutes, either:

- pass a lower `-rss_limit_mb` and treat hitting it as "time to restart,"
  or
- use libFuzzer's `-fork=N` mode, which periodically respawns worker
  processes (reclaiming their memory) while still sharing one corpus —
  the practical way to fuzz for hours unattended.

## What the HPACK target found

Within the first second of running `fuzz/hpack/hpack_fuzzer` against its
seed corpus, it reproduced a crash: a truncated/malformed HPACK header
block (as short as 3 bytes) made `Hpack::decodeInt`/`decodeStr` index past
the end of the input `List<Int64>`, which `tinox_array_get`'s bounds check
turns into `exit(1)` — i.e. the *entire process* exits, not just the one
malformed request. Since `Http2Server.dispatchStream` calls
`Hpack::decode` directly on client-controlled `stream.headerBlock` with no
validation beforehand, this was a remotely-triggerable denial of service
against any Tinox HTTP/2 server: one connection sending a truncated HPACK
literal took down every other connection's process too. Fixed by bounds-
checking `decodeInt`'s entry and `decodeStr`'s consuming loop against
`data.len()` (stopping gracefully instead of reading past the buffer) and
capping `decodeInt`'s continuation-byte loop so a long run of
`0x80`-prefixed bytes can't shift-overflow past 64 bits — see
`crates/tinox-core/hpack/Hpack.tnx` and the `tests/e2e/hpack_truncated_input.tnx`
regression test. Exactly the bug class this issue set out to catch.

## What the AMQP-0-9-1 target found

Within seconds of running `fuzz/amqp091/amqp091_fuzzer`, it hit libFuzzer's
`-rss_limit_mb` OOM abort orders of magnitude faster than the other
targets (tens of thousands of executions instead of the ~230k baseline
above) — not from a memory-safety bug (ASan never flagged an
out-of-bounds/use-after-free access across a 35M+-execution `-fork=4`
run after the fix below), but from a single allocation: a crashing input
made `runtime.c`'s `httpConnReadN` allocate gigabytes across a modest
number of iterations. Root cause: `httpConnReadN(conn, n)` pre-allocated
a buffer sized to the full DECLARED `n` upfront, before confirming that
many bytes actually existed on the connection — a 7-byte AMQP frame
header claiming a size just under `readFrame`'s own 16MB cap triggered a
~128MB array allocation (`n` elements × 8 bytes/`Int64` slot) even when
the connection was closed right after the header, without a single
payload byte sent. Any peer (a malicious/compromised broker against this
AMQP *client*) could repeat that cheaply.

The exact same shape existed a second time, arguably more severely,
outside anything this fuzz target directly exercises: `runtime.c`'s
`httpServerReadRawBytes` (which `Http2Server::readFrame` uses for RFC
7540 frame headers/payloads) pre-allocated a C string buffer sized to a
peer-declared HTTP/2 frame length the same way — server-side, so any
HTTP/2 *client* could trigger it against a Tinox HTTP/2 server, found by
inspection once the `httpConnReadN` pattern was recognized rather than by
a dedicated HTTP/2 fuzz target (see "Extending further" below).

Fixed both by capping the *initial* allocation to the read loop's own
chunk size (4096) and growing it — via `tinox_array_push`'s existing
amortized doubling for `httpConnReadN`, via an equivalent doubling loop
added to `httpServerReadRawBytes` — only as bytes are actually received,
so the worst case is bounded by what a peer actually sends, not by what
it merely claims. Neither function's external behavior/return value
changed (same result, same short-read/EOF semantics), so every caller
across `amqp091`, `amqp10`, and WebSocket (`httpConnReadN`) and HTTP/2
(`httpServerReadRawBytes`) is protected by this one runtime-level fix
with no per-protocol changes needed. See `runtime.c`'s `httpConnReadN`/
`httpServerReadRawBytes` and the `tests/e2e/amqp091_readframe_declared_size.tnx`
/ `tests/e2e/http2_readrawbytes_declared_size.tnx` regression tests.

## What the AMQP-1.0 target found

Nothing beyond the `httpConnReadN` fix above, which already covered it —
`Amqp10::readFrame` uses the same `httpConnReadN` primitive as
`Amqp091::readFrame`, so the fix found via the AMQP-0-9-1 target closed
the identical amplification path here too. 47M+ executions across a
`-fork=4` run turned up no ASan findings.

## What the HTTP/2 target found

Nothing beyond the `httpServerReadRawBytes` fix above, which already
covered it — that fix predates this target (found by inspection once the
`httpConnReadN` amplification pattern was recognized during the
AMQP-0-9-1 investigation, see above) and already caps the exact
allocation path `Http2Server::readFrame` drives. A `-fork=4` run against
the seed corpus turned up only the same harmless `-rss_limit_mb`
OOM every other target here hits eventually under `-DTINOX_NO_GC` — no
ASan findings.

## What the MessagePack target found

Nothing — no ASan findings across a `-fork=4` run against a seed corpus
generated with Python's `msgpack` library (issue #136: the encoder/
decoder were cross-verified against that same real, independent
implementation in both directions before this target was even built,
see that issue's closing comment). `Msgpack::decode`'s own bounds
checks (declared string/array/map lengths checked against actual
remaining bytes before ever looping, the same declared-length-vs-
actual-size shape as the `httpConnReadN`/`httpServerReadRawBytes`
fixes above) already reject a huge declared length instantly instead of
looping/allocating proportionally to it — verified this holds under
fuzzing, not just against the handful of manually-written truncated-
input cases in the e2e test.

## Extending further

`amqp091`/`amqp10`/HPACK/HTTP/2 frame parsing/MessagePack are now
covered; ZIP and JSON were covered from the start (see "HPACK: calling
compiled Tinox code, not C" above for why those three needed different
bridging techniques). What's left, tracked as follow-up in
[#111](https://github.com/subnix-work/tinox/issues/111):

- **The rest of the HTTP/2 connection state machine.** The `http2/`
  target above covers `readFrame` — the frame-header/payload parsing
  layer, same scope as the AMQP targets — but not
  `Http2Server::handleConnection`'s surrounding state (SETTINGS
  negotiation, HPACK-decoded headers accumulating across CONTINUATION
  frames, stream multiplexing/lifecycle). Driving a realistic amount of
  that state machine would need a bigger harness than any target here —
  intentionally left out of this pass per "gezielt statt pauschal fixen"
  (CLAUDE.md).
