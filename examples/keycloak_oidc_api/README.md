# Keycloak-protected REST endpoint

A minimal REST API with one endpoint, `GET /api/hello`, that only answers
if the caller's OIDC access token is valid **and** contains a specific
realm role — declared entirely with a single annotation:

```tinox
@GET("/api/hello")
@OIDCRolesAllowed(["api-user"])
fnc hello(ctx: HttpContext) -> Nothing
{
    ctx.response.status(200).json("{\"message\":\"Hello, you are authorized!\"}")
}
```

`@OIDCRolesAllowed` wires up token verification automatically: the
compiler generates code that fetches the IdP's JWKS document live
(matching the token's `kid`), verifies the RS256 signature, and checks
`iss`/`aud`/`exp`/`nbf` and the required role — no key material is ever
hardcoded, and the handler above never has to know any of this happened.
The IdP connection itself (issuer, JWKS endpoint, expected audience) comes
from environment variables, so the same compiled binary works against any
OIDC provider without a rebuild; only the *required role* is a compile-time
property of the endpoint (like Jakarta EE's `@RolesAllowed`). This example
ships a `docker-compose.yml` that runs a real, pre-configured Keycloak so
you can try it end-to-end.

## 1. Start Keycloak

```sh
cd examples/keycloak_oidc_api
docker compose up -d
```

This imports a realm `tinox-demo` (`keycloak/realm-export.json`) with:

- a public client `tinox-api` (direct-access-grants enabled, so `curl` can
  fetch a token directly — see below)
- a realm role `api-user`
- two users: `alice` / `alice-pw` (**has** `api-user`) and `bob` / `bob-pw`
  (does **not**)

Wait until it's ready (first boot takes a few seconds):

```sh
until curl -s -o /dev/null -w "%{http_code}" http://localhost:8080/realms/tinox-demo | grep -q 200; do sleep 2; done
```

## 2. Run the app

```sh
export OIDC_ISSUER=http://localhost:8080/realms/tinox-demo
export OIDC_JWKS_URI=http://localhost:8080/realms/tinox-demo/protocol/openid-connect/certs
export OIDC_AUDIENCE=tinox-api

TINOX_PORT=8095 tinox run src/HelloController.tnx
```

(`TINOX_PORT` picks the listen port at *build* time — it's read by the
compiler, not the running program, since the port is baked into the
auto-generated `main()` the compiler synthesizes for annotation-driven
routes. Omit it to default to 8090.)

## 3. Get a token and call the endpoint

```sh
# Alice has the required role -> 200
TOKEN=$(curl -s -X POST http://localhost:8080/realms/tinox-demo/protocol/openid-connect/token \
  -d grant_type=password -d client_id=tinox-api -d username=alice -d password=alice-pw \
  | jq -r .access_token)
curl -i http://localhost:8095/api/hello -H "Authorization: Bearer $TOKEN"
# HTTP/1.1 200 OK
# {"message":"Hello, you are authorized!"}

# Bob is a valid user but lacks the role -> 403
TOKEN=$(curl -s -X POST http://localhost:8080/realms/tinox-demo/protocol/openid-connect/token \
  -d grant_type=password -d client_id=tinox-api -d username=bob -d password=bob-pw \
  | jq -r .access_token)
curl -i http://localhost:8095/api/hello -H "Authorization: Bearer $TOKEN"
# HTTP/1.1 403 Forbidden
# {"error":"missing required role (one of: api-user)"}

# No token at all -> 401
curl -i http://localhost:8095/api/hello
# HTTP/1.1 401 Unauthorized
# {"error":"missing bearer token"}
```

(No `jq`? Swap the `| jq -r .access_token` for
`| python3 -c "import json,sys;print(json.load(sys.stdin)['access_token'])"`.)

## Environment variables

| Variable          | Meaning                                    | Example (this compose file)                                             |
|--------------------|---------------------------------------------|-----------------------------------------------------------------------------|
| `OIDC_ISSUER`      | Expected `iss` claim                       | `http://localhost:8080/realms/tinox-demo`                                   |
| `OIDC_JWKS_URI`    | Where to fetch the IdP's public keys       | `http://localhost:8080/realms/tinox-demo/protocol/openid-connect/certs`     |
| `OIDC_AUDIENCE`    | Expected `aud` claim (this app's client ID)| `tinox-api`                                                                 |
| `TINOX_PORT`       | Build-time: port the compiled app listens on| `8095`                                                                     |

The required *role* (`api-user`) is not an environment variable — it's
declared right on the endpoint via `@OIDCRolesAllowed([...])`.

## 4. Browser login (a different pattern, `src/WebLogin.tnx`)

`HelloController.tnx` above is a *resource server*: it expects the caller
to already hold an access token and rejects anyone who doesn't, with a
plain `401` — it never redirects, the same way Jakarta EE's
`@RolesAllowed`/Spring's `@PreAuthorize` never redirect either. If you
instead want "visit a page, get bounced to Keycloak's login screen, get
bounced back" — the classic server-rendered web app pattern (Spring
Security's "OAuth2 Login") — that's a different piece of code,
`src/WebLogin.tnx`, built on `tinox.core.oidc`'s `OidcWebApp`: a reusable,
"batteries-included" class that registers `/login` + `/callback` and
manages the session for you (signed/encrypted, AES-256-GCM cookies — no
server-side session store), so the app itself only has to call
`app.requireLogin(ctx)` / `app.currentUser(ctx)` / `app.logout(ctx)`. See
`docs.html`/`docs_en.html`'s `oidc` module section for the full API.

`OidcWebApp` is built on the plain `tinox.core.http_server.HttpServer`
class (not the annotation-driven auto-server `HelloController.tnx` uses)
— this flow does real allocation (PKCE state, JSON parsing, a JWKS
fetch) on every login, and the annotation-driven auto-server has a known
crash under allocation-heavy routes (see the known issue below);
`HttpServer` doesn't share it.

```sh
export OIDC_ISSUER=http://localhost:8080/realms/tinox-demo
export OIDC_AUTHORIZE_URL=http://localhost:8080/realms/tinox-demo/protocol/openid-connect/auth
export OIDC_TOKEN_URL=http://localhost:8080/realms/tinox-demo/protocol/openid-connect/token
export OIDC_JWKS_URI=http://localhost:8080/realms/tinox-demo/protocol/openid-connect/certs
export OIDC_CLIENT_ID=tinox-api
export OIDC_REDIRECT_URI=http://localhost:8096/callback
export OIDC_REQUIRED_ROLE=api-user
export OIDC_COOKIE_SECRET=some-long-random-string
export PORT=8096

tinox run src/WebLogin.tnx
```

Then open `http://localhost:8096/` in a **browser** (not curl — it needs
to follow redirects and render Keycloak's login form): you'll land on
Keycloak's login page, log in as `alice`/`alice-pw` or `bob`/`bob-pw`,
and get bounced back to a page showing whether you have the `api-user`
role. `/logout` clears the session.

## Cleanup

```sh
docker compose down -v
```

## Notes

- `HelloController.tnx` validates a Bearer token presented by *some
  other* client — a resource server, not a login flow. `WebLogin.tnx` is
  the login flow that would obtain that token in the first place (or, in
  its own case, an OIDC session for itself). In a real app the two
  usually coexist: a frontend does the browser login, then calls the
  Bearer-token-protected API with the token it got.
- `@OIDCRolesAllowed` requires `import tinox.core.rest.server;` in the
  file that uses it (that's where the annotation and its backing
  verification logic, `OidcGuard`, both live).
- **Known issue (issue #140):** the compiler's auto-generated `main()`
  for annotation-driven routes (`tinox_HttpServer_listen` in `runtime.c`
  — a separate, epoll/thread-pool-based server implementation from the
  single-threaded `tinox.core.http_server.HttpServer` class used by
  `WebLogin.tnx`) can crash inside the Boehm GC after a handful of
  requests to *any* route that allocates heavily — not specific to OIDC
  or this example (reproduces with a trivial route that just does a
  tight string-concatenation loop, no crypto/JSON/network involved).
  Light, interactive testing (the walkthrough above) reliably works;
  hammering the endpoint with many rapid requests may crash the server.
  Root cause and diagnostics are in the issue; a fix is out of scope for
  this example.
