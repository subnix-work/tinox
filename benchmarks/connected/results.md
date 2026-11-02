# Warm compiler feedback on densely connected projects (#559)

`scripts/benchmark-connected.py` generates 150-module projects in three topologies and measures `tinox check` and
`tinox build` through the real CLI (`TINOX_TIMINGS=1`, `TINOX_CACHE_TRACE=1`, separate frontend/runtime cache
directories, the C runtime warmed once and excluded):

* `chain`: module i imports module i+1 (deep dependency chain);
* `dense`: module i imports the next 8 modules (heavily connected DAG, transitive closures of up to ~150 files);
* `generic`: `dense` with every signature exchanging generic `Box<T>` / `Pair<T, U>` values.

Scenarios: `clean` (all caches off), `cold`, `warm` (nothing changed), `leaf-edit` (a body literal of the module
nothing but `Main` imports), `popular-body-edit` (body of the module everything depends on, signature unchanged) and
`popular-signature-edit` (a new parameter on that module, passed by its direct callers). Every sample writes content
no earlier sample had, so each is a real invalidation. Five runs per cell; the table shows medians in milliseconds.

Raw repeated samples with per-stage timings, frontend/build cache hit-miss counts, the platform and the compiler
SHA-256 are in `before.json` (compiler at the parent commit of this change) and `after.json`.
Machine: Arch Linux, 32 logical CPUs, `cc` = clang.

## Where the time went (before)

For the `dense` project, warm `check`: 170 ms wall, of which the frontend `query` stage was 95 ms over 454 queries and
`file-typecheck` 87 ms, while `parse` was 7 ms and the whole-program typecheck 2 ms. A warm run did almost no
checking; it spent its time *deciding that nothing changed*. Cause: every per-file `visibility` query serialized
and SHA-256-hashed the full AST of the file plus the full ASTs of its whole transitive prelude closure. In a densely
connected project those closures are large, so the bytes hashed grow quadratically with the number of modules.

## Change

`query` keys for `visibility` are now built from the file identity plus one SHA-256 digest per parsed source
(`tinox_typecheck::cache::ast_digest`), computed once per file for the whole pass
(`check_with_prelude_digests`). The key still changes with any byte-level change of the file or of any prelude, full
bodies included, so the conservative invalidation rule in `crates/tinox-typecheck/src/cache.rs` is unchanged. The LSP's
`check_with_prelude_cached` computes the same digests, so both callers share cache entries.

## Result (median ms, 5 runs)

| topology | command | scenario | before | after | speedup |
|---|---|---|---|---|---|
| chain | check | clean | 450 | 448 | 1.01x |
| chain | check | cold | 523 | 451 | 1.16x |
| chain | check | warm | 103 | 28 | 3.67x |
| chain | check | leaf-edit | 125 | 49 | 2.56x |
| chain | check | popular-body-edit | 527 | 448 | 1.18x |
| chain | check | popular-signature-edit | 520 | 448 | 1.16x |
| chain | build | clean | 650 | 655 | 0.99x |
| chain | build | cold | 733 | 657 | 1.12x |
| chain | build | warm | 224 | 151 | 1.49x |
| chain | build | leaf-edit | 519 | 443 | 1.17x |
| chain | build | popular-body-edit | 913 | 841 | 1.09x |
| chain | build | popular-signature-edit | 924 | 845 | 1.09x |
| dense | check | clean | 532 | 537 | 0.99x |
| dense | check | cold | 619 | 541 | 1.14x |
| dense | check | warm | 170 | 88 | 1.94x |
| dense | check | leaf-edit | 190 | 110 | 1.73x |
| dense | check | popular-body-edit | 617 | 531 | 1.16x |
| dense | check | popular-signature-edit | 613 | 527 | 1.16x |
| dense | build | clean | 739 | 741 | 1.00x |
| dense | build | cold | 823 | 739 | 1.11x |
| dense | build | warm | 289 | 212 | 1.36x |
| dense | build | leaf-edit | 588 | 505 | 1.16x |
| dense | build | popular-body-edit | 1008 | 926 | 1.09x |
| dense | build | popular-signature-edit | 1007 | 934 | 1.08x |
| generic | check | clean | 625 | 627 | 1.00x |
| generic | check | cold | 720 | 619 | 1.16x |
| generic | check | warm | 198 | 108 | 1.83x |
| generic | check | leaf-edit | 226 | 134 | 1.69x |
| generic | check | popular-body-edit | 713 | 615 | 1.16x |
| generic | check | popular-signature-edit | 707 | 617 | 1.15x |
| generic | build | clean | 922 | 930 | 0.99x |
| generic | build | cold | 1024 | 944 | 1.09x |
| generic | build | warm | 324 | 236 | 1.37x |
| generic | build | leaf-edit | 721 | 631 | 1.14x |
| generic | build | popular-body-edit | 1206 | 1109 | 1.09x |
| generic | build | popular-signature-edit | 1212 | 1113 | 1.09x |

Reading:

* Unchanged and leaf-edit feedback improved: warm `check` is 1.8x-3.7x faster (chain 103 to 28 ms), leaf-edit `check`
  1.7x-2.6x. `clean` is unchanged (1.00x), as expected: a clean run has no cached queries to key.
* Editing the most popular module, with or without a signature change, still costs about as much as a clean check
  (about 450-615 ms): every user is invalidated by design and must be re-checked. This is the cost of the documented
  conservative rule ("`this` usage affects call semantics, so stripping bodies to approximate an exported signature
  is unsafe today"); a signature-level key would be the next step, but it needs a compiler-side definition of what
  can affect a dependent first, and was not attempted.
* `build` is dominated by the backend after any source change (`miss::build`/`compiling`: LLVM + link), which this
  change does not touch: leaf-edit `build` is 443-631 ms of which the frontend is now small.

## Correctness evidence

`frontend_cache`, `build_cache`, `local_overrides` and `check_all_files` (invalidation under source, dependency,
flag and tool changes, corrupt/misplaced entries, concurrent publication) pass unchanged. The benchmark itself checks
that every changed scenario reports the expected cache misses.
