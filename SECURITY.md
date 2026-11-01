# Security Policy

Tinox is a young, actively developed language implementation (compiler,
runtime, and standard library). It has not undergone an independent
security audit and should not yet be treated as hardened for running
untrusted code or handling untrusted network input in production.

## Reporting a Vulnerability

If you find a security issue — for example a memory-safety bug in the
runtime, a compiler bug that produces unsafe generated code, or a flaw in
a stdlib module such as `crypto`, `jwt`, `amqp091`/`amqp10` (TLS handling),
`http_server`, or `websocket` — please report it privately rather than
opening a public issue:

- Preferred: use [GitHub's private vulnerability reporting](https://github.com/subnix-work/tinox/security/advisories/new)
  for this repository.
- If that's not available to you, open a regular issue that asks for a
  private contact channel without including exploit details, and we'll
  follow up.

Please include:

- A description of the issue and its impact.
- Steps to reproduce (a minimal `.tnx` program, if applicable).
- Which component is affected (compiler/typechecker, codegen, C runtime,
  or a specific `tinox-core` module).

We'll acknowledge reports as soon as we can and work with you on a fix
and disclosure timeline. Once a fix is available, we'll publish it via a
GitHub issue/advisory following the project's normal bug-tracking
conventions.

## Scope notes

Current protocol support includes WebSocket fragment reassembly in
[`Ws::readMessage`](crates/tinox-core-ext/websocket/tinox/core/websocket/Ws.tnx)
and AMQP-091 publisher confirms through
[`confirmSelect` / `waitForConfirm`](crates/tinox-core-ext/amqp091/tinox/core/amqp091/AmqpChannel091.tnx).
The [fragmentation fixture](tests/e2e/ws_fragmentation/Main.tnx) covers
continuations, interleaved control frames and invalid frame sequences; the
[publisher-confirm fixture](tests/e2e/amqp091_publisher_confirms/Main.tnx)
covers positive and negative broker acknowledgements. These checks do not
constitute an independent security audit or exhaustive protocol validation.

Known boundaries remain: AMQP-091 consume and confirm traffic on the same
channel are not demultiplexed by method kind
([issue #541](https://github.com/subnix-work/tinox/issues/541)), and the blocking
confirm API does not provide a per-call deadline. TLS clients expose an explicit
`verify=false` opt-out for self-signed test certificates; it disables certificate
verification and must not be confused with authenticated TLS.

Design limitations and deferred work are tracked in
[GitHub issues](https://github.com/subnix-work/tinox/issues). Reports showing
that a documented limitation has a security impact are welcome through the
private reporting process above.
