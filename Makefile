# One entry point for everything (TESTPLAN Phase 0.1):
# Rule: no compiler commit without a green `make check`.
# `make install-hooks` activates the pre-push gate.

DOGFOOD_DIR ?= ../jgrep-tinox
export DOGFOOD_DIR

.PHONY: check test e2e dogfood install-hooks asan checked clippy fuzz grammar-sync

check: clippy test dogfood grammar-sync

# Lint gate: 0 warnings across the whole workspace (bins + tests).
# Deliberate exceptions are #[allow(...)] with a justification in the code.
clippy:
	cargo clippy --release --workspace --all-targets -- -D warnings

# Rust unit tests (lexer, parser, typecheck, codegen, …) -- unrestricted
# `cargo test` also auto-discovers and runs every crates/tinox/tests/*.rs
# integration test binary, including e2e.rs/matrix.rs/boundary.rs/
# stdlib_smoke.rs below, so `check` does NOT also depend on `e2e` (found
# via `make check` itself taking measurably longer than it should: those
# four suites were running twice, once here and once via the `e2e` target
# -- an accidental side effect of e2e.rs originally being a standalone
# `bash tests/runtime_tests.sh` script with no overlap with `test`, then
# migrated to a plain cargo integration test without updating `check`'s
# dependency list -- not a deliberate flakiness-catching double-run).
test:
	cargo test --release

# End-to-end: golden tests (tests/e2e/*.tnx) + generated context matrix
# + boundary cases + stdlib smoke gate (use every tinox-core module once).
# Already covered by `test` above -- kept as its own target for quickly
# iterating on just these suites during development, not part of `check`.
e2e:
	cargo test --release -p tinox --test e2e --test matrix --test boundary --test stdlib_smoke

# Dogfood: build examples/ + benchmarks/, build and test jgrep/ygrep
# (jgrep checkout configurable via DOGFOOD_DIR)
dogfood:
	cargo build --release
	bash scripts/dogfood.sh

# Sanitizer run (TESTPLAN 2.3): e2e suite with the AddressSanitizer runtime.
# Plain malloc instead of Boehm-GC (-DTINOX_NO_GC), so ASan sees every
# allocation; leaks are deliberate here (detect_leaks=0), the target is
# overflows/UAF. Not part of `make check` — run weekly/before releases.
asan:
	cargo build --release
	TINOX_CFLAGS="-fsanitize=address -g -DTINOX_NO_GC" \
	ASAN_OPTIONS="detect_leaks=0" \
	cargo test --release -p tinox --test e2e --test boundary

# Checked run (TESTPLAN Phase 4): e2e + boundary suite with the
# heap-kind registry (-DTINOX_CHECKED, see `tinox build --checked`).
# Array/map runtime functions check their pointers — dispatch bugs
# abort loudly. Green = no false positives across the whole test set.
checked:
	cargo build --release
	TINOX_CFLAGS="-DTINOX_CHECKED" \
	cargo test --release -p tinox --test e2e --test boundary

