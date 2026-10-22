# GC pause-time benchmark (#116)

`benchmarks/bench_gc_pause.tnx` measures Boehm GC stop-the-world pause
time as a function of live heap size: build up N live objects (a
`List<String>` kept reachable through the measurement, each string long
enough to force a real heap allocation rather than a small-int/interned
fast path), force 10 back-to-back full collections with
`Debug::gcCollect()`, and average using `tinox_clock_nanos()` (the
`tinox.core.metrics.Stopwatch` builtin clock — sub-millisecond
resolution, unlike `tinox.core.time`'s millisecond clock which rounds
these pauses to 0 at the smaller sizes).

Run it with `tinox run benchmarks/bench_gc_pause.tnx`.

**Scope:** this measures *forced full-collection pause time only*, with no
request load. For request p50/p95/p99, RSS and container-limit behavior
see `benchmarks/service/README.md` (#312).

## Results

Measured on the repo's dev machine (AMD Ryzen 9 9950X3D, 32 logical
cores, 60 GiB RAM, Linux), average of 3 runs — variance between runs was
under 10% at every size:

| Live objects | Live heap | Avg full-collection pause |
|---:|---:|---:|
| 10,000 | 0.6 MB | ~90 µs |
| 50,000 | 2.8 MB | ~270 µs |
| 100,000 | 5.6 MB | ~540 µs |
| 500,000 | 27 MB | ~2.5 ms |
| 1,000,000 | 54 MB | ~5.2 ms |
| 2,000,000 | 108 MB | ~10.6 ms |
| 5,000,000 | 294 MB | ~31.5 ms |

Roughly linear up to ~1M live objects (~5 µs per 1,000 objects), then a
bit worse than linear by 5M/300MB — plausibly cache effects on this
machine as the live set stops fitting in L3, not necessarily
representative of every machine/heap shape.

## What this means for request-handler-style workloads

This runtime has **no generational or incremental collection** — every
collection is a full stop-the-world mark-sweep over the *entire* live
heap (see the `TINOX_NO_GC`/Boehm `#ifdef` in `runtime/runtime.c`; the
GC is used in its default conservative, non-incremental configuration).
So the number that matters for tail latency isn't total heap size, it's
**how large your live set typically is when a collection happens to
land**: a server holding a few MB of live state (connection objects,
in-flight request buffers) pays sub-millisecond pauses per the table
above; one accumulating tens of millions of long-lived objects (a large
in-memory cache, a big session store) pays tens of milliseconds per
collection, stop-the-world, on every request handler thread
simultaneously (`spawn` is real pthreads, and Boehm's stop-the-world
suspend pauses all of them together, not just the allocating thread).

## Tuning knobs currently exposed

None, beyond what's implicit: `tinox.core.debug.Debug::gcCollect()`
(force a collection — used by this benchmark) and `::memoryUsage()`
(current Boehm heap size). Boehm itself supports incremental collection
(`GC_enable_incremental()`) and allocation-pressure tuning
(`GC_set_free_space_divisor`), neither of which this runtime currently
calls or exposes to Tinox programs. Enabling incremental mode would
trade these full-stop pauses for many smaller ones interleaved with
mutator execution — a reasonable follow-up if a workload's live set
grows large enough that the numbers above become a real latency problem,
but it wasn't attempted here (see CLAUDE.md's "gezielt statt pauschal
fixen": this pass measured and documented actual behavior rather than
also changing the collection strategy in the same step).
