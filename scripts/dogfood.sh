#!/usr/bin/env bash
# Dogfood gate (TESTPLAN Phase 3): build examples/ + benchmarks/ (with
# smoke runs where deterministic) and build + run jgrep-tinox's tests.
# Invoked via `make dogfood`; expects a fresh target/release/tinox.
#
# All jobs are independent of each other (each its own tinox process, its
# own output path) and therefore run in parallel in the background; only
# collecting/printing the results happens afterward, sequentially, in the
# original, stable order. jgrep-tinox's `tinox test` writes PID-scoped
# temp files (.tinox_test_{pid}_{n}, see main.rs), so multiple concurrent
# runs in the same checkout are safe. Before: ~4:45min, almost purely
# sequential on a 32-core machine.
set -uo pipefail
cd "$(dirname "$0")/.."

TINOX="$PWD/target/release/tinox"
DOGFOOD_DIR="${DOGFOOD_DIR:-../jgrep-tinox}"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/out" "$TMP/status" "$TMP/log"

FAIL=0
step() { printf '  %-44s' "$1"; }
ok()   { echo "OK"; }
bad()  { echo "FAIL"; FAIL=1; }

# Sanitizes a label into a safe filename fragment for per-job status/log/output files.
job_id() { echo "$1" | tr -c 'A-Za-z0-9' '_'; }

# Launches "$@" in the background; exit code -> $TMP/status/<id>, combined
# output -> $TMP/log/<id>.
run_job() {
    local id="$1"; shift
    ( "$@" >"$TMP/log/$id" 2>&1; echo $? >"$TMP/status/$id" ) &
}

# Prints the step label followed by OK/FAIL, based on a previously run_job'd id.
# On FAIL, also dumps the job's captured stdout/stderr (from run_job's
# $TMP/log/<id>) so the real underlying error is visible in the CI log
# instead of just "FAIL" -- previously the only way to see it was to
# rerun the failing command by hand, which isn't possible on a CI runner
# after the fact (issue #179: this swallowed the actual jgrep-tinox
# build.sh error on every CI run for days).
report() {
    step "$1"
    if [ "$(cat "$TMP/status/$2" 2>/dev/null)" = "0" ]; then
        ok
    else
        bad
        echo "  --- $1 output ($TMP/log/$2) ---"
        sed 's/^/    /' "$TMP/log/$2" 2>/dev/null
        echo "  --- end $1 output ---"
    fi
}

# Launches a build+run+compare-stdout job in the background (same check as
# the old sequential `smoke()`: exact stdout match, not just exit code).
# Writes 0/1 to $TMP/status/<id> itself instead of using run_job, since the
# pass/fail decision here is the string comparison, not a command's exit code.
run_smoke_job() { # id file expected
    local id="$1" file="$2" expected="$3"
    (
        if ! "$TINOX" build "$file" -o "$TMP/out/$id" >"$TMP/log/$id" 2>&1; then
            echo 1 >"$TMP/status/$id"
            exit
        fi
        out=$(cd "$TMP/out" && timeout 10 "./$id" 2>&1)
        if [ "$out" = "$expected" ]; then
            echo 0 >"$TMP/status/$id"
        else
            { echo "expected: $expected"; echo "actual:   $out"; } >>"$TMP/log/$id"
            echo 1 >"$TMP/status/$id"
        fi
    ) &
}

GOOD_EXAMPLES=(
    examples/examples/Main.tnx
    examples/GreetCommand.tnx
    examples/simple_test/Main.tnx
    examples/vtable_dispatch/Main.tnx
    examples/rest_minimal/Main.tnx
    examples/rest_auto/Main.tnx
    examples/modules/main_example/Main.tnx
    examples/modules/multi_import_example/Main.tnx
    examples/interface_extends/Main.tnx
    examples/rest_with_mini/Main.tnx
)
# Core/extended stdlib split (see CLAUDE.md): any example importing an
# extended-tier module (e.g. tinox.core.http_server) needs its declared
# dependency actually installed before it'll build -- `tinox install` walks
# up from its own cwd to find that example's tinox.toml (unaffected by
# `find_project_root_from`'s build-time walk-up-from-the-FILE's-directory
# behavior, since install genuinely is meant to be cwd-based -- see its own
# doc comment in pm.rs). Sequential, not backgrounded like the builds below:
# each install is a few seconds at most (a warm ~/.tinox/repository/ cache
# skips the network entirely, see install_dep's cache-hit check), and every
# build below needs its own example's install to have already landed.
for f in "${GOOD_EXAMPLES[@]}"; do
    dir="$(dirname "$f")"
    if [ -f "$dir/tinox.toml" ]; then
        if ! (cd "$dir" && "$TINOX" install) >"$TMP/log/$(job_id "install_$f")" 2>&1; then
            echo "warning: tinox install failed for $dir (see $TMP/log/$(job_id "install_$f"))" >&2
        fi
    fi
