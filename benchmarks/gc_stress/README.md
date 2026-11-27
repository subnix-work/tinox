# GC stress workloads (Tinox GC vs Boehm)

`run.py` builds `Main.tnx` once per collector (`TINOX_GC=tinox|boehm`), runs each workload in fresh
processes with alternating order, checks that both collectors produce the same checksum and reports
medians of wall time, CPU seconds, peak RSS, collection count and the mutator stalls seen per batch
(log2 histogram, so p99 is an upper bucket bound).

```sh
python3 benchmarks/gc_stress/run.py --runs 3 --scale 0.25 --cpus 0-15 --out /tmp/gc_stress.json
```

Workloads: `churn` (short-lived small objects), `trees` (binary-trees), `liveheap-2M` (2M live
objects plus churn), `mapchurn`, `bigblocks` (large pointer-free strings), `fragment` (mixed sizes,
random survival), `threads-N` (the churn workload on N threads, total work constant).
`results.json` holds the raw samples of the run described in the issue.

## Results

`results-before-723.json` (global heap lock) and `results.json` (thread-owned spans, #723): three
runs per workload on a 32-thread Ryzen 9 9950X3D, workload pinned to CPUs 0-15, medians. Wall time
for the constant-total-work churn workload (Tinox / Boehm):

| Threads | Before | After |
|---:|---:|---:|
| 1 | 1.14 s / 1.00 s | 0.68 s / 0.99 s |
| 2 | 4.0 s / 0.63 s | 0.57 s / 0.65 s |
| 4 | 5.2 s / 0.41 s | 0.65 s / 0.57 s |
| 8 | 9.5 s / 0.65 s | 0.61 s / 0.68 s |
| 16 | 17.9 s / 0.88 s | 0.57 s / 1.14 s |

Known gaps: a 2M-object live heap still shows a collection stall of about 360 ms (Boehm 31-104 ms),
`trees`/`fragment` have 2-4x Boehm's p99 stall, and peak RSS with 16 threads is higher (each thread
keeps partially filled spans).
