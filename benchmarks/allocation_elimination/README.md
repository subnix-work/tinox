# Evaluating allocation elimination in generated JSON serializers (#709)

**Decision.** Keep the prototype opt-in. It removes one short-lived allocation per
eligible object. The measured saving is about 48 Boehm bytes per object and 7.5%
of the allocation volume of the existing HTTP report workload. Four short runs on
one shared x86-64 machine give insufficient evidence for a general latency or
retained-memory improvement.

## Candidate and correctness contract

The [service profiles](../service_latency/current_results.md) reported 361.78 MiB/s
of GC allocation for serialization. In the report route each application-created
Item escapes through a list, the Report escapes through the HTTP response, and
serialization allocates a result buffer. Independently, the compiler-generated
Item serializer calls jsonBuilderCreate once per object. Its private header holds
the current buffer pointer, length, capacity and comma state. A report containing
200 items thus creates 200 candidate headers even when the application retains
every Item. The focused fixture serializes one pre-existing Item 100,000 times
to isolate this cost from creating Items, lists or HTTP responses.

Set `TINOX_EXPERIMENT_LOCAL_JSON_BUILDER=1` **when compiling** to enable the
prototype. The generated serializer selects a local header only when every
emitted field has a checked String, Int64, Float64 or Bool type. The existing
semantic field lookup supplies those facts. Empty, unresolved and other field
sets take the original path, including Report with its list of Items.

The generated field writer borrows the header only for synchronous runtime
scalar writers. None stores or returns the header, calls user code or throws
a Tinox exception. The native wrapper owns the C struct for the duration of
the call, so LLVM does not encode its size or alignment. The buffer remains a
Boehm allocation rooted by the header on the registered thread's stack while
it grows. Only the buffer escapes on return. Each invocation owns its header,
including calls from multiple service workers. Receivers keep their identity.
Returns, field stores, aliases, captured mutable state, indirect callbacks,
native calls with unknown retention, tasks and channels can all extend an
application object's lifetime. This prototype makes no stack-placement
decision for such objects.

## Reproduce

From the repository root on Linux with the Tinox build toolchain and access
to the locked service benchmark dependencies, run:

```sh
cargo build --release --bin tinox
python3 benchmarks/allocation_elimination/profile.py target/release/tinox /tmp/allocation-709.json
```

The harness stages the existing service fixtures, checks emitted IR for both
the optimized Item and fallback Report, then builds and runs both configurations
in alternating order. The focused case checks a growing buffer containing a
quote, a string returned from another thread and retained through a forced
collection and receiver mutation, and a nested list serialized by the fallback.
Python checks the decoded fields independently. The HTTP case uses the real
annotated controller with two workers. Each fresh process serves ten warmup and
100 measured sequential requests. Each returned report is compared to all 200
expected items. Timings include reading the body, and exclude Python decoding.

## Measurements

Medians over four fresh processes per variant, from the [raw samples](results.json).
The compiler and runtime object cache were warm for the measured forced builds.
Build time includes linking. The shared host was also running project checks.
RSS is a snapshot after the measured requests. GC used is Boehm's estimate after
an explicit collection, with page granularity and possible conservative roots.

| Workload and metric | Baseline | Local header |
| --- | ---: | ---: |
| 100,000 Item serializations, GC bytes (release) | 32,001,344 | 27,200,304 |
| Same, debug | 32,001,560 | 27,200,576 |
| Same, debug with checked runtime | 32,001,344 | 27,200,032 |
| Focused release, GC used estimate after collection | 16 KiB | 14 KiB |
| 100 HTTP reports, GC bytes between stats requests | 12,732,160 | 11,772,504 |
| HTTP process RSS | 9,560 KiB | 9,474 KiB |
| HTTP reserved GC heap | 968 KiB | 968 KiB |
| Median of per-run HTTP median latency | 0.139 ms | 0.132 ms |
| Service application build time | 0.508 s | 0.512 s |

The HTTP delta corresponds to about 9.6 kB per 200-item request, consistent with
the focused result. The sampling requests themselves also allocate. The stable
allocation reduction supports this particular optimization. Default enablement
would require evidence from more applications, sustained concurrent traffic
and other supported architectures.
