# Service tail-latency and memory benchmark (#312)

`benchmarks/gc_pause_results.md` (#116) measures forced full-collection
pauses vs. live heap size in isolation. It says nothing about request
p95/p99, RSS, or behavior under a container memory limit. This directory
adds reproducible *service* workloads for that, as the evidence step before
any GC tuning (incremental mode, `GC_set_free_space_divisor`, ...).

**Status: harness only. No results are recorded here yet.** Numbers must be
produced on a real machine with the commands below and committed together
with their raw JSON; nothing in this repo may claim a latency or
memory-limit guarantee from Hello World-style runs.

## Layout

- `app/` — one annotation-driven Tinox service (`BenchController`) with
  all workload endpoints, plus `/stats` (Boehm heap size via
  `Debug::memoryUsage()`).
- `bench_service.py` — driver (stdlib-only Python). Builds `app/` with a
  free `TINOX_PORT`, starts it, measures readiness, runs a fixed-duration
  load phase, samples RSS / thread count / GC heap every second, writes raw
  per-request latencies + machine metadata + tinox version/commit as JSON.

## Profiles

| Profile | Request | Default connections | What it stresses |
|---|---|---:|---|
| `stateless_json` | `GET /hello` | 16 | per-request overhead, tiny response |
| `serialize_heavy` | `GET /items/200` | 16 | allocation + JSON serialization (200 objects/request) |
| `long_lived_state` | `GET /hello` after `--fill N` | 16 | collection cost with a large live set (N cached strings) |
| `concurrent_conn` | `GET /hello` | 256 | many keep-alive connections / thread count |

## Running

```sh
cargo build --release
# 30 s steady state after 5 s warm-up (both configurable)
benchmarks/service/bench_service.py stateless_json --out /tmp/stateless.json
benchmarks/service/bench_service.py serialize_heavy --out /tmp/ser.json
benchmarks/service/bench_service.py long_lived_state --fill 1000000 --out /tmp/state.json
benchmarks/service/bench_service.py concurrent_conn --connections 1024 --out /tmp/conn.json   # overload/error behavior

# Under documented CPU/memory limits (systemd --user cgroup scope, exec'd in
# place so the sampled PID is the service itself):
benchmarks/service/bench_service.py long_lived_state --fill 2000000 \
    --limits 'MemoryMax=256M CPUQuota=200%' --out /tmp/state_256m.json
```

The summary printed to stdout reports p50/p95/p99/max, request rate, error
counts (I/O errors and non-200 statuses), peak/final RSS, peak thread count,
peak GC heap, readiness time and whether the process was still alive at the
end (an OOM kill under `MemoryMax` shows up as errors + `alive_at_end:
false`). Always report latency and memory together, and quote the machine
metadata block from the JSON.

## Methodology notes / known limits

- **Cold vs. warm:** `ready_s` (process start to first 200) and
  `cold_first_requests` are recorded separately from the steady-state phase;
  the warm-up phase is discarded.
- **Load client:** the Python client is independent of Tinox but is
  GIL-bound and will itself saturate before a fast server does. Treat its
  absolute RPS as a lower bound and cross-check tail latency with a native
  client (`wrk -t4 -c16 -d30s --latency`, `oha`) against the same port where
  possible (keep the service running by building `app/` manually with
  `TINOX_PORT=…`).
- **Closed loop:** each connection sends its next request after the
  previous response, so latency under overload is subject to coordinated
  omission. Use a fixed-rate client for rigorous overload curves.
- **Allocation rate is not measured yet:** the runtime exposes only the heap
  size (`memoryUsage()`), not Boehm's total-bytes-allocated counter.
  Exposing it is a candidate follow-up before attributing tails to
  allocation volume.
- **Shared state:** `BenchController.cache` is unsynchronized by design;
  `--fill` uses a single connection. Concurrency between worker threads on
  that list is not a supported benchmark scenario.
- **Next steps (not done here):** profile copies/allocations in the
  serialize path and evaluate buffer reuse / compact byte payloads; only
  then evaluate opt-in Boehm tuning, with thread-root, foreign-allocation
  and signal-safety checks (#140, #226, #290) and defaults left unchanged
  unless the measurements support a change.
