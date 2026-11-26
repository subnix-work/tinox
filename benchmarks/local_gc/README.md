# Tinox garbage collector (#712)

Native builds use the Tinox collector by default on Linux x86-64 and ARM64 with glibc.
To verify the default, run from a checkout with the native Tinox toolchain:

```sh
cargo build --release --bin tinox
target/release/tinox build tests/e2e/gc_telemetry/Main.tnx -o /tmp/gc-demo
ldd /tmp/gc-demo
/tmp/gc-demo
```

The selector applies at compile time. Rebuild with `TINOX_GC=boehm tinox build`
to switch to Boehm. The Tinox-GC executable uses libc for block storage and has no dependency
on libgc. The runtime flags and link inputs participate in the build cache.
The REPL, checked builds (the registry of object addresses is a side table that does not
depend on the allocator) and the pinned Docker source builder use the Tinox GC too. Only the
no-GC sanitizer mode (`TINOX_NO_GC`) has no collector by design; an explicit Tinox-GC
request is rejected there.

## Heap and roots

* Objects stay at a fixed address until explicitly freed, resized or collected.
  New payloads are zeroed and aligned to max_align_t. Runtime realloc copies
  the common prefix. Ordinary payloads are conservatively scanned. Known text
  buffers (including concatenation, numeric formatting and JSON output) are
  pointer-free. Resizing a mixed typed object falls back to scanning all slots.
  Uncollectable executor queues are traced until explicitly released.
* Objects up to 512 bytes use aligned 64 KiB slabs with a power-of-two slot
  size and a common field mask. An address lookup hashes its slab base and
  computes its slot from the offset. The marker checks both neighbours at
  an exact boundary. Free slots are reused and empty slabs are released.
  Larger and permanent allocations use the sorted index. Both paths use
  an explicit mark stack outside the scanned roots.
* For class literals and `new` with a known layout of at most 64 slots, the
  compiler can pass a field mask. It omits checked floating-point, Boolean
  and narrow integer fields. Full-width integer slots can carry native handles;
  they and unknown or synthetic fields are traced. Other allocations use
  the conservative path unless the runtime knows the payload is pointer-free.
* An aligned machine word that points to the start, interior or one-past end
  of a payload retains it. This includes references represented as integers.
  Conservative false positives can retain garbage. Unreachable cycles are
  swept. Before the next allocation, the runtime collects after at least
  max(1 MiB, previous surviving payload bytes) of allocation since the last
  collection. Debug::gcCollect() forces a collection.
* The initial thread initializes the collector on its first managed allocation
  or at runtime startup. Eager component constructors can allocate before main.
  Roots include the executable's writable data and BSS, its ELF TLS block
  on each registered thread, active pthread stacks and saved registers.
  The collecting thread captures its context before entering the marker.
  Runtime thread creation pins the start argument until the child registers.
  Foreign pthreads call tinox_thread_attach before touching the heap and
  balance nested attachments with tinox_thread_detach. A pthread exit
  destructor removes the thread record before its stack or TLS can disappear.
* The collector owns SIGUSR2. It interrupts registered mutators, including
  ones blocked in a syscall, and parks them on a futex. The kernel signal
  frame contains the interrupted registers. The collector waits for every
  acknowledgement before scanning. Scratch allocations precede suspension;
  freeing dead blocks follows resumption. A missing acknowledgement fails
  the process. Applications must preserve the signal disposition and mask.
* A native function may keep an ordinary aligned managed pointer live on
  its registered stack across allocation and collection. Executable globals
  and TLS also work for native sources linked into the executable. A library
  retaining a pointer solely in its own globals, dynamic-library TLS, an
  unmanaged malloc block, packed/encoded state or a kernel data structure
  must arrange an independent scanned owner for the entire borrow. Custom
  stacks, fork followed by managed execution, and native use of Boehm's API
  are outside this prototype's contract.

## Counters

Debug::gcAllocatedBytes() counts cumulative requested payload bytes (minimum
one per allocation, resize counts the new block). Debug::gcHeapBytes(),
Debug::memoryUsage() and Debug::gcUsedEstimateBytes() report the sum of
managed payload bytes still owned, including unswept garbage. The prototype's
payload counters exclude unused slab capacity. Debug::gcFreeBytes() continues
to return zero; it does not measure reusable slots. The collection
counter includes explicit and automatic collections. Experimental native
symbol `tinox_gc_swept_bytes()` counts payload bytes reclaimed by tracing,
excluding explicit free and resize. Allocator metadata, libc arenas and OS
pages contribute to RSS separately. Boehm's heap and allocation counters use
its own size classes and reservation semantics.

