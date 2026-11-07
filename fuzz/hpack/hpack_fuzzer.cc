// libFuzzer harness for the HPACK decoder (crates/tinox-core/hpack).
// Unlike JSON/ZIP, HPACK decoding is written in Tinox itself, not
// runtime.c, so there is no plain C parse(bytes) entry point to call
// directly. HpackDriver.tnx imports the real Hpack.tnx module unmodified
// and adds a one-line wrapper method (tinoxHpackDecode); build.sh compiles
// that driver down to LLVM IR via the real `tinox build`, then recompiles
// the emitted IR with ASan + libFuzzer coverage instrumentation (tinox
// build's own clang/opt/llc pipeline adds neither) and links it with
// runtime.c here. No copy of the HPACK parsing logic to keep in sync.
//
// Issue #149 stage 3: tinoxHpackDecode is now a static method of `class
// HpackDriver` (Tinox no longer allows top-level free functions), which
// mangles its compiled LLVM symbol to `HpackDriver_tinoxHpackDecode` --
// this extern declaration/call must track that mangled name exactly, since
// the link is a plain symbol reference with no indirection.

#include <cstddef>
#include <cstdint>
#include <cstdlib>

extern "C" int64_t HpackDriver_tinoxHpackDecode(int64_t *bytes);

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
// program with a `fn main()`; HpackDriver.tnx has none, so stub it here.
// The renamed main is unreachable (libFuzzer's own main drives this
// binary) but its body still references tinox_main, so the symbol must
// resolve at link time regardless — same as fuzz/json/json_fuzzer.cc.
// (Unlike the JSON/ZIP harnesses, __tinox_err needs no stub here: codegen
// always emits that global for any compiled Tinox file — driver_out.ll
// already defines it, redefining it here would collide.)
// tinox_main is now emitted by the driver itself (class Main), no stub needed.

extern "C" int LLVMFuzzerTestOneInput(const uint8_t *data, size_t size) {
    // Not a correctness workaround — HpackDynTable::add()'s eviction loop
    // is bounded by table size, not input size — just keeps each libFuzzer
    // iteration fast.
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

    HpackDriver_tinoxHpackDecode((int64_t *)arr);

    free(elems);
    free(arr);
    return 0;
}
