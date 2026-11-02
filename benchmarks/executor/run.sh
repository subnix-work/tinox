#!/usr/bin/env bash
# Builds the benchmark and runs each mode REPEATS times, reporting wall time, batch latency and max RSS.
set -euo pipefail
cd "$(dirname "$0")"
TINOX=${TINOX:-../../target/release/tinox}
"$TINOX" build Main.tnx -o /tmp/tinox-executor-bench >/dev/null
JOBS=${JOBS:-60000} BATCH=${BATCH:-256} WORKERS=${WORKERS:-8} REPEATS=${REPEATS:-5}
for mode in spawn executor; do
  for r in $(seq 1 "$REPEATS"); do
    MODE=$mode JOBS=$JOBS BATCH=$BATCH WORKERS=$WORKERS python3 -I -c '
import resource, subprocess
out = subprocess.run(["/tmp/tinox-executor-bench"], capture_output=True, text=True)
print(out.stdout.strip(), out.stderr.strip(), "max_rss_kb=%d" % resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss)
'
  done
done
