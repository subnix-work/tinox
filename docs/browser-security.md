# Browser security: `@Cors` and `@Csrf`

Declarative, program-wide CORS and CSRF policy (issue #529). Put the annotations on any class
(by convention a small `Policy` class); there can be at most one `@Cors` and one `@Csrf` per
program. They are compiled into the binary and enforced in the runtime's HTTP/1 request
boundary, so they apply to **every route served by the runtime HTTP/1 server** — annotated
(`@GET`, `@POST`, …) and imperative (`HttpServer.get(...)`) alike — and to preflights, which
have no route of their own.

```tinox
@Cors(["https://app.example.com"], true, 600, ["Content-Type", "X-Request-Id"])
@Csrf(["https://admin.example.com"])
class Policy {
}
```

## `@Cors(origins, credentials, maxAgeSeconds[, allowedHeaders])`

- `origins`: exact `http(s)://host[:port]` strings, or `"*"` (only with `credentials=false`).
  No paths, no wildcards inside a host.
- `credentials`: sends `Access-Control-Allow-Credentials: true`.
- `allowedHeaders` (default `Content-Type`, `Authorization`, `X-Request-Id`): request headers a
  preflight may ask for.

## `@Csrf([trustedOrigins])`

An unsafe request (anything but GET/HEAD/OPTIONS) that carries a `Cookie` header must prove its
origin: the `Origin` header (or, if absent, the origin of `Referer`) must be same-origin (equal
to the `Host` header), one of the `@Cors` origins, or one of the `@Csrf` trusted origins.
No `Origin` and no usable `Referer` is a rejection. A `Sec-Fetch-Site: cross-site` request must
come from a trusted/CORS origin as well. Requests **without cookies** (bearer-token and other
non-browser clients) are not subject to CSRF checks — there is no ambient credential to abuse.

## Guard order (identical for every route)

1. Same-origin requests and requests without `Origin` skip the CORS step.
2. **CORS:** a foreign `Origin` that is neither a `@Cors` origin nor a `@Csrf` trusted origin
   → `403 CORS_ORIGIN_DENIED` (including `Origin: null`) before any handler runs.
3. **Preflight** (`OPTIONS` + `Origin` + `Access-Control-Request-Method`): allowed origin, a route
   that exists for the requested method and path, and only allowed headers → `204` with
   `Access-Control-Allow-Origin/-Methods/-Headers/-Max-Age`, `-Credentials`, `Vary: Origin`;
   otherwise `403 CORS_METHOD_DENIED` / `CORS_HEADER_DENIED`, with no allow-* header.
4. **Actual CORS request:** `Access-Control-Allow-Origin` (+ `Vary: Origin`, `-Credentials`) are
   added to the response, then the route runs.
5. **CSRF** as above → `403 CSRF_REJECTED`.
6. Route-level guards (`@Auth`, `@OIDCRolesAllowed`, …) and the handler.

Refusals use the standard error envelope (`{"error":{"code","message","correlationId"}}`) and
echo/generate `X-Request-Id`.

## Inspection

With `[dev] enabled = true` the Dev UI API serves the active policy at `GET /security`.

## Threat model and limits

- Protects against: cross-site form posts / fetches that ride on the browser's cookies, and
  cross-origin reads from origins that were never allowed.
- Does **not** include a token-based (double-submit) CSRF scheme, SameSite cookie management
  beyond `Cookie.sameSite` (#528), or protection against XSS. `Origin`/`Referer` checks assume a
  browser that sends them; non-browser clients can forge any header, which is why cookie-less
  requests are out of scope.
- The scheme of the `Origin` is not compared with the request's (the server cannot see a TLS
  terminator): `http://x` and `https://x` are both "same origin" for host `x`.
- Transports: the guard runs for HTTP/1 (runtime server and the Tinox-level `HttpServer`, http_server 1.2.1), HTTP/2 (`Http2Server`, http2_server
  1.1.3) and HTTP/3 (`Http3Server` incl. `@Http3RestController`, http3_server 1.2.0) through the
  same runtime function, so the order above and the error envelope are identical. The `Host` the
  same-origin check compares against comes from `:authority` on HTTP/2 and HTTP/3. Servers you
  write yourself on the raw `httpConn*`/socket primitives do not get the guard.
- Only the framework-wide policy exists; there is no per-route override yet. This feature has not
  been independently security-audited.

Verified by `crates/tinox/tests/browser_security_curl.rs` (curl as an independent browser-style
client over HTTP/1, `--http2-prior-knowledge` and `--http3-only`: preflight allow/deny for origin,
method and header; credentialed requests; opaque origin; same-origin; CSRF accept/reject matrix)
and annotation unit tests.
