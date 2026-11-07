// libFuzzer harness for AMQP-0-9-1 frame reading
// (crates/tinox-core/amqp091/Amqp091.tnx::readFrame). Unlike HPACK,
// readFrame() doesn't take a buffer — it reads directly off a "conn"
// handle (a runtime.c TinoxConn* wrapping a socket fd) via httpConnReadN,
// which loops on conn_recv() until it gets N bytes or EOF. So the fuzz
// input can't be handed over as a plain byte array the way
// tinoxHpackDecode's List<Int64> is; it has to look like bytes arriving
// on a real socket.
//
// Bridge: a `socketpair(AF_UNIX, SOCK_STREAM, ...)`, write the whole fuzz
// input into one end, then `shutdown(SHUT_WR)` that end so the kernel
// signals EOF once the buffered bytes are drained — no feeder thread
// needed, since the input is small enough to fit in the socket's send
// buffer in one non-blocking write (see MAX_INPUT below) and
// httpConnReadN() already treats `conn_recv() <= 0` (EOF or error) as "no
// more data" instead of blocking (runtime.c:2654, `if (got <= 0) break;`)
// — confirmed by reading the source before relying on it here, not
// assumed. Amqp091Driver.tnx imports the real crates/tinox-core/amqp091
// module unmodified and adds a one-line wrapper (Amqp091Driver_tinoxAmqp091ReadFrame)
// that takes the already-wrapped conn handle and returns just the
// frameType, so this harness doesn't need to know AmqpFrame091's struct
// layout. build.sh compiles the driver via the real `tinox build` down to
// LLVM IR, then recompiles that IR with ASan + libFuzzer coverage
// instrumentation and links it against an instrumented runtime.c (which
// supplies httpConnFromFd) plus this file — same technique as
// fuzz/hpack, see fuzz/README.md.

#include <cstddef>
#include <cstdint>
#include <sys/socket.h>
#include <unistd.h>

extern "C" int64_t Amqp091Driver_tinoxAmqp091ReadFrame(int64_t conn);
extern "C" int64_t httpConnFromFd(int64_t fd);

// runtime.c's main() (renamed at compile time, see build.sh) calls into
// tinox_main() — a symbol codegen normally supplies for a real Tinox
// program with a `fn main()`; Amqp091Driver.tnx has none, so stub it
// here. The renamed main is unreachable (libFuzzer's own main drives this
// binary) but its body still references tinox_main, so the symbol must
// resolve at link time regardless — same as fuzz/hpack/hpack_fuzzer.cc.
// tinox_main is now emitted by the driver itself (class Main), no stub needed.

// readFrame() itself already rejects any declared frame size above
// 16777216 (16MB) before trying to read it (Amqp091.tnx: `size > 16777216`
// -> frameType -2), so a huge *declared* size can't make this hang or
// allocate unboundedly. This cap is just about keeping each libFuzzer
// iteration fast, same rationale as fuzz/hpack's 65536-byte cap.
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
    // Signals EOF on fds[1] once the buffered bytes are drained, so
    // httpConnReadN's read loop terminates instead of blocking even if
    // the input is shorter than whatever frame length it declares.
    shutdown(fds[0], SHUT_WR);

    int64_t conn = httpConnFromFd(static_cast<int64_t>(fds[1]));
    if (conn > 0) {
        Amqp091Driver_tinoxAmqp091ReadFrame(conn);
    }
    // File descriptors are a much scarcer resource than the heap memory
    // this harness otherwise leaks on purpose (-DTINOX_NO_GC, see
    // fuzz/README.md) — a typical `ulimit -n` (1024) would be exhausted
    // within a second of fuzzing if these weren't closed every iteration,
    // long before the memory-based rss_limit_mb stop condition the other
    // targets rely on ever kicks in. The malloc'd TinoxConn* itself stays
    // leaked (consistent with every other target); closing its underlying
    // fd directly is safe regardless — nothing calls conn_recv/conn_send
    // on this conn handle again after this iteration.
    close(fds[0]);
    close(fds[1]);
    return 0;
}