done

for f in "${GOOD_EXAMPLES[@]}"; do
    run_job "$(job_id "build_$f")" "$TINOX" build "$f" -o "$TMP/out/$(job_id "build_$f")"
done

run_smoke_job "$(job_id smoke_simple)" examples/simple_test/Main.tnx ""
run_smoke_job "$(job_id smoke_vtable)" examples/vtable_dispatch/Main.tnx "$(printf '5\n10\n42')"
run_smoke_job "$(job_id smoke_modules)" examples/modules/main_example/Main.tnx "$(printf '7\n12')"
run_smoke_job "$(job_id smoke_multiimport)" examples/modules/multi_import_example/Main.tnx "$(printf '25\n30')"
run_smoke_job "$(job_id smoke_ifaceext)" examples/interface_extends/Main.tnx "42"

run_job "$(job_id mini_http_check)" "$TINOX" check examples/mini_http/HttpServer.tnx

for f in benchmarks/*/Main.tnx; do
    run_job "$(job_id "bench_$f")" "$TINOX" build "$f" -o "$TMP/out/$(job_id "bench_$f")"
done

if [ -d "$DOGFOOD_DIR" ]; then
    run_job "$(job_id jgrep_build)" bash -c "cd '$DOGFOOD_DIR' && PATH='$(dirname "$TINOX")':\"\$PATH\" bash build.sh"
    # Discover test-entry files by content (`@Test` annotation), not a filename
    # pattern: one-type-per-file split jgrep-tinox's *_test.tnx files into
    # PascalCase `<Name>Test.tnx` + `<Name>Helper.tnx` pairs, and a stale
    # filename glob here would silently match zero files — `tinox test` on a
    # nonexistent path prints an error but still exits 0 (0 tests, 0 failed),
    # so a wrong glob here would report this whole step "OK" while testing
    # nothing (found the hard way once already).
    # 600s per file, not 180s: `tinox test` compiles+links+runs a fresh
    # binary PER @Test method (no shared binary), and the largest jgrep-tinox
    # test files have 40-60 tests each (~78s/~44s locally on a fast 32-core
    # box). 180s was tight enough to time out on GitHub Actions' much slower
    # shared runners (observed CI failure 2026-07-26 on an unrelated commit,
    # reproduced as a timing issue, not a real test regression).
    jgrep_test_files=$(grep -l "@Test" "$DOGFOOD_DIR"/tests/*.tnx 2>/dev/null)
    for t in $jgrep_test_files; do
        run_job "$(job_id "jgrep_test_$t")" bash -c "cd '$DOGFOOD_DIR' && PATH='$(dirname "$TINOX")':\"\$PATH\" timeout 600 tinox test '$t'"
    done
fi

wait

echo "== Dogfood: building examples =="
for f in "${GOOD_EXAMPLES[@]}"; do
    report "$f" "$(job_id "build_$f")"
done

echo "== Dogfood: example smoke runs =="
report examples/simple_test/Main.tnx "$(job_id smoke_simple)"
report examples/vtable_dispatch/Main.tnx "$(job_id smoke_vtable)"
report examples/modules/main_example/Main.tnx "$(job_id smoke_modules)"
report examples/modules/multi_import_example/Main.tnx "$(job_id smoke_multiimport)"
report examples/interface_extends/Main.tnx "$(job_id smoke_ifaceext)"

echo "== Dogfood: typechecking library examples =="
report "examples/mini_http/HttpServer.tnx (check)" "$(job_id mini_http_check)"

echo "== Dogfood: compiling benchmarks =="
for f in benchmarks/*/Main.tnx; do
    report "$f" "$(job_id "bench_$f")"
done

echo "== Dogfood: jgrep-tinox (${DOGFOOD_DIR}) =="
if [ -d "$DOGFOOD_DIR" ]; then
    report "build.sh" "$(job_id jgrep_build)"
    for t in $jgrep_test_files; do
        report "$(basename "$t")" "$(job_id "jgrep_test_$t")"
    done
else
    echo "  skipped ($DOGFOOD_DIR not found)"
fi

if [ "$FAIL" -ne 0 ]; then
    echo
    echo "Dogfood FAILED — details: rerun the command by hand."
    exit 1
fi
echo
echo "Dogfood OK"
