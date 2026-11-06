#!/usr/bin/env bash
# Builds fuzz/amqp10/amqp10_fuzzer. Requires a clang with libFuzzer
# support (compiler-rt's fuzzer runtime — the same clang tinox itself
# requires) and a built tinox binary (`cargo build -p tinox` / `--release`).
set -euo pipefail
cd "$(dirname "$0")"

RUNTIME_C="../../runtime/runtime.c"
OUT="amqp10_fuzzer"

TINOX_BIN="../../target/release/tinox"
if [ ! -x "$TINOX_BIN" ]; then
    TINOX_BIN="../../target/debug/tinox"
fi
if [ ! -x "$TINOX_BIN" ]; then
    echo "no tinox binary at target/{release,debug}/tinox — run 'cargo build -p tinox' first" >&2
    exit 1
fi

# Amqp10Driver.tnx imports the real tinox.core.amqp10 module (crates/
# tinox-core-ext/amqp10/*.tnx, an extended-tier stdlib package since the
# core/extended split — see tinox.toml here) and adds a one-line
# tinoxAmqp10ReadFrame(conn: Int64) wrapper. No copy of the frame-parsing
# logic. `tinox install` fetches it (cached under ~/.tinox/repository/
# after the first run). `tinox build` always tries to link a full
# executable and fails here because the driver has no main()/tinox_main —
# that failure is expected and harmless, we only need the driver_out.ll it
# leaves behind before the failing final link step.
"$TINOX_BIN" install >/dev/null
rm -f driver_out.ll driver_out.o driver_out_runtime.o
"$TINOX_BIN" build Main.tnx -o driver_out >/dev/null 2>&1 || true
if [ ! -f driver_out.ll ]; then
    echo "tinox build did not produce driver_out.ll — compile error in Main.tnx/Amqp10Driver.tnx?" >&2
    "$TINOX_BIN" build Main.tnx -o driver_out
    exit 1
fi

clang -c driver_out.ll -o driver_instrumented.o \
    -fsanitize=fuzzer-no-link,address -g -O1

clang -c "$RUNTIME_C" -o runtime_fuzz.o \
    -DTINOX_NO_GC -Dmain=tinox_runtime_unused_main \
    -fsanitize=fuzzer-no-link,address -g -O1

clang++ -fsanitize=fuzzer,address amqp10_fuzzer.cc driver_instrumented.o runtime_fuzz.o \
    -o "$OUT" -lm -lpthread -lz

rm -f driver_out driver_out.ll driver_out.o driver_out_runtime.o driver_instrumented.o runtime_fuzz.o
echo "built fuzz/amqp10/$OUT"
