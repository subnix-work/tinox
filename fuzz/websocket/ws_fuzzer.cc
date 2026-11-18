// libFuzzer harness for WebSocket message reading (crates/tinox-core-ext/websocket,
// Ws::readMessageGeneric: frame header/length parsing, unmasking, fragment reassembly, control
// frames, UTF-8 and close-payload validation). Same socketpair bridge as fuzz/http2: the input is
// written into one end, which is then shut down for writing so reads see EOF once it is drained.
//
// The first input byte's low bit picks the role: 1 = server (client frames must be masked),
// 0 = client (server frames must be unmasked). The rest is the raw frame stream. Replies the
// library writes (pong, close) go to the unread end and are discarded.

#include <cstddef>
#include <cstdint>
#include <sys/socket.h>
#include <unistd.h>

extern "C" int64_t WsDriver_tinoxWsReadMessage(int64_t conn, int8_t requireMasked);
extern "C" int64_t httpConnFromFd(int64_t fd);

// Keeps each iteration fast; the library itself caps a frame at 16MB.
static constexpr size_t MAX_INPUT = 65536;

extern "C" int LLVMFuzzerTestOneInput(const uint8_t *data, size_t size) {
    if (size < 1 || size > MAX_INPUT) return 0;
    const bool server = (data[0] & 1) != 0;
    data++; size--;

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
        WsDriver_tinoxWsReadMessage(conn, server ? 1 : 0);
    }

    // Close both ends every iteration: fds are scarcer than the heap this -DTINOX_NO_GC harness leaks.
    close(fds[0]);
    close(fds[1]);
    return 0;
}
