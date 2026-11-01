# Binary buffers and bounded streaming (#311)

The current checkout adds owned TCP/TLS streams, a bounded HTTP/1.1 exchange,
compact gzip and selected protocol bridges. Legacy APIs remain available.
These additions require a compiler/core build from this checkout and matching
workspace extended sources; no package is published by this change.

## Ownership and supported transports

`ByteStream::own(conn)` takes exclusive ownership of an already connected
TinoxConn, switches its socket to nonblocking mode, and supports plaintext TCP
or an already completed TLS connection. Do not mix the stream with other reads,
writes, a protocol dispatcher, or concurrent close on that connection. Dial and
TLS handshake deadlines belong to the setup API, not this stream.

- `read(maximum, timeout)` returns one transport chunk, up to 1 MiB. A successful
  zero-byte read is EOF. It does not wait for a full buffer or imply an HTTP frame.
- `write(bytes, timeout)` sends at most 1 MiB and waits for the peer to drain.
  It retries nonblocking/TLS readiness and EINTR, with a total operation deadline (1..86,400,000 ms).
- `StreamResult` carries code, operation, target, cause, bytes and actual
  transferred count. Zero code means success; other codes are Linux errno,
  including ETIMEDOUT and ECANCELED. A partial write is a failure with a nonzero
  transferred count; resending the whole buffer would duplicate bytes.
- Waits check cancellation at intervals of at most 10 ms. Cancellation is returned
  as ECANCELED, and every stream error closes the connection. This does not claim
  preemption of arbitrary foreign calls. A surrounding task scope retains its
  own cancellation semantics.
- `close()` is idempotent. Put it in `finally` for EOF, application exceptions and
  early returns too. Do not mutate a buffer until its synchronous write returns.

Memory per operation is bounded by the chunk limit; the API does not retain a
body history. Callers can still retain arbitrarily many returned buffers. GC
allocation bounds are not a promise of an exact RSS ceiling or zero copying.

## One streaming HTTP/1.1 exchange

`Http1Stream::receive(conn, timeout)` owns the connected TCP/TLS transport and
reads at most 16 KiB of headers under one total deadline, without pre-reading
body bytes. Its method, target, headers and remaining byte count describe the
request. `Content-Length` must be a single nonnegative decimal Int64; duplicate,
empty and overflowing values are rejected. Missing length means no body.
Transfer-Encoding is rejected explicitly. Header NULs, oversized/incomplete
headers and unsupported request-line versions fail and close the transport.

```tinox
import tinox.core.bytes

// conn is an accepted TCP or TLS TinoxConn, not a raw socket fd.
const request = Http1Stream::receive(conn, 2000)
try
{
    request.beginResponse(request.remaining, 2000)
    while (request.remaining > 0)
    {
        const got = request.readBody(65536, 2000)
        if (!got.isOk()) { throw got.cause }
        const sent = request.writeBody(got.bytes, 2000)
        if (!sent.isOk()) { throw sent.cause }
    }
    request.finish()
}
finally
{
    request.close()
}
```

The response contract is deliberately a 200 application/octet-stream response
with an exact Content-Length and Connection: close. `writeBody` rejects an
excess length; `finish` closes and rejects unread request bytes or an incomplete
response. Premature request EOF is an error, not a complete body. Chunked
transfer, pipelining, upgrades, trailers, HEAD-specific response semantics and
annotation-driven streaming handlers are outside this API. The existing
annotation server still materializes bodies; passing it `Bytes.asResponseBody`
does not make it a streaming server. Use bounded chunks and an enclosing task
deadline when an application also needs a total exchange deadline.

## Compact compression and protocol boundaries

`Bytes.gzip(maximum)` and `gunzip(maximum)` pass the compact input directly to
zlib and grow a compact output up to the caller's ceiling (1..64 MiB). Invalid,
truncated, CRC-invalid or trailing/concatenated gzip data fails; a valid empty
member remains valid. Output growth can copy; this is not a shared-view codec.
Malformed input or an exceeded ceiling throws, without a partial success value.

`Bytes.fromRequestBody(ctx.request.body)` and `asResponseBody()` now preserve
NUL/arbitrary bytes through the workspace HTTP/2 and HTTP/3 body bridges as well
as the existing HTTP/1.1 path. Call them in the live handler. String operations
still cannot process binary body content safely. HTTP/2 raw frame I/O no longer
uses strlen; its legacy frame/parser lists still exist, and response widening
is limited to one DATA frame at a time. HTTP/3 copies the exact byte length into
its response storage. Neither adapter promises bounded whole-body streaming.
HTTP/2 retains its existing non-Huffman HPACK and incomplete flow-control scope;
these byte tests do not establish general curl interoperability for HTTP/2.

`AmqpChannel091.publishBytes` accepts a compact message, retaining it compact
until copying/widening individual body frames (at most 1 MiB per chunk, subject
to negotiated frame size). As with legacy publish, use publisher confirms to
establish broker acceptance. `AmqpMessage091.bodyBytes()` copies a legacy body
into a compact snapshot; `setBodyBytes` copies/widens into the legacy body.
Neither aliases the original buffer. Receiving still accumulates a legacy body;
this is not an AMQP streaming/flow-control redesign. AMQP protocol connections
must not be handed to ByteStream while their dispatcher owns reads.

Slices, concatenation, text/list adapters and message snapshots copy. Mutating a
Bytes reference affects its aliases, not a copied slice or snapshot. Shared
views/buffer reuse remain deferred after profiling: the compact codec already
removes the measured full-message widening. A future view needs a traced parent
reference plus checked offset/length (the current atomic payload contains no
pointers), and a small slice would retain its entire parent allocation. Mutable
aliases would need an explicit immutability/borrowing rule and synchronization;
reused buffers must not be recycled before an I/O operation finishes. Those
lifetime/mutability costs need a concrete consumer before adding a second buffer
representation. The measurements do not establish a zero-copy design.

## Verification and measurements

- `cargo test --release -p tinox --test byte_stream`: 15 repetitions per case
  over TCP and TLS, including binary echo, an actual HTTP/1.1 exchange, blocked
  write backpressure, timeout and cancellation; independent Python clients.
- `cargo test --release -p tinox --test protocol_bytes`: independent HTTP/2
  non-Huffman frame client and curl HTTP/3, 15 binary round trips each. The
  messaging snapshot adapters are checked for independent copy ownership.
- `python3 tests/fixtures/protocol_bytes/check_broker.py "$PWD/target/release/tinox" "$PWD"`:
  15 real RabbitMQ trials, independent HTTP publication/consumption, multi-frame
  compact publication, matching confirms and byte comparisons.
- `python3 benchmarks/compact_bytes/measure.py --out /tmp/compact-measurement`:
  five rotated-order repetitions comparing legacy/compact reads and gzip,
  per-process RSS, GC allocation counters and independent Python decompression.

Reviewed measurement artifacts live under `benchmarks/compact_bytes/results/`.
For a deterministic 16 MiB random payload on this shared machine, compact reads
used about 25 MiB peak RSS versus 153 MiB for the legacy list path; compact gzip
used about 70 MiB versus 357 MiB. GC allocation totals were about 16/144 MiB for
reads and 80/352 MiB for gzip. These are workload-specific observed values,
including startup/GC behavior. The raw five-run values and variance are retained.
GC counters exclude allocations internal to native libraries; copy contracts
are source-inspected, not hardware copy counters. No universal latency, RSS,
cross-transport streaming, ABI rewrite or zero-copy guarantee follows.
