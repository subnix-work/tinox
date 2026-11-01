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

## Binding by annotation

```tinox
@POST("/profile")
fnc save(@MultipartParam form: MultipartResult, @HttpContext ctx: HttpContext) -> HttpContext {
    ...
}
```

`@MultipartParam` parses the body with `MultipartLimits::defaults()` before the handler runs. A request that is not
`multipart/form-data`, has no valid boundary, is truncated/malformed or exceeds a budget is answered with **400** and the
reason; the handler never sees it. Call `Multipart::parse` yourself when you need other budgets.

## Large uploads and file ownership

- **Body cap:** the whole request body is held in memory while it is handled. The cap is `TINOX_MAX_BODY_BYTES` (or
  `tinox.limits.max_body_bytes` in `application.properties`), default 4 MiB, at most 1 GiB; a larger `Content-Length` is
  refused with 413 before anything is read. Budget `cap x concurrent uploads` as memory; the per-thread read buffer is
  released again after an upload above 16 MiB. It appears in the Dev UI `/limits` output as `maxBodyBytes`.
- **Spooling:** `r.spool(dir)` writes each file part to its own **new** file `dir/tinox-upload-<uuid>.part` (created with
  `O_EXCL`, no symlink following, mode 0600) and drops the in-memory copy (`part.data` is empty afterwards, `part.size`
  keeps the size). `part.open()` streams it back in bounded chunks (`ByteReader`).
- **Ownership:** the files belong to the `MultipartResult`. `r.release()` deletes them (idempotent, tolerant of vanished files).
  Write `defer r.release()` right after a successful parse: it runs on success, on an exception and on cancellation. If
  `spool` itself fails half-way it removes what it already wrote and throws.
- **After a crash:** `MultipartResult::sweep(dir, olderThanMs)` removes `tinox-upload-*.part` files older than the age
  (nothing else in the directory is touched); call it at startup with a generous age.

Not covered: reading the request body incrementally from the socket (the cap above is the memory bound), a request
streamed to disk before the handler runs, `Content-Transfer-Encoding` other than binary, nested `multipart/mixed`.

Verified with curl's own multipart encoder: `crates/tinox/tests/multipart_upload.rs` (binary values, budgets, 3 MB file)
and `crates/tinox/tests/multipart_spool.rs` (413 at the default cap, a 30 MB upload accepted with a raised cap, spooled,
streamed back and removed, removal also after an exception and after a rejection, `@MultipartParam` 200/400, the sweeper
keeping young and foreign files).