## Verification and measurements

```sh
cargo test --release -p tinox --test local_gc
TINOX_GC=tinox cargo test --release -p tinox --test foreign_threads --test http_server_gc_stress_curl
python3 benchmarks/local_gc/compare.py target/release/tinox /tmp/gc-comparison.json
```

The native root fixture holds a one-MiB block through an interior TLS pointer
while the thread blocks in a pipe read, then leaves a two-block cycle behind
on thread exit. It checks both the surviving contents and the number of swept
bytes. The Tinox fixture keeps captured list elements reachable through a
closure on a worker. The suite also checks the linked libraries. The HTTP
workload exercises registered epoll workers under allocation pressure (#140);
repeated collection after thread exit exercises TLS removal (#290).

## Original prototype at 69e8df1b

The original comparison reuses the 10k and 50k cases from the existing pause benchmark
and the existing annotation-driven /report/200 route. Each variant gets four
fresh processes in alternating order, the same two-CPU affinity and a 1 GiB
address-space limit. Each pause result averages ten forced collections. Each
service process has two workers, serves 30 warmup requests and then 1,000
measured requests over two concurrent clients. The Python clients validate
all 200 returned items in every response. Reported throughput includes client
validation time. CPU time is from /proc with clock-tick granularity. Results
are medians across processes on a shared host. [Raw samples](results.json)

| Metric | Boehm | Tinox |
| --- | ---: | ---: |
| Mean full collection call at 10k, µs | 130 | 914 |
| Mean full collection call at 50k, µs | 228.5 | 2,971 |
| HTTP p99, ms | 0.715 | 3.541 |
| Validated requests/s | 5,101 | 3,482 |
| Service CPU seconds per 1,000 requests | 0.075 | 0.225 |
| Peak service RSS, KiB | 9,892 | 16,910 |

## First slab collector at 8c1afcd5

The initial 50k profile spent about 0.8 ms preparing the sorted index and
1.7 ms marking on a typical forced collection. The small-object path now
avoids that sort, and string payloads no longer enter the mark stack. The
allocator also checks a kernel-cleared fork cookie on Linux kernels that
support it, retaining the getpid fallback. A smaller minimum allocation
budget limits the amount of garbage accumulated between cheap collections.

Six fresh processes per workload and collector covered every ordering of
Boehm, the original Tinox compiler/runtime and the optimized version. The
same CPU affinity, address-space limit, checked outputs and HTTP workload
apply as above. The third pause case holds 500,000 live entries. Values
below are medians; CPU readings have clock-tick granularity.
[Per-run pause averages, individual HTTP latencies and binary/source hashes](optimized-results.json.gz)

| Metric | Original Tinox | Optimized Tinox | Boehm |
| --- | ---: | ---: | ---: |
| Full collection at 10k, µs | 903 | 46 | 132.5 |
| Full collection at 50k, µs | 4,428.5 | 166.5 | 239 |
| Full collection at 500k, µs | 36,218.5 | 1,543.5 | 1,406 |
| HTTP p99, ms | 2.497 | 0.586 | 0.594 |
| Validated requests/s | 3,727 | 5,264 | 5,341 |
| Service CPU seconds per 1,000 requests | 0.210 | 0.090 | 0.075 |
| Peak service RSS, KiB | 16,744 | 9,828 | 9,906 |

To reproduce the three-way comparison with a compiler and matching runtime
built from revision `69e8df1b`:

```sh
python3 benchmarks/local_gc/compare.py target/release/tinox /tmp/optimized.json.gz \
  --baseline-compiler /path/to/baseline/target/release/tinox \
  --baseline-runtime /path/to/baseline/runtime/runtime.c --larger-live-set
```

**Decision at 8c1afcd5.** Keep the collector opt-in. It substantially improves both
workloads over the original prototype. It completes the two smaller forced
collections faster than Boehm and the 500k collection more slowly. HTTP p99
ranges overlap. These workloads do not establish performance for long-lived
heterogeneous heaps, sustained overload or larger worker counts.

## Marking ordered and shuffled references at d9cd6ef3

Profiling the first slab version (8c1afcd5) at 500k entries attributed about
1.39 ms to marking and 0.20 ms to sweeping. The next version stores slot
metadata outside the payload slabs so a scattered trace can access it on
fewer pages. References into the same slab share a directory lookup.
Consecutive references to pointer-free slots can be marked in a single
run. The marker only checks the preceding slot for a one-past reference if
the slab has ever held a payload as wide as its slot. It reserves work-stack
space only for objects that can contain outgoing references.

The same six-order comparison now includes deterministic Fisher–Yates shuffles
of the list. Every shuffled string is compared with its expected contents
after timing. The ordered case measures locality typical of a list built by
appending new objects. The shuffled case visits the same objects in a different
order. Values are process medians. Each pause sample averages ten full
collections. [Raw samples and hashes](locality-results.json.gz)

| Workload | Previous Tinox (8c1afcd5) | Tinox at d9cd6ef3 | Boehm |
| --- | ---: | ---: | ---: |
| Ordered 10k, µs | 45.5 | 26.5 | 128 |
| Ordered 50k, µs | 164.5 | 65 | 237.5 |
| Ordered 500k, µs | 1,501 | 468.5 | 1,402 |
| Shuffled 10k, µs | 62.5 | 38.5 | 149 |
| Shuffled 50k, µs | 322 | 133.5 | 320 |
| Shuffled 500k, µs | 4,606 | 1,870.5 | 2,328 |
| HTTP p99, ms | 0.529 | 0.526 | 0.511 |
| Validated requests/s | 5,735 | 5,620 | 5,927 |
| Service CPU seconds per 1,000 requests | 0.090 | 0.090 | 0.070 |
| Peak service RSS, KiB | 9,982 | 8,874 | 9,910 |

At 500k, the current collector completes an ordered collection in 33% of
Boehm's time and a shuffled collection in 80%. HTTP p99 ranges overlap across
all three versions. Boehm used less service CPU and had the highest median
throughput. Tinox was opt-in at the time of this measurement. To reproduce, use the compiler and runtime
from 8c1afcd5 as the baseline:

```sh
python3 benchmarks/local_gc/compare.py target/release/tinox /tmp/locality.json.gz \
  --baseline-compiler /path/to/baseline/target/release/tinox \
  --baseline-runtime /path/to/baseline/runtime/runtime.c --larger-live-set --shuffle-live-set
```

## Reducing allocation during JSON serialization and sweeping whole slabs

The Tinox build of a generated `@JsonSerializable` method borrows a builder
from the current stack frame. Each nested serializer gets a separate frame.
Short results are copied from a 128-byte scratch buffer to an exactly sized,
pointer-free allocation. When the builder grows beyond that scratch space,
it moves the output to managed storage and returns that buffer directly.
The native builder API still accepts its existing heap-backed handles.

The collector now reads each slab's packed size and mark metadata before
sweeping. It clears marks in bulk when every occupied slot survives and
returns an entirely dead slab directly to libc. Only mixed slabs need to
link newly dead slots for reuse. The native regression fixture checks the
heap and swept-byte counters across dead slabs and verifies serializer
results across the scratch boundary and after explicit collections.

Six fresh processes per workload and collector cover every execution order.
Each HTTP process serves 6,000 measured requests via two clients. Every
response is checked. CPU time is normalized to 1,000 requests. Allocation
figures use each collector's own counter semantics as described above.
[Raw trials, individual HTTP timings and runtime hashes](serializer-sweep-results.json.gz)

| Workload (process medians) | Previous Tinox (d9cd6ef3) | Updated Tinox | Boehm |
| --- | ---: | ---: | ---: |
| Ordered 500k strings, µs per collection | 429.5 | 370.5 | 1,394 |
| Shuffled 500k strings, µs per collection | 1,848 | 1,788.5 | 2,384 |
| Allocated bytes per validated request | 107,283 | 63,433 | 127,217 |
| CPU seconds per 1,000 validated requests | 0.0958 | 0.0775 | 0.0725 |
| Validated requests/s | 5,781 | 6,117 | 5,994 |
| HTTP p99, ms | 0.519 | 0.487 | 0.536 |
| Peak service RSS, KiB | 8,924 | 9,250 | 9,856 |

In this workload, Tinox allocated 41% fewer bytes and used 19% less CPU
than its previous revision. Peak RSS rose by 326 KiB. The throughput
ranges for updated Tinox and Boehm overlap. Tinox was opt-in at the time of this measurement.

```sh
python3 benchmarks/local_gc/compare.py target/release/tinox /tmp/serializer-sweep.json.gz \
  --baseline-compiler /path/to/d9cd6ef3/target/release/tinox \
  --baseline-runtime /path/to/d9cd6ef3/runtime/runtime.c \
  --larger-live-set --shuffle-live-set --service-requests 6000
```
