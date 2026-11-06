// libFuzzer harness for the MessagePack decoder (crates/tinox-core/msgpack,
// issue #136). Same driver-module + recompiled-IR technique as fuzz/hpack
// (see that harness's own comment for the full rationale) -- Msgpack::decode
// is pure Tinox operating on a List<Int64> byte array, no socket/conn
// dependency, so no bridging beyond the TinoxArray handle itself is needed.

#include <cstddef>
#include <cstdint>
#include <cstdlib>

extern "C" int64_t MsgpackDriver_tinoxMsgpackDecode(int64_t *bytes);

// Same layout as runtime.c's TinoxArray ({len, cap, data}) — a Tinox
// List<Int64> value IS a pointer to this struct (see fuzz/README.md /
// CLAUDE.md "Array-Handle-ABI").
struct TinoxArray {
    int64_t len;
    int64_t cap;
    int64_t *data;
};

// runtime.c's main() (renamed at compile time, see build.sh) calls into
// tinox_main() — a symbol codegen normally supplies for a real Tinox
// program with a `fn main()`; MsgpackDriver.tnx has none, so stub it here.
// Unreachable (libFuzzer's own main drives this binary) but the renamed
// main's body still references tinox_main, so the symbol must resolve at
// link time regardless — same as every other driver-module target here.
// tinox_main is now emitted by the driver itself (class Main), no stub needed.

extern "C" int LLVMFuzzerTestOneInput(const uint8_t *data, size_t size) {
    // Msgpack::decode's own bounds checks (declared length vs. actual
    // remaining bytes) already bound its own work to the input size, so
    // this cap is purely about keeping each libFuzzer iteration fast, same
    // rationale as fuzz/hpack's.
    if (size > 65536) return 0;

    TinoxArray *arr = (TinoxArray *)malloc(sizeof(TinoxArray));
    if (!arr) return 0;
    size_t n = size > 0 ? size : 1;
    int64_t *elems = (int64_t *)malloc(sizeof(int64_t) * n);
    if (!elems) { free(arr); return 0; }
    for (size_t i = 0; i < size; i++) {
        elems[i] = (int64_t)data[i]; // unsigned byte, matches fromCharCode's range
    }
    arr->len = (int64_t)size;
    arr->cap = (int64_t)size;
    arr->data = elems;

    MsgpackDriver_tinoxMsgpackDecode((int64_t *)arr);

    free(elems);
    free(arr);
    return 0;
}
