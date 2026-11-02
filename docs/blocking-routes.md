# Blocking routes: `@Blocking(workers, queue)`

An HTTP/1 worker thread multiplexes many connections with `epoll`, but it runs a route handler synchronously. A
handler that waits on a slow database or upstream call therefore also stalls every other connection that worker
serves. `@Blocking` gives such a route its own **bounded pool** (#552). It builds on the bounded executor
(`docs/SPEC.md`, "Bounded executor for many small jobs").

```tinox
class Reports {
    @GET
    @Path("/reports/:id")
    @Blocking(8, 32)                  // 8 worker threads, up to 32 waiting requests
    fn report(@PathParam("id") id: Int64) -> String {
        return Database::slowQuery(id)
    }
}
```

* **Where:** a REST method with `@GET`/`@POST`/`@PUT`/`@PATCH`/`@DELETE`. `workers` 1..256, `queue` 1..1,000,000, both
  integer literals; anything else is a compile error. Each annotated route has its own pool, so one slow route cannot
  fill another's. Imperative `server.get(...)` routes and the HTTP/2, HTTP/3 and TLS transports are not affected.
* **What happens:** the epoll worker reads the request, finds the route, copies the request and hands the connection to
  the pool, then returns to its other connections. A pool thread parses the request, runs the handler (request-scoped
  injection, `@Auth`, DTO validation, telemetry and `X-Request-Id` all happen there, as they would inline), writes the
  response and closes the connection.
* **Always `Connection: close`.** An offloaded response closes the connection; the epoll worker never takes a
  connection back. Clients that reuse connections reconnect on their next request to such a route; other routes on the
  same server keep their keep-alive connections.
* **Overload is visible and bounded.** When all workers are busy and the queue is full, the client gets `503 Service
  Unavailable` with `Retry-After: 1` and `{"error":"overloaded: the blocking route's queue is full"}` at once. There is
  no unbounded queue and no caller-runs fallback. `tinox_blocking_route_jobs_total` and
  `tinox_blocking_route_rejected_total` count accepted and refused requests.
* **Draining.** With `@Service`, a request is in-flight work from the moment it is queued, so a graceful stop waits for
  it. Requests that have not started when the service begins to stop are answered `503 Service draining`; running ones
  complete; the process then exits.
* **Sizing.** Waiting happens on pool threads, so `workers` should not exceed what the resource behind the route can
  serve (for example the database connection pool); an excess only moves the wait from the pool queue to the pool
  of connections. Queued requests do not time out on their own: size `queue` so that `queue / throughput` is an
  acceptable wait, and keep client-side deadlines.
* **Cancellation.** A request that is queued or running is not cancelled when the client disconnects (inline handlers
  are not either). A service stop is the cancellation point described above.

Measured (`crates/tinox/tests/blocking_route.rs`, one HTTP worker, a handler that sleeps 1 s): with three such requests
in flight, an unrelated fast route took about 2.8 s inline and 0.2 ms with `@Blocking(2, 4)`; of 12 simultaneous
requests to a `(2 workers, 4 queue)` route, 6 were served and 6 refused immediately.
