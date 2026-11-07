// libFuzzer harness for HTTP/2 frame reading
// (crates/tinox-core/http2_server/Http2Server.tnx::readFrame, RFC 7540
// §4.1). Deferred in earlier revisions of fuzz/README.md as "meaningfully
// bigger" because readFrame() is an *instance* method needing a
// constructed Http2Server (routes, middleware, socket state) plus an
// Http2Conn (streams map, HPACK dynamic tables) rather than a free
// function like Amqp091::readFrame -- but neither constructor actually
// does any I/O or requires real routes/tables to be populated
// (Http2Server::new(port) and Http2Conn::new(handle) are both plain
// struct literals, confirmed by reading both before relying on it here),
// and readFrame() itself only touches `this.readBytes`, which is a thin
// wrapper over the same httpServerReadRawBytes native the amplification
// fix from the AMQP-0-9-1 target (issue #111) also covers server-side.
// So the scope here is exactly the frame-header/payload parsing layer
// this file's sibling targets already parse from, just entered via
// Http2Driver_tinoxHttp2ReadFrame instead of a free function.
//
// One difference from fuzz/amqp091: httpServerReadRawBytes(fd, count)
// (runtime.c) takes a raw fd and calls read() on it directly -- unlike
// httpConnReadN, which goes through a runtime.c TinoxConn* built via
// httpConnFromFd(). So Http2Conn::new(conn)'s `handle` field can be the
// socketpair fd directly; no httpConnFromFd() wrapping step is needed
// here at all.
//
// Bridge: same socketpair(AF_UNIX, SOCK_STREAM, ...) + shutdown(SHUT_WR)
// technique as fuzz/amqp091/amqp091_fuzzer.cc -- write the whole fuzz
// input into one end, shut it down for writing so the kernel signals EOF
// once the buffered bytes are drained (httpServerReadRawBytes's read()
// loop already treats a `read() <= 0` short/zero result as "stop", see
// runtime.c, not assumed), then read off the other end. No connection
// preface is fed in: readFrame() itself doesn't check for one (that's
// Http2Server::handleConnection's job, one layer up, not exercised by
// this target), so seeds are raw 9-byte-header(+payload) frames, not
// full connections.

#include <cstddef>
#include <cstdint>
#include <sys/socket.h>
#include <unistd.h>

extern "C" int64_t Http2Driver_tinoxHttp2ReadFrame(int64_t conn);
extern "C" int64_t httpConnFromFd(int64_t fd);

// runtime.c's main() (renamed at compile time, see build.sh) calls into
// tinox_main() -- a symbol codegen normally supplies for a real Tinox
// program with a `fn main()`; Http2Driver.tnx has none, so stub it here.
// Unreachable (libFuzzer's own main drives this binary) but the renamed
// main's body still references tinox_main, so the symbol must resolve at
// link time regardless -- same as every other driver-module target here.
// tinox_main is now emitted by the driver itself (class Main), no stub needed.

// readFrame()'s length field is a 24-bit wire value (RFC 7540 §4.1, up to
// ~16MB) and httpServerReadRawBytes additionally hard-caps at 16MB
// (TINOX_HTTP2_MAX_RAW_READ, runtime.c) regardless of what's declared --
// this cap is just about keeping each libFuzzer iteration fast, same
// rationale as fuzz/hpack's/fuzz/amqp091's input caps, not a safety
// requirement.
static constexpr size_t MAX_INPUT = 65536;

extern "C" int LLVMFuzzerTestOneInput(const uint8_t *data, size_t size) {
    if (size > MAX_INPUT) return 0;

    int fds[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, fds) != 0) return 0;

    size_t written = 0;
    while (written < size) {
        ssize_t w = write(fds[0], data + written, size - written);
        if (w <= 0) break;
        written += static_cast<size_t>(w);
    }
    shutdown(fds[0], SHUT_WR);

    // readFrame() takes a runtime TinoxConn* handle, not a raw fd (passing the fd
    // itself dereferenced a near-null pointer on every input, including the empty one).
    int64_t conn = httpConnFromFd(static_cast<int64_t>(fds[1]));
    if (conn > 0) {
        Http2Driver_tinoxHttp2ReadFrame(conn);
    }

    // Same fd-exhaustion rationale as fuzz/amqp091/amqp091_fuzzer.cc:
    // file descriptors are far scarcer than the heap memory this
    // -DTINOX_NO_GC harness otherwise leaks on purpose -- close both ends
    // every iteration instead of relying on the memory-based
    // -rss_limit_mb restart point.
    close(fds[0]);
    close(fds[1]);
    return 0;
}
