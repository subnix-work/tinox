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

## Parallel marking (#726)

A collection spent 330-510 ms marking the 2M-object live heap (about 9M objects, ~55 ns each, one
cache miss per object) and only ~40 ms sweeping. Three changes, measured on the same machine
(`results-after-726.json`, full scale, three runs, CPUs 0-15):

- The mark loop prefetches: popped objects wait in a 16-entry ring, and the slot metadata of an
  object's children is fetched before they are looked up.
- The span index had 1024 buckets and chains of ~6 descriptors for a 370 MiB heap; it has 32768.
  Together with the prefetching this halves a single-threaded mark (500 -> 255 ms).
- Heaps of 32 MiB and more are marked by up to 4 threads (`TINOX_GC_MARKERS=N` overrides, 1 = mark
  alone; more than 4 gave no further gain on this machine). The helpers are created before the world
  is stopped, because a parked mutator may hold a libc lock that `pthread_create` needs. Mark bits
  are set atomically, work is shared through the collector's mark stack in chunks, and big
  conservative ranges are split so every participant gets a share.

| liveheap-2M | Before | After | Boehm |
|---|---:|---:|---:|
| wall | 2772 ms | 1521 ms | 1317 ms |
| max stall | 523 ms | 166 ms | 149 ms |
| peak RSS | 667 MiB | 583 MiB | 479 MiB |

Known gaps: `trees`/`fragment` have 2-4x Boehm's max stall (small heaps are still marked by one
thread), and peak RSS with 16 threads is higher (each thread keeps partially filled spans).
