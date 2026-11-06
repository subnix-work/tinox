#!/usr/bin/env bash
# Builds fuzz/http2/http2_fuzzer. Requires a clang with libFuzzer
# support (compiler-rt's fuzzer runtime — the same clang tinox itself
# requires) and a built tinox binary (`cargo build -p tinox` / `--release`).
set -euo pipefail
cd "$(dirname "$0")"

RUNTIME_C="../../runtime/runtime.c"
OUT="http2_fuzzer"

TINOX_BIN="../../target/release/tinox"
if [ ! -x "$TINOX_BIN" ]; then
    TINOX_BIN="../../target/debug/tinox"
fi
if [ ! -x "$TINOX_BIN" ]; then
    echo "no tinox binary at target/{release,debug}/tinox — run 'cargo build -p tinox' first" >&2
    exit 1
fi

# Http2Driver.tnx imports the real tinox.core.http2_server module (crates/
# tinox-core-ext/http2_server/*.tnx, an extended-tier stdlib package since
# the core/extended split — see tinox.toml here) and adds a one-line
# tinoxHttp2ReadFrame(conn: Int64) wrapper. No copy of the frame-parsing
# logic. `tinox install` fetches it and its own transitive deps (hpack,
# http_server — see http2_server's own tinox.toml), cached under
# ~/.tinox/repository/ after the first run. `tinox build` always tries to
# link a full executable and fails here because the driver has no
# main()/tinox_main — that failure is expected and harmless, we only need
# the driver_out.ll it leaves behind before the failing final link step.
"$TINOX_BIN" install >/dev/null
rm -f driver_out.ll driver_out.o driver_out_runtime.o
"$TINOX_BIN" build Main.tnx -o driver_out >/dev/null 2>&1 || true
if [ ! -f driver_out.ll ]; then
    echo "tinox build did not produce driver_out.ll — compile error in Main.tnx/Http2Driver.tnx?" >&2
    "$TINOX_BIN" build Main.tnx -o driver_out
    exit 1
fi

# The mandatory `class Main` makes the IR define @main and tinox_main; rename @main so it
# cannot clash with libFuzzer's own main() (the harness never calls it).
sed -i 's/@main(/@tinox_driver_unused_main(/' driver_out.ll

# Recompile the emitted IR with ASan + libFuzzer coverage instrumentation —
# tinox build's own clang/opt/llc pipeline adds neither.
clang -c driver_out.ll -o driver_instrumented.o \
    -fsanitize=fuzzer-no-link,address -g -O1

# Same runtime.c recipe as fuzz/json, fuzz/zip, fuzz/hpack, fuzz/amqp091 —
# see their build.sh for the full rationale of each flag. Needs
# httpServerReadRawBytes, which lives in runtime.c same as every other
# target here.
clang -c "$RUNTIME_C" -o runtime_fuzz.o \
    -DTINOX_NO_GC -Dmain=tinox_runtime_unused_main \
    -fsanitize=fuzzer-no-link,address -g -O1

clang++ -fsanitize=fuzzer,address http2_fuzzer.cc driver_instrumented.o runtime_fuzz.o \
    -o "$OUT" -lm -lpthread -lz

rm -f driver_out driver_out.ll driver_out.o driver_out_runtime.o driver_instrumented.o runtime_fuzz.o
echo "built fuzz/http2/$OUT"
