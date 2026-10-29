`Response<T>` adds dynamic JSON responses to annotation-driven handlers.
Import `tinox.core.http_server`; HTTP/3 handlers use the same type and
serialization path. This feature currently requires the library sources
from this checkout; no extended package has been published by this change.
See `examples/typed_rest/build.sh` for a local staging example.

```tnx
@GET
@Path("/users/:id")
fn getUser(@PathParam id: Int64) -> Response<User>
{
    if (id != 1) { return Response<User>::error(404, "user not found") }
    return Response<User>::ok(User { id: 1, name: "Ada" }).header("X-Result", "found")
}
```

The initial contract has three alternatives:

| Construction | Status | Body |
| --- | --- | --- |
| `Response<T>::ok(value)` | `@StatusCode`, or 200 | Serialized T |
| `.withStatus(code)` on a success | 200–299 except 204/205 | Serialized T |
| `Response<T>::error(code, message)` | 400–599 | `{"error": String}` |
| `Response<T>::noContent()` | 204 | Empty |

T supports the existing route return types: a `@JsonSerializable` class,
a list/array of such classes, String, Int64, Int32, or Bool. The wrapper
stores zero/one typed body values so errors need no fabricated null T.
Body and error JSON are produced centrally, including quote, backslash,
newline, and control-byte escaping. Headers are merged into the existing
response. The helper reserves Content-Type, Content-Length,
Transfer-Encoding, and Connection for serialization and transport framing.

`@StatusCode` sets the default success status only. Errors and no-content
responses override it. With typed responses it must be a body-bearing
2xx status. `@Produces` can be omitted or specify `application/json`;
other media types are compile errors. `@Consumes` retains the existing
request-content-type guard, including the pre-handler 415 response.
`@HttpContext` parameters cannot be mixed with a typed response; return
HttpContext for manual handling. Existing plain domain returns and the
raw HttpContext API continue to work.

Invalid dynamic statuses and malformed variants produce a JSON 500;
they never silently produce a success. `.withStatus` on an error changes
its error code, still constrained to 400–599. On noContent it must remain
204. Applications should construct wrappers through the helpers rather
than changing their ABI fields directly.

`tinox contract [file]` resolves imports and type-checks the application,
then writes OpenAPI 3.1 YAML to stdout. It includes paths, bound parameters,
request bodies, response alternatives, and schemas of annotated classes.
Class schemas follow inheritance and exclude `@DoNotSerialize` fields.
Existing serializer shapes without a reliable schema remain unconstrained
instead of advertising an incompatible field representation.
Typed success and error alternatives use status ranges because constructor
arguments may be dynamic; this is a contract of permitted outcomes, not
control-flow inference of the particular codes a handler uses. A specific
204 alternative has no content. The default success status is recorded in
`x-tinox-default-success-status`. Guard outcomes are included separately;
auth/content-type guards do not promise the typed handler's error shape.
Manual responses are marked `x-tinox-manual-response` and do not invent a
schema. Custom headers remain application-defined; the generator does not
infer their names from arbitrary handler code.

The dev UI `/routes` metadata exposes `responseMode`, `responseAlternatives`,
and the unwrapped success example. Contracts use the same route types and
annotations as compilation. Duplicate method/path entries fail contract
generation instead of silently overwriting an operation.

Verification: `cargo test --release -p tinox-typecheck typed_response`,
`cargo test --release -p tinox contract`, and
`cargo test --release -p tinox --test typed_rest_responses_curl`.
The curl test stages checkout libraries without a registry connection,
checks wire statuses/headers/JSON against the generated contract, and
repeats server startup and requests 15 times per supported transport.
HTTP/3 requires curl with HTTP/3 and ngtcp2/nghttp3 development libraries;
the TCP test always runs. The required final gate remains `make check`.

Request field constraints and pre-handler validation are described in [Recursive request DTO validation](request-validation.md).
