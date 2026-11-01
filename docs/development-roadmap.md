# Native service and AI development roadmap

This is the repository delivery map for [issue #316](https://github.com/subnix-work/tinox/issues/316),
reviewed against the Tinox 2.8.0 sources on 2026-10-28.
The original review baseline was `044c773`. Issue state and follow-up work remain
in GitHub; this page connects the proposals to code, contracts and verification
in this checkout. A landed implementation does not establish that every
acceptance experiment has run.

The original overview's next-item pointer to #297 is superseded: structured
diagnostics landed in PR #324 (`4aeb95d`). The CLI and LSP share versioned
diagnostic records, and their focused tests pass in this checkout. The next
missing implementation in the original numerical sequence is #308. #315 has
an evaluation runner and protocol tests; measured model baselines and efficiency
comparisons remain outstanding. Re-check the integration branch before starting
follow-up work; commits on other branches are not evidence of delivery here.

The prerequisite entry-discovery fixes #293/#317 landed through PR #319.
Their historical completion is separate from the cache and frontend work below.

## Feedback and compiler tools

| Proposal | Delivery evidence in this checkout | Verification and boundary |
| --- | --- | --- |
| #294 Runtime object caching | [Cache implementation](../crates/tinox/src/runtime_cache.rs), PR #318 | [Runtime cache tests](../crates/tinox/tests/runtime_cache.rs); [measurements](../benchmarks/runtime_cache_results.json). Cache identity includes compiler configuration and preprocessed runtime source. |
| #295 Application build reuse | [Build cache tests](../crates/tinox/tests/build_cache.rs), PR #322 | [Measurements](../benchmarks/build_cache_results.json). The fingerprint and conservative bypasses are described in the [build guide](../README.md); this is separate from runtime object reuse. |
| #296 Incremental frontend queries | [Frontend cache tests](../crates/tinox/tests/frontend_cache.rs), PR #323 | [Measurements](../benchmarks/frontend_cache_results.json). Exact numbered AST identity is required for whole-program type tables; dependency changes conservatively invalidate users. |
| #297 Structured diagnostics | [Version 1 protocol](compiler-diagnostics.md), PR #324 | [CLI protocol tests](../crates/tinox/tests/diagnostics.rs) and LSP diagnostic tests. CLI coordinates count Unicode scalars; LSP positions count UTF-16 units. Message wording is not an identifier. |
| #298 Compact symbol descriptions | [Command and schema](compiler-describe.md), PR #328 | [Description tests](../crates/tinox/tests/describe.rs). Use resolved semantic metadata instead of parsing prose or generated LLVM. |
| #299 Bootstrap explanation | [Command and wiring model](compiler-explain.md), PR #332 | [Explanation tests](../crates/tinox/tests/explain.rs). Inspection reports configured wiring; actual binding, readiness and shutdown are runtime facts. |
| #300 Semantic typing through lowering | [Checked lowering contracts](compiler-semantic-lowering.md), PRs #335/#339 | [Composition tests](../crates/tinox/tests/semantic_lowering.rs) cover debug, release and checked builds. Dynamic native contracts and runtime word layouts remain explicit boundaries. |

## Concurrency and service contracts

| Proposal | Delivery evidence in this checkout | Verification and boundary |
| --- | --- | --- |
| #301 Typed tasks, lifetime, deadlines and cancellation | [Typed task fixture](../tests/e2e/typed_tasks/Main.tnx), task completion commit `673f505` | [Scope tests](../crates/tinox/tests/task_scopes.rs), [cancellation tests](../crates/tinox/tests/task_cancellation.rs), and task watchdog e2e fixtures. Cancellation is cooperative; watchdog failures are visible. |
| #303 Safe application-scoped state | [Shared-state tests](../crates/tinox/tests/shared_state.rs), PR #347 | Independent native clients check singleton publication and guarded shared state. Publication alone does not synchronize later application mutations. |
| #302 Resource budgets and overload | [Budget tests](../crates/tinox/tests/concurrency_budgets.rs), PR #346 | Tests cover affinity/quota, overrides, task queues, HTTP 503 overload recovery and WebSocket connection bounds. Budget settings must agree with task and lifecycle behavior. |
| #308 Validated runtime configuration | **Remaining implementation in this snapshot.** [Current runtime](../runtime/runtime.c) still uses `tinox_config_load`/`tinox_config_get*`. | Missing properties return empty strings or zero; integer parsing uses `atoll`. Existing `@Config` support does not establish required-value validation or the proposed override contract. Validate before readiness and avoid disclosing secret values. |
| #304 Coordinated lifecycle | [Lifecycle integration tests](../crates/tinox/tests/service_lifecycle.rs), commit `de49e37` | Independent clients and real signals exercise readiness, draining and disposal. Startup, task cancellation and cleanup must retain one consistent failure contract. |
| #307 Typed REST responses | [Response contract](typed-rest-responses.md), commit `3e1422f` | [Independent curl tests](../crates/tinox/tests/typed_rest_responses_curl.rs) cover dynamic status and response behavior. Keep status, headers and body serialization consistent with generated contracts. |
| #309 Deployable containers | [Container build workflow](docker.md), commit `0c5b615` | Generator tests exist; real Docker builder/image execution, two-host compatibility, REST/DB/TLS smoke tests, size and startup measurements remain unverified in the documented delivery. Pinned environments do not promise byte-identical binaries. |
| #313 Local dependency development | [Override contracts and separate test lanes](local-dependencies.md), PR #391 | [Workspace override tests](../crates/tinox/tests/local_override.rs) and `make workspace-stdlib`. Preserve registry/artifact compatibility as an independent lane. |

## Scale, memory and platform coverage

| Proposal | Delivery evidence in this checkout | Verification and boundary |
| --- | --- | --- |
| #305 Logs, correlation and tracing | [Observability contract](observability.md), commit `7dc41de` | [Integration tests](../crates/tinox/tests/observability.rs) use independent HTTP clients and an OTLP JSON receiver. A real OpenTelemetry Collector remains an independent verification step. |
| #310 Explicit Linux targets | [Target selection tests](../crates/tinox/tests/target_selection.rs), PR #390 | Native x86-64/ARM64 selection and invalid-target rejection are implemented. Foreign-target compilation is rejected; a host-native passing gate does not verify the other architecture. |
| #311 Compact bytes and bounded streaming | [Bytes API](../crates/tinox-core/tinox/core/bytes/Bytes.tnx), commit `94703de` | [Compact buffer fixture](../tests/e2e/bytes_compact_buffer/Main.tnx) and [streaming file fixture](../tests/e2e/streaming_file_byte_io/Main.tnx). Check each service API's own bounds before generalizing these contracts. |
| #312 Service/GC measurements | [Harness](../benchmarks/service_latency/README.md) and [results](../benchmarks/service_latency/results.md), PR #394 | Tail latency, overload and opt-in GC experiments are recorded. Enforced container-memory runs, live-byte accounting and allocation/copy profiling remain outstanding. Boehm defaults were retained. |
| #306 Absence and recoverable errors | [API contract inventory](api-contracts.md), first additive slice in commit `173f32f` | [Contract tests](../crates/tinox/tests/api_contracts.rs). HTTP lookup migration is additive; the broader inventory still needs checked runtime operations and optional APIs. Legacy sentinel APIs remain available as documented. |

## References and AI efficiency

| Proposal | Delivery evidence in this checkout | Verification and boundary |
| --- | --- | --- |
| #314 Checked references and AI guide | [Shared metadata workflow](REFERENCE_WORKFLOW.md), [compact guide](AI_GUIDE.md), PR #393 | `make docs-check` checks current snapshots, canonical examples and documentation inventories. Immutable release pages have a separate staging workflow. |
| #315 Reproducible AI task evaluations | [Runner and protocol](../benchmarks/ai/README.md), [protocol tests](../benchmarks/ai/runner_test.py), `make ai-eval-test` | Infrastructure exists for isolated task copies, independent acceptance checks, pinned model identity, usage/cost budgets, transcripts and controlled comparisons. Initial tasks cover arithmetic bug fixes, HTTP validation and in-memory CRUD. No measured model baseline or efficiency improvement is established; broader transactional, broker and concurrency tasks remain follow-ups. |

## Remaining delivery sequence

1. Implement #308 on the integration branch after checking for newer work. Cover
   missing/invalid required values, deterministic override precedence, injection
   construction paths, secret redaction and startup failure before readiness.
2. Complete independent deployment experiments for #309 and architecture coverage
   for #310; collect #312 measurements under enforced container memory limits.
   Keep environment limitations visible alongside results.
3. Complete #315's measurement phase using the existing evaluation runner and
   independently checked task outcomes. Record model/configuration, task inputs,
   total tokens through verified completion, raw outcomes and reproducible
   controls. Compare feedback latency and total tokens against baselines; the
   runner, guide and caches alone are not evidence of AI efficiency. A green
   compiler gate is required before interpreting comparative results.
4. Continue #306's API migration using the source inventory, re-checking later
   filesystem fixes before choosing the next operation. Keep absence distinct
   from recoverable failure and preserve documented compatibility.

These tasks can proceed independently where their contracts allow. Preserve
annotation-first application examples and focused changes; do not reopen
historical foundations #61/#62/#64, #116, #186, #191 or #284/#292. Reproduce #289
against current code before treating the historical await crash as still open.

## Verification and measurement policy

Run the complete gate from the repository root, with the downstream checkout at
the revision pinned in `.github/workflows/check.yml`:

```sh
DOGFOOD_DIR=/path/to/pinned/jgrep-tinox make check
```

A missing downstream checkout is reported as skipped by the dogfood script;
that is not evidence of downstream compatibility. Retain both workspace-source
and registry-artifact stdlib lanes. Repeat changed spawn/await concurrency tests
15–40 times and use independent protocol implementations for network changes,
as required by the contribution guide. Container and cross-platform experiments
remain separate from a passing local gate.

The original tiny-program baseline measured approximately 1 ms typecheck,
1.60 s debug/release build, 1.59 s runtime C compilation, 0.90 ms process
launch-to-exit and a 175 KiB dynamic executable. Build samples used three runs,
launch used 30, with warm filesystem caches; shared-library sizes were excluded.
Those host-specific figures do not measure service readiness or production load.
Use the committed cache and service benchmark methodology for comparisons,
recording environment, sample count, cold/warm state and exact reuse boundary.

