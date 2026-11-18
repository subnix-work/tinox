// libFuzzer harness for the HTTP/1 request framing helpers in runtime/runtime.c
// (http_request_framing, http_chunked_end, http_dechunk, http_headers_has_chunked_encoding,
// http_header_value, http_parse_url). They are `static`, so this file #includes runtime.c into
// its own translation unit instead of linking against it (build.sh renames runtime.c's main).
//
// The first input byte selects the target, the rest is the payload. Buffers are heap copies with
// EXACTLY size+1 bytes and a trailing NUL, mirroring conn_read_request (which NUL-terminates
// after every recv) -- so ASan flags any read past that NUL, which is where request-smuggling
// and over-read bugs live.

#include "../../runtime/runtime.c"

// Symbols compiled Tinox code normally defines; the harness never reaches them, but runtime.c
// references them.
__thread int64_t __tinox_err;
int64_t tinox_main(void) { return 0; }

#include <stdint.h>

int LLVMFuzzerTestOneInput(const uint8_t *data, size_t size) {
    if (size < 1 || size > 65536) return 0;
    uint8_t mode = data[0] % 5;
    size_t n = size - 1;
    char *buf = (char *)malloc(n + 1);
    if (!buf) return 0;
    memcpy(buf, data + 1, n);
    buf[n] = '\0';

    switch (mode) {
    case 0: { // server request framing, exactly as conn_read_request drives it
        char *hdr_end = strstr(buf, "\r\n\r\n");
        if (hdr_end) {
            long cl; int chunked;
            int framing = http_request_framing(buf, hdr_end, &cl, &chunked);
            if (!framing && chunked) {
                size_t end = 0;
                http_chunked_end(buf, n, (size_t)(hdr_end - buf) + 4, &end);
            }
        }
        break;
    }
    case 1: { // chunk scanner from an arbitrary offset
        size_t end = 0;
        size_t from = n ? (size_t)((uint8_t)buf[0]) % (n + 1) : 0;
        http_chunked_end(buf, n, from, &end);
        break;
    }
    case 2: { // client-side dechunking
        char *out = http_dechunk(buf);
        free(out);
        break;
    }
    case 3: { // header helpers
        http_headers_has_chunked_encoding(buf, n);
        char out[64];
        http_header_value(buf, n, "content-length", out, sizeof(out));
        break;
    }
    case 4: { // client URL parser
        char host[256], path[1024]; int port, https;
        http_parse_url(buf, host, sizeof(host), &port, path, sizeof(path), &https);
        break;
    }
    }
    free(buf);
    return 0;
}
