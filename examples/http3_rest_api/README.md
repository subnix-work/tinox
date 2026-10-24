# HTTP/3 REST API (tasks)

A small in-memory JSON REST API -- list/get/create/update/toggle-done/delete
on a `TodoItem { id, title, done }` resource -- served entirely over HTTP/3
(RFC 9114, QUIC/RFC 9000) via `tinox.core.http3_server.Http3Server`.

`Http3Server` has the exact same fluent route-registration API as
`tinox.core.http_server.HttpServer` (`get/post/put/patch/delete/use`,
`ctx.request`/`ctx.response`) -- if you've written a Tinox REST handler
before, this reads like any other one. The only differences: the
transport underneath is QUIC over UDP instead of TCP, and TLS is
mandatory (QUIC has no plaintext mode), so a certificate/key pair goes
straight into the constructor instead of a separate `listenTls()` call.

## 1. Build

HTTP/3 support is opt-in at build time (`TINOX_HTTP3=1`), since it links
against `ngtcp2`/`ngtcp2_crypto_ossl`/`nghttp3` -- unlike OpenSSL, these
aren't universally installed, so the flag defaults off rather than
breaking `tinox build` on machines that don't have them.

```sh
cd examples/http3_rest_api
openssl req -x509 -newkey rsa:2048 -nodes \
  -keyout key.pem -out cert.pem -days 365 -subj "/CN=localhost"

TINOX_HTTP3=1 tinox build src/main.tnx -o tasks_api
./tasks_api
```

You should see:

```
HTTP/3 tasks API listening on https://localhost:8843 (try: curl --http3-only -k https://localhost:8843/tasks)
```

## 2. Try it (needs an HTTP/3-capable curl -- `curl -V` should list `HTTP3`
in its Features line; this repo's dev environment has one)

```sh
# List the two seeded tasks
curl --http3-only -k https://localhost:8843/tasks

# Create a new one
curl --http3-only -k -X POST -H "Content-Type: application/json" \
  -d '{"id":0,"title":"Write a real REST example","done":false}' \
  https://localhost:8843/tasks
# {"id":3,"title":"Write a real REST example","done":false}

# Fetch it by id
curl --http3-only -k https://localhost:8843/tasks/3

# Toggle done
curl --http3-only -k -X PATCH https://localhost:8843/tasks/3/done

# Replace a task
curl --http3-only -k -X PUT -H "Content-Type: application/json" \
  -d '{"id":0,"title":"Renamed task","done":true}' \
  https://localhost:8843/tasks/1

# Delete one
curl --http3-only -k -X DELETE https://localhost:8843/tasks/2
# -> 204 No Content

# Missing id -> 404
curl --http3-only -k -i https://localhost:8843/tasks/999
# HTTP/3 404
# {"error":"task not found"}
```

`-k` skips certificate verification, since the cert generated above is
self-signed. `-v` on any of these shows the QUIC/TLS-1.3 handshake and
`ALPN, offering h3` / negotiated `h3` in the log.

## 3. The declarative version (`examples/http3_rest_api_annotated/`)

The exact same API, but written with the same `@GET`/`@POST`/`@PUT`/
`@PATCH`/`@DELETE`/`@Path`/`@StatusCode` annotations already used by the
plain (TCP) REST examples (`examples/rest_minimal`) -- the compiler
generates the entire `Http3Server::new()`/`.get()`/`.post()`/.../
`.listen()` wiring that this directory's `src/Main.tnx` writes out by
hand. It lives in a sibling example directory rather than right next to
this one's `src/Main.tnx`, since every Tinox program now requires exactly
one `class Main { fnc main() -> Int32 }` as its entry point, and a second
one can't also be named `Main.tnx` in the same directory:

```tinox
@ApplicationComponent
@Http3RestController(8843, "cert.pem", "key.pem")
class TaskController
{
    var tasks: List<TodoItem>

    @GET
    @Path("/tasks")
    fn listTasks(ctx: HttpContext) -> Nothing
    {
        ctx.response.json(Json::serialize(this.tasks))
    }
    // ...
}
```

`@Http3RestController(port, certPath, keyPath)` is what routes these
routes through `Http3Server` (QUIC) instead of the default, GC-crash-prone
TCP auto-server (issue #140) -- at most one per program, but it can now
freely be combined with `@WebsocketEndpoint`/`@Amqp10Consumer`/
`@Amqp091Consumer` (each is spawned on its own thread by the `Main.tnx`
bootstrap instead of competing for a single auto-run `main`).
`@ApplicationComponent` makes the compiler reuse one `TaskController`
instance across every request (instead of a fresh, zeroed one per call)
-- required here since `tasks` needs to persist between requests; see the
comment at the top of `TaskController.tnx` for why `tasks` is lazily
seeded in an `ensureInit()` method rather than a constructor.

Build and run it (own certs, same as step 1):

```sh
cd ../http3_rest_api_annotated
openssl req -x509 -newkey rsa:2048 -nodes \
  -keyout key.pem -out cert.pem -days 365 -subj "/CN=localhost"
TINOX_HTTP3=1 tinox build src/Main.tnx -o tasks_api_annotated
./tasks_api_annotated
```

Same routes, same responses, same port (8843) -- a separate example
directory, so both this one and the declarative one can run at the same
time if you want to compare them side by side.

## Notes

- Storage is a single in-memory `List<TodoItem>`, shared by every route
  handler's closure. `List<T>` is a reference-semantic handle (`push`,
  `pop`, and index-assignment all mutate the same underlying buffer every
  closure's captured copy of the handle points to), which is what makes
  this safe as a shared "database" with no locking -- `Http3Server`'s
  event loop is single-threaded. A plain `Int64` id counter would **not**
  work the same way (each closure captures its own independent copy of a
  scalar value at registration time), which is why `nextTaskId()` in
  `src/main.tnx` derives the next id from the tasks list itself instead
  of a separately captured counter, and why deleting a task shifts
  elements down and `pop()`s in place rather than reassigning `tasks` to
  a filtered copy.
- Restarting the server resets all data (nothing is persisted) -- this is
  a protocol demo, not a storage example (see `examples/crud` for a
  SQLite-backed one, over plain HTTP/1.1).
- See `docs.html`/`docs_en.html`'s `http3_server` module section for the
  full `Http3Server` API, build-flag details, and the one currently known
  limitation (0-RTT early data is wired end-to-end but not yet active,
  see there for why).
