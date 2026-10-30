# Multipart uploads (`tinox.core.bytes`)

```tinox
import tinox.core.bytes

@POST("/upload")
fnc upload(@HttpContext ctx: HttpContext) -> HttpContext {
    const r = Multipart::parse(ctx.request.getHeader("Content-Type"),
                               Bytes::fromRequestBody(ctx.request.body),
                               MultipartLimits::defaults())
    if (!r.ok) {
        ctx.response.badRequest("error: " + r.error)
        return ctx
    }
    const file = r.get("avatar")          // check r.has("avatar") first
    file.saveTo("/var/uploads/" + file.safeFileName())
    ...
}
```

- **Byte-exact.** Part values are `Bytes`; NUL, CR/LF and fake boundary lines inside a value survive.
- **Budgets** (`MultipartLimits`, defaults 100 parts, 1 MiB per field, 4 MiB per file, 8 KiB part headers). The
  HTTP server already caps the whole body at 4 MiB (413 above). Anything over a budget, a missing
  boundary, a truncated body, a part without a form-data name or a NUL in the headers gives
  `ok == false` with a reason -- never partial parts, never truncation. Answer it with 400.
- **No hidden resources.** The parser works in memory on the already bounded body and creates no temporary
  files or connections, so rejection, exceptions and cancellation leave nothing to clean up. Writing a file is
  an explicit `saveTo(path)`; `safeFileName()` gives the last path segment without control characters (never
  use the raw client `filename` as a path).
- `Bytes::fromRequestBody` works for handlers served by the compiler's HTTP/1 server (annotated routes).

Not covered (yet): streaming to disk beyond the 4 MiB body cap, `@MultipartParam` style binding,
`Content-Transfer-Encoding` other than binary, nested `multipart/mixed`.

Verified with curl's own multipart encoder (`crates/tinox/tests/multipart_upload.rs`).
