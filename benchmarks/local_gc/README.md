# Tinox collector experiment (#712)

The normal build uses Boehm. To try the Tinox collector on Linux/x86-64 with
glibc, run from a checkout with the native Tinox toolchain:

```sh
cargo build --release --bin tinox
TINOX_GC=tinox target/release/tinox build tests/e2e/gc_telemetry/Main.tnx -o /tmp/gc-demo
ldd /tmp/gc-demo
/tmp/gc-demo
```

The selector applies at compile time. `TINOX_GC=boehm` selects the baseline.
The experimental executable uses libc for block storage and has no dependency
on libgc. The runtime flags and link inputs participate in the build cache.
The REPL and pinned Docker source builder support Boehm. The prototype requires
the normal runtime build (debug or release). Checked mode keeps a separate
registry of object addresses, and the no-GC sanitizer mode uses a different
allocator; both combinations are rejected.

## Heap and roots

* Objects stay at a fixed address until explicitly freed, resized or collected.
  New payloads are zeroed and aligned to max_align_t. Runtime realloc copies
  the common prefix and preserves the block's scan policy. Ordinary payloads
  are scanned; atomic byte buffers and duplicated strings contain no pointers.
  Uncollectable executor queues are traced until explicitly released.
* A collection builds a sorted address index and uses an explicit mark stack.
  An aligned machine word that points to the start, interior or one-past end
  of a payload retains it. This includes references represented as integers.
  Conservative false positives can retain garbage. Unreachable cycles are
  swept. Before the next allocation, the runtime collects after at least
  max(4 MiB, previous surviving payload bytes) of allocation since the last
  collection. Debug::gcCollect() forces a collection.
* Roots include the executable's writable data and BSS, its ELF TLS block
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
managed payload bytes still owned, including unswept garbage. The collector
has no managed free list, so Debug::gcFreeBytes() returns zero. The collection
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

The comparison reuses the 10k and 50k cases from the existing pause benchmark
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

**Decision.** Keep Boehm as the default. The Tinox prototype runs the tested
workloads independently, but the larger live set takes about 13 times as
long to collect, and the service uses more CPU and memory. Profile index
construction, conservative pointer lookup and allocator overhead before
expanding architecture or workload support.
