# Service tail-latency and memory results (#312)

Scope: **one machine, loopback, the workloads in `README.md`, Boehm GC 8.2.12
as shipped by Arch, compiler 2.4.0 at commit `db1c127` (+ this directory).**
Nothing here is a general latency or memory-limit guarantee; Hello-World-style
numbers (W1) in particular say nothing about services with real live heaps.
The earlier `benchmarks/gc_pause_results.md` (#116) measures one *forced* full
collection per live-heap size and is unchanged; this document measures what
requests and RSS do while the collector runs on its own schedule.

## Setup

| | |
|---|---|
| CPU / RAM | AMD Ryzen 9 9950X3D (16C/32T), 60 GiB |
| OS | Linux 7.2.9-arch1-1 |
| Service | compiled `benchmarks/service_latency`, pinned with `taskset -c 0-3` (4 logical CPUs; Boehm marker threads and ~48 worker/runtime threads share them) |
| Load | `loadgen.py`, Python 3.14.8, 4 processes pinned to CPUs 8-15, keep-alive HTTP/1.1 |
| Phases | readiness poll → cold 5 s → warm-up 10 s → **steady 30 s** (reported) |
| Memory limit | **none enforced** (this host has no docker and no delegated cgroup; `run_bench.py --wrapper` supports a limiting launcher, but no limited run was made — see "Not covered") |

Raw per-run JSON (percentiles, counters, 0.5 s RSS/thread/heap samples,
metadata, exact commands; per-request latency arrays stripped) is in
`results/`. Regenerate everything, including the raw latencies, with
`run_suite.sh`; render the table with `summarize.py`. Run-to-run variance is
real and large for the tail (see tuning section) — single runs below are
labelled as such.

## Baseline (closed loop, one run each)

| Workload | req/s | p50 ms | p95 | p99 | p99.9 | max | failed | peak RSS MiB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| W1 small JSON, 32 conns | 302k | 0.09 | 0.16 | 0.28 | 1.3 | 14.5 | 0 | 14 |
| W1 small JSON, 512 conns | 425k | 1.17 | 1.46 | 1.73 | 2.4 | 8.5 | 0 | 14 |
| W2 `/report/200`, 32 conns | 22.8k | 1.18 | 3.21 | 4.32 | 5.8 | 16.6 | 0 | 17 |
| W3 cache 300k live items (GET) | 267k | 0.05 | 0.08 | 0.11 | 2.8 | 14.5 | 0 | 77 |
| W3 cache 1M live items (GET) | 249k | 0.05 | 0.08 | 0.12 | 2.8 | 27.3 | 0 | 199 |

(W3 mixes GET and churn POST 50/50; the POST rows are within 1-2 % of the GET
rows, see `results/`.) Observations that the data supports:

* With a small live set (W1, W2) the service sits at 14-17 MiB peak RSS and
  the tail is queueing/CPU, not collection.
* With a large live set (W3) p50-p99 are unaffected but p99.9 and max follow the
  collection pause: ~1M live `Item`s (~128 MiB RSS right after fill) give
  stop-the-world pauses of ~30-60 ms in this pinned 4-CPU setup (Boehm's own
  `GC_PRINT_STATS` log, "Complete collection took", max 27-79 ms across the
  tuning runs below). That is larger than #116's ~5 ms per 1M *strings* because
  each `Item` is a struct plus three strings, and the collector competes with
  ~48 runtime threads for 4 CPUs.
* Idle thread count is a constant 48 regardless of load (no per-connection
  threads at 512 connections).
* Cold phase: W2 shows a worse p99 in the first 5 s (5.6 ms vs 4.3 ms steady),
  the others are within noise. The one 47 ms cold max in W3-1M is the first
  full collection after the 1M-item fill.
* Startup to first successful response: 8-25 ms across runs.

## Overload (open loop, offered 60k req/s vs ~23-28k capacity, 256 conns, 5 s client deadline)

One run: 26.4k req/s served, 1,007,490 of ~1.8M offered requests failed
(client-side `expired`: older than the 5 s deadline before they could be
sent), served-request latency ≈ 5.0 s p50-max, i.e. the client-side queue
sits at the deadline. The service stayed up, RSS 18 MiB, no HTTP errors, no
connection errors, no crash. This shows graceful behaviour (no memory
blow-up, throughput holds at capacity); it is *not* a statement about
server-side load shedding — the server has none, the overload is absorbed
by the client queue and kernel socket buffers. (An earlier draft of
`loadgen.py` drained its backlog after the window and reported 100+ s
latencies; the `expired` accounting replaced that.)

## Allocation rate (Boehm log, tuning workload only)

`GC_PRINT_STATS=1` sums to ~712 MB allocated between collections over the
~46 s (cold+warm+steady) of the 40k req/s W3-1M run ≈ 15 MB/s, ≈ 390 B per
request on average over the GET/churn mix (not broken down per route). Not measured for W1/W2 (no stats log was enabled there, to
keep their latency free of logging overhead).

## Tuning experiment (opt-in only; no default changed)

Boehm reads these variables itself at startup; **no runtime or compiler change
was made** — they are passed with `--env`. Workload: W3 with 1M live items,
open loop 40k req/s total (below saturation, so tail = pauses), 64 conns,
3 runs per configuration (fresh process each, 30 s steady). `GC_PRINT_STATS=1`
on in all configurations including base.

| Config | p99 ms (3 runs) | p99.9 ms (3 runs) | max ms (3 runs) | peak RSS MiB | collections |
|---|---|---|---|---:|---:|
| base (defaults) | 3.1 / 1.2 / 1.2 | 45 / 16 / 28 | 63 / 22 / 56 | 198 | 19 |
| `GC_FREE_SPACE_DIVISOR=1` (collect less often) | 1.2 / 1.2 / 1.2 | 1.5 / 54 / 40 | 22 / 82 / 69 | 340 | 8 |
| `GC_FREE_SPACE_DIVISOR=8` (collect more often) | 1.2 / 1.2 / 1.2 | 17 / 17 / 17 | 22 / 22 / 22 | 158 | 42 |
| `GC_MARKERS=1` (single mark thread) | 13 / 1.2 / 2.6 | 64 / 49 / 53 | 74 / 58 / 64 | 195 | 19 |
| `GC_ENABLE_INCREMENTAL=1` | 31 / 29 / 42 | 64 / 63 / 129 | 76 / 74 / 151 | 280 | n/a (log differs) |

Reading it conservatively:

* **Incremental mode is worse here** (p99 ~30-40 ms vs 1.2 ms) and costs
  ~40 % more RSS; do not enable it. (Its cost with the runtime's per-thread
  roots/`SIGPWR` handling is untested beyond the stress test below.)
* **Single mark thread is worse or equal**; keep the default parallel marking.
* **`DIVISOR=1`** raises RSS by ~70 % and halves the number of collections,
  but its tail is bimodal (one run excellent, two bad) — the pause that lands
  is not shorter, only rarer. No consistent win → rejected.
* **`DIVISOR=8`** is the only configuration with a repeatable shape: ~20 % less
  peak RSS, p99.9 stable at ~17 ms (vs 16-45 ms for base), max ~22 ms (vs
  22-63 ms), at the price of 2.2× more collections (which would cost throughput on
  allocation-bound workloads; W2 was not re-run with it). The sample is 3
  runs on one machine and one workload; **it is a candidate worth a wider
  evaluation, not a recommendation to change the default**, and the default
  stays unchanged.

### Correctness / stress check of the tuning variables

With each variable set in the environment of the test process (inherited by the
compiled binaries), `cargo test --release -p tinox --test http_server_gc_stress_curl`
(the GC stress test guarding #140/#226/#290) passes for `GC_FREE_SPACE_DIVISOR=8`,
`GC_FREE_SPACE_DIVISOR=1`, `GC_ENABLE_INCREMENTAL=1` and `GC_MARKERS=1`.
The full `e2e` suite with `GC_FREE_SPACE_DIVISOR=8` passed in 2 of 3 attempts:
the first attempt had `e2e_shard_2` fail (not diagnosed — I did not capture
that run's log, it passed when re-run alone and in both later full runs, and
the unmodified-environment run also passed). It may be a pre-existing flake or
an effect of the setting under load; this was not established, which is one more
reason `DIVISOR=8` stays a candidate only.

## Not covered / open

* **Container memory limits.** No limited run was possible on this host
  (no docker, no cgroup delegation). Before anyone quotes an RSS-under-limit
  figure, run `run_bench.py --wrapper '<launcher that execs the service>'
  --mem-limit-mib N`. Note Boehm sizes its heap from its own growth policy, not
  from the cgroup limit, so behaviour at the limit (OOM kill vs. back-pressure)
  is untested.
* Live-heap counter: only the Boehm heap size (`Debug::memoryUsage`) is
  sampled; there is no "live bytes after last collection" or allocation-rate
  API in the runtime (the allocation rate above comes from the stats log).
  Adding one would be a runtime change and was left out of this measurement-first step.
* No copy/allocation profiling pass or buffer-reuse change was made; W2's per-request
  allocation (build `n` items, serialize, copy into the response) is the obvious
  first target for that follow-up, and this harness is the before/after instrument.
* Latency is loopback with a Python client; absolute numbers will differ with a
  real network and a faster client.
