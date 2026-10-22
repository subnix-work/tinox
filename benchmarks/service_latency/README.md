# Service tail-latency / memory benchmark (#312)

`benchmarks/gc_pause_results.md` (#116) answers *"how long is one forced full
Boehm collection at live-heap size N?"*. It says nothing about what a request
sees, how fast a service allocates, or what RSS does under a container limit.
This directory measures the service-level side. The two are complementary;
the GC-pause numbers are unchanged and still reproduce with
`tinox run benchmarks/bench_gc_pause/Main.tnx`.

## Pieces

| File | Role |
|---|---|
| `Main.tnx`, `BenchController.tnx`, `Item.tnx`, `Report.tnx` | The service under test: an annotation-driven REST controller (`@GET`/`@POST`, `@PathParam`, auto-serialize) with the workload routes below. |
| `loadgen.py` | Independent load client (pure-Python stdlib, raw keep-alive HTTP/1.1, multi-process). Closed loop, or open loop with latency charged from the *intended* send time (avoids coordinated omission). |
| `run_bench.py` | One measurement run: build, start (optionally pinned/wrapped/with Boehm env), startup-to-readiness, cold phase, warm-up, fixed-duration steady phase; samples RSS, peak RSS, thread count and Boehm heap from outside; writes one JSON with raw latencies, samples, exact command lines and machine metadata. |
| `summarize.py` | Markdown table over a directory of result JSONs; `--strip DIR` writes slim copies without the per-request latency arrays (what is checked in under `results/`). |
| `run_suite.sh` | The matrix reported in `results.md`. |
| `results.md` | Results, machine description and the conclusions that are (and are not) supported. |

## Workload profiles

| Profile | Routes | What it stresses |
|---|---|---|
| W1 `small` | `GET /small` | Stateless small JSON; per-request garbage is tiny. Baseline for server overhead. |
| W2 `report` | `GET /report/:n` | Allocation-heavy: builds `n` `Item` objects and serializes them per request. |
| W3 `cache` | `POST /cache/fill/:n` (once, before load), `GET /cache/:id`, `POST /cache/:id/churn` | Large long-lived live set (`n` items in an `@ApplicationComponent`) while churn replaces slots, i.e. old objects die and new ones are allocated into a big heap. |
| W4 connections | any profile with `--conns 512` (or more) | Many concurrent connections. |
| Overload | any profile with `--rate` above capacity | Open loop above capacity: errors/timeouts and tail latency are reported together. |

`GET /stats` returns `Debug::memoryUsage()` (Boehm heap size) and is sampled
by the harness twice a second. The cache is filled by a single request before
load because the server handles requests on several real threads and a
`List` has no internal locking; load only reads and overwrites existing slots.

## Running

```
cargo build --release
python3 benchmarks/service_latency/run_bench.py --profile report --n 200 \
    --conns 32 --duration 30 --warmup 10 --cpus 0-3 --client-cpus 8-15 \
    --out /tmp/svc_bench/report.json
benchmarks/service_latency/run_suite.sh /tmp/svc_bench all   # full matrix
```

Useful flags: `--rate R` (open loop, total req/s), `--env K=V` (set a variable
for the service, e.g. Boehm's `GC_FREE_SPACE_DIVISOR`), `--env GC_PRINT_STATS=1`
(adds a collection/allocation summary from the Boehm log to the JSON — this
prints on every collection, so keep it out of runs whose latency you intend
to quote on their own), `--wrapper CMD` (a launcher prefix that must `exec`
the service, e.g. `docker run --rm --network host --memory 256m --cpus 2 -v ...`
or a `systemd-run --scope -p MemoryMax=...` / cgroup wrapper), with
`--mem-limit-mib` recording the limit the wrapper enforces. **A memory limit is
only enforced if the wrapper enforces it**; without `--wrapper` the result's
`meta.memory_limit_enforced` is `false`.

The compiled port is baked in at build time through `TINOX_PORT` (the harness
sets it from `--port`).

## Reading the JSON

`steady[]` holds one entry per concurrently driven route, with `ok`, `rps`,
`http_error`/`timeout`/`conn_error` counts, `p50_ms`…`max_ms` and the raw
`latencies_ms` (re-derive any percentile). `cold[]` is the first seconds after
readiness (JIT-less, but caches, heap growth and thread start are cold).
`samples[]` is the RSS/threads/Boehm-heap time series of the steady phase.
