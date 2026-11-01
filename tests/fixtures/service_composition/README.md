# Composed service recovery contract

`crates/tinox/tests/service_composition.rs` builds these annotation-driven
components together, rather than running unrelated framework examples:

- REST requests and WebSocket messages mutate the same injected, synchronized
  application component. Independent concurrent clients require exactly one
  occurrence of each counter value.
- Successful, explicitly failed and task-cancelled PostgreSQL transactions use
  a pool of two connections. A separate `psql` observer verifies committed
  balances, rollback and the absence of idle transactions.
- A bound, non-listening TCP socket supplies deterministic connection refusal.
  The route maps the existing HTTP client's status-zero failure to an explicit
  dependency-unavailable response; this does not imply the instance deadline
  API proposed in #525 already exists.
- Another transaction updates a row and blocks on a real independent HTTP
  dependency. SIGINT/SIGTERM initiates draining while that transaction is open.
  Releasing the dependency triggers an exception, rollback and an observed
  HTTP error response before WebSocket close and component disposal.
- After exit, the observer requires closed listeners, no PostgreSQL client
  backends and no surviving service process threads. A process-group watchdog
  removes the service if the whole scenario stalls.

Both protocols request OS-assigned ports and discover their actual listeners
through `serviceBoundPort`; the client identifies HTTP by its health response.
Every lane repeats the entire startup/failure/drain/disposal cycle 20 times.
Failure diagnostics identify the iteration and preserve stdout/stderr and the
staged project. The source lane stages current workspace package sources; the
fresh lane starts with an empty package home, installs pinned registry packages
and repeats the same independent checks.

Run with real Docker and PostgreSQL client tools available:

```sh
TINOX_REQUIRE_COMPOSITION=1 cargo test --release -p tinox --test service_composition -- --nocapture
```

The environment flag and GitHub Actions both make missing infrastructure a hard failure. Otherwise,
the test reports an explicit infrastructure skip, following the other real
PostgreSQL integration suites. Container startup, SQL errors, compilation,
package installation and protocol assertions always fail visibly.

This regression does not claim to prove the absence of every possible resource
leak. It checks externally observable listener/process/backend ownership after
each exit and transaction release between operations with a small live pool.
