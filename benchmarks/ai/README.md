# AI coding task evaluations (#315)

Status: evaluation infrastructure, **no measured model baseline or efficiency
improvement claimed**. Python 3.10+, standard library only. The initial suite
covers a price calculation bug fix, HTTP validation/not-found handling, and
annotation-driven in-memory CRUD. Transactional updates, real broker consumers,
and concurrent work are follow-up tasks, not represented by these results.

Issue history rechecked on 2026-10-22: #315 remains proposed, without comments.
#293 was fixed on develop via #319, but this checkout predates that fix. Checks
here build and execute code directly and compare behavior using Python; they do
not accept a model's test report or rely on `tinox test` discovery/exit status.

## Configure and run

Build the compiler with `cargo build --release`. Copy `config.example.json` to
`/tmp/eval.json`. Configure an already installed Ollama model's exact tag and
`digest` from `GET /api/tags`, the server version, and the absolute adapter path.
Do not auto-pull a moving model tag. The bundled adapter uses the documented
[Ollama chat API](https://docs.ollama.com/api/chat) and
[model digest metadata](https://docs.ollama.com/api/tags). It checks the digest
before every turn. Start with a non-thinking model whose JSON output fits the
configured output limit. Set `TINOX_EVAL_OLLAMA_URL` in the environment to change
the local server address. No server or model is downloaded by the runner.

```sh
python3 benchmarks/ai/runner.py --config /tmp/eval.json --out /tmp/baseline
# Validation only: the above does not invoke a provider.
python3 benchmarks/ai/runner.py --config /tmp/eval.json --out /tmp/baseline --execute
python3 -m unittest discover -s benchmarks/ai -p '*_test.py'
```

Output must be a new directory. Exit zero requires every run to pass. Every task
starts from a clean copy of its starter, with no conversation shared across runs.
The adapter runs from that workspace. Provide documentation explicitly through
`context`, e.g. `{"compact-guide": "/absolute/path/to/guide.txt"}`. The model can
retrieve it with `read` on `docs/compact-guide`; retrieval results and all later
requests remain in the transcript. Record compiler/package cache setup before
measurement; HTTP starters pin `tinox.core:http_server` to 1.0.3 in their manifests.
The compiler may retrieve that package if it is missing; prewarm it consistently.
No package is published by this suite.

For another provider, configure an absolute command argv implementing the
single-call protocol below. Paid providers require `billing: "metered"`, `--execute`, explicit nonzero
prices and a total `max_cost_usd`. The runner reserves one full context plus
maximum output at the configured per-million prices **before each call**. The
cost cap covers the whole task/run matrix. Zero prices are for local models;
they must never be used to disguise a billed endpoint. Set rates conservatively,
including reasoning, caching and provider-specific charges. Adapters must disable
SDK retries and extra model calls and enforce the request limits; the runner
cannot enforce a remote provider's billing behavior. A timeout or missing usage
marks accounting incomplete, reserves the possible cost, saves the failure, and
halts further provider calls. Such runs cannot support an efficiency comparison.

## Provider adapter protocol (version 1)

The runner sends one JSON object on stdin with `model`, immutable
`model_revision`, `settings`, `context_tokens`, `max_output_tokens`, and
`messages` (system/user/assistant). Adapter stdout must contain exactly:

```json
{"content":"{\"tool\":\"read\",\"args\":{\"path\":\"Main.tnx\"}}","usage":{"input_tokens":412,"output_tokens":27}}
```

Usage comes from the provider, including cached input and generated reasoning,
not estimates of source length. Exactly one generation is permitted per adapter
invocation. All documentation/retrieval and repair history must be included;
never silently truncate history. The runner also uses a conservative UTF-8 byte
bound with a 256-token template allowance per message to stop before context
exhaustion. Adapters with larger templates must apply a stricter check. Ollama
reports `prompt_eval_count` and `eval_count`; verify the accounting semantics of
your pinned server version before publishing results.

The assistant content is one tool action: `list`, `read`, `write`, `build`, `run`,
or `submit`. See `TOOLS` in runner.py for arguments. `build` compiles Main.tnx;
`run` executes the resulting program for at most 10 seconds. `submit` checks a
fresh snapshot with independent assertions and returns only pass/fail. All
submissions, including failures, cost time and are recorded. First-attempt
success means the **first acceptance submission** passed, not that the first
build worked; build failures are separate. Tool responses are bounded at 12,000
characters and the exact text sent back is stored. No human repairs are allowed
within a run (`manual_interventions=0`). If intervention is needed, stop and
retain the failed run rather than editing it into a success.

Acceptance checks live outside the model workspace, with cases absent from the
task prompt. They use direct execution for arithmetic and Python's independent
HTTP client for service status/body/state checks. They are held out by protocol,
not secret in this public repository. For publication, freeze additional private
cases before running models and record their checker hash. This is a local
trusted-code runner, **not an OS sandbox**: the adapter and compiled submissions
can execute code. For untrusted models use an isolated machine/container with
acceptance code mounted only into the checking process. A model-written test,
comment or success message never determines correctness.

## Artifacts and comparisons

`protocol.json` records initial instructions, model/settings, adapter revision,
budgets/stopping rules, tool definitions, compiler version/binary hash, task and
checker hashes, documentation hashes, platform, repository commit and dirty-tree
status. Each task/run JSON records every request/response/tool result, usage,
wall time, build attempts/failures, submissions, final correctness, stopping
reason, accounting completeness and final Tinox sources. `summary.json` includes
success counts and mean/sample standard deviation/min/max for all runs,
including failures. Failed provider calls have unknown tokens, explicitly marked;
never interpret their partial token counts as a saving.

Keep credentials only in the adapter environment. Never put tokens, passwords,
authenticated URLs or private material in config argv/settings/context. Adapter
stderr is deliberately not stored. Transcripts contain submitted code and tool
output: use credential-free workspaces and inspect artifacts before sharing.
Generated artifacts belong outside the repository unless deliberately reviewed.

Use at least five repetitions (more for small effects); temperature zero does not
remove nondeterminism. Keep sampling, context, tool access, packages, machine,
prices, limits and stopping rules fixed. For a tooling change, build baseline and
changed compilers and run the same suite with `--tinox /absolute/compiler`, using
separate output directories. Change only `condition` plus the intended variable.

```sh
python3 benchmarks/ai/compare.py /tmp/baseline /tmp/changed --variable compiler > /tmp/comparison.json
```

The comparison rejects incomplete matrices, unknown accounting and undeclared
protocol differences. `--variable context` permits only documentation changes;
`--variable runner` permits only the runner implementation to differ. Compiler
comparisons permit binary/version differences. Repository commits/timestamps and
dirty status remain visible as provenance. Report correctness alongside effort,
first-submission success, failure reasons and variability; do not discard hard
runs or compare only successful samples. Avoid efficiency claims until measured
results support them. Use the evidence to refine defaults and the compact guide,
rather than optimizing source-token counts alone.
