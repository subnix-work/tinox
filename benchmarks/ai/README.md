# AI coding task evaluations (#315)

Status: [measured Codex subscription baseline/context comparison](results/2026-11-01-codex/README.md),
60 runs on pinned compiler 2.8.0: baseline 17/30 correct, added compact guide
12/30 correct. **No efficiency improvement established.** The adapter is also
verified on current develop (compiler 2.9.0); those measurements are not 2.9 results.
Python 3.10+, standard library only. Six tasks cover a
price bug fix, HTTP validation, CRUD, bounded parallel work, a transactional
update and a finite binary broker consumer.

The three new held-out checks measure behavior independently:

- `concurrent` replaces Job with native instrumentation, checks actual overlapping
  worker execution, the concurrency limit, exact invocation count and completion
  of started workers before return/throw. A sequential calculation is rejected.
- `transactional` provisions PostgreSQL, exercises validation plus a second-save
  database constraint failure, and checks balances independently with psql after
  every case. The recorded protocol's database-error case verifies atomicity
  after disconnect; it does not establish in-process recovery.
- `broker` provisions RabbitMQ, publishes binary messages through its independent
  management HTTP API, checks delivered bytes and verifies that messages do not
  reappear after connection closure. It uses a durable test queue because modern
  RabbitMQ rejects transient nonexclusive queues by default.

Docker, local `psql` and cached `postgres:17` / `rabbitmq:4-management` images are
needed for those service checks. Submit provisions their disposable daemons;
model-workspace build/run does not silently start a persistent shared service.
Preload/pin the exact image IDs before measured comparisons. The runner records
those IDs and stops before any provider call when an image is unavailable.
Record cache warm/cold policies and machine conditions in `execution_conditions`;
these fields and the relevant build/cache environment are part of comparison
compatibility. Acceptance replaces
only the smoke entry point and its held-out instrumentation; it never accepts
model-written tests or claimed success as proof.

`validate_controls.py --out /tmp/checker-controls` runs each new checker against
15 known positive controls plus a deliberately wrong control. It invokes no
model/provider. Checker regression results **are not model baseline results**.
The native checker and good controls remain outside the model's tool workspace.

The runner, expanded tasks, checker controls and measured subscription comparison
are implemented. New execution needs an explicit provider/model revision and authorized total spending limit;
no configured model workload is implied by validating the checkers. Entry-discovery fix
#293/#319 is present in this checkout. Acceptance checks still build and execute
code directly and compare behavior using Python; they do not accept a model's
test report or rely on `tinox test` discovery/exit status.

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
measurement; HTTP starters pin `tinox.core:http_server` versions in their manifests.
The compiler may retrieve that package if it is missing; prewarm it consistently.
No package is published by this suite.

For another provider, configure an absolute command argv implementing the
single-call protocol below. Paid providers require `billing: "metered"`, `--execute`, explicit nonzero
prices and a total `max_cost_usd`. The runner reserves one full context plus
maximum output at the configured per-million prices **before each call**. The
cost cap covers the whole task/run matrix. Zero prices are for local models
and explicitly declared subscription execution;
they must never be used to disguise a billed endpoint. Set rates conservatively,
including reasoning, caching and provider-specific charges. Adapters must disable
SDK retries and extra model calls and enforce the request limits; the runner
cannot enforce a remote provider's billing behavior. A timeout or missing usage
marks accounting incomplete, reserves the possible cost, saves the failure, and
halts further provider calls. Such runs cannot support an efficiency comparison.

### Codex through a ChatGPT subscription

`codex_adapter.py` uses the installed official Codex CLI and an existing ChatGPT
login. Copy `config.codex.example.json` and replace its explicit placeholders.
Set `provider: "codex-chatgpt"`, `billing: "subscription"`, and all three
USD fields to zero: this records **zero additional API spending**, not free
inference or a per-token subscription price. An explicit `--execute` is still
required. Subscription quota is shared with ordinary Codex work.

Use an absolute adapter argv as above, and configure:

```json
{
  "model": "YOUR_EXPLICIT_AVAILABLE_MODEL",
  "model_revision": "YOUR_EXPLICIT_AVAILABLE_MODEL@codex-cli YOUR_INSTALLED_VERSION;backend-snapshot-unavailable",
  "adapter_version": "codex_adapter.py-v2",
  "settings": {"cli_version": "codex-cli YOUR_INSTALLED_VERSION", "reasoning_effort": "low"}
}
```

The adapter checks the CLI version and ChatGPT login before every generation,
forces ChatGPT authentication, removes API keys/endpoint overrides from the
child environment, and ignores user config and project instructions. Each turn
starts in an empty temporary directory with native tools, plugins, MCP servers,
web search, hooks, memory and agents disabled. The external runner remains the
only tool executor. Native tool events, error/retry events, multiple completions
or missing usage invalidate the turn and stop the matrix; a known startup notice
about the deliberately disabled code-mode host is retained but allowed. CLI
JSON events are preserved in each response's `provider_metadata`, including
cached-input and reasoning counters. Input tokens include cached input; generated
reasoning is not added a second time to the CLI's output total.
The CLI's `--output-last-message` file selects the action. It must agree with the
last completed message event. Additional commentary messages remain in the raw
events and full-turn token totals; they do not execute extra runner actions.
The complete runner conversation is serialized as the CLI's user input under
the adapter's fixed instructions. CLI/server instructions add overhead, included
in the reported token totals. This measures that specific restricted workflow,
not a bare-model API or unrestricted interactive Codex session.

Unlike the local adapter, the CLI cannot pin an immutable backend model snapshot
or enforce the runner's output-token reservation before generation. Model aliases
can change server-side. The runner checks actual usage against reservations
afterward. For subscriptions, a fully reported over-budget turn is a failed
`provider_token_limit` task with known usage, and its action is not executed;
missing usage still stops the entire matrix. Metered-provider reservation
violations remain an accounting failure that halts further calls. Built-in transport
retries cannot be fully disabled; visible retry/error events stop the run. Record
these limitations with results, retain failed pilots, and do not claim an exact
backend reproduction, uncached inference cost, or universal model comparison.
No API fallback, credit purchase, or quota-reset redemption is performed.

Official interfaces: [Codex non-interactive mode](https://learn.chatgpt.com/docs/non-interactive-mode)
and [subscription pricing/limits](https://learn.chatgpt.com/docs/pricing?codex-pricing-plans=individual).

## Provider adapter protocol (version 1)

The runner sends one JSON object on stdin with `model`, `model_revision`,
`settings`, `context_tokens`, `max_output_tokens`, and
`messages` (system/user/assistant). Adapter stdout must contain exactly:

```json
{"content":"{\"tool\":\"read\",\"args\":{\"path\":\"Main.tnx\"}}","usage":{"input_tokens":412,"output_tokens":27}}
```

Usage comes from the provider, including cached input and generated reasoning,
not estimates of source length. Each adapter invocation permits one provider
turn (one API generation, or one complete tool-free Codex CLI turn with its
full reported usage). All documentation/retrieval and repair history must be included;
never silently truncate history. The runner also uses a conservative UTF-8 byte
bound with a 256-token template allowance per message to stop before context
exhaustion. Adapters with larger templates must apply a stricter check. Ollama
reports `prompt_eval_count` and `eval_count`; verify the accounting semantics of
your pinned server version before publishing results.

Pin an immutable `model_revision` whenever the provider exposes one. The Codex
subscription adapter instead records the selected alias, pinned CLI version and
explicit `backend-snapshot-unavailable` marker; it must never masquerade as an
immutable provider revision.

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
