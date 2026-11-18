#!/usr/bin/env bash
# Builds fuzz/http1/http1_fuzzer. Requires a clang with libFuzzer support. runtime.c is #included
# into the harness (the targets are static), so there is no separate runtime object.
set -euo pipefail
cd "$(dirname "$0")"

OUT="http1_fuzzer"
clang -fsanitize=fuzzer,address -g -O1 -DTINOX_NO_GC -Dmain=tinox_runtime_unused_main \
    http1_fuzzer.c -o "$OUT" -lm -lpthread -lz -lssl -lcrypto
echo "built fuzz/http1/$OUT"
