#!/usr/bin/env bash
# Reproduces the measurement matrix reported in results.md (#312).
# Usage: benchmarks/service_latency/run_suite.sh <out-dir> [baseline|tuning|all]
# Service pinned to cpus 0-3, load clients to 8-15 (disjoint). Edit SVC_CPUS /
# CLIENT_CPUS for a different machine; the values used are recorded in each
# result JSON's meta block. Needs a built target/release/tinox.
set -euo pipefail
cd "$(dirname "$0")/../.."
OUT="${1:?out dir}"; WHICH="${2:-all}"
SVC_CPUS="${SVC_CPUS:-0-3}"; CLIENT_CPUS="${CLIENT_CPUS:-8-15}"
mkdir -p "$OUT"
B="python3 benchmarks/service_latency/run_bench.py --cpus $SVC_CPUS --client-cpus $CLIENT_CPUS --warmup 10 --cold 5 --duration 30"

run() { local name="$1"; shift; echo "=== $name"; $B "$@" --out "$OUT/$name.json"; }

if [[ "$WHICH" == baseline || "$WHICH" == all ]]; then
  run w1_small_c32        --profile small  --conns 32
  run w1_small_c512       --profile small  --conns 512
  run w2_report200_c32    --profile report --n 200 --conns 32
  run w3_cache300k_c32    --profile cache  --cache-items 300000  --conns 32
  run w3_cache1m_c32      --profile cache  --cache-items 1000000 --conns 32
  # Overload: open loop at ~2x the closed-loop capacity measured for w2 (~23-28k req/s).
  run w2_overload_rate60k --profile report --n 200 --conns 256 --rate 60000
fi

if [[ "$WHICH" == tuning || "$WHICH" == all ]]; then
  # Fixed open-loop rate well below saturation, big live set: tail latency is
  # then dominated by collection pauses, not queueing.
  for rep in 1 2 3; do
  for cfg in "base:" "div1:GC_FREE_SPACE_DIVISOR=1" "div8:GC_FREE_SPACE_DIVISOR=8" \
             "markers1:GC_MARKERS=1" "incr:GC_ENABLE_INCREMENTAL=1"; do
    name="${cfg%%:*}"; envs="${cfg#*:}"
    extra=(); [[ -n "$envs" ]] && extra=(--env "$envs")
    run "tune_cache1m_${name}_r$rep" --profile cache --cache-items 1000000 --conns 64 --rate 40000 \
        --env GC_PRINT_STATS=1 "${extra[@]}"
  done
  done
fi
