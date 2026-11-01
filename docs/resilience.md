# Retry and circuit breaker (`@Retry`, `@CircuitBreaker`, `@Idempotent`)

Declarative resilience for calls to external services (issue #527). A failure is a thrown
`String`; the annotations decide which failures count.

```tinox
class Api
{
    var client: HttpClient

    @Retry(5, 50, ["UNAVAILABLE", "TIMEOUT"], 2000, 20, 10000)
    @Idempotent
    fn read(url: String) -> String { ... }

    @CircuitBreaker(5, 30000, ["UNAVAILABLE"])
    fn lookup(id: Int64) -> User { ... }
}
```

## `@Retry(maxAttempts, backoffMs, [prefixes] [, maxBackoffMs [, jitterPercent [, budgetMs]]])`

- `maxAttempts` (2–10) counts all attempts including the first.
- **Eligible failures are explicit:** only errors starting with one of the listed prefixes are
  retried (`"UNAVAILABLE: ..."` matches `"UNAVAILABLE"`). The list is required and non-empty.
- Wait before attempt *n+1*: `backoffMs · 2^(n-1)` capped at `maxBackoffMs` (default 30000),
  ± `jitterPercent` (default 20).
- `budgetMs` (default 30000, 0 = none) bounds the elapsed time since the first attempt: no
  retry starts if its wait would reach the budget.
- **Replay needs a declaration:** a method with `@Retry` must also carry `@Idempotent`, otherwise
  it is a compile error. Methods without `@Retry` are never replayed.
- **Cancellation wins:** the backoff wait is a cancellation point. A cancelled task, an expired
  task deadline or a service shutdown stops further attempts immediately and the error is
  rethrown; these errors are never retried.
- When all attempts fail, the last error is rethrown unchanged.

## `@CircuitBreaker(failureThreshold, openMs [, [prefixes] [, probes]])`

- **Closed:** calls run; `failureThreshold` *consecutive* eligible failures open the circuit
  (an empty/missing prefix list means every failure counts; a non-eligible error such as a
  domain `NOT_FOUND` counts as a success of the service).
- **Open:** calls are rejected without running the body, throwing
  `"CIRCUIT_OPEN: Class.method rejected the call"`. `CIRCUIT_OPEN` is never retried.
- **Half-open** after `openMs`: up to `probes` (default 1) calls run concurrently as probes; a
  successful probe closes the circuit, an eligible failure re-opens it for another `openMs`.
  Other callers are rejected while the probes are in flight.
- Cancellation errors neither count as success nor failure.
- Together with `@Retry` on the same method, the breaker sits inside the retry loop: each
  attempt asks the breaker first.

State is per annotated method (process-wide, shared by all instances) and thread-safe.

## Interaction with other annotations

`@Transactional` and `@Synchronized` apply to each attempt (a failed attempt rolls back before
the next one starts). All other annotations (`@GET`, `@Auth`, `@Log`, ...) stay on the public
method. Generic classes and methods are not supported.

## Metrics

Appended to `/metrics`, labelled only by the compile-time site (`Class.method`, so the
cardinality is bounded by the program text, at most 256 sites):

`tinox_resilience_retries_total`, `tinox_resilience_rejections_total`,
`tinox_resilience_breaker_opened_total`, `tinox_resilience_breaker_state`
(0 closed, 1 open, 2 half-open), `tinox_resilience_calls_failed_total`,
`tinox_resilience_calls_ok_total`.

## Implementation notes and limits

- The compiler (parser) rewrites the method into a wrapper that loops around the original body,
  which is kept as the private method `name__tinox_impl`; this name can show up in stack
  traces, call graphs and the debugger.
- Overload (503/`Retry-After`, task or connection budgets) is not special-cased: list the error
  prefixes you map those to. Retries add load; keep `maxAttempts` small and the budget tight.
- Trace relationship: every attempt runs inside the caller's context, so HTTP client spans of
  the attempts share the caller's trace; attempts are not separate spans of their own.
- Verified by `tests/e2e/resilience_policies` (deterministic transitions, 15 identical runs) and
  `crates/tinox/tests/resilience_http.rs` (real `HttpClient` against a failing raw-socket service).
