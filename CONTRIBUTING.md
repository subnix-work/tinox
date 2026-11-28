# Contributing to Tinox

Thanks for your interest in Tinox! This document covers how to build the
project, run the test suite, and the conventions this repo follows.

## Building

Requirements: a recent Rust toolchain (stable), `clang`, and `llc`/`opt`
(LLVM tools).

```bash
git clone https://github.com/subnix-work/tinox.git
cd tinox
cargo build --release
# Binary: target/release/tinox
```

## Testing

`make check` is the single local entry point that must pass before any
compiler commit:

```bash
make check
```

The current `Makefile` gate runs:

- `cargo clippy --release --workspace --all-targets -- -D warnings` — zero
  warnings across the workspace. Justified exceptions are inline
  `#[allow(...)]` with a comment explaining why.
- `test-and-dogfood`: runs `test` and `dogfood` concurrently. `test` uses
  `cargo-nextest` when available (otherwise `cargo test --profile ci`), and
  includes the Rust unit tests and all `crates/tinox/tests/*.rs` integration
  suites, including golden e2e, matrix, boundary and stdlib smoke tests.
  Do not add the standalone `e2e` target to this aggregate: those suites are
  already discovered by `test`.
- `dogfood`: builds `examples/` and `benchmarks/`, then builds/tests the
  downstream [jgrep-tinox](https://github.com/subnix-work/jgrep-tinox)
  checkout selected by `DOGFOOD_DIR` (default `../jgrep-tinox`). If that
  checkout is absent, dogfood reports a skip; that is not downstream
  compatibility evidence.
- `grammar-sync`, `workspace-stdlib`, `docs-check`, `ai-eval-test` and
  `service-benchmark-test`.

The full run time depends on the host and cache state. Additional targets
exist for deeper verification and are **not** part of `make check`:

```bash
make asan      # AddressSanitizer build (-DTINOX_NO_GC), catches overflows/UAF
make tsan      # ThreadSanitizer checks selected C-runtime concurrency paths
make checked   # Heap-kind registry (-DTINOX_CHECKED), catches dispatch-on-wrong-type bugs
make fuzz      # libFuzzer harnesses against their seed corpora
```

The scheduled deep-checks workflow runs these additional targets. They cover
different boundaries from the normal gate and do not replace it.

Enable the pre-push hook (runs `make check` automatically before every
push) with:

```bash
make install-hooks
```

## Adding e2e tests

End-to-end fixtures live under `tests/e2e/<scenario>/Main.tnx` and use
`// expect:` directives that are compared line-by-line against stdout. Tests
that bind a port should request an OS-assigned port with
`httpServerCreate(0)` and read it back with `httpServerBoundPort(fd)`, rather
than reserving a hard-coded port by hand. For a reconnect test that must bind
the same port again, retain that first assigned port and explicitly rebind it.

To find existing fixtures using the dynamic-port pattern:

```bash
rg -n 'httpServerCreate\(0\)|httpServerBoundPort' tests/e2e examples
```

Tests that exercise `spawn`/`await` (a simulated broker/server over
loopback) should be run repeatedly (15–40×) before you consider them
stable — the async runtime has had timing-dependent bugs that only
surface under repeated runs.

## Code conventions

- **One class/interface/enum per file.** A `.tnx` file may contain at
  most one top-level `class`/`interface`/`enum` declaration, and if it
  has one, the filename must match the type name exactly
  (`class Player` → `Player.tnx`). This is enforced by the compiler as a
  hard error. Files with no top-level type (plain `fn`/`main` scripts,
  e.g. most of `tests/e2e/*.tnx`) are unaffected. Modules with multiple
  types become directories (`import tinox.core.amqp10;` resolves to
  `crates/tinox-core/amqp10/`, one `<TypeName>.tnx` file per type); an
  `import` of the module pulls in every file in that directory.
- **English.** Commit messages and code (including comments, identifiers,
  and doc strings) are in English.
- **No silent failures.** Every error case should produce a hard, visible
  error rather than silently corrupting data or falling back to a quiet
  default value.
- **Verify network/protocol features against a real, independent
  implementation** when possible (e.g. a real broker, not just a
  simulated one), not only self-consistent tests — bugs where an
  implementation is internally consistent but wrong (protocol encoding
  mistakes, missing mandatory fields, …) are structurally invisible to
  simulated peers.
- **Prefer small, targeted fixes** over large, risky rewrites. A
  documented, known limitation is an acceptable outcome if a full fix
  would be disproportionately invasive — file (or comment on) a GitHub
  issue describing the gap.
- Keep `docs.html` (German) and `docs_en.html` (English) in sync — every
  new `<div class="mod-section">` (new stdlib module) needs an entry in
  both files.

## Reporting bugs / proposing features

Bugs and completed feature work are tracked as
[GitHub issues](https://github.com/subnix-work/tinox/issues). When filing
a bug report, please include:

- A minimal `.tnx` reproduction, if possible.
- What you expected vs. what happened.
- Whether it reproduces reliably or only intermittently (see the async
  runtime note above).

## Submitting changes

1. Fork the repo and create a branch for your change.
2. Make sure `make check` passes locally.
3. Open a pull request describing the change and, for bug fixes, the root
   cause.

By contributing, you agree that your contributions will be licensed under
the same dual MIT/Apache-2.0 license as the rest of the project.

## Reference documentation

Run `make docs-generate` after changing compiler annotation/signature metadata,
canonical guide examples, or package versions; commit the generated outputs.
`make check` includes `make docs-check`. Python 3.11+ is required. Before a
package release, use `make docs-stage` to create missing immutable version docs,
then regenerate the current inventory. Existing version directories are never
overwritten. See [the documentation workflow](docs/REFERENCE_WORKFLOW.md).
