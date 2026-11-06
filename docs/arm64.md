# Native ARM64 coverage matrix (#557)

Tinox builds and runs natively on `x86_64` and `aarch64` Linux (`TINOX_TARGET`, `docs/SPEC.md`). Cross-compiling and
running under emulation are not supported targets, and an emulated pass would not count as coverage: a test run
under QEMU can hide ABI, layout, atomics and signal differences. This page says what is verified on real ARM64
hardware, by which lane, and what is not.

## Architecture-sensitive surface (inventory)

Found by reading `runtime/runtime.c`, `runtime/runtime.h` and `crates/tinox-codegen`:

| Area | x86-only assumption? | Notes |
|---|---|---|
| Inline assembly / intrinsics (`asm`, `rdtsc`, `cpuid`, `_mm_*`, `ucontext` register access) | none found | the runtime has no architecture-specific code paths |
| Plain `char` signedness | not assumed | plain `char` is unsigned on aarch64 and signed on x86-64; no `char`-sign dependent comparison compiles to a different result (checked with `-Wtype-limits` / `-Wtautological-*` under `-funsigned-char`) |
| `long double` (JSON range validation) | differs | 80-bit x87 on x86-64, IEEE binary128 (software) on aarch64; used only for comparing numbers, results are the same for any `int64`/`double` input |
| `__int128` (file mtime in nanoseconds) | none | supported by clang on both |
| Boehm GC stop-the-world (`SIGPWR`, thread suspend) and `pthread` roots | yes, by nature | thread-root registration and signal-based suspend are per architecture; covered by the GC stress, foreign-thread and task tests |
| LLVM data layout / triple | per target | `crates/tinox-codegen/src/target.rs`; native-only |
| Varargs / struct passing in generated calls and FFI (`extern fn`) | per ABI | AAPCS64 differs from SysV: covered by the FFI and native-link tests |
| Native libraries (libpq, libsqlite3, libmariadb, OpenSSL, zlib, ngtcp2/nghttp3) | package availability | HTTP/3 needs ngtcp2/nghttp3, not provisioned in the Debian builder (see `docs/docker.md`) |
| Container image execution | per image | Docker images are per-architecture; the builder image builds on an ARM64 host without changes |

## What is verified, and by which lane

`.github/workflows/check.yml`:

| Lane | Runner | Covers |
|---|---|---|
| `arm64` (existing) | `ubuntu-24.04-arm` | target selection, codegen, e2e smoke (`cargo test -p tinox-codegen target`, `--test target_selection --test codegen --test e2e`) |
| `arm64-services` (new) | `ubuntu-24.04-arm` | SQLite and MySQL `DB::raw` and transactions, Postgres raw and `@Transactional`, bounded executor and task scopes, foreign threads / thread roots, FFI, native link, HTTP/1 curl body-limit suite, `docker` source-build image execution. Run through `scripts/check-no-skips.sh`: **a skipped test fails the lane**, so a missing optional tool cannot pass silently |

Not covered on ARM64 (stated, not hidden): TLS and HTTP/3 client suites (`browser_security_curl` may skip its HTTP/3 part, which a mandatory lane cannot allow; HTTP/3 needs ngtcp2/nghttp3), the AMQP and Redis suites that need brokers
(`rabbitmq`, `redis-server`; they are architecture-neutral protocol code and run on x86-64), Kubernetes and the
long-running fuzz/ASan deep checks.

## Status of this verification

* The inventory above is from source reading plus one x86-64 proxy run: the full `e2e`, `boundary`, `stdlib_smoke` and
  `matrix` suites pass with the runtime compiled under `TINOX_CFLAGS=-funsigned-char` (aarch64's `char`). A proxy for one
  hazard class, not a substitute for the hardware.
* The `arm64-services` lane has **run green on real ARM64 hardware** (GitHub `ubuntu-24.04-arm`, `aarch64-unknown-linux-gnu`,
  first recorded on the PR #637 run, 2026-11-06, again on every later run): the database, executor, thread-root, FFI,
  native-link and body-limit suites and the container source-build suite all passed, with `scripts/check-no-skips.sh`
  failing the lane on any skipped test. Published support claims may therefore say: "ARM64: build, codegen, e2e, SQLite,
  MySQL and PostgreSQL (raw and transactional), executor/task scopes, foreign threads, FFI, HTTP/1 limits and container
  image build+run verified natively". The "Not covered" list above still applies (TLS and HTTP/3 clients, broker suites,
  Kubernetes, deep fuzz/ASan).
* An earlier x86-64 proxy run (`TINOX_CFLAGS=-funsigned-char`) covered the `char` signedness hazard before hardware was used;
  it is not a substitute for the lane.
* The lane needs Actions minutes (or a self-hosted ARM runner); with the minutes exhausted (#597, closed as not planned)
  it will not run on GitHub-hosted runners.