# Fuzz regression check (issue #111, see fuzz/README.md for the full
# rationale/architecture): builds every libFuzzer harness in fuzz/*/ and
# runs each briefly against its checked-in seed corpus. -fork=N so the
# harness's own well-documented per-worker OOM (every target builds
# runtime.c with -DTINOX_NO_GC, same as `make asan` — nothing is ever
# freed, so a single worker's RSS grows unbounded) gets recycled instead
# of stopping the whole campaign. FUZZ_SECONDS is per-target, not total —
# override for a longer local run, e.g. `make fuzz FUZZ_SECONDS=300`.
#
# Found while wiring this up: libFuzzer's -fork driver still exits
# non-zero (observed: 71) whenever ANY worker hit -rss_limit_mb during the
# run, even with -ignore_ooms=1 (verified: doesn't change the exit code,
# only governs whether the coordinator keeps scheduling new jobs after an
# OOM). For these harnesses that is the EXPECTED steady state, not a
# finding — e.g. fuzz/zip calls zipEntryCount/zipEntryName/zipEntrySize,
# each re-reading + re-parsing the whole input from scratch, so a single
# input burns 3 allocations per exec; at 100k+ exec/s that alone reaches
# -rss_limit_mb=2048 well inside a 60s run under -DTINOX_NO_GC. The same
# never-freed heap also makes libFuzzer misreport slow-unit-* "hangs": as
# a worker's heap grows into the hundreds of MB/GB over the run, glibc
# malloc's own bookkeeping overhead grows with it, so late-run inputs take
# longer in wall-clock terms than early-run ones even though they're not
# algorithmically slower -- verified by replaying several slow-unit-*
# artifacts standalone in a *fresh* process (tiny <500-byte inputs, each
# parses in ~0.1-0.17s, nothing like the outlier that got them flagged).
# So: trust the artifacts, not the raw exit code. -fork writes one file
# per stop-condition to -artifact_prefix, named by kind (oom-*,
# slow-unit-*, crash-*, timeout-*, leak-*) -- oom-*/slow-unit-* are the
# two harmless cases above; crash-*/timeout-*/leak-* are real
# libFuzzer/ASan findings. So each target gets its own empty
# fuzz/$t/artifacts/ dir per run, and only a crash-*/timeout-*/leak-* file
# there (or a non-zero exit with NO artifact at all to explain it) fails
# `make fuzz` -- a run that exits non-zero with only oom-*/slow-unit-*
# files is logged and treated as a pass.
#
# Found via issue #136 (msgpack): the -DTINOX_NO_GC leak-driven slowdown
# above isn't just "malloc bookkeeping grows with heap size" -- ASan's own
# allocator/redzone tracking also gets measurably slower as the total
# LIVE (never-freed) allocation count grows, and for a target with a
# high allocations-per-exec ratio (msgpack decodes into a tree of several
# heap objects per value, unlike e.g. hpack's flatter List<HpackHeader>)
# that compounds badly: exec/s visibly degrades over a run's lifetime
# even though no *individual* execution is ever slow (confirmed directly:
# -timeout=1 -- 1 SECOND per input -- never fired across repeated runs,
# ruling out a single pathological/hanging input), and worse, libFuzzer's
# own post-OOM shutdown/cleanup path itself gets slow at that allocation
# volume, so the run can keep running well past both -max_total_time and
# the moment its own log prints "libFuzzer: out-of-memory" / "run
# interrupted; exiting". Lowering -rss_limit_mb does NOT reliably fix
# this (tested down to 512MB, still didn't return promptly) since it's
# allocation COUNT, not RSS size, driving the slowdown. So: wrap the
# invocation itself in `timeout` with a grace period beyond
# FUZZ_SECONDS -- if a target doesn't self-terminate in time for ANY
# reason (this one, or a future different one), the outer timeout forces
# it and `status` becomes exactly 124 (coreutils' `timeout` own,
# reserved exit code for "I killed this"). That status gets its own
# branch below, checked BEFORE the generic "no artifact" one: killing a
# worker before it ever crosses the threshold that would make it write
# an oom-*/slow-unit-* artifact is exactly what's expected here (msgpack
# observed: killed with ZERO artifacts on disk, well before the ~10+
# minutes it can take to naturally reach one) -- the ABSENCE of an
# artifact is not suspicious in this specific case, the 124 exit code
# already IS the full explanation, so this must not fall through to the
# generic "non-zero exit with no artifact = fail" check below (confirmed
# by running into exactly that false failure before adding this branch).
# --kill-after gives a short grace window for a plain SIGTERM before
# escalating to SIGKILL, in case a target's own shutdown path is merely
# slow rather than truly stuck.
FUZZ_SECONDS ?= 60
fuzz:
	cargo build --release -p tinox
	@set -e; \
	for t in json zip hpack amqp091 amqp10 http2 msgpack; do \
		echo "=== fuzz/$$t ==="; \
		bash fuzz/$$t/build.sh; \
		mkdir -p fuzz/$$t/corpus; \
		rm -rf fuzz/$$t/artifacts; mkdir -p fuzz/$$t/artifacts; \
		status=0; \
		ASAN_OPTIONS=detect_leaks=0 timeout --kill-after=15 $$(( $(FUZZ_SECONDS) + 60 )) \
			fuzz/$$t/$${t}_fuzzer \
			-fork=4 -max_total_time=$(FUZZ_SECONDS) -rss_limit_mb=2048 -detect_leaks=0 \
			-ignore_ooms=1 -artifact_prefix=fuzz/$$t/artifacts/ \
			fuzz/$$t/corpus/ fuzz/$$t/seeds/ || status=$$?; \
		real_findings=$$(find fuzz/$$t/artifacts -type f ! -name 'oom-*' ! -name 'slow-unit-*' 2>/dev/null); \
		harmless=$$(find fuzz/$$t/artifacts -type f \( -name 'oom-*' -o -name 'slow-unit-*' \) 2>/dev/null | wc -l); \
		if [ -n "$$real_findings" ]; then \
			echo "fuzz/$$t: REAL finding(s), not just OOM/slow-unit recycling:"; \
			echo "$$real_findings"; \
			exit 1; \
		elif [ "$$status" = 124 ]; then \
			echo "fuzz/$$t: hit the outer timeout safety net (didn't self-terminate within FUZZ_SECONDS+60s) -- expected for high-allocation-density targets under -DTINOX_NO_GC+ASan (see comment above), not a finding -- treating as pass"; \
		elif [ "$$status" != 0 ] && [ "$$harmless" = 0 ]; then \
			echo "fuzz/$$t: exited $$status with no artifact to explain it"; \
			exit 1; \
		elif [ "$$status" != 0 ]; then \
			echo "fuzz/$$t: exit $$status but only $$harmless harmless oom-*/slow-unit-* artifact(s) (expected under -DTINOX_NO_GC) -- treating as pass"; \
		fi; \
		rm -rf fuzz/$$t/artifacts; \
	done

# Issue #267: editors/vscode/syntaxes/tinox.tmLanguage.json and
# editors/eclipse/tinox-eclipse/grammars/tinox.tmLanguage.json are
# deliberately maintained as byte-identical duplicates (CLAUDE.md) -- each
# editor ecosystem wants the grammar in its own conventional location, and
# there's no symlink/build step keeping the two in sync. This is what
# makes that convention self-enforcing instead of relying on remembering
# to update both by hand: docs.html/docs_en.html, the other pair
# maintained under the identical convention, silently drifted for weeks
# with nothing catching it.
grammar-sync:
	cmp editors/vscode/syntaxes/tinox.tmLanguage.json editors/eclipse/tinox-eclipse/grammars/tinox.tmLanguage.json

# Activate git hooks (pre-push runs `make check`)
install-hooks:
	git config core.hooksPath .githooks
	@echo "core.hooksPath = .githooks set (pre-push: make check)"
