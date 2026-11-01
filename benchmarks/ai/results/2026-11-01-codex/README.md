# Codex subscription evaluation — 2026-11-01 (#315)

Complete measured matrix: six tasks × five repetitions × two context conditions (60 task runs). All final runs have complete CLI-reported token accounting. Failed tasks remain in denominators. This is a measurement of the configured workflow; it does not automatically establish an efficiency improvement.

## Results

| Task | Baseline correct | Guide correct | Baseline tokens, mean ± sample SD | Guide tokens, mean ± sample SD |
| --- | ---: | ---: | ---: | ---: |
| broker | 4/5 | 2/5 | 342,674 ± 306,353 | 462,198 ± 345,296 |
| bugfix | 5/5 | 5/5 | 82,427 ± 17,054 | 57,450 ± 20,332 |
| concurrent | 5/5 | 4/5 | 191,990 ± 20,727 | 184,448 ± 63,558 |
| crud | 0/5 | 0/5 | 361,930 ± 98,286 | 316,343 ± 110,649 |
| transactional | 2/5 | 1/5 | 536,027 ± 262,333 | 475,666 ± 175,423 |
| validation | 1/5 | 0/5 | 371,797 ± 80,887 | 678,706 ± 191,022 |

Across all tasks: baseline 17/30 correct; compact-guide condition 12/30 correct.

Total reported input + output tokens were 9,434,225 for the baseline and
10,874,053 with the guide (about 15.3% more). The guide condition had fewer
successful tasks in this sample; no efficiency improvement is established.
These descriptive totals do not establish a causal effect or statistical significance.

Both conditions receive the same language specification and source-declaration inventories for HTTP, ORM, AMQP and Bytes. The changed condition additionally exposes the existing compact guide through the same read tool. Only the declared context variable differs.

- baseline: {'context_limit': 12, 'passed': 17, 'provider_token_limit': 1}; 34 failed builds; 15 successful first acceptance submissions; 0 manual interventions.
- guide: {'context_limit': 17, 'passed': 12, 'provider_token_limit': 1}; 32 failed builds; 12 successful first acceptance submissions; 0 manual interventions.

`context_limit` is the runner’s conservative UTF-8 guard, not a claim that the provider exhausted its advertised context window. `provider_token_limit` is a fully reported CLI turn above the configured reservation: its usage is counted, its action is not executed, and the task fails. Lower token use on an unsuccessful task is not evidence of improved efficiency.

## Conditions and limits

- Official Codex CLI 0.164.0, ChatGPT Plus login, `gpt-6-luna`, reasoning effort `medium`. The CLI/backend does not expose an immutable model snapshot. The revision field explicitly records that limitation. No seed or temperature control is claimed.
- Zero additional API spending. No API keys, alternate endpoints, credit purchases or reset-credit redemption. The existing subscription fee is not allocated as a per-token cost. Quota snapshots are account-wide observations and cannot attribute all usage changes to this suite.
- One fresh tool-free CLI turn per adapter invocation. The external runner owns read/list/write/build/run/submit. Full conversation history is serialized as user input under fixed adapter instructions. Final-message selection is checked against raw events; commentary, cached input, reasoning and CLI overhead remain in full-turn token accounting.
- Native tool events, visible transport errors/retries, missing counters or mismatched final messages stop accounting. Built-in transport retries cannot be fully disabled; conclusions concern the usage the CLI actually reports, not an independently audited backend workload.
- 35 turns, 65,536-token input reservation, 8,192-token output reservation, 2,000,000-token total task budget, 1,800-second task deadline and 900-second call deadline. CLI reservations are checked after generation, not enforced remotely as hard caps.
- Shared Linux host, sequential model calls, prewarmed package/runtime caches, pinned Docker image IDs, fresh task starters and disposable PostgreSQL/RabbitMQ checks. The baseline block precedes the guide block: time/provider-cache effects are potential confounders. Wall times are descriptive and do not establish a latency improvement.
- Checkers are outside model tool workspaces and verify actual service/database/concurrency behavior. Tasks/checkers are public and held out by protocol; they are not a secret generalization benchmark. Five repetitions characterize this sample only.

Compiler: `tinox 2.8.0`, SHA-256 `38bcd30d0e4107c4aa1e1fb854905d98944f5410210d505bb15f0e8971481674`. Input repository commit: `13d7f020683f2c3b5126ca6407004d3783734d19`; dirty-tree state and exact source/context/checker hashes are retained in each protocol. Source-declaration inventories are generated from the public source, with bodies omitted; compiler-intercepted ORM stubs still require the documented compiler semantics.

After both measured blocks finished, the adapter was integrated with current
develop, compiler 2.9.0, at `61221a91`. Its aggregate verification is recorded
separately below. The 2.8.0 measurements must not be presented as 2.9.0 results.

## Retained evidence and reproduction

- [Totals](totals.json) and [comparison](comparison.json) include all 60 final task runs. Each condition directory retains deterministic gzip copies of protocol, summary and every full task transcript/source.
- `exploratory/` retains 47 task attempts across startup-warning, commentary-selection,
  input-reservation and provider-call/deadline interruptions, the two Sol pilot runs
  and the Sol quota interruption before the 18:27 reset, plus the trivial CLI probe.
  These incomplete/pre-fix matrices are not pooled into the final comparison. No
  successful partial records were substituted into the fresh final matrix.
- `protocol-source/` retains the exact runner/adapter/comparison source hashes for all versions. `context/` retains the exact public documentation/inventory inputs. The full task/checker trees are those identified by the recorded base commit and hashes.
- [Manifest](manifest.json) contains raw/compressed evidence hashes. [Verification](verification.json) records the aggregate gate; its full log is retained as gzip.

To inspect a run, decompress its `.json.gz`. To repeat the comparison, decompress both final condition directories into new temporary directories and run `python3 benchmarks/ai/compare.py BASELINE GUIDE --variable context`. To perform new model runs, check out the recorded input commit, restore the recorded runner/adapter under `benchmarks/ai`, use a new output directory, restore the configuration, point context/adapter paths at local copies, verify the CLI/account/model availability, and explicitly pass `--execute`. The unavailable backend snapshot and server cache state prevent a claim of exact backend reproduction.
