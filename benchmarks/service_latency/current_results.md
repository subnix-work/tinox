# Current service measurements (#312)

Measured 2026-11-01 on the current compiler 2.8.0 checkout. Every primary run used
a fresh local Docker image/process with a verified 2-CPU cgroup quota and 512 MiB
memory limit (64 MiB for the deliberate OOM case), with swap bounded to the same
total. Effective ancestor limits and Docker configuration are retained together.
The desktop machine was shared with background applications and other checks;
neither machine isolation nor universal latency/memory guarantees are claimed.

p99 is conditional on successful responses and includes open-loop scheduling delay.
Offered failures include HTTP errors, connection errors, timeouts and client-expired
requests; they must not be interpreted as server-side rejection counts. The table
shows the mean and range of per-run p99 values, with all failed runs retained.

| Suite / profile | Runs | Steady p99 mean [range], ms | Peak RSS, MiB | Sample-window GC allocation, MiB/s | Steady failed/offered | Recovery ok/offered | Limits verified |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| matrix/large-live-set | 3 | 2.94 [1.20, 4.64] | 196.10 | 7.62 | 0/90000 | 3000/3000 | True |
| matrix/serialization | 3 | 1.31 [1.29, 1.34] | 9.43 | 361.78 | 0/90000 | 3000/3000 | True |
| matrix/small | 3 | 1.16 [1.10, 1.27] | 8.98 | 7.68 | 0/90000 | 3000/3000 | True |
| sustained/large-live-set | 1 | 1.21 [1.21, 1.21] | 195.96 | 1.20 | 0/600000 | 1000/1000 | True |
| dependencies/db-pool | 3 | 101.56 [101.12, 102.25] | 12.19 | 0.02 | 0/3600 | 0/156 | True |
| dependencies/slow-upstream | 3 | 102.96 [102.69, 103.26] | 9.31 | 0.24 | 0/3600 | 0/156 | True |
| dependencies/tls | 3 | 3.11 [2.87, 3.57] | 13.32 | 1.00 | 0/3600 | 3000/3000 | True |
| oom/large-live-set | 1 | n/a | n/a | n/a | 0/0 | 0/0 | True |

## Evidence and interpretation

- Raw per-request latencies, samples, process/thread/fd counts, admission queue counters,
  cgroup memory/CPU events, startup readiness, overload, recovery and exit/OOM state
  are retained as individually compressed files under `results/2026-11-01/`.
  `manifest.json` hashes both raw and compressed bytes. Native curl cross-checks
  validate the service response independently of the Python load driver.
- The frozen matrix has three repetitions per workload; the large live-set follow-up
  holds one million entries through a 600-second steady phase. The separate
  dependency matrix repeats TLS, a real HTTP upstream delayed by 50 ms, and a real
  PostgreSQL pool of two connections (eight HTTP workers). Independent pg_stat_activity
  samples retain observed active database queries; they are not a general DB queue gauge.
- Admission queue counters are the runtime counters returned by tinox_limits_json,
  not every internal worker/backend queue. Sampling can be delayed when the HTTP
  observation endpoint itself saturates; absent samples are not zero usage.
- GC allocation rates cover the sampled window, including cold/warm/steady/overload/
  recovery traffic and observation overhead. They exclude allocations inside native
  libraries. Prefill and exact heap/counter snapshots remain in the raw artifacts.
- Compact payload copy/allocation profiling and the narrow gzip optimization are
  measured separately in `../compact_bytes/results/2026-11-01.json`: five rotated-order
  before/after runs, independent decompression and per-process RSS. Copy boundaries
  are source-inspected; no hardware copy counter or zero-copy guarantee is inferred.
- The 64-MiB case intentionally fails during large-live-set prefill. Docker OOMKilled
  and exit 137 are retained. An OOM is not presented as a successful load result.
- Exploratory earlier artifacts remain marked separately. Earlier TLS results used
  a client that incorrectly reused Connection: close sockets; those error rates are
  invalid as service evidence. Some earlier daemon checks encountered a full root
  filesystem. They are not silently replaced with successful results.
- Boehm defaults remain unchanged. These results do not establish that GC tuning,
  serialization rewrites or a different collector would improve the service.

Observed maximum active PostgreSQL sleep queries in dependencies/db-pool: 2.
Recorded failure in oom/large-live-set: Remote end closed connection without response.

dependencies/db-pool returned no successful responses in its configured 10-second recovery windows at reduced offered load. This bounds the observation; eventual recovery time was not measured and is not inferred to be infinite. Blocking-route isolation and outbound deadline/pooling work remain tracked by #552, #525 and #526.

dependencies/slow-upstream returned no successful responses in its configured 10-second recovery windows at reduced offered load. This bounds the observation; eventual recovery time was not measured and is not inferred to be infinite. Blocking-route isolation and outbound deadline/pooling work remain tracked by #552, #525 and #526.
