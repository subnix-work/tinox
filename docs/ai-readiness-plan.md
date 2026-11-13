# Making Tinox efficient for AI-written programs — plan

Status: proposal (nothing in here is implemented yet). Owner: open.

## Goal

An AI coding agent should write a correct Tinox program in as few iterations,
tokens and human corrections as possible, even though Tinox is not in any
model's training data. Java is the reference point: a model writes Java from
memory. For Tinox the model must be *told* the language, and the
compiler must *steer it back* when it is wrong. So the plan has three levers:

1. **Context** — the model reads a small, correct, current description.
2. **Feedback** — the compiler's errors say what to do, in a machine-readable form.
3. **Measurement** — we know whether (1) and (2) work, instead of guessing.

## What already exists (build on it, do not duplicate)

| Asset | What it gives an agent |
| --- | --- |
| `docs/AI_GUIDE.md` | Generated, CI-checked compact guide (4312 of 8192 byte budget used, 2 checked examples), see `docs/REFERENCE_WORKFLOW.md` |
| `tinox reference`, `docs/current/reference.json` | Builtin signatures + annotation targets/arity as JSON |
| `tinox describe <Symbol> --json`, `tinox explain --bootstrap --json` | Resolved API lookup, explanation of generated wiring |
| `tinox check/build --json`, `--message-format=json` | Versioned diagnostics with codes and ranges (`docs/compiler-diagnostics.md`) |
| `docs/SPEC.md`, `examples/`, `tests/e2e/*.tnx` | Spec plus ~35 examples and ~250 executable fixtures |
| `make docs-check` | Mechanism that keeps generated guide examples compiling and passing |

## Gaps (observed, not assumed)

- The guide covers only the basics (entry point, accessors, `@Test`). Everything
  that agents actually build — REST handlers (`@GET` + `@PathParam`/`@PostParam`/
  `@QueryParam`), JSON, DB/`@Transactional`, `spawn`/`scope`, CLI `@Command`,
  WebSocket, config — is absent. The model falls back to guessing from Java/Kotlin.
- Nothing points an agent at the guide. `tinox new` creates no `AGENTS.md`/
  `CLAUDE.md`, there is no `llms.txt`, no skill/plugin.
- `CLAUDE.md` is ~2000 lines of mostly historical debugging notes and is loaded
  into every session. That is a large token cost and buries the few rules an
  agent must follow (`fn` vs `fnc`, `Type::method()`, no `;`, one type per
  file, class `Main`, REST parameters need a binding annotation).
- Diagnostics have codes and ranges but, as far as `compiler-diagnostics.md`
  shows, no uniform "how to fix" hint or machine-applicable fix. To verify in phase 3.
- There is no measurement. We cannot say whether a guide change helped.
- Published extended-tier packages can lag the language (the `;` removal left
  old package sources unparseable until republished), which makes an agent's
  correct code fail for reasons it cannot fix.

## Phases

### Phase 0 — Baseline eval harness (first, because everything else is judged by it)

- `tests/ai-eval/`: ~25 small task specs in English, each with an acceptance
  check that is *executable* (compile, run, compare stdout, or `tinox test`,
  or an HTTP probe for server tasks). Spread across: basics/collections,
  classes+interfaces, REST CRUD, JSON, DB with `@Transactional`, concurrency
  (`spawn`/`scope`/channels), CLI `@Command`, WebSocket, error handling, tests.
- A runner that gives an agent only a fresh project dir plus a chosen context
  bundle (none / AI_GUIDE only / new guide + topic pages), runs it headless
  (e.g. `claude -p`), then applies the acceptance check.
- Metrics per task: pass/fail, number of `tinox check/build` iterations to
  green, tokens, wall time. Report as JSON; compare runs.
- Not part of `make check` (needs a model and costs money); a manual /
  scheduled lane, results committed under `docs/current/` or kept as CI artifacts.
- **Exit criterion:** a baseline number for today's guide.

### Phase 1 — Discoverability

- `tinox new` writes a short `AGENTS.md` (and `CLAUDE.md` pointing to it) that
  states the ten rules and links to `tinox guide`.
- New `tinox guide [topic]` prints the compact guide / a topic page to stdout, so
  an agent can pull it on demand without knowing repo paths.
