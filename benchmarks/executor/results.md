# Executor versus thread-per-task spawn (#551)

`benchmarks/executor/run.sh` builds `Main.tnx` and runs each mode five times. Every run submits batches of 256
trivial jobs (a 200-iteration integer loop), awaits each batch, and totals 60,160 jobs. The `check=` value is a
checksum of all results; it is identical for both modes.

Environment: Arch Linux, 32 logical CPUs, compiler 2.9.0 (release build), `WORKERS=8`.

| mode | wall ms (5 runs) | batch mean / max ms | max RSS (KB) |
|---|---|---|---|
| `spawn` (one pthread per job) | 1112, 1087, 1096, 1090, 986 | 4 / 6 | 28,316-30,332 |
| executor (8 workers) | 62, 127, 90, 63, 60 | 0 / 1-2 | 27,480-29,032 |

Reading: for jobs this small the executor removes thread creation and join per job, which dominates the `spawn`
time (about 18 microseconds per job versus 1-2 microseconds). Memory is unchanged because only 256 jobs are in
flight in either mode. Latency is reported per batch with the 1 ms resolution of `now()`, so it only shows the
order of magnitude. The thread count of a process that has run 3,000 jobs through a 4-worker executor is the idle
runtime baseline plus 4 (`crates/tinox/tests/executor.rs`).

Limits of this measurement: one machine, one job shape, no concurrent load, no I/O-bound jobs. It does not show
how the executor compares for jobs that block for long (those occupy a worker for their whole duration; that is the
case #552 is about).
