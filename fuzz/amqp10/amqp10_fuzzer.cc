// libFuzzer harness for AMQP-1.0 frame reading
// (crates/tinox-core/amqp10/Amqp10.tnx::readFrame). Same bridging
// technique as fuzz/amqp091/amqp091_fuzzer.cc (see its comment for the
// full rationale) — a socketpair with all fuzz bytes pre-written and the
// write end shut down, so httpConnReadN's `conn_recv() <= 0` EOF check
// terminates each read loop without a feeder thread. Amqp10Driver.tnx
// imports the real crates/tinox-core/amqp10 module unmodified and adds a
// one-line wrapper (Amqp10Driver_tinoxAmqp10ReadFrame) returning just the frameType,
// same shape as the HPACK/amqp091 drivers.

#include <cstddef>
#include <cstdint>
#include <sys/socket.h>
#include <unistd.h>

extern "C" int64_t Amqp10Driver_tinoxAmqp10ReadFrame(int64_t conn);
extern "C" int64_t httpConnFromFd(int64_t fd);

// tinox_main is now emitted by the driver itself (class Main), no stub needed.

// readFrame() itself already rejects a body size above 16777216 (16MB)
// before trying to read it (Amqp10.tnx: `bodyBytes > 16777216` ->
// frameType -2), same cap as amqp091 — this is just about keeping each
// libFuzzer iteration fast.
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

    int64_t conn = httpConnFromFd(static_cast<int64_t>(fds[1]));
    if (conn > 0) {
        Amqp10Driver_tinoxAmqp10ReadFrame(conn);
    }
    // See amqp091_fuzzer.cc's comment: fds must be closed every iteration
    // (unlike the heap, which this -DTINOX_NO_GC harness leaks on
    // purpose) or a typical `ulimit -n` is exhausted in well under a
    // second of fuzzing.
    close(fds[0]);
    close(fds[1]);
    return 0;
}