- `llms.txt` at the repo root and on the docs site; a Claude Code skill
  (`SKILL.md`) in the repo that loads the guide when a `.tnx` file or
  `tinox.toml` is present.
- **Exit criterion:** an agent in an empty `tinox new` project finds the guide
  without being told.

### Phase 2 — Content: topic guides and a recipe cookbook

- Keep `AI_GUIDE.md` as the always-loaded core (raise or keep the 8 KB budget,
  but fill it with the highest-value rules: the ten rules, the top common
  mistakes, the annotation table for REST).
- Add `docs/guide/topics/<topic>.md` (rest, json, db, concurrency, cli,
  websocket, testing, config, errors). Each is small, shows one *complete,
  runnable* program and a "do not" list.
- Every example is registered in `docs/guide/examples.json` so the existing
  `make docs-check` compiles, runs and diffs it — the guide cannot rot.
- A **common-mistakes table** distilled from our own bug history and from the
  eval failures of phase 0 (e.g. `;`, missing `class Main`, `ctx: HttpContext`
  old handler shape, top-level `fn`, calling a static with `.`). Driven by data,
  not by memory.
- **Exit criterion:** eval pass rate and iteration count improve against baseline.

### Phase 3 — Compiler feedback loop

- Audit the diagnostics for every error an agent plausibly hits. Each should
  carry: stable code, the offending range, **a one-line fix hint**, and where
  useful a `suggestion` (replacement text). Add `tinox explain <CODE>` giving
  the rule plus a minimal correct example.
- "Did you mean" for unknown identifiers, methods, annotations (the LSP has
  the symbol tables).
- Migration errors in the style of the `;` error: when legacy syntax is
  detected, say what replaces it.
- Optionally `tinox fix` applying machine-applicable suggestions.
- **Exit criterion:** fewer iterations per task in the eval; no diagnostic
  without a hint in the audited set.

### Phase 4 — Fast verification loop and tool access

- Measure `tinox check` latency on a mid-size project; keep it under a second
  for the edit loop. Single-test selection (`tinox test --filter`) with JSON
  results (events already exist).
- Expose `check`, `describe`, `reference`, `test` as an MCP server (thin
  wrapper over the existing JSON commands) so agents call them as tools
  instead of shelling out and parsing text.
- **Exit criterion:** an agent can go edit → check → test in one tool call each.

### Phase 5 — Stability and package freshness

- Written compatibility policy: what may change between minor versions, how
  deprecations are announced, that every syntax change ships with a compiler-
  driven migration (`migrate-semicolons` was the template).
- A CI lane that fails when a published extended-tier package no longer parses
  with the current compiler, or when the workspace and published versions
  diverge, so agents are not blocked by stale dependencies.
- **Exit criterion:** the guide's version stamp and the package inventory
  (`docs/current/packages.json`) are both enforced as current.

### Phase 6 — Repo hygiene for agents

- Split `CLAUDE.md`: a short normative file (< ~300 lines: conventions, build/
  test, the rules) and the historical notes moved to `docs/internal/` with a
  pointer. Same content, much smaller always-loaded context.
- Keep `docs.html`/`docs_en.html` parity as today.
- **Exit criterion:** token cost of a fresh session context drops measurably.

## Order and effort

Phase 0 first (small, makes the rest measurable). Then 1 and 6 (cheap, large
effect on context cost), then 2 (largest content work, driven by phase-0 failures),
then 3 and 4 (compiler work), then 5. Phases 1/6 can start in parallel with 0.

## Risks

- **Overfitting to the eval:** keep the task set varied, add hidden tasks, and
  re-sample from real user prompts.
- **Guide bloat:** a bigger guide costs tokens on every task; keep the core
  small and load topics on demand.
- **Models change:** results are per model; record the model in each run.
- **Spec churn:** every language change must update guide examples in the same
  PR (already enforced by `make docs-check` once the examples exist).

## Open decisions

1. Is the eval allowed to call a paid model in a scheduled CI lane, or manual only?
2. Do we want an MCP server in this repo or as a separate package?
3. Target budget for the always-loaded core guide (keep 8 KB or change)?
4. Is `AGENTS.md` in `tinox new` acceptable for all users, or opt-in (`--ai`)?
