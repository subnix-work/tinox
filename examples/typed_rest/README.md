Run `bash build.sh` with a freshly built compiler, then `.tinox/server`.
This example stages the extended library sources from this checkout;
the published http_server packages do not contain Response<T> yet.

The controller demonstrates domain-list returns alongside typed retrieval,
not-found errors, validation errors, headers, and empty deletion responses:

```sh
curl -i localhost:8080/users/1
curl -i localhost:8080/users/99
curl -i -H 'Content-Type: application/json' -d '{"id":3,"name":""}' localhost:8080/users
curl -i -X DELETE localhost:8080/users/1
../../target/release/tinox contract Main.tnx > .tinox/openapi.yaml
```

Expected statuses are 200, 404, 422, and 204. This is an in-memory demo:
creation echoes the validated input and deletion acknowledges a seeded ID;
neither persists changes. The older `rest_with_mini` example retains the
manual HttpContext response API.
