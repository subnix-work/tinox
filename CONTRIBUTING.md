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

`make check` is the single entry point that must pass before any commit
that touches the compiler:

```bash
make check   # clippy + unit tests + e2e/matrix/boundary/stdlib_smoke + dogfood
```

This runs:

- `cargo clippy --release --workspace --all-targets -- -D warnings` — zero
  warnings across the workspace. Justified exceptions are inline
  `#[allow(...)]` with a comment explaining why.
- `cargo test --release` — Rust unit tests (lexer, parser, typecheck,
  codegen, …).
- The e2e suite (`tests/e2e/*.tnx` golden tests, a generated context
  matrix, boundary tests, and a stdlib smoke gate that exercises every
  `tinox-core` module once).
- `dogfood`: builds `examples/` and `benchmarks/`, and builds/tests a
  downstream project ([jgrep-tinox](https://github.com/subnix-work/jgrep-tinox),
  checked out as a sibling directory via `DOGFOOD_DIR`, default
  `../jgrep-tinox`).

The full run takes roughly 15–25 minutes. Two additional targets exist for
deeper verification but are **not** part of `make check` — run them when
you suspect a memory or dispatch bug, or before a release:

```bash
make asan      # AddressSanitizer build (-DTINOX_NO_GC), catches overflows/UAF
make checked   # Heap-kind registry (-DTINOX_CHECKED), catches dispatch-on-wrong-type bugs
```

Enable the pre-push hook (runs `make check` automatically before every
push) with:

```bash
make install-hooks
```

## Adding e2e tests

New end-to-end tests live under `tests/e2e/*.tnx` and use `// expect:`
directives that are compared line-by-line against stdout. If a test binds
a network port, pick one that's actually free — grep for existing usage
first:

```bash
grep -rn "httpServerCreate(4" tests/e2e/*.tnx examples/*.tnx
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
