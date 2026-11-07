// Tinox Runtime

// Linux-only by design, not by oversight (#113): the HTTP/WebSocket/HTTP2
// event loop uses epoll (sys/epoll.h below, no kqueue/IOCP fallback), the
// event-driven socket code relies on Linux's MSG_NOSIGNAL, and the Boehm
// GC's stop-the-world suspend + crash backtraces (execinfo.h) assume a
// glibc/ELF target. Failing here with one clear message beats the reader
// hitting a cascade of "sys/epoll.h: No such file or directory" and
// similar errors scattered across this file with no indication of why —
// see CLAUDE.md's "no silent garbage" principle and
// https://github.com/subnix-work/tinox/issues/113 for what porting this
// to another OS would actually require.
#ifndef __linux__
#error "Tinox's runtime only supports Linux (epoll-based event loop, glibc/ELF-specific GC + backtraces) — see https://github.com/subnix-work/tinox/issues/113"
#endif

#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <string.h>
#include <ctype.h>
#include <math.h>
#include <pthread.h>
#include <semaphore.h>
#include <unistd.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <arpa/inet.h>
#include <poll.h>
#include <sys/uio.h>
#include <signal.h>
#include <sys/epoll.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <spawn.h>
#include <time.h>
#include <errno.h>
#include <sched.h>
#include <sys/stat.h>
#include <dirent.h>
#include <limits.h>
#include <zlib.h>
#include <stdatomic.h>
#include <fcntl.h>
#ifdef __GLIBC__
#include <execinfo.h>
#include <pty.h>
#include <termios.h>
#endif

#ifdef TINOX_NO_GC
// Sanitizer mode (make asan): plain malloc instead of Boehm GC, so ASan
// sees every allocation (the GC heap is invisible to ASan). Nothing gets
// freed — leaks are intentional here, overflows/UAF are the target.
#include <string.h>
#define GC_malloc(s)     calloc(1, (s))
#define GC_malloc_atomic(s) calloc(1, (s))
#define GC_malloc_uncollectable(s) calloc(1, (s))
#define GC_realloc(p, s) realloc((p), (s))
#define GC_free(p)       ((void)(p))
#define GC_strdup(s)     strdup(s)
#define GC_INIT()        ((void)0)
#define GC_gcollect()      ((void)0)
#define GC_get_heap_size() ((size_t)0)
#define GC_get_free_bytes() ((size_t)0)
#define GC_get_bytes_since_gc() ((size_t)0)
#define GC_get_total_bytes() ((size_t)0)
#define GC_get_gc_no() ((size_t)0)
#else
// Boehm GC — redirect all heap allocation through the collector
//
// GC_THREADS alone only enables the thread-aware *API* declarations in
// gc.h; it does NOT register threads with the collector. That needs
// GC_PTHREADS too, which pulls in gc_pthread_redirects.h and macro-
// redirects pthread_create/join/detach (used below by tinox_task_spawn
// for `spawn` and tinox_worker_run for the HttpServer thread pool) to
// GC_pthread_create/... . Without it, every thread this runtime spawns
// via plain pthread_create is *never* registered with the GC -- any
// GC_malloc from such a thread is undefined behavior, not just "slow":
// found via a crash inside GC_malloc_kind_global on an HttpServer worker
// thread doing heavy allocation (RS256/JWKS token verification, many
// small string concatenations) under concurrent load across the
// thread-per-CPU worker pool. <pthread.h> above must stay included
// before <gc.h> for the redirect macros to see the real prototypes first.
#define GC_THREADS
#define GC_PTHREADS
#include <gc.h>
#undef malloc
#undef calloc
#undef realloc
#undef free
#undef strdup
#define malloc(s)    GC_malloc(s)
#define calloc(n,s)  GC_malloc((size_t)(n)*(size_t)(s))
#define realloc(p,s) GC_realloc((p),(s))
#define free(p)      GC_free(p)
#define strdup(s)    GC_strdup(s)
#endif

// Memory allocation (kept for ABI compatibility — codegen calls these)
void* tinox_alloc(size_t size) {
    return GC_malloc(size);
}

void tinox_free(void* ptr) {
    GC_free(ptr);
}

// Bug 140: `malloc`/`calloc`/`realloc`/`free` are macro-redirected to
// GC_malloc/GC_realloc/GC_free above, so essentially every heap pointer this
// runtime holds -- including the many `static __thread` buffers reused
// across requests (recv/response/wrap buffers, the per-thread HttpContext-
// faking arrays and their TinoxMaps, the thrown-error slot) -- lives on the
// GC heap. Boehm's conservative collector scans the registered stack of
// each known thread plus the process's static data/BSS segments as roots,
// but does NOT automatically scan `__thread` (ELF TLS) storage: a TLS
// block is allocated by the dynamic linker/pthread machinery in its own
// region, separate from the regular static data segment GC_INIT()
// discovers. Any GC pointer reachable ONLY via a `__thread` variable is
// therefore invisible to the collector's root scan and can be collected
// out from under a thread that is still actively using it -- confirmed via
// a minimal repro (an allocation-heavy handler under `tinox_HttpServer_
// listen`'s epoll worker threads) that crashed inside GC-managed memory
// after a handful of requests with a stale/reused pointer, and which
// survived 400+ requests once these TLS roots were registered explicitly.
// `GC_add_roots()` on a `__thread` variable only covers the CALLING
// thread's TLS slot, so this must run once per thread (guarded by a
// per-thread flag) on EVERY thread that can execute Tinox code: the main
// thread, every `spawn`-created thread, and every HTTP worker thread. The
// function body is defined further down (after every `__thread` variable
// it registers has been declared); this forward declaration lets the
// earlier call sites (tinox_task_spawn, main) reference it.
static void tinox_gc_register_thread_roots(void);
static void tinox_gc_unregister_thread_roots(void);

// Print functions
void tinox_print_int(int64_t val) {
    printf("%ld", val);
}

void tinox_print_uint(int64_t val) {
    printf("%lu", (uint64_t)val);
}

void tinox_print_float(double val) {
    printf("%g", val);
}

void tinox_print_string(const char* val) {
    printf("%s", val);
}

void tinox_print_bool(bool val) {
    printf("%s", val ? "true" : "false");
}

void tinox_print_newline() {
    printf("\n");
}

// Monotonic milliseconds -- used by the compiler-generated @tinox_main
// bootstrap (emit_tinox_main_bootstrap in tinox-codegen) to measure and
// print its own startup time. CLOCK_MONOTONIC rather than
// CLOCK_REALTIME: only ever diffed against another call from the same
// process, so immune to wall-clock adjustments (NTP, timezone, manual
// changes) a two-call delta over REALTIME would otherwise pick up.
int64_t tinox_now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

// Issue #270 investigation: the tinox_ui WS-handshake read-timeout flake
// (WouldBlock on the client's handshake-response read) has never been
// reproduced under controlled conditions, so its "slow scheduling" vs
// "a race in the accept/upgrade path" question was never actually
// answered -- only mitigated (#245 raised the client's own read timeout
// from 5s to 30s). This is a permanent, always-compiled-in but
// opt-in-only diagnostic: checked once (a plain data race on the cached
// flag is harmless -- every thread converges on the same env var value,
// worst case a handful of extra getenv() calls very early in a run) and
// silent unless TINOX_WS_DEBUG=1 is set, so it costs nothing in the
// default case. Timestamps every step of the accept/handshake path
// (accept() returns, request-read starts/ends, response-send
// starts/ends) with the calling thread id and elapsed-ms-since-accept,
// so a future reproduction attempt can tell "the accept loop's own
// thread was simply descheduled for N seconds" (a wide gap with no
// activity in between) apart from "a race" (overlapping/out-of-order
// timestamps across two connections, or a send that starts before its
// own connection's read finished) on sight, without needing a debugger.
static int g_tinox_ws_debug = -1; // -1 = not yet checked, 0 = off, 1 = on
static bool tinox_ws_debug_enabled(void) {
    // Lazily initialised from several threads at once; the computed value is
    // idempotent, so relaxed atomics are enough to make the race well-defined.
    int state = __atomic_load_n(&g_tinox_ws_debug, __ATOMIC_RELAXED);
    if (state < 0) {
        const char* v = getenv("TINOX_WS_DEBUG");
        state = (v && v[0] == '1') ? 1 : 0;
        __atomic_store_n(&g_tinox_ws_debug, state, __ATOMIC_RELAXED);
    }
    return state == 1;
}
static void tinox_ws_debug_log(const char* fmt, ...) {
    if (!tinox_ws_debug_enabled()) return;
    fprintf(stderr, "[tinox-ws-debug t=%lldms tid=%lu] ", (long long)tinox_now_ms(), (unsigned long)pthread_self());
    va_list args;
    va_start(args, fmt);
    vfprintf(stderr, fmt, args);
    va_end(args);
    fprintf(stderr, "\n");
    fflush(stderr);
}

// Panic/Error handling
void tinox_panic(const char* msg) {
    fprintf(stderr, "PANIC: %s\n", msg);
    exit(1);
}

// Array allocation
void* tinox_array_alloc(size_t element_size, size_t length) {
    return calloc(element_size * length, 1);
}

// ---- Tinox arrays: stable handle {len, cap, data} ----
// A Tinox array value is a pointer to this 3-slot handle. push/pop/removeAt
// mutate the handle in place (push is amortized O(1) via geometric growth);
// slice/sort/reverse return fresh arrays. All aliases share the handle.
typedef struct {
    int64_t len;
    int64_t cap;
    int64_t* data;
} TinoxArray;

// ---- --checked: heap-kind registry (TESTPLAN phase 4) ----
// Arrays/maps are created exclusively through their constructors; in
// checked builds (-DTINOX_CHECKED, via `tinox build --checked`) those
// constructors register the pointer, and every array/map runtime function
// checks it. A dispatch bug (map_len on a string, array_push on a map)
// then aborts loudly instead of silently reading heap garbage
// (Bug-15 class). No ABI/layout difference from a normal build —
// the registry is a side table (plain malloc, invisible to the GC;
// address reuse is handled by re-registering in the constructor).
#ifdef TINOX_CHECKED
#define TINOX_KIND_ARRAY 1
#define TINOX_KIND_MAP 2

static pthread_mutex_t _tinox_ck_mu = PTHREAD_MUTEX_INITIALIZER;
static uintptr_t* _tinox_ck_keys = NULL;
static unsigned char* _tinox_ck_kinds = NULL;
static size_t _tinox_ck_cap = 0;
static size_t _tinox_ck_len = 0;

static void _tinox_ck_insert_raw(uintptr_t key, unsigned char kind) {
    size_t i = (key >> 4) & (_tinox_ck_cap - 1);
    while (_tinox_ck_keys[i] != 0 && _tinox_ck_keys[i] != key) {
        i = (i + 1) & (_tinox_ck_cap - 1);
    }
    if (_tinox_ck_keys[i] == 0) _tinox_ck_len++;
    _tinox_ck_keys[i] = key;
    _tinox_ck_kinds[i] = kind;
}

static void tinox_checked_register(const void* p, unsigned char kind) {
    if (!p) return;
    pthread_mutex_lock(&_tinox_ck_mu);
    if (_tinox_ck_len * 2 >= _tinox_ck_cap) {
        size_t new_cap = _tinox_ck_cap ? _tinox_ck_cap * 2 : 1024;
        uintptr_t* old_keys = _tinox_ck_keys;
        unsigned char* old_kinds = _tinox_ck_kinds;
        size_t old_cap = _tinox_ck_cap;
        _tinox_ck_keys = (uintptr_t*)calloc(new_cap, sizeof(uintptr_t));
        _tinox_ck_kinds = (unsigned char*)calloc(new_cap, 1);
        _tinox_ck_cap = new_cap;
        _tinox_ck_len = 0;
        for (size_t i = 0; i < old_cap; i++) {
            if (old_keys[i]) _tinox_ck_insert_raw(old_keys[i], old_kinds[i]);
        }
        free(old_keys);
        free(old_kinds);
    }
    _tinox_ck_insert_raw((uintptr_t)p, kind);
    pthread_mutex_unlock(&_tinox_ck_mu);
}

static const char* _tinox_ck_kind_name(unsigned char kind) {
    switch (kind) {
        case TINOX_KIND_ARRAY: return "Array";
        case TINOX_KIND_MAP: return "Map";
        default: return "unregistriert (String/Objekt?)";
    }
}

static void tinox_checked_expect(const void* p, unsigned char kind, const char* op) {
    if (!p) return;
    unsigned char found = 0;
    pthread_mutex_lock(&_tinox_ck_mu);
    if (_tinox_ck_cap) {
        uintptr_t key = (uintptr_t)p;
        size_t i = (key >> 4) & (_tinox_ck_cap - 1);
        while (_tinox_ck_keys[i] != 0) {
            if (_tinox_ck_keys[i] == key) { found = _tinox_ck_kinds[i]; break; }
            i = (i + 1) & (_tinox_ck_cap - 1);
        }
    }
    pthread_mutex_unlock(&_tinox_ck_mu);
    if (found != kind) {
        fprintf(stderr,
            "tinox --checked: %s auf %s-Pointer %p (erwartet: %s) — "
            "Codegen-Dispatch-Bug, bitte mit Quelldatei melden\n",
            op, _tinox_ck_kind_name(found), p, _tinox_ck_kind_name(kind));
        abort();
    }
}

#define TINOX_CK_REG(p, k) tinox_checked_register((p), (k))
#define TINOX_CK_EXPECT(p, k, op) tinox_checked_expect((p), (k), (op))
#else
#define TINOX_CK_REG(p, k) ((void)0)
#define TINOX_CK_EXPECT(p, k, op) ((void)0)
#endif

int64_t* tinox_array_new(int64_t len, int64_t cap) {
    if (cap < len) cap = len;
    if (cap < 4) cap = 4;
    // Bug 93: `cap` can be attacker-controlled (e.g. a WebSocket/AMQP frame
    // length flowing through httpConnReadN(conn, n)). `cap * sizeof(int64_t)`
    // used to be computed with no overflow check -- for cap around 2^61 that
    // wraps size_t to a tiny value, so GC_malloc below allocates far less
    // than `cap` claims while `a->cap` still stores the huge original value,
    // and later tinox_array_push() calls trust that capacity and write past
    // the undersized buffer. Reject instead of silently wrapping.
    if (cap < 0 || (uint64_t)cap > (SIZE_MAX / sizeof(int64_t))) {
        fprintf(stderr, "runtime error: array capacity %lld is invalid\n", (long long)cap);
        exit(1);
    }
    TinoxArray* a = (TinoxArray*)GC_malloc(sizeof(TinoxArray));
    a->len = len;
    a->cap = cap;
    a->data = (int64_t*)GC_malloc((size_t)cap * sizeof(int64_t));
    TINOX_CK_REG(a, TINOX_KIND_ARRAY);
    return (int64_t*)a;
}

// Checked integer division / modulo. Division by zero was LLVM UB (the optimizer
// folded `10/0` to a garbage value); INT64_MIN/-1 overflows and is also UB. Both
// are now a hard error / defined result instead of silent garbage.
void tinox_shift_out_of_range(int64_t count, int64_t bits) {
    fprintf(stderr, "runtime error: shift count %ld out of range for a %ld-bit value\n", (long)count, (long)bits);
    exit(1);
}
int64_t tinox_checked_sdiv(int64_t a, int64_t b) {
    if (b == 0) {
        fprintf(stderr, "runtime error: division by zero\n");
        exit(1);
    }
    if (a == INT64_MIN && b == -1) return INT64_MIN; // avoid overflow UB (wraps, as in Java)
    return a / b;
}
int64_t tinox_checked_srem(int64_t a, int64_t b) {
    if (b == 0) {
        fprintf(stderr, "runtime error: modulo by zero\n");
        exit(1);
    }
    if (a == INT64_MIN && b == -1) return 0; // avoid overflow UB
    return a % b;
}

// Bounds-checked element read. An out-of-range (or negative) index is a hard
// error with a clear message instead of reading past the buffer (the inline
// codegen version did an unchecked load → garbage / UB on out-of-bounds access).
int64_t tinox_array_get(int64_t* handle, int64_t idx) {
    TinoxArray* a = (TinoxArray*)handle;
    if (!a || idx < 0 || idx >= a->len) {
        fprintf(stderr, "runtime error: array index out of bounds: %ld (length %ld)\n",
                (long)idx, a ? (long)a->len : 0L);
        exit(1);
    }
    return a->data[idx];
}

// Bounds check for index assignment (`a[i] = v`): a store past the end used to
// write into adjacent heap memory.
void tinox_array_check_index(int64_t* handle, int64_t idx) {
    TinoxArray* a = (TinoxArray*)handle;
    if (!a || idx < 0 || idx >= a->len) {
        fprintf(stderr, "runtime error: array index out of bounds: %ld (length %ld)\n",
                (long)idx, a ? (long)a->len : 0L);
        exit(1);
    }
}

// String functions
int64_t tinox_string_length(const char* str) {
    int64_t len = 0;
    while (str[len] != '\0') len++;
    return len;
}

char* tinox_string_concat(const char* a, const char* b) {
    size_t len_a = strlen(a);
    size_t len_b = strlen(b);
    char* result = malloc(len_a + len_b + 1);
    memcpy(result, a, len_a);
    memcpy(result + len_a, b, len_b);
    result[len_a + len_b] = '\0';
    return result;
}

char* tinox_int_to_string(int64_t val) {
    char buf[32];
    int len = 0;
    int neg = val < 0;
    // Magnitude as unsigned: -INT64_MIN overflows int64_t (Bug 466).
    uint64_t mag = neg ? (uint64_t)0 - (uint64_t)val : (uint64_t)val;
    do { buf[len++] = '0' + (int)(mag % 10); mag /= 10; } while (mag > 0);
    if (neg) buf[len++] = '-';
    char* result = malloc(len + 1);
    for (int i = 0; i < len; i++) result[i] = buf[len - 1 - i];
    result[len] = '\0';
    return result;
}

char* tinox_uint_to_string(int64_t sval) {
    uint64_t val = (uint64_t)sval;
    char buf[32];
    int len = 0;
    do { buf[len++] = '0' + (val % 10); val /= 10; } while (val > 0);
    char* result = malloc(len + 1);
    for (int i = 0; i < len; i++) result[i] = buf[len - 1 - i];
    result[len] = '\0';
    return result;
}

char* tinox_float_to_string(double val) {
    char* result = malloc(40);
    // shortest representation that round-trips exactly
    for (int prec = 15; prec <= 17; prec++) {
        snprintf(result, 40, "%.*g", prec, val);
        if (strtod(result, NULL) == val) return result;
    }
    return result;
}

int64_t* tinox_array_slice(int64_t* h, int64_t from, int64_t to) {
    TINOX_CK_EXPECT(h, TINOX_KIND_ARRAY, "array_slice");
    TinoxArray* a = (TinoxArray*)h;
    if (from < 0 || to > a->len || from > to) {
        fprintf(stderr, "runtime error: slice range out of bounds: [%ld, %ld) (length %ld)\n",
                (long)from, (long)to, (long)a->len);
        exit(1);
    }
    int64_t len = to - from;
    int64_t* nh = tinox_array_new(len, 0);
    if (len > 0) memcpy(((TinoxArray*)nh)->data, a->data + from, (size_t)len * sizeof(int64_t));
    return nh;
}

int64_t* tinox_array_push(int64_t* h, int64_t val) {
    TINOX_CK_EXPECT(h, TINOX_KIND_ARRAY, "array_push");
    TinoxArray* a = (TinoxArray*)h;
    if (a->len == a->cap) {
        int64_t ncap = a->cap < 4 ? 4 : a->cap * 2;
        int64_t* nd = (int64_t*)GC_malloc((size_t)ncap * sizeof(int64_t));
        if (a->len > 0) memcpy(nd, a->data, (size_t)a->len * sizeof(int64_t));
        a->data = nd;
        a->cap = ncap;
    }
    a->data[a->len++] = val;
    return h;
}

int64_t* tinox_array_pop(int64_t* h) {
    TINOX_CK_EXPECT(h, TINOX_KIND_ARRAY, "array_pop");
    TinoxArray* a = (TinoxArray*)h;
    if (a->len == 0) {
        fprintf(stderr, "runtime error: pop() on an empty list\n");
        exit(1);
    }
    a->len--;
    return h;
}

// Serialize a list of @JsonSerializable objects: "[" + toJson(elem) joined
// with "," + "]". to_json is the class's generated ClassName_toJson.
char* tinox_json_list_serialize(int64_t* h, char* (*to_json)(void*)) {
    TinoxArray* a = (TinoxArray*)h;
    int64_t n = a ? a->len : 0;
    char** parts = (char**)malloc(sizeof(char*) * (n > 0 ? (size_t)n : 1));
    size_t total = 2; // "[" + "]"
    for (int64_t i = 0; i < n; i++) {
        parts[i] = to_json((void*)(uintptr_t)a->data[i]);
        total += strlen(parts[i]) + 1; // + ","
    }
    char* out = (char*)malloc(total + 1);
    size_t pos = 0;
    out[pos++] = '[';
    for (int64_t i = 0; i < n; i++) {
        if (i > 0) out[pos++] = ',';
        size_t l = strlen(parts[i]);
        memcpy(out + pos, parts[i], l);
        pos += l;
    }
    out[pos++] = ']';
    out[pos] = '\0';
    return out;
}

// Insert val at index idx (0..=len), shifting the tail right.
int64_t* tinox_array_insert(int64_t* h, int64_t idx, int64_t val) {
    TINOX_CK_EXPECT(h, TINOX_KIND_ARRAY, "array_insert");
    TinoxArray* a = (TinoxArray*)h;
    if (idx < 0 || idx > a->len) {
        fprintf(stderr, "runtime error: insert index out of bounds: %ld (length %ld)\n",
                (long)idx, (long)a->len);
        exit(1);
    }
    if (a->len == a->cap) {
        int64_t ncap = a->cap < 4 ? 4 : a->cap * 2;
        int64_t* nd = (int64_t*)GC_malloc((size_t)ncap * sizeof(int64_t));
        if (a->len > 0) memcpy(nd, a->data, (size_t)a->len * sizeof(int64_t));
        a->data = nd;
        a->cap = ncap;
    }
    memmove(a->data + idx + 1, a->data + idx, (size_t)(a->len - idx) * sizeof(int64_t));
    a->data[idx] = val;
    a->len++;
    return h;
}

char* tinox_char_at(const char* s, int64_t i) {
    int64_t len = s ? (int64_t)strlen(s) : 0;
    if (i < 0 || i >= len) {
        fprintf(stderr, "runtime error: string index out of bounds: %lld (length %lld)\n",
                (long long)i, (long long)len);
        exit(1);
    }
    char* result = malloc(2);
    result[0] = s[i];
    result[1] = '\0';
    return result;
}

// Single-char string from a byte value (fromCharCode builtin)
char* tinox_from_char_code(int64_t c) {
    char* result = malloc(2);
    result[0] = (char)c;
    result[1] = '\0';
    return result;
}

void tinox_print_char(int32_t c) {
    printf("%c", (char)c);
}

// Strict whole-string integer parse (optional sign, digits only), with
// overflow detection. Returns 1 and writes *out on success.
static int tinox_parse_i64_strict(const char* s, int64_t* out) {
    if (!s || !*s) return 0;
    const char* p = s;
    int neg = 0;
    if (*p == '-' || *p == '+') { neg = (*p == '-'); p++; }
    if (!*p) return 0; // just a sign, no digits
    int64_t result = 0;
    while (*p) {
        if (*p < '0' || *p > '9') return 0;
        int digit = *p - '0';
        // Accumulate negatively so INT64_MIN is representable.
        if (__builtin_mul_overflow(result, (int64_t)10, &result)) return 0;
        if (__builtin_sub_overflow(result, (int64_t)digit, &result)) return 0;
        p++;
    }
    if (!neg) {
        if (result == INT64_MIN) return 0;
        result = -result;
    }
    *out = result;
    return 1;
}

// String.toInt(): aborts on anything that is not a complete, in-range
// integer (use String.isInt() to validate untrusted input first).
int64_t tinox_string_to_int(const char* s) {
    int64_t v;
    if (!tinox_parse_i64_strict(s, &v)) {
        fprintf(stderr, "runtime error: cannot convert \"%s\" to Int64 (invalid or out of range)\n", s ? s : "(null)");
        exit(1);
    }
    return v;
}

int64_t tinox_string_is_int(const char* s) {
    int64_t v;
    return tinox_parse_i64_strict(s, &v) ? 1 : 0;
}

static int tinox_parse_f64_strict(const char* s, double* out) {
    if (!s || !*s) return 0;
    char* end = NULL;
    errno = 0;
    double result = strtod(s, &end);
    if (end == s || *end != '\0' || errno == ERANGE) return 0;
    if (isspace((unsigned char)*s)) return 0; // strtod skips leading whitespace
    *out = result;
    return 1;
}

// String.toFloat(): aborts on invalid input (use String.isFloat() first).
double tinox_string_to_float(const char* s) {
    double v;
    if (!tinox_parse_f64_strict(s, &v)) {
        fprintf(stderr, "runtime error: cannot convert \"%s\" to Float64 (invalid or out of range)\n", s ? s : "(null)");
        exit(1);
    }
    return v;
}

int64_t tinox_string_is_float(const char* s) {
    double v;
    return tinox_parse_f64_strict(s, &v) ? 1 : 0;
}

// Strict variants for @PathParam/@QueryParam binding (emit_route_shim_body,
// codegen.rs): tinox_string_to_int/_to_float above silently return 0/0.0 on
// garbage input, indistinguishable from a legitimate zero -- exactly the
// silent-garbage failure mode the REST parameter binding feature's 400
// response exists to prevent. These return 1 on success (writing *out) or 0
// on failure (empty string or any non-numeric content), so the caller can
// tell "absent/invalid" apart from "genuinely zero".
int tinox_parse_int_checked(const char* s, int64_t* out) {
    return tinox_parse_i64_strict(s, out);
}

int tinox_parse_float_checked(const char* s, double* out) {
    return tinox_parse_f64_strict(s, out);
}

int tinox_parse_bool_checked(const char* s, int* out) {
    if (!s) return 0;
    if (strcmp(s, "true") == 0) { *out = 1; return 1; }
    if (strcmp(s, "false") == 0) { *out = 0; return 1; }
    return 0;
}

// Quotes+escapes a bare string as a standalone JSON string value (not a
// key:value pair) -- backs REST auto-serialize responses whose return
// type is `String` (emit_route_shim_body, codegen.rs). Same escaping
// rules as jsonBuilderAddString (further down in this file), just
// without that function's object-key/comma bookkeeping.
char* tinox_json_encode_string(const char* s) {
    size_t sl = s ? strlen(s) : 0;
    char* buf = (char*)malloc(sl * 6 + 3); // allow six-byte JSON control escapes
    size_t len = 0;
    buf[len++] = '"';
    for (size_t i = 0; i < sl; i++) {
        unsigned char c = (unsigned char)s[i];
        if      (c == '"')  { buf[len++] = '\\'; buf[len++] = '"'; }
        else if (c == '\\') { buf[len++] = '\\'; buf[len++] = '\\'; }
        else if (c == '\n') { buf[len++] = '\\'; buf[len++] = 'n'; }
        else if (c == '\r') { buf[len++] = '\\'; buf[len++] = 'r'; }
        else if (c == '\t') { buf[len++] = '\\'; buf[len++] = 't'; }
        else if (c < 0x20) {
            static const char hex[] = "0123456789abcdef";
            buf[len++] = '\\'; buf[len++] = 'u';
            buf[len++] = '0'; buf[len++] = '0';
            buf[len++] = hex[c >> 4]; buf[len++] = hex[c & 15];
        }
        else                { buf[len++] = (char)c; }
    }
    buf[len++] = '"';
    buf[len] = '\0';
    return buf;
}

char* tinox_bool_to_string(int val) {
    const char* s = val ? "true" : "false";
    size_t len = val ? 4 : 5;
    char* result = malloc(len + 1);
    for (size_t i = 0; i <= len; i++) result[i] = s[i];
    return result;
}

// String utility functions
int64_t tinox_string_equals(const char* a, const char* b) {
    if (a == b) return 1;
    if (!a || !b) return 0;
    return strcmp(a, b) == 0 ? 1 : 0;
}

int64_t tinox_string_compare(const char* a, const char* b) {
    if (a == b) return 0;
    if (!a) return -1;
    if (!b) return 1;
    int r = strcmp(a, b);
    return r < 0 ? -1 : (r > 0 ? 1 : 0);
}

int64_t tinox_string_contains(const char* haystack, const char* needle) {
    return strstr(haystack, needle) != NULL ? 1 : 0;
}

int64_t tinox_string_index_of(const char* haystack, const char* needle) {
    const char* pos = strstr(haystack, needle);
    return pos ? (int64_t)(pos - haystack) : -1;
}

// Offsets and results use bytes, consistently with String.len().
int64_t tinox_string_index_of_from(const char* haystack, const char* needle, int64_t from) {
    if (from < 0) from = 0;
    if ((uint64_t)from > strlen(haystack)) return -1;
    const char* pos = strstr(haystack + from, needle);
    return pos ? (int64_t)(pos - haystack) : -1;
}

int64_t tinox_string_last_index_of(const char* haystack, const char* needle) {
    size_t hlen = strlen(haystack);
    size_t nlen = strlen(needle);
    if (nlen > hlen) return -1;
    for (size_t i = hlen - nlen + 1; i-- > 0; ) {
        if (memcmp(haystack + i, needle, nlen) == 0) return (int64_t)i;
    }
    return -1;
}

char* tinox_string_reverse(const char* s) {
    size_t len = strlen(s);
    char* result = malloc(len + 1);
    for (size_t i = 0; i < len; i++)
        result[i] = s[len - 1 - i];
    result[len] = '\0';
    return result;
}

char* tinox_string_to_upper(const char* s) {
    size_t len = strlen(s);
    char* result = malloc(len + 1);
    for (size_t i = 0; i <= len; i++)
        result[i] = (s[i] >= 'a' && s[i] <= 'z') ? s[i] - 32 : s[i];
    return result;
}

char* tinox_string_to_lower(const char* s) {
    size_t len = strlen(s);
    char* result = malloc(len + 1);
    for (size_t i = 0; i <= len; i++)
        result[i] = (s[i] >= 'A' && s[i] <= 'Z') ? s[i] + 32 : s[i];
    return result;
}

int64_t tinox_string_starts_with(const char* s, const char* prefix) {
    if (!s || !prefix) return 0;
    size_t plen = strlen(prefix);
    return strncmp(s, prefix, plen) == 0 ? 1 : 0;
}

int64_t tinox_string_ends_with(const char* s, const char* suffix) {
    size_t slen = strlen(s), suflen = strlen(suffix);
    if (suflen > slen) return 0;
    return strcmp(s + slen - suflen, suffix) == 0 ? 1 : 0;
}

char* tinox_string_trim(const char* s) {
    while (*s == ' ' || *s == '\t' || *s == '\n' || *s == '\r') s++;
    size_t len = strlen(s);
    while (len > 0 && (s[len-1] == ' ' || s[len-1] == '\t' || s[len-1] == '\n' || s[len-1] == '\r')) len--;
    char* result = malloc(len + 1);
    memcpy(result, s, len);
    result[len] = '\0';
    return result;
}

// Array utility functions
static int cmp_i64(const void* a, const void* b) {
    int64_t x = *(const int64_t*)a, y = *(const int64_t*)b;
    return (x > y) - (x < y);
}

// Fast inlined int64 sort: insertion sort for small ranges, quicksort otherwise.
// No function-pointer overhead, no temp-buffer malloc (unlike glibc qsort/msort).
static inline void i64_swap(int64_t* a, int64_t* b) { int64_t t = *a; *a = *b; *b = t; }

static void sort_i64_range(int64_t* arr, int64_t lo, int64_t hi) {
    while (lo < hi) {
        if (hi - lo < 16) {
            // Insertion sort: optimal for small ranges, no overhead
            for (int64_t i = lo + 1; i <= hi; i++) {
                int64_t key = arr[i];
                int64_t j = i;
                while (j > lo && arr[j-1] > key) { arr[j] = arr[j-1]; j--; }
                arr[j] = key;
            }
            return;
        }
        // Median-of-3 pivot to avoid worst-case
        int64_t mid = lo + (hi - lo) / 2;
        if (arr[lo] > arr[mid]) i64_swap(&arr[lo], &arr[mid]);
        if (arr[lo] > arr[hi])  i64_swap(&arr[lo], &arr[hi]);
        if (arr[mid] > arr[hi]) i64_swap(&arr[mid], &arr[hi]);
        int64_t pivot = arr[mid];
        i64_swap(&arr[mid], &arr[hi-1]);
        int64_t i = lo, j = hi - 1;
        while (1) {
            while (arr[++i] < pivot);
            while (arr[--j] > pivot);
            if (i >= j) break;
            i64_swap(&arr[i], &arr[j]);
        }
        i64_swap(&arr[i], &arr[hi-1]);
        // Recurse on smaller partition to bound stack depth
        if (i - lo < hi - i) { sort_i64_range(arr, lo, i - 1); lo = i + 1; }
        else                  { sort_i64_range(arr, i + 1, hi); hi = i - 1; }
    }
}

int64_t* tinox_array_sort(int64_t* h) {
    TINOX_CK_EXPECT(h, TINOX_KIND_ARRAY, "array_sort");
    TinoxArray* a = (TinoxArray*)h;
    int64_t len = a->len;
    int64_t* nh = tinox_array_new(len, 0);
    int64_t* nd = ((TinoxArray*)nh)->data;
    if (len > 0) memcpy(nd, a->data, (size_t)len * sizeof(int64_t));
    if (len > 1) sort_i64_range(nd, 0, len - 1);
    return nh;
}

// Natural-order sorts for String (by content) and Float64 (numerically) lists;
// tinox_array_sort above compares the raw words as signed integers (#459).
static int tinox_cmp_str_word(const void* a, const void* b) {
    const char* x = (const char*)(intptr_t)*(const int64_t*)a;
    const char* y = (const char*)(intptr_t)*(const int64_t*)b;
    return strcmp(x, y);
}
static int tinox_cmp_f64_word(const void* a, const void* b) {
    double x, y;
    memcpy(&x, a, sizeof x);
    memcpy(&y, b, sizeof y);
    return (x > y) - (x < y);
}
static int64_t* tinox_array_sort_with(int64_t* h, int (*cmp)(const void*, const void*)) {
    TINOX_CK_EXPECT(h, TINOX_KIND_ARRAY, "array_sort");
    TinoxArray* a = (TinoxArray*)h;
    int64_t len = a->len;
    int64_t* nh = tinox_array_new(len, 0);
    int64_t* nd = ((TinoxArray*)nh)->data;
    if (len > 0) memcpy(nd, a->data, (size_t)len * sizeof(int64_t));
    if (len > 1) qsort(nd, (size_t)len, sizeof(int64_t), cmp);
    return nh;
}
int64_t* tinox_array_sort_str(int64_t* h) { return tinox_array_sort_with(h, tinox_cmp_str_word); }
int64_t* tinox_array_sort_f64(int64_t* h) { return tinox_array_sort_with(h, tinox_cmp_f64_word); }

int64_t* tinox_array_reverse(int64_t* h) {
    TINOX_CK_EXPECT(h, TINOX_KIND_ARRAY, "array_reverse");
    TinoxArray* a = (TinoxArray*)h;
    int64_t len = a->len;
    int64_t* nh = tinox_array_new(len, 0);
    int64_t* nd = ((TinoxArray*)nh)->data;
    for (int64_t i = 0; i < len; i++) nd[i] = a->data[len - 1 - i];
    return nh;
}

int64_t tinox_array_contains(int64_t* h, int64_t val) {
    TINOX_CK_EXPECT(h, TINOX_KIND_ARRAY, "array_contains");
    TinoxArray* a = (TinoxArray*)h;
    for (int64_t i = 0; i < a->len; i++) if (a->data[i] == val) return 1;
    return 0;
}

// String-element variants: compare by content, not pointer identity.
int64_t tinox_array_contains_str(int64_t* h, const char* val) {
    TINOX_CK_EXPECT(h, TINOX_KIND_ARRAY, "array_contains");
    TinoxArray* a = (TinoxArray*)h;
    for (int64_t i = 0; i < a->len; i++) if (tinox_string_equals((const char*)a->data[i], val)) return 1;
    return 0;
}

int64_t tinox_array_index_of_str(int64_t* h, const char* val) {
    TINOX_CK_EXPECT(h, TINOX_KIND_ARRAY, "array_indexOf");
    TinoxArray* a = (TinoxArray*)h;
    for (int64_t i = 0; i < a->len; i++) if (tinox_string_equals((const char*)a->data[i], val)) return i;
    return -1;
}

int64_t tinox_array_index_of(int64_t* h, int64_t val) {
    TINOX_CK_EXPECT(h, TINOX_KIND_ARRAY, "array_indexOf");
    TinoxArray* a = (TinoxArray*)h;
    for (int64_t i = 0; i < a->len; i++) if (a->data[i] == val) return i;
    return -1;
}

int64_t* tinox_array_remove_at(int64_t* h, int64_t idx) {
    TINOX_CK_EXPECT(h, TINOX_KIND_ARRAY, "array_removeAt");
    TinoxArray* a = (TinoxArray*)h;
    if (idx < 0 || idx >= a->len) {
        fprintf(stderr, "runtime error: removeAt index out of bounds: %ld (length %ld)\n",
                (long)idx, (long)a->len);
        exit(1);
    }
    for (int64_t i = idx; i < a->len - 1; i++) a->data[i] = a->data[i + 1];
    a->len--;
    return h;
}

// ---- Observability (#305): W3C Trace Context and OTLP/HTTP JSON ----
// Fixed-size, value-only contexts can cross threads without sharing GC roots.
// No headers, bodies, URLs, exception text, or application fields are captured.
#include <sys/random.h>
static const char* tinox_config_lookup(const char* key);
void tinox_counter_inc(const char* name);
void tinox_histogram_record(const char* name, int64_t duration_ns);

typedef struct {
    char trace[33], span[17], parent[17], request[65], name[65];
    int sampled, kind;
    int64_t start, monotonic;
} TinoxTelemetryContext;
static __thread TinoxTelemetryContext telemetry_context;
static __thread TinoxTelemetryContext telemetry_stack[32];
static __thread int telemetry_depth;
static int telemetry_enabled, telemetry_sample = 10000;
static char telemetry_service[65] = "tinox";
static struct sockaddr_in telemetry_collector;
static int telemetry_export_enabled;
static pthread_once_t telemetry_once = PTHREAD_ONCE_INIT;
#define TINOX_TELEMETRY_QUEUE 128
static TinoxTelemetryContext telemetry_queue[TINOX_TELEMETRY_QUEUE];
static size_t telemetry_head, telemetry_count;
static int telemetry_busy;
static pthread_mutex_t telemetry_mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t telemetry_cv = PTHREAD_COND_INITIALIZER;

static int64_t telemetry_clock(clockid_t clock) {
    struct timespec ts;
    clock_gettime(clock, &ts);
    return (int64_t)ts.tv_sec * 1000000000LL + ts.tv_nsec;
}
static void telemetry_random(char* out, size_t bytes) {
    unsigned char data[16];
    size_t off = 0;
    while (off < bytes) {
        ssize_t n = getrandom(data + off, bytes - off, 0);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) { fprintf(stderr, "tinox: cannot generate telemetry ID\n"); exit(1); }
        off += (size_t)n;
    }
    // IDs must be nonzero, including in the astronomically unlikely all-zero draw.
    int nonzero = 0;
    for (size_t i = 0; i < bytes; i++) nonzero |= data[i];
    if (!nonzero) data[0] = 1;
    for (size_t i = 0; i < bytes; i++) {
        out[i*2] = "0123456789abcdef"[data[i] >> 4];
        out[i*2+1] = "0123456789abcdef"[data[i] & 15];
    }
    out[bytes*2] = 0;
}
static int telemetry_hex(char c) {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    return -1;
}
static int telemetry_parent(const char* p, TinoxTelemetryContext* ctx) {
    if (!p) return 0;
    size_t n = strnlen(p, 513);
    if (n < 55 || n > 512 || p[2] != '-' || p[35] != '-' || p[52] != '-') return 0;
    if (telemetry_hex(p[0]) < 0 || telemetry_hex(p[1]) < 0 || !strncmp(p, "ff", 2)) return 0;
    if (!strncmp(p, "00", 2) ? n != 55 : (n > 55 && (n < 57 || p[55] != '-'))) return 0;
    int trace_nonzero = 0, span_nonzero = 0;
    for (int i = 3; i < 55; i++) {
        if (i == 35 || i == 52) continue;
        int h = telemetry_hex(p[i]);
        if (h < 0) return 0;
        if (i < 35) trace_nonzero |= h;
        else if (i < 52) span_nonzero |= h;
    }
    if (!trace_nonzero || !span_nonzero) return 0;
    memcpy(ctx->trace, p + 3, 32); ctx->trace[32] = 0;
    memcpy(ctx->parent, p + 36, 16); ctx->parent[16] = 0;
    ctx->sampled = telemetry_hex(p[54]) & 1;
    return 1;
}
// Only ASCII tokens enter correlation fields/resource names. Invalid incoming
// request IDs are replaced, never truncated into colliding identifiers.
static int telemetry_token(const char* s) {
    if (!s || !*s || strnlen(s, 65) > 64) return 0;
    for (const unsigned char* p = (const unsigned char*)s; *p; p++)
        if (!( (*p >= 'a' && *p <= 'z') || (*p >= 'A' && *p <= 'Z') ||
               (*p >= '0' && *p <= '9') || *p == '-' || *p == '_' || *p == '.')) return 0;
    return 1;
}
static const char* telemetry_setting(const char* env, const char* key, const char* dflt) {
    const char* v = getenv(env);
    if (!v) v = tinox_config_lookup(key);
    return v && *v ? v : dflt;
}
static void telemetry_config_error(const char* key) {
    fprintf(stderr, "tinox: invalid telemetry configuration: %s\n", key);
    exit(1);
}
// All exporter I/O uses a single absolute 200ms deadline. Numeric IPv4 avoids
// unbounded DNS resolution. Deploy a local collector for TLS/auth/retry routing.
static int telemetry_wait(int fd, short events, int64_t deadline) {
    for (;;) {
        int64_t remaining = deadline - telemetry_clock(CLOCK_MONOTONIC);
        if (remaining <= 0) return 0;
        struct pollfd p = { fd, events, 0 };
        int rc = poll(&p, 1, (int)((remaining + 999999) / 1000000));
        if (rc < 0 && errno == EINTR) continue;
        return rc > 0 && (p.revents & events);
    }
}
static int telemetry_export(const TinoxTelemetryContext* c) {
    char body[1536], req[2048];
    int64_t end = c->start + c->monotonic; // queued monotonic stores elapsed ns
    int n = snprintf(body, sizeof(body),
        "{\"resourceSpans\":[{\"resource\":{\"attributes\":[{\"key\":\"service.name\","
        "\"value\":{\"stringValue\":\"%s\"}}]},\"scopeSpans\":[{\"scope\":{\"name\":\"tinox\"},"
        "\"spans\":[{\"traceId\":\"%s\",\"spanId\":\"%s\",\"parentSpanId\":\"%s\","
        "\"name\":\"%s\",\"kind\":%d,\"flags\":%d,\"startTimeUnixNano\":\"%lld\",\"endTimeUnixNano\":\"%lld\"}]}]}]}",
        telemetry_service, c->trace, c->span, c->parent, c->name, c->kind, c->sampled,
        (long long)c->start, (long long)end);
    int len = snprintf(req, sizeof(req),
        "POST /v1/traces HTTP/1.1\r\nHost: %s:%u\r\nContent-Type: application/json\r\n"
        "Connection: close\r\nContent-Length: %d\r\n\r\n%s",
        inet_ntoa(telemetry_collector.sin_addr), ntohs(telemetry_collector.sin_port), n, body);
    int fd = socket(AF_INET, SOCK_STREAM | SOCK_NONBLOCK, 0);
    if (fd < 0) return 0;
    int64_t deadline = telemetry_clock(CLOCK_MONOTONIC) + 200000000;
    int ok = 0;
    if (connect(fd, (struct sockaddr*)&telemetry_collector, sizeof(telemetry_collector)) < 0 && errno != EINPROGRESS) goto done;
    if (!telemetry_wait(fd, POLLOUT, deadline)) goto done;
    int error = 0; socklen_t error_len = sizeof(error);
    if (getsockopt(fd, SOL_SOCKET, SO_ERROR, &error, &error_len) || error) goto done;
    size_t off = 0;
    while (off < (size_t)len) {
        if (!telemetry_wait(fd, POLLOUT, deadline)) goto done;
        ssize_t sent = send(fd, req + off, (size_t)len - off, MSG_NOSIGNAL);
        if (sent < 0 && (errno == EINTR || errno == EAGAIN)) continue;
        if (sent <= 0) goto done;
        off += (size_t)sent;
    }
    char response[256]; off = 0;
    while (off < sizeof(response) - 1) {
        if (!telemetry_wait(fd, POLLIN, deadline)) goto done;
        ssize_t got = recv(fd, response + off, sizeof(response) - 1 - off, 0);
        if (got < 0 && (errno == EINTR || errno == EAGAIN)) continue;
        if (got <= 0) goto done;
        off += (size_t)got; response[off] = 0;
        if (strstr(response, "\r\n")) {
            // OTLP success is 200. No retries: collector owns durable delivery.
            ok = !strncmp(response, "HTTP/1.1 200 ", 13) || !strncmp(response, "HTTP/1.0 200 ", 13);
            break;
        }
    }
 done:
    close(fd);
    return ok;
}
static void* telemetry_worker(void* unused) {
    (void)unused;
    for (;;) {
        pthread_mutex_lock(&telemetry_mu);
        while (!telemetry_count) pthread_cond_wait(&telemetry_cv, &telemetry_mu);
        TinoxTelemetryContext c = telemetry_queue[telemetry_head];
        telemetry_head = (telemetry_head + 1) % TINOX_TELEMETRY_QUEUE;
        telemetry_count--; telemetry_busy = 1;
        pthread_mutex_unlock(&telemetry_mu);
        tinox_counter_inc(telemetry_export(&c) ? "tinox_telemetry_exported_total" : "tinox_telemetry_export_failed_total");
        pthread_mutex_lock(&telemetry_mu);
        telemetry_busy = 0;
        pthread_mutex_unlock(&telemetry_mu);
    }
    return NULL;
}
static void telemetry_init_once(void) {
    const char* enabled = telemetry_setting("TINOX_TELEMETRY_ENABLED", "tinox.telemetry.enabled", "false");
    if (strcmp(enabled, "true") && strcmp(enabled, "false")) telemetry_config_error("enabled (true/false)");
    telemetry_enabled = !strcmp(enabled, "true");
    const char* sample = telemetry_setting("TINOX_TELEMETRY_SAMPLE", "tinox.telemetry.sample", "10000");
    char* end; errno = 0;
    long rate = strtol(sample, &end, 10);
    if (errno || end == sample || *end || rate < 0 || rate > 10000) telemetry_config_error("sample (0..10000)");
    telemetry_sample = (int)rate;
    const char* service = telemetry_setting("TINOX_TELEMETRY_SERVICE", "tinox.telemetry.service", "tinox");
    if (!telemetry_token(service)) telemetry_config_error("service (1..64 ASCII token bytes)");
    strcpy(telemetry_service, service);
    const char* endpoint = telemetry_setting("TINOX_TELEMETRY_ENDPOINT", "tinox.telemetry.endpoint", "");
    if (*endpoint) {
        char ip[16]; unsigned port; int consumed = 0;
        if (sscanf(endpoint, "http://%15[0-9.]:%u/v1/traces%n", ip, &port, &consumed) != 2 ||
            !consumed || endpoint[consumed] || !port || port > 65535 ||
            inet_pton(AF_INET, ip, &telemetry_collector.sin_addr) != 1) telemetry_config_error("endpoint (http://IPv4:port/v1/traces)");
        telemetry_collector.sin_family = AF_INET;
        telemetry_collector.sin_port = htons((uint16_t)port);
        if (telemetry_enabled) {
            pthread_t worker;
            if (pthread_create(&worker, NULL, telemetry_worker, NULL)) telemetry_config_error("exporter thread");
            pthread_detach(worker);
            telemetry_export_enabled = 1;
        }
    }
}
void telemetryInit(void) { pthread_once(&telemetry_once, telemetry_init_once); }
int64_t telemetryEnabled(void) { return telemetry_enabled; }

static int64_t telemetry_enter(const char* name, const char* parent, const char* request, int kind) {
    if (!telemetry_enabled) return 0;
    if (telemetry_depth == 32) { tinox_counter_inc("tinox_telemetry_scope_rejected_total"); return 0; }
    telemetry_stack[telemetry_depth++] = telemetry_context;
    TinoxTelemetryContext old = telemetry_context;
    memset(&telemetry_context, 0, sizeof(telemetry_context));
    if (parent ? !telemetry_parent(parent, &telemetry_context) : !old.trace[0]) {
        telemetry_random(telemetry_context.trace, 16);
        unsigned draw = 0;
        for (int i = 28; i < 32; i++) draw = draw * 16 + (unsigned)telemetry_hex(telemetry_context.trace[i]);
        telemetry_context.sampled = draw < (unsigned)((65536ULL * telemetry_sample) / 10000);
    } else if (!parent) {
        strcpy(telemetry_context.trace, old.trace);
        strcpy(telemetry_context.parent, old.span);
        telemetry_context.sampled = old.sampled;
    }
    telemetry_random(telemetry_context.span, 8);
    const char* id = request && *request ? request : parent ? NULL : old.request;
    strcpy(telemetry_context.request, telemetry_token(id) ? id : telemetry_context.trace);
    strcpy(telemetry_context.name, telemetry_token(name) ? name : "operation");
    telemetry_context.kind = kind;
    telemetry_context.start = telemetry_clock(CLOCK_REALTIME);
    telemetry_context.monotonic = telemetry_clock(CLOCK_MONOTONIC);
    return telemetry_depth;
}
int64_t telemetryBegin(const char* name, const char* parent, const char* request) {
    // Empty parent continues the current operation; incoming adapters pass a
    // non-NULL parent directly to telemetry_enter to force a fresh boundary.
    return telemetry_enter(name, parent && *parent ? parent : NULL, request, 1);
}
int64_t telemetryEnd(int64_t token) {
    if (!token) return 1;
    if (token != telemetry_depth) return 0;
    TinoxTelemetryContext c = telemetry_context;
    c.monotonic = telemetry_clock(CLOCK_MONOTONIC) - c.monotonic;
    const char* metric = c.kind == 2 ? "tinox_http_server_duration_ns" :
                         c.kind == 3 ? "tinox_http_client_duration_ns" : "tinox_operation_duration_ns";
    tinox_histogram_record(metric, c.monotonic);
    if (c.sampled && telemetry_export_enabled) {
        // Producers never wait behind network I/O. Full queues drop newest.
        pthread_mutex_lock(&telemetry_mu);
        if (telemetry_count < TINOX_TELEMETRY_QUEUE) {
            telemetry_queue[(telemetry_head + telemetry_count) % TINOX_TELEMETRY_QUEUE] = c;
            telemetry_count++;
            pthread_cond_signal(&telemetry_cv);
        } else tinox_counter_inc("tinox_telemetry_dropped_total");
        pthread_mutex_unlock(&telemetry_mu);
    }
    telemetry_context = telemetry_stack[--telemetry_depth];
    return 1;
}
// Automatic boundaries also close application scopes abandoned by an error
// path, so a reused worker cannot leak context into the next request.
static void telemetry_leave(int64_t token) {
    if (!token) return;
    while (telemetry_depth >= token) telemetryEnd(telemetry_depth);
}
// Automatic message boundaries (WebSocket @OnMessage, AMQP consumers, issue #548): every message
// starts a fresh consumer span (kind 5) so no context from a previous message on a reused worker
// can leak into it; the scope is closed (together with anything the handler left open) afterwards.
int64_t tinox_telemetry_message_begin(const char* name) { return telemetry_enter(name, "", NULL, 5); }
void tinox_telemetry_message_end(int64_t token) { telemetry_leave(token); }
char* telemetryTraceparent(void) {
    char p[56] = "";
    if (telemetry_context.trace[0]) snprintf(p, sizeof(p), "00-%s-%s-%02x", telemetry_context.trace, telemetry_context.span, telemetry_context.sampled);
    return GC_strdup(p);
}
char* telemetryRequestId(void) { return GC_strdup(telemetry_context.request); }
// An explicit bounded best-effort drain for short-lived apps; never called by
// a handler. Pending spans may be lost on process exit or exporter failure.
int64_t telemetryFlush(void) {
    if (!telemetry_export_enabled) return 1;
    int64_t deadline = telemetry_clock(CLOCK_MONOTONIC) + 250000000;
    do {
        pthread_mutex_lock(&telemetry_mu);
        int pending = telemetry_count || telemetry_busy;
        pthread_mutex_unlock(&telemetry_mu);
        if (!pending) return 1;
        struct timespec pause = {0, 1000000}; nanosleep(&pause, NULL);
    } while (telemetry_clock(CLOCK_MONOTONIC) < deadline);
    return 0;
}
// Bounded JSON string escaping, including every control byte. Preserve UTF-8
// boundaries when truncating. The caller's existing @Sensitive/@Masked
// toString conversion happens before logging, exactly as for text output.
static void telemetry_quote(char** out, const char* s, size_t limit) {
    *(*out)++ = '"';
    if (!s) s = "";
    size_t n = strnlen(s, limit + 1);
    if (n > limit) { n = limit; while (n && ((unsigned char)s[n] & 0xc0) == 0x80) n--; }
    for (size_t i = 0; i < n; i++) {
        unsigned char c = (unsigned char)s[i];
        if (c == '"' || c == '\\') { *(*out)++ = '\\'; *(*out)++ = (char)c; }
        else if (c < 32) { int len = sprintf(*out, "\\u%04x", c); *out += len; }
        else *(*out)++ = (char)c;
    }
    *(*out)++ = '"'; **out = 0;
}
char* telemetryLogJson(const char* timestamp, const char* level, const char* logger, const char* message) {
    char buf[32768], *out = buf;
    const char* keys[] = {"timestamp", "level", "logger", "message", "request_id", "trace_id", "span_id"};
    const char* vals[] = {timestamp, level, logger, message, telemetry_context.request, telemetry_context.trace, telemetry_context.span};
    const size_t limits[] = {64, 16, 128, 4096, 64, 32, 16};
    *out++ = '{';
    for (int i = 0; i < 7; i++) {
        if (i) *out++ = ',';
        telemetry_quote(&out, keys[i], 32); *out++ = ':';
        telemetry_quote(&out, vals[i], limits[i]);
    }
    *out++ = '}'; *out = 0;
    return GC_strdup(buf);
}
void telemetryWriteLogJson(const char* timestamp, const char* level, const char* logger, const char* message) {
    // One stdio operation keeps concurrent structured records on separate lines.
    puts(telemetryLogJson(timestamp, level, logger, message));
}
static void telemetry_event(const char* event) {
    char stamp[32]; snprintf(stamp, sizeof(stamp), "%lld", (long long)telemetry_clock(CLOCK_REALTIME));
    puts(telemetryLogJson(stamp, "INFO", "tinox", event));
}

// Request providers belong to exactly one synchronous annotated REST dispatch.
// The cache is a GC-managed graph rooted explicitly for each participating
// thread; clearing the root releases ownership, not native resource handles.
typedef struct TinoxRequestEntry {
    int64_t key;
    int64_t* instance;
    struct TinoxRequestEntry* next;
} TinoxRequestEntry;
static __thread TinoxRequestEntry* request_instances;
static __thread int request_active;
static void request_scope_fail(const char* message) {
    fprintf(stderr, "Request scope error: %s\n", message);
    exit(1);
}
void tinox_request_begin(void) {
    if (request_active) request_scope_fail("nested request dispatch is unsupported");
    tinox_gc_register_thread_roots();
    request_instances = NULL;
    request_active = 1;
}
void tinox_request_end(void) {
    request_instances = NULL;
    request_active = 0;
}
int64_t* tinox_request_get(int64_t key) {
    if (!request_active) request_scope_fail("provider used outside annotated REST dispatch");
    for (TinoxRequestEntry* e = request_instances; e; e = e->next)
        if (e->key == key) return e->instance;
    return NULL;
}
void tinox_request_put(int64_t key, int64_t* instance) {
    if (!request_active) request_scope_fail("provider used outside annotated REST dispatch");
    TinoxRequestEntry* e = GC_malloc(sizeof(*e));
    e->key = key; e->instance = instance; e->next = request_instances;
    request_instances = e;
}

// ---- Async runtime ----

// Structured tasks (#301). Handles are monotonically allocated IDs, never
// unchecked addresses. The active registry roots task data for the GC and
// rejects forged, detached, or expired handles without dereferencing them.
extern __thread int64_t __tinox_err;
typedef struct TinoxTask TinoxTask;
typedef struct TinoxTaskScope TinoxTaskScope;
struct TinoxTaskScope {
    _Atomic(TinoxTaskScope*) parent;
    atomic_bool cancelled;
    _Atomic int64_t deadline;
    int closed;
    TinoxTask* children; // modified only by the owning thread
    TinoxTask* children_tail;
};
struct TinoxTask {
    uintptr_t id;
    pthread_t thread;
    pthread_t creator_thread;
    pthread_mutex_t mu;
    pthread_cond_t cond;
    int done, joined, observed, detached;
    int budgeted; // holds a slot of the TINOX_MAX_TASKS budget (#302)
    int64_t executor; // id of the bounded executor running this job (#551), 0 for a thread-per-task spawn
    void* retval;
    int64_t error;
    TinoxTaskScope* scope;
    TinoxTaskScope* owner;
    TinoxTask* sibling;
    TinoxTask* previous;
    TinoxTask* registry_next;
    TinoxTask* waiting_on; // protected by task_registry_mu
};
static __thread TinoxTaskScope* task_scope;
static __thread TinoxTaskScope* task_frame_scope;
static __thread TinoxTask* current_task;
static __thread int64_t current_executor_id; // set on executor worker threads (#551)
static pthread_mutex_t task_registry_mu = PTHREAD_MUTEX_INITIALIZER;
// Bucket heads are also conservative GC roots for all active tasks.
#define TINOX_TASK_BUCKETS 1024
static TinoxTask* task_registry[TINOX_TASK_BUCKETS];
static uintptr_t task_next_id = 1;
static __thread int service_task_id = -1;
static __thread int service_work_depth;
static __thread int service_cleanup_mode;
static int service_request_if_active(void);
static int service_enabled(void);
static int service_should_cancel(void);
bool serviceIsStopping(void);
static atomic_bool task_shutdown_requested;
static atomic_bool task_shutdown_complete;
static const char task_cancelled_error[] = "Task cancelled";
static const char task_deadline_error[] = "Task deadline exceeded";
static const char task_shutdown_error[] = "Task service shutdown requested";


static void task_fatal(const char* message) {
    fprintf(stderr, "tinox: %s\n", message);
    exit(1);
}
static void task_pthread_check(int rc) {
    if (rc) {
        fprintf(stderr, "tinox: task pthread operation failed: %s\n", strerror(rc));
        exit(1);
    }
}
static void* task_shutdown_watchdog(void* unused) {
    (void)unused;
    struct timespec remaining = { 5, 0 };
    while (nanosleep(&remaining, &remaining) < 0 && errno == EINTR) {}
    if (!atomic_load(&task_shutdown_complete))
        task_fatal("service shutdown request ignored for 5 seconds; check cancellation checkpoints and blocking operations");
    return NULL;
}
static void task_request_shutdown_now(void) {
    if (atomic_exchange(&task_shutdown_requested, true)) return;
    if (service_enabled()) return;
    // The owner itself may be stuck outside any cancellation point. A bounded
    // process watchdog makes that failure visible without terminating a thread
    // asynchronously or releasing another thread's resources.
    pthread_t watchdog;
    task_pthread_check(pthread_create(&watchdog, NULL, task_shutdown_watchdog, NULL));
    task_pthread_check(pthread_detach(watchdog));
}
void taskRequestShutdown(void) {
    if (!service_request_if_active()) task_request_shutdown_now();
}
static void task_cond_init(pthread_cond_t* cond) {
    pthread_condattr_t attr;
    task_pthread_check(pthread_condattr_init(&attr));
    task_pthread_check(pthread_condattr_setclock(&attr, CLOCK_MONOTONIC));
    task_pthread_check(pthread_cond_init(cond, &attr));
    task_pthread_check(pthread_condattr_destroy(&attr));
}
static int64_t task_clock_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}
static TinoxTaskScope* task_new_scope(TinoxTaskScope* parent) {
    TinoxTaskScope* scope = calloc(1, sizeof(*scope));
    atomic_init(&scope->parent, parent);
    atomic_init(&scope->cancelled, false);
    atomic_init(&scope->deadline, 0);
    return scope;
}
static TinoxTaskScope* task_ensure_scope(void) {
    if (!task_scope) task_frame_scope = task_scope = task_new_scope(NULL);
    return task_scope;
}
static const char* task_cancellation(TinoxTaskScope* scope) {
    if (!service_cleanup_mode && (atomic_load(&task_shutdown_requested) || service_should_cancel())) return task_shutdown_error;
    for (; scope; scope = atomic_load(&scope->parent)) {
        if (atomic_load(&scope->cancelled)) return task_cancelled_error;
        int64_t deadline = atomic_load(&scope->deadline);
        if (deadline && task_clock_ms() >= deadline) return task_deadline_error;
    }
    return NULL;
}
void taskCheckCancellation(void) {
    const char* error = task_cancellation(task_scope);
    if (error && !__tinox_err) __tinox_err = (int64_t)(intptr_t)error;
}
void taskSetDeadline(int64_t milliseconds) {
    int64_t now_ms = task_clock_ms();
    if (milliseconds < 0 || milliseconds > INT64_MAX - now_ms)
        task_fatal("taskSetDeadline requires a nonnegative duration in milliseconds");
    atomic_store(&task_ensure_scope()->deadline, now_ms + milliseconds);
    taskCheckCancellation();
}
void taskWaitReadable(int64_t fd) {
    if (fd < 0 || fd > INT_MAX) task_fatal("taskWaitReadable requires a valid file descriptor");
    struct pollfd pfd = { .fd = (int)fd, .events = POLLIN };
    while (1) {
        taskCheckCancellation();
        if (__tinox_err) return;
        int rc = poll(&pfd, 1, 10);
        if (rc < 0 && errno == EINTR) continue;
        if (rc < 0 || (pfd.revents & POLLNVAL)) {
            if (!__tinox_err) __tinox_err = (int64_t)(intptr_t)"Task I/O wait failed";
            return;
        }
        if (rc > 0) return;
    }
}
static void taskScopeEnter(void) {
    task_scope = task_new_scope(task_ensure_scope());
}
static TinoxTask* task_lookup(uintptr_t id) {
    pthread_mutex_lock(&task_registry_mu);
    TinoxTask* task = task_registry[id % TINOX_TASK_BUCKETS];
    while (task && task->id != id) task = task->registry_next;
    pthread_mutex_unlock(&task_registry_mu);
    if (!task) task_fatal("invalid or expired task handle (integer handles are deprecated; use Task<T>)");
    return task;
}
void taskCancel(int64_t handle) {
    TinoxTask* task = task_lookup((uintptr_t)handle);
    atomic_store(&task->scope->cancelled, true);
}
static void task_unregister(TinoxTask* task) {
    pthread_mutex_lock(&task_registry_mu);
    TinoxTask** link = &task_registry[task->id % TINOX_TASK_BUCKETS];
    while (*link && *link != task) link = &(*link)->registry_next;
    if (*link) *link = task->registry_next;
    task->registry_next = NULL;
    pthread_mutex_unlock(&task_registry_mu);
}
// Called with task->mu locked. Short timed waits allow cancellation without
// asynchronous termination, signals, or releasing resources from another thread.
static void task_wait_tick(TinoxTask* task) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    ts.tv_nsec += 10000000;
    if (ts.tv_nsec >= 1000000000) { ts.tv_sec++; ts.tv_nsec -= 1000000000; }
    int rc = pthread_cond_timedwait(&task->cond, &task->mu, &ts);
    if (rc != ETIMEDOUT) task_pthread_check(rc);
}
static void task_join_locked(TinoxTask* task) {
    if (!task->joined) {
        task_pthread_check(pthread_join(task->thread, NULL));
        task->joined = 1;
    }
}
static int task_is_cancellation(int64_t error) {
    return error == (int64_t)(intptr_t)task_cancelled_error
        || error == (int64_t)(intptr_t)task_deadline_error
        || error == (int64_t)(intptr_t)task_shutdown_error;
}
static void task_close_scope(TinoxTaskScope* scope, int suppress_all, int shutdown) {
    if (scope->closed) return;
    int64_t original_error = __tinox_err;
    if (original_error || suppress_all || shutdown) atomic_store(&scope->cancelled, true);
    int64_t cancel_started = 0;
    for (TinoxTask* task = scope->children; task; task = task->sibling) {
        pthread_mutex_lock(&task->mu);
        while (!task->done) {
            // Notice failures out of spawn order, but deliver them below in
            // spawn order. Otherwise a blocked earlier sibling can hide them.
            for (TinoxTask* other = scope->children; other; other = other->sibling) {
                if (other == task || pthread_mutex_trylock(&other->mu) != 0) continue;
                if (other->done && !other->observed && other->error && !task_is_cancellation(other->error))
                    atomic_store(&scope->cancelled, true);
                pthread_mutex_unlock(&other->mu);
            }
            if (task_cancellation(scope)) {
                if (!cancel_started) cancel_started = task_clock_ms();
                if ((!service_enabled() || !serviceIsStopping()) && task_clock_ms() - cancel_started >= 5000)
                    task_fatal("task ignored cooperative cancellation for 5 seconds during scope shutdown");
            }
            task_wait_tick(task);
        }
        task_join_locked(task);
        pthread_mutex_unlock(&task->mu);
    }
    int64_t first_error = 0;
    // A failure causing sibling cancellation takes precedence over that
    // cancellation. An explicit cancel/deadline is still observable on its own.
    for (int cancellation_pass = 0; cancellation_pass < 2; cancellation_pass++) {
        for (TinoxTask* task = scope->children; task; task = task->sibling) {
            pthread_mutex_lock(&task->mu);
            if (!task->observed && task->error && task_is_cancellation(task->error) == cancellation_pass) {
                task->observed = 1;
                if (!original_error && !suppress_all && !first_error && !(shutdown && cancellation_pass))
                    first_error = task->error;
                else if (!cancellation_pass)
                    fprintf(stderr, "tinox: task error suppressed: %s\n", (char*)(intptr_t)task->error);
            }
            pthread_mutex_unlock(&task->mu);
        }
    }
    for (TinoxTask* task = scope->children; task; task = task->sibling) task_unregister(task);
    scope->children = scope->children_tail = NULL;
    scope->closed = 1;
    __tinox_err = original_error ? original_error : first_error;
}
int64_t __scope_new(void) {
    taskScopeEnter();
    return (int64_t)(intptr_t)task_scope;
}
static void task_remove_owner(TinoxTask* task) {
    TinoxTaskScope* owner = task->owner;
    if (task->previous) task->previous->sibling = task->sibling;
    else owner->children = task->sibling;
    if (task->sibling) task->sibling->previous = task->previous;
    else owner->children_tail = task->previous;
    task->previous = task->sibling = NULL;
}
static void task_add_owner(TinoxTask* task, TinoxTaskScope* owner) {
    task->owner = owner;
    task->previous = owner->children_tail;
    if (owner->children_tail) owner->children_tail->sibling = task;
    else owner->children = task;
    owner->children_tail = task;
}
void __scope_add(int64_t handle, int64_t task_handle) {
    TinoxTaskScope* scope = (TinoxTaskScope*)(intptr_t)handle;
    TinoxTask* task = task_lookup((uintptr_t)task_handle);
    if (scope != task_scope || scope->closed || task->owner != task_frame_scope)
        task_fatal("invalid task scope registration");
    if (task->executor && task->executor == current_executor_id)
        task_fatal("a scope inside an executor job cannot own jobs of the same executor (joining them would deadlock)");
    // Registration stays lexical: spawns in called methods or closures remain
    // owned by the thread frame, exactly as the parser's scope stack specifies.
    task_remove_owner(task);
    task_add_owner(task, scope);
}
void __scope_abort(int64_t handle) {
    TinoxTaskScope* scope = (TinoxTaskScope*)(intptr_t)handle;
    task_close_scope(scope, 1, 0);
    if (task_scope == scope) task_scope = atomic_load(&scope->parent);
}
void __scope_join(int64_t handle) {
    TinoxTaskScope* scope = (TinoxTaskScope*)(intptr_t)handle;
    task_close_scope(scope, 0, 0);
    // A rethrow may bypass the generated finally; abort restores it too.
    if (task_scope == scope) task_scope = atomic_load(&scope->parent);
}

typedef struct TinoxChannelNode {
    int64_t value;
    struct TinoxChannelNode* next;
} TinoxChannelNode;
typedef struct {
    TinoxChannelNode* head;
    TinoxChannelNode* tail;
    pthread_mutex_t mutex;
    pthread_cond_t cond;
} TinoxChannel;

// ---- Concurrency budgets (issue #302) ----
// One validated set of limits per process, resolved lazily on first use:
// an explicit TINOX_* environment override wins over the detected default,
// and a malformed override is a hard startup failure (no silent fallback
// to a default the operator did not ask for).
//
//   TINOX_CPUS                  effective CPU count (default: detected)
//   TINOX_HTTP_WORKERS          HTTP/1 epoll worker threads (default: CPUs)
//   TINOX_MAX_CONNECTIONS       open HTTP/1 connections, all workers (4096)
//   TINOX_MAX_TASKS             concurrently running `spawn` threads (4096)
//   TINOX_MAX_WS_CONNECTIONS    concurrently open WebSocket connections (1024)
//   TINOX_MAX_BODY_BYTES        largest HTTP/1 request body (4 MiB); larger ones get 413. The
//                               whole body is held in memory per request in flight, so budget
//                               max body x concurrent uploads (issue #542)
//   TINOX_QUEUE_WAIT_MS         how long `spawn` / a WebSocket accept may
//                               wait for a free slot before the request is
//                               rejected (default 0 = reject immediately)
//
// Each setting can also be given in application.properties as
// `tinox.limits.<name>` (e.g. `tinox.limits.max_tasks=256`); the
// environment variable wins, then the properties file, then the default.
//
// Detected CPUs = min(sched affinity/cpuset, cgroup CPU quota), so a
// container started with `--cpus=1.5` or a `taskset` mask no longer sizes
// its pools by the host's online core count.
// Upper bound for TINOX_MAX_CONNECTIONS. The per-worker slot table is indexed by fd and grown
// on demand (see epoll_slot_reserve), so this only caps what a configuration may ask for;
// the default stays 4096.
#define TINOX_MAX_CONNECTIONS_CEILING 1000000

typedef struct {
    int64_t cpus;
    const char* cpu_source;
    int64_t http_workers;
    int64_t max_connections;
    int64_t max_tasks;
    int64_t max_ws_connections;
    int64_t queue_wait_ms;
    int64_t max_body_bytes;
} TinoxLimits;

static TinoxLimits g_limits;
static pthread_once_t g_limits_once = PTHREAD_ONCE_INIT;
static atomic_llong g_active_tasks, g_peak_tasks, g_rejected_tasks;
static atomic_llong g_open_conns, g_peak_conns, g_rejected_conns;
static atomic_llong g_offload_accepted, g_offload_rejected; // @Blocking routes (#552)
static atomic_llong g_active_ws, g_peak_ws, g_rejected_ws;
// Queue (wait-for-slot) accounting shared by the task and WebSocket budgets.
static atomic_llong g_waiting, g_wait_total_ms, g_wait_max_ms, g_wait_timeouts;
static pthread_mutex_t g_budget_mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t g_budget_cv;
static const char* tinox_config_lookup(const char* key);

static void tinox_atomic_max(atomic_llong* peak, long long v) {
    long long cur = atomic_load(peak);
    while (v > cur && !atomic_compare_exchange_weak(peak, &cur, v)) {}
}

// Reads a small text file into buf; returns 0 on success.
static int tinox_read_small_file(const char* path, char* buf, size_t cap) {
    FILE* f = fopen(path, "r");
    if (!f) return -1;
    size_t n = fread(buf, 1, cap - 1, f);
    fclose(f);
    buf[n] = '\0';
    return 0;
}

// CPU quota of the enclosing cgroup as a whole number of CPUs (rounded
// up, at least 1), or 0 when there is no quota. TINOX_CGROUP_ROOT only
// exists so tests can point this at a fixture tree.
static int64_t tinox_cgroup_cpu_quota(void) {
    const char* root = getenv("TINOX_CGROUP_ROOT");
    int fixture = root && *root;
    if (!fixture) root = "/sys/fs/cgroup";
    char path[1024], buf[256];
    long long quota = 0, period = 0;

    // cgroup v2: "<quota|max> <period>". The process may sit in a nested
    // cgroup (systemd scope, k8s pod slice) whose parents carry the quota,
    // so walk from the process's own cgroup up to the root keeping the
    // tightest limit. Inside a container the cgroup namespace makes the
    // own cgroup "/" already.
    char rel[512] = "";
    if (!fixture && tinox_read_small_file("/proc/self/cgroup", buf, sizeof(buf)) == 0) {
        char* line = strstr(buf, "0::");
        if (line) {
            line += 3;
            size_t n = strcspn(line, "\n");
            if (n < sizeof(rel)) { memcpy(rel, line, n); rel[n] = '\0'; }
        }
    }
    int64_t best = 0;
    int found_v2 = 0;
    for (;;) {
        snprintf(path, sizeof(path), "%s%s/cpu.max", root, rel);
        if (tinox_read_small_file(path, buf, sizeof(buf)) == 0) {
            found_v2 = 1;
            if (strncmp(buf, "max", 3) != 0 && sscanf(buf, "%lld %lld", &quota, &period) == 2
                && quota > 0 && period > 0) {
                int64_t cpus = (quota + period - 1) / period;
                if (best == 0 || cpus < best) best = cpus;
            }
        }
        char* slash = strrchr(rel, '/');
        if (!slash) break;
        *slash = '\0';
    }
    if (found_v2) return best;

    // cgroup v1: separate quota (-1 = none) and period files
    snprintf(path, sizeof(path), "%s/cpu/cpu.cfs_quota_us", root);
    if (tinox_read_small_file(path, buf, sizeof(buf)) != 0) return 0;
    quota = atoll(buf);
    snprintf(path, sizeof(path), "%s/cpu/cpu.cfs_period_us", root);
    if (tinox_read_small_file(path, buf, sizeof(buf)) != 0) return 0;
    period = atoll(buf);
    if (quota > 0 && period > 0) return (quota + period - 1) / period;
    return 0;
}

static int64_t tinox_limit_env(const char* name, int64_t dflt, int64_t min, int64_t max) {
    const char* v = getenv(name);
    if (!v || !*v) {
        // application.properties: tinox.limits.<lowercased name without TINOX_>
        char key[128] = "tinox.limits.";
        size_t o = strlen(key);
        for (const char* c = name + 6; *c && o < sizeof(key) - 1; c++)
            key[o++] = (char)tolower((unsigned char)*c);
        key[o] = '\0';
        v = tinox_config_lookup(key);
        if (!v || !*v) return dflt;
        name = key;
    }
    char* end = NULL;
    errno = 0;
    long long n = strtoll(v, &end, 10);
    if (errno != 0 || *end != '\0' || n < min || n > max) {
        fprintf(stderr, "tinox: invalid %s=\"%s\" (expected an integer in %lld..%lld)\n",
                name, v, (long long)min, (long long)max);
        exit(1);
    }
    return (int64_t)n;
}

static void tinox_limits_init_once(void) {
    int64_t cpus = 0;
    const char* source = "online";
    cpu_set_t set;
    if (sched_getaffinity(0, sizeof(set), &set) == 0 && CPU_COUNT(&set) > 0) {
        cpus = CPU_COUNT(&set);
        source = "affinity";
    } else {
        long n = sysconf(_SC_NPROCESSORS_ONLN);
        cpus = n > 0 ? n : 1;
    }
    int64_t quota = tinox_cgroup_cpu_quota();
    if (quota > 0 && quota < cpus) {
        cpus = quota;
        source = "cgroup quota";
    }
    const char* ov = getenv("TINOX_CPUS");
    if (ov && *ov) source = "TINOX_CPUS";
    g_limits.cpus = tinox_limit_env("TINOX_CPUS", cpus, 1, 4096);
    g_limits.cpu_source = source;
    g_limits.http_workers = tinox_limit_env("TINOX_HTTP_WORKERS", g_limits.cpus, 1, 256);
    g_limits.max_connections = tinox_limit_env("TINOX_MAX_CONNECTIONS", 4096, 1, TINOX_MAX_CONNECTIONS_CEILING);
    if (g_limits.max_connections > 4096) {
        // Every connection is one fd (plus listeners/files): make sure the process may open them,
        // raising the soft limit up to the hard limit, and refuse to start with a budget the
        // kernel will not honour rather than failing later at accept().
        struct rlimit rl;
        rlim_t need = (rlim_t)g_limits.max_connections + 256;
        if (getrlimit(RLIMIT_NOFILE, &rl) == 0 && rl.rlim_cur < need) {
            rlim_t want = (rl.rlim_max == RLIM_INFINITY || rl.rlim_max >= need) ? need : rl.rlim_max;
            rl.rlim_cur = want;
            if (setrlimit(RLIMIT_NOFILE, &rl) != 0 || want < need) {
                fprintf(stderr, "tinox: TINOX_MAX_CONNECTIONS=%lld needs about %llu file descriptors but RLIMIT_NOFILE allows %llu; raise `ulimit -n` or lower the budget\n",
                        (long long)g_limits.max_connections, (unsigned long long)need, (unsigned long long)want);
                exit(1);
            }
        }
    }
    g_limits.max_tasks = tinox_limit_env("TINOX_MAX_TASKS", 4096, 1, 1000000);
    g_limits.max_ws_connections = tinox_limit_env("TINOX_MAX_WS_CONNECTIONS", 1024, 1, 1000000);
    g_limits.queue_wait_ms = tinox_limit_env("TINOX_QUEUE_WAIT_MS", 0, 0, 600000);
    g_limits.max_body_bytes = tinox_limit_env("TINOX_MAX_BODY_BYTES", 4 * 1024 * 1024, 1024, 1073741824LL);
    pthread_condattr_t ca;
    pthread_condattr_init(&ca);
    pthread_condattr_setclock(&ca, CLOCK_MONOTONIC);
    pthread_cond_init(&g_budget_cv, &ca);
    pthread_condattr_destroy(&ca);
}

static const TinoxLimits* tinox_limits(void) {
    pthread_once(&g_limits_once, tinox_limits_init_once);
    return &g_limits;
}

// Reserves one slot in a counter bounded by `max`; 0 when exhausted.
static int tinox_budget_acquire(atomic_llong* active, atomic_llong* peak, int64_t max) {
    long long cur = atomic_load(active);
    while (cur < max) {
        if (atomic_compare_exchange_weak(active, &cur, cur + 1)) {
            tinox_atomic_max(peak, cur + 1);
            return 1;
        }
    }
    return 0;
}

// Like tinox_budget_acquire, but may queue for up to TINOX_QUEUE_WAIT_MS
// for a slot to free up. The wait is bounded by that deadline, so a queue
// can never grow past (callers x deadline) and a request that does not
// get a slot in time is rejected, never retried or run late.
static int tinox_budget_acquire_queued(atomic_llong* active, atomic_llong* peak, int64_t max) {
    if (tinox_budget_acquire(active, peak, max)) return 1;
    int64_t wait_ms = tinox_limits()->queue_wait_ms;
    if (wait_ms <= 0) return 0;
    struct timespec t0, deadline;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    deadline = t0;
    deadline.tv_sec += wait_ms / 1000;
    deadline.tv_nsec += (wait_ms % 1000) * 1000000L;
    if (deadline.tv_nsec >= 1000000000L) { deadline.tv_sec++; deadline.tv_nsec -= 1000000000L; }
    atomic_fetch_add(&g_waiting, 1);
    int got = 0;
    pthread_mutex_lock(&g_budget_mu);
    for (;;) {
        if (tinox_budget_acquire(active, peak, max)) { got = 1; break; }
        if (pthread_cond_timedwait(&g_budget_cv, &g_budget_mu, &deadline) == ETIMEDOUT) {
            got = tinox_budget_acquire(active, peak, max);
            break;
        }
    }
    pthread_mutex_unlock(&g_budget_mu);
    atomic_fetch_sub(&g_waiting, 1);
    struct timespec t1;
    clock_gettime(CLOCK_MONOTONIC, &t1);
    long long waited = (long long)(t1.tv_sec - t0.tv_sec) * 1000 + (t1.tv_nsec - t0.tv_nsec) / 1000000;
    atomic_fetch_add(&g_wait_total_ms, waited);
    tinox_atomic_max(&g_wait_max_ms, waited);
    if (!got) atomic_fetch_add(&g_wait_timeouts, 1);
    return got;
}

// Frees a slot and wakes any queued acquirer.
static void tinox_budget_release(atomic_llong* active) {
    atomic_fetch_sub(active, 1);
    if (atomic_load(&g_waiting) > 0) {
        pthread_mutex_lock(&g_budget_mu);
        pthread_cond_broadcast(&g_budget_cv);
        pthread_mutex_unlock(&g_budget_mu);
    }
}

// Effective limits, for the startup banner / inspection output.
void tinox_print_limits(void) {
    const TinoxLimits* l = tinox_limits();
    printf("Limits: cpus=%lld (%s), http workers=%lld, max connections=%lld, max tasks=%lld, max websocket connections=%lld, queue wait=%lld ms\n",
           (long long)l->cpus, l->cpu_source, (long long)l->http_workers,
           (long long)l->max_connections, (long long)l->max_tasks,
           (long long)l->max_ws_connections, (long long)l->queue_wait_ms);
    fflush(stdout);
}

// Effective limits and live saturation counters as JSON (Dev UI /limits).
char* tinox_limits_json(void) {
    const TinoxLimits* l = tinox_limits();
    char* buf = (char*)GC_malloc(1024);
    snprintf(buf, 1024,
        "{\"cpus\":%lld,\"cpuSource\":\"%s\",\"httpWorkers\":%lld,\"maxConnections\":%lld,"
        "\"maxTasks\":%lld,\"maxWebsocketConnections\":%lld,\"queueWaitMs\":%lld,"
        "\"connectionsOpen\":%lld,\"connectionsRejected\":%lld,"
        "\"tasksActive\":%lld,\"tasksRejected\":%lld,"
        "\"websocketConnectionsActive\":%lld,\"websocketConnectionsRejected\":%lld,"
        "\"queueWaiting\":%lld,\"queueWaitMaxMs\":%lld,\"queueTimeouts\":%lld,\"maxBodyBytes\":%lld}",
        (long long)l->cpus, l->cpu_source, (long long)l->http_workers,
        (long long)l->max_connections, (long long)l->max_tasks,
        (long long)l->max_ws_connections, (long long)l->queue_wait_ms,
        (long long)atomic_load(&g_open_conns), (long long)atomic_load(&g_rejected_conns),
        (long long)atomic_load(&g_active_tasks), (long long)atomic_load(&g_rejected_tasks),
        (long long)atomic_load(&g_active_ws), (long long)atomic_load(&g_rejected_ws),
        (long long)atomic_load(&g_waiting), (long long)atomic_load(&g_wait_max_ms),
        (long long)atomic_load(&g_wait_timeouts), (long long)l->max_body_bytes);
    return buf;
}

int64_t gcStat(int64_t kind);

// Limits + saturation counters as Prometheus text (appended to /metrics).
static int tinox_limits_metrics_text(char* buf, size_t cap) {
    const TinoxLimits* l = tinox_limits();
    return snprintf(buf, cap,
        "# TYPE tinox_cpus gauge\ntinox_cpus %lld\n"
        "# TYPE tinox_http_workers gauge\ntinox_http_workers %lld\n"
        "# TYPE tinox_http_connections_limit gauge\ntinox_http_connections_limit %lld\n"
        "# TYPE tinox_http_connections_open gauge\ntinox_http_connections_open %lld\n"
        "# TYPE tinox_http_connections_peak gauge\ntinox_http_connections_peak %lld\n"
        "# TYPE tinox_http_connections_rejected_total counter\ntinox_http_connections_rejected_total %lld\n"
        "# TYPE tinox_tasks_limit gauge\ntinox_tasks_limit %lld\n"
        "# TYPE tinox_tasks_active gauge\ntinox_tasks_active %lld\n"
        "# TYPE tinox_tasks_peak gauge\ntinox_tasks_peak %lld\n"
        "# TYPE tinox_tasks_rejected_total counter\ntinox_tasks_rejected_total %lld\n"
        "# TYPE tinox_websocket_connections_limit gauge\ntinox_websocket_connections_limit %lld\n"
        "# TYPE tinox_websocket_connections_active gauge\ntinox_websocket_connections_active %lld\n"
        "# TYPE tinox_websocket_connections_peak gauge\ntinox_websocket_connections_peak %lld\n"
        "# TYPE tinox_websocket_connections_rejected_total counter\ntinox_websocket_connections_rejected_total %lld\n"
        "# TYPE tinox_queue_wait_limit_ms gauge\ntinox_queue_wait_limit_ms %lld\n"
        "# TYPE tinox_queue_waiting gauge\ntinox_queue_waiting %lld\n"
        "# TYPE tinox_queue_wait_ms_total counter\ntinox_queue_wait_ms_total %lld\n"
        "# TYPE tinox_queue_wait_ms_max gauge\ntinox_queue_wait_ms_max %lld\n"
        "# TYPE tinox_queue_timeouts_total counter\ntinox_queue_timeouts_total %lld\n"
        "# TYPE tinox_blocking_route_jobs_total counter\ntinox_blocking_route_jobs_total %lld\n"
        "# TYPE tinox_blocking_route_rejected_total counter\ntinox_blocking_route_rejected_total %lld\n"
        "# TYPE tinox_gc_heap_bytes gauge\ntinox_gc_heap_bytes %lld\n"
        "# TYPE tinox_gc_free_bytes gauge\ntinox_gc_free_bytes %lld\n"
        "# TYPE tinox_gc_used_estimate_bytes gauge\ntinox_gc_used_estimate_bytes %lld\n"
        "# TYPE tinox_gc_allocated_bytes_total counter\ntinox_gc_allocated_bytes_total %lld\n"
        "# TYPE tinox_gc_collections_total counter\ntinox_gc_collections_total %lld\n",
        (long long)l->cpus, (long long)l->http_workers,
        (long long)l->max_connections, (long long)atomic_load(&g_open_conns),
        (long long)atomic_load(&g_peak_conns), (long long)atomic_load(&g_rejected_conns),
        (long long)l->max_tasks, (long long)atomic_load(&g_active_tasks),
        (long long)atomic_load(&g_peak_tasks), (long long)atomic_load(&g_rejected_tasks),
        (long long)l->max_ws_connections, (long long)atomic_load(&g_active_ws),
        (long long)atomic_load(&g_peak_ws), (long long)atomic_load(&g_rejected_ws),
        (long long)l->queue_wait_ms, (long long)atomic_load(&g_waiting),
        (long long)atomic_load(&g_wait_total_ms), (long long)atomic_load(&g_wait_max_ms),
        (long long)atomic_load(&g_wait_timeouts),
        (long long)atomic_load(&g_offload_accepted), (long long)atomic_load(&g_offload_rejected),
        (long long)gcStat(0), (long long)gcStat(1), (long long)gcStat(5),
        (long long)gcStat(3), (long long)gcStat(4));
}

// WebSocket admission: the accept loop asks before handing a connection to
// a worker thread, and releases when that worker finishes.
int64_t tinox_ws_admit(void) {
    if (tinox_budget_acquire_queued(&g_active_ws, &g_peak_ws, tinox_limits()->max_ws_connections)) return 1;
    atomic_fetch_add(&g_rejected_ws, 1);
    return 0;
}
void tinox_ws_release(void) { tinox_budget_release(&g_active_ws); }

typedef struct {
    void* (*fn)(void*);
    void* args;
    TinoxTask* task;
    TinoxTelemetryContext telemetry;
    int64_t queued_at;
    int service_id;
    int cleanup_mode;
} TinoxSpawnTrampolineArgs;

static void* tinox_spawn_trampoline(void* raw) {
    TinoxSpawnTrampolineArgs* t = raw;
    tinox_gc_register_thread_roots();
    if (telemetry_enabled) telemetry_context = t->telemetry;
    int64_t span_token = telemetry_enter("task", NULL, NULL, 1);
    if (span_token) tinox_histogram_record("tinox_task_queue_duration_ns", telemetry_clock(CLOCK_MONOTONIC) - t->queued_at);
    service_task_id = t->service_id;
    service_cleanup_mode = t->cleanup_mode;
    current_task = t->task;
    task_frame_scope = task_scope = current_task->scope;
    void* (*fn)(void*) = t->fn;
    void* args = t->args;
    free(t);
    void* ret = fn(args);
    task_close_scope(task_frame_scope, 0, 0);
    telemetry_leave(span_token);
    TinoxTask* task = current_task;
    pthread_mutex_lock(&task->mu);
    task->retval = ret;
    task->error = __tinox_err;
    task->done = 1;
    if (task->detached) {
        if (task->error && task->error != (int64_t)(intptr_t)task_shutdown_error) {
            fprintf(stderr, "Uncaught detached task error: %s\n", (char*)(intptr_t)task->error);
            exit(1);
        }
        task_unregister(task);
    }
    pthread_cond_broadcast(&task->cond);
    pthread_mutex_unlock(&task->mu);
    task_frame_scope = task_scope = NULL;
    current_task = NULL;
    tinox_gc_unregister_thread_roots();
    if (task->budgeted) tinox_budget_release(&g_active_tasks);
    return ret;
}


// ---- Bounded executor for many small jobs (#551) ----
// `spawn f(x)` creates a pthread per task. An executor is a separate, explicit abstraction: a fixed number of
// reusable worker threads behind a bounded FIFO queue. `spawn f(x) on ex` hands the same typed task to it. Jobs keep
// Task<T> results, error propagation, cancellation (through the job's scope) and the submitter's telemetry/service
// context. A full queue or a shut-down executor rejects the job (the handle comes back finished and failed); there
// is no caller-runs fallback and no unbounded growth. Awaiting a job of the worker's own executor is refused.
#define TINOX_EXECUTOR_MAX 64
#define TINOX_EXECUTOR_MAX_WORKERS 256
#define TINOX_EXECUTOR_MAX_QUEUE 1000000
typedef struct {
    void* (*fn)(void*);
    void* args;
    TinoxTask* task;
    TinoxTelemetryContext telemetry;
    int64_t queued_at;
    int service_id;
    int cleanup_mode;
} TinoxExecJob;
enum { EXEC_RUNNING, EXEC_DRAINING, EXEC_STOPPED };
typedef struct {
    int64_t id;
    pthread_mutex_t mu;
    pthread_cond_t work, idle;
    TinoxExecJob* queue; // GC_malloc_uncollectable: the GC scans queued job arguments
    int64_t cap, head, count;
    int64_t n_workers;
    pthread_t* threads;
    TinoxTask** running; // per worker: the job it is executing (for cancellation on shutdown)
    int state;
    int64_t active, submitted, completed, rejected, peak_queued;
} TinoxExecutor;
static pthread_mutex_t executors_mu = PTHREAD_MUTEX_INITIALIZER;
static TinoxExecutor* executors[TINOX_EXECUTOR_MAX];
static int64_t executor_next_id = 1;

static TinoxExecutor* executor_find(int64_t id) {
    TinoxExecutor* found = NULL;
    pthread_mutex_lock(&executors_mu);
    for (int i = 0; i < TINOX_EXECUTOR_MAX; i++)
        if (executors[i] && executors[i]->id == id) found = executors[i];
    pthread_mutex_unlock(&executors_mu);
    return found;
}
static TinoxExecutor* executor_lookup(int64_t id) {
    TinoxExecutor* ex = executor_find(id);
    if (!ex) task_fatal("invalid or expired executor handle");
    return ex;
}
// Finishes `task` as failed without running it (no thread ever existed for it).
static void task_fail_unstarted(TinoxTask* task, const char* message) {
    pthread_mutex_lock(&task->mu);
    task->error = (int64_t)(intptr_t)message;
    task->done = 1;
    task->joined = 1;
    pthread_cond_broadcast(&task->cond);
    pthread_mutex_unlock(&task->mu);
}
static void executor_run_job(TinoxExecutor* ex, TinoxExecJob* job) {
    TinoxTask* task = job->task;
    if (!task) {
        // A runtime-internal job (no Task<T>, e.g. an offloaded HTTP request): it owns its whole context.
        job->fn(job->args);
        __tinox_err = 0;
        return;
    }
    if (telemetry_enabled) telemetry_context = job->telemetry;
    int64_t span_token = telemetry_enter("task", NULL, NULL, 1);
    if (span_token) tinox_histogram_record("tinox_task_queue_duration_ns", telemetry_clock(CLOCK_MONOTONIC) - job->queued_at);
    service_task_id = job->service_id;
    service_cleanup_mode = job->cleanup_mode;
    current_task = task;
    task_frame_scope = task_scope = task->scope;
    __tinox_err = 0;
    void* ret = NULL;
    // A job cancelled while it waited in the queue never starts.
    const char* before = task_cancellation(task->scope);
    if (before) __tinox_err = (int64_t)(intptr_t)before;
    else ret = job->fn(job->args);
    task_close_scope(task_frame_scope, 0, 0);
    telemetry_leave(span_token);
    pthread_mutex_lock(&task->mu);
    task->retval = ret;
    task->error = __tinox_err;
    pthread_mutex_unlock(&task->mu);
    // Worker reset: nothing of this job's context may leak into the next one.
    task_frame_scope = task_scope = NULL;
    current_task = NULL;
    __tinox_err = 0;
    service_task_id = -1;
    service_cleanup_mode = 0;
    if (telemetry_enabled) memset(&telemetry_context, 0, sizeof telemetry_context);
}
typedef struct { TinoxExecutor* ex; int64_t index; } TinoxExecWorkerArgs;
static void* executor_worker(void* raw) {
    TinoxExecWorkerArgs* wa = raw;
    TinoxExecutor* ex = wa->ex;
    int64_t index = wa->index;
    free(wa);
    tinox_gc_register_thread_roots();
    current_executor_id = ex->id;
    for (;;) {
        pthread_mutex_lock(&ex->mu);
        while (!ex->count && ex->state == EXEC_RUNNING) task_pthread_check(pthread_cond_wait(&ex->work, &ex->mu));
        if (!ex->count) { pthread_mutex_unlock(&ex->mu); break; }
        TinoxExecJob job = ex->queue[ex->head];
        memset(&ex->queue[ex->head], 0, sizeof(TinoxExecJob));
        ex->head = (ex->head + 1) % ex->cap;
        ex->count--;
        ex->active++;
        ex->running[index] = job.task;
        pthread_mutex_unlock(&ex->mu);
        executor_run_job(ex, &job);
        pthread_mutex_lock(&ex->mu);
        ex->running[index] = NULL;
        ex->active--;
        ex->completed++;
        if (!ex->count && !ex->active) pthread_cond_broadcast(&ex->idle);
        pthread_mutex_unlock(&ex->mu);
        // Awaiters may read executor stats immediately after completion. Publish
        // done only after accounting and worker-context cleanup are finished.
        if (job.task) {
            pthread_mutex_lock(&job.task->mu);
            job.task->done = 1;
            pthread_cond_broadcast(&job.task->cond);
            pthread_mutex_unlock(&job.task->mu);
        }
    }
    tinox_gc_unregister_thread_roots();
    return NULL;
}
// Creates an executor with `workers` reusable threads and room for `queue` waiting jobs; returns its handle.
int64_t executorCreate(int64_t workers, int64_t queue) {
    if (workers < 1 || workers > TINOX_EXECUTOR_MAX_WORKERS)
        task_fatal("executorCreate requires 1..256 workers");
    if (queue < 1 || queue > TINOX_EXECUTOR_MAX_QUEUE)
        task_fatal("executorCreate requires a queue capacity of 1..1000000");
    TinoxExecutor* ex = calloc(1, sizeof(*ex));
    task_pthread_check(pthread_mutex_init(&ex->mu, NULL));
    task_cond_init(&ex->work);
    task_cond_init(&ex->idle);
    ex->cap = queue;
    ex->queue = (TinoxExecJob*)GC_malloc_uncollectable(sizeof(TinoxExecJob) * (size_t)queue);
    ex->n_workers = workers;
    ex->threads = calloc((size_t)workers, sizeof(pthread_t));
    ex->running = calloc((size_t)workers, sizeof(TinoxTask*));
    pthread_mutex_lock(&executors_mu);
    int slot = -1;
    for (int i = 0; i < TINOX_EXECUTOR_MAX && slot < 0; i++) {
        if (!executors[i]) slot = i;
        else if (executors[i]->state == EXEC_STOPPED) {
            // Reclaim a finished executor; its handle is expired from now on.
            TinoxExecutor* old = executors[i];
            GC_free(old->queue);
            free(old->threads); free(old->running); free(old);
            slot = i;
        }
    }
    if (slot < 0) { pthread_mutex_unlock(&executors_mu); task_fatal("too many live executors (max 64)"); }
    ex->id = executor_next_id++;
    executors[slot] = ex;
    pthread_mutex_unlock(&executors_mu);
    for (int64_t i = 0; i < workers; i++) {
        TinoxExecWorkerArgs* wa = malloc(sizeof(*wa));
        wa->ex = ex; wa->index = i;
        task_pthread_check(pthread_create(&ex->threads[i], NULL, executor_worker, wa));
    }
    return ex->id;
}
static void executor_submit(int64_t id, TinoxTask* task, void* (*fn)(void*), void* args, TinoxSpawnTrampolineArgs* t) {
    TinoxExecutor* ex = executor_lookup(id);
    TinoxExecJob job;
    memset(&job, 0, sizeof job);
    job.fn = fn; job.args = args; job.task = task;
    job.telemetry = t->telemetry;
    job.queued_at = t->queued_at;
    job.service_id = t->service_id;
    job.cleanup_mode = t->cleanup_mode;
    free(t);
    const char* reject = NULL;
    pthread_mutex_lock(&ex->mu);
    if (ex->state != EXEC_RUNNING) reject = "executor is shut down";
    else if (ex->count >= ex->cap) reject = "executor queue full: too many waiting jobs";
    else {
        ex->queue[(ex->head + ex->count) % ex->cap] = job;
        ex->count++;
        ex->submitted++;
        if (ex->count > ex->peak_queued) ex->peak_queued = ex->count;
        pthread_cond_signal(&ex->work);
    }
    if (reject) ex->rejected++;
    pthread_mutex_unlock(&ex->mu);
    if (reject) task_fail_unstarted(task, reject);
}
// Queues a runtime-internal job (no Task<T> handle). Returns 0 when the queue is full or the executor is shut down;
// the caller then owns `args` again.
static int executor_submit_raw(int64_t id, void* (*fn)(void*), void* args) {
    TinoxExecutor* ex = executor_find(id);
    if (!ex) return 0;
    TinoxExecJob job;
    memset(&job, 0, sizeof job);
    job.fn = fn; job.args = args;
    int accepted = 0;
    pthread_mutex_lock(&ex->mu);
    if (ex->state == EXEC_RUNNING && ex->count < ex->cap) {
        ex->queue[(ex->head + ex->count) % ex->cap] = job;
        ex->count++;
        ex->submitted++;
        if (ex->count > ex->peak_queued) ex->peak_queued = ex->count;
        pthread_cond_signal(&ex->work);
        accepted = 1;
    } else {
        ex->rejected++;
    }
    pthread_mutex_unlock(&ex->mu);
    return accepted;
}
// Stops accepting jobs. With drain != 0 queued jobs still run; otherwise they are rejected and running jobs are
// cancelled (cooperatively). Returns after every worker has exited.
void executorShutdown(int64_t id, int64_t drain) {
    TinoxExecutor* ex = executor_lookup(id);
    if (current_executor_id == ex->id) task_fatal("an executor job cannot shut down its own executor");
    pthread_mutex_lock(&ex->mu);
    if (ex->state == EXEC_STOPPED) { pthread_mutex_unlock(&ex->mu); return; }
    ex->state = EXEC_DRAINING;
    if (!drain) {
        while (ex->count) {
            TinoxExecJob job = ex->queue[ex->head];
            memset(&ex->queue[ex->head], 0, sizeof(TinoxExecJob));
            ex->head = (ex->head + 1) % ex->cap;
            ex->count--;
            ex->rejected++;
            task_fail_unstarted(job.task, "executor shut down before the job started");
        }
        for (int64_t i = 0; i < ex->n_workers; i++)
            if (ex->running[i]) atomic_store(&ex->running[i]->scope->cancelled, true);
    }
    pthread_cond_broadcast(&ex->work);
    pthread_mutex_unlock(&ex->mu);
    for (int64_t i = 0; i < ex->n_workers; i++) task_pthread_check(pthread_join(ex->threads[i], NULL));
    pthread_mutex_lock(&ex->mu);
    ex->state = EXEC_STOPPED;
    pthread_mutex_unlock(&ex->mu);
}
// Counters: 0 workers, 1 queued, 2 active, 3 completed, 4 rejected, 5 peak queue length, 6 submitted.
int64_t executorStat(int64_t id, int64_t which) {
    TinoxExecutor* ex = executor_lookup(id);
    pthread_mutex_lock(&ex->mu);
    int64_t v = 0;
    switch (which) {
        case 0: v = ex->n_workers; break;
        case 1: v = ex->count; break;
        case 2: v = ex->active; break;
        case 3: v = ex->completed; break;
        case 4: v = ex->rejected; break;
        case 5: v = ex->peak_queued; break;
        case 6: v = ex->submitted; break;
        default: pthread_mutex_unlock(&ex->mu); task_fatal("executorStat: unknown counter");
    }
    pthread_mutex_unlock(&ex->mu);
    return v;
}

static void* task_spawn_impl(void* (*fn)(void*), void* args, int budgeted, int64_t executor);
static void* task_spawn_impl(void* (*fn)(void*), void* args, int budgeted, int64_t executor) {
    if (request_active && request_instances)
        request_scope_fail("spawn cannot escape an active request provider graph; copy immutable data before scheduling outside the request");
    task_ensure_scope();
    TinoxTaskScope* owner = task_frame_scope;
    TinoxTask* task = calloc(1, sizeof(*task));
    task->owner = owner;
    task->creator_thread = pthread_self();
    task->scope = task_new_scope(task_scope);
    task_pthread_check(pthread_mutex_init(&task->mu, NULL));
    task_cond_init(&task->cond);
    pthread_mutex_lock(&task_registry_mu);
    if (!task_next_id || task_next_id > INT64_MAX) task_fatal("task handle space exhausted");
    task->id = task_next_id++;
    size_t bucket = task->id % TINOX_TASK_BUCKETS;
    task->registry_next = task_registry[bucket];
    task_registry[bucket] = task;
    pthread_mutex_unlock(&task_registry_mu);
    task_add_owner(task, owner);
    TinoxSpawnTrampolineArgs* t = malloc(sizeof(*t));
    t->fn = fn; t->args = args; t->task = task;
    if (telemetry_enabled) {
        t->telemetry = telemetry_context;
        t->queued_at = telemetry_clock(CLOCK_MONOTONIC);
    }
    t->service_id = service_task_id;
    t->cleanup_mode = service_cleanup_mode;
    if (executor) {
        task->budgeted = 0;
        task->joined = 1; // no thread of its own to join
        task->executor = executor;
        if (executor == current_executor_id) {
            // The job's own scope joins its children when the job ends. Children waiting in the queue
            // behind the workers that are busy joining them would never run.
            free(t);
            task_fail_unstarted(task, "executor job cannot submit to its own executor (joining the child would deadlock)");
            return (void*)task->id;
        }
        executor_submit(executor, task, fn, args, t);
        return (void*)task->id;
    }
    task->budgeted = budgeted;
    // Admission control (#302): over the task budget no thread is created.
    // The handle comes back already finished and failed, so `await` / the
    // owning scope re-throw the rejection instead of the process growing
    // threads without bound -- or silently dropping the work.
    if (budgeted && !tinox_budget_acquire_queued(&g_active_tasks, &g_peak_tasks, tinox_limits()->max_tasks)) {
        atomic_fetch_add(&g_rejected_tasks, 1);
        free(t);
        task->budgeted = 0;
        pthread_mutex_lock(&task->mu);
        task->error = (int64_t)(intptr_t)"task budget exhausted: too many concurrently running tasks (TINOX_MAX_TASKS)";
        task->done = 1;
        task->joined = 1; // there is no thread to join
        pthread_cond_broadcast(&task->cond);
        pthread_mutex_unlock(&task->mu);
        return (void*)task->id;
    }
    task_pthread_check(pthread_create(&task->thread, NULL, tinox_spawn_trampoline, t));
    return (void*)task->id;
}
// Joinable tasks count against TINOX_MAX_TASKS; the detached per-connection
// workers are governed by their own (WebSocket) budget instead.
void* tinox_task_spawn(void* (*fn)(void*), void* args) { return task_spawn_impl(fn, args, 1, 0); }
void* tinox_task_spawn_on(int64_t executor, void* (*fn)(void*), void* args) { return task_spawn_impl(fn, args, 1, executor); }
int64_t tinox_task_await(void* handle) {
    TinoxTask* task = task_lookup((uintptr_t)handle);
    // A job waiting for a job of its own executor can occupy every worker while the awaited job sits in the
    // queue: refuse instead of deadlocking.
    if (task->executor && task->executor == current_executor_id) {
        if (!__tinox_err) __tinox_err = (int64_t)(intptr_t)"executor job cannot await a job of its own executor (nested wait would deadlock)";
        return 0;
    }
    for (TinoxTaskScope* scope = task_scope; scope; scope = atomic_load(&scope->parent))
        if (scope == task->scope) task_fatal("cyclic task await of self or ancestor");
    pthread_mutex_lock(&task_registry_mu);
    for (TinoxTask* p = task; p; p = p->waiting_on)
        if (p == current_task) task_fatal("cyclic task await");
    if (current_task) current_task->waiting_on = task;
    pthread_mutex_unlock(&task_registry_mu);
    pthread_mutex_lock(&task->mu);
    if (task->detached) task_fatal("cannot await a detached task");
    int64_t cancel_started = 0;
    while (!task->done && !task_cancellation(task_scope)) {
        if (task_cancellation(task->scope)) {
            if (!cancel_started) cancel_started = task_clock_ms();
            if (task_clock_ms() - cancel_started >= 5000)
                task_fatal("task ignored cooperative cancellation for 5 seconds during await");
        }
        task_wait_tick(task);
    }
    taskCheckCancellation();
    int64_t result = 0;
    if (!__tinox_err) {
        task_join_locked(task);
        task->observed = 1;
        __tinox_err = task->error;
        result = (int64_t)(uintptr_t)task->retval;
    }
    pthread_mutex_unlock(&task->mu);
    pthread_mutex_lock(&task_registry_mu);
    if (current_task) current_task->waiting_on = NULL;
    pthread_mutex_unlock(&task_registry_mu);
    return result;
}
void taskDetach(int64_t handle) {
    TinoxTask* task = task_lookup((uintptr_t)handle);
    if (task->executor) task_fatal("an executor job cannot be detached; shut the executor down instead");
    if (!pthread_equal(task->creator_thread, pthread_self()))
        task_fatal("only a task's owning thread may detach it");
    if (task->owner != task_scope && task->owner != task_frame_scope) task_fatal("only a task's owning scope may detach it");
    pthread_mutex_lock(&task->mu);
    if (task->joined || task->detached) task_fatal("cannot detach an awaited or detached task");
    task_pthread_check(pthread_detach(task->thread));
    task->detached = 1;
    task_remove_owner(task);
    atomic_store(&task->scope->parent, NULL);
    if (task->done) {
        if (task->error && task->error != (int64_t)(intptr_t)task_shutdown_error) {
            fprintf(stderr, "Uncaught detached task error: %s\n", (char*)(intptr_t)task->error);
            exit(1);
        }
        task_unregister(task);
    }
    pthread_mutex_unlock(&task->mu);
}
void tinox_task_spawn_detached(void* (*fn)(void*), void* args) {
    taskDetach((int64_t)(uintptr_t)task_spawn_impl(fn, args, 0, 0));
}

// ---- Coordinated services (#304); activated explicitly by @Service ----
enum { SERVICE_OFF, SERVICE_INITIALIZING, SERVICE_STARTING, SERVICE_READY,
       SERVICE_UNREADY, SERVICE_DRAINING, SERVICE_STOPPING, SERVICE_STOPPED, SERVICE_FAILED };
#define SERVICE_MAX_TASKS 1024
static atomic_int service_state;
static atomic_bool service_stop_requested, service_initialized, service_complete;
static atomic_bool service_live = true;
static atomic_int service_work, service_workers;
static _Atomic int64_t service_started_at, service_ready_at, service_shutdown_at;
static int64_t service_startup_limit, service_shutdown_limit;
static int service_expected;
static bool service_ready_tasks[SERVICE_MAX_TASKS];
static int64_t service_ports[SERVICE_MAX_TASKS];
static pthread_mutex_t service_mu = PTHREAD_MUTEX_INITIALIZER;
static char* service_error;
static pthread_t service_coordinator_thread;
static atomic_bool service_health_stop;
static int service_pipe[2] = { -1, -1 };
static volatile sig_atomic_t service_signal_fd = -1;
static volatile sig_atomic_t service_signal_pending;
extern int64_t __tinox_service_ready_checks(void) __attribute__((weak));
extern int64_t __tinox_service_live_checks(void) __attribute__((weak));
extern void __tinox_service_stop_hooks(void) __attribute__((weak));
static void service_close_database(void);

static int service_enabled(void) { return atomic_load(&service_state) != SERVICE_OFF; }
bool serviceIsStopping(void) { return service_enabled() && atomic_load(&service_stop_requested); }
static int service_should_cancel(void) {
    return serviceIsStopping() && !service_work_depth && !atomic_load(&service_work);
}
static int service_quiescing(void) { return service_enabled() && atomic_load(&service_state) >= SERVICE_STOPPING; }
bool serviceIsReady(void) { return atomic_load(&service_state) == SERVICE_READY && !serviceIsStopping(); }
bool serviceIsLive(void) {
    int state = atomic_load(&service_state);
    return state >= SERVICE_INITIALIZING && state < SERVICE_STOPPED && atomic_load(&service_live);
}
char* serviceState(void) {
    static char* const names[] = { "inactive", "initializing", "starting", "ready", "unready", "draining", "stopping", "stopped", "failed" };
    return names[atomic_load(&service_state)];
}
int64_t serviceStartedMs(void) { return atomic_load(&service_started_at); }
int64_t serviceReadyMs(void) { return atomic_load(&service_ready_at); }
int64_t serviceShutdownMs(void) { return atomic_load(&service_shutdown_at); }
static void service_wake(void) {
    char byte = 1;
    ssize_t n;
    do { n = write(service_pipe[1], &byte, 1); } while (n < 0 && errno == EINTR);
    if (n < 0 && errno != EAGAIN) task_fatal("service coordinator wake failed");
}
static int service_request_if_active(void) {
    if (!service_enabled()) return 0;
    atomic_store(&service_stop_requested, true);
    int64_t unset = 0;
    if (atomic_compare_exchange_strong(&service_shutdown_at, &unset, task_clock_ms()))
        atomic_store(&service_state, SERVICE_DRAINING);
    service_wake();
    return 1;
}
void serviceRequestShutdown(void) {
    if (!service_request_if_active()) task_fatal("serviceRequestShutdown requires @Service");
}
// The signal handler only writes to a nonblocking self-pipe and preserves errno.
static void service_signal_handler(int signal_number) {
    (void)signal_number;
    int saved = errno;
    service_signal_pending = 1;
    char byte = 1;
    if (service_signal_fd >= 0) { ssize_t ignored = write(service_signal_fd, &byte, 1); (void)ignored; }
    errno = saved;
}
static void service_record_error(const char* context, int include_thread_error) {
    pthread_mutex_lock(&service_mu);
    if (!service_error) {
        const char* detail = include_thread_error && __tinox_err ? (char*)(intptr_t)__tinox_err : NULL;
        size_t size = strlen(context) + (detail ? strlen(detail) + 2 : 0) + 1;
        service_error = GC_malloc(size);
        snprintf(service_error, size, detail ? "%s: %s" : "%s", context, detail);
    }
    pthread_mutex_unlock(&service_mu);
    atomic_store(&service_live, false);
    __tinox_err = 0;
    serviceRequestShutdown();
}
void tinox_service_fail(const char* context) { service_record_error(context, 1); }
int64_t tinox_service_check_error(const char* context) {
    if (serviceIsStopping() && __tinox_err == (int64_t)(intptr_t)task_shutdown_error) { __tinox_err = 0; return 0; }
    if (__tinox_err) { service_record_error(context, 1); return 0; }
    return 1;
}
void tinox_service_cleanup_enter(void) {
    service_cleanup_mode = 1;
    if (__tinox_err == (int64_t)(intptr_t)task_shutdown_error) __tinox_err = 0;
}
void tinox_service_cleanup_error(const char* context) { if (__tinox_err) service_record_error(context, 1); }
int64_t tinox_service_failed(void) {
    pthread_mutex_lock(&service_mu);
    int failed = service_error != NULL;
    pthread_mutex_unlock(&service_mu);
    return failed;
}
typedef struct ServiceHookFrame {
    TinoxTaskScope* scope;
    TinoxTaskScope* frame;
    const char* label;
    struct ServiceHookFrame* previous;
} ServiceHookFrame;
static __thread ServiceHookFrame* service_hook_stack;
void tinox_service_hook_begin(const char* label) {
    ServiceHookFrame* saved = GC_malloc(sizeof(*saved));
    saved->scope = task_scope; saved->frame = task_frame_scope;
    saved->label = label; saved->previous = service_hook_stack;
    service_hook_stack = saved;
    task_frame_scope = task_scope = task_new_scope(task_scope);
}
void tinox_service_hook_end(void) {
    ServiceHookFrame* saved = service_hook_stack;
    if (!saved) task_fatal("unbalanced service hook scope");
    if (__tinox_err && __tinox_err != (int64_t)(intptr_t)task_shutdown_error) {
        int64_t error = __tinox_err;
        service_record_error(saved->label, 1); __tinox_err = error;
    }
    task_close_scope(task_frame_scope, 0, 0);
    if (__tinox_err && __tinox_err != (int64_t)(intptr_t)task_shutdown_error) {
        int64_t error = __tinox_err;
        service_record_error(saved->label, 1); __tinox_err = error;
    }
    task_scope = saved->scope; task_frame_scope = saved->frame;
    service_hook_stack = saved->previous;
}
static int64_t service_invoke_health(int64_t (*check)(void), const char* label) {
    if (!check) return 1;
    tinox_service_hook_begin(label);
    int64_t result = check();
    tinox_service_hook_end();
    return result;
}
static void service_force_exit(const char* reason) {
    int flags = fcntl(STDERR_FILENO, F_GETFL);
    if (flags >= 0) (void)fcntl(STDERR_FILENO, F_SETFL, flags | O_NONBLOCK);
    (void)write(STDERR_FILENO, "tinox: ", 7);
    (void)write(STDERR_FILENO, reason, strlen(reason));
    (void)write(STDERR_FILENO, "\n", 1);
    _Exit(1);
}
static void* service_watchdog(void* unused) {
    (void)unused;
    while (!atomic_load(&service_complete)) {
        // Health callbacks can block; signal deadlines must not depend on them.
        if (service_signal_pending) service_request_if_active();
        int64_t now_ms = task_clock_ms();
        int64_t shutdown_at = atomic_load(&service_shutdown_at);
        if (shutdown_at && now_ms - shutdown_at >= service_shutdown_limit)
            service_force_exit("service shutdown deadline exceeded; forced process termination");
        if (!shutdown_at && !atomic_load(&service_ready_at) && now_ms - serviceStartedMs() >= service_startup_limit)
            service_force_exit("service startup/readiness deadline exceeded; forced process termination");
        struct timespec delay = { 0, 10000000 };
        while (nanosleep(&delay, &delay) < 0 && errno == EINTR) {}
    }
    return NULL;
}
static void* service_coordinator(void* unused) {
    (void)unused;
    tinox_gc_register_thread_roots();
    task_ensure_scope();
    while (!atomic_load(&service_health_stop)) {
        struct pollfd pfd = { .fd = service_pipe[0], .events = POLLIN };
        int rc;
        do { rc = poll(&pfd, 1, 10); } while (rc < 0 && errno == EINTR);
        if (rc < 0) service_force_exit("service coordinator poll failed");
        if (rc > 0) {
            char bytes[64];
            while (read(service_pipe[0], bytes, sizeof(bytes)) > 0) {}
            atomic_store(&service_stop_requested, true);
        }
        if (serviceIsStopping()) {
            if (!atomic_load(&service_shutdown_at)) {
                atomic_store(&service_shutdown_at, task_clock_ms());
                atomic_store(&service_state, SERVICE_DRAINING);
            }
            if (!atomic_load(&service_work) && !atomic_load(&service_workers))
                atomic_store(&service_state, SERVICE_STOPPING);
            continue;
        }
        if (!atomic_load(&service_initialized)) continue;
        int ready = 1;
        pthread_mutex_lock(&service_mu);
        for (int i = 0; i < service_expected; i++) if (!service_ready_tasks[i]) ready = 0;
        pthread_mutex_unlock(&service_mu);
        // Health functions run outside service_mu; user synchronization may block.
        if (ready) ready = service_invoke_health(__tinox_service_ready_checks, "Readiness hook failed") != 0;
        if (serviceIsStopping() && __tinox_err == (int64_t)(intptr_t)task_shutdown_error) { __tinox_err = 0; continue; }
        if (__tinox_err) { service_record_error("Readiness hook failed", 1); continue; }
        int live = service_invoke_health(__tinox_service_live_checks, "Liveness hook failed") != 0;
        if (serviceIsStopping() && __tinox_err == (int64_t)(intptr_t)task_shutdown_error) { __tinox_err = 0; continue; }
        if (__tinox_err) { service_record_error("Liveness hook failed", 1); continue; }
        atomic_store(&service_live, live != 0);
        ready = ready && live;
        if (serviceIsStopping()) continue;
        if (ready && !atomic_load(&service_ready_at)) atomic_store(&service_ready_at, task_clock_ms());
        atomic_store(&service_state, ready ? SERVICE_READY : (atomic_load(&service_ready_at) ? SERVICE_UNREADY : SERVICE_STARTING));
    }
    task_close_scope(task_frame_scope, 0, 1);
    tinox_gc_unregister_thread_roots();
    return NULL;
}
void tinox_service_begin(int64_t expected, int64_t startup_ms, int64_t shutdown_ms) {
    if (service_enabled() || expected < 0 || expected > SERVICE_MAX_TASKS || startup_ms <= 0 || shutdown_ms <= 0)
        task_fatal("invalid service lifecycle configuration");
    (void)tinox_limits(); // Validate effective concurrency configuration before initialization.
    service_expected = (int)expected;
    service_startup_limit = startup_ms;
    service_shutdown_limit = shutdown_ms;
    for (int i = 0; i < service_expected; i++) service_ports[i] = -1;
    atomic_store(&service_started_at, task_clock_ms());
    if (pipe2(service_pipe, O_NONBLOCK | O_CLOEXEC) < 0) task_fatal("service self-pipe creation failed");
    service_signal_fd = service_pipe[1];
    struct sigaction action = {0};
    action.sa_handler = service_signal_handler;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGTERM, &action, NULL) || sigaction(SIGINT, &action, NULL)) task_fatal("service signal registration failed");
    atomic_store(&service_state, SERVICE_INITIALIZING);
    pthread_t thread;
    task_pthread_check(pthread_create(&thread, NULL, service_watchdog, NULL));
    task_pthread_check(pthread_detach(thread));
    task_pthread_check(pthread_create(&service_coordinator_thread, NULL, service_coordinator, NULL));
}
void tinox_service_initialized(void) {
    atomic_store(&service_initialized, true);
    if (!serviceIsStopping()) atomic_store(&service_state, SERVICE_STARTING);
}
void tinox_service_task_enter(int64_t id) { service_task_id = (int)id; }
void tinox_service_task_exit(int64_t result) {
    int expected_stop = serviceIsStopping() && __tinox_err == (int64_t)(intptr_t)task_shutdown_error;
    if (expected_stop) __tinox_err = 0;
    if (__tinox_err) service_record_error("Required service task failed", 1);
    else if (result && !expected_stop) service_record_error("Required service startup/listener failed", 0);
    else if (!serviceIsStopping()) service_record_error("Required service task exited unexpectedly", 0);
}
void tinox_service_listener_ready(int64_t port) {
    if (!service_enabled() || service_task_id < 0) return;
    pthread_mutex_lock(&service_mu);
    if (service_task_id >= service_expected) task_fatal("invalid service listener identity");
    service_ready_tasks[service_task_id] = true;
    service_ports[service_task_id] = port;
    pthread_mutex_unlock(&service_mu);
}
int64_t serviceBoundPort(int64_t id) {
    if (!service_enabled() || id < 0 || id >= service_expected) task_fatal("serviceBoundPort requires a registered task index");
    pthread_mutex_lock(&service_mu);
    int64_t port = service_ports[id];
    pthread_mutex_unlock(&service_mu);
    return port;
}
bool serviceAwaitReady(int64_t milliseconds) {
    if (milliseconds < 0 || milliseconds > INT64_MAX - task_clock_ms()) task_fatal("serviceAwaitReady requires a nonnegative duration");
    int64_t end = task_clock_ms() + milliseconds;
    while (!serviceIsReady() && !serviceIsStopping() && task_clock_ms() < end) {
        struct timespec delay = {0, 1000000}; nanosleep(&delay, NULL);
    }
    return serviceIsReady();
}
void serviceAwaitShutdown(void) {
    if (!service_enabled()) task_fatal("serviceAwaitShutdown requires @Service");
    while (!serviceIsStopping()) { struct timespec delay = {0, 1000000}; nanosleep(&delay, NULL); }
}
int64_t tinox_service_work_enter(void) {
    if (!service_enabled()) return 1;
    if (service_work_depth) { service_work_depth++; return 1; }
    pthread_mutex_lock(&service_mu);
    int admitted = !serviceIsStopping();
    if (admitted) { atomic_fetch_add(&service_work, 1); service_work_depth = 1; }
    pthread_mutex_unlock(&service_mu);
    return admitted;
}
void tinox_service_existing_work_enter(void) {
    if (service_enabled() && !service_work_depth++) atomic_fetch_add(&service_work, 1);
}
void tinox_service_work_exit(void) {
    if (service_enabled() && service_work_depth > 0 && !--service_work_depth) atomic_fetch_sub(&service_work, 1);
}
// Admission for work that another thread will perform (an offloaded request, #552): counts as in-flight work
// from now until the performing thread calls service_work_release_token(), including the time spent queued, so a
// graceful stop waits for it. Returns 0 when the service is stopping.
static int service_work_take_token(void) {
    if (!service_enabled()) return 1;
    if (!tinox_service_work_enter()) return 0;
    service_work_depth = 0; // the count stays; this thread no longer owns it
    return 1;
}
static void service_work_release_token(void) {
    if (service_enabled()) atomic_fetch_sub(&service_work, 1);
}
int64_t tinox_service_worker_start(void) {
    pthread_mutex_lock(&service_mu);
    int admitted = !serviceIsStopping();
    if (admitted && service_enabled()) atomic_fetch_add(&service_workers, 1);
    pthread_mutex_unlock(&service_mu);
    return admitted;
}
void tinox_service_worker_end(void) {
    if (service_enabled()) {
        task_close_scope(task_frame_scope, 0, 0);
        if (__tinox_err) service_record_error("WebSocket worker child failed", 1);
        atomic_fetch_sub(&service_workers, 1);
    }
}
// Waiting readers return EOF when draining and no admitted callback remains.
// Writers still run so completed handlers can send replies/acknowledgements.
static int service_io_wait(int fd, short events) {
    if (!service_enabled()) return 1;
    for (;;) {
        if (serviceIsStopping() && !service_work_depth && !atomic_load(&service_work)) return 0;
        struct pollfd pfd = { .fd = fd, .events = events };
        int rc = poll(&pfd, 1, 10);
        if (rc < 0 && errno == EINTR) continue;
        if (rc < 0 || (pfd.revents & POLLNVAL)) return 0;
        if (rc > 0) return 1;
    }
}
void tinox_service_main_returned(int64_t main_result) {
    if (serviceIsStopping() && __tinox_err == (int64_t)(intptr_t)task_shutdown_error) __tinox_err = 0;
    if (__tinox_err) service_record_error("Main failed", 1);
    else if (main_result) service_record_error("Main returned a nonzero status", 0);
    else if (!service_expected) serviceRequestShutdown();
}
int64_t tinox_service_finish(int64_t main_result) {
    if (serviceIsStopping() && __tinox_err == (int64_t)(intptr_t)task_shutdown_error) __tinox_err = 0;
    if (__tinox_err) service_record_error("Main failed", 1);
    else if (main_result) service_record_error("Main returned a nonzero status", 0);
    serviceRequestShutdown();
    if (!atomic_load(&service_shutdown_at)) atomic_store(&service_shutdown_at, task_clock_ms());
    // Detached connection workers own their OnClose descendants. They must
    // finish before shared resources are torn down; the watchdog bounds this wait.
    while (atomic_load(&service_work) || atomic_load(&service_workers)) {
        struct timespec delay = {0, 1000000};
        while (nanosleep(&delay, &delay) < 0 && errno == EINTR) {}
    }
    // All generated listeners are joined by bootstrap before this point. Join
    // the remaining structured children before running resource-disposal hooks.
    atomic_store(&service_health_stop, true);
    service_wake();
    task_pthread_check(pthread_join(service_coordinator_thread, NULL));
    task_request_shutdown_now();
    task_close_scope(task_frame_scope, 0, 1);
    if (__tinox_err == (int64_t)(intptr_t)task_shutdown_error) __tinox_err = 0;
    if (__tinox_err) service_record_error("Background task failed during shutdown", 1);
    service_cleanup_mode = 1;
    task_frame_scope = task_scope = task_new_scope(NULL);
    if (__tinox_service_stop_hooks) __tinox_service_stop_hooks();
    task_close_scope(task_frame_scope, 0, 0);
    if (__tinox_err) service_record_error("Shutdown cleanup failed", 1);
    service_close_database();
    int failed = (int)tinox_service_failed();
    atomic_store(&service_state, failed ? SERVICE_FAILED : SERVICE_STOPPED);
    atomic_store(&service_live, false);
    atomic_store(&service_complete, true);
    if (failed) fprintf(stderr, "tinox: service failed: %s\n", service_error);
    return failed ? 1 : main_result;
}

void* tinox_channel_create(void) {
    TinoxChannel* ch = calloc(1, sizeof(TinoxChannel));
    pthread_mutex_init(&ch->mutex, NULL);
    task_cond_init(&ch->cond);
    return ch;
}

void tinox_channel_send(void* handle, int64_t value) {
    TinoxChannel* ch = (TinoxChannel*)handle;
    TinoxChannelNode* node = malloc(sizeof(TinoxChannelNode));
    node->value = value;
    node->next = NULL;
    pthread_mutex_lock(&ch->mutex);
    if (ch->tail) ch->tail->next = node; else ch->head = node;
    ch->tail = node;
    pthread_cond_signal(&ch->cond);
    pthread_mutex_unlock(&ch->mutex);
}

int64_t tinox_channel_recv(void* handle) {
    taskCheckCancellation();
    if (__tinox_err) return 0;
    TinoxChannel* ch = (TinoxChannel*)handle;
    pthread_mutex_lock(&ch->mutex);
    while (!ch->head) {
        taskCheckCancellation();
        if (__tinox_err) { pthread_mutex_unlock(&ch->mutex); return 0; }
        struct timespec ts;
        clock_gettime(CLOCK_MONOTONIC, &ts);
        ts.tv_nsec += 10000000;
        if (ts.tv_nsec >= 1000000000) { ts.tv_sec++; ts.tv_nsec -= 1000000000; }
        int rc = pthread_cond_timedwait(&ch->cond, &ch->mutex, &ts);
        if (rc != ETIMEDOUT) task_pthread_check(rc);
    }
    TinoxChannelNode* node = ch->head;
    ch->head = node->next;
    if (!ch->head) ch->tail = NULL;
    int64_t val = node->value;
    free(node);
    pthread_mutex_unlock(&ch->mutex);
    return val;
}

// Non-blocking recv: returns 1 and stores value if a message is ready, else returns 0.
int tinox_channel_try_recv(void* handle, int64_t* out) {
    TinoxChannel* ch = (TinoxChannel*)handle;
    pthread_mutex_lock(&ch->mutex);
    if (!ch->head) {
        pthread_mutex_unlock(&ch->mutex);
        return 0;
    }
    TinoxChannelNode* node = ch->head;
    ch->head = node->next;
    if (!ch->head) ch->tail = NULL;
    *out = node->value;
    free(node);
    pthread_mutex_unlock(&ch->mutex);
    return 1;
}

// ---- Map (open-addressing hash table, string keys, i64 values) ----

#define TINOX_MAP_INIT_CAP 16
#define TINOX_MAP_LOAD_NUM 3
#define TINOX_MAP_LOAD_DEN 4

typedef struct TinoxMapEntry {
    char*   key;   // NULL = empty slot, (char*)1 = tombstone
    int64_t value;
} TinoxMapEntry;

typedef struct TinoxMap {
    TinoxMapEntry* entries;
    size_t cap;
    size_t len;
    int borrowed_keys; // 1 = keys/entries are arena-owned, don't free
    unsigned int tombs; // tombstone slots (lives in the padding after borrowed_keys)
} TinoxMap;

static size_t map_hash(const char* key, size_t cap) {
    size_t h = 14695981039346656037ULL;
    for (const unsigned char* p = (const unsigned char*)key; *p; p++)
        h = (h ^ *p) * 1099511628211ULL;
    return h & (cap - 1);
}

static void map_rehash(TinoxMap* m); // forward declaration

// Reset a map without freeing keys/entries (for static maps with borrowed_keys=1)
static void tinox_map_reset(TinoxMap* m) {
    TINOX_CK_REG(m, TINOX_KIND_MAP);
    m->len = 0;
    m->tombs = 0;
    memset(m->entries, 0, m->cap * sizeof(TinoxMapEntry));
}

// Store key without strdup — caller guarantees key lifetime exceeds map use
// Insert-or-update by bounded probe. Probing never relies on finding a NULL
// slot (tombstones can fill the table); a key is only inserted after the whole
// chain proved it absent, reusing the first tombstone seen on the way.
static void map_store(TinoxMap* m, const char* key, int64_t value, int copy_key) {
    if ((size_t)(m->len + m->tombs + 1) * TINOX_MAP_LOAD_DEN > m->cap * TINOX_MAP_LOAD_NUM)
        map_rehash(m);
    size_t idx = map_hash(key, m->cap);
    size_t tomb = (size_t)-1;
    for (size_t n = 0; n < m->cap; n++) {
        char* k = m->entries[idx].key;
        if (!k) break;
        if (k == (char*)1) {
            if (tomb == (size_t)-1) tomb = idx;
        } else if (strcmp(k, key) == 0) {
            m->entries[idx].value = value;
            return;
        }
        idx = (idx + 1) & (m->cap - 1);
    }
    if (tomb != (size_t)-1) { idx = tomb; m->tombs--; }
    m->entries[idx].key   = copy_key ? strdup(key) : (char*)key;
    m->entries[idx].value = value;
    m->len++;
}

// Store key without strdup — caller guarantees key lifetime exceeds map use
static void tinox_map_set_borrow(void* map, const char* key, int64_t value) {
    map_store((TinoxMap*)map, key, value, 0);
}

static void map_rehash(TinoxMap* m) {
    // Grow only when live entries need it; otherwise rebuild at the same size
    // to drop the tombstones.
    size_t new_cap = m->cap;
    if ((size_t)(m->len + 1) * TINOX_MAP_LOAD_DEN * 2 > m->cap * TINOX_MAP_LOAD_NUM)
        new_cap = m->cap * 2;
    TinoxMapEntry* new_entries = calloc(new_cap, sizeof(TinoxMapEntry));
    for (size_t i = 0; i < m->cap; i++) {
        char* k = m->entries[i].key;
        if (!k || k == (char*)1) continue;
        size_t idx = map_hash(k, new_cap);
        while (new_entries[idx].key) idx = (idx + 1) & (new_cap - 1);
        new_entries[idx].key   = k;
        new_entries[idx].value = m->entries[i].value;
    }
    if (!m->borrowed_keys) free(m->entries);
    m->entries = new_entries;
    m->cap     = new_cap;
    m->tombs   = 0;
    m->borrowed_keys = 0; // entries now heap-allocated
}

void* tinox_map_create(void) {
    TinoxMap* m = malloc(sizeof(TinoxMap));
    m->cap          = TINOX_MAP_INIT_CAP;
    m->len          = 0;
    m->tombs        = 0;
    m->entries      = calloc(m->cap, sizeof(TinoxMapEntry));
    m->borrowed_keys = 0;
    TINOX_CK_REG(m, TINOX_KIND_MAP);
    return m;
}

void tinox_map_set(void* map, const char* key, int64_t value) {
    TINOX_CK_EXPECT(map, TINOX_KIND_MAP, "map_set");
    TinoxMap* m = (TinoxMap*)map;
    map_store(m, key, value, !m->borrowed_keys);
}

// Bounded probe: index of the live slot holding key, or (size_t)-1.
static size_t map_find(TinoxMap* m, const char* key) {
    size_t idx = map_hash(key, m->cap);
    for (size_t n = 0; n < m->cap; n++) {
        char* k = m->entries[idx].key;
        if (!k) return (size_t)-1;
        if (k != (char*)1 && strcmp(k, key) == 0) return idx;
        idx = (idx + 1) & (m->cap - 1);
    }
    return (size_t)-1;
}

int64_t tinox_map_get(void* map, const char* key) {
    TINOX_CK_EXPECT(map, TINOX_KIND_MAP, "map_get");
    TinoxMap* m = (TinoxMap*)map;
    size_t i = map_find(m, key);
    return i == (size_t)-1 ? 0 : m->entries[i].value;
}

int64_t tinox_map_contains(void* map, const char* key) {
    TINOX_CK_EXPECT(map, TINOX_KIND_MAP, "map_contains");
    return map_find((TinoxMap*)map, key) != (size_t)-1;
}

void tinox_map_remove(void* map, const char* key) {
    TINOX_CK_EXPECT(map, TINOX_KIND_MAP, "map_remove");
    TinoxMap* m = (TinoxMap*)map;
    size_t i = map_find(m, key);
    if (i == (size_t)-1) return;
    if (!m->borrowed_keys) free(m->entries[i].key);
    m->entries[i].key = (char*)1; // tombstone
    m->len--;
    m->tombs++;
}

int64_t tinox_map_len(void* map) {
    TINOX_CK_EXPECT(map, TINOX_KIND_MAP, "map_len");
    return (int64_t)((TinoxMap*)map)->len;
}

int64_t* tinox_map_keys(void* map) {
    TINOX_CK_EXPECT(map, TINOX_KIND_MAP, "map_keys");
    TinoxMap* m = (TinoxMap*)map;
    int64_t* nh = tinox_array_new((int64_t)m->len, 0);
    int64_t* nd = ((TinoxArray*)nh)->data;
    size_t j = 0;
    for (size_t i = 0; i < m->cap; i++) {
        char* k = m->entries[i].key;
        if (k && k != (char*)1)
            nd[j++] = (int64_t)(uintptr_t)k;
    }
    return nh;
}

int64_t* tinox_map_values(void* map) {
    TINOX_CK_EXPECT(map, TINOX_KIND_MAP, "map_values");
    TinoxMap* m = (TinoxMap*)map;
    int64_t* nh = tinox_array_new((int64_t)m->len, 0);
    int64_t* nd = ((TinoxArray*)nh)->data;
    size_t j = 0;
    for (size_t i = 0; i < m->cap; i++) {
        char* k = m->entries[i].key;
        if (k && k != (char*)1)
            nd[j++] = m->entries[i].value;
    }
    return nh;
}

void tinox_map_free(void* map) {
    TinoxMap* m = (TinoxMap*)map;
    if (m->borrowed_keys) return; // arena-owned memory, nothing to free
    for (size_t i = 0; i < m->cap; i++) {
        char* k = m->entries[i].key;
        if (k && k != (char*)1) free(k);
    }
    free(m->entries);
    free(m);
}

// Returns a partially masked version of a string:
// keeps up to 2 leading and 2 trailing chars, replaces the middle with "***".
// Short strings (len <= 4) are fully replaced with "***".
char* tinox_string_mask_partial(const char* s) {
    size_t len = strlen(s);
    if (len <= 4) {
        char* r = malloc(4); memcpy(r, "***", 4); return r;
    }
    size_t prefix = 2, suffix = 2;
    // result: prefix chars + "***" + suffix chars
    size_t rlen = prefix + 3 + suffix;
    char* result = malloc(rlen + 1);
    memcpy(result, s, prefix);
    memcpy(result + prefix, "***", 3);
    memcpy(result + prefix + 3, s + len - suffix, suffix);
    result[rlen] = '\0';
    return result;
}

// Byte value at index, bounds-checked: returns -1 for an out-of-range index
// instead of reading past the string (the inline codegen version did an
// unchecked load → garbage / UB on out-of-bounds access).
int64_t tinox_string_char_code_at(const char* s, int64_t idx) {
    if (!s || idx < 0) return -1;
    int64_t len = (int64_t)strlen(s);
    if (idx >= len) return -1;
    return (int64_t)(unsigned char)s[idx];
}

// 1 when the whole NUL-terminated string is well-formed UTF-8.
static int tinox_utf8_valid(const unsigned char* s) {
    while (*s) {
        int n;
        if (*s < 0x80) n = 1;
        else if ((*s & 0xE0) == 0xC0 && *s >= 0xC2) n = 2;
        else if ((*s & 0xF0) == 0xE0) n = 3;
        else if ((*s & 0xF8) == 0xF0 && *s <= 0xF4) n = 4;
        else return 0;
        for (int i = 1; i < n; i++) if ((s[i] & 0xC0) != 0x80) return 0;
        s += n;
    }
    return 1;
}

// Decode one Unicode scalar value without reading beyond the remaining bytes.
// Reject overlong encodings, surrogate code points and values above U+10FFFF.
static size_t tinox_utf8_scalar_width(const unsigned char* s, size_t remaining) {
    unsigned char first = s[0];
    size_t width;
    if (first < 0x80) return 1;
    if (first >= 0xC2 && first <= 0xDF) width = 2;
    else if (first >= 0xE0 && first <= 0xEF) width = 3;
    else if (first >= 0xF0 && first <= 0xF4) width = 4;
    else return 0;
    if (remaining < width) return 0;
    for (size_t i = 1; i < width; i++) {
        if ((s[i] & 0xC0) != 0x80) return 0;
    }
    if ((first == 0xE0 && s[1] < 0xA0) ||
        (first == 0xED && s[1] >= 0xA0) ||
        (first == 0xF0 && s[1] < 0x90) ||
        (first == 0xF4 && s[1] >= 0x90)) return 0;
    return width;
}

// Additive Unicode API: existing indexing/length/iteration remain byte based.
int64_t* tinox_string_chars(const char* s) {
    if (!s) {
        fprintf(stderr, "runtime error: String.chars requires a non-null string\n");
        exit(1);
    }
    size_t length = strlen(s), count = 0;
    for (size_t offset = 0; offset < length;) {
        size_t width = tinox_utf8_scalar_width((const unsigned char*)s + offset, length - offset);
        if (!width) {
            fprintf(stderr, "runtime error: String.chars invalid UTF-8 at byte %zu\n", offset);
            exit(1);
        }
        offset += width;
        count++;
    }
    if (count > INT64_MAX || count > SIZE_MAX / sizeof(int64_t)) {
        fprintf(stderr, "runtime error: String.chars result is too large\n");
        exit(1);
    }
    int64_t* result = tinox_array_new((int64_t)count, 0);
    int64_t* data = ((TinoxArray*)result)->data;
    size_t offset = 0;
    for (size_t i = 0; i < count; i++) {
        size_t width = tinox_utf8_scalar_width((const unsigned char*)s + offset, length - offset);
        char* scalar = (char*)malloc(width + 1);
        if (!scalar) {
            fprintf(stderr, "runtime error: String.chars allocation failed\n");
            exit(1);
        }
        memcpy(scalar, s + offset, width);
        scalar[width] = '\0';
        data[i] = (int64_t)(uintptr_t)scalar;
        offset += width;
    }
    return result;
}

// ---- tinox.core.unicode: code points and full default case mapping --------------------
// Byte-based String semantics are untouched; these functions work on UTF-8 explicitly and
// never replace malformed input. The Tinox wrappers validate first and throw catchable
// errors, the aborts below are the defence for a direct native call.

// BEGIN GENERATED UNICODE CASE TABLES (Unicode 16.0.0, scripts/gen-unicode-case.py)
#define TINOX_UNICODE_VERSION "16.0.0"
static const uint32_t tinox_unicode_upper_pool[] = {
    65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76,
    77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88,
    89, 90, 924, 83, 83, 192, 193, 194, 195, 196, 197, 198,
    199, 200, 201, 202, 203, 204, 205, 206, 207, 208, 209, 210,
    211, 212, 213, 214, 216, 217, 218, 219, 220, 221, 222, 376,
    256, 258, 260, 262, 264, 266, 268, 270, 272, 274, 276, 278,
    280, 282, 284, 286, 288, 290, 292, 294, 296, 298, 300, 302,
    73, 306, 308, 310, 313, 315, 317, 319, 321, 323, 325, 327,
    700, 78, 330, 332, 334, 336, 338, 340, 342, 344, 346, 348,
    350, 352, 354, 356, 358, 360, 362, 364, 366, 368, 370, 372,
    374, 377, 379, 381, 83, 579, 386, 388, 391, 395, 401, 502,
    408, 573, 42972, 544, 416, 418, 420, 423, 428, 431, 435, 437,
    440, 444, 503, 452, 452, 455, 455, 458, 458, 461, 463, 465,
    467, 469, 471, 473, 475, 398, 478, 480, 482, 484, 486, 488,
    490, 492, 494, 74, 780, 497, 497, 500, 504, 506, 508, 510,
    512, 514, 516, 518, 520, 522, 524, 526, 528, 530, 532, 534,
    536, 538, 540, 542, 546, 548, 550, 552, 554, 556, 558, 560,
    562, 571, 11390, 11391, 577, 582, 584, 586, 588, 590, 11375, 11373,
    11376, 385, 390, 393, 394, 399, 400, 42923, 403, 42924, 404, 42955,
    42893, 42922, 407, 406, 42926, 11362, 42925, 412, 11374, 413, 415, 11364,
    422, 42949, 425, 42929, 430, 580, 433, 434, 581, 439, 42930, 42928,
    921, 880, 882, 886, 1021, 1022, 1023, 921, 776, 769, 902, 904,
    905, 906, 933, 776, 769, 913, 914, 915, 916, 917, 918, 919,
    920, 921, 922, 923, 924, 925, 926, 927, 928, 929, 931, 931,
    932, 933, 934, 935, 936, 937, 938, 939, 908, 910, 911, 914,
    920, 934, 928, 975, 984, 986, 988, 990, 992, 994, 996, 998,
    1000, 1002, 1004, 1006, 922, 929, 1017, 895, 917, 1015, 1018, 1040,
    1041, 1042, 1043, 1044, 1045, 1046, 1047, 1048, 1049, 1050, 1051, 1052,
    1053, 1054, 1055, 1056, 1057, 1058, 1059, 1060, 1061, 1062, 1063, 1064,
    1065, 1066, 1067, 1068, 1069, 1070, 1071, 1024, 1025, 1026, 1027, 1028,
    1029, 1030, 1031, 1032, 1033, 1034, 1035, 1036, 1037, 1038, 1039, 1120,
    1122, 1124, 1126, 1128, 1130, 1132, 1134, 1136, 1138, 1140, 1142, 1144,
    1146, 1148, 1150, 1152, 1162, 1164, 1166, 1168, 1170, 1172, 1174, 1176,
    1178, 1180, 1182, 1184, 1186, 1188, 1190, 1192, 1194, 1196, 1198, 1200,
    1202, 1204, 1206, 1208, 1210, 1212, 1214, 1217, 1219, 1221, 1223, 1225,
    1227, 1229, 1216, 1232, 1234, 1236, 1238, 1240, 1242, 1244, 1246, 1248,
    1250, 1252, 1254, 1256, 1258, 1260, 1262, 1264, 1266, 1268, 1270, 1272,
    1274, 1276, 1278, 1280, 1282, 1284, 1286, 1288, 1290, 1292, 1294, 1296,
    1298, 1300, 1302, 1304, 1306, 1308, 1310, 1312, 1314, 1316, 1318, 1320,
    1322, 1324, 1326, 1329, 1330, 1331, 1332, 1333, 1334, 1335, 1336, 1337,
    1338, 1339, 1340, 1341, 1342, 1343, 1344, 1345, 1346, 1347, 1348, 1349,
    1350, 1351, 1352, 1353, 1354, 1355, 1356, 1357, 1358, 1359, 1360, 1361,
    1362, 1363, 1364, 1365, 1366, 1333, 1362, 7312, 7313, 7314, 7315, 7316,
    7317, 7318, 7319, 7320, 7321, 7322, 7323, 7324, 7325, 7326, 7327, 7328,
    7329, 7330, 7331, 7332, 7333, 7334, 7335, 7336, 7337, 7338, 7339, 7340,
    7341, 7342, 7343, 7344, 7345, 7346, 7347, 7348, 7349, 7350, 7351, 7352,
    7353, 7354, 7357, 7358, 7359, 5104, 5105, 5106, 5107, 5108, 5109, 1042,
    1044, 1054, 1057, 1058, 1058, 1066, 1122, 42570, 7305, 42877, 11363, 42950,
    7680, 7682, 7684, 7686, 7688, 7690, 7692, 7694, 7696, 7698, 7700, 7702,
    7704, 7706, 7708, 7710, 7712, 7714, 7716, 7718, 7720, 7722, 7724, 7726,
    7728, 7730, 7732, 7734, 7736, 7738, 7740, 7742, 7744, 7746, 7748, 7750,
    7752, 7754, 7756, 7758, 7760, 7762, 7764, 7766, 7768, 7770, 7772, 7774,
    7776, 7778, 7780, 7782, 7784, 7786, 7788, 7790, 7792, 7794, 7796, 7798,
    7800, 7802, 7804, 7806, 7808, 7810, 7812, 7814, 7816, 7818, 7820, 7822,
    7824, 7826, 7828, 72, 817, 84, 776, 87, 778, 89, 778, 65,
    702, 7776, 7840, 7842, 7844, 7846, 7848, 7850, 7852, 7854, 7856, 7858,
    7860, 7862, 7864, 7866, 7868, 7870, 7872, 7874, 7876, 7878, 7880, 7882,
    7884, 7886, 7888, 7890, 7892, 7894, 7896, 7898, 7900, 7902, 7904, 7906,
    7908, 7910, 7912, 7914, 7916, 7918, 7920, 7922, 7924, 7926, 7928, 7930,
    7932, 7934, 7944, 7945, 7946, 7947, 7948, 7949, 7950, 7951, 7960, 7961,
    7962, 7963, 7964, 7965, 7976, 7977, 7978, 7979, 7980, 7981, 7982, 7983,
    7992, 7993, 7994, 7995, 7996, 7997, 7998, 7999, 8008, 8009, 8010, 8011,
    8012, 8013, 933, 787, 8025, 933, 787, 768, 8027, 933, 787, 769,
    8029, 933, 787, 834, 8031, 8040, 8041, 8042, 8043, 8044, 8045, 8046,
    8047, 8122, 8123, 8136, 8137, 8138, 8139, 8154, 8155, 8184, 8185, 8170,
    8171, 8186, 8187, 7944, 921, 7945, 921, 7946, 921, 7947, 921, 7948,
    921, 7949, 921, 7950, 921, 7951, 921, 7944, 921, 7945, 921, 7946,
    921, 7947, 921, 7948, 921, 7949, 921, 7950, 921, 7951, 921, 7976,
    921, 7977, 921, 7978, 921, 7979, 921, 7980, 921, 7981, 921, 7982,
    921, 7983, 921, 7976, 921, 7977, 921, 7978, 921, 7979, 921, 7980,
    921, 7981, 921, 7982, 921, 7983, 921, 8040, 921, 8041, 921, 8042,
    921, 8043, 921, 8044, 921, 8045, 921, 8046, 921, 8047, 921, 8040,
    921, 8041, 921, 8042, 921, 8043, 921, 8044, 921, 8045, 921, 8046,
    921, 8047, 921, 8120, 8121, 8122, 921, 913, 921, 902, 921, 913,
    834, 913, 834, 921, 913, 921, 921, 8138, 921, 919, 921, 905,
    921, 919, 834, 919, 834, 921, 919, 921, 8152, 8153, 921, 776,
    768, 921, 776, 769, 921, 834, 921, 776, 834, 8168, 8169, 933,
    776, 768, 933, 776, 769, 929, 787, 8172, 933, 834, 933, 776,
    834, 8186, 921, 937, 921, 911, 921, 937, 834, 937, 834, 921,
    937, 921, 8498, 8544, 8545, 8546, 8547, 8548, 8549, 8550, 8551, 8552,
    8553, 8554, 8555, 8556, 8557, 8558, 8559, 8579, 9398, 9399, 9400, 9401,
    9402, 9403, 9404, 9405, 9406, 9407, 9408, 9409, 9410, 9411, 9412, 9413,
    9414, 9415, 9416, 9417, 9418, 9419, 9420, 9421, 9422, 9423, 11264, 11265,
    11266, 11267, 11268, 11269, 11270, 11271, 11272, 11273, 11274, 11275, 11276, 11277,
    11278, 11279, 11280, 11281, 11282, 11283, 11284, 11285, 11286, 11287, 11288, 11289,
    11290, 11291, 11292, 11293, 11294, 11295, 11296, 11297, 11298, 11299, 11300, 11301,
    11302, 11303, 11304, 11305, 11306, 11307, 11308, 11309, 11310, 11311, 11360, 570,
    574, 11367, 11369, 11371, 11378, 11381, 11392, 11394, 11396, 11398, 11400, 11402,
    11404, 11406, 11408, 11410, 11412, 11414, 11416, 11418, 11420, 11422, 11424, 11426,
    11428, 11430, 11432, 11434, 11436, 11438, 11440, 11442, 11444, 11446, 11448, 11450,
    11452, 11454, 11456, 11458, 11460, 11462, 11464, 11466, 11468, 11470, 11472, 11474,
    11476, 11478, 11480, 11482, 11484, 11486, 11488, 11490, 11499, 11501, 11506, 4256,
    4257, 4258, 4259, 4260, 4261, 4262, 4263, 4264, 4265, 4266, 4267, 4268,
    4269, 4270, 4271, 4272, 4273, 4274, 4275, 4276, 4277, 4278, 4279, 4280,
    4281, 4282, 4283, 4284, 4285, 4286, 4287, 4288, 4289, 4290, 4291, 4292,
    4293, 4295, 4301, 42560, 42562, 42564, 42566, 42568, 42570, 42572, 42574, 42576,
    42578, 42580, 42582, 42584, 42586, 42588, 42590, 42592, 42594, 42596, 42598, 42600,
    42602, 42604, 42624, 42626, 42628, 42630, 42632, 42634, 42636, 42638, 42640, 42642,
    42644, 42646, 42648, 42650, 42786, 42788, 42790, 42792, 42794, 42796, 42798, 42802,
    42804, 42806, 42808, 42810, 42812, 42814, 42816, 42818, 42820, 42822, 42824, 42826,
    42828, 42830, 42832, 42834, 42836, 42838, 42840, 42842, 42844, 42846, 42848, 42850,
    42852, 42854, 42856, 42858, 42860, 42862, 42873, 42875, 42878, 42880, 42882, 42884,
    42886, 42891, 42896, 42898, 42948, 42902, 42904, 42906, 42908, 42910, 42912, 42914,
    42916, 42918, 42920, 42932, 42934, 42936, 42938, 42940, 42942, 42944, 42946, 42951,
    42953, 42956, 42960, 42966, 42968, 42970, 42997, 42931, 5024, 5025, 5026, 5027,
    5028, 5029, 5030, 5031, 5032, 5033, 5034, 5035, 5036, 5037, 5038, 5039,
    5040, 5041, 5042, 5043, 5044, 5045, 5046, 5047, 5048, 5049, 5050, 5051,
    5052, 5053, 5054, 5055, 5056, 5057, 5058, 5059, 5060, 5061, 5062, 5063,
    5064, 5065, 5066, 5067, 5068, 5069, 5070, 5071, 5072, 5073, 5074, 5075,
    5076, 5077, 5078, 5079, 5080, 5081, 5082, 5083, 5084, 5085, 5086, 5087,
    5088, 5089, 5090, 5091, 5092, 5093, 5094, 5095, 5096, 5097, 5098, 5099,
    5100, 5101, 5102, 5103, 70, 70, 70, 73, 70, 76, 70, 70,
    73, 70, 70, 76, 83, 84, 83, 84, 1348, 1350, 1348, 1333,
    1348, 1339, 1358, 1350, 1348, 1341, 65313, 65314, 65315, 65316, 65317, 65318,
    65319, 65320, 65321, 65322, 65323, 65324, 65325, 65326, 65327, 65328, 65329, 65330,
    65331, 65332, 65333, 65334, 65335, 65336, 65337, 65338, 66560, 66561, 66562, 66563,
    66564, 66565, 66566, 66567, 66568, 66569, 66570, 66571, 66572, 66573, 66574, 66575,
    66576, 66577, 66578, 66579, 66580, 66581, 66582, 66583, 66584, 66585, 66586, 66587,
    66588, 66589, 66590, 66591, 66592, 66593, 66594, 66595, 66596, 66597, 66598, 66599,
    66736, 66737, 66738, 66739, 66740, 66741, 66742, 66743, 66744, 66745, 66746, 66747,
    66748, 66749, 66750, 66751, 66752, 66753, 66754, 66755, 66756, 66757, 66758, 66759,
    66760, 66761, 66762, 66763, 66764, 66765, 66766, 66767, 66768, 66769, 66770, 66771,
    66928, 66929, 66930, 66931, 66932, 66933, 66934, 66935, 66936, 66937, 66938, 66940,
    66941, 66942, 66943, 66944, 66945, 66946, 66947, 66948, 66949, 66950, 66951, 66952,
    66953, 66954, 66956, 66957, 66958, 66959, 66960, 66961, 66962, 66964, 66965, 68736,
    68737, 68738, 68739, 68740, 68741, 68742, 68743, 68744, 68745, 68746, 68747, 68748,
    68749, 68750, 68751, 68752, 68753, 68754, 68755, 68756, 68757, 68758, 68759, 68760,
    68761, 68762, 68763, 68764, 68765, 68766, 68767, 68768, 68769, 68770, 68771, 68772,
    68773, 68774, 68775, 68776, 68777, 68778, 68779, 68780, 68781, 68782, 68783, 68784,
    68785, 68786, 68944, 68945, 68946, 68947, 68948, 68949, 68950, 68951, 68952, 68953,
    68954, 68955, 68956, 68957, 68958, 68959, 68960, 68961, 68962, 68963, 68964, 68965,
    71840, 71841, 71842, 71843, 71844, 71845, 71846, 71847, 71848, 71849, 71850, 71851,
    71852, 71853, 71854, 71855, 71856, 71857, 71858, 71859, 71860, 71861, 71862, 71863,
    71864, 71865, 71866, 71867, 71868, 71869, 71870, 71871, 93760, 93761, 93762, 93763,
    93764, 93765, 93766, 93767, 93768, 93769, 93770, 93771, 93772, 93773, 93774, 93775,
    93776, 93777, 93778, 93779, 93780, 93781, 93782, 93783, 93784, 93785, 93786, 93787,
    93788, 93789, 93790, 93791, 125184, 125185, 125186, 125187, 125188, 125189, 125190, 125191,
    125192, 125193, 125194, 125195, 125196, 125197, 125198, 125199, 125200, 125201, 125202, 125203,
    125204, 125205, 125206, 125207, 125208, 125209, 125210, 125211, 125212, 125213, 125214, 125215,
    125216, 125217,
};
static const struct { uint32_t cp; uint16_t offset; uint8_t len; } tinox_unicode_upper_map[] = {
    {97,0,1}, {98,1,1}, {99,2,1}, {100,3,1}, {101,4,1}, {102,5,1},
    {103,6,1}, {104,7,1}, {105,8,1}, {106,9,1}, {107,10,1}, {108,11,1},
    {109,12,1}, {110,13,1}, {111,14,1}, {112,15,1}, {113,16,1}, {114,17,1},
    {115,18,1}, {116,19,1}, {117,20,1}, {118,21,1}, {119,22,1}, {120,23,1},
    {121,24,1}, {122,25,1}, {181,26,1}, {223,27,2}, {224,29,1}, {225,30,1},
    {226,31,1}, {227,32,1}, {228,33,1}, {229,34,1}, {230,35,1}, {231,36,1},
    {232,37,1}, {233,38,1}, {234,39,1}, {235,40,1}, {236,41,1}, {237,42,1},
    {238,43,1}, {239,44,1}, {240,45,1}, {241,46,1}, {242,47,1}, {243,48,1},
    {244,49,1}, {245,50,1}, {246,51,1}, {248,52,1}, {249,53,1}, {250,54,1},
    {251,55,1}, {252,56,1}, {253,57,1}, {254,58,1}, {255,59,1}, {257,60,1},
    {259,61,1}, {261,62,1}, {263,63,1}, {265,64,1}, {267,65,1}, {269,66,1},
    {271,67,1}, {273,68,1}, {275,69,1}, {277,70,1}, {279,71,1}, {281,72,1},
    {283,73,1}, {285,74,1}, {287,75,1}, {289,76,1}, {291,77,1}, {293,78,1},
    {295,79,1}, {297,80,1}, {299,81,1}, {301,82,1}, {303,83,1}, {305,84,1},
    {307,85,1}, {309,86,1}, {311,87,1}, {314,88,1}, {316,89,1}, {318,90,1},
    {320,91,1}, {322,92,1}, {324,93,1}, {326,94,1}, {328,95,1}, {329,96,2},
    {331,98,1}, {333,99,1}, {335,100,1}, {337,101,1}, {339,102,1}, {341,103,1},
    {343,104,1}, {345,105,1}, {347,106,1}, {349,107,1}, {351,108,1}, {353,109,1},
    {355,110,1}, {357,111,1}, {359,112,1}, {361,113,1}, {363,114,1}, {365,115,1},
    {367,116,1}, {369,117,1}, {371,118,1}, {373,119,1}, {375,120,1}, {378,121,1},
    {380,122,1}, {382,123,1}, {383,124,1}, {384,125,1}, {387,126,1}, {389,127,1},
    {392,128,1}, {396,129,1}, {402,130,1}, {405,131,1}, {409,132,1}, {410,133,1},
    {411,134,1}, {414,135,1}, {417,136,1}, {419,137,1}, {421,138,1}, {424,139,1},
    {429,140,1}, {432,141,1}, {436,142,1}, {438,143,1}, {441,144,1}, {445,145,1},
    {447,146,1}, {453,147,1}, {454,148,1}, {456,149,1}, {457,150,1}, {459,151,1},
    {460,152,1}, {462,153,1}, {464,154,1}, {466,155,1}, {468,156,1}, {470,157,1},
    {472,158,1}, {474,159,1}, {476,160,1}, {477,161,1}, {479,162,1}, {481,163,1},
    {483,164,1}, {485,165,1}, {487,166,1}, {489,167,1}, {491,168,1}, {493,169,1},
    {495,170,1}, {496,171,2}, {498,173,1}, {499,174,1}, {501,175,1}, {505,176,1},
    {507,177,1}, {509,178,1}, {511,179,1}, {513,180,1}, {515,181,1}, {517,182,1},
    {519,183,1}, {521,184,1}, {523,185,1}, {525,186,1}, {527,187,1}, {529,188,1},
    {531,189,1}, {533,190,1}, {535,191,1}, {537,192,1}, {539,193,1}, {541,194,1},
    {543,195,1}, {547,196,1}, {549,197,1}, {551,198,1}, {553,199,1}, {555,200,1},
    {557,201,1}, {559,202,1}, {561,203,1}, {563,204,1}, {572,205,1}, {575,206,1},
    {576,207,1}, {578,208,1}, {583,209,1}, {585,210,1}, {587,211,1}, {589,212,1},
    {591,213,1}, {592,214,1}, {593,215,1}, {594,216,1}, {595,217,1}, {596,218,1},
    {598,219,1}, {599,220,1}, {601,221,1}, {603,222,1}, {604,223,1}, {608,224,1},
    {609,225,1}, {611,226,1}, {612,227,1}, {613,228,1}, {614,229,1}, {616,230,1},
    {617,231,1}, {618,232,1}, {619,233,1}, {620,234,1}, {623,235,1}, {625,236,1},
    {626,237,1}, {629,238,1}, {637,239,1}, {640,240,1}, {642,241,1}, {643,242,1},
    {647,243,1}, {648,244,1}, {649,245,1}, {650,246,1}, {651,247,1}, {652,248,1},
    {658,249,1}, {669,250,1}, {670,251,1}, {837,252,1}, {881,253,1}, {883,254,1},
    {887,255,1}, {891,256,1}, {892,257,1}, {893,258,1}, {912,259,3}, {940,262,1},
    {941,263,1}, {942,264,1}, {943,265,1}, {944,266,3}, {945,269,1}, {946,270,1},
    {947,271,1}, {948,272,1}, {949,273,1}, {950,274,1}, {951,275,1}, {952,276,1},
    {953,277,1}, {954,278,1}, {955,279,1}, {956,280,1}, {957,281,1}, {958,282,1},
    {959,283,1}, {960,284,1}, {961,285,1}, {962,286,1}, {963,287,1}, {964,288,1},
    {965,289,1}, {966,290,1}, {967,291,1}, {968,292,1}, {969,293,1}, {970,294,1},
    {971,295,1}, {972,296,1}, {973,297,1}, {974,298,1}, {976,299,1}, {977,300,1},
    {981,301,1}, {982,302,1}, {983,303,1}, {985,304,1}, {987,305,1}, {989,306,1},
    {991,307,1}, {993,308,1}, {995,309,1}, {997,310,1}, {999,311,1}, {1001,312,1},
    {1003,313,1}, {1005,314,1}, {1007,315,1}, {1008,316,1}, {1009,317,1}, {1010,318,1},
    {1011,319,1}, {1013,320,1}, {1016,321,1}, {1019,322,1}, {1072,323,1}, {1073,324,1},
    {1074,325,1}, {1075,326,1}, {1076,327,1}, {1077,328,1}, {1078,329,1}, {1079,330,1},
    {1080,331,1}, {1081,332,1}, {1082,333,1}, {1083,334,1}, {1084,335,1}, {1085,336,1},
    {1086,337,1}, {1087,338,1}, {1088,339,1}, {1089,340,1}, {1090,341,1}, {1091,342,1},
    {1092,343,1}, {1093,344,1}, {1094,345,1}, {1095,346,1}, {1096,347,1}, {1097,348,1},
    {1098,349,1}, {1099,350,1}, {1100,351,1}, {1101,352,1}, {1102,353,1}, {1103,354,1},
    {1104,355,1}, {1105,356,1}, {1106,357,1}, {1107,358,1}, {1108,359,1}, {1109,360,1},
    {1110,361,1}, {1111,362,1}, {1112,363,1}, {1113,364,1}, {1114,365,1}, {1115,366,1},
    {1116,367,1}, {1117,368,1}, {1118,369,1}, {1119,370,1}, {1121,371,1}, {1123,372,1},
    {1125,373,1}, {1127,374,1}, {1129,375,1}, {1131,376,1}, {1133,377,1}, {1135,378,1},
    {1137,379,1}, {1139,380,1}, {1141,381,1}, {1143,382,1}, {1145,383,1}, {1147,384,1},
    {1149,385,1}, {1151,386,1}, {1153,387,1}, {1163,388,1}, {1165,389,1}, {1167,390,1},
    {1169,391,1}, {1171,392,1}, {1173,393,1}, {1175,394,1}, {1177,395,1}, {1179,396,1},
    {1181,397,1}, {1183,398,1}, {1185,399,1}, {1187,400,1}, {1189,401,1}, {1191,402,1},
    {1193,403,1}, {1195,404,1}, {1197,405,1}, {1199,406,1}, {1201,407,1}, {1203,408,1},
    {1205,409,1}, {1207,410,1}, {1209,411,1}, {1211,412,1}, {1213,413,1}, {1215,414,1},
    {1218,415,1}, {1220,416,1}, {1222,417,1}, {1224,418,1}, {1226,419,1}, {1228,420,1},
    {1230,421,1}, {1231,422,1}, {1233,423,1}, {1235,424,1}, {1237,425,1}, {1239,426,1},
    {1241,427,1}, {1243,428,1}, {1245,429,1}, {1247,430,1}, {1249,431,1}, {1251,432,1},
    {1253,433,1}, {1255,434,1}, {1257,435,1}, {1259,436,1}, {1261,437,1}, {1263,438,1},
    {1265,439,1}, {1267,440,1}, {1269,441,1}, {1271,442,1}, {1273,443,1}, {1275,444,1},
    {1277,445,1}, {1279,446,1}, {1281,447,1}, {1283,448,1}, {1285,449,1}, {1287,450,1},
    {1289,451,1}, {1291,452,1}, {1293,453,1}, {1295,454,1}, {1297,455,1}, {1299,456,1},
    {1301,457,1}, {1303,458,1}, {1305,459,1}, {1307,460,1}, {1309,461,1}, {1311,462,1},
    {1313,463,1}, {1315,464,1}, {1317,465,1}, {1319,466,1}, {1321,467,1}, {1323,468,1},
    {1325,469,1}, {1327,470,1}, {1377,471,1}, {1378,472,1}, {1379,473,1}, {1380,474,1},
    {1381,475,1}, {1382,476,1}, {1383,477,1}, {1384,478,1}, {1385,479,1}, {1386,480,1},
    {1387,481,1}, {1388,482,1}, {1389,483,1}, {1390,484,1}, {1391,485,1}, {1392,486,1},
    {1393,487,1}, {1394,488,1}, {1395,489,1}, {1396,490,1}, {1397,491,1}, {1398,492,1},
    {1399,493,1}, {1400,494,1}, {1401,495,1}, {1402,496,1}, {1403,497,1}, {1404,498,1},
    {1405,499,1}, {1406,500,1}, {1407,501,1}, {1408,502,1}, {1409,503,1}, {1410,504,1},
    {1411,505,1}, {1412,506,1}, {1413,507,1}, {1414,508,1}, {1415,509,2}, {4304,511,1},
    {4305,512,1}, {4306,513,1}, {4307,514,1}, {4308,515,1}, {4309,516,1}, {4310,517,1},
    {4311,518,1}, {4312,519,1}, {4313,520,1}, {4314,521,1}, {4315,522,1}, {4316,523,1},
    {4317,524,1}, {4318,525,1}, {4319,526,1}, {4320,527,1}, {4321,528,1}, {4322,529,1},
    {4323,530,1}, {4324,531,1}, {4325,532,1}, {4326,533,1}, {4327,534,1}, {4328,535,1},
    {4329,536,1}, {4330,537,1}, {4331,538,1}, {4332,539,1}, {4333,540,1}, {4334,541,1},
    {4335,542,1}, {4336,543,1}, {4337,544,1}, {4338,545,1}, {4339,546,1}, {4340,547,1},
    {4341,548,1}, {4342,549,1}, {4343,550,1}, {4344,551,1}, {4345,552,1}, {4346,553,1},
    {4349,554,1}, {4350,555,1}, {4351,556,1}, {5112,557,1}, {5113,558,1}, {5114,559,1},
    {5115,560,1}, {5116,561,1}, {5117,562,1}, {7296,563,1}, {7297,564,1}, {7298,565,1},
    {7299,566,1}, {7300,567,1}, {7301,568,1}, {7302,569,1}, {7303,570,1}, {7304,571,1},
    {7306,572,1}, {7545,573,1}, {7549,574,1}, {7566,575,1}, {7681,576,1}, {7683,577,1},
    {7685,578,1}, {7687,579,1}, {7689,580,1}, {7691,581,1}, {7693,582,1}, {7695,583,1},
    {7697,584,1}, {7699,585,1}, {7701,586,1}, {7703,587,1}, {7705,588,1}, {7707,589,1},
    {7709,590,1}, {7711,591,1}, {7713,592,1}, {7715,593,1}, {7717,594,1}, {7719,595,1},
    {7721,596,1}, {7723,597,1}, {7725,598,1}, {7727,599,1}, {7729,600,1}, {7731,601,1},
    {7733,602,1}, {7735,603,1}, {7737,604,1}, {7739,605,1}, {7741,606,1}, {7743,607,1},
    {7745,608,1}, {7747,609,1}, {7749,610,1}, {7751,611,1}, {7753,612,1}, {7755,613,1},
    {7757,614,1}, {7759,615,1}, {7761,616,1}, {7763,617,1}, {7765,618,1}, {7767,619,1},
    {7769,620,1}, {7771,621,1}, {7773,622,1}, {7775,623,1}, {7777,624,1}, {7779,625,1},
    {7781,626,1}, {7783,627,1}, {7785,628,1}, {7787,629,1}, {7789,630,1}, {7791,631,1},
    {7793,632,1}, {7795,633,1}, {7797,634,1}, {7799,635,1}, {7801,636,1}, {7803,637,1},
    {7805,638,1}, {7807,639,1}, {7809,640,1}, {7811,641,1}, {7813,642,1}, {7815,643,1},
    {7817,644,1}, {7819,645,1}, {7821,646,1}, {7823,647,1}, {7825,648,1}, {7827,649,1},
    {7829,650,1}, {7830,651,2}, {7831,653,2}, {7832,655,2}, {7833,657,2}, {7834,659,2},
    {7835,661,1}, {7841,662,1}, {7843,663,1}, {7845,664,1}, {7847,665,1}, {7849,666,1},
    {7851,667,1}, {7853,668,1}, {7855,669,1}, {7857,670,1}, {7859,671,1}, {7861,672,1},
    {7863,673,1}, {7865,674,1}, {7867,675,1}, {7869,676,1}, {7871,677,1}, {7873,678,1},
    {7875,679,1}, {7877,680,1}, {7879,681,1}, {7881,682,1}, {7883,683,1}, {7885,684,1},
    {7887,685,1}, {7889,686,1}, {7891,687,1}, {7893,688,1}, {7895,689,1}, {7897,690,1},
    {7899,691,1}, {7901,692,1}, {7903,693,1}, {7905,694,1}, {7907,695,1}, {7909,696,1},
    {7911,697,1}, {7913,698,1}, {7915,699,1}, {7917,700,1}, {7919,701,1}, {7921,702,1},
    {7923,703,1}, {7925,704,1}, {7927,705,1}, {7929,706,1}, {7931,707,1}, {7933,708,1},
    {7935,709,1}, {7936,710,1}, {7937,711,1}, {7938,712,1}, {7939,713,1}, {7940,714,1},
    {7941,715,1}, {7942,716,1}, {7943,717,1}, {7952,718,1}, {7953,719,1}, {7954,720,1},
    {7955,721,1}, {7956,722,1}, {7957,723,1}, {7968,724,1}, {7969,725,1}, {7970,726,1},
    {7971,727,1}, {7972,728,1}, {7973,729,1}, {7974,730,1}, {7975,731,1}, {7984,732,1},
    {7985,733,1}, {7986,734,1}, {7987,735,1}, {7988,736,1}, {7989,737,1}, {7990,738,1},
    {7991,739,1}, {8000,740,1}, {8001,741,1}, {8002,742,1}, {8003,743,1}, {8004,744,1},
    {8005,745,1}, {8016,746,2}, {8017,748,1}, {8018,749,3}, {8019,752,1}, {8020,753,3},
    {8021,756,1}, {8022,757,3}, {8023,760,1}, {8032,761,1}, {8033,762,1}, {8034,763,1},
    {8035,764,1}, {8036,765,1}, {8037,766,1}, {8038,767,1}, {8039,768,1}, {8048,769,1},
    {8049,770,1}, {8050,771,1}, {8051,772,1}, {8052,773,1}, {8053,774,1}, {8054,775,1},
    {8055,776,1}, {8056,777,1}, {8057,778,1}, {8058,779,1}, {8059,780,1}, {8060,781,1},
    {8061,782,1}, {8064,783,2}, {8065,785,2}, {8066,787,2}, {8067,789,2}, {8068,791,2},
    {8069,793,2}, {8070,795,2}, {8071,797,2}, {8072,799,2}, {8073,801,2}, {8074,803,2},
    {8075,805,2}, {8076,807,2}, {8077,809,2}, {8078,811,2}, {8079,813,2}, {8080,815,2},
    {8081,817,2}, {8082,819,2}, {8083,821,2}, {8084,823,2}, {8085,825,2}, {8086,827,2},
    {8087,829,2}, {8088,831,2}, {8089,833,2}, {8090,835,2}, {8091,837,2}, {8092,839,2},
    {8093,841,2}, {8094,843,2}, {8095,845,2}, {8096,847,2}, {8097,849,2}, {8098,851,2},
    {8099,853,2}, {8100,855,2}, {8101,857,2}, {8102,859,2}, {8103,861,2}, {8104,863,2},
    {8105,865,2}, {8106,867,2}, {8107,869,2}, {8108,871,2}, {8109,873,2}, {8110,875,2},
    {8111,877,2}, {8112,879,1}, {8113,880,1}, {8114,881,2}, {8115,883,2}, {8116,885,2},
    {8118,887,2}, {8119,889,3}, {8124,892,2}, {8126,894,1}, {8130,895,2}, {8131,897,2},
    {8132,899,2}, {8134,901,2}, {8135,903,3}, {8140,906,2}, {8144,908,1}, {8145,909,1},
    {8146,910,3}, {8147,913,3}, {8150,916,2}, {8151,918,3}, {8160,921,1}, {8161,922,1},
    {8162,923,3}, {8163,926,3}, {8164,929,2}, {8165,931,1}, {8166,932,2}, {8167,934,3},
    {8178,937,2}, {8179,939,2}, {8180,941,2}, {8182,943,2}, {8183,945,3}, {8188,948,2},
    {8526,950,1}, {8560,951,1}, {8561,952,1}, {8562,953,1}, {8563,954,1}, {8564,955,1},
    {8565,956,1}, {8566,957,1}, {8567,958,1}, {8568,959,1}, {8569,960,1}, {8570,961,1},
    {8571,962,1}, {8572,963,1}, {8573,964,1}, {8574,965,1}, {8575,966,1}, {8580,967,1},
    {9424,968,1}, {9425,969,1}, {9426,970,1}, {9427,971,1}, {9428,972,1}, {9429,973,1},
    {9430,974,1}, {9431,975,1}, {9432,976,1}, {9433,977,1}, {9434,978,1}, {9435,979,1},
    {9436,980,1}, {9437,981,1}, {9438,982,1}, {9439,983,1}, {9440,984,1}, {9441,985,1},
    {9442,986,1}, {9443,987,1}, {9444,988,1}, {9445,989,1}, {9446,990,1}, {9447,991,1},
    {9448,992,1}, {9449,993,1}, {11312,994,1}, {11313,995,1}, {11314,996,1}, {11315,997,1},
    {11316,998,1}, {11317,999,1}, {11318,1000,1}, {11319,1001,1}, {11320,1002,1}, {11321,1003,1},
    {11322,1004,1}, {11323,1005,1}, {11324,1006,1}, {11325,1007,1}, {11326,1008,1}, {11327,1009,1},
    {11328,1010,1}, {11329,1011,1}, {11330,1012,1}, {11331,1013,1}, {11332,1014,1}, {11333,1015,1},
    {11334,1016,1}, {11335,1017,1}, {11336,1018,1}, {11337,1019,1}, {11338,1020,1}, {11339,1021,1},
    {11340,1022,1}, {11341,1023,1}, {11342,1024,1}, {11343,1025,1}, {11344,1026,1}, {11345,1027,1},
    {11346,1028,1}, {11347,1029,1}, {11348,1030,1}, {11349,1031,1}, {11350,1032,1}, {11351,1033,1},
    {11352,1034,1}, {11353,1035,1}, {11354,1036,1}, {11355,1037,1}, {11356,1038,1}, {11357,1039,1},
    {11358,1040,1}, {11359,1041,1}, {11361,1042,1}, {11365,1043,1}, {11366,1044,1}, {11368,1045,1},
    {11370,1046,1}, {11372,1047,1}, {11379,1048,1}, {11382,1049,1}, {11393,1050,1}, {11395,1051,1},
    {11397,1052,1}, {11399,1053,1}, {11401,1054,1}, {11403,1055,1}, {11405,1056,1}, {11407,1057,1},
    {11409,1058,1}, {11411,1059,1}, {11413,1060,1}, {11415,1061,1}, {11417,1062,1}, {11419,1063,1},
    {11421,1064,1}, {11423,1065,1}, {11425,1066,1}, {11427,1067,1}, {11429,1068,1}, {11431,1069,1},
    {11433,1070,1}, {11435,1071,1}, {11437,1072,1}, {11439,1073,1}, {11441,1074,1}, {11443,1075,1},
    {11445,1076,1}, {11447,1077,1}, {11449,1078,1}, {11451,1079,1}, {11453,1080,1}, {11455,1081,1},
    {11457,1082,1}, {11459,1083,1}, {11461,1084,1}, {11463,1085,1}, {11465,1086,1}, {11467,1087,1},
    {11469,1088,1}, {11471,1089,1}, {11473,1090,1}, {11475,1091,1}, {11477,1092,1}, {11479,1093,1},
    {11481,1094,1}, {11483,1095,1}, {11485,1096,1}, {11487,1097,1}, {11489,1098,1}, {11491,1099,1},
    {11500,1100,1}, {11502,1101,1}, {11507,1102,1}, {11520,1103,1}, {11521,1104,1}, {11522,1105,1},
    {11523,1106,1}, {11524,1107,1}, {11525,1108,1}, {11526,1109,1}, {11527,1110,1}, {11528,1111,1},
    {11529,1112,1}, {11530,1113,1}, {11531,1114,1}, {11532,1115,1}, {11533,1116,1}, {11534,1117,1},
    {11535,1118,1}, {11536,1119,1}, {11537,1120,1}, {11538,1121,1}, {11539,1122,1}, {11540,1123,1},
    {11541,1124,1}, {11542,1125,1}, {11543,1126,1}, {11544,1127,1}, {11545,1128,1}, {11546,1129,1},
    {11547,1130,1}, {11548,1131,1}, {11549,1132,1}, {11550,1133,1}, {11551,1134,1}, {11552,1135,1},
    {11553,1136,1}, {11554,1137,1}, {11555,1138,1}, {11556,1139,1}, {11557,1140,1}, {11559,1141,1},
    {11565,1142,1}, {42561,1143,1}, {42563,1144,1}, {42565,1145,1}, {42567,1146,1}, {42569,1147,1},
    {42571,1148,1}, {42573,1149,1}, {42575,1150,1}, {42577,1151,1}, {42579,1152,1}, {42581,1153,1},
    {42583,1154,1}, {42585,1155,1}, {42587,1156,1}, {42589,1157,1}, {42591,1158,1}, {42593,1159,1},
    {42595,1160,1}, {42597,1161,1}, {42599,1162,1}, {42601,1163,1}, {42603,1164,1}, {42605,1165,1},
    {42625,1166,1}, {42627,1167,1}, {42629,1168,1}, {42631,1169,1}, {42633,1170,1}, {42635,1171,1},
    {42637,1172,1}, {42639,1173,1}, {42641,1174,1}, {42643,1175,1}, {42645,1176,1}, {42647,1177,1},
    {42649,1178,1}, {42651,1179,1}, {42787,1180,1}, {42789,1181,1}, {42791,1182,1}, {42793,1183,1},
    {42795,1184,1}, {42797,1185,1}, {42799,1186,1}, {42803,1187,1}, {42805,1188,1}, {42807,1189,1},
    {42809,1190,1}, {42811,1191,1}, {42813,1192,1}, {42815,1193,1}, {42817,1194,1}, {42819,1195,1},
    {42821,1196,1}, {42823,1197,1}, {42825,1198,1}, {42827,1199,1}, {42829,1200,1}, {42831,1201,1},
    {42833,1202,1}, {42835,1203,1}, {42837,1204,1}, {42839,1205,1}, {42841,1206,1}, {42843,1207,1},
    {42845,1208,1}, {42847,1209,1}, {42849,1210,1}, {42851,1211,1}, {42853,1212,1}, {42855,1213,1},
    {42857,1214,1}, {42859,1215,1}, {42861,1216,1}, {42863,1217,1}, {42874,1218,1}, {42876,1219,1},
    {42879,1220,1}, {42881,1221,1}, {42883,1222,1}, {42885,1223,1}, {42887,1224,1}, {42892,1225,1},
    {42897,1226,1}, {42899,1227,1}, {42900,1228,1}, {42903,1229,1}, {42905,1230,1}, {42907,1231,1},
    {42909,1232,1}, {42911,1233,1}, {42913,1234,1}, {42915,1235,1}, {42917,1236,1}, {42919,1237,1},
    {42921,1238,1}, {42933,1239,1}, {42935,1240,1}, {42937,1241,1}, {42939,1242,1}, {42941,1243,1},
    {42943,1244,1}, {42945,1245,1}, {42947,1246,1}, {42952,1247,1}, {42954,1248,1}, {42957,1249,1},
    {42961,1250,1}, {42967,1251,1}, {42969,1252,1}, {42971,1253,1}, {42998,1254,1}, {43859,1255,1},
    {43888,1256,1}, {43889,1257,1}, {43890,1258,1}, {43891,1259,1}, {43892,1260,1}, {43893,1261,1},
    {43894,1262,1}, {43895,1263,1}, {43896,1264,1}, {43897,1265,1}, {43898,1266,1}, {43899,1267,1},
    {43900,1268,1}, {43901,1269,1}, {43902,1270,1}, {43903,1271,1}, {43904,1272,1}, {43905,1273,1},
    {43906,1274,1}, {43907,1275,1}, {43908,1276,1}, {43909,1277,1}, {43910,1278,1}, {43911,1279,1},
    {43912,1280,1}, {43913,1281,1}, {43914,1282,1}, {43915,1283,1}, {43916,1284,1}, {43917,1285,1},
    {43918,1286,1}, {43919,1287,1}, {43920,1288,1}, {43921,1289,1}, {43922,1290,1}, {43923,1291,1},
    {43924,1292,1}, {43925,1293,1}, {43926,1294,1}, {43927,1295,1}, {43928,1296,1}, {43929,1297,1},
    {43930,1298,1}, {43931,1299,1}, {43932,1300,1}, {43933,1301,1}, {43934,1302,1}, {43935,1303,1},
    {43936,1304,1}, {43937,1305,1}, {43938,1306,1}, {43939,1307,1}, {43940,1308,1}, {43941,1309,1},
    {43942,1310,1}, {43943,1311,1}, {43944,1312,1}, {43945,1313,1}, {43946,1314,1}, {43947,1315,1},
    {43948,1316,1}, {43949,1317,1}, {43950,1318,1}, {43951,1319,1}, {43952,1320,1}, {43953,1321,1},
    {43954,1322,1}, {43955,1323,1}, {43956,1324,1}, {43957,1325,1}, {43958,1326,1}, {43959,1327,1},
    {43960,1328,1}, {43961,1329,1}, {43962,1330,1}, {43963,1331,1}, {43964,1332,1}, {43965,1333,1},
    {43966,1334,1}, {43967,1335,1}, {64256,1336,2}, {64257,1338,2}, {64258,1340,2}, {64259,1342,3},
    {64260,1345,3}, {64261,1348,2}, {64262,1350,2}, {64275,1352,2}, {64276,1354,2}, {64277,1356,2},
    {64278,1358,2}, {64279,1360,2}, {65345,1362,1}, {65346,1363,1}, {65347,1364,1}, {65348,1365,1},
    {65349,1366,1}, {65350,1367,1}, {65351,1368,1}, {65352,1369,1}, {65353,1370,1}, {65354,1371,1},
    {65355,1372,1}, {65356,1373,1}, {65357,1374,1}, {65358,1375,1}, {65359,1376,1}, {65360,1377,1},
    {65361,1378,1}, {65362,1379,1}, {65363,1380,1}, {65364,1381,1}, {65365,1382,1}, {65366,1383,1},
    {65367,1384,1}, {65368,1385,1}, {65369,1386,1}, {65370,1387,1}, {66600,1388,1}, {66601,1389,1},
    {66602,1390,1}, {66603,1391,1}, {66604,1392,1}, {66605,1393,1}, {66606,1394,1}, {66607,1395,1},
    {66608,1396,1}, {66609,1397,1}, {66610,1398,1}, {66611,1399,1}, {66612,1400,1}, {66613,1401,1},
    {66614,1402,1}, {66615,1403,1}, {66616,1404,1}, {66617,1405,1}, {66618,1406,1}, {66619,1407,1},
    {66620,1408,1}, {66621,1409,1}, {66622,1410,1}, {66623,1411,1}, {66624,1412,1}, {66625,1413,1},
    {66626,1414,1}, {66627,1415,1}, {66628,1416,1}, {66629,1417,1}, {66630,1418,1}, {66631,1419,1},
    {66632,1420,1}, {66633,1421,1}, {66634,1422,1}, {66635,1423,1}, {66636,1424,1}, {66637,1425,1},
    {66638,1426,1}, {66639,1427,1}, {66776,1428,1}, {66777,1429,1}, {66778,1430,1}, {66779,1431,1},
    {66780,1432,1}, {66781,1433,1}, {66782,1434,1}, {66783,1435,1}, {66784,1436,1}, {66785,1437,1},
    {66786,1438,1}, {66787,1439,1}, {66788,1440,1}, {66789,1441,1}, {66790,1442,1}, {66791,1443,1},
    {66792,1444,1}, {66793,1445,1}, {66794,1446,1}, {66795,1447,1}, {66796,1448,1}, {66797,1449,1},
    {66798,1450,1}, {66799,1451,1}, {66800,1452,1}, {66801,1453,1}, {66802,1454,1}, {66803,1455,1},
    {66804,1456,1}, {66805,1457,1}, {66806,1458,1}, {66807,1459,1}, {66808,1460,1}, {66809,1461,1},
    {66810,1462,1}, {66811,1463,1}, {66967,1464,1}, {66968,1465,1}, {66969,1466,1}, {66970,1467,1},
    {66971,1468,1}, {66972,1469,1}, {66973,1470,1}, {66974,1471,1}, {66975,1472,1}, {66976,1473,1},
    {66977,1474,1}, {66979,1475,1}, {66980,1476,1}, {66981,1477,1}, {66982,1478,1}, {66983,1479,1},
    {66984,1480,1}, {66985,1481,1}, {66986,1482,1}, {66987,1483,1}, {66988,1484,1}, {66989,1485,1},
    {66990,1486,1}, {66991,1487,1}, {66992,1488,1}, {66993,1489,1}, {66995,1490,1}, {66996,1491,1},
    {66997,1492,1}, {66998,1493,1}, {66999,1494,1}, {67000,1495,1}, {67001,1496,1}, {67003,1497,1},
    {67004,1498,1}, {68800,1499,1}, {68801,1500,1}, {68802,1501,1}, {68803,1502,1}, {68804,1503,1},
    {68805,1504,1}, {68806,1505,1}, {68807,1506,1}, {68808,1507,1}, {68809,1508,1}, {68810,1509,1},
    {68811,1510,1}, {68812,1511,1}, {68813,1512,1}, {68814,1513,1}, {68815,1514,1}, {68816,1515,1},
    {68817,1516,1}, {68818,1517,1}, {68819,1518,1}, {68820,1519,1}, {68821,1520,1}, {68822,1521,1},
    {68823,1522,1}, {68824,1523,1}, {68825,1524,1}, {68826,1525,1}, {68827,1526,1}, {68828,1527,1},
    {68829,1528,1}, {68830,1529,1}, {68831,1530,1}, {68832,1531,1}, {68833,1532,1}, {68834,1533,1},
    {68835,1534,1}, {68836,1535,1}, {68837,1536,1}, {68838,1537,1}, {68839,1538,1}, {68840,1539,1},
    {68841,1540,1}, {68842,1541,1}, {68843,1542,1}, {68844,1543,1}, {68845,1544,1}, {68846,1545,1},
    {68847,1546,1}, {68848,1547,1}, {68849,1548,1}, {68850,1549,1}, {68976,1550,1}, {68977,1551,1},
    {68978,1552,1}, {68979,1553,1}, {68980,1554,1}, {68981,1555,1}, {68982,1556,1}, {68983,1557,1},
    {68984,1558,1}, {68985,1559,1}, {68986,1560,1}, {68987,1561,1}, {68988,1562,1}, {68989,1563,1},
    {68990,1564,1}, {68991,1565,1}, {68992,1566,1}, {68993,1567,1}, {68994,1568,1}, {68995,1569,1},
    {68996,1570,1}, {68997,1571,1}, {71872,1572,1}, {71873,1573,1}, {71874,1574,1}, {71875,1575,1},
    {71876,1576,1}, {71877,1577,1}, {71878,1578,1}, {71879,1579,1}, {71880,1580,1}, {71881,1581,1},
    {71882,1582,1}, {71883,1583,1}, {71884,1584,1}, {71885,1585,1}, {71886,1586,1}, {71887,1587,1},
    {71888,1588,1}, {71889,1589,1}, {71890,1590,1}, {71891,1591,1}, {71892,1592,1}, {71893,1593,1},
    {71894,1594,1}, {71895,1595,1}, {71896,1596,1}, {71897,1597,1}, {71898,1598,1}, {71899,1599,1},
    {71900,1600,1}, {71901,1601,1}, {71902,1602,1}, {71903,1603,1}, {93792,1604,1}, {93793,1605,1},
    {93794,1606,1}, {93795,1607,1}, {93796,1608,1}, {93797,1609,1}, {93798,1610,1}, {93799,1611,1},
    {93800,1612,1}, {93801,1613,1}, {93802,1614,1}, {93803,1615,1}, {93804,1616,1}, {93805,1617,1},
    {93806,1618,1}, {93807,1619,1}, {93808,1620,1}, {93809,1621,1}, {93810,1622,1}, {93811,1623,1},
    {93812,1624,1}, {93813,1625,1}, {93814,1626,1}, {93815,1627,1}, {93816,1628,1}, {93817,1629,1},
    {93818,1630,1}, {93819,1631,1}, {93820,1632,1}, {93821,1633,1}, {93822,1634,1}, {93823,1635,1},
    {125218,1636,1}, {125219,1637,1}, {125220,1638,1}, {125221,1639,1}, {125222,1640,1}, {125223,1641,1},
    {125224,1642,1}, {125225,1643,1}, {125226,1644,1}, {125227,1645,1}, {125228,1646,1}, {125229,1647,1},
    {125230,1648,1}, {125231,1649,1}, {125232,1650,1}, {125233,1651,1}, {125234,1652,1}, {125235,1653,1},
    {125236,1654,1}, {125237,1655,1}, {125238,1656,1}, {125239,1657,1}, {125240,1658,1}, {125241,1659,1},
    {125242,1660,1}, {125243,1661,1}, {125244,1662,1}, {125245,1663,1}, {125246,1664,1}, {125247,1665,1},
    {125248,1666,1}, {125249,1667,1}, {125250,1668,1}, {125251,1669,1},
};
static const uint32_t tinox_unicode_lower_pool[] = {
    97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108,
    109, 110, 111, 112, 113, 114, 115, 116, 117, 118, 119, 120,
    121, 122, 224, 225, 226, 227, 228, 229, 230, 231, 232, 233,
    234, 235, 236, 237, 238, 239, 240, 241, 242, 243, 244, 245,
    246, 248, 249, 250, 251, 252, 253, 254, 257, 259, 261, 263,
    265, 267, 269, 271, 273, 275, 277, 279, 281, 283, 285, 287,
    289, 291, 293, 295, 297, 299, 301, 303, 105, 775, 307, 309,
    311, 314, 316, 318, 320, 322, 324, 326, 328, 331, 333, 335,
    337, 339, 341, 343, 345, 347, 349, 351, 353, 355, 357, 359,
    361, 363, 365, 367, 369, 371, 373, 375, 255, 378, 380, 382,
    595, 387, 389, 596, 392, 598, 599, 396, 477, 601, 603, 402,
    608, 611, 617, 616, 409, 623, 626, 629, 417, 419, 421, 640,
    424, 643, 429, 648, 432, 650, 651, 436, 438, 658, 441, 445,
    454, 454, 457, 457, 460, 460, 462, 464, 466, 468, 470, 472,
    474, 476, 479, 481, 483, 485, 487, 489, 491, 493, 495, 499,
    499, 501, 405, 447, 505, 507, 509, 511, 513, 515, 517, 519,
    521, 523, 525, 527, 529, 531, 533, 535, 537, 539, 541, 543,
    414, 547, 549, 551, 553, 555, 557, 559, 561, 563, 11365, 572,
    410, 11366, 578, 384, 649, 652, 583, 585, 587, 589, 591, 881,
    883, 887, 1011, 940, 941, 942, 943, 972, 973, 974, 945, 946,
    947, 948, 949, 950, 951, 952, 953, 954, 955, 956, 957, 958,
    959, 960, 961, 963, 964, 965, 966, 967, 968, 969, 970, 971,
    983, 985, 987, 989, 991, 993, 995, 997, 999, 1001, 1003, 1005,
    1007, 952, 1016, 1010, 1019, 891, 892, 893, 1104, 1105, 1106, 1107,
    1108, 1109, 1110, 1111, 1112, 1113, 1114, 1115, 1116, 1117, 1118, 1119,
    1072, 1073, 1074, 1075, 1076, 1077, 1078, 1079, 1080, 1081, 1082, 1083,
    1084, 1085, 1086, 1087, 1088, 1089, 1090, 1091, 1092, 1093, 1094, 1095,
    1096, 1097, 1098, 1099, 1100, 1101, 1102, 1103, 1121, 1123, 1125, 1127,
    1129, 1131, 1133, 1135, 1137, 1139, 1141, 1143, 1145, 1147, 1149, 1151,
    1153, 1163, 1165, 1167, 1169, 1171, 1173, 1175, 1177, 1179, 1181, 1183,
    1185, 1187, 1189, 1191, 1193, 1195, 1197, 1199, 1201, 1203, 1205, 1207,
    1209, 1211, 1213, 1215, 1231, 1218, 1220, 1222, 1224, 1226, 1228, 1230,
    1233, 1235, 1237, 1239, 1241, 1243, 1245, 1247, 1249, 1251, 1253, 1255,
    1257, 1259, 1261, 1263, 1265, 1267, 1269, 1271, 1273, 1275, 1277, 1279,
    1281, 1283, 1285, 1287, 1289, 1291, 1293, 1295, 1297, 1299, 1301, 1303,
    1305, 1307, 1309, 1311, 1313, 1315, 1317, 1319, 1321, 1323, 1325, 1327,
    1377, 1378, 1379, 1380, 1381, 1382, 1383, 1384, 1385, 1386, 1387, 1388,
    1389, 1390, 1391, 1392, 1393, 1394, 1395, 1396, 1397, 1398, 1399, 1400,
    1401, 1402, 1403, 1404, 1405, 1406, 1407, 1408, 1409, 1410, 1411, 1412,
    1413, 1414, 11520, 11521, 11522, 11523, 11524, 11525, 11526, 11527, 11528, 11529,
    11530, 11531, 11532, 11533, 11534, 11535, 11536, 11537, 11538, 11539, 11540, 11541,
    11542, 11543, 11544, 11545, 11546, 11547, 11548, 11549, 11550, 11551, 11552, 11553,
    11554, 11555, 11556, 11557, 11559, 11565, 43888, 43889, 43890, 43891, 43892, 43893,
    43894, 43895, 43896, 43897, 43898, 43899, 43900, 43901, 43902, 43903, 43904, 43905,
    43906, 43907, 43908, 43909, 43910, 43911, 43912, 43913, 43914, 43915, 43916, 43917,
    43918, 43919, 43920, 43921, 43922, 43923, 43924, 43925, 43926, 43927, 43928, 43929,
    43930, 43931, 43932, 43933, 43934, 43935, 43936, 43937, 43938, 43939, 43940, 43941,
    43942, 43943, 43944, 43945, 43946, 43947, 43948, 43949, 43950, 43951, 43952, 43953,
    43954, 43955, 43956, 43957, 43958, 43959, 43960, 43961, 43962, 43963, 43964, 43965,
    43966, 43967, 5112, 5113, 5114, 5115, 5116, 5117, 7306, 4304, 4305, 4306,
    4307, 4308, 4309, 4310, 4311, 4312, 4313, 4314, 4315, 4316, 4317, 4318,
    4319, 4320, 4321, 4322, 4323, 4324, 4325, 4326, 4327, 4328, 4329, 4330,
    4331, 4332, 4333, 4334, 4335, 4336, 4337, 4338, 4339, 4340, 4341, 4342,
    4343, 4344, 4345, 4346, 4349, 4350, 4351, 7681, 7683, 7685, 7687, 7689,
    7691, 7693, 7695, 7697, 7699, 7701, 7703, 7705, 7707, 7709, 7711, 7713,
    7715, 7717, 7719, 7721, 7723, 7725, 7727, 7729, 7731, 7733, 7735, 7737,
    7739, 7741, 7743, 7745, 7747, 7749, 7751, 7753, 7755, 7757, 7759, 7761,
    7763, 7765, 7767, 7769, 7771, 7773, 7775, 7777, 7779, 7781, 7783, 7785,
    7787, 7789, 7791, 7793, 7795, 7797, 7799, 7801, 7803, 7805, 7807, 7809,
    7811, 7813, 7815, 7817, 7819, 7821, 7823, 7825, 7827, 7829, 223, 7841,
    7843, 7845, 7847, 7849, 7851, 7853, 7855, 7857, 7859, 7861, 7863, 7865,
    7867, 7869, 7871, 7873, 7875, 7877, 7879, 7881, 7883, 7885, 7887, 7889,
    7891, 7893, 7895, 7897, 7899, 7901, 7903, 7905, 7907, 7909, 7911, 7913,
    7915, 7917, 7919, 7921, 7923, 7925, 7927, 7929, 7931, 7933, 7935, 7936,
    7937, 7938, 7939, 7940, 7941, 7942, 7943, 7952, 7953, 7954, 7955, 7956,
    7957, 7968, 7969, 7970, 7971, 7972, 7973, 7974, 7975, 7984, 7985, 7986,
    7987, 7988, 7989, 7990, 7991, 8000, 8001, 8002, 8003, 8004, 8005, 8017,
    8019, 8021, 8023, 8032, 8033, 8034, 8035, 8036, 8037, 8038, 8039, 8064,
    8065, 8066, 8067, 8068, 8069, 8070, 8071, 8080, 8081, 8082, 8083, 8084,
    8085, 8086, 8087, 8096, 8097, 8098, 8099, 8100, 8101, 8102, 8103, 8112,
    8113, 8048, 8049, 8115, 8050, 8051, 8052, 8053, 8131, 8144, 8145, 8054,
    8055, 8160, 8161, 8058, 8059, 8165, 8056, 8057, 8060, 8061, 8179, 969,
    107, 229, 8526, 8560, 8561, 8562, 8563, 8564, 8565, 8566, 8567, 8568,
    8569, 8570, 8571, 8572, 8573, 8574, 8575, 8580, 9424, 9425, 9426, 9427,
    9428, 9429, 9430, 9431, 9432, 9433, 9434, 9435, 9436, 9437, 9438, 9439,
    9440, 9441, 9442, 9443, 9444, 9445, 9446, 9447, 9448, 9449, 11312, 11313,
    11314, 11315, 11316, 11317, 11318, 11319, 11320, 11321, 11322, 11323, 11324, 11325,
    11326, 11327, 11328, 11329, 11330, 11331, 11332, 11333, 11334, 11335, 11336, 11337,
    11338, 11339, 11340, 11341, 11342, 11343, 11344, 11345, 11346, 11347, 11348, 11349,
    11350, 11351, 11352, 11353, 11354, 11355, 11356, 11357, 11358, 11359, 11361, 619,
    7549, 637, 11368, 11370, 11372, 593, 625, 592, 594, 11379, 11382, 575,
    576, 11393, 11395, 11397, 11399, 11401, 11403, 11405, 11407, 11409, 11411, 11413,
    11415, 11417, 11419, 11421, 11423, 11425, 11427, 11429, 11431, 11433, 11435, 11437,
    11439, 11441, 11443, 11445, 11447, 11449, 11451, 11453, 11455, 11457, 11459, 11461,
    11463, 11465, 11467, 11469, 11471, 11473, 11475, 11477, 11479, 11481, 11483, 11485,
    11487, 11489, 11491, 11500, 11502, 11507, 42561, 42563, 42565, 42567, 42569, 42571,
    42573, 42575, 42577, 42579, 42581, 42583, 42585, 42587, 42589, 42591, 42593, 42595,
    42597, 42599, 42601, 42603, 42605, 42625, 42627, 42629, 42631, 42633, 42635, 42637,
    42639, 42641, 42643, 42645, 42647, 42649, 42651, 42787, 42789, 42791, 42793, 42795,
    42797, 42799, 42803, 42805, 42807, 42809, 42811, 42813, 42815, 42817, 42819, 42821,
    42823, 42825, 42827, 42829, 42831, 42833, 42835, 42837, 42839, 42841, 42843, 42845,
    42847, 42849, 42851, 42853, 42855, 42857, 42859, 42861, 42863, 42874, 42876, 7545,
    42879, 42881, 42883, 42885, 42887, 42892, 613, 42897, 42899, 42903, 42905, 42907,
    42909, 42911, 42913, 42915, 42917, 42919, 42921, 614, 604, 609, 620, 618,
    670, 647, 669, 43859, 42933, 42935, 42937, 42939, 42941, 42943, 42945, 42947,
    42900, 642, 7566, 42952, 42954, 612, 42957, 42961, 42967, 42969, 42971, 411,
    42998, 65345, 65346, 65347, 65348, 65349, 65350, 65351, 65352, 65353, 65354, 65355,
    65356, 65357, 65358, 65359, 65360, 65361, 65362, 65363, 65364, 65365, 65366, 65367,
    65368, 65369, 65370, 66600, 66601, 66602, 66603, 66604, 66605, 66606, 66607, 66608,
    66609, 66610, 66611, 66612, 66613, 66614, 66615, 66616, 66617, 66618, 66619, 66620,
    66621, 66622, 66623, 66624, 66625, 66626, 66627, 66628, 66629, 66630, 66631, 66632,
    66633, 66634, 66635, 66636, 66637, 66638, 66639, 66776, 66777, 66778, 66779, 66780,
    66781, 66782, 66783, 66784, 66785, 66786, 66787, 66788, 66789, 66790, 66791, 66792,
    66793, 66794, 66795, 66796, 66797, 66798, 66799, 66800, 66801, 66802, 66803, 66804,
    66805, 66806, 66807, 66808, 66809, 66810, 66811, 66967, 66968, 66969, 66970, 66971,
    66972, 66973, 66974, 66975, 66976, 66977, 66979, 66980, 66981, 66982, 66983, 66984,
    66985, 66986, 66987, 66988, 66989, 66990, 66991, 66992, 66993, 66995, 66996, 66997,
    66998, 66999, 67000, 67001, 67003, 67004, 68800, 68801, 68802, 68803, 68804, 68805,
    68806, 68807, 68808, 68809, 68810, 68811, 68812, 68813, 68814, 68815, 68816, 68817,
    68818, 68819, 68820, 68821, 68822, 68823, 68824, 68825, 68826, 68827, 68828, 68829,
    68830, 68831, 68832, 68833, 68834, 68835, 68836, 68837, 68838, 68839, 68840, 68841,
    68842, 68843, 68844, 68845, 68846, 68847, 68848, 68849, 68850, 68976, 68977, 68978,
    68979, 68980, 68981, 68982, 68983, 68984, 68985, 68986, 68987, 68988, 68989, 68990,
    68991, 68992, 68993, 68994, 68995, 68996, 68997, 71872, 71873, 71874, 71875, 71876,
    71877, 71878, 71879, 71880, 71881, 71882, 71883, 71884, 71885, 71886, 71887, 71888,
    71889, 71890, 71891, 71892, 71893, 71894, 71895, 71896, 71897, 71898, 71899, 71900,
    71901, 71902, 71903, 93792, 93793, 93794, 93795, 93796, 93797, 93798, 93799, 93800,
    93801, 93802, 93803, 93804, 93805, 93806, 93807, 93808, 93809, 93810, 93811, 93812,
    93813, 93814, 93815, 93816, 93817, 93818, 93819, 93820, 93821, 93822, 93823, 125218,
    125219, 125220, 125221, 125222, 125223, 125224, 125225, 125226, 125227, 125228, 125229, 125230,
    125231, 125232, 125233, 125234, 125235, 125236, 125237, 125238, 125239, 125240, 125241, 125242,
    125243, 125244, 125245, 125246, 125247, 125248, 125249, 125250, 125251,
};
static const struct { uint32_t cp; uint16_t offset; uint8_t len; } tinox_unicode_lower_map[] = {
    {65,0,1}, {66,1,1}, {67,2,1}, {68,3,1}, {69,4,1}, {70,5,1},
    {71,6,1}, {72,7,1}, {73,8,1}, {74,9,1}, {75,10,1}, {76,11,1},
    {77,12,1}, {78,13,1}, {79,14,1}, {80,15,1}, {81,16,1}, {82,17,1},
    {83,18,1}, {84,19,1}, {85,20,1}, {86,21,1}, {87,22,1}, {88,23,1},
    {89,24,1}, {90,25,1}, {192,26,1}, {193,27,1}, {194,28,1}, {195,29,1},
    {196,30,1}, {197,31,1}, {198,32,1}, {199,33,1}, {200,34,1}, {201,35,1},
    {202,36,1}, {203,37,1}, {204,38,1}, {205,39,1}, {206,40,1}, {207,41,1},
    {208,42,1}, {209,43,1}, {210,44,1}, {211,45,1}, {212,46,1}, {213,47,1},
    {214,48,1}, {216,49,1}, {217,50,1}, {218,51,1}, {219,52,1}, {220,53,1},
    {221,54,1}, {222,55,1}, {256,56,1}, {258,57,1}, {260,58,1}, {262,59,1},
    {264,60,1}, {266,61,1}, {268,62,1}, {270,63,1}, {272,64,1}, {274,65,1},
    {276,66,1}, {278,67,1}, {280,68,1}, {282,69,1}, {284,70,1}, {286,71,1},
    {288,72,1}, {290,73,1}, {292,74,1}, {294,75,1}, {296,76,1}, {298,77,1},
    {300,78,1}, {302,79,1}, {304,80,2}, {306,82,1}, {308,83,1}, {310,84,1},
    {313,85,1}, {315,86,1}, {317,87,1}, {319,88,1}, {321,89,1}, {323,90,1},
    {325,91,1}, {327,92,1}, {330,93,1}, {332,94,1}, {334,95,1}, {336,96,1},
    {338,97,1}, {340,98,1}, {342,99,1}, {344,100,1}, {346,101,1}, {348,102,1},
    {350,103,1}, {352,104,1}, {354,105,1}, {356,106,1}, {358,107,1}, {360,108,1},
    {362,109,1}, {364,110,1}, {366,111,1}, {368,112,1}, {370,113,1}, {372,114,1},
    {374,115,1}, {376,116,1}, {377,117,1}, {379,118,1}, {381,119,1}, {385,120,1},
    {386,121,1}, {388,122,1}, {390,123,1}, {391,124,1}, {393,125,1}, {394,126,1},
    {395,127,1}, {398,128,1}, {399,129,1}, {400,130,1}, {401,131,1}, {403,132,1},
    {404,133,1}, {406,134,1}, {407,135,1}, {408,136,1}, {412,137,1}, {413,138,1},
    {415,139,1}, {416,140,1}, {418,141,1}, {420,142,1}, {422,143,1}, {423,144,1},
    {425,145,1}, {428,146,1}, {430,147,1}, {431,148,1}, {433,149,1}, {434,150,1},
    {435,151,1}, {437,152,1}, {439,153,1}, {440,154,1}, {444,155,1}, {452,156,1},
    {453,157,1}, {455,158,1}, {456,159,1}, {458,160,1}, {459,161,1}, {461,162,1},
    {463,163,1}, {465,164,1}, {467,165,1}, {469,166,1}, {471,167,1}, {473,168,1},
    {475,169,1}, {478,170,1}, {480,171,1}, {482,172,1}, {484,173,1}, {486,174,1},
    {488,175,1}, {490,176,1}, {492,177,1}, {494,178,1}, {497,179,1}, {498,180,1},
    {500,181,1}, {502,182,1}, {503,183,1}, {504,184,1}, {506,185,1}, {508,186,1},
    {510,187,1}, {512,188,1}, {514,189,1}, {516,190,1}, {518,191,1}, {520,192,1},
    {522,193,1}, {524,194,1}, {526,195,1}, {528,196,1}, {530,197,1}, {532,198,1},
    {534,199,1}, {536,200,1}, {538,201,1}, {540,202,1}, {542,203,1}, {544,204,1},
    {546,205,1}, {548,206,1}, {550,207,1}, {552,208,1}, {554,209,1}, {556,210,1},
    {558,211,1}, {560,212,1}, {562,213,1}, {570,214,1}, {571,215,1}, {573,216,1},
    {574,217,1}, {577,218,1}, {579,219,1}, {580,220,1}, {581,221,1}, {582,222,1},
    {584,223,1}, {586,224,1}, {588,225,1}, {590,226,1}, {880,227,1}, {882,228,1},
    {886,229,1}, {895,230,1}, {902,231,1}, {904,232,1}, {905,233,1}, {906,234,1},
    {908,235,1}, {910,236,1}, {911,237,1}, {913,238,1}, {914,239,1}, {915,240,1},
    {916,241,1}, {917,242,1}, {918,243,1}, {919,244,1}, {920,245,1}, {921,246,1},
    {922,247,1}, {923,248,1}, {924,249,1}, {925,250,1}, {926,251,1}, {927,252,1},
    {928,253,1}, {929,254,1}, {931,255,1}, {932,256,1}, {933,257,1}, {934,258,1},
    {935,259,1}, {936,260,1}, {937,261,1}, {938,262,1}, {939,263,1}, {975,264,1},
    {984,265,1}, {986,266,1}, {988,267,1}, {990,268,1}, {992,269,1}, {994,270,1},
    {996,271,1}, {998,272,1}, {1000,273,1}, {1002,274,1}, {1004,275,1}, {1006,276,1},
    {1012,277,1}, {1015,278,1}, {1017,279,1}, {1018,280,1}, {1021,281,1}, {1022,282,1},
    {1023,283,1}, {1024,284,1}, {1025,285,1}, {1026,286,1}, {1027,287,1}, {1028,288,1},
    {1029,289,1}, {1030,290,1}, {1031,291,1}, {1032,292,1}, {1033,293,1}, {1034,294,1},
    {1035,295,1}, {1036,296,1}, {1037,297,1}, {1038,298,1}, {1039,299,1}, {1040,300,1},
    {1041,301,1}, {1042,302,1}, {1043,303,1}, {1044,304,1}, {1045,305,1}, {1046,306,1},
    {1047,307,1}, {1048,308,1}, {1049,309,1}, {1050,310,1}, {1051,311,1}, {1052,312,1},
    {1053,313,1}, {1054,314,1}, {1055,315,1}, {1056,316,1}, {1057,317,1}, {1058,318,1},
    {1059,319,1}, {1060,320,1}, {1061,321,1}, {1062,322,1}, {1063,323,1}, {1064,324,1},
    {1065,325,1}, {1066,326,1}, {1067,327,1}, {1068,328,1}, {1069,329,1}, {1070,330,1},
    {1071,331,1}, {1120,332,1}, {1122,333,1}, {1124,334,1}, {1126,335,1}, {1128,336,1},
    {1130,337,1}, {1132,338,1}, {1134,339,1}, {1136,340,1}, {1138,341,1}, {1140,342,1},
    {1142,343,1}, {1144,344,1}, {1146,345,1}, {1148,346,1}, {1150,347,1}, {1152,348,1},
    {1162,349,1}, {1164,350,1}, {1166,351,1}, {1168,352,1}, {1170,353,1}, {1172,354,1},
    {1174,355,1}, {1176,356,1}, {1178,357,1}, {1180,358,1}, {1182,359,1}, {1184,360,1},
    {1186,361,1}, {1188,362,1}, {1190,363,1}, {1192,364,1}, {1194,365,1}, {1196,366,1},
    {1198,367,1}, {1200,368,1}, {1202,369,1}, {1204,370,1}, {1206,371,1}, {1208,372,1},
    {1210,373,1}, {1212,374,1}, {1214,375,1}, {1216,376,1}, {1217,377,1}, {1219,378,1},
    {1221,379,1}, {1223,380,1}, {1225,381,1}, {1227,382,1}, {1229,383,1}, {1232,384,1},
    {1234,385,1}, {1236,386,1}, {1238,387,1}, {1240,388,1}, {1242,389,1}, {1244,390,1},
    {1246,391,1}, {1248,392,1}, {1250,393,1}, {1252,394,1}, {1254,395,1}, {1256,396,1},
    {1258,397,1}, {1260,398,1}, {1262,399,1}, {1264,400,1}, {1266,401,1}, {1268,402,1},
    {1270,403,1}, {1272,404,1}, {1274,405,1}, {1276,406,1}, {1278,407,1}, {1280,408,1},
    {1282,409,1}, {1284,410,1}, {1286,411,1}, {1288,412,1}, {1290,413,1}, {1292,414,1},
    {1294,415,1}, {1296,416,1}, {1298,417,1}, {1300,418,1}, {1302,419,1}, {1304,420,1},
    {1306,421,1}, {1308,422,1}, {1310,423,1}, {1312,424,1}, {1314,425,1}, {1316,426,1},
    {1318,427,1}, {1320,428,1}, {1322,429,1}, {1324,430,1}, {1326,431,1}, {1329,432,1},
    {1330,433,1}, {1331,434,1}, {1332,435,1}, {1333,436,1}, {1334,437,1}, {1335,438,1},
    {1336,439,1}, {1337,440,1}, {1338,441,1}, {1339,442,1}, {1340,443,1}, {1341,444,1},
    {1342,445,1}, {1343,446,1}, {1344,447,1}, {1345,448,1}, {1346,449,1}, {1347,450,1},
    {1348,451,1}, {1349,452,1}, {1350,453,1}, {1351,454,1}, {1352,455,1}, {1353,456,1},
    {1354,457,1}, {1355,458,1}, {1356,459,1}, {1357,460,1}, {1358,461,1}, {1359,462,1},
    {1360,463,1}, {1361,464,1}, {1362,465,1}, {1363,466,1}, {1364,467,1}, {1365,468,1},
    {1366,469,1}, {4256,470,1}, {4257,471,1}, {4258,472,1}, {4259,473,1}, {4260,474,1},
    {4261,475,1}, {4262,476,1}, {4263,477,1}, {4264,478,1}, {4265,479,1}, {4266,480,1},
    {4267,481,1}, {4268,482,1}, {4269,483,1}, {4270,484,1}, {4271,485,1}, {4272,486,1},
    {4273,487,1}, {4274,488,1}, {4275,489,1}, {4276,490,1}, {4277,491,1}, {4278,492,1},
    {4279,493,1}, {4280,494,1}, {4281,495,1}, {4282,496,1}, {4283,497,1}, {4284,498,1},
    {4285,499,1}, {4286,500,1}, {4287,501,1}, {4288,502,1}, {4289,503,1}, {4290,504,1},
    {4291,505,1}, {4292,506,1}, {4293,507,1}, {4295,508,1}, {4301,509,1}, {5024,510,1},
    {5025,511,1}, {5026,512,1}, {5027,513,1}, {5028,514,1}, {5029,515,1}, {5030,516,1},
    {5031,517,1}, {5032,518,1}, {5033,519,1}, {5034,520,1}, {5035,521,1}, {5036,522,1},
    {5037,523,1}, {5038,524,1}, {5039,525,1}, {5040,526,1}, {5041,527,1}, {5042,528,1},
    {5043,529,1}, {5044,530,1}, {5045,531,1}, {5046,532,1}, {5047,533,1}, {5048,534,1},
    {5049,535,1}, {5050,536,1}, {5051,537,1}, {5052,538,1}, {5053,539,1}, {5054,540,1},
    {5055,541,1}, {5056,542,1}, {5057,543,1}, {5058,544,1}, {5059,545,1}, {5060,546,1},
    {5061,547,1}, {5062,548,1}, {5063,549,1}, {5064,550,1}, {5065,551,1}, {5066,552,1},
    {5067,553,1}, {5068,554,1}, {5069,555,1}, {5070,556,1}, {5071,557,1}, {5072,558,1},
    {5073,559,1}, {5074,560,1}, {5075,561,1}, {5076,562,1}, {5077,563,1}, {5078,564,1},
    {5079,565,1}, {5080,566,1}, {5081,567,1}, {5082,568,1}, {5083,569,1}, {5084,570,1},
    {5085,571,1}, {5086,572,1}, {5087,573,1}, {5088,574,1}, {5089,575,1}, {5090,576,1},
    {5091,577,1}, {5092,578,1}, {5093,579,1}, {5094,580,1}, {5095,581,1}, {5096,582,1},
    {5097,583,1}, {5098,584,1}, {5099,585,1}, {5100,586,1}, {5101,587,1}, {5102,588,1},
    {5103,589,1}, {5104,590,1}, {5105,591,1}, {5106,592,1}, {5107,593,1}, {5108,594,1},
    {5109,595,1}, {7305,596,1}, {7312,597,1}, {7313,598,1}, {7314,599,1}, {7315,600,1},
    {7316,601,1}, {7317,602,1}, {7318,603,1}, {7319,604,1}, {7320,605,1}, {7321,606,1},
    {7322,607,1}, {7323,608,1}, {7324,609,1}, {7325,610,1}, {7326,611,1}, {7327,612,1},
    {7328,613,1}, {7329,614,1}, {7330,615,1}, {7331,616,1}, {7332,617,1}, {7333,618,1},
    {7334,619,1}, {7335,620,1}, {7336,621,1}, {7337,622,1}, {7338,623,1}, {7339,624,1},
    {7340,625,1}, {7341,626,1}, {7342,627,1}, {7343,628,1}, {7344,629,1}, {7345,630,1},
    {7346,631,1}, {7347,632,1}, {7348,633,1}, {7349,634,1}, {7350,635,1}, {7351,636,1},
    {7352,637,1}, {7353,638,1}, {7354,639,1}, {7357,640,1}, {7358,641,1}, {7359,642,1},
    {7680,643,1}, {7682,644,1}, {7684,645,1}, {7686,646,1}, {7688,647,1}, {7690,648,1},
    {7692,649,1}, {7694,650,1}, {7696,651,1}, {7698,652,1}, {7700,653,1}, {7702,654,1},
    {7704,655,1}, {7706,656,1}, {7708,657,1}, {7710,658,1}, {7712,659,1}, {7714,660,1},
    {7716,661,1}, {7718,662,1}, {7720,663,1}, {7722,664,1}, {7724,665,1}, {7726,666,1},
    {7728,667,1}, {7730,668,1}, {7732,669,1}, {7734,670,1}, {7736,671,1}, {7738,672,1},
    {7740,673,1}, {7742,674,1}, {7744,675,1}, {7746,676,1}, {7748,677,1}, {7750,678,1},
    {7752,679,1}, {7754,680,1}, {7756,681,1}, {7758,682,1}, {7760,683,1}, {7762,684,1},
    {7764,685,1}, {7766,686,1}, {7768,687,1}, {7770,688,1}, {7772,689,1}, {7774,690,1},
    {7776,691,1}, {7778,692,1}, {7780,693,1}, {7782,694,1}, {7784,695,1}, {7786,696,1},
    {7788,697,1}, {7790,698,1}, {7792,699,1}, {7794,700,1}, {7796,701,1}, {7798,702,1},
    {7800,703,1}, {7802,704,1}, {7804,705,1}, {7806,706,1}, {7808,707,1}, {7810,708,1},
    {7812,709,1}, {7814,710,1}, {7816,711,1}, {7818,712,1}, {7820,713,1}, {7822,714,1},
    {7824,715,1}, {7826,716,1}, {7828,717,1}, {7838,718,1}, {7840,719,1}, {7842,720,1},
    {7844,721,1}, {7846,722,1}, {7848,723,1}, {7850,724,1}, {7852,725,1}, {7854,726,1},
    {7856,727,1}, {7858,728,1}, {7860,729,1}, {7862,730,1}, {7864,731,1}, {7866,732,1},
    {7868,733,1}, {7870,734,1}, {7872,735,1}, {7874,736,1}, {7876,737,1}, {7878,738,1},
    {7880,739,1}, {7882,740,1}, {7884,741,1}, {7886,742,1}, {7888,743,1}, {7890,744,1},
    {7892,745,1}, {7894,746,1}, {7896,747,1}, {7898,748,1}, {7900,749,1}, {7902,750,1},
    {7904,751,1}, {7906,752,1}, {7908,753,1}, {7910,754,1}, {7912,755,1}, {7914,756,1},
    {7916,757,1}, {7918,758,1}, {7920,759,1}, {7922,760,1}, {7924,761,1}, {7926,762,1},
    {7928,763,1}, {7930,764,1}, {7932,765,1}, {7934,766,1}, {7944,767,1}, {7945,768,1},
    {7946,769,1}, {7947,770,1}, {7948,771,1}, {7949,772,1}, {7950,773,1}, {7951,774,1},
    {7960,775,1}, {7961,776,1}, {7962,777,1}, {7963,778,1}, {7964,779,1}, {7965,780,1},
    {7976,781,1}, {7977,782,1}, {7978,783,1}, {7979,784,1}, {7980,785,1}, {7981,786,1},
    {7982,787,1}, {7983,788,1}, {7992,789,1}, {7993,790,1}, {7994,791,1}, {7995,792,1},
    {7996,793,1}, {7997,794,1}, {7998,795,1}, {7999,796,1}, {8008,797,1}, {8009,798,1},
    {8010,799,1}, {8011,800,1}, {8012,801,1}, {8013,802,1}, {8025,803,1}, {8027,804,1},
    {8029,805,1}, {8031,806,1}, {8040,807,1}, {8041,808,1}, {8042,809,1}, {8043,810,1},
    {8044,811,1}, {8045,812,1}, {8046,813,1}, {8047,814,1}, {8072,815,1}, {8073,816,1},
    {8074,817,1}, {8075,818,1}, {8076,819,1}, {8077,820,1}, {8078,821,1}, {8079,822,1},
    {8088,823,1}, {8089,824,1}, {8090,825,1}, {8091,826,1}, {8092,827,1}, {8093,828,1},
    {8094,829,1}, {8095,830,1}, {8104,831,1}, {8105,832,1}, {8106,833,1}, {8107,834,1},
    {8108,835,1}, {8109,836,1}, {8110,837,1}, {8111,838,1}, {8120,839,1}, {8121,840,1},
    {8122,841,1}, {8123,842,1}, {8124,843,1}, {8136,844,1}, {8137,845,1}, {8138,846,1},
    {8139,847,1}, {8140,848,1}, {8152,849,1}, {8153,850,1}, {8154,851,1}, {8155,852,1},
    {8168,853,1}, {8169,854,1}, {8170,855,1}, {8171,856,1}, {8172,857,1}, {8184,858,1},
    {8185,859,1}, {8186,860,1}, {8187,861,1}, {8188,862,1}, {8486,863,1}, {8490,864,1},
    {8491,865,1}, {8498,866,1}, {8544,867,1}, {8545,868,1}, {8546,869,1}, {8547,870,1},
    {8548,871,1}, {8549,872,1}, {8550,873,1}, {8551,874,1}, {8552,875,1}, {8553,876,1},
    {8554,877,1}, {8555,878,1}, {8556,879,1}, {8557,880,1}, {8558,881,1}, {8559,882,1},
    {8579,883,1}, {9398,884,1}, {9399,885,1}, {9400,886,1}, {9401,887,1}, {9402,888,1},
    {9403,889,1}, {9404,890,1}, {9405,891,1}, {9406,892,1}, {9407,893,1}, {9408,894,1},
    {9409,895,1}, {9410,896,1}, {9411,897,1}, {9412,898,1}, {9413,899,1}, {9414,900,1},
    {9415,901,1}, {9416,902,1}, {9417,903,1}, {9418,904,1}, {9419,905,1}, {9420,906,1},
    {9421,907,1}, {9422,908,1}, {9423,909,1}, {11264,910,1}, {11265,911,1}, {11266,912,1},
    {11267,913,1}, {11268,914,1}, {11269,915,1}, {11270,916,1}, {11271,917,1}, {11272,918,1},
    {11273,919,1}, {11274,920,1}, {11275,921,1}, {11276,922,1}, {11277,923,1}, {11278,924,1},
    {11279,925,1}, {11280,926,1}, {11281,927,1}, {11282,928,1}, {11283,929,1}, {11284,930,1},
    {11285,931,1}, {11286,932,1}, {11287,933,1}, {11288,934,1}, {11289,935,1}, {11290,936,1},
    {11291,937,1}, {11292,938,1}, {11293,939,1}, {11294,940,1}, {11295,941,1}, {11296,942,1},
    {11297,943,1}, {11298,944,1}, {11299,945,1}, {11300,946,1}, {11301,947,1}, {11302,948,1},
    {11303,949,1}, {11304,950,1}, {11305,951,1}, {11306,952,1}, {11307,953,1}, {11308,954,1},
    {11309,955,1}, {11310,956,1}, {11311,957,1}, {11360,958,1}, {11362,959,1}, {11363,960,1},
    {11364,961,1}, {11367,962,1}, {11369,963,1}, {11371,964,1}, {11373,965,1}, {11374,966,1},
    {11375,967,1}, {11376,968,1}, {11378,969,1}, {11381,970,1}, {11390,971,1}, {11391,972,1},
    {11392,973,1}, {11394,974,1}, {11396,975,1}, {11398,976,1}, {11400,977,1}, {11402,978,1},
    {11404,979,1}, {11406,980,1}, {11408,981,1}, {11410,982,1}, {11412,983,1}, {11414,984,1},
    {11416,985,1}, {11418,986,1}, {11420,987,1}, {11422,988,1}, {11424,989,1}, {11426,990,1},
    {11428,991,1}, {11430,992,1}, {11432,993,1}, {11434,994,1}, {11436,995,1}, {11438,996,1},
    {11440,997,1}, {11442,998,1}, {11444,999,1}, {11446,1000,1}, {11448,1001,1}, {11450,1002,1},
    {11452,1003,1}, {11454,1004,1}, {11456,1005,1}, {11458,1006,1}, {11460,1007,1}, {11462,1008,1},
    {11464,1009,1}, {11466,1010,1}, {11468,1011,1}, {11470,1012,1}, {11472,1013,1}, {11474,1014,1},
    {11476,1015,1}, {11478,1016,1}, {11480,1017,1}, {11482,1018,1}, {11484,1019,1}, {11486,1020,1},
    {11488,1021,1}, {11490,1022,1}, {11499,1023,1}, {11501,1024,1}, {11506,1025,1}, {42560,1026,1},
    {42562,1027,1}, {42564,1028,1}, {42566,1029,1}, {42568,1030,1}, {42570,1031,1}, {42572,1032,1},
    {42574,1033,1}, {42576,1034,1}, {42578,1035,1}, {42580,1036,1}, {42582,1037,1}, {42584,1038,1},
    {42586,1039,1}, {42588,1040,1}, {42590,1041,1}, {42592,1042,1}, {42594,1043,1}, {42596,1044,1},
    {42598,1045,1}, {42600,1046,1}, {42602,1047,1}, {42604,1048,1}, {42624,1049,1}, {42626,1050,1},
    {42628,1051,1}, {42630,1052,1}, {42632,1053,1}, {42634,1054,1}, {42636,1055,1}, {42638,1056,1},
    {42640,1057,1}, {42642,1058,1}, {42644,1059,1}, {42646,1060,1}, {42648,1061,1}, {42650,1062,1},
    {42786,1063,1}, {42788,1064,1}, {42790,1065,1}, {42792,1066,1}, {42794,1067,1}, {42796,1068,1},
    {42798,1069,1}, {42802,1070,1}, {42804,1071,1}, {42806,1072,1}, {42808,1073,1}, {42810,1074,1},
    {42812,1075,1}, {42814,1076,1}, {42816,1077,1}, {42818,1078,1}, {42820,1079,1}, {42822,1080,1},
    {42824,1081,1}, {42826,1082,1}, {42828,1083,1}, {42830,1084,1}, {42832,1085,1}, {42834,1086,1},
    {42836,1087,1}, {42838,1088,1}, {42840,1089,1}, {42842,1090,1}, {42844,1091,1}, {42846,1092,1},
    {42848,1093,1}, {42850,1094,1}, {42852,1095,1}, {42854,1096,1}, {42856,1097,1}, {42858,1098,1},
    {42860,1099,1}, {42862,1100,1}, {42873,1101,1}, {42875,1102,1}, {42877,1103,1}, {42878,1104,1},
    {42880,1105,1}, {42882,1106,1}, {42884,1107,1}, {42886,1108,1}, {42891,1109,1}, {42893,1110,1},
    {42896,1111,1}, {42898,1112,1}, {42902,1113,1}, {42904,1114,1}, {42906,1115,1}, {42908,1116,1},
    {42910,1117,1}, {42912,1118,1}, {42914,1119,1}, {42916,1120,1}, {42918,1121,1}, {42920,1122,1},
    {42922,1123,1}, {42923,1124,1}, {42924,1125,1}, {42925,1126,1}, {42926,1127,1}, {42928,1128,1},
    {42929,1129,1}, {42930,1130,1}, {42931,1131,1}, {42932,1132,1}, {42934,1133,1}, {42936,1134,1},
    {42938,1135,1}, {42940,1136,1}, {42942,1137,1}, {42944,1138,1}, {42946,1139,1}, {42948,1140,1},
    {42949,1141,1}, {42950,1142,1}, {42951,1143,1}, {42953,1144,1}, {42955,1145,1}, {42956,1146,1},
    {42960,1147,1}, {42966,1148,1}, {42968,1149,1}, {42970,1150,1}, {42972,1151,1}, {42997,1152,1},
    {65313,1153,1}, {65314,1154,1}, {65315,1155,1}, {65316,1156,1}, {65317,1157,1}, {65318,1158,1},
    {65319,1159,1}, {65320,1160,1}, {65321,1161,1}, {65322,1162,1}, {65323,1163,1}, {65324,1164,1},
    {65325,1165,1}, {65326,1166,1}, {65327,1167,1}, {65328,1168,1}, {65329,1169,1}, {65330,1170,1},
    {65331,1171,1}, {65332,1172,1}, {65333,1173,1}, {65334,1174,1}, {65335,1175,1}, {65336,1176,1},
    {65337,1177,1}, {65338,1178,1}, {66560,1179,1}, {66561,1180,1}, {66562,1181,1}, {66563,1182,1},
    {66564,1183,1}, {66565,1184,1}, {66566,1185,1}, {66567,1186,1}, {66568,1187,1}, {66569,1188,1},
    {66570,1189,1}, {66571,1190,1}, {66572,1191,1}, {66573,1192,1}, {66574,1193,1}, {66575,1194,1},
    {66576,1195,1}, {66577,1196,1}, {66578,1197,1}, {66579,1198,1}, {66580,1199,1}, {66581,1200,1},
    {66582,1201,1}, {66583,1202,1}, {66584,1203,1}, {66585,1204,1}, {66586,1205,1}, {66587,1206,1},
    {66588,1207,1}, {66589,1208,1}, {66590,1209,1}, {66591,1210,1}, {66592,1211,1}, {66593,1212,1},
    {66594,1213,1}, {66595,1214,1}, {66596,1215,1}, {66597,1216,1}, {66598,1217,1}, {66599,1218,1},
    {66736,1219,1}, {66737,1220,1}, {66738,1221,1}, {66739,1222,1}, {66740,1223,1}, {66741,1224,1},
    {66742,1225,1}, {66743,1226,1}, {66744,1227,1}, {66745,1228,1}, {66746,1229,1}, {66747,1230,1},
    {66748,1231,1}, {66749,1232,1}, {66750,1233,1}, {66751,1234,1}, {66752,1235,1}, {66753,1236,1},
    {66754,1237,1}, {66755,1238,1}, {66756,1239,1}, {66757,1240,1}, {66758,1241,1}, {66759,1242,1},
    {66760,1243,1}, {66761,1244,1}, {66762,1245,1}, {66763,1246,1}, {66764,1247,1}, {66765,1248,1},
    {66766,1249,1}, {66767,1250,1}, {66768,1251,1}, {66769,1252,1}, {66770,1253,1}, {66771,1254,1},
    {66928,1255,1}, {66929,1256,1}, {66930,1257,1}, {66931,1258,1}, {66932,1259,1}, {66933,1260,1},
    {66934,1261,1}, {66935,1262,1}, {66936,1263,1}, {66937,1264,1}, {66938,1265,1}, {66940,1266,1},
    {66941,1267,1}, {66942,1268,1}, {66943,1269,1}, {66944,1270,1}, {66945,1271,1}, {66946,1272,1},
    {66947,1273,1}, {66948,1274,1}, {66949,1275,1}, {66950,1276,1}, {66951,1277,1}, {66952,1278,1},
    {66953,1279,1}, {66954,1280,1}, {66956,1281,1}, {66957,1282,1}, {66958,1283,1}, {66959,1284,1},
    {66960,1285,1}, {66961,1286,1}, {66962,1287,1}, {66964,1288,1}, {66965,1289,1}, {68736,1290,1},
    {68737,1291,1}, {68738,1292,1}, {68739,1293,1}, {68740,1294,1}, {68741,1295,1}, {68742,1296,1},
    {68743,1297,1}, {68744,1298,1}, {68745,1299,1}, {68746,1300,1}, {68747,1301,1}, {68748,1302,1},
    {68749,1303,1}, {68750,1304,1}, {68751,1305,1}, {68752,1306,1}, {68753,1307,1}, {68754,1308,1},
    {68755,1309,1}, {68756,1310,1}, {68757,1311,1}, {68758,1312,1}, {68759,1313,1}, {68760,1314,1},
    {68761,1315,1}, {68762,1316,1}, {68763,1317,1}, {68764,1318,1}, {68765,1319,1}, {68766,1320,1},
    {68767,1321,1}, {68768,1322,1}, {68769,1323,1}, {68770,1324,1}, {68771,1325,1}, {68772,1326,1},
    {68773,1327,1}, {68774,1328,1}, {68775,1329,1}, {68776,1330,1}, {68777,1331,1}, {68778,1332,1},
    {68779,1333,1}, {68780,1334,1}, {68781,1335,1}, {68782,1336,1}, {68783,1337,1}, {68784,1338,1},
    {68785,1339,1}, {68786,1340,1}, {68944,1341,1}, {68945,1342,1}, {68946,1343,1}, {68947,1344,1},
    {68948,1345,1}, {68949,1346,1}, {68950,1347,1}, {68951,1348,1}, {68952,1349,1}, {68953,1350,1},
    {68954,1351,1}, {68955,1352,1}, {68956,1353,1}, {68957,1354,1}, {68958,1355,1}, {68959,1356,1},
    {68960,1357,1}, {68961,1358,1}, {68962,1359,1}, {68963,1360,1}, {68964,1361,1}, {68965,1362,1},
    {71840,1363,1}, {71841,1364,1}, {71842,1365,1}, {71843,1366,1}, {71844,1367,1}, {71845,1368,1},
    {71846,1369,1}, {71847,1370,1}, {71848,1371,1}, {71849,1372,1}, {71850,1373,1}, {71851,1374,1},
    {71852,1375,1}, {71853,1376,1}, {71854,1377,1}, {71855,1378,1}, {71856,1379,1}, {71857,1380,1},
    {71858,1381,1}, {71859,1382,1}, {71860,1383,1}, {71861,1384,1}, {71862,1385,1}, {71863,1386,1},
    {71864,1387,1}, {71865,1388,1}, {71866,1389,1}, {71867,1390,1}, {71868,1391,1}, {71869,1392,1},
    {71870,1393,1}, {71871,1394,1}, {93760,1395,1}, {93761,1396,1}, {93762,1397,1}, {93763,1398,1},
    {93764,1399,1}, {93765,1400,1}, {93766,1401,1}, {93767,1402,1}, {93768,1403,1}, {93769,1404,1},
    {93770,1405,1}, {93771,1406,1}, {93772,1407,1}, {93773,1408,1}, {93774,1409,1}, {93775,1410,1},
    {93776,1411,1}, {93777,1412,1}, {93778,1413,1}, {93779,1414,1}, {93780,1415,1}, {93781,1416,1},
    {93782,1417,1}, {93783,1418,1}, {93784,1419,1}, {93785,1420,1}, {93786,1421,1}, {93787,1422,1},
    {93788,1423,1}, {93789,1424,1}, {93790,1425,1}, {93791,1426,1}, {125184,1427,1}, {125185,1428,1},
    {125186,1429,1}, {125187,1430,1}, {125188,1431,1}, {125189,1432,1}, {125190,1433,1}, {125191,1434,1},
    {125192,1435,1}, {125193,1436,1}, {125194,1437,1}, {125195,1438,1}, {125196,1439,1}, {125197,1440,1},
    {125198,1441,1}, {125199,1442,1}, {125200,1443,1}, {125201,1444,1}, {125202,1445,1}, {125203,1446,1},
    {125204,1447,1}, {125205,1448,1}, {125206,1449,1}, {125207,1450,1}, {125208,1451,1}, {125209,1452,1},
    {125210,1453,1}, {125211,1454,1}, {125212,1455,1}, {125213,1456,1}, {125214,1457,1}, {125215,1458,1},
    {125216,1459,1}, {125217,1460,1},
};
// END GENERATED UNICODE CASE TABLES

static int tinox_unicode_cp_valid(int64_t cp) {
    return cp > 0 && cp <= 0x10FFFF && !(cp >= 0xD800 && cp <= 0xDFFF);
}

// Byte offset of the first malformed byte, or -1 when `s` is valid UTF-8.
int64_t tinoxUtf8Invalid(const char* s) {
    if (!s) return 0;
    size_t length = strlen(s);
    for (size_t offset = 0; offset < length;) {
        size_t width = tinox_utf8_scalar_width((const unsigned char*)s + offset, length - offset);
        if (!width) return (int64_t)offset;
        offset += width;
    }
    return -1;
}

static uint32_t tinox_utf8_decode(const unsigned char* s, size_t width) {
    if (width == 1) return s[0];
    uint32_t cp = s[0] & (0xFFu >> (width + 1));
    for (size_t i = 1; i < width; i++) cp = (cp << 6) | (s[i] & 0x3F);
    return cp;
}

static size_t tinox_utf8_encode(uint32_t cp, char* out) {
    if (cp < 0x80) { out[0] = (char)cp; return 1; }
    if (cp < 0x800) { out[0] = (char)(0xC0 | (cp >> 6)); out[1] = (char)(0x80 | (cp & 0x3F)); return 2; }
    if (cp < 0x10000) {
        out[0] = (char)(0xE0 | (cp >> 12)); out[1] = (char)(0x80 | ((cp >> 6) & 0x3F)); out[2] = (char)(0x80 | (cp & 0x3F));
        return 3;
    }
    out[0] = (char)(0xF0 | (cp >> 18)); out[1] = (char)(0x80 | ((cp >> 12) & 0x3F));
    out[2] = (char)(0x80 | ((cp >> 6) & 0x3F)); out[3] = (char)(0x80 | (cp & 0x3F));
    return 4;
}

static void tinox_unicode_require_valid(const char* s, const char* what) {
    if (!s) { fprintf(stderr, "runtime error: %s requires a non-null string\n", what); exit(1); }
    int64_t bad = tinoxUtf8Invalid(s);
    if (bad >= 0) { fprintf(stderr, "runtime error: %s invalid UTF-8 at byte %lld\n", what, (long long)bad); exit(1); }
}

int64_t tinoxUtf8Count(const char* s) {
    tinox_unicode_require_valid(s, "Unicode.codePointCount");
    int64_t count = 0;
    size_t length = strlen(s);
    for (size_t offset = 0; offset < length;) {
        offset += tinox_utf8_scalar_width((const unsigned char*)s + offset, length - offset);
        count++;
    }
    return count;
}

int64_t* tinoxUtf8CodePoints(const char* s) {
    int64_t count = tinoxUtf8Count(s);
    int64_t* result = tinox_array_new(count, 0);
    int64_t* data = ((TinoxArray*)result)->data;
    size_t length = strlen(s), offset = 0;
    for (int64_t i = 0; i < count; i++) {
        size_t width = tinox_utf8_scalar_width((const unsigned char*)s + offset, length - offset);
        data[i] = (int64_t)tinox_utf8_decode((const unsigned char*)s + offset, width);
        offset += width;
    }
    return result;
}

// Index of the first element that is not a Unicode scalar value (or is NUL), -1 when all are.
int64_t tinoxUnicodeInvalidScalar(int64_t* codes) {
    TinoxArray* a = (TinoxArray*)codes;
    for (int64_t i = 0; a && i < a->len; i++) {
        if (!tinox_unicode_cp_valid(a->data[i])) return i;
    }
    return -1;
}

char* tinoxUtf8FromCodePoints(int64_t* codes) {
    TinoxArray* a = (TinoxArray*)codes;
    int64_t n = a ? a->len : 0;
    if (tinoxUnicodeInvalidScalar(codes) >= 0) {
        fprintf(stderr, "runtime error: Unicode.fromCodePoints element %lld is not a Unicode scalar value\n", (long long)tinoxUnicodeInvalidScalar(codes));
        exit(1);
    }
    char* buf = (char*)GC_malloc((size_t)n * 4 + 1);
    size_t o = 0;
    for (int64_t i = 0; i < n; i++) o += tinox_utf8_encode((uint32_t)a->data[i], buf + o);
    buf[o] = '\0';
    return buf;
}

// Byte offset where code point number `index` starts (`index` == count gives the length),
// or -1 when the index is outside 0..count.
int64_t tinoxUtf8ByteOffset(const char* s, int64_t index) {
    tinox_unicode_require_valid(s, "Unicode.byteOffset");
    if (index < 0) return -1;
    size_t length = strlen(s), offset = 0;
    for (int64_t i = 0; i < index; i++) {
        if (offset >= length) return -1;
        offset += tinox_utf8_scalar_width((const unsigned char*)s + offset, length - offset);
    }
    return (int64_t)offset;
}

#define TINOX_UNICODE_LOOKUP(NAME, cp, map_out, len_out) do { \
    size_t lo = 0, hi = sizeof(tinox_unicode_##NAME##_map) / sizeof(tinox_unicode_##NAME##_map[0]); \
    while (lo < hi) { \
        size_t mid = (lo + hi) / 2; \
        if (tinox_unicode_##NAME##_map[mid].cp < (cp)) lo = mid + 1; else hi = mid; \
    } \
    if (lo < sizeof(tinox_unicode_##NAME##_map) / sizeof(tinox_unicode_##NAME##_map[0]) && tinox_unicode_##NAME##_map[lo].cp == (cp)) { \
        (map_out) = tinox_unicode_##NAME##_pool + tinox_unicode_##NAME##_map[lo].offset; \
        (len_out) = tinox_unicode_##NAME##_map[lo].len; \
    } \
} while (0)

// Full default case mapping (Unicode `TINOX_UNICODE_VERSION`), no locale and no context
// rules (a capital sigma always lowers to U+03C3, never to a final sigma).
char* tinoxUnicodeCase(const char* s, int64_t upper) {
    tinox_unicode_require_valid(s, upper ? "Unicode.toUpper" : "Unicode.toLower");
    size_t length = strlen(s);
    char* buf = (char*)GC_malloc(length * 3 + 1);
    size_t o = 0;
    for (size_t offset = 0; offset < length;) {
        size_t width = tinox_utf8_scalar_width((const unsigned char*)s + offset, length - offset);
        uint32_t cp = tinox_utf8_decode((const unsigned char*)s + offset, width);
        const uint32_t* mapped = NULL;
        size_t mapped_len = 0;
        if (upper) TINOX_UNICODE_LOOKUP(upper, cp, mapped, mapped_len);
        else TINOX_UNICODE_LOOKUP(lower, cp, mapped, mapped_len);
        if (mapped) {
            for (size_t i = 0; i < mapped_len; i++) o += tinox_utf8_encode(mapped[i], buf + o);
        } else {
            memcpy(buf + o, s + offset, width);
            o += width;
        }
        offset += width;
    }
    buf[o] = '\0';
    return buf;
}

char* tinox_string_substring(const char* s, int64_t from, int64_t to) {
    int64_t len = (int64_t)strlen(s);
    if (from < 0 || to > len || from > to) {
        fprintf(stderr, "runtime error: substring range out of bounds: [%lld, %lld) (length %lld)\n",
                (long long)from, (long long)to, (long long)len);
        exit(1);
    }
    // Strings are byte strings: slicing is by byte offset. Cutting through the
    // middle of a multi-byte character of a valid UTF-8 string would silently
    // produce invalid UTF-8, so that is an error (issue #472).
    if (((from < len && ((unsigned char)s[from] & 0xC0) == 0x80) || (to < len && ((unsigned char)s[to] & 0xC0) == 0x80))
        && tinox_utf8_valid((const unsigned char*)s)) {
        fprintf(stderr, "runtime error: substring [%lld, %lld) splits a multi-byte UTF-8 character (offsets are bytes)\n",
                (long long)from, (long long)to);
        exit(1);
    }
    if (from == to) { char* r = malloc(1); r[0] = '\0'; return r; }
    int64_t slen = to - from;
    char* result = malloc(slen + 1);
    memcpy(result, s + from, slen);
    result[slen] = '\0';
    return result;
}

char* tinox_string_repeat(const char* s, int64_t n) {
    size_t len = strlen(s);
    if (n < 0 || (len > 0 && (uint64_t)n > (uint64_t)INT32_MAX * 1024ULL / len)) {
        fprintf(stderr, "runtime error: String.repeat count out of range: %lld\n", (long long)n);
        exit(1);
    }
    char* result = malloc(len * (size_t)n + 1);
    for (int64_t i = 0; i < n; i++) memcpy(result + (size_t)i * len, s, len);
    result[len * (size_t)n] = '\0';
    return result;
}

// Pads `s` to `width` bytes with repetitions of `pad` (truncated to fit) on the
// left or right. A string already at least `width` long is returned unchanged.
char* tinox_string_pad(const char* s, int64_t width, const char* pad, int64_t left) {
    size_t len = strlen(s), plen = strlen(pad);
    if (plen == 0) {
        fprintf(stderr, "runtime error: String.pad* needs a non-empty pad string\n");
        exit(1);
    }
    if (width < 0 || (uint64_t)width > (uint64_t)INT32_MAX * 1024ULL) {
        fprintf(stderr, "runtime error: String.pad* width out of range: %lld\n", (long long)width);
        exit(1);
    }
    size_t total = (size_t)width > len ? (size_t)width : len;
    char* result = malloc(total + 1);
    size_t fill = total - len;
    if (left) {
        for (size_t i = 0; i < fill; i++) result[i] = pad[i % plen];
        memcpy(result + fill, s, len);
    } else {
        memcpy(result, s, len);
        for (size_t i = 0; i < fill; i++) result[len + i] = pad[i % plen];
    }
    result[total] = '\0';
    return result;
}

// ---- Message translation (#588): gettext catalogs ----
// tr(msgid), trc(context, msgid), trn(singular, plural, n) look the text up in a GNU gettext binary catalog
// (.mo, produced by msgfmt) and return the msgid / the English plural rule when there is none.
//
// Catalog search: the language comes from LANGUAGE (colon separated list), else LC_ALL, LC_MESSAGES, LANG
// (encoding and @modifier are dropped; "de_DE" also tries "de"; C/POSIX mean no translation). The first
// candidate with a catalog is used. A catalog is <dir>/<language>/LC_MESSAGES/<domain>.mo for dir in
// TINOX_LOCALE_DIR, <directory of the executable>/locale, ./locale, /usr/share/locale. The domain is
// TINOX_TEXTDOMAIN, default "messages". The catalog is read once per process and never freed (returned
// strings point into it).
typedef struct {
    const unsigned char* data;
    size_t size;
    int swap;
    uint32_t count, orig, trans;
    char plural[256]; // the expression after "plural=" in the header's Plural-Forms
} TinoxCatalog;
static TinoxCatalog* tr_catalog;
static pthread_once_t tr_once = PTHREAD_ONCE_INIT;

static uint32_t tr_u32(const TinoxCatalog* c, size_t off) {
    uint32_t v;
    memcpy(&v, c->data + off, 4);
    return c->swap ? __builtin_bswap32(v) : v;
}
// i-th string of the original (table = c->orig) or translation (c->trans) table, or NULL if malformed.
static const char* tr_entry(const TinoxCatalog* c, uint32_t table, uint32_t i, uint32_t* len) {
    size_t at = (size_t)table + (size_t)i * 8;
    if (at + 8 > c->size) return NULL;
    uint32_t l = tr_u32(c, at), off = tr_u32(c, at + 4);
    if ((size_t)off + l + 1 > c->size || c->data[(size_t)off + l] != 0) return NULL;
    *len = l;
    return (const char*)c->data + off;
}
static TinoxCatalog* tr_load(const char* path) {
    FILE* f = fopen(path, "rb");
    if (!f) return NULL;
    fseek(f, 0, SEEK_END);
    long size = ftell(f);
    fseek(f, 0, SEEK_SET);
    if (size < 28 || size > (64L << 20)) { fclose(f); return NULL; }
    unsigned char* data = malloc((size_t)size);
    if (!data || fread(data, 1, (size_t)size, f) != (size_t)size) { free(data); fclose(f); return NULL; }
    fclose(f);
    TinoxCatalog* c = calloc(1, sizeof(*c));
    c->data = data;
    c->size = (size_t)size;
    uint32_t magic;
    memcpy(&magic, data, 4);
    if (magic == 0x950412deu) c->swap = 0;
    else if (magic == 0xde120495u) c->swap = 1;
    else { free(data); free(c); return NULL; }
    c->count = tr_u32(c, 8);
    c->orig = tr_u32(c, 12);
    c->trans = tr_u32(c, 16);
    if (c->count > (c->size / 16) || (size_t)c->orig + (size_t)c->count * 8 > c->size || (size_t)c->trans + (size_t)c->count * 8 > c->size) {
        free(data); free(c); return NULL;
    }
    // The header entry (empty msgid) carries "Plural-Forms: nplurals=N; plural=EXPR;".
    uint32_t len;
    const char* key = tr_entry(c, c->orig, 0, &len);
    const char* header = key && len == 0 ? tr_entry(c, c->trans, 0, &len) : NULL;
    const char* p = header ? strstr(header, "plural=") : NULL;
    if (p) {
        p += 7;
        size_t n = 0;
        while (p[n] && p[n] != ';' && p[n] != '\n' && n < sizeof(c->plural) - 1) n++;
        memcpy(c->plural, p, n);
        c->plural[n] = 0;
    }
    return c;
}
static void tr_candidates(char out[8][64], int* count) {
    *count = 0;
    const char* sources[] = { getenv("LANGUAGE"), getenv("LC_ALL"), getenv("LC_MESSAGES"), getenv("LANG") };
    const char* list = NULL;
    for (int i = 0; i < 4 && !list; i++)
        if (sources[i] && *sources[i]) list = sources[i];
    if (!list) return;
    char buf[256];
    snprintf(buf, sizeof buf, "%s", list);
    for (char* tok = strtok(buf, ":"); tok && *count < 6; tok = strtok(NULL, ":")) {
        char* cut = strpbrk(tok, ".@");
        if (cut) *cut = 0;
        if (!*tok || !strcmp(tok, "C") || !strcmp(tok, "POSIX")) continue;
        snprintf(out[(*count)++], 64, "%s", tok);
        char* us = strchr(tok, '_');
        if (us && *count < 8) { *us = 0; snprintf(out[(*count)++], 64, "%s", tok); }
    }
}
static void tr_init(void) {
    char langs[8][64];
    int n;
    tr_candidates(langs, &n);
    if (!n) return;
    const char* domain = getenv("TINOX_TEXTDOMAIN");
    if (!domain || !*domain) domain = "messages";
    char exe[PATH_MAX], exedir[PATH_MAX + 8] = "";
    ssize_t len = readlink("/proc/self/exe", exe, sizeof exe - 1);
    if (len > 0) {
        exe[len] = 0;
        char* slash = strrchr(exe, '/');
        if (slash) { *slash = 0; snprintf(exedir, sizeof exedir, "%s/locale", exe); }
    }
    const char* dirs[] = { getenv("TINOX_LOCALE_DIR"), exedir[0] ? exedir : NULL, "locale", "/usr/share/locale" };
    for (int i = 0; i < n; i++) {
        for (int d = 0; d < 4; d++) {
            if (!dirs[d] || !*dirs[d]) continue;
            char path[PATH_MAX + 256];
            snprintf(path, sizeof path, "%s/%s/LC_MESSAGES/%s.mo", dirs[d], langs[i], domain);
            if ((tr_catalog = tr_load(path))) return;
        }
    }
}
// Binary search of the sorted original table; returns the translation or NULL.
static const char* tr_find(const char* key, size_t key_len, uint32_t* out_len) {
    pthread_once(&tr_once, tr_init);
    const TinoxCatalog* c = tr_catalog;
    if (!c) return NULL;
    uint32_t lo = 0, hi = c->count;
    while (lo < hi) {
        uint32_t mid = lo + (hi - lo) / 2, l;
        const char* orig = tr_entry(c, c->orig, mid, &l);
        if (!orig) return NULL;
        size_t m = l < key_len ? l : key_len;
        int cmp = memcmp(orig, key, m);
        if (!cmp) cmp = l < key_len ? -1 : (l > key_len ? 1 : 0);
        if (!cmp) {
            const char* t = tr_entry(c, c->trans, mid, out_len);
            return t && *out_len ? t : NULL;
        }
        if (cmp < 0) lo = mid + 1; else hi = mid;
    }
    return NULL;
}

// GNU plural expression: n, integers, ! ( ) * / % + - < <= > >= == != && || ?:
typedef struct { const char* p; unsigned long n; int bad; } TrExpr;
static unsigned long tr_ternary(TrExpr* e);
static void tr_ws(TrExpr* e) { while (*e->p == ' ' || *e->p == '\t') e->p++; }
static unsigned long tr_primary(TrExpr* e) {
    tr_ws(e);
    if (*e->p == '(') { e->p++; unsigned long v = tr_ternary(e); tr_ws(e); if (*e->p == ')') e->p++; else e->bad = 1; return v; }
    if (*e->p == '!') { e->p++; return !tr_primary(e); }
    if (*e->p == 'n') { e->p++; return e->n; }
    if (*e->p >= '0' && *e->p <= '9') { unsigned long v = 0; while (*e->p >= '0' && *e->p <= '9') v = v * 10 + (unsigned long)(*e->p++ - '0'); return v; }
    e->bad = 1;
    return 0;
}
static unsigned long tr_mul(TrExpr* e) {
    unsigned long v = tr_primary(e);
    for (;;) {
        tr_ws(e);
        char op = *e->p;
        if (op != '*' && op != '/' && op != '%') return v;
        e->p++;
        unsigned long r = tr_primary(e);
        if (op == '*') v *= r; else if (!r) { e->bad = 1; return 0; } else v = op == '/' ? v / r : v % r;
    }
}
static unsigned long tr_add(TrExpr* e) {
    unsigned long v = tr_mul(e);
    for (;;) {
        tr_ws(e);
        if (*e->p != '+' && *e->p != '-') return v;
        char op = *e->p++;
        unsigned long r = tr_mul(e);
        v = op == '+' ? v + r : v - r;
    }
}
static unsigned long tr_rel(TrExpr* e) {
    unsigned long v = tr_add(e);
    for (;;) {
        tr_ws(e);
        if (e->p[0] == '<' && e->p[1] == '=') { e->p += 2; v = v <= tr_add(e); }
        else if (e->p[0] == '>' && e->p[1] == '=') { e->p += 2; v = v >= tr_add(e); }
        else if (e->p[0] == '<') { e->p++; v = v < tr_add(e); }
        else if (e->p[0] == '>') { e->p++; v = v > tr_add(e); }
        else return v;
    }
}
static unsigned long tr_eq(TrExpr* e) {
    unsigned long v = tr_rel(e);
    for (;;) {
        tr_ws(e);
        if (e->p[0] == '=' && e->p[1] == '=') { e->p += 2; v = v == tr_rel(e); }
        else if (e->p[0] == '!' && e->p[1] == '=') { e->p += 2; v = v != tr_rel(e); }
        else return v;
    }
}
static unsigned long tr_and(TrExpr* e) {
    unsigned long v = tr_eq(e);
    for (;;) { tr_ws(e); if (e->p[0] == '&' && e->p[1] == '&') { e->p += 2; unsigned long r = tr_eq(e); v = v && r; } else return v; }
}
static unsigned long tr_or(TrExpr* e) {
    unsigned long v = tr_and(e);
    for (;;) { tr_ws(e); if (e->p[0] == '|' && e->p[1] == '|') { e->p += 2; unsigned long r = tr_and(e); v = v || r; } else return v; }
}
static unsigned long tr_ternary(TrExpr* e) {
    unsigned long c = tr_or(e);
    tr_ws(e);
    if (*e->p != '?') return c;
    e->p++;
    unsigned long a = tr_ternary(e);
    tr_ws(e);
    if (*e->p == ':') e->p++; else e->bad = 1;
    unsigned long b = tr_ternary(e);
    return c ? a : b;
}
// Plural form index for n; falls back to the English rule (n != 1) when the header has no usable expression.
static unsigned long tr_plural_index(const TinoxCatalog* c, unsigned long n) {
    if (c && c->plural[0]) {
        TrExpr e = { c->plural, n, 0 };
        unsigned long v = tr_ternary(&e);
        if (!e.bad) return v;
    }
    return n != 1;
}

char* tr(const char* msgid) {
    uint32_t len;
    const char* t = tr_find(msgid, strlen(msgid), &len);
    return (char*)(t ? t : msgid);
}
char* trc(const char* context, const char* msgid) {
    size_t cl = strlen(context), ml = strlen(msgid);
    char* key = malloc(cl + 1 + ml + 1);
    memcpy(key, context, cl);
    key[cl] = '\004';
    memcpy(key + cl + 1, msgid, ml + 1);
    uint32_t len;
    const char* t = tr_find(key, cl + 1 + ml, &len);
    free(key);
    return (char*)(t ? t : msgid);
}
char* trn(const char* singular, const char* plural, int64_t n) {
    size_t sl = strlen(singular), pl = strlen(plural);
    char* key = malloc(sl + 1 + pl + 1);
    memcpy(key, singular, sl);
    key[sl] = 0;
    memcpy(key + sl + 1, plural, pl + 1);
    uint32_t len;
    const char* t = tr_find(key, sl + 1 + pl, &len);
    free(key);
    unsigned long un = n < 0 ? (unsigned long)(-n) : (unsigned long)n;
    if (t) {
        unsigned long want = tr_plural_index(tr_catalog, un);
        const char* form = t;
        const char* end = t + len;
        for (unsigned long i = 0; i < want; i++) {
            const char* next = memchr(form, 0, (size_t)(end - form));
            if (!next || next + 1 >= end) { form = NULL; break; }
            form = next + 1;
        }
        if (form && *form) return (char*)form;
    }
    return (char*)(n == 1 ? singular : plural);
}

// "text %1 and %2".arg(a): replaces every occurrence of the lowest-numbered placeholder %N (N = 1..99).
// Placeholders keep their number, so a translation may reorder them.
char* tinox_string_arg(const char* s, const char* value) {
    int lowest = 0;
    for (const char* p = s; *p; p++) {
        if (*p != '%' || *(p + 1) < '0' || *(p + 1) > '9') continue;
        int n = p[1] - '0', digits = 1;
        if (p[2] >= '0' && p[2] <= '9') { n = n * 10 + (p[2] - '0'); digits = 2; }
        if (n > 0 && (!lowest || n < lowest)) lowest = n;
        p += digits;
    }
    if (!lowest) return (char*)s;
    size_t vl = strlen(value), cap = strlen(s) + 1, used = 0;
    for (const char* p = s; *p; p++) cap += vl;
    char* out = (char*)GC_malloc_atomic(cap);
    for (const char* p = s; *p;) {
        if (*p == '%' && p[1] >= '0' && p[1] <= '9') {
            int n = p[1] - '0', digits = 1;
            if (p[2] >= '0' && p[2] <= '9') { n = n * 10 + (p[2] - '0'); digits = 2; }
            if (n == lowest) { memcpy(out + used, value, vl); used += vl; p += 1 + digits; continue; }
        }
        out[used++] = *p++;
    }
    out[used] = 0;
    return out;
}
char* tinox_string_arg_int(const char* s, int64_t value) {
    char buf[32];
    snprintf(buf, sizeof buf, "%lld", (long long)value);
    return tinox_string_arg(s, buf);
}

char* tinox_string_replace(const char* s, const char* from, const char* to) {
    if (!from || !*from) { size_t l = strlen(s); char* r = malloc(l+1); memcpy(r,s,l+1); return r; }
    size_t flen = strlen(from), tlen = strlen(to), slen = strlen(s);
    // Count occurrences
    size_t count = 0;
    const char* p = s;
    while ((p = strstr(p, from)) != NULL) { count++; p += flen; }
    if (count == 0) { char* r = malloc(slen+1); memcpy(r,s,slen+1); return r; }
    size_t rlen = slen + count * (tlen - flen);
    char* result = malloc(rlen + 1);
    char* out = result;
    p = s;
    const char* found;
    while ((found = strstr(p, from)) != NULL) {
        size_t pre = (size_t)(found - p);
        memcpy(out, p, pre); out += pre;
        memcpy(out, to, tlen); out += tlen;
        p = found + flen;
    }
    size_t rest = strlen(p);
    memcpy(out, p, rest);
    out[rest] = '\0';
    return result;
}

// ---- String split / Array join ----

int64_t* tinox_string_split(const char* str, const char* delim) {
    size_t dlen = strlen(delim);
    size_t count = 1;
    if (dlen > 0) {
        const char* p = str;
        while ((p = strstr(p, delim)) != NULL) { count++; p += dlen; }
    } else {
        count = strlen(str);
        if (count == 0) count = 1;
    }
    int64_t* nh = tinox_array_new((int64_t)count, 0);
    int64_t* nd = ((TinoxArray*)nh)->data;
    if (dlen == 0) {
        for (size_t i = 0; i < count; i++) {
            char* s = (char*)malloc(2);
            s[0] = str[i]; s[1] = '\0';
            nd[i] = (int64_t)(uintptr_t)s;
        }
        return nh;
    }
    size_t i = 0;
    const char* start = str;
    const char* found;
    while ((found = strstr(start, delim)) != NULL) {
        size_t plen = (size_t)(found - start);
        char* part = (char*)malloc(plen + 1);
        memcpy(part, start, plen); part[plen] = '\0';
        nd[i++] = (int64_t)(uintptr_t)part;
        start = found + dlen;
    }
    size_t plen = strlen(start);
    char* part = (char*)malloc(plen + 1);
    memcpy(part, start, plen); part[plen] = '\0';
    nd[i] = (int64_t)(uintptr_t)part;
    return nh;
}

char* tinox_string_join(int64_t* h, const char* sep) {
    TinoxArray* a = (TinoxArray*)h;
    int64_t* arr = a->data;
    int64_t len = a->len;
    if (len == 0) { char* r = (char*)malloc(1); r[0] = '\0'; return r; }
    size_t seplen = strlen(sep);
    size_t total = 0;
    for (int64_t i = 0; i < len; i++) {
        const char* s = (const char*)(uintptr_t)arr[i];
        total += strlen(s);
        if (i < len - 1) total += seplen;
    }
    char* result = (char*)malloc(total + 1);
    char* p = result;
    for (int64_t i = 0; i < len; i++) {
        const char* s = (const char*)(uintptr_t)arr[i];
        size_t slen = strlen(s);
        memcpy(p, s, slen); p += slen;
        if (i < len - 1) { memcpy(p, sep, seplen); p += seplen; }
    }
    *p = '\0';
    return result;
}

// ---- File I/O ----

void* tinox_file_open(const char* path, const char* mode) {
    FILE* f = fopen(path, mode);
    return (void*)f;
}

void tinox_file_close(void* handle) {
    if (handle) fclose((FILE*)handle);
}

char* tinox_file_read(void* handle) {
    if (!handle) return (char*)tinox_alloc(1);
    FILE* f = (FILE*)handle;
    fseek(f, 0, SEEK_END);
    long size = ftell(f);
    fseek(f, 0, SEEK_SET);
    char* buf = (char*)tinox_alloc(size + 1);
    fread(buf, 1, size, f);
    buf[size] = '\0';
    return buf;
}

char* tinox_file_readline(void* handle) {
    if (!handle) return (char*)tinox_alloc(1);
    FILE* f = (FILE*)handle;
    size_t cap = 256;
    char* buf = (char*)tinox_alloc(cap);
    size_t len = 0;
    int c;
    while ((c = fgetc(f)) != EOF && c != '\n') {
        if (len + 1 >= cap) {
            cap *= 2;
            char* nb = (char*)tinox_alloc(cap);
            memcpy(nb, buf, len);
            free(buf);
            buf = nb;
        }
        buf[len++] = (char)c;
    }
    buf[len] = '\0';
    return buf;
}

void tinox_file_write(void* handle, const char* s) {
    if (handle) fputs(s, (FILE*)handle);
}

int64_t tinox_file_eof(void* handle) {
    if (!handle) return 1;
    return feof((FILE*)handle) ? 1 : 0;
}

int64_t tinox_file_exists(const char* path) {
    FILE* f = fopen(path, "r");
    if (f) { fclose(f); return 1; }
    return 0;
}

void tinox_file_delete(const char* path) {
    remove(path);
}

// ---- High-level file I/O (used by Tinox builtins) ----

char* fileReadAllText(const char* path) {
    FILE* f = fopen(path, "rb");
    if (!f) return GC_strdup("");
    // Try seek-based size detection first (works for regular files)
    if (fseek(f, 0, SEEK_END) == 0) {
        long size = ftell(f);
        if (size > 0) {
            fseek(f, 0, SEEK_SET);
            char* buf = (char*)GC_malloc(size + 1);
            size_t got = fread(buf, 1, size, f);
            fclose(f);
            buf[got] = '\0';
            return buf;
        }
        fseek(f, 0, SEEK_SET);
    }
    // Fall back to dynamic read (for pipes, /dev/stdin, character devices)
    size_t capacity = 4096;
    size_t used = 0;
    char* buf = (char*)GC_malloc(capacity);
    size_t n;
    while ((n = fread(buf + used, 1, capacity - used - 1, f)) > 0) {
        used += n;
        if (used + 1 >= capacity) {
            capacity *= 2;
            char* newbuf = (char*)GC_malloc(capacity);
            memcpy(newbuf, buf, used);
            buf = newbuf;
        }
    }
    fclose(f);
    buf[used] = '\0';
    return buf;
}

void fileWriteAllText(const char* path, const char* content) {
    FILE* f = fopen(path, "w");
    if (f) { fputs(content, f); fclose(f); }
}

// Checked variant: returns "" on success, otherwise "<path>: <strerror>".
static char* tinox_fs_err(const char* what, const char* path) {
    char tmp[64];
    snprintf(tmp, sizeof(tmp), "%s", strerror(errno));
    size_t n = strlen(what) + strlen(path) + strlen(tmp) + 8;
    char* out = (char*)GC_malloc(n);
    snprintf(out, n, "%s %s: %s", what, path, tmp);
    return out;
}

char* fileWriteAllTextChecked(const char* path, const char* content) {
    FILE* f = fopen(path, "w");
    if (!f) return tinox_fs_err("cannot write", path);
    int bad = fputs(content, f) < 0;
    if (fclose(f) != 0) bad = 1;
    if (bad) return tinox_fs_err("cannot write", path);
    return (char*)"";
}

void fileAppendText(const char* path, const char* content) {
    FILE* f = fopen(path, "a");
    if (f) { fputs(content, f); fclose(f); }
}

void fileClose(void* handle) {
    if (handle) fclose((FILE*)handle);
}

char* tinoxIoCause(int64_t code) { return GC_strdup(code ? strerror((int)code) : ""); }

// Additive recoverable I/O ABI (#306). Each call owns its error snapshot:
// [errno, operation, target, cause, optional value]. No global last-error state.
static int64_t* tinox_io_outcome(int code, const char* operation,
                                 const char* target, const char* value) {
    int64_t* out = tinox_array_new(0, 5);
    char number[32];
    snprintf(number, sizeof(number), "%d", code);
    tinox_array_push(out, (int64_t)GC_strdup(number));
    tinox_array_push(out, (int64_t)GC_strdup(operation));
    tinox_array_push(out, (int64_t)GC_strdup(target));
    tinox_array_push(out, (int64_t)GC_strdup(code ? strerror(code) : ""));
    if (!code && value) tinox_array_push(out, (int64_t)value);
    return out;
}

int64_t* fsReadTextOutcome(const char* path) {
    FILE* file = fopen(path, "rb");
    if (!file) return tinox_io_outcome(errno, "read", path, NULL);
    size_t capacity = 4096, used = 0;
    char* text = GC_malloc_atomic(capacity);
    int error = 0;
    for (;;) {
        errno = 0;
        size_t got = fread(text + used, 1, capacity - used - 1, file);
        if (memchr(text + used, 0, got)) error = EILSEQ;
        used += got;
        if (ferror(file)) error = errno ? errno : EIO;
        if (error || feof(file)) break;
        if (used == capacity - 1) {
            if (capacity > SIZE_MAX / 2) { error = EOVERFLOW; break; }
            capacity *= 2;
            text = GC_realloc(text, capacity);
        }
    }
    if (fclose(file) != 0 && !error) error = errno ? errno : EIO;
    text[used] = 0;
    // A partial read never exposes a success payload.
    return tinox_io_outcome(error, "read", path, error ? NULL : text);
}

int64_t* fsWriteTextOutcome(const char* path, const char* text, int64_t append) {
    const char* operation = append ? "append" : "write";
    FILE* file = fopen(path, append ? "ab" : "wb");
    if (!file) return tinox_io_outcome(errno, operation, path, NULL);
    size_t size = strlen(text);
    int error = 0;
    if (fwrite(text, 1, size, file) != size) error = errno ? errno : EIO;
    if (fclose(file) != 0 && !error) error = errno ? errno : EIO;
    return tinox_io_outcome(error, operation, path, NULL);
}

int64_t* fsCopyOutcome(const char* from, const char* to) {
    FILE* source = fopen(from, "rb");
    if (!source) return tinox_io_outcome(errno, "copy.read", from, NULL);
    // Open without truncation first: aliases/hard links must not destroy input.
    int fd = open(to, O_WRONLY | O_CREAT | O_CLOEXEC, 0666);
    int error = fd < 0 ? errno : 0;
    const char* target = to;
    const char* operation = "copy.write";
    struct stat input, output;
    if (!error && (fstat(fileno(source), &input) || fstat(fd, &output))) error = errno;
    if (!error && input.st_dev == output.st_dev && input.st_ino == output.st_ino) error = EINVAL;
    if (!error && S_ISREG(output.st_mode) && ftruncate(fd, 0)) error = errno;
    FILE* destination = NULL;
    if (!error) {
        destination = fdopen(fd, "wb");
        if (!destination) error = errno;
    }
    if (!destination && fd >= 0) close(fd);
    unsigned char buffer[65536];
    while (!error) {
        errno = 0;
        size_t got = fread(buffer, 1, sizeof(buffer), source);
        if (ferror(source)) {
            error = errno ? errno : EIO; target = from; operation = "copy.read"; break;
        }
        if (got && fwrite(buffer, 1, got, destination) != got) { error = errno ? errno : EIO; break; }
        if (feof(source)) break;
    }
    if (destination && fclose(destination) && !error) error = errno ? errno : EIO;
    if (fclose(source) && !error) {
        error = errno ? errno : EIO; target = from; operation = "copy.read";
    }
    return tinox_io_outcome(error, operation, target, NULL);
}

int64_t* fsMutateOutcome(const char* from, const char* to, int64_t move) {
    int status = move ? rename(from, to) : unlink(from);
    return tinox_io_outcome(status ? errno : 0, move ? "move" : "delete", from, NULL);
}

// ---- Byte-exact file/stdin I/O ----
// fileReadAllText/fileWriteAllText above are strlen/fputs-based char* --
// silently truncate at an embedded 0x00 byte (same reason tinox.core.zip
// documents itself as "text content only"). These three operate on
// List<Int64> (one element per byte, 0-255) end to end instead, using the
// same TinoxArray ABI tinoxDeflateRaw/tinoxGzip already use below, so no
// NUL-related truncation is possible anywhere in the read/write path.
// Not registered as compiler builtins -- consumed via `extern fn` in
// Tinox source, same pattern tinox.core.compress/zip already use for
// their own runtime primitives.

int64_t* tinoxFileReadAllBytes(const char* path) {
    FILE* f = fopen(path, "rb");
    if (!f) {
        fprintf(stderr, "runtime error: cannot open file for reading: %s\n", path);
        exit(1);
    }
    if (fseek(f, 0, SEEK_END) == 0) {
        long size = ftell(f);
        if (size >= 0) {
            fseek(f, 0, SEEK_SET);
            unsigned char* raw = (unsigned char*)malloc(size > 0 ? (size_t)size : 1);
            size_t got = fread(raw, 1, (size_t)size, f);
            fclose(f);
            int64_t* h = tinox_array_new((int64_t)got, (int64_t)got);
            TinoxArray* a = (TinoxArray*)h;
            for (size_t i = 0; i < got; i++) a->data[i] = (int64_t)raw[i];
            free(raw);
            return h;
        }
        fseek(f, 0, SEEK_SET);
    }
    // Fallback for pipes/character devices, where SEEK_END doesn't work.
    size_t cap = 65536, used = 0;
    unsigned char* raw = (unsigned char*)malloc(cap);
    size_t n;
    while ((n = fread(raw + used, 1, cap - used, f)) > 0) {
        used += n;
        if (used == cap) {
            cap *= 2;
            unsigned char* nb = (unsigned char*)malloc(cap);
            memcpy(nb, raw, used);
            free(raw);
            raw = nb;
        }
    }
    fclose(f);
    int64_t* h = tinox_array_new((int64_t)used, (int64_t)used);
    TinoxArray* a = (TinoxArray*)h;
    for (size_t i = 0; i < used; i++) a->data[i] = (int64_t)raw[i];
    free(raw);
    return h;
}

void tinoxFileWriteAllBytes(const char* path, int64_t* bytes) {
    TinoxArray* a = (TinoxArray*)bytes;
    FILE* f = fopen(path, "wb");
    if (!f) {
        fprintf(stderr, "runtime error: cannot open file for writing: %s\n", path);
        exit(1);
    }
    int64_t len = a->len;
    if (len > 0) {
        unsigned char* raw = (unsigned char*)malloc((size_t)len);
        for (int64_t i = 0; i < len; i++) raw[i] = (unsigned char)(a->data[i] & 0xFF);
        fwrite(raw, 1, (size_t)len, f);
        free(raw);
    }
    fclose(f);
}

int64_t* tinoxStdinReadAllBytes(void) {
    size_t cap = 65536, used = 0;
    unsigned char* raw = (unsigned char*)malloc(cap);
    size_t n;
    while ((n = fread(raw + used, 1, cap - used, stdin)) > 0) {
        used += n;
        if (used == cap) {
            cap *= 2;
            unsigned char* nb = (unsigned char*)malloc(cap);
            memcpy(nb, raw, used);
            free(raw);
            raw = nb;
        }
    }
    int64_t* h = tinox_array_new((int64_t)used, (int64_t)used);
    TinoxArray* a = (TinoxArray*)h;
    for (size_t i = 0; i < used; i++) a->data[i] = (int64_t)raw[i];
    free(raw);
    return h;
}

// ---- Streaming byte-exact file I/O ----
// The *AllBytes primitives above materialize a whole file as one
// List<Int64> -- 8 bytes of heap per file byte, so a multi-GB input needs
// tens of GB of RAM. These operate on an open handle (the FILE* as an
// Int64, 0 = open failed) in caller-sized chunks instead, so memory stays
// bounded by the chunk size regardless of file size. Every I/O error
// (short write, read error, failed flush on close) is a hard runtime
// error, never a silently truncated file. Consumed via `extern fn`, same
// as the primitives above.

int64_t tinoxFileOpenBytes(const char* path, const char* mode) {
    FILE* f = fopen(path, mode);
    return (int64_t)(intptr_t)f;
}

// Reads up to maxBytes; returns fewer only at EOF (an empty list = EOF).
int64_t* tinoxFileReadChunk(int64_t handle, int64_t maxBytes) {
    FILE* f = (FILE*)(intptr_t)handle;
    if (!f) {
        fprintf(stderr, "runtime error: tinoxFileReadChunk on a closed/invalid handle\n");
        exit(1);
    }
    if (maxBytes < 0) maxBytes = 0;
    unsigned char* raw = (unsigned char*)malloc(maxBytes > 0 ? (size_t)maxBytes : 1);
    size_t used = 0;
    while (used < (size_t)maxBytes) {
        size_t n = fread(raw + used, 1, (size_t)maxBytes - used, f);
        if (n == 0) break;
        used += n;
    }
    if (ferror(f)) {
        free(raw);
        fprintf(stderr, "runtime error: read error: %s\n", strerror(errno));
        exit(1);
    }
    int64_t* h = tinox_array_new((int64_t)used, (int64_t)used);
    TinoxArray* a = (TinoxArray*)h;
    for (size_t i = 0; i < used; i++) a->data[i] = (int64_t)raw[i];
    free(raw);
    return h;
}

void tinoxFileWriteChunk(int64_t handle, int64_t* bytes) {
    FILE* f = (FILE*)(intptr_t)handle;
    if (!f) {
        fprintf(stderr, "runtime error: tinoxFileWriteChunk on a closed/invalid handle\n");
        exit(1);
    }
    TinoxArray* a = (TinoxArray*)bytes;
    int64_t len = a->len;
    if (len <= 0) return;
    unsigned char* raw = (unsigned char*)malloc((size_t)len);
    for (int64_t i = 0; i < len; i++) raw[i] = (unsigned char)(a->data[i] & 0xFF);
    size_t wrote = fwrite(raw, 1, (size_t)len, f);
    free(raw);
    if (wrote != (size_t)len) {
        fprintf(stderr, "runtime error: write error: %s\n", strerror(errno));
        exit(1);
    }
}

// Skips n bytes forward; returns false if the seek failed or would land
// past EOF (e.g. a truncated file).
bool tinoxFileSkipBytes(int64_t handle, int64_t n) {
    FILE* f = (FILE*)(intptr_t)handle;
    if (!f || n < 0) return false;
    off_t cur = ftello(f);
    if (cur < 0 || fseeko(f, 0, SEEK_END) != 0) return false;
    off_t end = ftello(f);
    if (end < 0 || (int64_t)(end - cur) < n) {
        fseeko(f, cur, SEEK_SET);
        return false;
    }
    return fseeko(f, cur + (off_t)n, SEEK_SET) == 0;
}

void tinoxFileCloseBytes(int64_t handle) {
    FILE* f = (FILE*)(intptr_t)handle;
    if (!f) return;
    if (fclose(f) != 0) {
        fprintf(stderr, "runtime error: error closing file: %s\n", strerror(errno));
        exit(1);
    }
}

// ---- Compact byte buffers (issue #311) ----
// A buffer handle (carried as an Int64 in Tinox source, wrapped by
// tinox.core.bytes.Bytes) points at ONE pointer-free GC block:
//   [ int64 len | len payload bytes ]
// i.e. 1 byte of heap per logical byte (+8 header), versus 8 bytes per
// byte for List<Int64>. The whole block is allocated with
// GC_malloc_atomic, which is correct because it never contains a pointer
// (the length is a plain integer), so the collector never has to scan the
// payload. The block is never resized in place: slice/concat copy, so a
// handle's length is immutable and every access is checked against it.
// Embedded 0x00 bytes are ordinary payload -- nothing here is strlen-based
// except the explicit String adapters, which are documented as such.
// Every misuse (out-of-range index/slice, invalid length, I/O error,
// converting a buffer with a NUL to a String) is a hard runtime error,
// never a silent truncation or clamp.
// Binary-safe HTTP bodies (#311). The request/response body fields stay plain
// NUL-terminated C strings for every existing API; these side channels let
// tinox.core.bytes recover the exact byte length of the CURRENT request body
// and let a handler hand the writer a byte buffer with an explicit length.
// Identity of the pointer is the guard: a stale/unrelated String never matches.
static __thread const char* g_req_body_ptr = NULL;
static __thread int64_t     g_req_body_len = 0;
// Exact byte length of the latest byte-to-String/HTTP3 body adapter. This
// does not extend Bytes.fromRequestBody's plain-TCP identity contract.
static __thread const char* g_json_body_ptr = NULL;
static __thread size_t g_json_body_len = 0;
static __thread const char* g_resp_bytes_ptr = NULL;
static __thread int64_t     g_resp_bytes_len = 0;

typedef struct {
    int64_t len;
    unsigned char data[];
} TinoxBuf;

#define TINOX_BUF_MAX ((int64_t)1 << 40)

static TinoxBuf* tinox_buf_alloc(int64_t len) {
    if (len < 0 || len > TINOX_BUF_MAX) {
        fprintf(stderr, "runtime error: byte buffer length %lld is invalid\n", (long long)len);
        exit(1);
    }
    TinoxBuf* b = (TinoxBuf*)GC_malloc_atomic(sizeof(TinoxBuf) + (size_t)len + 1);
    if (!b) {
        fprintf(stderr, "runtime error: out of memory allocating %lld byte buffer\n", (long long)len);
        exit(1);
    }
    b->len = len;
    b->data[len] = 0;
    return b;
}

static TinoxBuf* tinox_buf_of(int64_t h, const char* op) {
    if (h == 0) {
        fprintf(stderr, "runtime error: %s on a null byte buffer\n", op);
        exit(1);
    }
    return (TinoxBuf*)(intptr_t)h;
}

int64_t tinoxBufNew(int64_t len) {
    TinoxBuf* b = tinox_buf_alloc(len);
    memset(b->data, 0, (size_t)len);
    return (int64_t)(intptr_t)b;
}

int64_t tinoxBufLen(int64_t h) {
    return tinox_buf_of(h, "length")->len;
}

int64_t tinoxBufGet(int64_t h, int64_t i) {
    TinoxBuf* b = tinox_buf_of(h, "get");
    if (i < 0 || i >= b->len) {
        fprintf(stderr, "runtime error: byte buffer index %lld out of bounds (length %lld)\n",
                (long long)i, (long long)b->len);
        exit(1);
    }
    return (int64_t)b->data[i];
}

void tinoxBufSet(int64_t h, int64_t i, int64_t v) {
    TinoxBuf* b = tinox_buf_of(h, "set");
    if (i < 0 || i >= b->len) {
        fprintf(stderr, "runtime error: byte buffer index %lld out of bounds (length %lld)\n",
                (long long)i, (long long)b->len);
        exit(1);
    }
    if (v < 0 || v > 255) {
        fprintf(stderr, "runtime error: byte value %lld out of range 0..255\n", (long long)v);
        exit(1);
    }
    b->data[i] = (unsigned char)v;
}

// Copy of [start, end). start == end yields an empty buffer.
int64_t tinoxBufSlice(int64_t h, int64_t start, int64_t end) {
    TinoxBuf* b = tinox_buf_of(h, "slice");
    if (start < 0 || end < start || end > b->len) {
        fprintf(stderr, "runtime error: byte buffer slice [%lld, %lld) out of bounds (length %lld)\n",
                (long long)start, (long long)end, (long long)b->len);
        exit(1);
    }
    TinoxBuf* r = tinox_buf_alloc(end - start);
    memcpy(r->data, b->data + start, (size_t)(end - start));
    return (int64_t)(intptr_t)r;
}

// A @Scheduled run finished (issue #536): when the method left an error pending, print it with the
// job name, clear it so the next run starts clean, and return 1. A failing job never stops the
// process; the generated runner counts failures in a metric.
int64_t tinox_scheduled_report(const char* job, int64_t err) {
    if (err == 0) return 0;
    fprintf(stderr, "scheduled job %s failed: %s\n", job, (const char*)(intptr_t)err);
    __tinox_err = 0;
    return 1;
}

// An AMQP consumer handler returned (issue #537): when it left an error pending, print it with the
// handler name, clear it so the next attempt/message starts clean, and return 1. The generated loop
// then applies the failure policy; the delivery is not acknowledged on this path.
int64_t tinox_message_handler_failed(const char* handler, int64_t err) {
    if (err == 0) return 0;
    fprintf(stderr, "message handler %s failed: %s\n", handler, (const char*)(intptr_t)err);
    __tinox_err = 0;
    return 1;
}

// ---- LoadingCache (issue #543): bounded, TTL, thread-safe, single-flight ----
// Entries live in GC memory (values are GC strings). Every operation holds the cache mutex; a miss
// marks the key "loading" and concurrent identical lookups wait for that one loader (bounded by
// TINOX_LC_WAIT_MS) instead of loading again. evictAll bumps a generation so a load that raced with
// the eviction is returned to its caller but not stored -- a stale value never outlives an eviction.
typedef struct { char* key; char* value; int64_t expires; int64_t used; int state; } TinoxLcEntry; // 0 free, 1 ready, 2 loading
typedef struct {
    pthread_mutex_t mu; pthread_cond_t cv;
    int64_t cap, ttl_ms, tick, gen, hits, misses, loads, evictions, failures, waited_out;
    TinoxLcEntry* e;
} TinoxLc;
static __thread char* g_lc_value = NULL;

int64_t tinoxLcNew(int64_t cap, int64_t ttl_ms) {
    if (cap < 1 || cap > 100000 || ttl_ms < 1) {
        fprintf(stderr, "runtime error: LoadingCache needs 1 <= capacity <= 100000 and ttlMs >= 1\n");
        exit(1);
    }
    TinoxLc* c = (TinoxLc*)GC_malloc(sizeof(TinoxLc));
    pthread_mutex_init(&c->mu, NULL);
    pthread_cond_init(&c->cv, NULL);
    c->cap = cap; c->ttl_ms = ttl_ms;
    c->e = (TinoxLcEntry*)GC_malloc(sizeof(TinoxLcEntry) * (size_t)cap);
    return (int64_t)(intptr_t)c;
}

static TinoxLcEntry* tinox_lc_find(TinoxLc* c, const char* key) {
    for (int64_t i = 0; i < c->cap; i++) if (c->e[i].state != 0 && strcmp(c->e[i].key, key) == 0) return &c->e[i];
    return NULL;
}

static int64_t tinox_lc_wait_ms(void) {
    const char* v = getenv("TINOX_LC_WAIT_MS");
    long long n = v && *v ? atoll(v) : 0;
    return n > 0 ? n : 60000;
}

// 1 = hit (value via tinoxLcValue); otherwise the caller is the loader and must call tinoxLcStore or
// tinoxLcAbort with the returned token minus 2 as generation.
int64_t tinoxLcBegin(int64_t h, const char* key) {
    TinoxLc* c = (TinoxLc*)(intptr_t)h;
    struct timespec dl; clock_gettime(CLOCK_REALTIME, &dl);
    int64_t wait = tinox_lc_wait_ms();
    dl.tv_sec += wait / 1000; dl.tv_nsec += (wait % 1000) * 1000000L;
    if (dl.tv_nsec >= 1000000000L) { dl.tv_sec++; dl.tv_nsec -= 1000000000L; }
    pthread_mutex_lock(&c->mu);
    for (;;) {
        TinoxLcEntry* en = tinox_lc_find(c, key);
        int64_t now = tinox_now_ms();
        if (en && en->state == 1 && en->expires > now) {
            en->used = ++c->tick; c->hits++;
            g_lc_value = en->value;
            pthread_mutex_unlock(&c->mu);
            return 1;
        }
        if (en && en->state == 1) { en->state = 0; en->key = NULL; en->value = NULL; en = NULL; c->evictions++; } // expired
        if (!en) {
            // become the loader: take a free slot, or evict the least recently used ready entry
            TinoxLcEntry* slot = NULL;
            for (int64_t i = 0; i < c->cap; i++) if (c->e[i].state == 0) { slot = &c->e[i]; break; }
            if (!slot) {
                for (int64_t i = 0; i < c->cap; i++) if (c->e[i].state == 1 && (!slot || c->e[i].used < slot->used)) slot = &c->e[i];
                if (slot) c->evictions++;
            }
            if (slot) {
                slot->key = GC_strdup(key); slot->value = NULL; slot->state = 2; slot->used = ++c->tick;
                c->misses++;
                int64_t gen = c->gen;
                pthread_mutex_unlock(&c->mu);
                return gen + 2;
            }
            // every slot is another key's in-flight load: wait for one to finish
        }
        if (pthread_cond_timedwait(&c->cv, &c->mu, &dl) != 0) {
            // The loader did not finish in time: load independently rather than waiting forever.
            c->waited_out++; c->misses++;
            int64_t gen = c->gen;
            pthread_mutex_unlock(&c->mu);
            return gen + 2;
        }
    }
}

const char* tinoxLcValue(void) { return g_lc_value ? g_lc_value : ""; }

static void tinox_lc_release(TinoxLc* c, const char* key) {
    TinoxLcEntry* en = tinox_lc_find(c, key);
    if (en && en->state == 2) { en->state = 0; en->key = NULL; en->value = NULL; }
}

void tinoxLcStore(int64_t h, const char* key, const char* value, int64_t token) {
    TinoxLc* c = (TinoxLc*)(intptr_t)h;
    pthread_mutex_lock(&c->mu);
    c->loads++;
    TinoxLcEntry* en = tinox_lc_find(c, key);
    if (en && en->state == 2 && token - 2 == c->gen) {
        en->value = GC_strdup(value); en->expires = tinox_now_ms() + c->ttl_ms; en->state = 1; en->used = ++c->tick;
    } else {
        tinox_lc_release(c, key);   // evicted while loading: do not store a possibly stale value
    }
    pthread_cond_broadcast(&c->cv);
    pthread_mutex_unlock(&c->mu);
}

void tinoxLcAbort(int64_t h, const char* key) {
    TinoxLc* c = (TinoxLc*)(intptr_t)h;
    pthread_mutex_lock(&c->mu);
    c->failures++;
    tinox_lc_release(c, key);
    pthread_cond_broadcast(&c->cv);
    pthread_mutex_unlock(&c->mu);
}

static void tinox_lc_drop_ready(TinoxLc* c, const char* only_key) {
    for (int64_t i = 0; i < c->cap; i++) {
        if (c->e[i].state == 1 && (!only_key || strcmp(c->e[i].key, only_key) == 0)) {
            c->e[i].state = 0; c->e[i].key = NULL; c->e[i].value = NULL; c->evictions++;
        }
    }
}

// Drops every ready entry and invalidates loads in flight (their results are returned, not stored).
void tinoxLcEvictAll(int64_t h) {
    TinoxLc* c = (TinoxLc*)(intptr_t)h;
    pthread_mutex_lock(&c->mu);
    c->gen++;
    tinox_lc_drop_ready(c, NULL);
    pthread_mutex_unlock(&c->mu);
}

void tinoxLcEvict(int64_t h, const char* key) {
    TinoxLc* c = (TinoxLc*)(intptr_t)h;
    pthread_mutex_lock(&c->mu);
    c->gen++;   // a load of this key in flight must not store its (older) result
    tinox_lc_drop_ready(c, key);
    pthread_mutex_unlock(&c->mu);
}

int64_t tinoxLcStat(int64_t h, int64_t which) {
    TinoxLc* c = (TinoxLc*)(intptr_t)h;
    pthread_mutex_lock(&c->mu);
    int64_t r = 0;
    switch (which) {
        case 0: r = c->hits; break;
        case 1: r = c->misses; break;
        case 2: r = c->loads; break;
        case 3: r = c->evictions; break;
        case 4: r = c->failures; break;
        case 5: r = c->waited_out; break;
        case 6: for (int64_t i = 0; i < c->cap; i++) if (c->e[i].state == 1) r++; break;
    }
    pthread_mutex_unlock(&c->mu);
    return r;
}

// Registry behind @Cacheable / @CacheEvict (issue #543): one LoadingCache per annotated method, found by
// its "Class.method" name. Creation is serialized; the handle of a name never changes.
#define TINOX_CACHE_REGISTRY_MAX 256
static struct { char* name; int64_t handle; } g_cache_registry[TINOX_CACHE_REGISTRY_MAX];
static int g_cache_registry_n = 0;
static pthread_mutex_t g_cache_registry_mu = PTHREAD_MUTEX_INITIALIZER;

int64_t tinox_cache_named(const char* name, int64_t capacity, int64_t ttl_seconds) {
    pthread_mutex_lock(&g_cache_registry_mu);
    for (int i = 0; i < g_cache_registry_n; i++) {
        if (strcmp(g_cache_registry[i].name, name) == 0) {
            int64_t h = g_cache_registry[i].handle;
            pthread_mutex_unlock(&g_cache_registry_mu);
            return h;
        }
    }
    if (g_cache_registry_n == TINOX_CACHE_REGISTRY_MAX) {
        pthread_mutex_unlock(&g_cache_registry_mu);
        fprintf(stderr, "runtime error: more than %d @Cacheable methods\n", TINOX_CACHE_REGISTRY_MAX);
        exit(1);
    }
    int64_t h = tinoxLcNew(capacity, ttl_seconds * 1000);
    g_cache_registry[g_cache_registry_n].name = strdup(name);
    g_cache_registry[g_cache_registry_n].handle = h;
    g_cache_registry_n++;
    pthread_mutex_unlock(&g_cache_registry_mu);
    return h;
}
int64_t tinox_cache_begin(int64_t h, const char* key) { return tinoxLcBegin(h, key); }
const char* tinox_cache_value(void) { return tinoxLcValue(); }
void tinox_cache_store(int64_t h, const char* key, const char* value, int64_t token) { tinoxLcStore(h, key, value, token); }
void tinox_cache_abort(int64_t h, const char* key) { tinoxLcAbort(h, key); }
// @CacheEvict: a cache that was never used holds nothing, so an unknown name is not an error.
void tinox_cache_evict_named(const char* name) {
    int64_t h = 0;
    pthread_mutex_lock(&g_cache_registry_mu);
    for (int i = 0; i < g_cache_registry_n; i++) if (strcmp(g_cache_registry[i].name, name) == 0) h = g_cache_registry[i].handle;
    pthread_mutex_unlock(&g_cache_registry_mu);
    if (h) tinoxLcEvictAll(h);
}

// First index >= from where needle occurs in the haystack, or -1. An empty needle is a hard error.
int64_t tinoxBufFind(int64_t h, int64_t n, int64_t from) {
    TinoxBuf* hay = tinox_buf_of(h, "find");
    TinoxBuf* needle = tinox_buf_of(n, "find");
    if (needle->len == 0 || from < 0) {
        fprintf(stderr, "runtime error: byte buffer find needs a non-empty needle and a non-negative start\n");
        exit(1);
    }
    if (from > hay->len || needle->len > hay->len - from) return -1;
    const unsigned char* at = memmem(hay->data + from, (size_t)(hay->len - from), needle->data, (size_t)needle->len);
    return at ? (int64_t)(at - hay->data) : -1;
}

int64_t tinoxBufConcat(int64_t a, int64_t c) {
    TinoxBuf* x = tinox_buf_of(a, "concat");
    TinoxBuf* y = tinox_buf_of(c, "concat");
    TinoxBuf* r = tinox_buf_alloc(x->len + y->len);
    memcpy(r->data, x->data, (size_t)x->len);
    memcpy(r->data + x->len, y->data, (size_t)y->len);
    return (int64_t)(intptr_t)r;
}

bool tinoxBufEquals(int64_t a, int64_t c) {
    TinoxBuf* x = tinox_buf_of(a, "equals");
    TinoxBuf* y = tinox_buf_of(c, "equals");
    return x->len == y->len && memcmp(x->data, y->data, (size_t)x->len) == 0;
}

// Copies the String's UTF-8/char bytes (strlen of a Tinox String, so a
// String itself can never carry an embedded NUL).
int64_t tinoxBufFromString(const char* s) {
    size_t n = s ? strlen(s) : 0;
    TinoxBuf* r = tinox_buf_alloc((int64_t)n);
    if (n) memcpy(r->data, s, n);
    return (int64_t)(intptr_t)r;
}

// A Tinox String is NUL-terminated, so a buffer with an embedded 0x00
// cannot be represented as one: that is a hard error, not a truncation.
char* tinoxBufToString(int64_t h) {
    TinoxBuf* b = tinox_buf_of(h, "toString");
    if (b->len > 0 && memchr(b->data, 0, (size_t)b->len) != NULL) {
        fprintf(stderr, "runtime error: byte buffer contains an embedded NUL byte and cannot be converted to a String\n");
        exit(1);
    }
    char* s = (char*)GC_malloc_atomic((size_t)b->len + 1);
    memcpy(s, b->data, (size_t)b->len);
    s[b->len] = '\0';
    return s;
}

// HTTP/1.1 server body bridge. Only valid inside a request handler of the
// plain-TCP server (tinox_handle_one); anywhere else is a hard error rather
// than a guess. The request body arrives as a String whose bytes after the
// first NUL are invisible to String APIs -- this recovers the exact payload.
int64_t tinoxBufFromRequestBody(const char* body) {
    if (!body || body != g_req_body_ptr) {
        fprintf(stderr, "runtime error: Bytes.fromRequestBody requires the body of the request currently being handled\n");
        exit(1);
    }
    TinoxBuf* r = tinox_buf_alloc(g_req_body_len);
    memcpy(r->data, body, (size_t)g_req_body_len);
    return (int64_t)(intptr_t)r;
}

// Returns a body "String" that points at the buffer payload and registers the
// exact length for the response writer (which would otherwise strlen and
// truncate at the first 0x00). The pointer is only honoured when it is still
// the response body at write time.
char* tinoxBufAsResponseBody(int64_t h) {
    TinoxBuf* b = tinox_buf_of(h, "asResponseBody");
    g_resp_bytes_ptr = (const char*)b->data;
    g_resp_bytes_len = b->len;
    return (char*)b->data;
}

// Length-aware protocol bridges. Legacy frame parsers retain their lists;
// these adapters copy once at the application boundary, preserving all bytes.
char* tinoxHttpRequestBodyFromBytes(int64_t* list) {
    TinoxArray* bytes = (TinoxArray*)list;
    char* body = GC_malloc_atomic((size_t)bytes->len + 1);
    for (int64_t i = 0; i < bytes->len; i++) body[i] = (char)bytes->data[i];
    body[bytes->len] = 0;
    g_req_body_ptr = body; g_req_body_len = bytes->len;
    return body;
}
void tinoxHttpClearBodyBridge(void) {
    g_req_body_ptr = NULL; g_req_body_len = 0;
    g_resp_bytes_ptr = NULL; g_resp_bytes_len = 0;
}
int64_t tinoxHttpBodyLength(const char* body) {
    if (body && body == g_resp_bytes_ptr) return g_resp_bytes_len;
    return body ? (int64_t)strlen(body) : 0;
}
int64_t* tinoxHttpBodySlice(const char* body, int64_t start, int64_t end) {
    int64_t size = tinoxHttpBodyLength(body);
    if (start < 0 || end < start || end > size) {
        fprintf(stderr, "runtime error: invalid HTTP body slice\n"); exit(1);
    }
    int64_t* out = tinox_array_new(0, end - start);
    for (int64_t i = start; i < end; i++) tinox_array_push(out, (unsigned char)body[i]);
    return out;
}

// Explicit adapters to/from the legacy List<Int64> (one element per byte).
int64_t tinoxBufFromList(int64_t* list) {
    TinoxArray* a = (TinoxArray*)list;
    int64_t n = a ? a->len : 0;
    TinoxBuf* r = tinox_buf_alloc(n);
    for (int64_t i = 0; i < n; i++) {
        int64_t v = a->data[i];
        if (v < 0 || v > 255) {
            fprintf(stderr, "runtime error: list element %lld has value %lld, outside byte range 0..255\n",
                    (long long)i, (long long)v);
            exit(1);
        }
        r->data[i] = (unsigned char)v;
    }
    return (int64_t)(intptr_t)r;
}

int64_t* tinoxBufToList(int64_t h) {
    TinoxBuf* b = tinox_buf_of(h, "toList");
    int64_t* lh = tinox_array_new(b->len, b->len);
    TinoxArray* a = (TinoxArray*)lh;
    for (int64_t i = 0; i < b->len; i++) a->data[i] = (int64_t)b->data[i];
    return lh;
}

// Whole-file read/write straight into/out of the compact payload: no
// intermediate copy, no per-byte widening.
int64_t tinoxBufReadFile(const char* path) {
    FILE* f = fopen(path, "rb");
    if (!f) {
        fprintf(stderr, "runtime error: cannot open file for reading: %s\n", path);
        exit(1);
    }
    struct stat st;
    if (fstat(fileno(f), &st) == 0 && S_ISREG(st.st_mode)) {
        TinoxBuf* r = tinox_buf_alloc((int64_t)st.st_size);
        size_t got = fread(r->data, 1, (size_t)st.st_size, f);
        if (ferror(f) || got != (size_t)st.st_size) {
            fprintf(stderr, "runtime error: read error on %s\n", path);
            exit(1);
        }
        fclose(f);
        return (int64_t)(intptr_t)r;
    }
    // Pipes/devices: unknown size, grow geometrically.
    size_t cap = 65536, used = 0;
    unsigned char* raw = (unsigned char*)malloc(cap);
    size_t n;
    while ((n = fread(raw + used, 1, cap - used, f)) > 0) {
        used += n;
        if (used == cap) { cap *= 2; raw = (unsigned char*)realloc(raw, cap); }
    }
    if (ferror(f)) {
        fprintf(stderr, "runtime error: read error on %s\n", path);
        exit(1);
    }
    fclose(f);
    TinoxBuf* r = tinox_buf_alloc((int64_t)used);
    memcpy(r->data, raw, used);
    free(raw);
    return (int64_t)(intptr_t)r;
}

void tinoxBufWriteFile(const char* path, int64_t h) {
    TinoxBuf* b = tinox_buf_of(h, "writeFile");
    FILE* f = fopen(path, "wb");
    if (!f) {
        fprintf(stderr, "runtime error: cannot open file for writing: %s\n", path);
        exit(1);
    }
    if (b->len > 0 && fwrite(b->data, 1, (size_t)b->len, f) != (size_t)b->len) {
        fprintf(stderr, "runtime error: write error: %s\n", strerror(errno));
        exit(1);
    }
    if (fclose(f) != 0) {
        fprintf(stderr, "runtime error: error closing file: %s\n", strerror(errno));
        exit(1);
    }
}

// Creates a NEW file (O_EXCL, never follows a symlink, mode 0600) with exactly the buffer's bytes. Returns 0, or the
// errno of the failure; a partially written file is removed. Used for upload spooling (issue #542).
int64_t tinoxBufWriteFileExclusive(const char* path, int64_t h) {
    TinoxBuf* b = tinox_buf_of(h, "writeFileExclusive");
    int fd = open(path, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0600);
    if (fd < 0) return errno;
    int64_t off = 0;
    while (off < b->len) {
        ssize_t n = write(fd, b->data + off, (size_t)(b->len - off));
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) { int e = errno ? errno : EIO; close(fd); unlink(path); return e; }
        off += n;
    }
    if (close(fd) != 0) { int e = errno; unlink(path); return e; }
    return 0;
}

// Streaming counterparts of tinoxFileReadChunk/WriteChunk on the same
// open handle (tinoxFileOpenBytes). An empty buffer = EOF.
int64_t tinoxBufReadChunk(int64_t handle, int64_t maxBytes) {
    FILE* f = (FILE*)(intptr_t)handle;
    if (!f) {
        fprintf(stderr, "runtime error: tinoxBufReadChunk on a closed/invalid handle\n");
        exit(1);
    }
    if (maxBytes < 0) maxBytes = 0;
    TinoxBuf* r = tinox_buf_alloc(maxBytes);
    size_t used = 0;
    while (used < (size_t)maxBytes) {
        size_t n = fread(r->data + used, 1, (size_t)maxBytes - used, f);
        if (n == 0) break;
        used += n;
    }
    if (ferror(f)) {
        fprintf(stderr, "runtime error: read error: %s\n", strerror(errno));
        exit(1);
    }
    r->len = (int64_t)used;
    r->data[used] = 0;
    return (int64_t)(intptr_t)r;
}

void tinoxBufWriteChunk(int64_t handle, int64_t h) {
    FILE* f = (FILE*)(intptr_t)handle;
    if (!f) {
        fprintf(stderr, "runtime error: tinoxBufWriteChunk on a closed/invalid handle\n");
        exit(1);
    }
    TinoxBuf* b = tinox_buf_of(h, "writeChunk");
    if (b->len > 0 && fwrite(b->data, 1, (size_t)b->len, f) != (size_t)b->len) {
        fprintf(stderr, "runtime error: write error: %s\n", strerror(errno));
        exit(1);
    }
}

int64_t tinoxBufStdinReadAll(void) {
    size_t cap = 65536, used = 0;
    unsigned char* raw = (unsigned char*)malloc(cap);
    size_t n;
    while ((n = fread(raw + used, 1, cap - used, stdin)) > 0) {
        used += n;
        if (used == cap) { cap *= 2; raw = (unsigned char*)realloc(raw, cap); }
    }
    TinoxBuf* r = tinox_buf_alloc((int64_t)used);
    memcpy(r->data, raw, used);
    free(raw);
    return (int64_t)(intptr_t)r;
}

// Size of a regular file in bytes, or -1 if it doesn't exist / isn't one.
int64_t tinoxFileSizeBytes(const char* path) {
    struct stat st;
    if (stat(path, &st) != 0 || !S_ISREG(st.st_mode)) return -1;
    return (int64_t)st.st_size;
}

// What a path is, for recursive directory walks:
//   0 = doesn't exist (incl. a dangling symlink), 1 = regular file (or a
//   symlink to one), 2 = directory, 3 = something else (fifo, device,
//   socket), 4 = symlink to a directory, 5 = directory that can't be read.
// Distinguishing 4 from 2 lets a walker avoid following symlinked
// directories (loops); 5 lets it fail loudly instead of silently treating
// an unreadable directory as empty, which is what dirList() does.
int64_t tinoxPathKind(const char* path) {
    struct stat lst, st;
    if (lstat(path, &lst) != 0) return 0;
    if (stat(path, &st) != 0) return 0;
    if (S_ISREG(st.st_mode)) return 1;
    if (S_ISDIR(st.st_mode)) {
        if (S_ISLNK(lst.st_mode)) return 4;
        return access(path, R_OK | X_OK) == 0 ? 2 : 5;
    }
    return 3;
}

static int tinox_cmp_cstr(const void* a, const void* b) {
    return strcmp(*(const char* const*)a, *(const char* const*)b);
}

// Directory entry names (without "." / ".."), sorted bytewise so walks are
// deterministic. Unlike dirList(), an unreadable directory is a hard error.
int64_t* tinoxDirEntriesSorted(const char* path) {
    DIR* d = opendir(path);
    if (!d) {
        fprintf(stderr, "runtime error: cannot read directory %s: %s\n", path, strerror(errno));
        exit(1);
    }
    size_t cap = 32, n = 0;
    char** names = (char**)malloc(cap * sizeof(char*));
    struct dirent* ent;
    while ((ent = readdir(d)) != NULL) {
        if (strcmp(ent->d_name, ".") == 0 || strcmp(ent->d_name, "..") == 0) continue;
        if (n == cap) {
            cap *= 2;
            names = (char**)realloc(names, cap * sizeof(char*));
        }
        names[n++] = GC_strdup(ent->d_name);
    }
    closedir(d);
    qsort(names, n, sizeof(char*), tinox_cmp_cstr);
    int64_t* h = tinox_array_new(0, (int64_t)(n > 0 ? n : 4));
    for (size_t i = 0; i < n; i++) tinox_array_push(h, (int64_t)names[i]);
    free(names);
    return h;
}

// ---- File-system watching (issue #489, Linux inotify) ----
//
// Handles are small indices into a fixed table (the table lives in the
// data segment, so the GC keeps every watcher reachable). One watcher owns
// one inotify instance; `recursive` roots are expanded to one watch per
// directory, and directories created later are added on the fly (their
// existing contents are reported as created, closing the race between the
// mkdir and the new watch). Operations on one watcher are serialised by its
// mutex; the blocking wait in watchPoll does not hold it.
#include <sys/inotify.h>

typedef struct {
    int wd;
    char* path;
    bool recursive;
} tinox_watch_entry;

typedef struct {
    int fd;
    tinox_watch_entry* entries;
    size_t n, cap;
    pthread_mutex_t mu;
} tinox_watcher;

typedef struct {
    const char* kind;
    bool is_dir;
    char* path;
    char* old_path;
} tinox_watch_event;

typedef struct {
    tinox_watch_event* v;
    size_t n, cap;
} tinox_watch_batch;

#define TINOX_WATCHERS_MAX 64
static tinox_watcher* tinox_watchers[TINOX_WATCHERS_MAX];
static pthread_mutex_t tinox_watchers_mu = PTHREAD_MUTEX_INITIALIZER;
static __thread char tinox_watch_err[256];

char* watchLastError(void) { return GC_strdup(tinox_watch_err); }

static tinox_watcher* tinox_watcher_get(int64_t h) {
    tinox_watcher* w = NULL;
    pthread_mutex_lock(&tinox_watchers_mu);
    if (h >= 1 && h <= TINOX_WATCHERS_MAX) w = tinox_watchers[h - 1];
    pthread_mutex_unlock(&tinox_watchers_mu);
    if (!w) {
        fprintf(stderr, "runtime error: invalid or closed file watcher handle\n");
        exit(1);
    }
    return w;
}

// Returns a handle >= 1, or 0 with watchLastError() set.
int64_t watchCreate(void) {
    int fd = inotify_init1(IN_NONBLOCK | IN_CLOEXEC);
    if (fd < 0) {
        snprintf(tinox_watch_err, sizeof tinox_watch_err, "inotify_init1 failed: %s", strerror(errno));
        return 0;
    }
    tinox_watcher* w = (tinox_watcher*)GC_malloc(sizeof(tinox_watcher));
    w->fd = fd;
    pthread_mutex_init(&w->mu, NULL);
    pthread_mutex_lock(&tinox_watchers_mu);
    for (int i = 0; i < TINOX_WATCHERS_MAX; i++) {
        if (!tinox_watchers[i]) {
            tinox_watchers[i] = w;
            pthread_mutex_unlock(&tinox_watchers_mu);
            return i + 1;
        }
    }
    pthread_mutex_unlock(&tinox_watchers_mu);
    close(fd);
    snprintf(tinox_watch_err, sizeof tinox_watch_err, "too many open file watchers (max %d)", TINOX_WATCHERS_MAX);
    return 0;
}

static char* tinox_watch_join(const char* dir, const char* name) {
    size_t dl = strlen(dir), nl = strlen(name);
    char* out = (char*)GC_malloc_atomic(dl + nl + 2);
    memcpy(out, dir, dl);
    size_t o = dl;
    if (nl > 0) {
        if (dl == 0 || dir[dl - 1] != '/') out[o++] = '/';
        memcpy(out + o, name, nl);
        o += nl;
    }
    out[o] = '\0';
    return out;
}

static char* tinox_watch_normalize(const char* path) {
    char* p = GC_strdup(path);
    size_t l = strlen(p);
    while (l > 1 && p[l - 1] == '/') p[--l] = '\0';
    return p;
}

static tinox_watch_entry* tinox_watch_by_wd(tinox_watcher* w, int wd) {
    for (size_t i = 0; i < w->n; i++) if (w->entries[i].wd == wd) return &w->entries[i];
    return NULL;
}

// Adds (or re-labels) one inotify watch. 0 = ok, -1 = failed (message set).
static int tinox_watch_add_one(tinox_watcher* w, const char* path, bool recursive) {
    uint32_t mask = IN_CREATE | IN_DELETE | IN_MODIFY | IN_CLOSE_WRITE | IN_MOVED_FROM |
                    IN_MOVED_TO | IN_DELETE_SELF | IN_MOVE_SELF | IN_EXCL_UNLINK;
    int wd = inotify_add_watch(w->fd, path, mask);
    if (wd < 0) {
        if (errno == ENOSPC) {
            snprintf(tinox_watch_err, sizeof tinox_watch_err,
                     "inotify watch limit reached while watching %s (raise fs.inotify.max_user_watches)", path);
        } else {
            snprintf(tinox_watch_err, sizeof tinox_watch_err, "cannot watch %s: %s", path, strerror(errno));
        }
        return -1;
    }
    tinox_watch_entry* e = tinox_watch_by_wd(w, wd);
    if (!e) {
        if (w->n == w->cap) {
            w->cap = w->cap ? w->cap * 2 : 16;
            w->entries = (tinox_watch_entry*)GC_realloc(w->entries, w->cap * sizeof(tinox_watch_entry));
        }
        e = &w->entries[w->n++];
        e->wd = wd;
    }
    e->path = GC_strdup(path);
    e->recursive = recursive;
    return 0;
}

static int tinox_watch_add_tree(tinox_watcher* w, const char* path, bool recursive) {
    if (tinox_watch_add_one(w, path, recursive) != 0) return -1;
    if (!recursive) return 0;
    struct stat st;
    if (lstat(path, &st) != 0 || !S_ISDIR(st.st_mode)) return 0;
    DIR* d = opendir(path);
    if (!d) return 0;
    struct dirent* ent;
    int rc = 0;
    while (rc == 0 && (ent = readdir(d)) != NULL) {
        if (strcmp(ent->d_name, ".") == 0 || strcmp(ent->d_name, "..") == 0) continue;
        char* child = tinox_watch_join(path, ent->d_name);
        struct stat cst;
        if (lstat(child, &cst) == 0 && S_ISDIR(cst.st_mode)) rc = tinox_watch_add_tree(w, child, true);
    }
    closedir(d);
    return rc;
}

// Returns "" on success or an error message.
char* watchAdd(int64_t h, const char* path, bool recursive) {
    tinox_watcher* w = tinox_watcher_get(h);
    pthread_mutex_lock(&w->mu);
    tinox_watch_err[0] = '\0';
    char* p = tinox_watch_normalize(path);
    int rc = tinox_watch_add_tree(w, p, recursive);
    pthread_mutex_unlock(&w->mu);
    return GC_strdup(rc == 0 ? "" : tinox_watch_err);
}

static bool tinox_watch_under(const char* path, const char* root) {
    size_t rl = strlen(root);
    return strncmp(path, root, rl) == 0 && (path[rl] == '\0' || path[rl] == '/' || (rl == 1 && root[0] == '/'));
}

static void tinox_watch_drop_tree(tinox_watcher* w, const char* root) {
    size_t out = 0;
    for (size_t i = 0; i < w->n; i++) {
        if (tinox_watch_under(w->entries[i].path, root)) {
            inotify_rm_watch(w->fd, w->entries[i].wd);
        } else {
            w->entries[out++] = w->entries[i];
        }
    }
    w->n = out;
}

void watchRemove(int64_t h, const char* path) {
    tinox_watcher* w = tinox_watcher_get(h);
    pthread_mutex_lock(&w->mu);
    tinox_watch_drop_tree(w, tinox_watch_normalize(path));
    pthread_mutex_unlock(&w->mu);
}

void watchClose(int64_t h) {
    tinox_watcher* w = tinox_watcher_get(h);
    pthread_mutex_lock(&tinox_watchers_mu);
    tinox_watchers[h - 1] = NULL;
    pthread_mutex_unlock(&tinox_watchers_mu);
    pthread_mutex_lock(&w->mu);
    close(w->fd);
    w->fd = -1;
    w->n = 0;
    pthread_mutex_unlock(&w->mu);
}

static void tinox_watch_push(tinox_watch_batch* b, const char* kind, bool is_dir, char* path, char* old_path) {
    // Coalesce: an identical event, or a modification of a path that was
    // just created in the same batch, adds no information.
    for (size_t i = 0; i < b->n; i++) {
        tinox_watch_event* e = &b->v[i];
        if (strcmp(e->path, path) != 0) continue;
        if (strcmp(e->kind, kind) == 0) return;
        if (strcmp(kind, "modified") == 0 && strcmp(e->kind, "created") == 0) return;
    }
    if (b->n == b->cap) {
        b->cap = b->cap ? b->cap * 2 : 16;
        b->v = (tinox_watch_event*)GC_realloc(b->v, b->cap * sizeof(tinox_watch_event));
    }
    b->v[b->n++] = (tinox_watch_event){kind, is_dir, path, old_path};
}

// Reports everything already inside a freshly created/moved-in directory.
static void tinox_watch_scan_new(tinox_watcher* w, tinox_watch_batch* b, const char* dir) {
    DIR* d = opendir(dir);
    if (!d) return;
    struct dirent* ent;
    while ((ent = readdir(d)) != NULL) {
        if (strcmp(ent->d_name, ".") == 0 || strcmp(ent->d_name, "..") == 0) continue;
        char* child = tinox_watch_join(dir, ent->d_name);
        struct stat st;
        bool is_dir = lstat(child, &st) == 0 && S_ISDIR(st.st_mode);
        tinox_watch_push(b, "created", is_dir, child, GC_strdup(""));
        if (is_dir) {
            if (tinox_watch_add_one(w, child, true) == 0) tinox_watch_scan_new(w, b, child);
        }
    }
    closedir(d);
}

typedef struct { uint32_t cookie; char* path; bool is_dir; bool used; } tinox_watch_pending;

static void tinox_watch_drain(tinox_watcher* w, tinox_watch_batch* b) {
    char buf[16384] __attribute__((aligned(__alignof__(struct inotify_event))));
    tinox_watch_pending pend[64];
    size_t npend = 0;
    for (;;) {
        ssize_t len = read(w->fd, buf, sizeof buf);
        if (len <= 0) break;
        for (char* ptr = buf; ptr < buf + len;) {
            struct inotify_event* ev = (struct inotify_event*)ptr;
            ptr += sizeof(struct inotify_event) + ev->len;
            if (ev->mask & IN_Q_OVERFLOW) {
                tinox_watch_push(b, "overflow", false, GC_strdup(""), GC_strdup(""));
                continue;
            }
            tinox_watch_entry* dir = tinox_watch_by_wd(w, ev->wd);
            if (ev->mask & IN_IGNORED) {
                if (dir) *dir = w->entries[--w->n];
                continue;
            }
            if (!dir) continue;
            bool is_dir = (ev->mask & IN_ISDIR) != 0;
            char* path = tinox_watch_join(dir->path, ev->len ? ev->name : "");
            bool recursive = dir->recursive;
            if (ev->mask & IN_DELETE_SELF) {
                tinox_watch_push(b, "deleted", true, path, GC_strdup(""));
            } else if (ev->mask & IN_MOVE_SELF) {
                continue;  /* the parent's MOVED_FROM/MOVED_TO describes it */
            } else if (ev->mask & IN_CREATE) {
                tinox_watch_push(b, "created", is_dir, path, GC_strdup(""));
                if (is_dir && recursive && tinox_watch_add_one(w, path, true) == 0) tinox_watch_scan_new(w, b, path);
            } else if (ev->mask & IN_DELETE) {
                tinox_watch_push(b, "deleted", is_dir, path, GC_strdup(""));
            } else if (ev->mask & (IN_MODIFY | IN_CLOSE_WRITE)) {
                tinox_watch_push(b, "modified", is_dir, path, GC_strdup(""));
            } else if (ev->mask & IN_MOVED_FROM) {
                if (npend < 64) pend[npend++] = (tinox_watch_pending){ev->cookie, path, is_dir, false};
                else tinox_watch_push(b, "deleted", is_dir, path, GC_strdup(""));
            } else if (ev->mask & IN_MOVED_TO) {
                tinox_watch_pending* from = NULL;
                for (size_t i = 0; i < npend; i++) if (!pend[i].used && pend[i].cookie == ev->cookie) { from = &pend[i]; break; }
                if (from) {
                    from->used = true;
                    tinox_watch_push(b, "moved", is_dir, path, from->path);
                    if (is_dir) {
                        /* Re-label the watches below the old path. */
                        size_t ol = strlen(from->path);
                        for (size_t i = 0; i < w->n; i++) {
                            if (tinox_watch_under(w->entries[i].path, from->path)) {
                                w->entries[i].path = tinox_watch_join(path, w->entries[i].path + ol);
                            }
                        }
                    }
                } else {
                    tinox_watch_push(b, "created", is_dir, path, GC_strdup(""));
                    if (is_dir && recursive && tinox_watch_add_one(w, path, true) == 0) tinox_watch_scan_new(w, b, path);
                }
            }
        }
    }
    for (size_t i = 0; i < npend; i++) {
        if (pend[i].used) continue;
        tinox_watch_push(b, "deleted", pend[i].is_dir, pend[i].path, GC_strdup(""));
        if (pend[i].is_dir) tinox_watch_drop_tree(w, pend[i].path);
    }
}

// Waits up to timeoutMs (< 0: forever, 0: poll) for the first event, then
// keeps collecting until the stream has been quiet for debounceMs (bounded
// at one extra second). Returns a flat list of 4 strings per event:
// kind ("created"|"modified"|"deleted"|"moved"|"overflow"), "1"/"0" for
// is-directory, path, old path ("" unless moved).
int64_t* watchPoll(int64_t h, int64_t timeout_ms, int64_t debounce_ms) {
    tinox_watcher* w = tinox_watcher_get(h);
    struct pollfd pfd = {.fd = w->fd, .events = POLLIN};
    int pr;
    do { pr = poll(&pfd, 1, timeout_ms < 0 ? -1 : (int)timeout_ms); } while (pr < 0 && errno == EINTR);
    int64_t* out = tinox_array_new(0, 16);
    if (pr <= 0) return out;
    tinox_watch_batch b = {0};
    pthread_mutex_lock(&w->mu);
    if (w->fd < 0) { pthread_mutex_unlock(&w->mu); return out; }
    tinox_watch_drain(w, &b);
    int64_t extra = 0;
    while (debounce_ms > 0 && extra < 1000) {
        pthread_mutex_unlock(&w->mu);
        do { pr = poll(&pfd, 1, (int)debounce_ms); } while (pr < 0 && errno == EINTR);
        pthread_mutex_lock(&w->mu);
        if (pr <= 0 || w->fd < 0) break;
        tinox_watch_drain(w, &b);
        extra += debounce_ms;
    }
    pthread_mutex_unlock(&w->mu);
    for (size_t i = 0; i < b.n; i++) {
        tinox_array_push(out, (int64_t)GC_strdup(b.v[i].kind));
        tinox_array_push(out, (int64_t)GC_strdup(b.v[i].is_dir ? "1" : "0"));
        tinox_array_push(out, (int64_t)b.v[i].path);
        tinox_array_push(out, (int64_t)b.v[i].old_path);
    }
    return out;
}

// Canonical absolute path (symlinks resolved), or "" if it can't be
// resolved (e.g. doesn't exist).
char* tinoxRealPath(const char* path) {
    char buf[PATH_MAX];
    if (!realpath(path, buf)) return GC_strdup("");
    return GC_strdup(buf);
}

int64_t tinoxCpuCount(void) {
    return tinox_limits()->cpus;
}

// Unlike printStderr, no trailing newline -- for in-place "\r" progress
// lines. Only meaningful on a terminal; callers check tinoxStderrIsTty().
void tinoxStderrWrite(const char* msg) {
    fputs(msg, stderr);
    fflush(stderr);
}

bool tinoxStderrIsTty(void) {
    return isatty(2) != 0;
}

// ---- Socket builtins (tinox.core.socket) ----
// Handles are raw fds as i64; -1 = error. Blocking BSD sockets —
// deliberately kept simple (no epoll here; the HTTP server further down
// has its own epoll machinery).

#include <netdb.h>

int64_t socketCreateTcp(void) {
    return (int64_t)socket(AF_INET, SOCK_STREAM, 0);
}

int64_t socketCreateUdp(void) {
    return (int64_t)socket(AF_INET, SOCK_DGRAM, 0);
}

bool socketConnect(int64_t fd, const char* host, int64_t port) {
    if (fd < 0) return false;
    char port_str[16];
    snprintf(port_str, sizeof(port_str), "%ld", (long)port);
    struct addrinfo hints, *res = NULL;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_INET;
    // The socket type is already fixed (fd exists) — getaddrinfo is only
    // used for name resolution here, SOCK_STREAM as a filter is enough for
    // A records.
    hints.ai_socktype = SOCK_STREAM;
    if (getaddrinfo(host, port_str, &hints, &res) != 0 || !res) return false;
    int r = connect((int)fd, res->ai_addr, res->ai_addrlen);
    freeaddrinfo(res);
    return r == 0;
}

bool socketBind(int64_t fd, int64_t port) {
    if (fd < 0) return false;
    int opt = 1;
    setsockopt((int)fd, SOL_SOCKET, SO_REUSEADDR, &opt, sizeof(opt));
    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_ANY);
    addr.sin_port = htons((uint16_t)port);
    return bind((int)fd, (struct sockaddr*)&addr, sizeof(addr)) == 0;
}

bool socketListen(int64_t fd) {
    if (fd < 0) return false;
    return listen((int)fd, 16) == 0;
}

int64_t socketAccept(int64_t fd) {
    if (fd < 0) return -1;
    if (!service_io_wait((int)fd, POLLIN)) return -1;
    return (int64_t)accept((int)fd, NULL, NULL);
}

int64_t socketSend(int64_t fd, const char* data) {
    if (fd < 0) return -1;
    size_t len = strlen(data);
    ssize_t sent = send((int)fd, data, len, 0);
    return (int64_t)sent;
}

char* socketReceive(int64_t fd, int64_t size) {
    if (fd < 0 || size <= 0) return GC_strdup("");
    char* buf = (char*)GC_malloc((size_t)size + 1);
    ssize_t n = recv((int)fd, buf, (size_t)size, 0);
    if (n <= 0) { buf[0] = '\0'; return buf; }
    buf[n] = '\0';
    return buf;
}

// Read raw bytes from an fd (HTTP/2 server framing). Reads EXACTLY count
// bytes (blocking, loops over short reads) and returns them as a string;
// fewer than count means EOF/error midway through.
//
// Bug 94: used to be a single non-retrying read() call, so a frame whose
// payload arrived across more than one TCP segment (routine for anything
// beyond a few KB) was silently truncated instead of fully read — the
// HTTP/2 frame parser (http2_server/Http2Server.tnx: readFrame) would then
// misparse the rest of the connection. Also had no cap of its own on
// `count`; the wire format bounds a single frame's length field to 24 bits
// (~16MB, http2_server/Http2Server.tnx: readFrame), but this primitive is
// generic — cap it here too as defense in depth against any caller that
// doesn't already enforce that.
#define TINOX_HTTP2_MAX_RAW_READ (16 * 1024 * 1024)
// `count` is a peer-declared HTTP/2 frame length (Http2Server::readFrame,
// RFC 7540 §4.1's 24-bit Length field), confirmed only after the fact --
// same amplification shape as httpConnReadN's `n` (see that function's
// comment, found by fuzz/amqp091, issue #111): pre-allocating a buffer
// sized to the full (possibly ~16MB, TINOX_HTTP2_MAX_RAW_READ) `count`
// before any bytes are confirmed to exist on the socket let a malicious/
// misbehaving HTTP/2 client trigger an outsized SERVER-side allocation
// per frame header while sending almost no actual bytes -- arguably worse
// here than the AMQP client-side case, since an HTTP/2 server accepts
// connections from arbitrary untrusted clients. Grow in the same bounded-
// chunk + amortized-doubling shape as httpConnReadN instead of trusting
// `count` upfront.
// Binary frame I/O for the cleartext HTTP/2 parser. Declared lengths are
// clamped and allocation grows only as bytes arrive, never from peer claims.
int64_t httpFdReadBuf(int64_t fd, int64_t count) {
    if (fd < 0 || count <= 0) return tinoxBufNew(0);
    if (count > TINOX_HTTP2_MAX_RAW_READ) count = TINOX_HTTP2_MAX_RAW_READ;
    size_t capacity = count < 4096 ? (size_t)count : 4096, used = 0;
    TinoxBuf* buffer = tinox_buf_alloc((int64_t)capacity);
    while (used < (size_t)count) {
        if (used == capacity) {
            capacity = capacity * 2 > (size_t)count ? (size_t)count : capacity * 2;
            buffer = GC_realloc(buffer, sizeof(TinoxBuf) + capacity);
        }
        ssize_t n = recv((int)fd, buffer->data + used, capacity - used, 0);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) break;
        used += (size_t)n;
    }
    buffer->len = (int64_t)used;
    return (int64_t)(intptr_t)buffer;
}
int64_t httpFdWriteBuf(int64_t fd, int64_t handle) {
    TinoxBuf* buffer = tinox_buf_of(handle, "HTTP/2 write");
    size_t used = 0;
    while (used < (size_t)buffer->len) {
        ssize_t n = send((int)fd, buffer->data + used, (size_t)buffer->len - used, MSG_NOSIGNAL);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) return -1;
        used += (size_t)n;
    }
    return (int64_t)used;
}

char* httpServerReadRawBytes(int64_t fd, int64_t count) {
    if (fd < 0 || count <= 0) return GC_strdup("");
    if (count > TINOX_HTTP2_MAX_RAW_READ) count = TINOX_HTTP2_MAX_RAW_READ;
    size_t cap = (size_t)count < 4096 ? (size_t)count : 4096;
    if (cap < 1) cap = 1;
    char* buf = (char*)GC_malloc(cap + 1);
    size_t got = 0;
    while ((int64_t)got < count) {
        if (got == cap) {
            size_t ncap = cap * 2;
            char* nbuf = (char*)GC_malloc(ncap + 1);
            memcpy(nbuf, buf, got);
            buf = nbuf;
            cap = ncap;
        }
        size_t want = (size_t)count - got;
        size_t chunk = want < (cap - got) ? want : (cap - got);
        ssize_t n = read((int)fd, buf + got, chunk);
        if (n <= 0) break;
        got += (size_t)n;
    }
    buf[got] = '\0';
    return buf;
}

void socketClose(int64_t fd) {
    if (fd >= 0) close((int)fd);
}

// ---- HTTP/1.1 client builtins (tinox.core.http) ----
// Plaintext http:// only (no TLS). Builds on the same blocking
// BSD sockets as above. Request headers are thread-local state
// (httpSetHeader/httpClearHeaders), mirroring the C-globals convention
// of the db/metrics modules.

typedef struct {
    int64_t status;
    char*   body;
    char*   headers; // raw header block (without the status line) for httpHeader()
    char*   error;   // "" on success, otherwise why the request failed (timeout, DNS, connect, TLS, cancelled)
} TinoxHttpResponse;

static __thread char* _tinox_http_req_headers = NULL; // "Name: Value\r\n" chain

void httpSetHeader(const char* name, const char* value) {
    // The active context owns these headers; prevent duplicate wire values.
    if (telemetry_enabled && (!strcasecmp(name, "traceparent") || !strcasecmp(name, "x-request-id") || !strcasecmp(name, "tracestate"))) return;
    size_t old_len = _tinox_http_req_headers ? strlen(_tinox_http_req_headers) : 0;
    size_t add = strlen(name) + strlen(value) + 4; // ": " + "\r\n"
    char* buf = (char*)malloc(old_len + add + 1);
    if (old_len) memcpy(buf, _tinox_http_req_headers, old_len);
    snprintf(buf + old_len, add + 1, "%s: %s\r\n", name, value);
    _tinox_http_req_headers = buf;
}

void httpClearHeaders(void) {
    _tinox_http_req_headers = NULL;
}

// Per-thread TLS client config for https:// requests -- custom CA (self-
// signed cluster CAs, e.g. Kubernetes' own), client cert/key (mTLS), and
// insecure-skip-verify. Mirrors the header state above (thread-local,
// plain malloc/strdup -- never GC memory, so no GC-root registration
// needed, same reasoning as _tinox_http_req_headers). Consulted by
// http_request()'s TLS branch further below; unset (all NULL/0) preserves
// the exact previous behavior (shared lazily-initialized g_tls_client_ctx,
// default system trust store, hostname verification).
static __thread char* _tinox_tls_ca_cert_path = NULL;
static __thread char* _tinox_tls_client_cert_path = NULL;
static __thread char* _tinox_tls_client_key_path = NULL;
static __thread int64_t _tinox_tls_insecure_skip_verify = 0;

void httpSetTlsCaCertFile(const char* path) {
    _tinox_tls_ca_cert_path = path ? strdup(path) : NULL;
}

void httpSetTlsClientCertFile(const char* certPath, const char* keyPath) {
    _tinox_tls_client_cert_path = certPath ? strdup(certPath) : NULL;
    _tinox_tls_client_key_path = keyPath ? strdup(keyPath) : NULL;
}

void httpSetTlsInsecureSkipVerify(int64_t enabled) {
    _tinox_tls_insecure_skip_verify = enabled ? 1 : 0;
}

void httpClearTlsConfig(void) {
    _tinox_tls_ca_cert_path = NULL;
    _tinox_tls_client_cert_path = NULL;
    _tinox_tls_client_key_path = NULL;
    _tinox_tls_insecure_skip_verify = 0;
}

// http_parse_url/http_request/httpGet/httpPost/httpPut/httpDelete/httpPatch
// moved below (after the TLS connection-handle section, ~"---- Binary-safe
// conn primitives ----") so http_request can use g_tls_client_ctx for
// https:// -- that global (and the openssl headers it needs) aren't
// declared yet at this point in the file, and TINOX_TLS=0 opt-out builds
// must not require <openssl/ssl.h> unconditionally at the top of the file.

int64_t httpStatusCode(int64_t* resp) {
    return resp ? ((TinoxHttpResponse*)resp)->status : 0;
}

char* httpBody(int64_t* resp) {
    if (!resp) return GC_strdup("");
    char* b = ((TinoxHttpResponse*)resp)->body;
    return b ? b : GC_strdup("");
}

// Case-insensitive header lookup in the raw header block. "" if not present.
char* httpHeader(int64_t* resp, const char* name) {
    if (!resp) return GC_strdup("");
    const char* hdrs = ((TinoxHttpResponse*)resp)->headers;
    if (!hdrs) return GC_strdup("");
    size_t nlen = strlen(name);
    const char* line = hdrs;
    while (*line) {
        const char* eol = strstr(line, "\r\n");
        size_t line_len = eol ? (size_t)(eol - line) : strlen(line);
        if (line_len > nlen && line[nlen] == ':' && strncasecmp(line, name, nlen) == 0) {
            const char* v = line + nlen + 1;
            while (*v == ' ') v++;
            size_t vlen = line_len - (size_t)(v - line);
            char* out = (char*)GC_malloc(vlen + 1);
            memcpy(out, v, vlen);
            out[vlen] = '\0';
            return out;
        }
        if (!eol) break;
        line = eol + 2;
    }
    return GC_strdup("");
}

// ---- ZIP builtins (STORED/method 0, text contents) ---------------------------
// Minimal but genuine ZIP reader/writer: writes valid .zip files
// (readable by `unzip`), on the read side only supports uncompressed
// entries (method 0). Binary contents with null bytes aren't representable,
// since Tinox strings are null-terminated. The Tinox side (Zip::listEntries)
// builds the List<ZipEntry> itself from zipEntryCount/zipEntryName/zipEntrySize —
// that keeps C decoupled from the class ABI.

typedef struct {
    char*          name;
    unsigned char* data;
    uint32_t       size;
} TinoxZipMember;

static uint32_t tinox_zip_crc32(const unsigned char* data, size_t len) {
    static uint32_t table[256];
    static int have_table = 0;
    if (!have_table) {
        for (uint32_t i = 0; i < 256; i++) {
            uint32_t c = i;
            for (int k = 0; k < 8; k++)
                c = (c & 1u) ? (0xEDB88320u ^ (c >> 1)) : (c >> 1);
            table[i] = c;
        }
        have_table = 1;
    }
    uint32_t crc = 0xFFFFFFFFu;
    for (size_t i = 0; i < len; i++)
        crc = table[(crc ^ data[i]) & 0xFFu] ^ (crc >> 8);
    return crc ^ 0xFFFFFFFFu;
}

static uint16_t tinox_zip_rd16(const unsigned char* p) {
    return (uint16_t)((uint16_t)p[0] | ((uint16_t)p[1] << 8));
}
static uint32_t tinox_zip_rd32(const unsigned char* p) {
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8)
         | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}
static void tinox_zip_wr16(unsigned char* p, uint16_t v) {
    p[0] = (unsigned char)(v & 0xFF); p[1] = (unsigned char)((v >> 8) & 0xFF);
}
static void tinox_zip_wr32(unsigned char* p, uint32_t v) {
    p[0] = (unsigned char)(v & 0xFF);         p[1] = (unsigned char)((v >> 8) & 0xFF);
    p[2] = (unsigned char)((v >> 16) & 0xFF); p[3] = (unsigned char)((v >> 24) & 0xFF);
}

// Read the whole file into a buffer; NULL + *out_len=0 if it can't be opened.
static unsigned char* tinox_zip_read_file(const char* path, size_t* out_len) {
    *out_len = 0;
    FILE* f = fopen(path, "rb");
    if (!f) return NULL;
    if (fseek(f, 0, SEEK_END) != 0) { fclose(f); return NULL; }
    long sz = ftell(f);
    if (sz < 0) { fclose(f); return NULL; }
    rewind(f);
    unsigned char* buf = (unsigned char*)malloc((size_t)sz + 1);
    size_t rd = fread(buf, 1, (size_t)sz, f);
    fclose(f);
    buf[rd] = 0;
    *out_len = rd;
    return buf;
}

// Parse all STORED entries. Return value = count, *out = array (GC-allocated).
static int tinox_zip_parse(const char* path, TinoxZipMember** out) {
    *out = NULL;
    size_t len;
    unsigned char* buf = tinox_zip_read_file(path, &len);
    if (!buf || len < 4) return 0;

    TinoxZipMember* mem = NULL;
    int n = 0, cap = 0;
    size_t pos = 0;
    while (pos + 30 <= len && tinox_zip_rd32(buf + pos) == 0x04034b50u) {
        uint16_t method = tinox_zip_rd16(buf + pos + 8);
        uint32_t csize  = tinox_zip_rd32(buf + pos + 18);
        uint32_t usize  = tinox_zip_rd32(buf + pos + 22);
        uint16_t nlen   = tinox_zip_rd16(buf + pos + 26);
        uint16_t elen   = tinox_zip_rd16(buf + pos + 28);
        size_t name_off = pos + 30;
        size_t data_off = name_off + nlen + elen;
        if (data_off + csize > len) break;
        // Bug 98: STORED entries (method 0) have no compression, so usize
        // MUST equal csize by definition. The bounds check above only
        // validates csize (data_off + csize <= len), but the memcpy below
        // copies `usize` bytes -- a crafted archive with a small, in-bounds
        // csize and a much larger usize would read far past the allocated
        // archive buffer. Enforce the STORED invariant instead of trusting
        // usize independently; a mismatching entry is just skipped (pos
        // still advances by the validated csize below).
        if (method == 0 && usize == csize) {
            char* nm = (char*)malloc((size_t)nlen + 1);
            memcpy(nm, buf + name_off, nlen); nm[nlen] = 0;
            unsigned char* d = (unsigned char*)malloc((size_t)usize + 1);
            memcpy(d, buf + data_off, usize); d[usize] = 0;
            if (n == cap) {
                cap = cap ? cap * 2 : 8;
                mem = (TinoxZipMember*)realloc(mem, (size_t)cap * sizeof(TinoxZipMember));
            }
            mem[n].name = nm; mem[n].data = d; mem[n].size = usize;
            n++;
        }
        pos = data_off + csize;
    }
    *out = mem;
    return n;
}

// Write entries as a valid STORED .zip.
static void tinox_zip_write(const char* path, TinoxZipMember* mem, int n) {
    FILE* f = fopen(path, "wb");
    if (!f) return;

    // Local headers + data; remember offsets for the central directory.
    uint32_t* offsets = (uint32_t*)malloc((size_t)(n > 0 ? n : 1) * sizeof(uint32_t));
    uint32_t* crcs    = (uint32_t*)malloc((size_t)(n > 0 ? n : 1) * sizeof(uint32_t));
    uint32_t cursor = 0;
    unsigned char lh[30];
    for (int i = 0; i < n; i++) {
        uint16_t nlen = (uint16_t)strlen(mem[i].name);
        uint32_t crc  = tinox_zip_crc32(mem[i].data, mem[i].size);
        offsets[i] = cursor;
        crcs[i]    = crc;
        memset(lh, 0, sizeof(lh));
        tinox_zip_wr32(lh + 0, 0x04034b50u);      // local file header signature
        tinox_zip_wr16(lh + 4, 20);               // version needed
        tinox_zip_wr16(lh + 6, 0);                // flags
        tinox_zip_wr16(lh + 8, 0);                // method: STORED
        tinox_zip_wr16(lh + 10, 0);               // mod time
        tinox_zip_wr16(lh + 12, 0x21);            // mod date (1980-01-01)
        tinox_zip_wr32(lh + 14, crc);             // crc-32
        tinox_zip_wr32(lh + 18, mem[i].size);     // compressed size
        tinox_zip_wr32(lh + 22, mem[i].size);     // uncompressed size
        tinox_zip_wr16(lh + 26, nlen);            // name length
        tinox_zip_wr16(lh + 28, 0);               // extra length
        fwrite(lh, 1, sizeof(lh), f);
        fwrite(mem[i].name, 1, nlen, f);
        fwrite(mem[i].data, 1, mem[i].size, f);
        cursor += (uint32_t)sizeof(lh) + nlen + mem[i].size;
    }

    // Central directory.
    uint32_t cd_start = cursor;
    unsigned char ch[46];
    for (int i = 0; i < n; i++) {
        uint16_t nlen = (uint16_t)strlen(mem[i].name);
        memset(ch, 0, sizeof(ch));
        tinox_zip_wr32(ch + 0, 0x02014b50u);      // central dir signature
        tinox_zip_wr16(ch + 4, 20);               // version made by
        tinox_zip_wr16(ch + 6, 20);               // version needed
        tinox_zip_wr16(ch + 8, 0);                // flags
        tinox_zip_wr16(ch + 10, 0);               // method: STORED
        tinox_zip_wr16(ch + 12, 0);               // mod time
        tinox_zip_wr16(ch + 14, 0x21);            // mod date
        tinox_zip_wr32(ch + 16, crcs[i]);         // crc-32
        tinox_zip_wr32(ch + 20, mem[i].size);     // compressed size
        tinox_zip_wr32(ch + 24, mem[i].size);     // uncompressed size
        tinox_zip_wr16(ch + 28, nlen);            // name length
        tinox_zip_wr16(ch + 30, 0);               // extra length
        tinox_zip_wr16(ch + 32, 0);               // comment length
        tinox_zip_wr16(ch + 34, 0);               // disk number start
        tinox_zip_wr16(ch + 36, 0);               // internal attrs
        tinox_zip_wr32(ch + 38, 0);               // external attrs
        tinox_zip_wr32(ch + 42, offsets[i]);      // local header offset
        fwrite(ch, 1, sizeof(ch), f);
        fwrite(mem[i].name, 1, nlen, f);
        cursor += (uint32_t)sizeof(ch) + nlen;
    }
    uint32_t cd_size = cursor - cd_start;

    // End Of Central Directory.
    unsigned char eocd[22];
    memset(eocd, 0, sizeof(eocd));
    tinox_zip_wr32(eocd + 0, 0x06054b50u);        // EOCD signature
    tinox_zip_wr16(eocd + 8, (uint16_t)n);        // entries this disk
    tinox_zip_wr16(eocd + 10, (uint16_t)n);       // total entries
    tinox_zip_wr32(eocd + 12, cd_size);           // central dir size
    tinox_zip_wr32(eocd + 16, cd_start);          // central dir offset
    fwrite(eocd, 1, sizeof(eocd), f);

    fclose(f);
}

int64_t zipEntryCount(const char* path) {
    TinoxZipMember* mem;
    int n = tinox_zip_parse(path, &mem);
    return (int64_t)n;
}

char* zipEntryName(const char* path, int64_t idx) {
    TinoxZipMember* mem;
    int n = tinox_zip_parse(path, &mem);
    if (idx < 0 || idx >= n) return GC_strdup("");
    return GC_strdup(mem[idx].name);
}

int64_t zipEntrySize(const char* path, int64_t idx) {
    TinoxZipMember* mem;
    int n = tinox_zip_parse(path, &mem);
    if (idx < 0 || idx >= n) return 0;
    return (int64_t)mem[idx].size;
}

// An entry's content as a string; "" if not found.
char* zipExtractFile(const char* path, const char* name) {
    TinoxZipMember* mem;
    int n = tinox_zip_parse(path, &mem);
    for (int i = 0; i < n; i++) {
        if (strcmp(mem[i].name, name) == 0)
            return GC_strdup((const char*)mem[i].data);
    }
    return GC_strdup("");
}

// Add/replace a file (creates the .zip if needed).
void zipAddFile(const char* path, const char* name, const char* content) {
    TinoxZipMember* old;
    int n = tinox_zip_parse(path, &old);
    TinoxZipMember* mem = (TinoxZipMember*)malloc((size_t)(n + 1) * sizeof(TinoxZipMember));
    int m = 0;
    for (int i = 0; i < n; i++) {
        if (strcmp(old[i].name, name) == 0) continue; // replace
        mem[m++] = old[i];
    }
    mem[m].name = (char*)name;
    mem[m].data = (unsigned char*)content;
    mem[m].size = (uint32_t)strlen(content);
    m++;
    tinox_zip_write(path, mem, m);
}

// Remove a file (no error if it doesn't exist).
void zipRemoveFile(const char* path, const char* name) {
    TinoxZipMember* old;
    int n = tinox_zip_parse(path, &old);
    TinoxZipMember* mem = (TinoxZipMember*)malloc((size_t)(n > 0 ? n : 1) * sizeof(TinoxZipMember));
    int m = 0;
    for (int i = 0; i < n; i++) {
        if (strcmp(old[i].name, name) == 0) continue;
        mem[m++] = old[i];
    }
    tinox_zip_write(path, mem, m);
}

// ---- Process / Environment builtins ----

// Forward declarations for CLI argument globals (defined later in this file)
extern int    _tinox_argc;
extern char** _tinox_argv;

void processExit(int64_t code) {
    exit((int)code);
}

int64_t* processArgs(void) {
    // Returns a Tinox array handle of arg strings as i64 (ptrtoint)
    int64_t n = (int64_t)_tinox_argc;
    int64_t* nh = tinox_array_new(n, 0);
    int64_t* data = ((TinoxArray*)nh)->data;
    for (int64_t i = 0; i < n; i++) {
        data[i] = (int64_t)_tinox_argv[i];
    }
    return nh;
}

int64_t processId(void) {
    return (int64_t)getpid();
}

static void tinox_random_seed_once(void) {
    static int seeded = 0;
    if (!seeded) {
        srandom((unsigned int)(time(NULL) ^ getpid()));
        seeded = 1;
    }
}

// [min, max) — matches the tinox.core.random Random class convention.
int64_t randomInt(int64_t min, int64_t max) {
    tinox_random_seed_once();
    if (max <= min) return min;
    return min + (int64_t)(random() % (max - min));
}

double randomFloat(void) {
    tinox_random_seed_once();
    // random() returns [0, 2^31-1] per POSIX, independent of RAND_MAX.
    return (double)random() / 2147483648.0;
}

void gcCollect(void) {
    GC_gcollect();
}

int64_t memoryUsage(void) {
    return (int64_t)GC_get_heap_size();
}

// Boehm GC counters, read without triggering a collection. Kinds:
//   0 reserved heap bytes   1 free bytes in the heap (as of the last collection)
//   2 bytes allocated since the last collection   3 cumulative bytes allocated
//   4 completed collections   5 used estimate = heap - free + since-last-gc
// Kind 5 is an upper bound on live data: it still counts garbage not yet
// collected. It is NOT RSS (the heap excludes stacks, malloc'd runtime state
// and the C runtime) and not a live-byte measurement.
int64_t gcStat(int64_t kind) {
    switch (kind) {
    case 0: return (int64_t)GC_get_heap_size();
    case 1: return (int64_t)GC_get_free_bytes();
    case 2: return (int64_t)GC_get_bytes_since_gc();
    case 3: return (int64_t)GC_get_total_bytes();
    case 4: return (int64_t)GC_get_gc_no();
    case 5: {
        int64_t used = (int64_t)GC_get_heap_size() - (int64_t)GC_get_free_bytes() + (int64_t)GC_get_bytes_since_gc();
        return used < 0 ? 0 : used;
    }
    default:
        fprintf(stderr, "gcStat: unknown counter %lld\n", (long long)kind);
        exit(1);
    }
}

void printStackTrace(void) {
#ifdef __GLIBC__
    void* frames[64];
    int n = backtrace(frames, 64);
    backtrace_symbols_fd(frames, n, fileno(stderr));
#else
    fprintf(stderr, "<stack trace unavailable on this platform>\n");
#endif
}

char* envGet(const char* name) {
    char* v = getenv(name);
    return v ? GC_strdup(v) : GC_strdup("");
}

void envSet(const char* name, const char* value) {
    setenv(name, value, 1);
}

void envRemove(const char* name) {
    unsetenv(name);
}

char* envCurrentDir(void) {
    char buf[4096];
    if (getcwd(buf, sizeof(buf))) return GC_strdup(buf);
    return GC_strdup("");
}

void envSetCurrentDir(const char* path) {
    chdir(path);
}

// ---- Argv-based subprocess execution (tinox.core.process) -----------------
//
// posix_spawnp, never a shell: every argv element is passed as its own native
// argument, so a value flowing straight from untrusted input (e.g. a
// Kubernetes namespace/pod name taken from an HTTP path param) can never be
// interpreted as shell syntax. This is deliberately a separate mechanism from
// tinox_run_command_json below (popen-based, one shell-escaped command
// string) -- that one is fine for its single fixed, compile-time-constant
// call site (the dev-UI test runner), but is the wrong tool for a command
// built from caller-supplied arguments.
//
// Handles are returned as an Int64 (ptrtoint of a GC_malloc'd struct), the
// same "opaque native resource as an Int64 handle" idiom used throughout
// this runtime (HTTP connection fds, DB connections, etc.) rather than a
// pointer type — Tinox's type system has no generic opaque-pointer type.
// The struct and its two string fields are GC_malloc'd/GC_strdup'd, so
// there's no explicit free: once the Tinox-side handle is no longer
// reachable, the GC reclaims it like everything else in this runtime.

typedef struct {
    char* out;
    char* err;
    int64_t exit_code;
    int64_t timed_out;
} TinoxProcessResult;

typedef struct {
    char* buf;
    size_t len;
    size_t cap;
} TinoxGrowBuf;

static void tinox_growbuf_append(TinoxGrowBuf* b, const char* data, size_t n) {
    if (n == 0) return;
    if (b->len + n + 1 > b->cap) {
        size_t new_cap = b->cap ? b->cap * 2 : 4096;
        while (new_cap < b->len + n + 1) new_cap *= 2;
        char* nb = realloc(b->buf, new_cap);
        if (!nb) {
            fprintf(stderr, "runtime error: process_run: out of memory capturing output\n");
            exit(1);
        }
        b->buf = nb;
        b->cap = new_cap;
    }
    memcpy(b->buf + b->len, data, n);
    b->len += n;
    b->buf[b->len] = '\0';
}

// Build the child's environment in the parent: entries KEY=value override
// inherited values; bare KEY removes a value. Never mutate the host environment
// or call setenv/malloc after fork in a multithreaded runtime.
static pid_t tinox_spawn_piped(char** argv, const char* cwd, TinoxArray* changes,
                                int in_pipe[2], int out_pipe[2], int err_pipe[2], int new_group) {
    extern char** environ;
    size_t count = 0;
    while (environ[count]) count++;
    size_t extra = changes ? (size_t)changes->len : 0;
    char** env = calloc(count + extra + 1, sizeof(char*));
    if (!env) { perror("process environment"); exit(1); }
    memcpy(env, environ, count * sizeof(char*));
    for (size_t i = 0; i < extra; i++) {
        char* entry = (char*)changes->data[i];
        size_t keylen = strcspn(entry, "=");
        if (!keylen) {
            fprintf(stderr, "runtime error: process environment key must not be empty\n");
            exit(1);
        }
        for (size_t j = 0; j < count;) {
            if (strncmp(env[j], entry, keylen) == 0 && env[j][keylen] == '=') {
                memmove(env + j, env + j + 1, (count - j) * sizeof(char*));
                count--;
            } else j++;
        }
        if (entry[keylen] == '=') env[count++] = entry;
        env[count] = NULL;
    }
    posix_spawn_file_actions_t actions;
    int error = posix_spawn_file_actions_init(&actions);
    if (error) { errno = error; perror("process spawn actions"); exit(1); }
    #define PROCESS_ACTION(action) do { error = (action); if (error) { \
        errno = error; perror("process spawn action"); exit(1); } } while (0)
    #define PROCESS_ATTR_CHECK(call) do { error = (call); if (error) { \
        errno = error; perror("process spawn attributes"); exit(1); } } while (0)
    PROCESS_ACTION(posix_spawn_file_actions_adddup2(&actions, in_pipe[0], STDIN_FILENO));
    PROCESS_ACTION(posix_spawn_file_actions_adddup2(&actions, out_pipe[1], STDOUT_FILENO));
    PROCESS_ACTION(posix_spawn_file_actions_adddup2(&actions, err_pipe[1], STDERR_FILENO));
    for (int i = 0; i < 2; i++) {
        PROCESS_ACTION(posix_spawn_file_actions_addclose(&actions, in_pipe[i]));
        PROCESS_ACTION(posix_spawn_file_actions_addclose(&actions, out_pipe[i]));
        PROCESS_ACTION(posix_spawn_file_actions_addclose(&actions, err_pipe[i]));
    }
    if (cwd && *cwd) PROCESS_ACTION(posix_spawn_file_actions_addchdir_np(&actions, cwd));
    #undef PROCESS_ACTION
    // Long-lived interactive children get their own process group so
    // kill/terminate reach the whole tree (a build tool and the compilers
    // it started), not just the leader. Process::run keeps the caller's
    // group so a terminal Ctrl-C still reaches it.
    posix_spawnattr_t attr;
    posix_spawnattr_t* attrp = NULL;
    if (new_group) {
        PROCESS_ATTR_CHECK(posix_spawnattr_init(&attr));
        PROCESS_ATTR_CHECK(posix_spawnattr_setflags(&attr, POSIX_SPAWN_SETPGROUP));
        PROCESS_ATTR_CHECK(posix_spawnattr_setpgroup(&attr, 0));
        attrp = &attr;
    }
    pid_t pid;
    error = posix_spawnp(&pid, argv[0], &actions, attrp, argv, env);
    if (attrp) posix_spawnattr_destroy(&attr);
    posix_spawn_file_actions_destroy(&actions);
    free(env);
    if (error) {
        // Preserve Process::run's conventional 127 for setup/exec failures.
        return -1;
    }
    return pid;
}

// Runs argv[0] with argv[1..] as arguments, feeding `stdin_data` to the
// child's stdin (empty string = closed/empty stdin, e.g. for a plain
// `kubectl get`) and capturing stdout/stderr separately, enforcing a
// wall-clock timeout (timeout_ms <= 0 means no timeout). On timeout the
// child is SIGKILLed and reaped; `timed_out` is set on the result and
// exit_code is -1 (no real exit status exists in that case). A
// signal-terminated child reports exit_code as -signal, matching the
// negative-on-signal convention already used elsewhere in this runtime.
//
// stdin is written concurrently with draining stdout/stderr (poll() on all
// three fds at once, POLLOUT on the stdin pipe alongside POLLIN on the
// other two) rather than "write all of stdin, then read output" -- the
// naive sequential approach deadlocks as soon as the child's own stdout/
// stderr fills its OS pipe buffer before it has fully read stdin (a real
// case here: `kubectl apply -f -` with a large manifest can easily write
// enough progress/warning output to hit this before finishing reading).
//
// posix_spawnp performs child setup without invoking GC or allocation code
// after fork in this multithreaded runtime.
int64_t processRunWithOptions(int64_t* argv_handle, int64_t timeout_ms, const char* stdin_data,
                              const char* cwd, int64_t* env_handle) {
    TinoxArray* argv_arr = (TinoxArray*)argv_handle;
    if (!argv_arr || argv_arr->len < 1) {
        fprintf(stderr, "runtime error: process_run: argv must have at least one element (the program)\n");
        exit(1);
    }
    int64_t argc = argv_arr->len;
    char** argv = malloc((size_t)(argc + 1) * sizeof(char*));
    if (!argv) {
        fprintf(stderr, "runtime error: process_run: out of memory building argv\n");
        exit(1);
    }
    for (int64_t i = 0; i < argc; i++) {
        argv[i] = (char*)argv_arr->data[i];
    }
    argv[argc] = NULL;

    int in_pipe[2];
    int out_pipe[2];
    int err_pipe[2];
    if (pipe2(in_pipe, O_CLOEXEC) != 0 || pipe2(out_pipe, O_CLOEXEC) != 0 || pipe2(err_pipe, O_CLOEXEC) != 0) {
        fprintf(stderr, "runtime error: process_run: pipe() failed: %s\n", strerror(errno));
        exit(1);
    }

    pid_t pid = tinox_spawn_piped(argv, cwd, (TinoxArray*)env_handle, in_pipe, out_pipe, err_pipe, 0);
    if (pid < 0) {
        for (int i = 0; i < 2; i++) { close(in_pipe[i]); close(out_pipe[i]); close(err_pipe[i]); }
        free(argv);
        TinoxProcessResult* result = GC_malloc(sizeof(*result));
        result->out = GC_strdup(""); result->err = GC_strdup("");
        result->exit_code = 127; result->timed_out = 0;
        return (int64_t)(intptr_t)result;
    }

    // Parent
    close(in_pipe[0]);
    close(out_pipe[1]);
    close(err_pipe[1]);
    free(argv);

    int in_fd = in_pipe[1];
    int out_fd = out_pipe[0];
    int err_fd = err_pipe[0];
    // Non-blocking: a blocking write of a large input would stall here until the child has
    // consumed all of it, while the child may itself be blocked writing output nobody reads
    // (cat on 128 KB deadlocks). Under pipe-memory pressure the kernel shrinks new pipes to one
    // page, which makes this reproducible. The poll loop below handles EAGAIN.
    fcntl(in_fd, F_SETFL, fcntl(in_fd, F_GETFL, 0) | O_NONBLOCK);
    size_t stdin_len = strlen(stdin_data);
    size_t stdin_written = 0;
    int in_open = stdin_len > 0;
    if (!in_open) close(in_fd); // nothing to write -- EOF immediately, same as e.g. `kubectl get`
    int out_open = 1, err_open = 1;

    TinoxGrowBuf out_buf = {0};
    TinoxGrowBuf err_buf = {0};

    int has_deadline = timeout_ms > 0;
    struct timespec deadline;
    if (has_deadline) {
        clock_gettime(CLOCK_MONOTONIC, &deadline);
        deadline.tv_sec += timeout_ms / 1000;
        deadline.tv_nsec += (timeout_ms % 1000) * 1000000L;
        if (deadline.tv_nsec >= 1000000000L) { deadline.tv_sec++; deadline.tv_nsec -= 1000000000L; }
    }

    int64_t timed_out = 0;
    char readbuf[4096];

    while (in_open || out_open || err_open) {
        struct pollfd fds[3];
        int nfds = 0;
        int in_idx = -1, out_idx = -1, err_idx = -1;
        if (in_open) { in_idx = nfds; fds[nfds].fd = in_fd; fds[nfds].events = POLLOUT; nfds++; }
        if (out_open) { out_idx = nfds; fds[nfds].fd = out_fd; fds[nfds].events = POLLIN; nfds++; }
        if (err_open) { err_idx = nfds; fds[nfds].fd = err_fd; fds[nfds].events = POLLIN; nfds++; }

        int poll_timeout_ms = -1;
        if (has_deadline) {
            struct timespec now;
            clock_gettime(CLOCK_MONOTONIC, &now);
            long remain_ms = (deadline.tv_sec - now.tv_sec) * 1000L + (deadline.tv_nsec - now.tv_nsec) / 1000000L;
            if (remain_ms <= 0) { timed_out = 1; break; }
            poll_timeout_ms = (int)(remain_ms > INT32_MAX ? INT32_MAX : remain_ms);
        }

        int pr = poll(fds, nfds, poll_timeout_ms);
        if (pr < 0) {
            if (errno == EINTR) continue; // GC's SIGPWR (or any other signal): retry
            break;
        }
        if (pr == 0) { timed_out = 1; break; }

        if (in_open && (fds[in_idx].revents & (POLLOUT | POLLERR | POLLHUP))) {
            if (fds[in_idx].revents & POLLOUT) {
                ssize_t n = write(in_fd, stdin_data + stdin_written, stdin_len - stdin_written);
                if (n > 0) {
                    stdin_written += (size_t)n;
                    if (stdin_written == stdin_len) { close(in_fd); in_open = 0; }
                } else if (n < 0 && errno != EINTR && errno != EAGAIN) {
                    // Child closed its stdin early (e.g. doesn't read it at
                    // all) -- not our error to report, just stop writing.
                    close(in_fd); in_open = 0;
                }
            } else {
                close(in_fd); in_open = 0;
            }
        }
        if (out_open && (fds[out_idx].revents & (POLLIN | POLLHUP | POLLERR))) {
            ssize_t n = read(out_fd, readbuf, sizeof(readbuf));
            if (n > 0) tinox_growbuf_append(&out_buf, readbuf, (size_t)n);
            else if (n == 0) { close(out_fd); out_open = 0; }
            else if (errno != EINTR && errno != EAGAIN) { close(out_fd); out_open = 0; }
        }
        if (err_open && (fds[err_idx].revents & (POLLIN | POLLHUP | POLLERR))) {
            ssize_t n = read(err_fd, readbuf, sizeof(readbuf));
            if (n > 0) tinox_growbuf_append(&err_buf, readbuf, (size_t)n);
            else if (n == 0) { close(err_fd); err_open = 0; }
            else if (errno != EINTR && errno != EAGAIN) { close(err_fd); err_open = 0; }
        }
    }
    if (in_open) close(in_fd); // e.g. timed out while still writing stdin

    int64_t exit_code;
    if (timed_out) {
        kill(pid, SIGKILL);
        int status;
        while (waitpid(pid, &status, 0) < 0 && errno == EINTR) {}
        if (out_open) close(out_fd);
        if (err_open) close(err_fd);
        exit_code = -1;
    } else {
        int status;
        while (waitpid(pid, &status, 0) < 0 && errno == EINTR) {}
        if (WIFEXITED(status)) exit_code = WEXITSTATUS(status);
        else if (WIFSIGNALED(status)) exit_code = -WTERMSIG(status);
        else exit_code = -1;
    }

    TinoxProcessResult* result = (TinoxProcessResult*)GC_malloc(sizeof(TinoxProcessResult));
    result->out = out_buf.buf ? GC_strdup(out_buf.buf) : GC_strdup("");
    result->err = err_buf.buf ? GC_strdup(err_buf.buf) : GC_strdup("");
    result->exit_code = exit_code;
    result->timed_out = timed_out;
    free(out_buf.buf);
    free(err_buf.buf);
    return (int64_t)(intptr_t)result;
}

int64_t processRun(int64_t* argv, int64_t timeout_ms, const char* input) {
    return processRunWithOptions(argv, timeout_ms, input, "", NULL);
}

char* processResultStdout(int64_t handle) {
    return ((TinoxProcessResult*)(intptr_t)handle)->out;
}

char* processResultStderr(int64_t handle) {
    return ((TinoxProcessResult*)(intptr_t)handle)->err;
}

int64_t processResultExitCode(int64_t handle) {
    return ((TinoxProcessResult*)(intptr_t)handle)->exit_code;
}

int64_t processResultTimedOut(int64_t handle) {
    return ((TinoxProcessResult*)(intptr_t)handle)->timed_out;
}

// DI publication is serialized across the dependency graph. Recursive entry
// permits nested providers; dependency cycles are rejected before codegen.
static pthread_mutex_t tinox_di_mu = PTHREAD_RECURSIVE_MUTEX_INITIALIZER_NP;
void tinox_di_enter(void) { task_pthread_check(pthread_mutex_lock(&tinox_di_mu)); }
void tinox_di_exit(void) { task_pthread_check(pthread_mutex_unlock(&tinox_di_mu)); }

// One recursive monitor per annotated class, rooted by its generated global.
// This protects annotated calls, not direct field access or escaped objects.
static pthread_mutex_t tinox_monitor_init_mu = PTHREAD_MUTEX_INITIALIZER;
int64_t tinox_monitor_enter(int64_t* slot) {
    task_pthread_check(pthread_mutex_lock(&tinox_monitor_init_mu));
    if (!*slot) {
        pthread_mutex_t* mutex = GC_malloc(sizeof(*mutex));
        pthread_mutexattr_t attr;
        task_pthread_check(pthread_mutexattr_init(&attr));
        task_pthread_check(pthread_mutexattr_settype(&attr, PTHREAD_MUTEX_RECURSIVE));
        task_pthread_check(pthread_mutex_init(mutex, &attr));
        task_pthread_check(pthread_mutexattr_destroy(&attr));
        *slot = (int64_t)(intptr_t)mutex;
    }
    pthread_mutex_t* mutex = (pthread_mutex_t*)(intptr_t)*slot;
    task_pthread_check(pthread_mutex_unlock(&tinox_monitor_init_mu));
    for (;;) {
        // Preserve an already-unwinding error when called by cleanup code.
        if (!__tinox_err && task_cancellation(task_scope)) {
            taskCheckCancellation();
            return 0;
        }
        int rc = pthread_mutex_trylock(mutex);
        if (!rc) return 1;
        if (rc != EBUSY) task_pthread_check(rc);
        struct timespec delay = { 0, 10000000 };
        while (nanosleep(&delay, &delay) < 0 && errno == EINTR) {}
    }
}
void tinox_monitor_exit(int64_t* slot) {
    task_pthread_check(pthread_mutex_unlock((pthread_mutex_t*)(intptr_t)*slot));
}

// ---- Real synchronization primitives (tinox.core.semaphore) ---------------
//
// Fixes a real, previously-undiscovered bug found while designing
// tinox-k8s-ui's exec/terminal feature: Mutex/Semaphore/RWLock's Tinox-level
// implementations were a plain "if not locked { locked = true }" -- a
// classic check-then-act race with no atomic compare-and-swap or OS
// primitive underneath, so NOT actually thread-safe under real concurrent
// access despite the name/purpose (verified as broken by inspection, not
// hypothetically -- there is no way for pure Tinox code, which has no
// atomic-CAS builtin, to implement real mutual exclusion). These wrap real
// POSIX primitives instead; handles are Int64 (GC_malloc'd, same "opaque
// native resource as an Int64 handle" idiom as ProcessResult/DB connections
// elsewhere in this runtime).
//
// Mutex::lock() now genuinely BLOCKS until available (pthread_mutex_lock),
// not "throw if already locked" like the old implementation -- that was an
// accident of the broken check-then-act code, not a deliberate API choice
// (nothing named/documented as "Mutex.lock()" is expected to throw on
// contention in any mainstream language). tryLock()'s existing
// non-blocking-with-a-bool-result contract is unchanged, now backed by a
// real pthread_mutex_trylock.

int64_t mutexNew(void) {
    pthread_mutex_t* m = (pthread_mutex_t*)GC_malloc(sizeof(pthread_mutex_t));
    pthread_mutex_init(m, NULL);
    return (int64_t)(intptr_t)m;
}

void mutexLock(int64_t handle) {
    pthread_mutex_lock((pthread_mutex_t*)(intptr_t)handle);
}

void mutexUnlock(int64_t handle) {
    pthread_mutex_unlock((pthread_mutex_t*)(intptr_t)handle);
}

int64_t mutexTryLock(int64_t handle) {
    return pthread_mutex_trylock((pthread_mutex_t*)(intptr_t)handle) == 0 ? 1 : 0;
}

int64_t semaphoreNew(int64_t initialCount) {
    sem_t* s = (sem_t*)GC_malloc(sizeof(sem_t));
    sem_init(s, 0, (unsigned int)initialCount);
    return (int64_t)(intptr_t)s;
}

void semaphoreAcquire(int64_t handle) {
    sem_t* s = (sem_t*)(intptr_t)handle;
    while (sem_wait(s) != 0) {
        if (errno != EINTR) break; // GC's SIGPWR (or any other signal): retry
    }
}

void semaphoreRelease(int64_t handle) {
    sem_post((sem_t*)(intptr_t)handle);
}

int64_t semaphoreTryAcquire(int64_t handle) {
    return sem_trywait((sem_t*)(intptr_t)handle) == 0 ? 1 : 0;
}

int64_t rwlockNew(void) {
    pthread_rwlock_t* l = (pthread_rwlock_t*)GC_malloc(sizeof(pthread_rwlock_t));
    pthread_rwlock_init(l, NULL);
    return (int64_t)(intptr_t)l;
}

void rwlockReadLock(int64_t handle) {
    pthread_rwlock_rdlock((pthread_rwlock_t*)(intptr_t)handle);
}

void rwlockReadUnlock(int64_t handle) {
    pthread_rwlock_unlock((pthread_rwlock_t*)(intptr_t)handle);
}

void rwlockWriteLock(int64_t handle) {
    pthread_rwlock_wrlock((pthread_rwlock_t*)(intptr_t)handle);
}

void rwlockWriteUnlock(int64_t handle) {
    pthread_rwlock_unlock((pthread_rwlock_t*)(intptr_t)handle);
}

// ---- Long-lived interactive subprocess (tinox.core.process, PTY or pipes) ---------------------------------------------------
//
// Unlike processRun (blocks until the child exits, returns the full
// captured output), this spawns and returns IMMEDIATELY -- the caller
// drives it incrementally via processWriteStdin/processReadOutput for the
// lifetime of an interactive session (e.g. `kubectl exec -i`), not a single
// request/response. stdout+stderr are merged into one pipe (a real
// terminal has no separate channels either, and a UI showing this doesn't
// need them told apart). Handle is Int64 (GC_malloc'd), same idiom as
// TinoxProcessResult above.

// A real PTY, not a pair of plain pipes: a program reading from a plain
// pipe never gets terminal line-discipline handling, so a lone '\r'
// (what xterm.js and every real terminal emit for Enter -- terminal
// convention, not '\n') is never recognized as a line terminator and a
// shell just buffers it forever waiting for a real '\n'. This was found
// live wiring up ExecWebSocket.tnx: bulk writes ending in an actual
// '\n' worked, but character-by-character keystroke forwarding (ending
// in '\r') silently never produced output. A remote PTY (via `kubectl
// exec -it`) only gets allocated by kubectl when kubectl's OWN local
// stdin is a real PTY too -- verified live, `-it` with a plain-pipe
// local stdin just prints "Unable to use a TTY" and silently behaves
// exactly like `-i` alone. openpty() gives the child a real slave PTY
// as its controlling terminal, so the kernel's tty driver applies real
// line-discipline handling (ICANON: buffers by line, translates the
// child's own echo, delivers signals for Ctrl-C, etc.) exactly like a
// real terminal application expects -- and kubectl in turn sees a real
// local TTY and requests a real remote one.
typedef struct {
    pid_t pid;
    int fd;  // PTY master -- single fd, read AND write (a real terminal has no separate stdin/stdout either)
    int64_t exited;
    int64_t exit_code;
    int piped;
    int stdin_fd;
    int stderr_fd;
    TinoxGrowBuf output[2];
} TinoxInteractiveProcess;

int64_t processSpawnInteractive(int64_t* argv_handle) {
    TinoxArray* argv_arr = (TinoxArray*)argv_handle;
    if (!argv_arr || argv_arr->len < 1) {
        fprintf(stderr, "runtime error: process_spawn_interactive: argv must have at least one element (the program)\n");
        exit(1);
    }
    int64_t argc = argv_arr->len;
    char** argv = malloc((size_t)(argc + 1) * sizeof(char*));
    if (!argv) {
        fprintf(stderr, "runtime error: process_spawn_interactive: out of memory building argv\n");
        exit(1);
    }
    for (int64_t i = 0; i < argc; i++) {
        argv[i] = (char*)argv_arr->data[i];
    }
    argv[argc] = NULL;

    int master_fd;
    pid_t pid = forkpty(&master_fd, NULL, NULL, NULL);
    if (pid < 0) {
        fprintf(stderr, "runtime error: process_spawn_interactive: forkpty() failed: %s\n", strerror(errno));
        exit(1);
    }
    if (pid == 0) {
        // Child -- same fork-safety note as processRun above: no GC/Tinox
        // calls between forkpty() and execvp()/_exit(). forkpty() already
        // set up the slave as our stdin/stdout/stderr and controlling tty.
        execvp(argv[0], argv);
        _exit(127);
    }

    // Parent
    free(argv);
    fcntl(master_fd, F_SETFD, FD_CLOEXEC);

    TinoxInteractiveProcess* p = (TinoxInteractiveProcess*)GC_malloc(sizeof(TinoxInteractiveProcess));
    memset(p, 0, sizeof(*p));
    p->stdin_fd = p->stderr_fd = -1;
    p->pid = pid;
    p->fd = master_fd;
    p->exited = 0;
    p->exit_code = 0;
    return (int64_t)(intptr_t)p;
}

int64_t processSpawnPiped(int64_t* argv_handle, const char* cwd, int64_t* env_handle) {
    TinoxArray* args = (TinoxArray*)argv_handle;
    if (!args || args->len < 1) {
        fprintf(stderr, "runtime error: processSpawnPiped requires a program\n"); exit(1);
    }
    char** argv = calloc((size_t)args->len + 1, sizeof(char*));
    if (!argv) { perror("process argv"); exit(1); }
    for (int64_t i = 0; i < args->len; i++) argv[i] = (char*)args->data[i];
    int input[2], output[2], error[2];
    if (pipe2(input, O_CLOEXEC) || pipe2(output, O_CLOEXEC) || pipe2(error, O_CLOEXEC)) {
        perror("process pipes"); exit(1);
    }
    pid_t pid = tinox_spawn_piped(argv, cwd, (TinoxArray*)env_handle, input, output, error, 1);
    free(argv);
    close(input[0]); close(output[1]); close(error[1]);
    TinoxInteractiveProcess* p = GC_malloc(sizeof(*p));
    memset(p, 0, sizeof(*p));
    p->pid = pid; p->piped = 1;
    p->stdin_fd = input[1]; p->fd = output[0]; p->stderr_fd = error[0];
    if (pid < 0) { p->exited = 1; p->exit_code = 127; close(p->stdin_fd); p->stdin_fd = -1; }
    return (int64_t)(intptr_t)p;
}

void processCloseStdin(int64_t handle) {
    TinoxInteractiveProcess* p = (TinoxInteractiveProcess*)(intptr_t)handle;
    if (p->piped && p->stdin_fd >= 0) { close(p->stdin_fd); p->stdin_fd = -1; }
}

static int64_t tinox_process_millis(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

// Drain BOTH channels while waiting, so a full stderr pipe cannot block a
// JSON response on stdout. Partial lines survive timeouts. One reader per
// process; callers must serialize reads/kill/close operations themselves.
static char* tinox_piped_read(TinoxInteractiveProcess* p, int channel, int line, int64_t timeout) {
    TinoxGrowBuf* b = &p->output[channel];
    int64_t deadline = tinox_process_millis() + (timeout > 0 ? timeout : 0);
    for (;;) {
        char* newline = b->len ? memchr(b->buf, '\n', b->len) : NULL;
        int stream_fd = channel ? p->stderr_fd : p->fd;
        if (b->len && (!line || newline || stream_fd < 0)) {
            size_t n = newline && line ? (size_t)(newline - b->buf) + 1 : b->len;
            char* result = GC_malloc(n + 1);
            memcpy(result, b->buf, n); result[n] = 0;
            memmove(b->buf, b->buf + n, b->len - n);
            b->len -= n;
            return result;
        }
        if (stream_fd < 0) return GC_strdup("");
        int64_t remaining = deadline - tinox_process_millis();
        int wait = timeout < 0 ? -1 : remaining > INT_MAX ? INT_MAX : remaining > 0 ? (int)remaining : 0;
        struct pollfd fds[2] = {{p->fd, POLLIN, 0}, {p->stderr_fd, POLLIN, 0}};
        int ready = poll(fds, 2, wait);
        if (ready < 0 && errno == EINTR) continue;
        if (ready <= 0) return GC_strdup("");
        for (int i = 0; i < 2; i++) {
            if (!(fds[i].revents & (POLLIN | POLLHUP | POLLERR))) continue;
            char buf[8192];
            ssize_t n = read(fds[i].fd, buf, sizeof(buf));
            if (n > 0) tinox_growbuf_append(&p->output[i], buf, (size_t)n);
            else if (n == 0 || (errno != EINTR && errno != EAGAIN)) {
                close(fds[i].fd);
                if (i) p->stderr_fd = -1; else p->fd = -1;
            }
        }
        // Still return any completed line from this iteration before timeout.
        if (timeout >= 0 && tinox_process_millis() >= deadline) {
            newline = b->len ? memchr(b->buf, '\n', b->len) : NULL;
            stream_fd = channel ? p->stderr_fd : p->fd;
            if (!(b->len && (!line || newline || stream_fd < 0))) return GC_strdup("");
        }
    }
}

char* processReadStderr(int64_t handle, int64_t timeout) {
    TinoxInteractiveProcess* p = (TinoxInteractiveProcess*)(intptr_t)handle;
    return p->piped ? tinox_piped_read(p, 1, 0, timeout) : GC_strdup("");
}
char* processReadLine(int64_t handle, int64_t timeout) {
    TinoxInteractiveProcess* p = (TinoxInteractiveProcess*)(intptr_t)handle;
    return p->piped ? tinox_piped_read(p, 0, 1, timeout) : GC_strdup("");
}

// Best-effort: on a write error (e.g. the child already exited and closed
// its stdin) this simply stops writing rather than raising anything --
// the caller finds out the process is gone via processIsAlive, same as a
// real terminal just stops accepting input once the shell underneath it
// has exited.
void processWriteStdin(int64_t handle, const char* data) {
    TinoxInteractiveProcess* p = (TinoxInteractiveProcess*)(intptr_t)handle;
    size_t len = strlen(data);
    size_t written = 0;
    while (written < len) {
        ssize_t n = write(p->piped ? p->stdin_fd : p->fd, data + written, len - written);
        if (n > 0) { written += (size_t)n; continue; }
        if (n < 0 && errno == EINTR) continue;
        break;
    }
}

// Blocks up to timeoutMs waiting for output; returns whatever's available
// (possibly a partial line), or "" on timeout/EOF/error. A negative timeout
// waits indefinitely; zero polls without waiting.
char* processReadOutput(int64_t handle, int64_t timeoutMs) {
    TinoxInteractiveProcess* p = (TinoxInteractiveProcess*)(intptr_t)handle;
    if (p->piped) return tinox_piped_read(p, 0, 0, timeoutMs);
    struct pollfd fd;
    fd.fd = p->fd;
    fd.events = POLLIN;
    int pr = poll(&fd, 1, (int)timeoutMs);
    if (pr <= 0) {
        return GC_strdup("");
    }
    if (!(fd.revents & (POLLIN | POLLHUP | POLLERR))) {
        return GC_strdup("");
    }
    char buf[8192];
    // A PTY master's read() returns -1/EIO (not 0) once the slave side is
    // gone (child exited) -- the n<=0 check below already treats that the
    // same as a plain pipe's EOF, which is exactly the behavior wanted here.
    ssize_t n = read(p->fd, buf, sizeof(buf));
    if (n <= 0) {
        return GC_strdup("");
    }
    char* result = (char*)GC_malloc((size_t)n + 1);
    memcpy(result, buf, (size_t)n);
    result[n] = '\0';
    return result;
}

int64_t processIsAlive(int64_t handle) {
    TinoxInteractiveProcess* p = (TinoxInteractiveProcess*)(intptr_t)handle;
    if (p->exited) {
        return 0;
    }
    int status = 0;
    pid_t r;
    do { r = waitpid(p->pid, &status, WNOHANG); } while (r < 0 && errno == EINTR);
    if (r == 0) {
        return 1;
    }
    p->exited = 1;
    if (r < 0) p->exit_code = -1;
    else if (WIFEXITED(status)) p->exit_code = WEXITSTATUS(status);
    else if (WIFSIGNALED(status)) p->exit_code = -WTERMSIG(status);
    return 0;
}

// INT64_MIN means still running; every exit code (including negative
// signal numbers) remains distinguishable from that sentinel.
int64_t processExitCode(int64_t handle) {
    if (processIsAlive(handle)) return INT64_MIN;
    return ((TinoxInteractiveProcess*)(intptr_t)handle)->exit_code;
}

// Sends `sig` to a still-running process; pipe-mode children get it
// through their process group, so helpers they started receive it too.
// Once the child itself has exited nothing is signalled any more.
// Returns 1 when a signal was delivered, 0 when there was nothing to signal.
int64_t processSignal(int64_t handle, int64_t sig) {
    TinoxInteractiveProcess* p = (TinoxInteractiveProcess*)(intptr_t)handle;
    if (sig < 1 || sig > 64) {
        fprintf(stderr, "runtime error: processSignal: invalid signal number %ld\n", (long)sig);
        exit(1);
    }
    // Only while the leader is unreaped: a zombie still owns its pid, so
    // -pid cannot name a recycled, unrelated process group.
    if (!processIsAlive(handle)) return 0;
    if (p->piped) return kill(-p->pid, (int)sig) == 0 ? 1 : 0;
    return kill(p->pid, (int)sig) == 0 ? 1 : 0;
}

// Waits up to timeoutMs (< 0 forever, 0 polls) until stdout or stderr of a
// pipe-mode child has data or reached EOF, buffering what it reads so the
// matching read call returns immediately. Result bit mask: 1 = stdout data
// buffered, 2 = stderr data buffered, 4 = both streams are at EOF and
// drained. 0 = timeout. Callers must serialise it with the other reads.
int64_t processWaitReadable(int64_t handle, int64_t timeout) {
    TinoxInteractiveProcess* p = (TinoxInteractiveProcess*)(intptr_t)handle;
    if (!p->piped) return 0;
    int64_t deadline = tinox_process_millis() + (timeout > 0 ? timeout : 0);
    for (;;) {
        int64_t mask = 0;
        if (p->output[0].len) mask |= 1;
        if (p->output[1].len) mask |= 2;
        if (!mask && p->fd < 0 && p->stderr_fd < 0) mask = 4;
        if (mask) return mask;
        int64_t remaining = deadline - tinox_process_millis();
        int wait = timeout < 0 ? -1 : remaining > INT_MAX ? INT_MAX : remaining > 0 ? (int)remaining : 0;
        struct pollfd fds[2] = {{p->fd, POLLIN, 0}, {p->stderr_fd, POLLIN, 0}};
        int ready = poll(fds, 2, wait);
        if (ready < 0 && errno == EINTR) continue;
        if (ready <= 0) return 0;
        for (int i = 0; i < 2; i++) {
            if (!(fds[i].revents & (POLLIN | POLLHUP | POLLERR))) continue;
            char buf[8192];
            ssize_t n = read(fds[i].fd, buf, sizeof(buf));
            if (n > 0) tinox_growbuf_append(&p->output[i], buf, (size_t)n);
            else if (n == 0 || (errno != EINTR && errno != EAGAIN)) {
                close(fds[i].fd);
                if (i) p->stderr_fd = -1; else p->fd = -1;
            }
        }
    }
}

void processKillInteractive(int64_t handle) {
    TinoxInteractiveProcess* p = (TinoxInteractiveProcess*)(intptr_t)handle;
    if (processIsAlive(handle)) {
        // A pipe-mode child leads its own group: take down everything it
        // started. (Not after the leader was reaped: the group id could be
        // recycled by then, see processSignal.)
        if (p->piped) kill(-p->pid, SIGKILL);
        kill(p->pid, SIGKILL);
        int status = 0;
        pid_t r;
        do { r = waitpid(p->pid, &status, 0); } while (r < 0 && errno == EINTR);
        p->exit_code = r < 0 ? -1 : WIFSIGNALED(status) ? -WTERMSIG(status) : WEXITSTATUS(status);
        p->exited = 1;
    }
    if (p->fd >= 0) { close(p->fd); p->fd = -1; }
    processCloseStdin(handle);
    if (p->stderr_fd >= 0) { close(p->stderr_fd); p->stderr_fd = -1; }
    for (int i = 0; i < 2; i++) {
        free(p->output[i].buf);
        memset(&p->output[i], 0, sizeof(p->output[i]));
    }
}

// ---- Directory builtins ----

#include <dirent.h>
#include <sys/stat.h>

char* dirList(const char* path) {
    // Returns a Tinox array handle of filename strings
    int64_t* nh = tinox_array_new(0, 32);
    DIR* d = opendir(path);
    if (!d) return (char*)nh;
    struct dirent* ent;
    while ((ent = readdir(d)) != NULL) {
        if (strcmp(ent->d_name, ".") == 0 || strcmp(ent->d_name, "..") == 0) continue;
        tinox_array_push(nh, (int64_t)GC_strdup(ent->d_name));
    }
    closedir(d);
    return (char*)nh;
}

void dirCreate(const char* path) {
    mkdir(path, 0755);
}

char* dirCreateChecked(const char* path) {
    if (mkdir(path, 0755) != 0) return tinox_fs_err("cannot create directory", path);
    return (char*)"";
}

// Like `mkdir -p`: creates missing parents, succeeds if it already exists.
char* dirCreateAllChecked(const char* path) {
    size_t len = strlen(path);
    char* buf = (char*)GC_malloc(len + 2);
    memcpy(buf, path, len + 1);
    for (size_t i = 1; i <= len; i++) {
        if (buf[i] == '/' || buf[i] == '\0') {
            char saved = buf[i];
            buf[i] = '\0';
            if (mkdir(buf, 0755) != 0 && errno != EEXIST) return tinox_fs_err("cannot create directory", path);
            buf[i] = saved;
        }
    }
    struct stat st;
    if (stat(path, &st) != 0 || !S_ISDIR(st.st_mode)) return tinox_fs_err("cannot create directory", path);
    return (char*)"";
}

void dirDelete(const char* path) {
    rmdir(path);
}

// Checked filesystem metadata and mutation ABI. Arrays begin with an error
// string (empty on success); all following strings are owned GC snapshots.
#include <pwd.h>

static const char* tinox_fs_kind(mode_t mode) {
    return S_ISREG(mode) ? "file" : S_ISDIR(mode) ? "directory" :
           S_ISLNK(mode) ? "symlink" : "other";
}

static int tinox_fs_metadata_push(int64_t* out, const char* name, const struct stat* st) {
    __int128 ns = (__int128)st->st_mtim.tv_sec * 1000000000 + st->st_mtim.tv_nsec;
    if (ns < INT64_MIN || ns > INT64_MAX || st->st_size < 0) {
        errno = EOVERFLOW;
        return -1;
    }
    char size[32], modified[32], permissions[32];
    snprintf(size, sizeof(size), "%lld", (long long)st->st_size);
    snprintf(modified, sizeof(modified), "%lld", (long long)ns);
    snprintf(permissions, sizeof(permissions), "%u", (unsigned)(st->st_mode & 07777));
    tinox_array_push(out, (int64_t)GC_strdup(name));
    tinox_array_push(out, (int64_t)GC_strdup(tinox_fs_kind(st->st_mode)));
    tinox_array_push(out, (int64_t)GC_strdup(size));
    tinox_array_push(out, (int64_t)GC_strdup(modified));
    tinox_array_push(out, (int64_t)GC_strdup(permissions));
    return 0;
}

int64_t* fsMetadataChecked(const char* path, int64_t listing) {
    int64_t* out = tinox_array_new(0, 32);
    tinox_array_push(out, (int64_t)"");
    struct stat st;
    if (!listing) {
        // A trailing slash must not turn a final symlink into a followed
        // directory: this API always describes the named entry itself.
        char* entry_path = GC_strdup(path);
        size_t len = strlen(entry_path);
        while (len > 1 && entry_path[len - 1] == '/') entry_path[--len] = '\0';
        if (lstat(entry_path, &st) != 0) {
            if (errno == ENOENT || errno == ENOTDIR) return out;
            goto failure;
        }
        if (tinox_fs_metadata_push(out, path, &st) != 0) goto failure;
        return out;
    }
    DIR* dir = opendir(path);
    if (!dir) goto failure;
    for (;;) {
        errno = 0;
        struct dirent* entry = readdir(dir);
        if (!entry) {
            int saved = errno;
            if (closedir(dir) != 0 && !saved) saved = errno;
            if (saved) { errno = saved; goto failure; }
            return out;
        }
        if (!strcmp(entry->d_name, ".") || !strcmp(entry->d_name, "..")) continue;
        if (fstatat(dirfd(dir), entry->d_name, &st, AT_SYMLINK_NOFOLLOW) != 0 ||
            tinox_fs_metadata_push(out, entry->d_name, &st) != 0) {
            int saved = errno;
            closedir(dir);
            errno = saved;
            goto failure;
        }
    }
failure:;
    char* error = tinox_fs_err("cannot read metadata", path);
    // Discard partial listings: callers must never mistake partial success
    // for a complete directory snapshot.
    out = tinox_array_new(0, 1);
    tinox_array_push(out, (int64_t)error);
    return out;
}

char* fsRenameChecked(const char* from, const char* to) {
    if (rename(from, to) == 0) return (char*)"";
    if (errno == EXDEV) return (char*)"cannot rename: EXDEV (cross-device rename is not atomic)";
    return tinox_fs_err("cannot rename", from);
}

// Open each parent component without following a symlink. Destructive walks
// are anchored to these descriptors, even if a concurrent actor renames paths.
static int tinox_fs_parent(const char* path, char** name) {
    char* copy = GC_strdup(path);
    size_t len = strlen(copy);
    while (len > 1 && copy[len - 1] == '/') copy[--len] = '\0';
    char* last = strrchr(copy, '/');
    *name = last ? last + 1 : copy;
    if (!**name || !strcmp(*name, ".") || !strcmp(*name, "..")) { errno = EINVAL; return -1; }
    int fd = open(copy[0] == '/' ? "/" : ".", O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    if (fd < 0) return -1;
    if (!last) return fd;
    *last = '\0';
    char* save = NULL;
    for (char* part = strtok_r(copy, "/", &save); part; part = strtok_r(NULL, "/", &save)) {
        int next = openat(fd, part, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
        int saved = errno;
        close(fd);
        if (next < 0) { errno = saved; return -1; }
        fd = next;
    }
    return fd;
}

static int tinox_fs_remove_at(int parent, const char* name) {
    struct stat before, opened, current;
    if (fstatat(parent, name, &before, AT_SYMLINK_NOFOLLOW) != 0) return -1;
    if (!S_ISDIR(before.st_mode)) return unlinkat(parent, name, 0);
    int fd = openat(parent, name, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
    if (fd < 0) return -1;
    if (fstat(fd, &opened) != 0 || before.st_dev != opened.st_dev || before.st_ino != opened.st_ino) {
        close(fd); errno = ESTALE; return -1;
    }
    DIR* dir = fdopendir(fd);
    if (!dir) { int saved = errno; close(fd); errno = saved; return -1; }
    int result = 0;
    for (;;) {
        errno = 0;
        struct dirent* entry = readdir(dir);
        if (!entry) { if (errno) result = -1; break; }
        if (!strcmp(entry->d_name, ".") || !strcmp(entry->d_name, "..")) continue;
        if (tinox_fs_remove_at(fd, entry->d_name) != 0) { result = -1; break; }
    }
    int saved = errno;
    closedir(dir);
    if (result) { errno = saved; return -1; }
    if (fstatat(parent, name, &current, AT_SYMLINK_NOFOLLOW) != 0) return -1;
    if (current.st_dev != opened.st_dev || current.st_ino != opened.st_ino) { errno = ESTALE; return -1; }
    return unlinkat(parent, name, AT_REMOVEDIR);
}

char* fsRemoveDirectoryChecked(const char* path, int64_t recursive) {
    char* name;
    int parent = tinox_fs_parent(path, &name);
    if (parent < 0) return tinox_fs_err("cannot remove directory", path);
    struct stat st;
    int result;
    if (recursive && (fstatat(parent, name, &st, AT_SYMLINK_NOFOLLOW) != 0)) result = -1;
    else if (recursive && !S_ISDIR(st.st_mode) && !S_ISLNK(st.st_mode)) { errno = ENOTDIR; result = -1; }
    else result = recursive ? tinox_fs_remove_at(parent, name) : unlinkat(parent, name, AT_REMOVEDIR);
    int saved = errno;
    close(parent);
    if (result == 0) return (char*)"";
    errno = saved;
    return tinox_fs_err("cannot remove directory", path);
}

static int64_t* tinox_fs_string_result(char* value, const char* operation, const char* path) {
    int64_t* out = tinox_array_new(0, 2);
    tinox_array_push(out, (int64_t)(value ? "" : tinox_fs_err(operation, path)));
    if (value) tinox_array_push(out, (int64_t)GC_strdup(value));
    return out;
}

int64_t* fsRealPathChecked(const char* path) {
    char* resolved = realpath(path, NULL);
    int64_t* out = tinox_fs_string_result(resolved, "cannot resolve real path", path);
    (free)(resolved);
    return out;
}

int64_t* fsHomeDirectoryChecked(void) {
    const char* home = getenv("HOME");
    if (home && home[0]) return tinox_fs_string_result((char*)home, "cannot find home directory", "HOME");
    size_t size = 1024;
    struct passwd record, *result = NULL;
    for (;;) {
        char* buffer = malloc(size);
        if (!buffer) { errno = ENOMEM; return tinox_fs_string_result(NULL, "cannot find home directory", "passwd"); }
        int error = getpwuid_r(getuid(), &record, buffer, size, &result);
        if (error == ERANGE && size < 1048576) { free(buffer); size *= 2; continue; }
        errno = error ? error : ENOENT;
        int64_t* out = tinox_fs_string_result(!error && result && result->pw_dir[0] ? result->pw_dir : NULL,
                                            "cannot find home directory", "passwd");
        free(buffer);
        return out;
    }
}

// ---- Crypto/hashing builtins (MD5, SHA-256, HMAC-SHA256) ----
// Self-contained (no OpenSSL dependency, matches this file's existing
// "no external libs unless opted in via tinox.toml" convention).

static const uint32_t md5_K[64] = {
    0xd76aa478,0xe8c7b756,0x242070db,0xc1bdceee,0xf57c0faf,0x4787c62a,0xa8304613,0xfd469501,
    0x698098d8,0x8b44f7af,0xffff5bb1,0x895cd7be,0x6b901122,0xfd987193,0xa679438e,0x49b40821,
    0xf61e2562,0xc040b340,0x265e5a51,0xe9b6c7aa,0xd62f105d,0x02441453,0xd8a1e681,0xe7d3fbc8,
    0x21e1cde6,0xc33707d6,0xf4d50d87,0x455a14ed,0xa9e3e905,0xfcefa3f8,0x676f02d9,0x8d2a4c8a,
    0xfffa3942,0x8771f681,0x6d9d6122,0xfde5380c,0xa4beea44,0x4bdecfa9,0xf6bb4b60,0xbebfbc70,
    0x289b7ec6,0xeaa127fa,0xd4ef3085,0x04881d05,0xd9d4d039,0xe6db99e5,0x1fa27cf8,0xc4ac5665,
    0xf4292244,0x432aff97,0xab9423a7,0xfc93a039,0x655b59c3,0x8f0ccc92,0xffeff47d,0x85845dd1,
    0x6fa87e4f,0xfe2ce6e0,0xa3014314,0x4e0811a1,0xf7537e82,0xbd3af235,0x2ad7d2bb,0xeb86d391
};
static const int md5_S[64] = {
    7,12,17,22, 7,12,17,22, 7,12,17,22, 7,12,17,22,
    5, 9,14,20, 5, 9,14,20, 5, 9,14,20, 5, 9,14,20,
    4,11,16,23, 4,11,16,23, 4,11,16,23, 4,11,16,23,
    6,10,15,21, 6,10,15,21, 6,10,15,21, 6,10,15,21
};

static uint32_t md5_rotl(uint32_t x, int c) { return (x << c) | (x >> (32 - c)); }

static void md5_transform(uint32_t state[4], const unsigned char block[64]) {
    uint32_t a = state[0], b = state[1], c = state[2], d = state[3];
    uint32_t m[16];
    for (int i = 0; i < 16; i++) {
        m[i] = (uint32_t)block[i*4] | ((uint32_t)block[i*4+1] << 8) |
               ((uint32_t)block[i*4+2] << 16) | ((uint32_t)block[i*4+3] << 24);
    }
    for (int i = 0; i < 64; i++) {
        uint32_t f; int g;
        if (i < 16) { f = (b & c) | (~b & d); g = i; }
        else if (i < 32) { f = (d & b) | (~d & c); g = (5*i + 1) % 16; }
        else if (i < 48) { f = b ^ c ^ d; g = (3*i + 5) % 16; }
        else { f = c ^ (b | ~d); g = (7*i) % 16; }
        uint32_t temp = d;
        d = c;
        c = b;
        b = b + md5_rotl(a + f + md5_K[i] + m[g], md5_S[i]);
        a = temp;
    }
    state[0] += a; state[1] += b; state[2] += c; state[3] += d;
}

static void md5_raw(const unsigned char* data, size_t len, unsigned char out[16]) {
    uint32_t state[4] = {0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476};
    uint64_t bitlen = (uint64_t)len * 8;
    size_t padded_len = ((len + 8) / 64 + 1) * 64;
    unsigned char* msg = (unsigned char*)calloc(1, padded_len);
    memcpy(msg, data, len);
    msg[len] = 0x80;
    for (int i = 0; i < 8; i++) {
        msg[padded_len - 8 + i] = (unsigned char)(bitlen >> (8*i)); // little-endian length
    }
    for (size_t off = 0; off < padded_len; off += 64) {
        md5_transform(state, msg + off);
    }
    free(msg);
    for (int i = 0; i < 4; i++) {
        out[i*4]   = (unsigned char)(state[i]);
        out[i*4+1] = (unsigned char)(state[i] >> 8);
        out[i*4+2] = (unsigned char)(state[i] >> 16);
        out[i*4+3] = (unsigned char)(state[i] >> 24);
    }
}

static const uint32_t sha256_K[64] = {
    0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
    0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
    0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
    0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
    0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
    0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
    0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
    0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2
};

static uint32_t sha256_rotr(uint32_t x, int n) { return (x >> n) | (x << (32 - n)); }

static void sha256_transform(uint32_t state[8], const unsigned char block[64]) {
    uint32_t w[64];
    for (int i = 0; i < 16; i++) {
        w[i] = ((uint32_t)block[i*4] << 24) | ((uint32_t)block[i*4+1] << 16) |
               ((uint32_t)block[i*4+2] << 8) | (uint32_t)block[i*4+3];
    }
    for (int i = 16; i < 64; i++) {
        uint32_t s0 = sha256_rotr(w[i-15], 7) ^ sha256_rotr(w[i-15], 18) ^ (w[i-15] >> 3);
        uint32_t s1 = sha256_rotr(w[i-2], 17) ^ sha256_rotr(w[i-2], 19) ^ (w[i-2] >> 10);
        w[i] = w[i-16] + s0 + w[i-7] + s1;
    }
    uint32_t a=state[0],b=state[1],c=state[2],d=state[3],e=state[4],f=state[5],g=state[6],h=state[7];
    for (int i = 0; i < 64; i++) {
        uint32_t s1 = sha256_rotr(e,6) ^ sha256_rotr(e,11) ^ sha256_rotr(e,25);
        uint32_t ch = (e & f) ^ (~e & g);
        uint32_t temp1 = h + s1 + ch + sha256_K[i] + w[i];
        uint32_t s0 = sha256_rotr(a,2) ^ sha256_rotr(a,13) ^ sha256_rotr(a,22);
        uint32_t maj = (a & b) ^ (a & c) ^ (b & c);
        uint32_t temp2 = s0 + maj;
        h=g; g=f; f=e; e=d+temp1; d=c; c=b; b=a; a=temp1+temp2;
    }
    state[0]+=a; state[1]+=b; state[2]+=c; state[3]+=d;
    state[4]+=e; state[5]+=f; state[6]+=g; state[7]+=h;
}

static void sha256_raw(const unsigned char* data, size_t len, unsigned char out[32]) {
    uint32_t state[8] = {
        0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,
        0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19
    };
    uint64_t bitlen = (uint64_t)len * 8;
    size_t padded_len = ((len + 8) / 64 + 1) * 64;
    unsigned char* msg = (unsigned char*)calloc(1, padded_len);
    memcpy(msg, data, len);
    msg[len] = 0x80;
    for (int i = 0; i < 8; i++) {
        msg[padded_len - 1 - i] = (unsigned char)(bitlen >> (8*i)); // big-endian length
    }
    for (size_t off = 0; off < padded_len; off += 64) {
        sha256_transform(state, msg + off);
    }
    free(msg);
    for (int i = 0; i < 8; i++) {
        out[i*4]   = (unsigned char)(state[i] >> 24);
        out[i*4+1] = (unsigned char)(state[i] >> 16);
        out[i*4+2] = (unsigned char)(state[i] >> 8);
        out[i*4+3] = (unsigned char)(state[i]);
    }
}

static char* tinox_bytes_to_hex(const unsigned char* bytes, size_t n) {
    char* hex = (char*)GC_malloc(n*2 + 1);
    for (size_t i = 0; i < n; i++) {
        snprintf(hex + i*2, 3, "%02x", bytes[i]);
    }
    return hex;
}

// -1 = invalid hex character (caller MUST check this instead of silently reading 0).
static int tinox_hex_nibble(char c) {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}

char* md5Hash(const char* data) {
    unsigned char out[16];
    md5_raw((const unsigned char*)data, strlen(data), out);
    return tinox_bytes_to_hex(out, 16);
}

char* sha256Hash(const char* data) {
    unsigned char out[32];
    sha256_raw((const unsigned char*)data, strlen(data), out);
    return tinox_bytes_to_hex(out, 32);
}

// RFC 2104. Shared core for both the string variant (NUL-terminated,
// hex-encoded return) AND the bytes variant (issue 77, SCRAM needs HMAC over
// genuine binary data — salt/nonce/digests can contain null bytes,
// which a C-string-based variant would silently truncate).
static void hmac_sha256_raw(const unsigned char* data, size_t data_len,
                             const unsigned char* key, size_t key_len,
                             unsigned char out[32]) {
    unsigned char key_block[64];
    memset(key_block, 0, 64);
    if (key_len > 64) {
        unsigned char key_hash[32];
        sha256_raw(key, key_len, key_hash);
        memcpy(key_block, key_hash, 32);
    } else {
        memcpy(key_block, key, key_len);
    }

    unsigned char o_pad[64], i_pad[64];
    for (int i = 0; i < 64; i++) {
        o_pad[i] = (unsigned char)(key_block[i] ^ 0x5c);
        i_pad[i] = (unsigned char)(key_block[i] ^ 0x36);
    }

    unsigned char* inner_msg = (unsigned char*)malloc(64 + data_len);
    memcpy(inner_msg, i_pad, 64);
    memcpy(inner_msg + 64, data, data_len);
    unsigned char inner_hash[32];
    sha256_raw(inner_msg, 64 + data_len, inner_hash);
    free(inner_msg);

    unsigned char outer_msg[96];
    memcpy(outer_msg, o_pad, 64);
    memcpy(outer_msg + 64, inner_hash, 32);
    sha256_raw(outer_msg, 96, out);
}

char* hmacSha256Hash(const char* data, const char* key) {
    unsigned char final_hash[32];
    hmac_sha256_raw((const unsigned char*)data, strlen(data),
                     (const unsigned char*)key, strlen(key), final_hash);
    return tinox_bytes_to_hex(final_hash, 32);
}

// Bytes variants (issue 77 / SCRAM-SHA-256): Tinox arrays in, Tinox
// arrays out, no C-string conversion anywhere.
int64_t* hmacSha256Bytes(int64_t* dataArr, int64_t* keyArr) {
    TinoxArray* da = (TinoxArray*)dataArr;
    TinoxArray* ka = (TinoxArray*)keyArr;
    unsigned char* data = (unsigned char*)malloc(da->len > 0 ? (size_t)da->len : 1);
    for (int64_t i = 0; i < da->len; i++) data[i] = (unsigned char)(da->data[i] & 0xff);
    unsigned char* key = (unsigned char*)malloc(ka->len > 0 ? (size_t)ka->len : 1);
    for (int64_t i = 0; i < ka->len; i++) key[i] = (unsigned char)(ka->data[i] & 0xff);

    unsigned char out[32];
    hmac_sha256_raw(data, (size_t)da->len, key, (size_t)ka->len, out);
    free(data);
    free(key);

    int64_t* result = tinox_array_new(32, 32);
    TinoxArray* ra = (TinoxArray*)result;
    for (int i = 0; i < 32; i++) ra->data[i] = out[i];
    return result;
}

int64_t* sha256Bytes(int64_t* dataArr) {
    TinoxArray* da = (TinoxArray*)dataArr;
    unsigned char* data = (unsigned char*)malloc(da->len > 0 ? (size_t)da->len : 1);
    for (int64_t i = 0; i < da->len; i++) data[i] = (unsigned char)(da->data[i] & 0xff);

    unsigned char out[32];
    sha256_raw(data, (size_t)da->len, out);
    free(data);

    int64_t* result = tinox_array_new(32, 32);
    TinoxArray* ra = (TinoxArray*)result;
    for (int i = 0; i < 32; i++) ra->data[i] = out[i];
    return result;
}

// ---- SHA-1 (RFC 3174) — needed for the WebSocket handshake ----
// Same security note as for MD5: only for protocol compatibility
// (Sec-WebSocket-Accept), not for new cryptographic purposes.

static uint32_t sha1_rotl(uint32_t x, int n) { return (x << n) | (x >> (32 - n)); }

static void sha1_transform(uint32_t state[5], const unsigned char block[64]) {
    uint32_t w[80];
    for (int i = 0; i < 16; i++) {
        w[i] = ((uint32_t)block[i*4] << 24) | ((uint32_t)block[i*4+1] << 16) |
               ((uint32_t)block[i*4+2] << 8) | (uint32_t)block[i*4+3];
    }
    for (int i = 16; i < 80; i++) {
        w[i] = sha1_rotl(w[i-3] ^ w[i-8] ^ w[i-14] ^ w[i-16], 1);
    }
    uint32_t a=state[0],b=state[1],c=state[2],d=state[3],e=state[4];
    for (int i = 0; i < 80; i++) {
        uint32_t f, k;
        if (i < 20)      { f = (b & c) | ((~b) & d);           k = 0x5a827999; }
        else if (i < 40) { f = b ^ c ^ d;                      k = 0x6ed9eba1; }
        else if (i < 60) { f = (b & c) | (b & d) | (c & d);    k = 0x8f1bbcdc; }
        else             { f = b ^ c ^ d;                      k = 0xca62c1d6; }
        uint32_t tmp = sha1_rotl(a, 5) + f + e + k + w[i];
        e = d; d = c; c = sha1_rotl(b, 30); b = a; a = tmp;
    }
    state[0]+=a; state[1]+=b; state[2]+=c; state[3]+=d; state[4]+=e;
}

static void sha1_raw(const unsigned char* data, size_t len, unsigned char out[20]) {
    uint32_t state[5] = { 0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0 };
    uint64_t bitlen = (uint64_t)len * 8;
    size_t padded_len = ((len + 8) / 64 + 1) * 64;
    unsigned char* msg = (unsigned char*)calloc(1, padded_len);
    memcpy(msg, data, len);
    msg[len] = 0x80;
    for (int i = 0; i < 8; i++) {
        msg[padded_len - 1 - i] = (unsigned char)(bitlen >> (8*i)); // big-endian length
    }
    for (size_t off = 0; off < padded_len; off += 64) {
        sha1_transform(state, msg + off);
    }
    free(msg);
    for (int i = 0; i < 5; i++) {
        out[i*4]   = (unsigned char)(state[i] >> 24);
        out[i*4+1] = (unsigned char)(state[i] >> 16);
        out[i*4+2] = (unsigned char)(state[i] >> 8);
        out[i*4+3] = (unsigned char)(state[i]);
    }
}

char* sha1Hash(const char* data) {
    unsigned char out[20];
    sha1_raw((const unsigned char*)data, strlen(data), out);
    return tinox_bytes_to_hex(out, 20);
}

// Base64 over raw bytes (the Tinox-side base64.tnx works on strings and
// can't carry NUL bytes — the SHA-1 digest can).
static char* tinox_b64_encode(const unsigned char* in, size_t n) {
    static const char tbl[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    size_t out_len = 4 * ((n + 2) / 3);
    char* out = (char*)GC_malloc(out_len + 1);
    size_t o = 0;
    for (size_t i = 0; i < n; i += 3) {
        uint32_t v = (uint32_t)in[i] << 16;
        if (i + 1 < n) v |= (uint32_t)in[i+1] << 8;
        if (i + 2 < n) v |= (uint32_t)in[i+2];
        out[o++] = tbl[(v >> 18) & 63];
        out[o++] = tbl[(v >> 12) & 63];
        out[o++] = (i + 1 < n) ? tbl[(v >> 6) & 63] : '=';
        out[o++] = (i + 2 < n) ? tbl[v & 63] : '=';
    }
    out[o] = '\0';
    return out;
}

// Sec-WebSocket-Accept (RFC 6455 §4.2.2): base64(sha1(key + GUID)). Entirely
// in C, so the binary SHA-1 digest never has to pass through a Tinox string.
char* wsAcceptKey(const char* client_key) {
    static const char* guid = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    size_t klen = strlen(client_key), glen = strlen(guid);
    unsigned char* buf = (unsigned char*)malloc(klen + glen);
    memcpy(buf, client_key, klen);
    memcpy(buf + klen, guid, glen);
    unsigned char digest[20];
    sha1_raw(buf, klen + glen, digest);
    free(buf);
    return tinox_b64_encode(digest, 20);
}

// permessage-deflate (RFC 7692 §7.2.1) raw DEFLATE (windowBits=-15: no
// zlib/gzip header) compress/decompress. Deliberately stateless per call
// — issue #122's chosen design always negotiates
// client_no_context_takeover + server_no_context_takeover (see
// Ws::handshake), so every message gets a fresh DEFLATE context; there
// is no persistent per-connection z_stream to leak or wire into the
// connection lifecycle. §7.2.1: the compressor always ends a message
// with Z_SYNC_FLUSH's trailing 4-byte 0x00 0x00 0xFF 0xFF marker, which
// the sender discards and the receiver re-appends before inflating —
// both sides below implement exactly that.
int64_t* tinoxDeflateRaw(int64_t* bytes) {
    TinoxArray* a = (TinoxArray*)bytes;
    int64_t n = a ? a->len : 0;
    unsigned char* in = (unsigned char*)malloc(n > 0 ? (size_t)n : 1);
    for (int64_t i = 0; i < n; i++) in[i] = (unsigned char)(a->data[i] & 0xff);

    z_stream strm;
    memset(&strm, 0, sizeof(strm));
    if (deflateInit2(&strm, Z_DEFAULT_COMPRESSION, Z_DEFLATED, -15, 8, Z_DEFAULT_STRATEGY) != Z_OK) {
        free(in);
        return tinox_array_new(0, 4);
    }
    strm.next_in = in;
    strm.avail_in = (uInt)n;

    size_t cap = (size_t)(n > 0 ? n : 1) + 64;
    unsigned char* out = (unsigned char*)malloc(cap);
    size_t total = 0;
    for (;;) {
        strm.next_out = out + total;
        strm.avail_out = (uInt)(cap - total);
        deflate(&strm, Z_SYNC_FLUSH); // can't fail once deflateInit2 succeeded
        total = cap - strm.avail_out;
        if (strm.avail_out > 0) break; // this call didn't need the whole buffer -> done
        cap *= 2;
        out = (unsigned char*)realloc(out, cap);
    }
    deflateEnd(&strm);
    free(in);

    if (total >= 4 && out[total-4] == 0x00 && out[total-3] == 0x00
        && out[total-2] == 0xFF && out[total-1] == 0xFF) {
        total -= 4;
    }

    int64_t* nh = tinox_array_new(0, total > 0 ? (int64_t)total : 4);
    for (size_t i = 0; i < total; i++) tinox_array_push(nh, out[i]);
    free(out);
    return nh;
}

// Thread-local companion to tinoxInflateRaw (mirrors this file's
// existing _tinox_http_req_headers thread-local pattern) — tinoxInflateRaw
// itself must return List<Int64> (matching its Tinox extern fn
// signature), so a distinguishable "decompression failed" signal (kein
// Silent-Garbage: a truncated/malformed/bomb payload must not silently
// look like a valid empty message) travels out-of-band via this flag,
// checked by the caller immediately after each call.
static __thread bool _tinox_inflate_ok = true;

bool wsLastInflateOk(void) {
    return _tinox_inflate_ok;
}

int64_t* tinoxInflateRaw(int64_t* bytes) {
    _tinox_inflate_ok = true;
    TinoxArray* a = (TinoxArray*)bytes;
    int64_t n = a ? a->len : 0;
    unsigned char* in = (unsigned char*)malloc((size_t)n + 4);
    for (int64_t i = 0; i < n; i++) in[i] = (unsigned char)(a->data[i] & 0xff);
    in[n] = 0x00; in[n+1] = 0x00; in[n+2] = 0xFF; in[n+3] = 0xFF;

    z_stream strm;
    memset(&strm, 0, sizeof(strm));
    if (inflateInit2(&strm, -15) != Z_OK) {
        free(in);
        _tinox_inflate_ok = false;
        return tinox_array_new(0, 4);
    }
    strm.next_in = in;
    strm.avail_in = (uInt)(n + 4);

    size_t cap = 4096;
    unsigned char* out = (unsigned char*)malloc(cap);
    size_t total = 0;
    bool failed = false;
    for (;;) {
        strm.next_out = out + total;
        strm.avail_out = (uInt)(cap - total);
        uInt avail_in_before = strm.avail_in;
        uInt avail_out_before = strm.avail_out;
        int ret = inflate(&strm, Z_NO_FLUSH);
        total = cap - strm.avail_out;
        // Decompression-bomb cap: matches Ws::readMessageGeneric's 16MB
        // reassembled-message limit -- a small compressed payload
        // expanding far past that is treated as an attack, not data.
        if (total > 16777216) { failed = true; break; }
        if (ret == Z_STREAM_END) break;
        if (ret != Z_OK && ret != Z_BUF_ERROR) { failed = true; break; }
        if (strm.avail_in == avail_in_before && strm.avail_out == avail_out_before) {
            // No progress this call: genuinely done if there's no input
            // left to give it, otherwise the stream is truncated/stuck.
            if (strm.avail_in == 0) break;
            failed = true;
            break;
        }
        if (strm.avail_out == 0) {
            cap *= 2;
            out = (unsigned char*)realloc(out, cap);
        }
    }
    inflateEnd(&strm);
    free(in);

    if (failed) {
        free(out);
        _tinox_inflate_ok = false;
        return tinox_array_new(0, 4);
    }

    int64_t* nh = tinox_array_new(0, total > 0 ? (int64_t)total : 4);
    for (size_t i = 0; i < total; i++) tinox_array_push(nh, out[i]);
    free(out);
    return nh;
}

// ---- General-purpose gzip (tinox.core.compress, issue #132) ----
//
// Same zlib deflate/inflate as tinoxDeflateRaw/tinoxInflateRaw above, but
// windowBits=15+16 instead of -15: tells zlib to wrap the stream in a full
// RFC 1952 gzip container (magic bytes, CM/flags/mtime/XFL/OS header, then
// the deflate stream, then a CRC32 + ISIZE trailer) instead of bare raw
// DEFLATE -- the format most external tools/HTTP Content-Encoding: gzip
// expect. zlib computes and verifies that CRC32/ISIZE trailer internally
// as part of the gzip framing, so unlike the issue's suggested approach
// there's no need to call zlib's crc32() separately here -- a mismatched
// trailer on decompression already surfaces as inflate() returning
// Z_DATA_ERROR, caught by the same failure path as any other malformed
// stream below.
int64_t* tinoxGzip(int64_t* bytes) {
    TinoxArray* a = (TinoxArray*)bytes;
    int64_t n = a ? a->len : 0;
    unsigned char* in = (unsigned char*)malloc(n > 0 ? (size_t)n : 1);
    for (int64_t i = 0; i < n; i++) in[i] = (unsigned char)(a->data[i] & 0xff);

    z_stream strm;
    memset(&strm, 0, sizeof(strm));
    if (deflateInit2(&strm, Z_DEFAULT_COMPRESSION, Z_DEFLATED, 15 + 16, 8, Z_DEFAULT_STRATEGY) != Z_OK) {
        free(in);
        return tinox_array_new(0, 4);
    }
    strm.next_in = in;
    strm.avail_in = (uInt)n;

    size_t cap = (size_t)(n > 0 ? n : 1) + 64;
    unsigned char* out = (unsigned char*)malloc(cap);
    size_t total = 0;
    int ret;
    for (;;) {
        strm.next_out = out + total;
        strm.avail_out = (uInt)(cap - total);
        ret = deflate(&strm, Z_FINISH); // can't fail once deflateInit2 succeeded
        total = cap - strm.avail_out;
        if (ret == Z_STREAM_END) break;
        cap *= 2;
        out = (unsigned char*)realloc(out, cap);
    }
    deflateEnd(&strm);
    free(in);

    int64_t* nh = tinox_array_new(0, total > 0 ? (int64_t)total : 4);
    for (size_t i = 0; i < total; i++) tinox_array_push(nh, out[i]);
    free(out);
    return nh;
}

// Compact gzip boundary: input goes straight to zlib, output never widens
// to List<Int64>. A caller-supplied ceiling bounds decompression bombs.
int64_t* tinoxBufGzip(int64_t handle, int64_t maximum, int64_t decode) {
    TinoxBuf* input = tinox_buf_of(handle, "gzip");
    int64_t* result = tinox_array_new(0, 2);
    int error = 0;
    TinoxBuf* output = NULL;
    if (maximum <= 0 || maximum > 67108864 || input->len > UINT_MAX) error = EINVAL;
    z_stream stream = {0};
    bool initialized = false;
    if (!error) {
        int status = decode ? inflateInit2(&stream, 31)
                            : deflateInit2(&stream, Z_DEFAULT_COMPRESSION, Z_DEFLATED, 31, 8, Z_DEFAULT_STRATEGY);
        if (status != Z_OK) error = ENOMEM;
        else initialized = true;
    }
    if (!error) {
        size_t capacity = maximum < 4096 ? (size_t)maximum : 4096;
        output = tinox_buf_alloc((int64_t)capacity);
        stream.next_in = input->data;
        stream.avail_in = (uInt)input->len;
        for (;;) {
            if (task_cancellation(task_scope)) { error = ECANCELED; break; }
            size_t used = stream.total_out;
            if (used == capacity) {
                if (capacity == (size_t)maximum) {
                    // Distinguish an exactly full output from overflow: let
                    // zlib consume only a trailer using one scratch byte.
                    unsigned char extra;
                    stream.next_out = &extra; stream.avail_out = 1;
                    int status = decode ? inflate(&stream, Z_FINISH) : deflate(&stream, Z_FINISH);
                    if (stream.total_out > used || status != Z_STREAM_END) error = EFBIG;
                    if (!error && stream.avail_in != 0) error = EILSEQ;
                    break;
                }
                capacity = capacity * 2 > (size_t)maximum ? (size_t)maximum : capacity * 2;
                output = GC_realloc(output, sizeof(TinoxBuf) + capacity);
            }
            stream.next_out = output->data + stream.total_out;
            stream.avail_out = (uInt)(capacity - stream.total_out);
            uLong before_in = stream.total_in, before_out = stream.total_out;
            int status = decode ? inflate(&stream, Z_NO_FLUSH) : deflate(&stream, Z_FINISH);
            if (status == Z_STREAM_END) {
                if (stream.avail_in != 0) error = EILSEQ; // no hidden trailing members
                break;
            }
            if (status != Z_OK || (stream.total_in == before_in && stream.total_out == before_out)) {
                error = EILSEQ; break;
            }
        }
        output->len = error ? 0 : (int64_t)stream.total_out;
    }
    if (initialized) { if (decode) inflateEnd(&stream); else deflateEnd(&stream); }
    tinox_array_push(result, error);
    tinox_array_push(result, error ? tinoxBufNew(0) : (int64_t)(intptr_t)output);
    return result;
}

// Thread-local companion to tinoxGunzip, same rationale/pattern as
// wsLastInflateOk() above (kein Silent-Garbage: a truncated/malformed
// stream or a CRC32/ISIZE trailer mismatch must not silently look like a
// valid empty result) -- own dedicated flag, not shared with
// _tinox_inflate_ok, so the raw-DEFLATE (WebSocket) and gzip
// (tinox.core.compress) call paths can't clobber each other's status.
static __thread bool _tinox_gunzip_ok = true;

bool tinoxLastGunzipOk(void) {
    return _tinox_gunzip_ok;
}

int64_t* tinoxGunzip(int64_t* bytes) {
    _tinox_gunzip_ok = true;
    TinoxArray* a = (TinoxArray*)bytes;
    int64_t n = a ? a->len : 0;
    unsigned char* in = (unsigned char*)malloc(n > 0 ? (size_t)n : 1);
    for (int64_t i = 0; i < n; i++) in[i] = (unsigned char)(a->data[i] & 0xff);

    z_stream strm;
    memset(&strm, 0, sizeof(strm));
    if (inflateInit2(&strm, 15 + 16) != Z_OK) {
        free(in);
        _tinox_gunzip_ok = false;
        return tinox_array_new(0, 4);
    }
    strm.next_in = in;
    strm.avail_in = (uInt)n;

    size_t cap = 4096;
    unsigned char* out = (unsigned char*)malloc(cap);
    size_t total = 0;
    bool failed = false;
    for (;;) {
        strm.next_out = out + total;
        strm.avail_out = (uInt)(cap - total);
        uInt avail_in_before = strm.avail_in;
        uInt avail_out_before = strm.avail_out;
        int ret = inflate(&strm, Z_NO_FLUSH);
        total = cap - strm.avail_out;
        // Decompression-bomb cap: same 16MB limit as tinoxInflateRaw above.
        if (total > 16777216) { failed = true; break; }
        if (ret == Z_STREAM_END) break;
        if (ret != Z_OK && ret != Z_BUF_ERROR) { failed = true; break; }
        if (strm.avail_in == avail_in_before && strm.avail_out == avail_out_before) {
            // No progress this call. Unlike tinoxInflateRaw's raw DEFLATE
            // (which has no formal end marker of its own), a well-formed
            // complete gzip stream always reaches Z_STREAM_END on its own
            // -- running out of input before that means the trailer
            // (or more) is missing, i.e. truncated, not a valid result.
            failed = true;
            break;
        }
        if (strm.avail_out == 0) {
            cap *= 2;
            out = (unsigned char*)realloc(out, cap);
        }
    }
    inflateEnd(&strm);
    free(in);

    if (failed) {
        free(out);
        _tinox_gunzip_ok = false;
        return tinox_array_new(0, 4);
    }

    int64_t* nh = tinox_array_new(0, total > 0 ? (int64_t)total : 4);
    for (size_t i = 0; i < total; i++) tinox_array_push(nh, out[i]);
    free(out);
    return nh;
}

// ---- Float64/Float32 bit-pattern reinterpretation (issue #136) ----
//
// tinox.core.msgpack's encoder/decoder is written entirely in Tinox
// (mirroring hpack/Hpack.tnx's precedent: hand-roll the protocol framing
// directly over List<Int64> byte arrays, no bespoke runtime primitive
// for the format itself) -- but MessagePack's float32/float64 types are
// IEEE 754 bit patterns, and Tinox arithmetic alone can't reconstruct
// IEEE 754 bit-for-bit (sign/exponent/mantissa extraction without native
// bit ops risks subtle rounding/denormal/NaN bugs, exactly the kind of
// thing a fuzz harness would catch). So unlike the rest of the codec,
// these three are plain reinterpret-casts, not string/array framing
// logic, and stay in C where memcpy makes them trivially, obviously
// correct.
int64_t msgpackFloat64ToBits(double f) {
    int64_t bits;
    memcpy(&bits, &f, sizeof(bits));
    return bits;
}
double msgpackBitsToFloat64(int64_t bits) {
    double f;
    memcpy(&f, &bits, sizeof(f));
    return f;
}
// bits32 arrives as the low 32 bits of an Int64 (the Tinox-side decoder
// already reassembled them big-endian via shift/or into an Int64); only
// the low 32 bits are meaningful here.
double msgpackBitsToFloat32(int64_t bits32) {
    float f;
    int32_t b = (int32_t)(bits32 & 0xffffffff);
    memcpy(&f, &b, sizeof(f));
    return (double)f;
}

// Builds a String from a List<Int64> byte array in one pass (issue #136,
// found by fuzzing: Msgpack::decodeString originally built its result via
// a loop of `s = s + fromCharCode(byte)`, and every `+` on a Tinox string
// (tinox_string_concat below) mallocs a NEW buffer sized to BOTH operands
// and copies both in -- chaining N of those is O(n) per call over a
// string that grows to length n, i.e. O(n^2) total. Confirmed quadratic
// empirically (100k-byte string: ~0.9s; 200k-byte string: ~3.2s, ~3.4x
// not ~2x) before this fix. This primitive collects the bytes into a
// List<Int64> first (tinox_array_push's existing amortized-doubling
// growth, genuinely O(n) total) and does the byte->char conversion in a
// single fixed-size allocation, turning the whole decode back to O(n).
char* tinoxBytesToString(int64_t* bytes) {
    TinoxArray* a = (TinoxArray*)bytes;
    int64_t n = a ? a->len : 0;
    char* buf = (char*)GC_malloc((size_t)n + 1);
    for (int64_t i = 0; i < n; i++) buf[i] = (char)(a->data[i] & 0xff);
    buf[n] = '\0';
    g_json_body_ptr=buf; g_json_body_len=(size_t)n;
    return buf;
}

// ---- Regex builtins ----

#include <regex.h>

// tinox.core.regex dialect: POSIX extended regular expressions plus the Perl
// style classes \d \D \s \S \w \W (translated below; \b \B come from glibc).
// Backreferences, lookaround and lazy quantifiers are not supported.
static char* tinox_regex_translate(const char* pattern, char** error) {
    *error = NULL;
    size_t n = strlen(pattern);
    char* out = (char*)GC_malloc(n * 12 + 1);
    size_t o = 0;
    int in_bracket = 0;
    for (size_t i = 0; i < n; i++) {
        char c = pattern[i];
        if (in_bracket) {
            if (c == '\\' && i + 1 < n && strchr("dsw", pattern[i + 1])) {
                const char* rep = pattern[i + 1] == 'd' ? "0-9" : pattern[i + 1] == 's' ? "[:space:]" : "[:alnum:]_";
                size_t rl = strlen(rep);
                memcpy(out + o, rep, rl);
                o += rl;
                i++;
                continue;
            }
            if (c == '\\' && i + 1 < n && strchr("DSW", pattern[i + 1])) {
                size_t size = strlen(pattern) + 100;
                *error = GC_malloc(size);
                snprintf(*error, size, "invalid regular expression \"%s\": \\%c inside [...] is not supported", pattern, pattern[i + 1]);
                return NULL;
            }
            if (c == '[' && i + 1 < n && pattern[i + 1] == ':') {
                // copy a [:class:] whole
                const char* end = strstr(pattern + i + 2, ":]");
                if (end) {
                    size_t len = (size_t)(end + 2 - (pattern + i));
                    memcpy(out + o, pattern + i, len);
                    o += len;
                    i += len - 1;
                    continue;
                }
            }
            if (c == ']') in_bracket = 0;
            out[o++] = c;
            continue;
        }
        if (c == '\\' && i + 1 < n) {
            const char* rep = NULL;
            switch (pattern[i + 1]) {
                case 'd': rep = "[0-9]"; break;
                case 'D': rep = "[^0-9]"; break;
                case 's': rep = "[[:space:]]"; break;
                case 'S': rep = "[^[:space:]]"; break;
                case 'w': rep = "[[:alnum:]_]"; break;
                case 'W': rep = "[^[:alnum:]_]"; break;
                default: break;
            }
            if (rep) {
                size_t rl = strlen(rep);
                memcpy(out + o, rep, rl);
                o += rl;
            } else {
                out[o++] = c;
                out[o++] = pattern[i + 1];
            }
            i++;
            continue;
        }
        if (c == '[') {
            in_bracket = 1;
            out[o++] = c;
            // a leading ^ and/or ] are literal parts of the bracket expression
            if (i + 1 < n && pattern[i + 1] == '^') out[o++] = pattern[++i];
            if (i + 1 < n && pattern[i + 1] == ']') out[o++] = pattern[++i];
            continue;
        }
        out[o++] = c;
    }
    out[o] = '\0';
    return out;
}

// Compiles `pattern` (see dialect above). An invalid pattern is a hard error:
// it used to be indistinguishable from "no match" (issue #468).
static void tinox_regcomp(regex_t* re, const char* pattern, int cflags) {
    char* error;
    char* translated = tinox_regex_translate(pattern, &error);
    if (error) {
        fprintf(stderr, "runtime error: %s\n", error);
        exit(1);
    }
    int rc = regcomp(re, translated, cflags);
    if (rc != 0) {
        char msg[200];
        regerror(rc, re, msg, sizeof msg);
        fprintf(stderr, "runtime error: invalid regular expression \"%s\": %s\n", pattern, msg);
        exit(1);
    }
}

// Validation shares the execution dialect and flags. Public Regex methods turn
// this diagnostic into a Tinox exception; legacy raw builtins still fail hard.
char* tinoxRegexPatternError(const char* pattern, int64_t icase) {
    regex_t re;
    char* error;
    char* translated = tinox_regex_translate(pattern, &error);
    if (error) return error;
    int rc = regcomp(&re, translated, REG_EXTENDED | (icase ? REG_ICASE : 0));
    if (rc == 0) {
        regfree(&re);
        return GC_strdup("");
    }
    size_t len = regerror(rc, &re, NULL, 0);
    char* detail = GC_malloc(len);
    regerror(rc, &re, detail, len);
    size_t size = strlen(pattern) + len + 48;
    error = GC_malloc(size);
    snprintf(error, size, "invalid regular expression \"%s\": %s", pattern, detail);
    return error;
}

// REG_NOMATCH is an ordinary outcome; engine/resource failures are not.
static int tinox_regexec(regex_t* re, const char* subject, size_t count,
                         regmatch_t* matches, int flags) {
    int rc = regexec(re, subject, count, matches, flags);
    if (rc != 0 && rc != REG_NOMATCH) {
        char detail[200];
        regerror(rc, re, detail, sizeof detail);
        fprintf(stderr, "runtime error: regular expression execution failed: %s\n", detail);
        exit(1);
    }
    return rc;
}

static int64_t regexIsMatch_options(int64_t pattern_i64, int64_t subject_i64, int64_t icase) {
    const char* pattern = (const char*)pattern_i64;
    const char* subject = (const char*)subject_i64;
    regex_t re;
    tinox_regcomp(&re, pattern, REG_EXTENDED | (icase ? REG_ICASE : 0));
    int r = tinox_regexec(&re, subject, 0, NULL, 0);
    regfree(&re);
    return (r == 0) ? 1 : 0;
}

int64_t regexIsMatch(int64_t pattern_i64, int64_t subject_i64) {
    return regexIsMatch_options(pattern_i64, subject_i64, 0);
}

static int64_t regexFindAll_options(int64_t pattern_i64, int64_t subject_i64, int64_t icase) {
    const char* pattern = (const char*)pattern_i64;
    const char* subject = (const char*)subject_i64;
    int64_t* nh = tinox_array_new(0, 8);
    regex_t re;
    tinox_regcomp(&re, pattern, REG_EXTENDED | (icase ? REG_ICASE : 0));
    const char* s = subject;
    regmatch_t m;
    while (*s && tinox_regexec(&re, s, 1, &m, 0) == 0) {
        int mlen = m.rm_eo - m.rm_so;
        char* match_str = (char*)GC_malloc(mlen + 1);
        memcpy(match_str, s + m.rm_so, mlen);
        match_str[mlen] = '\0';
        tinox_array_push(nh, (int64_t)match_str);
        s += m.rm_eo;
        if (m.rm_eo == 0) s++;
    }
    regfree(&re);
    return (int64_t)nh;
}

int64_t regexFindAll(int64_t pattern_i64, int64_t subject_i64) {
    return regexFindAll_options(pattern_i64, subject_i64, 0);
}

int64_t regexReplace(int64_t pattern_i64, int64_t subject_i64, int64_t replacement_i64) {
    const char* pattern = (const char*)pattern_i64;
    const char* subject = (const char*)subject_i64;
    const char* replacement = (const char*)replacement_i64;
    // Simple replacement — replace first match
    regex_t re;
    tinox_regcomp(&re, pattern, REG_EXTENDED);
    regmatch_t m;
    if (tinox_regexec(&re, subject, 1, &m, 0) != 0) { regfree(&re); return subject_i64; }
    size_t pre = m.rm_so, rep_len = strlen(replacement), suf = strlen(subject) - m.rm_eo;
    char* result = (char*)GC_malloc(pre + rep_len + suf + 1);
    memcpy(result, subject, pre);
    memcpy(result + pre, replacement, rep_len);
    memcpy(result + pre + rep_len, subject + m.rm_eo, suf);
    result[pre + rep_len + suf] = '\0';
    regfree(&re);
    return (int64_t)result;
}

int64_t regexSplit(int64_t pattern_i64, int64_t subject_i64) {
    return regexFindAll(pattern_i64, subject_i64); // simplified
}

// First match, or "" if none; invalid patterns fail visibly.
static int64_t regexFindFirst_options(int64_t pattern_i64, int64_t subject_i64, int64_t icase) {
    const char* pattern = (const char*)pattern_i64;
    const char* subject = (const char*)subject_i64;
    regex_t re;
    tinox_regcomp(&re, pattern, REG_EXTENDED | (icase ? REG_ICASE : 0));
    regmatch_t m;
    if (tinox_regexec(&re, subject, 1, &m, 0) != 0) {
        regfree(&re);
        return (int64_t)GC_strdup("");
    }
    int mlen = m.rm_eo - m.rm_so;
    char* match_str = (char*)GC_malloc(mlen + 1);
    memcpy(match_str, subject + m.rm_so, mlen);
    match_str[mlen] = '\0';
    regfree(&re);
    return (int64_t)match_str;
}

int64_t regexFindFirst(int64_t pattern_i64, int64_t subject_i64) {
    return regexFindFirst_options(pattern_i64, subject_i64, 0);
}

// Replace every non-overlapping match of `pattern` in `subject` with
// `replacement` (literal, no backreferences — same as regexReplace).
static int64_t regexReplaceAll_options(int64_t pattern_i64, int64_t subject_i64, int64_t replacement_i64, int64_t icase) {
    const char* pattern = (const char*)pattern_i64;
    const char* subject = (const char*)subject_i64;
    const char* replacement = (const char*)replacement_i64;
    regex_t re;
    tinox_regcomp(&re, pattern, REG_EXTENDED | (icase ? REG_ICASE : 0));

    size_t rep_len = strlen(replacement);
    size_t cap = strlen(subject) + rep_len + 16;
    char* result = (char*)GC_malloc(cap);
    size_t out = 0;
    const char* s = subject;
    regmatch_t m;
    while (*s && tinox_regexec(&re, s, 1, &m, 0) == 0) {
        size_t pre = (size_t)m.rm_so;
        size_t needed = out + pre + rep_len + 1;
        if (needed > cap) {
            cap = needed * 2;
            char* grown = (char*)GC_malloc(cap);
            memcpy(grown, result, out);
            result = grown;
        }
        memcpy(result + out, s, pre);
        out += pre;
        memcpy(result + out, replacement, rep_len);
        out += rep_len;
        size_t adv = (size_t)m.rm_eo;
        if (adv == 0) {
            // Empty match — copy one char to avoid an infinite loop.
            if (s[0] == '\0') break;
            size_t needed2 = out + 2;
            if (needed2 > cap) {
                cap = needed2 * 2;
                char* grown = (char*)GC_malloc(cap);
                memcpy(grown, result, out);
                result = grown;
            }
            result[out++] = s[0];
            adv = 1;
        }
        s += adv;
    }
    size_t tail = strlen(s);
    size_t needed = out + tail + 1;
    if (needed > cap) {
        cap = needed;
        char* grown = (char*)GC_malloc(cap);
        memcpy(grown, result, out);
        result = grown;
    }
    memcpy(result + out, s, tail);
    out += tail;
    result[out] = '\0';
    regfree(&re);
    return (int64_t)result;
}

int64_t regexReplaceAll(int64_t pattern_i64, int64_t subject_i64, int64_t replacement_i64) {
    return regexReplaceAll_options(pattern_i64, subject_i64, replacement_i64, 0);
}

// Option-aware ABI for the guarded public Regex facade. Validation deliberately
// happens in Tinox before these calls so normal exception unwinding applies.
int64_t tinoxRegexIsMatch(const char* pattern, const char* subject, int64_t icase) {
    return regexIsMatch_options((int64_t)pattern, (int64_t)subject, icase);
}
int64_t* tinoxRegexFindAll(const char* pattern, const char* subject, int64_t icase) {
    return (int64_t*)regexFindAll_options((int64_t)pattern, (int64_t)subject, icase);
}
char* tinoxRegexFindFirst(const char* pattern, const char* subject, int64_t icase) {
    return (char*)regexFindFirst_options((int64_t)pattern, (int64_t)subject, icase);
}
char* tinoxRegexReplaceAll(const char* pattern, const char* subject, const char* replacement, int64_t icase) {
    return (char*)regexReplaceAll_options((int64_t)pattern, (int64_t)subject, (int64_t)replacement, icase);
}
int64_t* tinoxRegexSplit(const char* pattern, const char* subject, int64_t icase) {
    return (int64_t*)regexFindAll_options((int64_t)pattern, (int64_t)subject, icase);
}

// First match at/after byte offset. Returns Tinox i64-array
// [match_start, match_end, g1_start, g1_end, ...] (byte offsets into subject,
// -1/-1 for unmatched groups). Empty array = no match or invalid offset.
int64_t* regexMatchGroups(const char* pattern, const char* subject, int64_t offset, int64_t icase) {
    int64_t* empty = tinox_array_new(0, 0);

    size_t slen = strlen(subject);
    if (offset < 0 || (size_t)offset > slen) return empty;

    regex_t re;
    int cflags = REG_EXTENDED | (icase ? REG_ICASE : 0);
    tinox_regcomp(&re, pattern, cflags);

    size_t ngroups = re.re_nsub + 1;
    regmatch_t* m = (regmatch_t*)GC_malloc(sizeof(regmatch_t) * ngroups);
    int eflags = (offset > 0) ? REG_NOTBOL : 0;
    if (tinox_regexec(&re, subject + offset, ngroups, m, eflags) != 0) {
        regfree(&re);
        return empty;
    }
    regfree(&re);

    int64_t len = (int64_t)(ngroups * 2);
    int64_t* nh = tinox_array_new(len, 0);
    int64_t* data = ((TinoxArray*)nh)->data;
    for (size_t g = 0; g < ngroups; g++) {
        if (m[g].rm_so < 0) {
            data[g * 2] = -1;
            data[g * 2 + 1] = -1;
        } else {
            data[g * 2] = (int64_t)m[g].rm_so + offset;
            data[g * 2 + 1] = (int64_t)m[g].rm_eo + offset;
        }
    }
    return nh;
}

static size_t fast_i64_write(int64_t val, char* buf);

// ---- HTTP Server ----

// ---- TLS / Connection handles ----
//
// With TLS, a raw fd is no longer enough: every connection needs its own
// SSL* object. So we wrap {fd, ssl} in a GC-allocated TinoxConn and
// return its pointer as an opaque int64 handle (a userspace address is
// always > 0, errors are -1). ssl==NULL means plaintext — that way
// http:// and https:// share exactly the same read/write code (conn_recv/conn_send).
//
// TLS is gated behind -DTINOX_TLS + -lssl -lcrypto; the default build
// deliberately stays OpenSSL-free. Without that flag, the *Tls functions
// return -1, giving a clean runtime error instead of a link error.

// writeLock (issue 82): AMQP 1.0 heartbeats run on their own spawned
// background thread in parallel with the app-side frame writes on the
// SAME connection (necessary because the app thread can be blocked for
// a long time inside nextMessage()). conn_send_all loops on short writes
// (similar to bug 68, but here it's concurrency rather than EINTR): without
// a lock, two threads could interleave their bytes mid-frame on the
// wire. Only affects connections that write from MULTIPLE places (the
// AMQP 1.0 heartbeat feature); all other users just pay the lock/unlock
// cost of an uncontended mutex.
// wsCompressed (issue #122): set by Ws::handshake() when the peer
// negotiated permessage-deflate (RFC 7692) for this connection. Lives on
// TinoxConn -- the per-connection struct that already exists for
// exactly this kind of state -- rather than a separate side-table
// keyed by conn/fd, which would need its own cleanup-on-close to avoid
// a stale `true` entry misapplying to an unrelated later connection
// that happens to reuse the same fd number. Every construction site
// below sets it to false explicitly (malloc doesn't zero); it becomes
// true only if handshake() actually negotiates the extension.
// inflight/closing (issue #224): conn_close() used to free the TLS SSL*
// (and close the fd) unconditionally, racing a DIFFERENT thread
// concurrently blocked inside conn_recv/conn_send on the SAME connection
// -- a real heap-use-after-free under ASAN, reproduced via
// tinox.core.kubernetes's Watch::openRaw read loop on a background
// thread racing an unrelated thread's close() of the same connection.
// `inflight` is incremented for the duration of every conn_recv/
// conn_send call (both TLS and plaintext -- a plain close()+fd-number-
// reuse race while another thread is still blocked in recv() on that
// same fd number is a real, if rarer, hazard too) and `closing` is set
// by conn_close() BEFORE it calls shutdown(fd, SHUT_RDWR) -- a
// standard POSIX pattern where shutdown() on one thread reliably
// unblocks a DIFFERENT thread's concurrent blocking recv()/SSL_read()
// on the same fd. conn_close() then waits for `inflight` to drop to 0
// (every in-flight recv/send has actually returned and is no longer
// touching `ssl`/`fd`) before it's safe to SSL_free()/close().
// permessage-deflate context takeover (issue #560, opt-in): wsCtx marks a connection that keeps one
// DEFLATE and one INFLATE context for its whole life instead of a fresh one per message. wsDef/wsInf
// are lazily created z_streams (about 99 KiB per connection together), released by conn_close.
// defLock serialises compress+write so frames leave in the order they were compressed (the peer's
// window follows that order); infLock guards the inflate context against a concurrent close.
typedef struct { int fd; void* ssl; pthread_mutex_t writeLock; bool wsCompressed; atomic_int inflight; atomic_bool closing;
                 bool wsCtx; bool wsDefReset; bool wsInfReset; int wsDefBits; void* wsDef; void* wsInf;
                 pthread_mutex_t defLock; pthread_mutex_t infLock; } TinoxConn;   // ssl==NULL => plaintext
static void conn_ws_init(TinoxConn* c) {
    c->wsCtx = false; c->wsDefReset = false; c->wsInfReset = false; c->wsDefBits = 15;
    c->wsDef = NULL; c->wsInf = NULL;
    pthread_mutex_init(&c->defLock, NULL);
    pthread_mutex_init(&c->infLock, NULL);
}
static void conn_ws_release(TinoxConn* c) {
    pthread_mutex_lock(&c->defLock);
    if (c->wsDef) { deflateEnd((z_stream*)c->wsDef); free(c->wsDef); c->wsDef = NULL; }
    pthread_mutex_unlock(&c->defLock);
    pthread_mutex_lock(&c->infLock);
    if (c->wsInf) { inflateEnd((z_stream*)c->wsInf); free(c->wsInf); c->wsInf = NULL; }
    pthread_mutex_unlock(&c->infLock);
    pthread_mutex_destroy(&c->defLock);
    pthread_mutex_destroy(&c->infLock);
}
static void conn_send_all(TinoxConn* c, const char* data, size_t len);

#ifdef TINOX_TLS
#include <openssl/ssl.h>
#include <openssl/err.h>
static SSL_CTX* g_tls_ctx = NULL;        // server side (listenTls)
static SSL_CTX* g_tls_client_ctx = NULL; // client side (dialTls, e.g. amqps://)

// EINTR retry (bug 68): any blocking recv/send can be interrupted by ANY
// signal (errno=EINTR), not just our own handlers -- in particular the
// internal "stop the world" signal the Boehm GC uses to pause all threads
// during a collection (confirmed via `gdb`: SIGPWR). As soon as a `spawn`
// task on a second real thread (pthread_create, see tinox_task_spawn)
// allocates enough in parallel to trigger a collection WHILE another
// thread is blocked reading/writing in httpConnReadN/httpConnWriteBytes,
// that read/write gets interrupted by the signal. Without a retry loop,
// the caller saw this as a dropped connection (got<=0) and aborted the
// frame prematurely -- an empty/incomplete payload then silently
// propagated until a later, seemingly completely unrelated
// bounds-check crash (silent garbage until the crash).
// Deterministically reproducible under sufficient allocation pressure
// alongside a `spawn` task; see the GitHub issue history.
static ssize_t conn_recv(TinoxConn* c, char* buf, size_t n) {
    // Issue #224: see TinoxConn's own field comment. Checked BEFORE
    // incrementing inflight -- a connection already mid-close() should
    // never start a fresh SSL_read/recv at all, not just avoid racing
    // its teardown.
    if (atomic_load(&c->closing)) return 0;
    atomic_fetch_add(&c->inflight, 1);
    ssize_t result;
    if ((!c->ssl || SSL_pending((SSL*)c->ssl) == 0) && !service_io_wait(c->fd, POLLIN)) {
        atomic_fetch_sub(&c->inflight, 1); return 0;
    }
    if (c->ssl) {
        int r;
        do { r = SSL_read((SSL*)c->ssl, buf, (int)n); }
        while (r <= 0 && SSL_get_error((SSL*)c->ssl, r) == SSL_ERROR_SYSCALL && errno == EINTR);
        result = (ssize_t)r;
    } else {
        ssize_t r;
        do { r = recv(c->fd, buf, n, 0); } while (r < 0 && errno == EINTR);
        result = r;
    }
    atomic_fetch_sub(&c->inflight, 1);
    return result;
}
static ssize_t conn_send(TinoxConn* c, const char* buf, size_t n) {
    if (atomic_load(&c->closing)) return -1;
    atomic_fetch_add(&c->inflight, 1);
    ssize_t result;
    if (c->ssl) {
        int r;
        do { r = SSL_write((SSL*)c->ssl, buf, (int)n); }
        while (r <= 0 && SSL_get_error((SSL*)c->ssl, r) == SSL_ERROR_SYSCALL && errno == EINTR);
        result = (ssize_t)r;
    } else {
        ssize_t r;
        do { r = send(c->fd, buf, n, MSG_NOSIGNAL); } while (r < 0 && errno == EINTR);
        result = r;
    }
    atomic_fetch_sub(&c->inflight, 1);
    return result;
}
static void conn_close(TinoxConn* c) {
    // Issue #224: mark closing FIRST (so a recv/send racing to START
    // right now bails via the `closing` check above instead of
    // incrementing inflight after we've already begun waiting for it to
    // reach 0), then shutdown() the raw fd -- a standard POSIX pattern
    // where shutdown() on this thread reliably unblocks a DIFFERENT
    // thread's concurrent blocking recv()/SSL_read() on the same fd
    // (returning an error/EOF to it) without this thread needing that
    // other thread's cooperation. Only once every in-flight conn_recv/
    // conn_send has actually observed that and returned (inflight back
    // to 0) is it safe to SSL_free()/close() -- freeing while another
    // thread's SSL_read() is still internally touching the same SSL*
    // is the original use-after-free this whole mechanism exists to
    // prevent; shutdown() alone unblocks the underlying socket read but
    // doesn't guarantee SSL_read() has finished its own post-syscall
    // bookkeeping on `ssl` yet.
    atomic_store(&c->closing, true);
    if (c->fd >= 0) shutdown(c->fd, SHUT_RDWR);
    while (atomic_load(&c->inflight) > 0) {
        struct timespec ts = { 0, 1000000 }; // 1ms
        nanosleep(&ts, NULL);
    }
    if (c->ssl) { SSL_shutdown((SSL*)c->ssl); SSL_free((SSL*)c->ssl); c->ssl = NULL; }
    if (c->fd >= 0) { close(c->fd); c->fd = -1; }
    pthread_mutex_destroy(&c->writeLock);
    conn_ws_release(c);
}
#else
// Plaintext-only fallback — identical semantics without OpenSSL. For the
// EINTR retry, see the comment on the TLS variant above (bug 68).
static ssize_t conn_recv(TinoxConn* c, char* buf, size_t n) {
    // Issue #224: see the TLS variant above and TinoxConn's field comment
    // for the full rationale -- identical inflight/closing protocol here.
    if (atomic_load(&c->closing)) return 0;
    atomic_fetch_add(&c->inflight, 1);
    if (!service_io_wait(c->fd, POLLIN)) { atomic_fetch_sub(&c->inflight, 1); return 0; }
    ssize_t r;
    do { r = recv(c->fd, buf, n, 0); } while (r < 0 && errno == EINTR);
    atomic_fetch_sub(&c->inflight, 1);
    return r;
}
static ssize_t conn_send(TinoxConn* c, const char* buf, size_t n) {
    if (atomic_load(&c->closing)) return -1;
    atomic_fetch_add(&c->inflight, 1);
    ssize_t r;
    do { r = send(c->fd, buf, n, MSG_NOSIGNAL); } while (r < 0 && errno == EINTR);
    atomic_fetch_sub(&c->inflight, 1);
    return r;
}
static void conn_close(TinoxConn* c) {
    atomic_store(&c->closing, true);
    if (c->fd >= 0) shutdown(c->fd, SHUT_RDWR);
    while (atomic_load(&c->inflight) > 0) {
        struct timespec ts = { 0, 1000000 }; // 1ms
        nanosleep(&ts, NULL);
    }
    if (c->fd >= 0) { close(c->fd); c->fd = -1; }
    pthread_mutex_destroy(&c->writeLock);
    conn_ws_release(c);
}
#endif

// bind_addr NULL means INADDR_ANY (0.0.0.0) unless TINOX_BIND_ADDRESS is set
// (see below), the behavior every current caller relies on; a non-NULL dotted-quad (e.g. "127.0.0.1", used
// by the dev-mode introspection server -- see tinox_HttpServer_new_bind)
// restricts the listening socket to that interface only.
void tinox_service_close_connection(int64_t handle) {
    if (handle > 0) {
        TinoxConn* connection = (TinoxConn*)(intptr_t)handle;
        if (!atomic_load(&connection->closing)) conn_close(connection);
    }
}

int64_t httpServerCreateOn(int64_t port, const char* bind_addr) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return -1;
    int opt = 1;
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &opt, sizeof(opt));
    setsockopt(fd, SOL_SOCKET, SO_REUSEPORT, &opt, sizeof(opt));
    struct sockaddr_in addr = {0};
    addr.sin_family = AF_INET;
    // Process-wide default for every listener (HttpServer, @TinoxUIApp, WS,
    // HTTP/2, SSE) that didn't ask for a specific address. A malformed value
    // is a hard startup failure, never a silent fallback to 0.0.0.0.
    if (!bind_addr) {
        const char* env_bind = getenv("TINOX_BIND_ADDRESS");
        if (env_bind && *env_bind) {
            struct in_addr probe;
            if (inet_pton(AF_INET, env_bind, &probe) != 1) {
                fprintf(stderr, "tinox: invalid TINOX_BIND_ADDRESS '%s' (expected an IPv4 address such as 127.0.0.1)\n", env_bind);
                exit(1);
            }
            bind_addr = env_bind;
        }
    }
    if (bind_addr) {
        if (inet_pton(AF_INET, bind_addr, &addr.sin_addr) != 1) { close(fd); return -1; }
    } else {
        addr.sin_addr.s_addr = INADDR_ANY;
    }
    addr.sin_port = htons((uint16_t)port);
    if (bind(fd, (struct sockaddr*)&addr, sizeof(addr)) < 0) { close(fd); return -1; }
    if (listen(fd, 4096) < 0) { close(fd); return -1; }
    return (int64_t)fd;
}

int64_t httpServerCreate(int64_t port) {
    return httpServerCreateOn(port, NULL);
}

// `httpServerCreate(0)` already asks the OS to pick a free ephemeral port
// (ordinary bind() semantics) -- this is the missing other half, letting
// Tinox code find out which port it actually got via getsockname(). Added
// so e2e test fixtures (tests/e2e/**/*.tnx) can stop hardcoding literal
// ports for their simulated-broker `spawn`+connect pattern -- a hand-
// curated "which ports are already used by another test file" registry
// that had already caused a real collision (see CLAUDE.md).
int64_t httpServerBoundPort(int64_t server_fd) {
    struct sockaddr_in addr = {0};
    socklen_t len = sizeof(addr);
    if (getsockname((int)server_fd, (struct sockaddr*)&addr, &len) < 0) return -1;
    return (int64_t)ntohs(addr.sin_port);
}

int64_t httpServerAcceptConn(int64_t server_fd) {
    struct sockaddr_in client = {0};
    socklen_t len = sizeof(client);
    if (!service_io_wait((int)server_fd, POLLIN)) return -1;
    int fd = accept((int)server_fd, (struct sockaddr*)&client, &len);
    if (fd >= 0) {
        int one = 1;
        setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof(one));
        struct timeval tv = { .tv_sec = 5, .tv_usec = 0 }; // 5s zombie guard (poll handles keep-alive)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
    }
    return (int64_t)fd;
}

// Static recv buffer — reused across requests, grows as needed
static __thread char*  g_recv_buf = NULL;
static __thread size_t g_recv_used = 0; // bytes held in g_recv_buf after the last conn_read_request (binary-safe body length)
static __thread size_t g_recv_cap = 0;

// Bug 96: moved up from the route-based API section below (where it was
// defined but never referenced) so conn_read_request() can enforce it.
// The cap is TINOX_MAX_BODY_BYTES (default 4 MiB, see the limits block above).

static int http_chunked_end(const char* buf, size_t len, size_t from, size_t* end);
static void conn_send_all(TinoxConn* c, const char* data, size_t len);

// Answers a request that cannot be framed safely and hands the caller an empty request, so the
// connection is closed without ever running a handler on a guessed body.
static void conn_reject_request(TinoxConn* c, char* buf, int status) {
    const char* reason = status == 413 ? "Payload Too Large" : status == 501 ? "Not Implemented" :
                         status == 505 ? "HTTP Version Not Supported" : "Bad Request";
    char resp[256];
    int rn = snprintf(resp, sizeof(resp),
        "HTTP/1.1 %d %s\r\nContent-Type: text/plain\r\nContent-Length: %zu\r\nConnection: close\r\n\r\n%s\n",
        status, reason, strlen(reason) + 1, reason);
    conn_send_all(c, resp, (size_t)rn);
    buf[0] = '\0';
    g_recv_used = 0;
}

static int http_ascii_ieq(const char* a, size_t alen, const char* lower) {
    size_t n = strlen(lower);
    if (alen != n) return 0;
    for (size_t i = 0; i < n; i++) {
        char ch = a[i];
        if (ch >= 'A' && ch <= 'Z') ch = (char)(ch - 'A' + 'a');
        if (ch != lower[i]) return 0;
    }
    return 1;
}

// Strict request framing. *cl is the single Content-Length (-1 when absent), *chunked is set for
// "Transfer-Encoding: chunked". Returns 0, or the status to answer with: 400 for a malformed or
// ambiguous message (non-numeric / signed / differing Content-Length, Content-Length together with
// Transfer-Encoding, whitespace before a header colon), 501 for a transfer coding other than
// chunked, 505 for an unknown HTTP version. Ambiguity here is how request smuggling starts.
static int http_request_framing(const char* buf, const char* hdr_end, long* cl, int* chunked) {
    *cl = -1; *chunked = 0;
    const char* nl = memchr(buf, '\n', (size_t)(hdr_end - buf) + 2);
    if (!nl) return 400;
    // request line: "<method> <target> HTTP/1.x"
    const char* line_end = nl; if (line_end > buf && line_end[-1] == '\r') line_end--;
    const char* sp = line_end;
    while (sp > buf && *sp != ' ') sp--;
    if (sp > buf && (size_t)(line_end - sp) > 6 && !strncmp(sp + 1, "HTTP/", 5)) {
        if (!(line_end - sp == 9 && sp[6] == '1' && sp[7] == '.' && (sp[8] == '0' || sp[8] == '1'))) return 505;
    }
    int saw_cl = 0;
    const char* p = nl + 1;
    const char* limit = hdr_end + 2;
    while (p < limit) {
        const char* e = memchr(p, '\n', (size_t)(limit - p));
        if (!e) e = limit;
        const char* le = e; if (le > p && le[-1] == '\r') le--;
        if (le == p) break;
        const char* colon = memchr(p, ':', (size_t)(le - p));
        if (colon) {
            // field-name is a token: no whitespace or control characters (also none before the colon)
            for (const char* ch = p; ch < colon; ch++) if ((unsigned char)*ch <= ' ' || *ch == 127) return 400;
            const char* v = colon + 1;
            while (v < le && (*v == ' ' || *v == '\t')) v++;
            const char* ve = le;
            while (ve > v && (ve[-1] == ' ' || ve[-1] == '\t')) ve--;
            size_t nlen = (size_t)(colon - p);
            if (http_ascii_ieq(p, nlen, "content-length")) {
                if (ve == v || ve - v > 18) return 400;
                long val = 0;
                for (const char* d = v; d < ve; d++) {
                    if (*d < '0' || *d > '9') return 400;
                    val = val * 10 + (*d - '0');
                }
                if (saw_cl && val != *cl) return 400;
                saw_cl = 1; *cl = val;
            } else if (http_ascii_ieq(p, nlen, "transfer-encoding")) {
                if (!http_ascii_ieq(v, (size_t)(ve - v), "chunked")) return 501;
                *chunked = 1;
            }
        }
        p = e + 1;
    }
    if (*chunked && saw_cl) return 400;
    return 0;
}

// Reads a full HTTP/1.1 request from the connection into g_recv_buf (static, not freed by caller).
// Works for both plaintext (ssl==NULL) and TLS connections via conn_recv.
static char* conn_read_request(TinoxConn* c) {
    // A large upload grew this per-thread buffer; do not keep tens of megabytes pinned per worker.
    if (g_recv_buf && g_recv_cap > (size_t)16 * 1024 * 1024) { free(g_recv_buf); g_recv_buf = NULL; }
    if (!g_recv_buf) { g_recv_cap = 4096; g_recv_buf = (char*)malloc(g_recv_cap); }
    size_t used = 0;
    char* buf = g_recv_buf;
    size_t cap = g_recv_cap;
    while (1) {
        if (used + 1 >= cap) {
            cap *= 2;
            buf = (char*)realloc(buf, cap);
            g_recv_buf = buf; g_recv_cap = cap;
        }
        ssize_t n = conn_recv(c, buf + used, cap - used - 1);
        if (n <= 0) break;
        used += (size_t)n;
        buf[used] = '\0';
        // Stop once we have the full headers (and body if Content-Length matches)
        char* hdr_end = strstr(buf, "\r\n\r\n");
        if (!hdr_end) continue;
        // Strict request framing (RFC 9112 §6, §11.2): one unambiguous body length, or a hard error.
        long body_len = -1;
        int chunked = 0;
        int framing = http_request_framing(buf, hdr_end, &body_len, &chunked);
        if (framing) { conn_reject_request(c, buf, framing); return buf; }
        if (chunked) {
            long chunk_start = (long)(hdr_end - buf) + 4;
            for (;;) {
                size_t end = 0;
                int r = http_chunked_end(buf, used, (size_t)chunk_start, &end);
                if (r < 0) { conn_reject_request(c, buf, 400); return buf; }
                if (r == 1) break;
                if ((long)used - chunk_start > tinox_limits()->max_body_bytes + 65536) { conn_reject_request(c, buf, 413); return buf; }
                if (used + 1 >= cap) {
                    cap *= 2;
                    char* nb = (char*)realloc(buf, cap);
                    if (!nb) { conn_reject_request(c, buf, 400); return buf; }
                    buf = nb; g_recv_buf = buf; g_recv_cap = cap;
                }
                ssize_t m = conn_recv(c, buf + used, cap - used - 1);
                // A body that ends early is an error, never a silently shortened request.
                if (m <= 0) { conn_reject_request(c, buf, 400); return buf; }
                used += (size_t)m;
                buf[used] = '\0';
            }
            // Decode in place so the handler sees the payload, not the chunk framing.
            size_t src = (size_t)chunk_start, dst = (size_t)chunk_start;
            for (;;) {
                size_t q = src;
                while (!(buf[q] == '\r' && buf[q + 1] == '\n')) q++;
                unsigned long long size = strtoull(buf + src, NULL, 16);
                src = q + 2;
                if (size == 0) break;
                memmove(buf + dst, buf + src, (size_t)size);
                dst += (size_t)size;
                src += (size_t)size + 2;
            }
            if ((long)(dst - (size_t)chunk_start) > tinox_limits()->max_body_bytes) { conn_reject_request(c, buf, 413); return buf; }
            used = dst;
            break;
        }
        if (body_len >= 0) {
            long header_len = (long)(hdr_end - buf) + 4;
            // Bug 96 clamped an over-cap Content-Length down to
            // TINOX_MAX_BODY and kept reading -- which stopped the
            // unbounded-allocation attack, but silently handed the
            // application a TRUNCATED body with no signal anything was
            // cut (bug #174): a 150 MB upload became an unmarked ~4 MB
            // prefix, and the handler had no way to tell "this really is
            // a complete small request" from "this was silently
            // mangled". Bug #174 fix: reject up front instead of
            // clamping-and-continuing -- a Content-Length that's
            // negative (malformed) or already over the cap gets a hard,
            // visible 413 and the connection is closed without ever
            // reading/handing off a truncated body. This still bounds
            // allocation exactly like the clamp did (we never grow `cap`
            // past what a request under the cap needs), just via
            // rejection instead of quiet corruption.
            if (body_len < 0 || body_len > tinox_limits()->max_body_bytes) {
                static const char* body413 = "Payload Too Large\n";
                char resp413[256];
                int rn = snprintf(resp413, sizeof(resp413),
                    "HTTP/1.1 413 Payload Too Large\r\n"
                    "Content-Type: text/plain\r\n"
                    "Content-Length: %zu\r\n"
                    "Connection: close\r\n"
                    "\r\n"
                    "%s",
                    strlen(body413), body413);
                conn_send_all(c, resp413, (size_t)rn);
                buf[0] = '\0';
                g_recv_used = 0;
                return buf;
            }
            long total = header_len + body_len;
            while ((long)used < total) {
                while (cap < (size_t)total + 1) {
                    cap *= 2;
                    char* nb = (char*)realloc(buf, cap);
                    if (!nb) { cap /= 2; break; } // OOM: give up growing, keep what we have
                    buf = nb;
                    g_recv_buf = buf; g_recv_cap = cap;
                }
                if (cap < (size_t)total + 1) break; // couldn't grow enough; stop reading the body
                ssize_t m = conn_recv(c, buf + used, (size_t)(total - (long)used));
                if (m <= 0) break;
                used += (size_t)m;
                buf[used] = '\0';
            }
        }
        break;
    }
    buf[used] = '\0';
    g_recv_used = used;
    return buf;
}

// Reads a full HTTP/1.1 request from a raw fd (plaintext). Wraps the fd in a
// stack TinoxConn with ssl==NULL and delegates to the shared core.
char* httpServerReadRequest(int64_t client_fd) {
    TinoxConn c = { (int)client_fd, NULL, PTHREAD_MUTEX_INITIALIZER };
    return conn_read_request(&c);
}

static void conn_send_all(TinoxConn* c, const char* data, size_t len) {
    pthread_mutex_lock(&c->writeLock);
    size_t sent = 0;
    while (sent < len) {
        ssize_t n = conn_send(c, data + sent, len - sent);
        if (n <= 0) break;
        sent += (size_t)n;
    }
    pthread_mutex_unlock(&c->writeLock);
}

// Sends a raw HTTP response string and returns.
void httpServerSendRaw(int64_t client_fd, const char* data) {
    if (!data) return;
    TinoxConn c = { (int)client_fd, NULL, PTHREAD_MUTEX_INITIALIZER };
    conn_send_all(&c, data, strlen(data));
}

void httpServerCloseConn(int64_t client_fd) {
    close((int)client_fd);
}

// ---- TLS server entry points + connection-handle API ----
//
// These functions form the typed extern-fn interface for the
// Tinox side (see http_server.tnx: listenTls). The returned handle
// is the pointer to a GC-allocated TinoxConn.

// Sets up a TLS server: loads the cert chain + private key (both PEM) and
// binds/listens like httpServerCreate. Return value: server fd (>=0) or -1.
int64_t httpServerCreateTls(int64_t port, const char* cert_path, const char* key_path) {
#ifdef TINOX_TLS
    if (!g_tls_ctx) {
        SSL_library_init();
        SSL_load_error_strings();
        OpenSSL_add_ssl_algorithms();
        g_tls_ctx = SSL_CTX_new(TLS_server_method());
        if (!g_tls_ctx) return -1;
        SSL_CTX_set_min_proto_version(g_tls_ctx, TLS1_2_VERSION);
    }
    if (SSL_CTX_use_certificate_chain_file(g_tls_ctx, cert_path) <= 0) {
        ERR_print_errors_fp(stderr);
        return -1;
    }
    if (SSL_CTX_use_PrivateKey_file(g_tls_ctx, key_path, SSL_FILETYPE_PEM) <= 0) {
        ERR_print_errors_fp(stderr);
        return -1;
    }
    if (!SSL_CTX_check_private_key(g_tls_ctx)) {
        fprintf(stderr, "httpServerCreateTls: cert/key mismatch\n");
        return -1;
    }
    return httpServerCreate(port);
#else
    (void)port; (void)cert_path; (void)key_path;
    fprintf(stderr, "httpServerCreateTls: runtime built without TLS (-DTINOX_TLS missing)\n");
    return -1;
#endif
}

// Accepts a connection and performs the TLS handshake. Return value:
// opaque connection handle (>0) or -1. The handshake is blocking.
int64_t httpServerAcceptTls(int64_t server_fd) {
#ifdef TINOX_TLS
    if (!g_tls_ctx) return -1;
    struct sockaddr_in client = {0};
    socklen_t len = sizeof(client);
    if (!service_io_wait((int)server_fd, POLLIN)) return -1;
    int fd = accept((int)server_fd, (struct sockaddr*)&client, &len);
    if (fd < 0) return -1;
    int one = 1;
    setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof(one));
    // Bug 91: same 5s zombie guard as the plaintext path (httpServerAcceptConn
    // above). Without it, a client that opens the TCP connection and never
    // sends TLS ClientHello bytes blocks SSL_accept() forever; since both
    // HttpServer::listenTls and WsServer::acceptTls run a single-threaded
    // blocking accept loop, one such client prevents the server from ever
    // accepting anyone else. The timeout persists on `fd` for the life of
    // the connection, so it also protects later blocking reads after a
    // successful handshake (an idle client stalling mid-request/mid-frame).
    struct timeval tv = { .tv_sec = 5, .tv_usec = 0 };
    setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
    SSL* ssl = SSL_new(g_tls_ctx);
    if (!ssl) { close(fd); return -1; }
    SSL_set_fd(ssl, fd);
    if (SSL_accept(ssl) <= 0) {
        ERR_print_errors_fp(stderr);
        SSL_free(ssl);
        close(fd);
        return -1;
    }
    TinoxConn* c = (TinoxConn*)malloc(sizeof(TinoxConn));
    pthread_mutex_init(&c->writeLock, NULL);
    c->fd = fd;
    c->ssl = ssl;
    c->wsCompressed = false;
    c->inflight = 0;
    c->closing = false;
    conn_ws_init(c);
    return (int64_t)(intptr_t)c;
#else
    (void)server_fd;
    return -1;
#endif
}

// Accepts a plaintext connection and likewise returns a conn handle,
// so the Tinox code can use a SINGLE loop (httpConn*) for both http and https.
int64_t httpServerAcceptConnHandle(int64_t server_fd) {
    tinox_ws_debug_log("accept: waiting on server_fd=%lld", (long long)server_fd);
    int64_t fd = httpServerAcceptConn(server_fd);
    if (fd < 0) {
        tinox_ws_debug_log("accept: failed (fd=%lld)", (long long)fd);
        return -1;
    }
    tinox_ws_debug_log("accept: got fd=%lld", (long long)fd);
    TinoxConn* c = (TinoxConn*)malloc(sizeof(TinoxConn));
    pthread_mutex_init(&c->writeLock, NULL);
    c->fd = (int)fd;
    c->ssl = NULL;
    c->wsCompressed = false;
    c->inflight = 0;
    c->closing = false;
    conn_ws_init(c);
    return (int64_t)(intptr_t)c;
}

// Reads a request over a conn handle (TLS or plaintext).
char* httpConnReadRequest(int64_t conn) {
    if (conn <= 0) return (char*)"";
    tinox_ws_debug_log("read_request: start conn=%lld fd=%d", (long long)conn, ((TinoxConn*)(intptr_t)conn)->fd);
    char* req = conn_read_request((TinoxConn*)(intptr_t)conn);
    tinox_ws_debug_log("read_request: done conn=%lld fd=%d len=%zu", (long long)conn, ((TinoxConn*)(intptr_t)conn)->fd, strlen(req));
    return req;
}

// Sends a raw response over a conn handle.
void httpConnSendRaw(int64_t conn, const char* data) {
    if (conn <= 0 || !data) return;
    tinox_ws_debug_log("send_raw: start conn=%lld fd=%d len=%zu", (long long)conn, ((TinoxConn*)(intptr_t)conn)->fd, strlen(data));
    conn_send_all((TinoxConn*)(intptr_t)conn, data, strlen(data));
    tinox_ws_debug_log("send_raw: done conn=%lld fd=%d", (long long)conn, ((TinoxConn*)(intptr_t)conn)->fd);
}

// Wraps a bare socket fd (e.g. client side via socketConnect) in
// a plaintext conn handle, so the httpConn* primitives can be used on
// both sides (tests, later WsClient).
int64_t httpConnFromFd(int64_t fd) {
    if (fd < 0) return -1;
    TinoxConn* c = (TinoxConn*)malloc(sizeof(TinoxConn));
    pthread_mutex_init(&c->writeLock, NULL);
    c->fd = (int)fd;
    c->ssl = NULL;
    c->wsCompressed = false;
    c->inflight = 0;
    c->closing = false;
    conn_ws_init(c);
    return (int64_t)(intptr_t)c;
}

// wsCompressed accessors (issue #122) — see TinoxConn's wsCompressed
// field comment for why this lives on the conn struct instead of a
// side-table. conn<=0 is silently ignored (matches every other
// conn-handle builtin's convention in this file).
void wsSetCompressed(int64_t conn, bool val) {
    if (conn <= 0) return;
    ((TinoxConn*)(intptr_t)conn)->wsCompressed = val;
}
bool wsIsCompressed(int64_t conn) {
    if (conn <= 0) return false;
    return ((TinoxConn*)(intptr_t)conn)->wsCompressed;
}

// ---- permessage-deflate context takeover (issue #560) ----
//
// Opt-in: `tinox.websocket.compression = context` in application.properties (or the environment
// variable TINOX_WEBSOCKET_COMPRESSION) makes Ws::handshake keep one DEFLATE and one INFLATE
// context per connection instead of negotiating no_context_takeover. The default ("stateless")
// is unchanged. Anything else is a hard startup error (no silent fallback).
static const char* tinox_config_resolve(const char* key);
bool wsContextTakeoverConfigured(void) {
    static int cached = -1;
    if (cached >= 0) return cached == 1;
    const char* v = tinox_config_resolve("tinox.websocket.compression");
    if (!v || !*v || strcmp(v, "stateless") == 0) cached = 0;
    else if (strcmp(v, "context") == 0) cached = 1;
    else {
        fprintf(stderr, "tinox.websocket.compression must be 'stateless' or 'context', got '%s'\n", v);
        exit(1);
    }
    return cached == 1;
}

// Called by Ws::handshake after it accepted the offer: the connection then uses persistent contexts.
// serverNoCtx/clientNoCtx are the no_context_takeover parameters the client asked for (and the
// response echoes): the matching context is reset after every message. bits is the negotiated
// server_max_window_bits (9..15).
void wsSetContext(int64_t conn, bool serverNoCtx, bool clientNoCtx, int64_t bits) {
    if (conn <= 0) return;
    TinoxConn* c = (TinoxConn*)(intptr_t)conn;
    c->wsCtx = true;
    c->wsDefReset = serverNoCtx;
    c->wsInfReset = clientNoCtx;
    c->wsDefBits = (bits >= 9 && bits <= 15) ? (int)bits : 15;
}
bool wsHasContext(int64_t conn) {
    return conn > 0 && ((TinoxConn*)(intptr_t)conn)->wsCtx;
}
void wsDefLock(int64_t conn) {
    if (conn > 0) pthread_mutex_lock(&((TinoxConn*)(intptr_t)conn)->defLock);
}
void wsDefUnlock(int64_t conn) {
    if (conn > 0) pthread_mutex_unlock(&((TinoxConn*)(intptr_t)conn)->defLock);
}

// Compresses one outgoing message. Without context takeover this is tinoxDeflateRaw. With it, the
// caller holds wsDefLock across this call AND the write of the frame.
int64_t* wsDeflateMsg(int64_t conn, int64_t* bytes) {
    if (conn <= 0 || !((TinoxConn*)(intptr_t)conn)->wsCtx) return tinoxDeflateRaw(bytes);
    TinoxConn* c = (TinoxConn*)(intptr_t)conn;
    if (!c->wsDef) {
        z_stream* z = (z_stream*)calloc(1, sizeof(z_stream));
        if (!z || deflateInit2(z, Z_DEFAULT_COMPRESSION, Z_DEFLATED, -c->wsDefBits, 8, Z_DEFAULT_STRATEGY) != Z_OK) {
            free(z);
            fprintf(stderr, "websocket: cannot allocate a DEFLATE context\n");
            exit(1);
        }
        c->wsDef = z;
    } else if (c->wsDefReset) {
        deflateReset((z_stream*)c->wsDef);
    }
    z_stream* z = (z_stream*)c->wsDef;
    TinoxArray* a = (TinoxArray*)bytes;
    int64_t n = a ? a->len : 0;
    unsigned char* in = (unsigned char*)malloc(n > 0 ? (size_t)n : 1);
    for (int64_t i = 0; i < n; i++) in[i] = (unsigned char)(a->data[i] & 0xff);
    z->next_in = in;
    z->avail_in = (uInt)n;
    size_t cap = (size_t)(n > 0 ? n : 1) + 64;
    unsigned char* out = (unsigned char*)malloc(cap);
    size_t total = 0;
    for (;;) {
        z->next_out = out + total;
        z->avail_out = (uInt)(cap - total);
        deflate(z, Z_SYNC_FLUSH);
        total = cap - z->avail_out;
        if (z->avail_out > 0) break;
        cap *= 2;
        out = (unsigned char*)realloc(out, cap);
    }
    free(in);
    if (total >= 4 && out[total-4] == 0x00 && out[total-3] == 0x00 && out[total-2] == 0xFF && out[total-1] == 0xFF) total -= 4;
    int64_t* nh = tinox_array_new(0, total > 0 ? (int64_t)total : 4);
    for (size_t i = 0; i < total; i++) tinox_array_push(nh, out[i]);
    free(out);
    return nh;
}

// Decompresses one fully reassembled incoming message; failure is signalled through
// wsLastInflateOk() exactly like tinoxInflateRaw. After a failure the context is discarded: the
// caller closes the connection.
int64_t* wsInflateMsg(int64_t conn, int64_t* bytes) {
    if (conn <= 0 || !((TinoxConn*)(intptr_t)conn)->wsCtx) return tinoxInflateRaw(bytes);
    TinoxConn* c = (TinoxConn*)(intptr_t)conn;
    _tinox_inflate_ok = true;
    pthread_mutex_lock(&c->infLock);
    if (!c->wsInf) {
        z_stream* z = (z_stream*)calloc(1, sizeof(z_stream));
        if (!z || inflateInit2(z, -15) != Z_OK) {
            free(z);
            pthread_mutex_unlock(&c->infLock);
            _tinox_inflate_ok = false;
            return tinox_array_new(0, 4);
        }
        c->wsInf = z;
    } else if (c->wsInfReset) {
        inflateReset((z_stream*)c->wsInf);
    }
    z_stream* z = (z_stream*)c->wsInf;
    TinoxArray* a = (TinoxArray*)bytes;
    int64_t n = a ? a->len : 0;
    unsigned char* in = (unsigned char*)malloc((size_t)n + 4);
    for (int64_t i = 0; i < n; i++) in[i] = (unsigned char)(a->data[i] & 0xff);
    in[n] = 0x00; in[n+1] = 0x00; in[n+2] = 0xFF; in[n+3] = 0xFF;
    z->next_in = in;
    z->avail_in = (uInt)(n + 4);
    size_t cap = 4096;
    unsigned char* out = (unsigned char*)malloc(cap);
    size_t total = 0;
    bool failed = false;
    for (;;) {
        z->next_out = out + total;
        z->avail_out = (uInt)(cap - total);
        int ret = inflate(z, Z_SYNC_FLUSH);
        total = cap - z->avail_out;
        if (total > 16777216) { failed = true; break; }
        if (ret == Z_STREAM_END) { inflateReset(z); break; }
        if (ret != Z_OK && ret != Z_BUF_ERROR) { failed = true; break; }
        if (z->avail_out == 0) { cap *= 2; out = (unsigned char*)realloc(out, cap); continue; }
        if (z->avail_in == 0) break;
        failed = true; // input left but no progress: truncated or corrupt stream
        break;
    }
    free(in);
    if (failed) {
        inflateEnd(z); free(z); c->wsInf = NULL;
        pthread_mutex_unlock(&c->infLock);
        free(out);
        _tinox_inflate_ok = false;
        return tinox_array_new(0, 4);
    }
    pthread_mutex_unlock(&c->infLock);
    int64_t* nh = tinox_array_new(0, total > 0 ? (int64_t)total : 4);
    for (size_t i = 0; i < total; i++) tinox_array_push(nh, out[i]);
    free(out);
    return nh;
}

// Wraps an already-connected socket fd (client side, e.g. via
// socketConnect) in a TLS conn handle: performs the handshake as a TLS
// CLIENT (the counterpart to httpServerAcceptTls, which accepts server-side).
// For outgoing connections like amqps:// (see amqp091.tnx: Amqp091::dialTls).
// host is always sent as SNI; verify=true additionally checks the
// certificate chain AND the hostname against the system CA stores (the
// default case for real broker certificates) -- verify=false is a
// deliberate, explicitly named opt-out for self-signed test certificates.
int64_t httpConnFromFdTls(int64_t fd, const char* host, bool verify) {
#ifdef TINOX_TLS
    if (fd < 0) return -1;
    // Same custom-CA/client-cert(mTLS)/insecure-skip-verify thread-local
    // config http_request's https:// branch honors (httpSetTlsCaCertFile/
    // httpSetTlsClientCertFile/httpSetTlsInsecureSkipVerify) -- needed so
    // a conn-based client (e.g. tinox.core.kubernetes' Watch, which can't
    // use the ordinary httpGet/httpPost path for a long-lived streaming
    // response) can authenticate against a cluster with a self-signed CA
    // and/or client-cert auth the same way a plain request already can.
    // Falls back to the existing shared g_tls_client_ctx behavior
    // (system trust store only) when none of this is set, so every
    // existing caller (AMQP/WS/SMTP STARTTLS) is unaffected.
    int use_custom_ctx = (_tinox_tls_ca_cert_path != NULL) ||
                          (_tinox_tls_client_cert_path != NULL) ||
                          _tinox_tls_insecure_skip_verify;
    SSL_CTX* ctx = NULL;
    if (use_custom_ctx) {
        SSL_library_init();
        SSL_load_error_strings();
        OpenSSL_add_ssl_algorithms();
        ctx = SSL_CTX_new(TLS_client_method());
        if (ctx) {
            SSL_CTX_set_min_proto_version(ctx, TLS1_2_VERSION);
            if (_tinox_tls_ca_cert_path) {
                if (SSL_CTX_load_verify_locations(ctx, _tinox_tls_ca_cert_path, NULL) != 1) {
                    fprintf(stderr, "httpConnFromFdTls: failed to load CA cert file %s\n", _tinox_tls_ca_cert_path);
                    SSL_CTX_free(ctx);
                    ctx = NULL;
                }
            } else {
                SSL_CTX_set_default_verify_paths(ctx);
            }
            if (ctx && _tinox_tls_client_cert_path && _tinox_tls_client_key_path) {
                if (SSL_CTX_use_certificate_file(ctx, _tinox_tls_client_cert_path, SSL_FILETYPE_PEM) != 1 ||
                    SSL_CTX_use_PrivateKey_file(ctx, _tinox_tls_client_key_path, SSL_FILETYPE_PEM) != 1 ||
                    SSL_CTX_check_private_key(ctx) != 1) {
                    fprintf(stderr, "httpConnFromFdTls: failed to load client cert/key (%s / %s)\n",
                            _tinox_tls_client_cert_path, _tinox_tls_client_key_path);
                    SSL_CTX_free(ctx);
                    ctx = NULL;
                }
            }
        }
    } else {
        if (!g_tls_client_ctx) {
            SSL_library_init();
            SSL_load_error_strings();
            OpenSSL_add_ssl_algorithms();
            g_tls_client_ctx = SSL_CTX_new(TLS_client_method());
            if (g_tls_client_ctx) {
                SSL_CTX_set_min_proto_version(g_tls_client_ctx, TLS1_2_VERSION);
                SSL_CTX_set_default_verify_paths(g_tls_client_ctx);
            }
        }
        ctx = g_tls_client_ctx;
    }
    if (!ctx) { close((int)fd); return -1; }
    SSL* ssl = SSL_new(ctx);
    if (!ssl) { if (use_custom_ctx) SSL_CTX_free(ctx); close((int)fd); return -1; }
    SSL_set_tlsext_host_name(ssl, host); // SNI
    if (_tinox_tls_insecure_skip_verify) {
        SSL_set_verify(ssl, SSL_VERIFY_NONE, NULL);
    } else if (verify) {
        SSL_set_verify(ssl, SSL_VERIFY_PEER, NULL);
        SSL_set1_host(ssl, host); // hostname must match the peer certificate
    } else {
        SSL_set_verify(ssl, SSL_VERIFY_NONE, NULL);
    }
    SSL_set_fd(ssl, (int)fd);
    if (SSL_connect(ssl) <= 0) {
        ERR_print_errors_fp(stderr);
        SSL_free(ssl);
        if (use_custom_ctx) SSL_CTX_free(ctx);
        close((int)fd);
        return -1;
    }
    TinoxConn* c = (TinoxConn*)malloc(sizeof(TinoxConn));
    pthread_mutex_init(&c->writeLock, NULL);
    c->fd = (int)fd;
    c->ssl = ssl;
    c->wsCompressed = false;
    c->inflight = 0;
    c->closing = false;
    conn_ws_init(c);
    return (int64_t)(intptr_t)c;
#else
    // Bug 90: the TLS-enabled branch above closes `fd` on every error path;
    // this fallback used to just ignore it and leak the fd on every call.
    (void)host; (void)verify;
    if (fd >= 0) close((int)fd);
    fprintf(stderr, "httpConnFromFdTls: runtime built without TLS (TINOX_TLS=0)\n");
    return -1;
#endif
}

// ---- HTTP/1.1 client builtins (tinox.core.http), continued ----
// Placed here (not right after httpSetHeader/httpClearHeaders above) so
// http_request can use g_tls_client_ctx for https:// -- that global is
// declared earlier in this TLS section, and TINOX_TLS=0 opt-out builds
// must not require <openssl/ssl.h> unconditionally at the top of the
// file. Zerlegt "http[s]://host[:port]/path" -> host, port, path,
// is_https. Gibt 0 bei unbekanntem Schema.
static int http_parse_url(const char* url, char* host, size_t host_sz,
                          int* port, char* path, size_t path_sz, int* is_https) {
    const char* p = url;
    *is_https = 0;
    if (strncmp(p, "http://", 7) == 0) {
        p += 7;
    } else if (strncmp(p, "https://", 8) == 0) {
        p += 8;
        *is_https = 1;
    } else {
        return 0;
    }

    const char* host_start = p;
    while (*p && *p != ':' && *p != '/') p++;
    size_t hlen = (size_t)(p - host_start);
    if (hlen == 0 || hlen >= host_sz) return 0;
    memcpy(host, host_start, hlen);
    host[hlen] = '\0';

    *port = *is_https ? 443 : 80;
    if (*p == ':') {
        p++;
        *port = atoi(p);
        while (*p && *p != '/') p++;
    }
    if (*p == '\0') {
        snprintf(path, path_sz, "/");
    } else {
        snprintf(path, path_sz, "%s", p);
    }
    return 1;
}

static char* http_recv_all(int fd) {
    size_t cap = 8192, len = 0;
    char* buf = (char*)malloc(cap);
    ssize_t n;
    while ((n = recv(fd, buf + len, cap - len, 0)) > 0) {
        len += (size_t)n;
        if (len == cap) {
            cap *= 2;
            char* grown = (char*)malloc(cap);
            memcpy(grown, buf, len);
            buf = grown;
        }
    }
    buf[len] = '\0';
    return buf;
}

#ifdef TINOX_TLS
static char* http_recv_all_tls(SSL* ssl) {
    size_t cap = 8192, len = 0;
    char* buf = (char*)malloc(cap);
    int n;
    while ((n = SSL_read(ssl, buf + len, (int)(cap - len))) > 0) {
        len += (size_t)n;
        if (len == cap) {
            cap *= 2;
            char* grown = (char*)malloc(cap);
            memcpy(grown, buf, len);
            buf = grown;
        }
    }
    buf[len] = '\0';
    return buf;
}
#endif

// Case-insensitive search for "chunked" within a "Transfer-Encoding"
// header line in the (headers-only, no trailing blank line) block --
// HTTP header names/values are case-insensitive per RFC 7230, and real
// servers vary casing (the Kubernetes API server itself sends
// "Transfer-Encoding: chunked" this exact way for any List response with
// no fixed Content-Length, which is what surfaced this gap: httpGet's
// caller got the raw wire bytes -- literal hex chunk-size lines like
// "800\r\n"/"0\r\n\r\n" -- spliced into the middle of what should have
// been a plain JSON body, silently corrupting it instead of erroring).
static int http_headers_has_chunked_encoding(const char* hdrs, size_t hdr_len) {
    if (!hdrs) return 0;
    size_t te_len = strlen("transfer-encoding");
    for (size_t i = 0; i + te_len <= hdr_len; i++) {
        if (strncasecmp(hdrs + i, "transfer-encoding", te_len) == 0) {
            size_t j = i;
            while (j < hdr_len && hdrs[j] != '\r' && hdrs[j] != '\n') j++;
            for (size_t k = i; k + 7 <= j; k++) {
                if (strncasecmp(hdrs + k, "chunked", 7) == 0) return 1;
            }
        }
    }
    return 0;
}

// Decodes an HTTP/1.1 chunked-transfer-encoded body (RFC 7230 §4.1):
// a sequence of "<hex-size>\r\n<data>\r\n" chunks terminated by a
// zero-size chunk. `body` starts right after the header block's own
// trailing "\r\n\r\n". Chunk extensions (";...") and trailers after the
// terminating chunk are skipped, not surfaced -- no server this runtime
// talks to relies on either. Malformed input (a non-hex chunk-size line)
// stops decoding and returns whatever was successfully decoded so far,
// same "best effort, don't crash" spirit as this file's other lenient
// parsers.
static char* http_dechunk(const char* body) {
    size_t cap = 8192, len = 0;
    char* out = (char*)malloc(cap);
    const char* p = body;
    while (1) {
        char* endptr = NULL;
        long chunk_size = strtol(p, &endptr, 16);
        if (endptr == p || chunk_size < 0) break;
        const char* line_end = strstr(endptr, "\r\n");
        if (!line_end) break;
        p = line_end + 2;
        if (chunk_size == 0) break; // terminating chunk
        if (len + (size_t)chunk_size >= cap) {
            while (len + (size_t)chunk_size >= cap) cap *= 2;
            char* grown = (char*)malloc(cap);
            memcpy(grown, out, len);
            out = grown;
        }
        memcpy(out + len, p, (size_t)chunk_size);
        len += (size_t)chunk_size;
        p += chunk_size;
        if (p[0] == '\r' && p[1] == '\n') p += 2;
    }
    out[len] = '\0';
    return out;
}

// Per-request client options (#525). The legacy thread-local setters fill one of these for
// httpGet/httpPost/...; HttpClient (tinox.core.http) passes its own, so nothing set for one client
// can reach another caller that shares the worker thread. A deadline of 0 means "none".
typedef struct {
    const char* headers;          // "Name: value\r\n" chain
    const char* ca_cert_path;
    const char* client_cert_path;
    const char* client_key_path;
    int64_t     insecure_skip_verify;
    int64_t     connect_ms;       // DNS + TCP connect (+ TLS handshake counts against read/total)
    int64_t     read_ms;          // max idle time of any single read/write
    int64_t     total_ms;         // whole request
    int64_t     pool;             // keep-alive pool id (httpPoolCreate), 0 = one connection per request
} TinoxHttpOpts;

typedef struct {
    int64_t start_ms;
    int64_t total_ms;
    int64_t read_ms;
    char    error[160];
} TinoxHttpClock;

static int64_t http_clock_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

// Milliseconds a blocking step may still take (-1 = unbounded). Sets clk->error and returns 0
// when the total deadline is gone or the surrounding task was cancelled.
static int64_t http_budget_ms(TinoxHttpClock* clk, int64_t step_ms) {
    const char* cancelled = task_cancellation(task_scope);
    if (cancelled) { snprintf(clk->error, sizeof(clk->error), "cancelled: %s", cancelled); return 0; }
    int64_t budget = step_ms > 0 ? step_ms : -1;
    if (clk->total_ms > 0) {
        int64_t left = clk->total_ms - (http_clock_ms() - clk->start_ms);
        if (left <= 0) { snprintf(clk->error, sizeof(clk->error), "total deadline of %lld ms exceeded", (long long)clk->total_ms); return 0; }
        if (budget < 0 || left < budget) budget = left;
    }
    return budget < 0 ? -1 : budget;
}

static void http_set_socket_timeout(int fd, int64_t ms) {
    struct timeval tv = { .tv_sec = 0, .tv_usec = 0 };
    if (ms > 0) { tv.tv_sec = (time_t)(ms / 1000); tv.tv_usec = (suseconds_t)((ms % 1000) * 1000); }
    setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
    setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof(tv));
}

typedef struct {
    pthread_mutex_t mu;
    pthread_cond_t  cv;
    int             done;
    int             abandoned;
    int             rc;
    struct addrinfo* res;
    char            host[256];
    char            port[16];
} TinoxDnsJob;

static void* http_dns_thread(void* arg) {
    TinoxDnsJob* job = (TinoxDnsJob*)arg;
    struct addrinfo hints;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_INET;
    hints.ai_socktype = SOCK_STREAM;
    struct addrinfo* res = NULL;
    int rc = getaddrinfo(job->host, job->port, &hints, &res);
    pthread_mutex_lock(&job->mu);
    job->rc = rc;
    job->res = res;
    job->done = 1;
    int drop = job->abandoned;
    pthread_cond_broadcast(&job->cv);
    pthread_mutex_unlock(&job->mu);
    if (drop) { if (res) freeaddrinfo(res); free(job); }
    return NULL;
}

// getaddrinfo cannot be interrupted, so a bounded lookup runs on a helper thread that is simply
// abandoned (and cleans up after itself) when the deadline passes. IP literals skip it.
static struct addrinfo* http_resolve(const char* host, const char* port, TinoxHttpClock* clk, int64_t connect_ms) {
    struct in_addr probe;
    int literal = inet_pton(AF_INET, host, &probe) == 1;
    if (literal || (connect_ms <= 0 && clk->total_ms <= 0)) {
        struct addrinfo hints, *res = NULL;
        memset(&hints, 0, sizeof(hints));
        hints.ai_family = AF_INET;
        hints.ai_socktype = SOCK_STREAM;
        if (getaddrinfo(host, port, &hints, &res) != 0) { snprintf(clk->error, sizeof(clk->error), "DNS lookup failed for %s", host); return NULL; }
        return res;
    }
    int64_t budget = http_budget_ms(clk, connect_ms);
    if (budget == 0) return NULL;
    TinoxDnsJob* job = (TinoxDnsJob*)calloc(1, sizeof(*job));
    pthread_mutex_init(&job->mu, NULL);
    pthread_condattr_t ca;
    pthread_condattr_init(&ca);
    pthread_condattr_setclock(&ca, CLOCK_MONOTONIC);
    pthread_cond_init(&job->cv, &ca);
    pthread_condattr_destroy(&ca);
    snprintf(job->host, sizeof(job->host), "%s", host);
    snprintf(job->port, sizeof(job->port), "%s", port);
    pthread_t th;
    pthread_attr_t attr;
    pthread_attr_init(&attr);
    pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
    if (pthread_create(&th, &attr, http_dns_thread, job) != 0) {
        pthread_attr_destroy(&attr);
        free(job);
        snprintf(clk->error, sizeof(clk->error), "cannot start DNS lookup");
        return NULL;
    }
    pthread_attr_destroy(&attr);
    int64_t deadline = http_clock_ms() + (budget < 0 ? 3600000 : budget);
    pthread_mutex_lock(&job->mu);
    while (!job->done) {
        struct timespec ts;
        clock_gettime(CLOCK_MONOTONIC, &ts);
        int64_t slice = 50;
        int64_t now = http_clock_ms();
        if (now >= deadline) break;
        if (deadline - now < slice) slice = deadline - now;
        ts.tv_nsec += (slice % 1000) * 1000000L; ts.tv_sec += slice / 1000 + ts.tv_nsec / 1000000000L; ts.tv_nsec %= 1000000000L;
        pthread_cond_timedwait(&job->cv, &job->mu, &ts);
        if (task_cancellation(task_scope)) break;
    }
    struct addrinfo* out = NULL;
    if (job->done) {
        if (job->rc != 0) snprintf(clk->error, sizeof(clk->error), "DNS lookup failed for %s", host);
        out = job->res;
        pthread_mutex_unlock(&job->mu);
        pthread_mutex_destroy(&job->mu);
        free(job);
        return out;
    }
    const char* cancelled = task_cancellation(task_scope);
    if (cancelled) snprintf(clk->error, sizeof(clk->error), "cancelled: %s", cancelled);
    else snprintf(clk->error, sizeof(clk->error), "DNS lookup timed out after %lld ms", (long long)(budget < 0 ? 0 : budget));
    job->abandoned = 1;
    pthread_mutex_unlock(&job->mu);
    return NULL;
}

// Non-blocking connect polled in short slices so the connect deadline, the total deadline and
// task cancellation are all honoured.
static int http_connect_bounded(int fd, const struct sockaddr* addr, socklen_t len, TinoxHttpClock* clk, int64_t connect_ms) {
    int64_t step_deadline = connect_ms > 0 ? http_clock_ms() + connect_ms : 0;
    int flags = fcntl(fd, F_GETFL, 0);
    fcntl(fd, F_SETFL, flags | O_NONBLOCK);
    int rc = connect(fd, addr, len);
    if (rc != 0 && errno != EINPROGRESS) { snprintf(clk->error, sizeof(clk->error), "connect failed: %s", strerror(errno)); return 0; }
    while (rc != 0) {
        int64_t budget = http_budget_ms(clk, 0);
        if (budget == 0) return 0;
        int64_t slice = budget < 0 || budget > 50 ? 50 : budget;
        if (step_deadline) {
            int64_t left = step_deadline - http_clock_ms();
            if (left <= 0) { snprintf(clk->error, sizeof(clk->error), "connect timed out after %lld ms", (long long)connect_ms); return 0; }
            if (left < slice) slice = left;
        }
        struct pollfd pfd = { .fd = fd, .events = POLLOUT };
        int pr = poll(&pfd, 1, (int)slice);
        if (pr < 0 && errno == EINTR) continue;
        if (pr < 0) { snprintf(clk->error, sizeof(clk->error), "connect poll failed"); return 0; }
        if (pr > 0) {
            int err = 0;
            socklen_t el = sizeof(err);
            getsockopt(fd, SOL_SOCKET, SO_ERROR, &err, &el);
            if (err != 0) { snprintf(clk->error, sizeof(clk->error), "connect failed: %s", strerror(err)); return 0; }
            break;
        }
    }
    fcntl(fd, F_SETFL, flags);
    return 1;
}

// One read bounded by the idle and total deadlines and by task cancellation. Returns bytes read,
// 0 on a clean close, -1 when it stopped (clk->error is set for deadlines/cancellation; a plain
// socket error leaves it empty). Blocks in short slices whenever something could stop it.
static ssize_t http_read_some(int fd, void* ssl, TinoxHttpClock* clk, char* dst, size_t room, int64_t* last_progress) {
    for (;;) {
        int64_t budget = http_budget_ms(clk, 0);
        if (budget == 0) return -1;
        int64_t idle_left = -1;
        if (clk->read_ms > 0) {
            idle_left = clk->read_ms - (http_clock_ms() - *last_progress);
            if (idle_left <= 0) { snprintf(clk->error, sizeof(clk->error), "read timed out after %lld ms", (long long)clk->read_ms); return -1; }
        }
        int64_t slice = 0;
        if (budget > 0 || idle_left > 0 || task_scope != NULL) {
            slice = 50;
            if (budget > 0 && budget < slice) slice = budget;
            if (idle_left > 0 && idle_left < slice) slice = idle_left;
        }
        http_set_socket_timeout(fd, slice);
#ifdef TCP_QUICKACK
        // On a reused connection the kernel delays ACKs; a server that sends headers and body in
        // two segments (Nagle) would then stall ~40 ms per request. Re-armed before every read.
        { int one = 1; setsockopt(fd, IPPROTO_TCP, TCP_QUICKACK, &one, sizeof(one)); }
#endif
        ssize_t n;
#ifdef TINOX_TLS
        if (ssl) n = SSL_read((SSL*)ssl, dst, (int)room);
        else
#endif
        n = recv(fd, dst, room, 0);
        if (n > 0) { *last_progress = http_clock_ms(); return n; }
        if (n == 0) return 0;
        if (errno == EINTR || errno == EAGAIN || errno == EWOULDBLOCK) continue; // slice elapsed; limits re-checked above
        return -1;
    }
}

// Reads until the peer closes. Returns the bytes read so far; clk->error is set when the loop
// stopped for any reason but a clean close.
static char* http_recv_bounded(int fd, void* ssl, TinoxHttpClock* clk) {
    size_t cap = 8192, len = 0;
    char* buf = (char*)malloc(cap);
    int64_t last_progress = http_clock_ms();
    for (;;) {
        ssize_t n = http_read_some(fd, ssl, clk, buf + len, cap - len, &last_progress);
        if (n <= 0) break;
        len += (size_t)n;
        if (len == cap) {
            cap *= 2;
            char* grown = (char*)malloc(cap);
            memcpy(grown, buf, len);
            free(buf);
            buf = grown;
        }
    }
    buf[len] = '\0';
    return buf;
}

// ---- Keep-alive connections (#526) -------------------------------------------------------------
// A connection belongs to one pool (one HttpClient) and one origin+TLS-identity key. Pools never
// share sockets, so a client's headers/TLS settings cannot reach another client's traffic.
typedef struct {
    int      fd;
    void*    ssl;          // SSL*
    void*    ctx;          // SSL_CTX* owned by this connection (custom TLS config) or NULL
    int      state;        // 1 idle, 2 checked out
    int      pool;
    int64_t  created_ms;
    int64_t  idle_since_ms;
    char     key[640];
} TinoxHttpConn;

typedef struct {
    int     used;
    int64_t max_per_origin;   // concurrent connections (idle + in use) per origin
    int64_t idle_ms;          // idle connection is dropped after this long
    int64_t lifetime_ms;      // connection is dropped this long after it was created
    int64_t reused, opened, discarded, waits;
} TinoxHttpPool;

#define TINOX_HTTP_POOLS 64
#define TINOX_HTTP_CONNS 512
static TinoxHttpPool g_http_pools[TINOX_HTTP_POOLS];
static TinoxHttpConn* g_http_conns[TINOX_HTTP_CONNS];
static pthread_mutex_t g_http_pool_mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t g_http_pool_cv = PTHREAD_COND_INITIALIZER;

static void http_conn_destroy(TinoxHttpConn* c) {
    if (!c) return;
#ifdef TINOX_TLS
    if (c->ssl) { SSL_shutdown((SSL*)c->ssl); SSL_free((SSL*)c->ssl); }
    if (c->ctx) SSL_CTX_free((SSL_CTX*)c->ctx);
#endif
    if (c->fd >= 0) close(c->fd);
    free(c);
}

// Idle connection the peer already closed (or that has unsolicited bytes) must not be reused.
static int http_conn_alive(TinoxHttpConn* c) {
    struct pollfd pfd = { .fd = c->fd, .events = POLLIN };
    int pr = poll(&pfd, 1, 0);
    return pr == 0;
}

static void http_pool_drop_slot(int i) { g_http_conns[i] = NULL; }

int64_t httpPoolCreate(int64_t max_per_origin, int64_t idle_ms, int64_t lifetime_ms) {
    if (max_per_origin < 1) max_per_origin = 1;
    if (max_per_origin > 64) max_per_origin = 64;
    pthread_mutex_lock(&g_http_pool_mu);
    for (int i = 1; i < TINOX_HTTP_POOLS; i++) {
        if (!g_http_pools[i].used) {
            memset(&g_http_pools[i], 0, sizeof(TinoxHttpPool));
            g_http_pools[i].used = 1;
            g_http_pools[i].max_per_origin = max_per_origin;
            g_http_pools[i].idle_ms = idle_ms;
            g_http_pools[i].lifetime_ms = lifetime_ms;
            pthread_mutex_unlock(&g_http_pool_mu);
            return i;
        }
    }
    pthread_mutex_unlock(&g_http_pool_mu);
    return 0; // 0 = no pool available (caller fails visibly)
}

// Closes the idle connections now; connections currently checked out are closed on release.
int64_t httpPoolClose(int64_t id) {
    if (id <= 0 || id >= TINOX_HTTP_POOLS) return 0;
    TinoxHttpConn* doomed[TINOX_HTTP_CONNS];
    int n = 0;
    pthread_mutex_lock(&g_http_pool_mu);
    g_http_pools[id].used = 0;
    for (int i = 0; i < TINOX_HTTP_CONNS; i++) {
        TinoxHttpConn* c = g_http_conns[i];
        if (c && c->pool == (int)id && c->state == 1) { doomed[n++] = c; http_pool_drop_slot(i); }
    }
    pthread_cond_broadcast(&g_http_pool_cv);
    pthread_mutex_unlock(&g_http_pool_mu);
    for (int i = 0; i < n; i++) http_conn_destroy(doomed[i]);
    return 1;
}

// what: 0 idle, 1 in use, 2 opened total, 3 reused total, 4 discarded total, 5 pool waits
int64_t httpPoolStat(int64_t id, int64_t what) {
    if (id <= 0 || id >= TINOX_HTTP_POOLS) return -1;
    int64_t v = 0;
    pthread_mutex_lock(&g_http_pool_mu);
    if (what == 0 || what == 1) {
        for (int i = 0; i < TINOX_HTTP_CONNS; i++)
            if (g_http_conns[i] && g_http_conns[i]->pool == (int)id && g_http_conns[i]->state == (what == 0 ? 1 : 2)) v++;
    } else if (what == 2) v = g_http_pools[id].opened;
    else if (what == 3) v = g_http_pools[id].reused;
    else if (what == 4) v = g_http_pools[id].discarded;
    else if (what == 5) v = g_http_pools[id].waits;
    pthread_mutex_unlock(&g_http_pool_mu);
    return v;
}

// Returns an idle live connection for the key (*reused = 1), or a reserved empty connection
// (fd = -1) the caller must open, or NULL with clk->error set (pool closed / wait expired).
// Waiting for a free slot counts against the request's total deadline and honours cancellation.
static TinoxHttpConn* http_pool_checkout(int pool, const char* key, TinoxHttpClock* clk, int* reused) {
    *reused = 0;
    TinoxHttpConn* doomed[TINOX_HTTP_CONNS];
    int ndoomed = 0;
    TinoxHttpConn* result = NULL;
    int waited = 0;
    pthread_mutex_lock(&g_http_pool_mu);
    for (;;) {
        if (!g_http_pools[pool].used) { snprintf(clk->error, sizeof(clk->error), "HTTP client is closed"); break; }
        TinoxHttpPool* p = &g_http_pools[pool];
        int64_t now = http_clock_ms();
        int64_t count = 0;
        int free_slot = -1;
        for (int i = 0; i < TINOX_HTTP_CONNS; i++) {
            TinoxHttpConn* c = g_http_conns[i];
            if (!c) { if (free_slot < 0) free_slot = i; continue; }
            if (c->pool != pool || strcmp(c->key, key) != 0) continue;
            if (c->state == 1) {
                int expired = (p->idle_ms > 0 && now - c->idle_since_ms > p->idle_ms) ||
                              (p->lifetime_ms > 0 && now - c->created_ms > p->lifetime_ms);
                if (expired || !http_conn_alive(c)) {
                    doomed[ndoomed++] = c; http_pool_drop_slot(i); p->discarded++;
                    if (free_slot < 0) free_slot = i;
                    continue;
                }
                c->state = 2;
                p->reused++;
                *reused = 1;
                result = c;
                goto done;
            }
            count++;
        }
        if (count < p->max_per_origin && free_slot >= 0) {
            TinoxHttpConn* c = (TinoxHttpConn*)calloc(1, sizeof(*c));
            c->fd = -1; c->state = 2; c->pool = pool; c->created_ms = now;
            snprintf(c->key, sizeof(c->key), "%s", key);
            g_http_conns[free_slot] = c;
            p->opened++;
            result = c;
            goto done;
        }
        // At the limit: wait for a release, in short slices.
        int64_t budget = http_budget_ms(clk, 0);
        if (budget == 0) { if (!clk->error[0]) snprintf(clk->error, sizeof(clk->error), "deadline exceeded"); break; }
        if (!waited) { p->waits++; waited = 1; }
        struct timespec ts;
        clock_gettime(CLOCK_REALTIME, &ts);
        ts.tv_nsec += 50 * 1000000L; ts.tv_sec += ts.tv_nsec / 1000000000L; ts.tv_nsec %= 1000000000L;
        if (clk->total_ms <= 0 && clk->read_ms <= 0 && !task_scope) {
            // No deadline at all would wait forever; a pool without any deadline still must not hang silently.
            if (http_clock_ms() - clk->start_ms > 30000) { snprintf(clk->error, sizeof(clk->error), "connection pool exhausted"); break; }
        }
        pthread_cond_timedwait(&g_http_pool_cv, &g_http_pool_mu, &ts);
    }
done:
    pthread_mutex_unlock(&g_http_pool_mu);
    for (int i = 0; i < ndoomed; i++) http_conn_destroy(doomed[i]);
    return result;
}

// Returns a checked-out connection: back to idle when reusable, otherwise closed.
static void http_pool_release(TinoxHttpConn* c, int reusable) {
    pthread_mutex_lock(&g_http_pool_mu);
    int slot = -1;
    for (int i = 0; i < TINOX_HTTP_CONNS; i++) if (g_http_conns[i] == c) { slot = i; break; }
    int keep = reusable && c->fd >= 0 && g_http_pools[c->pool].used;
    if (keep) {
        c->state = 1;
        c->idle_since_ms = http_clock_ms();
    } else {
        if (slot >= 0) http_pool_drop_slot(slot);
        if (g_http_pools[c->pool].used) g_http_pools[c->pool].discarded++;
    }
    pthread_cond_broadcast(&g_http_pool_cv);
    pthread_mutex_unlock(&g_http_pool_mu);
    if (!keep) http_conn_destroy(c);
}

// Opens (DNS, connect, TLS handshake) into c. Returns 1 on success; on failure clk->error is set
// and c->fd stays -1 with nothing leaked.
static int http_conn_open(TinoxHttpConn* c, const char* host, int port, int is_https, const TinoxHttpOpts* opts, TinoxHttpClock* clk) {
    char port_str[16];
    snprintf(port_str, sizeof(port_str), "%d", port);
    struct addrinfo* res = http_resolve(host, port_str, clk, opts->connect_ms);
    if (!res) { if (!clk->error[0]) snprintf(clk->error, sizeof(clk->error), "DNS lookup failed"); return 0; }
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) { freeaddrinfo(res); snprintf(clk->error, sizeof(clk->error), "socket() failed"); return 0; }
    int connected = (opts->connect_ms > 0 || opts->total_ms > 0 || task_scope)
        ? http_connect_bounded(fd, res->ai_addr, res->ai_addrlen, clk, opts->connect_ms)
        : (connect(fd, res->ai_addr, res->ai_addrlen) == 0 || (snprintf(clk->error, sizeof(clk->error), "connect failed: %s", strerror(errno)), 0));
    freeaddrinfo(res);
    if (!connected) { close(fd); if (!clk->error[0]) snprintf(clk->error, sizeof(clk->error), "connect failed"); return 0; }
    int one = 1;
    setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof(one));
    int64_t io_budget = http_budget_ms(clk, clk->read_ms);
    if (io_budget == 0) { close(fd); if (!clk->error[0]) snprintf(clk->error, sizeof(clk->error), "deadline exceeded"); return 0; }
    // The handshake is bounded by the idle/total deadlines (not sliced).
    http_set_socket_timeout(fd, io_budget < 0 ? 0 : io_budget);
    c->fd = fd;
    if (!is_https) return 1;
#ifdef TINOX_TLS
    // A custom CA/client-cert/insecure-skip-verify config needs its own SSL_CTX rather than the
    // shared g_tls_client_ctx (lazily built once with the system trust store and no client
    // identity) -- mutating the shared one would leak one caller's CA/identity into every other.
    int use_custom_ctx = (opts->ca_cert_path != NULL) || (opts->client_cert_path != NULL) || opts->insecure_skip_verify;
    SSL_CTX* ctx = NULL;
    if (use_custom_ctx) {
        SSL_library_init();
        SSL_load_error_strings();
        OpenSSL_add_ssl_algorithms();
        ctx = SSL_CTX_new(TLS_client_method());
        if (ctx) {
            SSL_CTX_set_min_proto_version(ctx, TLS1_2_VERSION);
            if (opts->ca_cert_path) {
                if (SSL_CTX_load_verify_locations(ctx, opts->ca_cert_path, NULL) != 1) {
                    snprintf(clk->error, sizeof(clk->error), "failed to load CA cert file");
                    SSL_CTX_free(ctx);
                    ctx = NULL;
                }
            } else {
                SSL_CTX_set_default_verify_paths(ctx);
            }
            if (ctx && opts->client_cert_path && opts->client_key_path) {
                if (SSL_CTX_use_certificate_file(ctx, opts->client_cert_path, SSL_FILETYPE_PEM) != 1 ||
                    SSL_CTX_use_PrivateKey_file(ctx, opts->client_key_path, SSL_FILETYPE_PEM) != 1 ||
                    SSL_CTX_check_private_key(ctx) != 1) {
                    snprintf(clk->error, sizeof(clk->error), "failed to load client cert/key");
                    SSL_CTX_free(ctx);
                    ctx = NULL;
                }
            }
        }
    } else {
        if (!g_tls_client_ctx) {
            SSL_library_init();
            SSL_load_error_strings();
            OpenSSL_add_ssl_algorithms();
            g_tls_client_ctx = SSL_CTX_new(TLS_client_method());
            if (g_tls_client_ctx) {
                SSL_CTX_set_min_proto_version(g_tls_client_ctx, TLS1_2_VERSION);
                SSL_CTX_set_default_verify_paths(g_tls_client_ctx);
            }
        }
        ctx = g_tls_client_ctx;
    }
    if (!ctx) { close(fd); c->fd = -1; if (!clk->error[0]) snprintf(clk->error, sizeof(clk->error), "TLS context unavailable"); return 0; }
    SSL* ssl = SSL_new(ctx);
    if (!ssl) { if (use_custom_ctx) SSL_CTX_free(ctx); close(fd); c->fd = -1; snprintf(clk->error, sizeof(clk->error), "TLS session unavailable"); return 0; }
    SSL_set_tlsext_host_name(ssl, host); // SNI
    if (opts->insecure_skip_verify) {
        SSL_set_verify(ssl, SSL_VERIFY_NONE, NULL);
    } else {
        SSL_set_verify(ssl, SSL_VERIFY_PEER, NULL);
        SSL_set1_host(ssl, host); // hostname must match the peer certificate
    }
    SSL_set_fd(ssl, fd);
    // Handshake on a non-blocking socket, polled in short slices so the idle deadline, the total
    // deadline and task cancellation all stop it (a silent peer must not hang a cancelled task).
    int hs_flags = fcntl(fd, F_GETFL, 0);
    fcntl(fd, F_SETFL, hs_flags | O_NONBLOCK);
    int64_t hs_start = http_clock_ms();
    int hs_ok = 0;
    for (;;) {
        int rc = SSL_connect(ssl);
        if (rc == 1) { hs_ok = 1; break; }
        int want = SSL_get_error(ssl, rc);
        if (want != SSL_ERROR_WANT_READ && want != SSL_ERROR_WANT_WRITE) {
            if (!clk->error[0]) snprintf(clk->error, sizeof(clk->error), "TLS handshake failed");
            break;
        }
        int64_t budget = http_budget_ms(clk, 0);
        if (budget == 0) break; // total deadline / cancellation (clk->error set)
        if (clk->read_ms > 0 && http_clock_ms() - hs_start >= clk->read_ms) {
            snprintf(clk->error, sizeof(clk->error), "TLS handshake timed out after %lld ms", (long long)clk->read_ms);
            break;
        }
        int64_t slice = 50;
        if (budget > 0 && budget < slice) slice = budget;
        struct pollfd hp = { .fd = fd, .events = want == SSL_ERROR_WANT_READ ? POLLIN : POLLOUT };
        int pr = poll(&hp, 1, (int)slice);
        if (pr < 0 && errno != EINTR) { snprintf(clk->error, sizeof(clk->error), "TLS handshake failed"); break; }
    }
    fcntl(fd, F_SETFL, hs_flags);
    if (!hs_ok) {
        SSL_free(ssl);
        if (use_custom_ctx) SSL_CTX_free(ctx);
        close(fd);
        c->fd = -1;
        return 0;
    }
    c->ssl = ssl;
    c->ctx = use_custom_ctx ? ctx : NULL;
#endif
    return 1;
}

static int http_conn_write(TinoxHttpConn* c, const char* data, size_t len, TinoxHttpClock* clk) {
    int64_t io_budget = http_budget_ms(clk, clk->read_ms);
    if (io_budget == 0) { if (!clk->error[0]) snprintf(clk->error, sizeof(clk->error), "deadline exceeded"); return 0; }
    http_set_socket_timeout(c->fd, io_budget < 0 ? 0 : io_budget);
    size_t sent = 0;
    while (sent < len) {
        ssize_t w;
#ifdef TINOX_TLS
        if (c->ssl) w = SSL_write((SSL*)c->ssl, data + sent, (int)(len - sent));
        else
#endif
        w = send(c->fd, data + sent, len - sent, MSG_NOSIGNAL);
        if (w <= 0) { if (!clk->error[0]) snprintf(clk->error, sizeof(clk->error), "write failed or timed out"); return 0; }
        sent += (size_t)w;
    }
    return 1;
}

// Value of header `name` (case-insensitive) within a header block, trimmed, into out. 0 = absent.
static int http_header_value(const char* hdrs, size_t hdr_len, const char* name, char* out, size_t out_len) {
    size_t nl = strlen(name);
    size_t i = 0;
    while (i < hdr_len) {
        size_t j = i;
        while (j < hdr_len && hdrs[j] != '\n') j++;
        if (j - i > nl && strncasecmp(hdrs + i, name, nl) == 0 && hdrs[i + nl] == ':') {
            size_t v = i + nl + 1, e = j;
            while (v < e && (hdrs[v] == ' ' || hdrs[v] == '\t')) v++;
            while (e > v && (hdrs[e - 1] == '\r' || hdrs[e - 1] == ' ' || hdrs[e - 1] == '\t')) e--;
            size_t n = e - v < out_len - 1 ? e - v : out_len - 1;
            memcpy(out, hdrs + v, n);
            out[n] = '\0';
            return 1;
        }
        i = j + 1;
    }
    return 0;
}

// Scans a chunked body starting at buf[from]. 1 = complete (*end = index after the final CRLF),
// 0 = more bytes needed, -1 = malformed.
static int http_chunked_end(const char* buf, size_t len, size_t from, size_t* end) {
    size_t p = from;
    for (;;) {
        size_t q = p;
        while (q + 1 < len && !(buf[q] == '\r' && buf[q + 1] == '\n')) q++;
        if (q + 1 >= len) return 0;
        char* ep = NULL;
        errno = 0;
        unsigned long long size = strtoull(buf + p, &ep, 16);
        if (ep == buf + p || errno != 0 || size > (1ULL << 40)) return -1;
        p = q + 2;
        if (size == 0) {
            // trailers: header lines until an empty line
            for (;;) {
                size_t r = p;
                while (r + 1 < len && !(buf[r] == '\r' && buf[r + 1] == '\n')) r++;
                if (r + 1 >= len) return 0;
                if (r == p) { *end = r + 2; return 1; }
                p = r + 2;
            }
        }
        if (len - p < size + 2) return 0;
        p += (size_t)size;
        if (buf[p] != '\r' || buf[p + 1] != '\n') return -1;
        p += 2;
    }
}

// Reads exactly one HTTP/1.1 response (headers + body per its framing). *got_bytes tells whether
// any response byte arrived (a connection that failed before that may be a stale pooled one).
// *reusable is set only when the framing was explicit, nothing extra arrived and neither side
// asked to close. Returns the raw message (NUL-terminated, malloc'd).
static char* http_recv_framed(TinoxHttpConn* c, TinoxHttpClock* clk, const char* method, int* got_bytes, int* reusable) {
    size_t cap = 8192, len = 0;
    char* buf = (char*)malloc(cap);
    int64_t last_progress = http_clock_ms();
    *got_bytes = 0;
    *reusable = 0;
    size_t header_end = 0;           // index just after "\r\n\r\n"
    int have_headers = 0;
    int mode = 0;                    // 0 until close, 1 length, 2 chunked, 3 none
    size_t need = 0;                 // total message length once known (mode 1)
    size_t msg_end = 0;
    int complete = 0;
    int eof = 0;
    for (;;) {
        if (!have_headers) {
            for (size_t i = 0; i + 3 < len; i++) {
                if (buf[i] == '\r' && buf[i + 1] == '\n' && buf[i + 2] == '\r' && buf[i + 3] == '\n') { header_end = i + 4; have_headers = 1; break; }
            }
            if (have_headers) {
                int status = len > 9 ? atoi(buf + 9) : 0;
                if (status >= 100 && status < 200 && status != 101) {
                    // interim response (100 Continue): discard and keep waiting for the real one
                    memmove(buf, buf + header_end, len - header_end);
                    len -= header_end;
                    have_headers = 0;
                    continue;
                }
                char v[128];
                int head = strcmp(method, "HEAD") == 0;
                if (head || status == 204 || status == 304 || status == 101) { mode = 3; need = header_end; }
                else if (http_headers_has_chunked_encoding(buf, header_end)) mode = 2;
                else if (http_header_value(buf, header_end, "Content-Length", v, sizeof(v))) {
                    char* ep = NULL;
                    errno = 0;
                    unsigned long long cl = strtoull(v, &ep, 10);
                    if (ep == v || *ep != '\0' || errno != 0 || cl > (1ULL << 40)) { snprintf(clk->error, sizeof(clk->error), "invalid Content-Length in response"); break; }
                    mode = 1; need = header_end + (size_t)cl;
                } else mode = 0;
            }
        }
        if (have_headers) {
            if (mode == 1 || mode == 3) { if (len >= need) { msg_end = need; complete = 1; break; } }
            else if (mode == 2) {
                size_t end = 0;
                int r = http_chunked_end(buf, len, header_end, &end);
                if (r < 0) { snprintf(clk->error, sizeof(clk->error), "malformed chunked response"); break; }
                if (r == 1) { msg_end = end; complete = 1; break; }
            }
        }
        if (len + 1 >= cap) {
            cap *= 2;
            char* grown = (char*)malloc(cap);
            memcpy(grown, buf, len);
            free(buf);
            buf = grown;
        }
        ssize_t n = http_read_some(c->fd, c->ssl, clk, buf + len, cap - len - 1, &last_progress);
        if (n < 0) { if (!clk->error[0]) snprintf(clk->error, sizeof(clk->error), "read failed"); break; }
        if (n == 0) { eof = 1; break; }
        *got_bytes = 1;
        len += (size_t)n;
    }
    if (eof && have_headers && mode == 0) { /* body delimited by close: valid, never reusable */ }
    else if (eof && !complete && !clk->error[0]) {
        if (len > 0) snprintf(clk->error, sizeof(clk->error), "connection closed before the response was complete");
    }
    if (complete && len == msg_end && !clk->error[0]) {
        char conn_hdr[64];
        int close_requested = http_header_value(buf, header_end, "Connection", conn_hdr, sizeof(conn_hdr)) && strcasestr(conn_hdr, "close");
        int http11 = len >= 8 && strncmp(buf, "HTTP/1.1", 8) == 0;
        *reusable = http11 && !close_requested;
    }
    buf[len] = '\0';
    return buf;
}

static int http_method_idempotent(const char* m) {
    return !strcmp(m, "GET") || !strcmp(m, "HEAD") || !strcmp(m, "PUT") || !strcmp(m, "DELETE") ||
           !strcmp(m, "OPTIONS") || !strcmp(m, "TRACE");
}

static TinoxHttpResponse* http_request_opts(const char* method, const char* url, const char* body, const TinoxHttpOpts* opts) {
    TinoxHttpResponse* resp = (TinoxHttpResponse*)malloc(sizeof(TinoxHttpResponse));
    resp->status = 0;
    resp->body = GC_strdup("");
    resp->headers = GC_strdup("");
    resp->error = GC_strdup("");
    TinoxHttpClock clk = { .start_ms = http_clock_ms(), .total_ms = opts->total_ms, .read_ms = opts->read_ms };
    clk.error[0] = '\0';
#define HTTP_FAIL(msg) do { resp->error = GC_strdup(clk.error[0] ? clk.error : (msg)); return resp; } while (0)

    char host[256], path[2048];
    int port;
    int is_https;
    if (!http_parse_url(url, host, sizeof(host), &port, path, sizeof(path), &is_https)) HTTP_FAIL("invalid or unsupported URL");

#ifndef TINOX_TLS
    if (is_https) HTTP_FAIL("https is not available in this build");
#endif

    size_t body_len = body ? strlen(body) : 0;
    const char* extra = opts->headers ? opts->headers : "";
    char correlation[256] = "";
    if (telemetry_enabled && telemetry_context.trace[0]) {
        snprintf(correlation, sizeof(correlation), "traceparent: 00-%s-%s-%02x\r\nx-request-id: %s\r\n",
                 telemetry_context.trace, telemetry_context.span, telemetry_context.sampled, telemetry_context.request);
    }
    size_t req_cap = strlen(method) + strlen(path) + strlen(host) + strlen(extra) + strlen(correlation) + body_len + 256;
    char* req = (char*)malloc(req_cap);
    int req_len = snprintf(req, req_cap,
        "%s %s HTTP/1.1\r\nHost: %s\r\n%s%s%s"
        "Content-Length: %zu\r\n\r\n",
        method, path, host, opts->pool > 0 ? "" : "Connection: close\r\n", extra, correlation, body_len);
    if (body_len) {
        memcpy(req + req_len, body, body_len);
        req_len += (int)body_len;
    }

    char key[640];
    snprintf(key, sizeof(key), "%s://%s:%d|%s|%s|%s|%lld", is_https ? "https" : "http", host, port,
             opts->ca_cert_path ? opts->ca_cert_path : "", opts->client_cert_path ? opts->client_cert_path : "",
             opts->client_key_path ? opts->client_key_path : "", (long long)opts->insecure_skip_verify);

    char* raw = NULL;
    for (int attempt = 0; attempt < 2; attempt++) {
        TinoxHttpConn local;
        TinoxHttpConn* conn;
        int reused = 0;
        if (opts->pool > 0) {
            conn = http_pool_checkout((int)opts->pool, key, &clk, &reused);
            if (!conn) { free(req); HTTP_FAIL("connection pool unavailable"); }
        } else {
            memset(&local, 0, sizeof(local));
            local.fd = -1;
            conn = &local;
        }
        if (conn->fd < 0 && !http_conn_open(conn, host, port, is_https, opts, &clk)) {
            if (opts->pool > 0) http_pool_release(conn, 0);
            free(req);
            HTTP_FAIL("connect failed");
        }
        int got_bytes = 0, reusable = 0;
        if (http_conn_write(conn, req, (size_t)req_len, &clk)) {
            raw = opts->pool > 0 ? http_recv_framed(conn, &clk, method, &got_bytes, &reusable)
                                 : http_recv_bounded(conn->fd, conn->ssl, &clk);
        } else {
            raw = strdup("");
        }
        if (opts->pool > 0) http_pool_release(conn, reusable && !clk.error[0]);
        else if (conn == &local) {
            if (local.ssl || local.ctx || local.fd >= 0) {
                TinoxHttpConn* heap = (TinoxHttpConn*)malloc(sizeof(*heap));
                *heap = local;
                http_conn_destroy(heap);
            }
        }
        // A reused connection that died before any response byte is stale, not a failed request:
        // replay once on a fresh connection, but only for methods that are safe to repeat.
        int stale = opts->pool > 0 && reused && !got_bytes && attempt == 0 &&
                    (!clk.error[0] || strstr(clk.error, "write failed") || strstr(clk.error, "read failed"));
        if (stale && http_method_idempotent(method)) {
            free(raw);
            raw = NULL;
            clk.error[0] = '\0';
            continue;
        }
        if (stale) {
            snprintf(clk.error, sizeof(clk.error), "pooled connection was closed by the peer; %s not retried automatically", method);
        }
        break;
    }
    free(req);
    if (clk.error[0]) resp->error = GC_strdup(clk.error);

    // Statuszeile: "HTTP/1.1 200 OK"
    const char* sp = strchr(raw, ' ');
    if (sp) resp->status = atoi(sp + 1);

    // Header/Body-Trennung an "\r\n\r\n"
    char* sep = strstr(raw, "\r\n\r\n");
    if (sep) {
        size_t hdr_len = (size_t)(sep - raw);
        char* hdrs = (char*)GC_malloc(hdr_len + 1);
        memcpy(hdrs, raw, hdr_len);
        hdrs[hdr_len] = '\0';
        resp->headers = hdrs;
        const char* raw_body = sep + 4;
        if (http_headers_has_chunked_encoding(hdrs, hdr_len)) {
            char* dechunked = http_dechunk(raw_body);
            resp->body = GC_strdup(dechunked);
            free(dechunked);
        } else {
            resp->body = GC_strdup(raw_body);
        }
    } else {
        resp->body = GC_strdup(raw);
    }
    free(raw);
#undef HTTP_FAIL
    return resp;
}

// Legacy entry: headers and TLS config come from the per-thread setters, no deadlines.
static TinoxHttpResponse* http_request(const char* method, const char* url, const char* body) {
    TinoxHttpOpts opts = {
        .headers = _tinox_http_req_headers,
        .ca_cert_path = _tinox_tls_ca_cert_path,
        .client_cert_path = _tinox_tls_client_cert_path,
        .client_key_path = _tinox_tls_client_key_path,
        .insecure_skip_verify = _tinox_tls_insecure_skip_verify,
    };
    return http_request_opts(method, url, body, &opts);
}

// Instance-scoped entry (#525): every option is an argument, nothing is read from thread state.
// Empty strings mean "unset". Deadlines are milliseconds, 0 = none.
int64_t* httpRequestEx(const char* method, const char* url, const char* body, const char* headers,
                       const char* ca, const char* cert, const char* key, int64_t insecure,
                       int64_t connect_ms, int64_t read_ms, int64_t total_ms, int64_t pool) {
    TinoxHttpOpts opts = {
        .headers = headers && *headers ? headers : NULL,
        .ca_cert_path = ca && *ca ? ca : NULL,
        .client_cert_path = cert && *cert ? cert : NULL,
        .client_key_path = key && *key ? key : NULL,
        .insecure_skip_verify = insecure,
        .connect_ms = connect_ms, .read_ms = read_ms, .total_ms = total_ms, .pool = pool,
    };
    int64_t token = telemetry_enter("http.client", NULL, NULL, 3);
    TinoxHttpResponse* response = http_request_opts(method, url, body, &opts);
    telemetry_leave(token);
    return (int64_t*)response;
}

char* httpError(int64_t* resp) {
    if (!resp) return GC_strdup("no response");
    const char* e = ((TinoxHttpResponse*)resp)->error;
    return GC_strdup(e ? e : "");
}

static int64_t* telemetry_http_request(const char* method, const char* url, const char* body) {
    int64_t token = telemetry_enter("http.client", NULL, NULL, 3);
    TinoxHttpResponse* response = http_request(method, url, body);
    telemetry_leave(token);
    return (int64_t*)response;
}
int64_t* httpGet(const char* url) { return telemetry_http_request("GET", url, NULL); }
int64_t* httpPost(const char* url, const char* body) { return telemetry_http_request("POST", url, body); }
int64_t* httpPut(const char* url, const char* body) { return telemetry_http_request("PUT", url, body); }
int64_t* httpDelete(const char* url) { return telemetry_http_request("DELETE", url, NULL); }
int64_t* httpPatch(const char* url, const char* body) { return telemetry_http_request("PATCH", url, body); }

// ---- Binary-safe conn primitives (WebSocket frames etc.) ----
// httpConnReadRequest/httpConnSendRaw are C-string-based and cut off at
// the first NUL byte — frame data is binary (masking!). These variants
// carry the length explicitly; bytes travel as a Tinox array (one byte per
// i64 slot, 0..255). Works on both plain AND TLS handles via conn_recv/
// conn_send_all.

// Reads EXACTLY n bytes (blocking, loops over short reads). Return value:
// array of the bytes read; a length < n means EOF/error midway through — the
// caller MUST check the length (no silent padding).
//
// `n` is often a value declared directly by the peer (AMQP 0-9-1/
// 1.0 frame size, WS payload length, ...), BEFORE it's known whether that
// many bytes will actually arrive at all. Pre-allocating to the full
// declared `n` (instead of to the bytes actually received) lets a
// malicious/broken peer trigger a disproportionately large allocation
// with a tiny, never-fully-delivered header (found by
// fuzz/amqp091, issue #111: a 7-byte AMQP frame header declaring ~16MB
// triggers a ~128MB array allocation, even if the connection is then
// closed without a single payload byte). Pre-allocating only up to the
// chunk size below (4096) and letting tinox_array_push()'s existing
// amortized doubling handle the rest bounds the worst case to bytes
// actually received instead of to the peer's mere claim.
int64_t* httpConnReadN(int64_t conn, int64_t n) {
    int64_t initial_cap = n > 0 ? (n < 4096 ? n : 4096) : 4;
    int64_t* nh = tinox_array_new(0, initial_cap);
    if (conn <= 0 || n <= 0) return nh;
    TinoxConn* c = (TinoxConn*)(intptr_t)conn;
    unsigned char buf[4096];
    int64_t remaining = n;
    while (remaining > 0) {
        size_t chunk = remaining < (int64_t)sizeof(buf) ? (size_t)remaining : sizeof(buf);
        ssize_t got = conn_recv(c, (char*)buf, chunk);
        if (got <= 0) break;
        for (ssize_t i = 0; i < got; i++) {
            tinox_array_push(nh, (int64_t)buf[i]);
        }
        remaining -= got;
    }
    return nh;
}

// Writes a byte array (values 0..255 per slot) completely to the conn.
// Return value: bytes written (== len) or -1 for an invalid handle.
int64_t httpConnWriteBytes(int64_t conn, int64_t* arr) {
    if (conn <= 0 || !arr) return -1;
    TinoxConn* c = (TinoxConn*)(intptr_t)conn;
    TinoxArray* a = (TinoxArray*)arr;
    if (a->len <= 0) return 0;
    unsigned char* buf = (unsigned char*)malloc((size_t)a->len);
    for (int64_t i = 0; i < a->len; i++) {
        buf[i] = (unsigned char)(a->data[i] & 0xff);
    }
    conn_send_all(c, (const char*)buf, (size_t)a->len);
    free(buf);
    return a->len;
}

// ---- Compact-buffer conn I/O (issue #311) ----
// Length-aware, no per-byte widening: payloads go straight between the
// socket and a tinox.core.bytes buffer.

// Reads EXACTLY n bytes unless EOF/error comes first; the result's length
// is the number actually read (callers MUST check it, same contract as
// httpConnReadN). `n` is often peer-declared, so the buffer grows with the
// bytes that actually arrive (4096-byte steps, doubling) instead of
// trusting the claim -- same bound as httpConnReadN (issue #111).
int64_t httpConnReadBuf(int64_t conn, int64_t n) {
    if (conn <= 0 || n <= 0) return tinoxBufNew(0);
    TinoxConn* c = (TinoxConn*)(intptr_t)conn;
    int64_t cap = n < 4096 ? n : 4096;
    unsigned char* raw = (unsigned char*)GC_malloc_atomic((size_t)cap);
    int64_t used = 0;
    while (used < n) {
        if (used == cap) {
            int64_t ncap = cap * 2 < n ? cap * 2 : n;
            unsigned char* nb = (unsigned char*)GC_malloc_atomic((size_t)ncap);
            memcpy(nb, raw, (size_t)used);
            raw = nb;
            cap = ncap;
        }
        ssize_t got = conn_recv(c, (char*)raw + used, (size_t)(cap - used));
        if (got <= 0) break;
        used += got;
    }
    TinoxBuf* r = tinox_buf_alloc(used);
    memcpy(r->data, raw, (size_t)used);
    return (int64_t)(intptr_t)r;
}

// Writes the whole buffer. Returns bytes written (== len) or -1 for an
// invalid conn handle. Blocks while the peer's receive window is full,
// which is the backpressure bounded streaming relies on.
int64_t httpConnWriteBuf(int64_t conn, int64_t h) {
    if (conn <= 0) return -1;
    TinoxBuf* b = tinox_buf_of(h, "httpConnWriteBuf");
    if (b->len > 0) conn_send_all((TinoxConn*)(intptr_t)conn, (const char*)b->data, (size_t)b->len);
    return b->len;
}

// Exclusive bounded stream over an already connected TinoxConn (TCP/TLS).
// The owner must not mix these calls with legacy I/O or concurrent close.
// All results carry status, compact payload and actual transferred byte count.
static int64_t* tinox_stream_result(int error, int64_t buffer, int64_t count) {
    int64_t* out = tinox_array_new(0, 3);
    tinox_array_push(out, error);
    tinox_array_push(out, buffer);
    tinox_array_push(out, count);
    return out;
}
int64_t httpStreamPrepare(int64_t conn) {
    if (conn <= 0) return EBADF;
    TinoxConn* c = (TinoxConn*)(intptr_t)conn;
    if (atomic_load(&c->closing) || c->fd < 0) return EBADF;
    int flags = fcntl(c->fd, F_GETFL);
    if (flags < 0 || fcntl(c->fd, F_SETFL, flags | O_NONBLOCK)) return errno;
    return 0;
}
static int tinox_stream_wait(TinoxConn* c, short events, int64_t deadline) {
    for (;;) {
        if (task_cancellation(task_scope)) return ECANCELED;
        if (atomic_load(&c->closing)) return EBADF;
        int64_t remaining = deadline - task_clock_ms();
        if (remaining <= 0) return ETIMEDOUT;
        struct pollfd fd = { .fd = c->fd, .events = events };
        int ready = poll(&fd, 1, remaining < 10 ? (int)remaining : 10);
        if (ready < 0 && errno != EINTR) return errno;
        if (ready > 0) return (fd.revents & POLLNVAL) ? EBADF : 0;
    }
}
static ssize_t tinox_stream_io(TinoxConn* c, unsigned char* bytes, size_t size,
                               bool writing, short* wanted) {
#ifdef TINOX_TLS
    if (c->ssl) {
        int n = writing ? SSL_write((SSL*)c->ssl, bytes, (int)size)
                        : SSL_read((SSL*)c->ssl, bytes, (int)size);
        if (n > 0) return n;
        int status = SSL_get_error((SSL*)c->ssl, n);
        if (status == SSL_ERROR_ZERO_RETURN) return 0;
        if (status == SSL_ERROR_WANT_READ || status == SSL_ERROR_WANT_WRITE) {
            *wanted = status == SSL_ERROR_WANT_READ ? POLLIN : POLLOUT;
            errno = EAGAIN; return -1;
        }
        if (status != SSL_ERROR_SYSCALL || !errno) errno = EIO;
        return -1;
    }
#endif
    *wanted = writing ? POLLOUT : POLLIN;
    return writing ? send(c->fd, bytes, size, MSG_NOSIGNAL | MSG_DONTWAIT)
                   : recv(c->fd, bytes, size, MSG_DONTWAIT);
}
int64_t* httpStreamRead(int64_t conn, int64_t maximum, int64_t timeout) {
    if (conn <= 0 || maximum <= 0 || maximum > 1048576 || timeout <= 0 ||
        timeout > 86400000) return tinox_stream_result(EINVAL, tinoxBufNew(0), 0);
    TinoxConn* c = (TinoxConn*)(intptr_t)conn;
    TinoxBuf* buffer = tinox_buf_alloc(maximum);
    int64_t deadline = task_clock_ms() + timeout;
    short wanted = POLLIN;
    int error = 0;
    ssize_t got = -1;
    for (;;) {
        if (task_cancellation(task_scope)) { error = ECANCELED; break; }
        errno = 0;
        got = tinox_stream_io(c, buffer->data, (size_t)maximum, false, &wanted);
        if (got >= 0) break;
        if (errno == EINTR) continue;
        if (errno != EAGAIN && errno != EWOULDBLOCK) { error = errno ? errno : EIO; break; }
        error = tinox_stream_wait(c, wanted, deadline);
        if (error) break;
    }
    buffer->len = error ? 0 : got;
    return tinox_stream_result(error, (int64_t)(intptr_t)buffer, buffer->len);
}
// Read one bounded HTTP/1 header block without consuming any body bytes.
// A total deadline covers the entire header, including continuously arriving
// input. Framing is deliberately Content-Length only at the public API.
int64_t* httpStreamReadHead(int64_t conn, int64_t maximum, int64_t timeout) {
    if (conn <= 0 || maximum < 4 || maximum > 16384 || timeout <= 0 ||
        timeout > 86400000) return tinox_stream_result(EINVAL, tinoxBufNew(0), 0);
    TinoxConn* c = (TinoxConn*)(intptr_t)conn;
    TinoxBuf* buffer = tinox_buf_alloc(maximum);
    size_t used = 0;
    int error = 0;
    int64_t deadline = task_clock_ms() + timeout;
    short wanted = POLLIN;
    while (used < (size_t)maximum) {
        if (task_cancellation(task_scope)) { error = ECANCELED; break; }
        if (task_clock_ms() >= deadline) { error = ETIMEDOUT; break; }
        errno = 0;
        ssize_t got = tinox_stream_io(c, buffer->data + used, 1, false, &wanted);
        if (got > 0) {
            if (!buffer->data[used++]) { error = EILSEQ; break; }
            if (used >= 4 && !memcmp(buffer->data + used - 4, "\r\n\r\n", 4)) break;
            if (used == (size_t)maximum) error = EFBIG;
            continue;
        }
        if (!got) { error = EIO; break; }
        if (errno == EINTR) continue;
        if (errno != EAGAIN && errno != EWOULDBLOCK) { error = errno ? errno : EIO; break; }
        error = tinox_stream_wait(c, wanted, deadline);
        if (error) break;
    }
    buffer->len = error ? 0 : (int64_t)used;
    return tinox_stream_result(error, (int64_t)(intptr_t)buffer, buffer->len);
}

int64_t* httpStreamWrite(int64_t conn, int64_t handle, int64_t timeout) {
    TinoxBuf* buffer = tinox_buf_of(handle, "stream.write");
    if (conn <= 0 || buffer->len > 1048576 || timeout <= 0 ||
        timeout > 86400000) return tinox_stream_result(EINVAL, tinoxBufNew(0), 0);
    TinoxConn* c = (TinoxConn*)(intptr_t)conn;
    int64_t used = 0, deadline = task_clock_ms() + timeout;
    int error = 0;
    short wanted = POLLOUT;
    while (used < buffer->len) {
        if (task_cancellation(task_scope)) { error = ECANCELED; break; }
        errno = 0;
        ssize_t sent = tinox_stream_io(c, buffer->data + used, (size_t)(buffer->len - used), true, &wanted);
        if (sent > 0) { used += sent; continue; }
        if (!sent) { error = EPIPE; break; }
        if (errno == EINTR) continue;
        if (errno != EAGAIN && errno != EWOULDBLOCK) { error = errno ? errno : EIO; break; }
        error = tinox_stream_wait(c, wanted, deadline);
        if (error) break;
    }
    return tinox_stream_result(error, tinoxBufNew(0), used);
}

// Exclusive streams can send TLS close_notify before the legacy concurrent
// teardown shuts down the socket. Cleanup never waits indefinitely for a peer.
void httpStreamClose(int64_t conn) {
    if (conn <= 0) return;
    TinoxConn* c = (TinoxConn*)(intptr_t)conn;
#ifdef TINOX_TLS
    if (c->ssl && !atomic_load(&c->closing)) {
        int64_t deadline = task_clock_ms() + 100;
        for (;;) {
            int status = SSL_shutdown((SSL*)c->ssl);
            if (status >= 0) break; // close_notify sent; peer reply is optional
            int error = SSL_get_error((SSL*)c->ssl, status);
            if (error != SSL_ERROR_WANT_WRITE || task_clock_ms() >= deadline) break;
            struct pollfd fd = { .fd = c->fd, .events = POLLOUT };
            if (poll(&fd, 1, 10) < 0 && errno != EINTR) break;
        }
    }
#endif
    conn_close(c);
}

// Closes a connection (TLS shutdown + free + close).
void httpConnClose(int64_t conn) {
    if (conn <= 0) return;
    conn_close((TinoxConn*)(intptr_t)conn);
}

// Removes the 5s "zombie guard" SO_RCVTIMEO that httpServerAcceptConn/
// httpServerAcceptTls set on every accepted fd (meant to stop a slow-loris
// HTTP client from blocking the single-threaded accept loop forever by
// never finishing a request) -- that guard's own doc comment already notes
// it "persists on `fd` for the life of the connection, so it also protects
// later blocking reads after a successful handshake", which is exactly
// backwards for a WEBSOCKET connection: a legitimately idle WS client (the
// normal, expected state between user interactions, not a slow/broken one)
// hits this same 5s recv() timeout on every blocking Ws::readMessage call,
// gets treated as a dead connection, and is force-closed -- found live via
// a real Tinox-UI app (issue #215's tinox.core:ui) whose WS connection was
// silently reconnecting every ~5-6 seconds of user inactivity, discarding
// all server-side state each time (no session persistence across a
// reconnect, a separate documented v1 limitation) -- from the browser this
// looked like "my clicks keep getting reverted to the initial state for no
// reason". WsServer::accept/acceptTls (tinox.core:websocket) call this
// right after a successful WS handshake -- the zombie guard already did
// its job protecting the HANDSHAKE itself (which reuses the same blocking
// httpServerAcceptConnHandle path), it just has no business staying active
// for the rest of a legitimate long-lived connection's whole lifetime.
// {0,0} is standard POSIX for "no timeout, block indefinitely" (the
// default when SO_RCVTIMEO was never set at all).
void httpConnClearRecvTimeout(int64_t conn) {
    if (conn <= 0) return;
    TinoxConn* c = (TinoxConn*)(intptr_t)conn;
    struct timeval tv = { .tv_sec = 0, .tv_usec = 0 };
    setsockopt(c->fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
}

// Waits up to `ms` for the conn to become readable (data or EOF) WITHOUT consuming anything, so a
// caller can bound a wait and still read whole messages afterwards (a recv timeout in the middle
// of a message would lose the bytes already read). Returns 1 if readable, 0 on timeout, -1 if the
// conn is closed/invalid or poll fails. A concurrent httpConnClose wakes the wait within ~20 ms.
int64_t httpConnWaitReadable(int64_t conn, int64_t ms) {
    if (conn <= 0) return -1;
    TinoxConn* c = (TinoxConn*)(intptr_t)conn;
    if (ms < 0) ms = 0;
    int64_t waited = 0;
    for (;;) {
        if (atomic_load(&c->closing) || c->fd < 0) return -1;
        struct pollfd pfd = { .fd = c->fd, .events = POLLIN };
        int slice = (int)(ms - waited < 20 ? ms - waited : 20);
        int r = poll(&pfd, 1, slice);
        if (r > 0) return (pfd.revents & (POLLIN | POLLHUP | POLLERR)) ? 1 : -1;
        if (r < 0 && errno != EINTR) return -1;
        waited += slice;
        if (waited >= ms) return 0;
    }
}

// Reads a single '\n'-terminated line from a conn (issue #134, SMTP
// client: RFC 5321 is a line-based, \r\n-terminated command/response
// protocol -- unlike HTTP's blank-line-terminated request blocks
// (conn_read_request) or HTTP/2's declared-length frames
// (httpConnReadN), neither existing conn-read primitive fits). Reads one
// byte at a time (SMTP replies are short, at most a few hundred bytes
// per RFC 5321 §4.5.3.1.6, so per-byte recv overhead here is negligible)
// until '\n' or EOF; strips a trailing '\r' if present. Once the
// accumulated line hits an 8192-byte cap, further bytes are still read
// and discarded (not stored) rather than stopping outright -- a peer
// sending an oversized line gets truncated instead of desyncing every
// subsequent readLine() call on this conn with the undrained remainder.
#define TINOX_SMTP_MAX_LINE 8192
char* httpConnReadLine(int64_t conn) {
    if (conn <= 0) return GC_strdup("");
    TinoxConn* c = (TinoxConn*)(intptr_t)conn;
    size_t cap = 256;
    char* buf = (char*)GC_malloc(cap);
    size_t len = 0;
    for (;;) {
        char ch;
        ssize_t got = conn_recv(c, &ch, 1);
        if (got <= 0) break;
        if (ch == '\n') break;
        if (len >= TINOX_SMTP_MAX_LINE) continue;
        if (len + 1 >= cap) {
            size_t ncap = cap * 2;
            char* nbuf = (char*)GC_malloc(ncap);
            memcpy(nbuf, buf, len);
            buf = nbuf;
            cap = ncap;
        }
        buf[len++] = ch;
    }
    if (len > 0 && buf[len - 1] == '\r') len--;
    buf[len] = '\0';
    return buf;
}

// STARTTLS support (issue #134): releases a PLAINTEXT conn's C-level
// wrapper struct and hands back its underlying fd WITHOUT closing it --
// the caller passes that fd straight to httpConnFromFdTls to get a new
// TLS-wrapped conn for the SAME socket (RFC 3207 upgrades an
// already-connected plaintext session in place, unlike implicit TLS's
// connectTls()/httpConnFromFdTls-on-a-fresh-fd). Only valid on a still-
// plaintext conn (ssl == NULL) -- calling it on an already-TLS conn
// would silently discard the SSL* without shutting it down, so that case
// is refused (-1) rather than risking that.
int64_t httpConnTakeFd(int64_t conn) {
    if (conn <= 0) return -1;
    TinoxConn* c = (TinoxConn*)(intptr_t)conn;
    if (c->ssl) return -1;
    int fd = c->fd;
    pthread_mutex_destroy(&c->writeLock);
    free(c);
    return (int64_t)fd;
}

void httpServerClose(int64_t server_fd) {
    close((int)server_fd);
}

// ---- HTTP/3 Server (QUIC/ngtcp2 + nghttp3, RFC 9114) ----
//
// Unlike HttpServer (protocol parsed in C) and Http2Server (raw byte I/O
// in C, framing/HPACK hand-rolled in pure Tinox), HTTP/3 cannot push
// protocol logic into Tinox: ngtcp2 (QUIC transport, RFC 9000) and
// nghttp3 (HTTP/3 framing + QPACK, RFC 9114/9204) are C libraries built
// around C-ABI callback tables, which cannot invoke back into a Tinox
// closure mid-callback. So ALL QUIC/HTTP-3/QPACK state lives here,
// behind opaque Int64 handles -- tinox.core.http3_server.Http3Server
// only registers routes and pumps http3ServerPumpOnce().
//
// Single-threaded, poll()-driven event loop over the ONE bound UDP
// socket: ngtcp2 has no listener/multiplexer concept of its own, so this
// code owns the DCID -> connection demux itself. Not epoll (the existing
// epoll fast-path elsewhere in this file solves a different problem --
// many fds -- whereas QUIC has exactly one fd here) and not
// one-thread-per-connection (ngtcp2_conn/nghttp3_conn are not
// thread-safe). Every blocking syscall (poll/recvfrom/sendto) retries on
// EINTR, same discipline as conn_recv/conn_send above (Bug 68 -- the
// Boehm GC's SIGPWR stop-the-world signal can interrupt any blocking
// syscall on any thread).
#ifdef TINOX_HTTP3
#include <openssl/rand.h>
#include <ngtcp2/ngtcp2.h>
#include <ngtcp2/ngtcp2_crypto.h>
#include <ngtcp2/ngtcp2_crypto_ossl.h>
#include <nghttp3/nghttp3.h>

#define HTTP3_CIDLEN 8
#define HTTP3_CONN_HASH_BUCKETS 64
#define HTTP3_REQ_HASH_BUCKETS  64
#define HTTP3_MAX_UDP_PAYLOAD   1452
#define HTTP3_RETRY_TIMEOUT_NS  (10ULL * NGTCP2_SECONDS)

typedef struct Http3CidEntry {
    ngtcp2_cid cid;
    struct Http3Conn* conn;
    struct Http3CidEntry* next;
} Http3CidEntry;

typedef struct Http3ReqSlot {
    int64_t id;
    struct Http3Conn* conn;
    int64_t streamId;
    char* method;
    char* path;
    void* headersMap;      // TinoxMap*, populated directly in recv_header
    char* body;
    size_t bodyLen;
    size_t bodyCap;
    bool endStreamSeen;
    bool wasEarlyData;
    // Response side, filled by http3SubmitResponse(); read_data callback
    // streams respBody out in HTTP3_RESP_CHUNK-sized pieces (Phase 2).
    bool responseSubmitted;
    char* respBody;
    size_t respBodyLen;
    size_t respBodySent;
    struct Http3ReqSlot* nextReq;
} Http3ReqSlot;

typedef struct Http3Conn {
    ngtcp2_conn* qconn;
    nghttp3_conn* h3conn;
    SSL* ssl;
    ngtcp2_crypto_ossl_ctx* tlsCtx;
    ngtcp2_crypto_conn_ref connRef;
    struct sockaddr_storage remoteAddr;   // refreshed on every read_pkt (migration-safe, Phase 4)
    socklen_t remoteAddrLen;
    int64_t controlStreamId, qencStreamId, qdecStreamId;
    bool handshakeCompleted;
    bool streamsBound;
    bool draining;
    bool currentIs0Rtt;    // transient: set by ngtcp2 recv_stream_data just
                           // before nghttp3_conn_read_stream2, read back by
                           // nghttp3's begin_headers (Phase 5)
    struct Http3Server* server;
    struct Http3Conn* nextActive;   // intrusive list of all live connections
} Http3Conn;

typedef struct Http3Server {
    int udpFd;
    struct sockaddr_in localAddr;
    SSL_CTX* sslCtx;
    Http3CidEntry* cidBuckets[HTTP3_CONN_HASH_BUCKETS];
    Http3ReqSlot* allReqs;   // flat intrusive list, for pump-loop enumeration only
    Http3Conn* activeConns;
    uint8_t statelessResetSecret[32];   // regenerated fresh per process start
                                       // (Phase 3) -- known limitation: a
                                       // stateless reset issued after a
                                       // server restart for a CID from
                                       // before the restart will not
                                       // validate, since the secret isn't
                                       // persisted. Acceptable per the
                                       // plan: full cross-restart
                                       // durability would need the secret
                                       // stored in a file/config instead.
    bool requireRetry;
    uint8_t retrySecret[32];
    bool earlyDataEnabled;
    int64_t maxEarlyDataSize;
} Http3Server;

static uint64_t http3_fnv1a(const uint8_t* data, size_t len) {
    uint64_t h = 14695981039346656037ULL;
    for (size_t i = 0; i < len; i++) h = (h ^ data[i]) * 1099511628211ULL;
    return h;
}

static uint64_t http3_now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ULL + (uint64_t)ts.tv_nsec;
}

static Http3Conn* http3_conn_find(Http3Server* srv, const ngtcp2_cid* cid) {
    uint64_t h = http3_fnv1a(cid->data, cid->datalen) % HTTP3_CONN_HASH_BUCKETS;
    for (Http3CidEntry* e = srv->cidBuckets[h]; e; e = e->next) {
        if (ngtcp2_cid_eq(&e->cid, cid)) return e->conn;
    }
    return NULL;
}

static void http3_conn_register_cid(Http3Server* srv, const ngtcp2_cid* cid, Http3Conn* conn) {
    uint64_t h = http3_fnv1a(cid->data, cid->datalen) % HTTP3_CONN_HASH_BUCKETS;
    Http3CidEntry* e = (Http3CidEntry*)malloc(sizeof(Http3CidEntry));
    e->cid = *cid;
    e->conn = conn;
    e->next = srv->cidBuckets[h];
    srv->cidBuckets[h] = e;
}

static void http3_conn_remove_cid(Http3Server* srv, const ngtcp2_cid* cid) {
    uint64_t h = http3_fnv1a(cid->data, cid->datalen) % HTTP3_CONN_HASH_BUCKETS;
    Http3CidEntry** pp = &srv->cidBuckets[h];
    while (*pp) {
        if (ngtcp2_cid_eq(&(*pp)->cid, cid)) {
            Http3CidEntry* dead = *pp;
            *pp = dead->next;
            free(dead);
            return;
        }
        pp = &(*pp)->next;
    }
}

// The requestId Tinox holds IS the Http3ReqSlot pointer, cast to int64_t
// (same handle convention as TinoxConn/TinoxMap elsewhere in this file) --
// no separate id->slot lookup table needed. allReqs below is a flat list
// used ONLY by http3ServerPumpOnce to enumerate in-flight requests when
// looking for one whose end_stream has fired.
static void http3_req_register(Http3Server* srv, Http3ReqSlot* slot) {
    slot->nextReq = srv->allReqs;
    srv->allReqs = slot;
}

static void http3_req_unregister(Http3Server* srv, Http3ReqSlot* slot) {
    Http3ReqSlot** pp = &srv->allReqs;
    while (*pp) {
        if (*pp == slot) {
            Http3ReqSlot* dead = *pp;
            *pp = dead->nextReq;
            return;
        }
        pp = &(*pp)->nextReq;
    }
}

// ---- ngtcp2 callbacks ----

static int http3_ngtcp2_recv_stream_data(ngtcp2_conn* qconn, uint32_t flags,
                                          int64_t stream_id, uint64_t offset,
                                          const uint8_t* data, size_t datalen,
                                          void* user_data, void* stream_user_data) {
    (void)qconn; (void)offset; (void)stream_user_data;
    Http3Conn* conn = (Http3Conn*)user_data;
    conn->currentIs0Rtt = (flags & NGTCP2_STREAM_DATA_FLAG_0RTT) != 0;
    int fin = (flags & NGTCP2_STREAM_DATA_FLAG_FIN) != 0;
    nghttp3_ssize consumed = nghttp3_conn_read_stream2(conn->h3conn, stream_id, data, datalen, fin, http3_now_ns());
    conn->currentIs0Rtt = false;
    if (consumed < 0) return NGTCP2_ERR_CALLBACK_FAILURE;
    ngtcp2_conn_extend_max_stream_offset(qconn, stream_id, (uint64_t)consumed);
    ngtcp2_conn_extend_max_offset(qconn, (uint64_t)consumed);
    return 0;
}

static int http3_ngtcp2_acked_stream_data_offset(ngtcp2_conn* qconn, int64_t stream_id,
                                                  uint64_t offset, uint64_t datalen,
                                                  void* user_data, void* stream_user_data) {
    (void)qconn; (void)offset; (void)stream_user_data;
    Http3Conn* conn = (Http3Conn*)user_data;
    if (nghttp3_conn_add_ack_offset(conn->h3conn, stream_id, datalen) != 0) {
        return NGTCP2_ERR_CALLBACK_FAILURE;
    }
    return 0;
}

static int http3_ngtcp2_stream_open(ngtcp2_conn* qconn, int64_t stream_id, void* user_data) {
    (void)qconn; (void)stream_id; (void)user_data;
    return 0;
}

static int http3_ngtcp2_stream_close2(ngtcp2_conn* qconn, uint32_t flags, int64_t stream_id,
                                       uint64_t rx_app_error_code, uint64_t tx_app_error_code,
                                       void* user_data, void* stream_user_data) {
    (void)qconn; (void)stream_user_data;
    Http3Conn* conn = (Http3Conn*)user_data;
    uint32_t h3flags = 0;
    uint64_t err = 0;
    if (flags & NGTCP2_STREAM_CLOSE_FLAG_APP_ERROR_CODE_SET) { err = rx_app_error_code; (void)tx_app_error_code; }
    if (conn->h3conn) nghttp3_conn_close_stream(conn->h3conn, stream_id, err); (void)h3flags;
    return 0;
}

static int http3_ngtcp2_handshake_completed(ngtcp2_conn* qconn, void* user_data) {
    Http3Conn* conn = (Http3Conn*)user_data;
    conn->handshakeCompleted = true;
    if (!conn->streamsBound) {
        int rv;
        rv = ngtcp2_conn_open_uni_stream(qconn, &conn->controlStreamId, NULL);
        if (rv == 0) rv = ngtcp2_conn_open_uni_stream(qconn, &conn->qencStreamId, NULL);
        if (rv == 0) rv = ngtcp2_conn_open_uni_stream(qconn, &conn->qdecStreamId, NULL);
        if (rv == 0) rv = nghttp3_conn_bind_control_stream(conn->h3conn, conn->controlStreamId);
        if (rv == 0) rv = nghttp3_conn_bind_qpack_streams(conn->h3conn, conn->qencStreamId, conn->qdecStreamId);
        conn->streamsBound = (rv == 0);
    }
    return 0;
}

static void http3_ngtcp2_rand(uint8_t* dest, size_t destlen, const ngtcp2_rand_ctx* rand_ctx) {
    (void)rand_ctx;
    RAND_bytes(dest, (int)destlen);
}

static int http3_ngtcp2_get_new_connection_id2(ngtcp2_conn* qconn, ngtcp2_cid* cid,
                                                ngtcp2_stateless_reset_token* token,
                                                size_t cidlen, void* user_data) {
    (void)qconn;
    Http3Conn* conn = (Http3Conn*)user_data;
    uint8_t buf[NGTCP2_MAX_CIDLEN];
    RAND_bytes(buf, (int)cidlen);
    ngtcp2_cid_init(cid, buf, cidlen);
    if (ngtcp2_crypto_generate_stateless_reset_token(token->data, conn->server->statelessResetSecret,
                                                      sizeof(conn->server->statelessResetSecret), cid) != 0) {
        return NGTCP2_ERR_CALLBACK_FAILURE;
    }
    http3_conn_register_cid(conn->server, cid, conn);
    return 0;
}

static int http3_ngtcp2_remove_connection_id(ngtcp2_conn* qconn, const ngtcp2_cid* cid, void* user_data) {
    (void)qconn;
    Http3Conn* conn = (Http3Conn*)user_data;
    http3_conn_remove_cid(conn->server, cid);
    return 0;
}

// ---- nghttp3 callbacks ----

static int http3_nghttp3_begin_headers(nghttp3_conn* h3conn, int64_t stream_id,
                                        void* conn_user_data, void* stream_user_data) {
    (void)stream_user_data;
    Http3Conn* conn = (Http3Conn*)conn_user_data;
    Http3ReqSlot* slot = (Http3ReqSlot*)malloc(sizeof(Http3ReqSlot));
    slot->id = (int64_t)(intptr_t)slot;
    slot->conn = conn;
    slot->streamId = stream_id;
    slot->method = NULL;
    slot->path = NULL;
    slot->headersMap = tinox_map_create();
    slot->body = NULL;
    slot->bodyLen = 0;
    slot->bodyCap = 0;
    slot->endStreamSeen = false;
    slot->wasEarlyData = conn->currentIs0Rtt;
    slot->responseSubmitted = false;
    slot->respBody = NULL;
    slot->respBodyLen = 0;
    slot->respBodySent = 0;
    slot->nextReq = NULL;
    http3_req_register(conn->server, slot);
    nghttp3_conn_set_stream_user_data(h3conn, stream_id, slot);
    return 0;
}

static int http3_nghttp3_recv_header(nghttp3_conn* h3conn, int64_t stream_id, int32_t token,
                                      nghttp3_rcbuf* name, nghttp3_rcbuf* value, uint8_t flags,
                                      void* conn_user_data, void* stream_user_data) {
    (void)h3conn; (void)stream_id; (void)token; (void)flags; (void)conn_user_data;
    Http3ReqSlot* slot = (Http3ReqSlot*)stream_user_data;
    if (!slot) return 0;
    nghttp3_vec nv = nghttp3_rcbuf_get_buf(name);
    nghttp3_vec vv = nghttp3_rcbuf_get_buf(value);
    char* nameStr = (char*)malloc(nv.len + 1);
    memcpy(nameStr, nv.base, nv.len);
    nameStr[nv.len] = '\0';
    char* valStr = (char*)malloc(vv.len + 1);
    memcpy(valStr, vv.base, vv.len);
    valStr[vv.len] = '\0';
    if (strcmp(nameStr, ":method") == 0) {
        slot->method = valStr;
    } else if (strcmp(nameStr, ":path") == 0) {
        slot->path = valStr;
    } else if (strcmp(nameStr, ":authority") == 0) {
        tinox_map_set(slot->headersMap, "Host", (int64_t)(intptr_t)valStr); // same-origin checks need it
    } else if (nameStr[0] != ':') {
        // HTTP/3 field names are lowercase on the wire; the handlers look headers up in their
        // canonical HTTP/1 spelling ("Content-Type", "Authorization"), so store them that way.
        for (size_t i = 0; nameStr[i]; i++) {
            nameStr[i] = (char)((i == 0 || nameStr[i - 1] == '-') ? toupper((unsigned char)nameStr[i]) : tolower((unsigned char)nameStr[i]));
        }
        tinox_map_set(slot->headersMap, nameStr, (int64_t)(intptr_t)valStr);
    }
    return 0;
}

static int http3_nghttp3_recv_data(nghttp3_conn* h3conn, int64_t stream_id, const uint8_t* data,
                                    size_t datalen, void* conn_user_data, void* stream_user_data) {
    (void)h3conn;
    Http3ReqSlot* slot = (Http3ReqSlot*)stream_user_data;
    if (slot) {
        if (slot->bodyLen + datalen + 1 > slot->bodyCap) {
            size_t newCap = slot->bodyCap == 0 ? 4096 : slot->bodyCap * 2;
            while (newCap < slot->bodyLen + datalen + 1) newCap *= 2;
            char* nb = (char*)malloc(newCap);
            if (slot->body) { memcpy(nb, slot->body, slot->bodyLen); }
            slot->body = nb;
            slot->bodyCap = newCap;
        }
        memcpy(slot->body + slot->bodyLen, data, datalen);
        slot->bodyLen += datalen;
        slot->body[slot->bodyLen] = '\0';
    }
    // nghttp3_conn_read_stream2's own "consumed" return value (extended in
    // http3_ngtcp2_recv_stream_data) deliberately EXCLUDES DATA-frame
    // application payload bytes -- only recv_data sees those. Since this
    // implementation buffers the whole body eagerly (no real
    // backpressure/deferred_consume), credit for those bytes must be
    // granted here, immediately, or any request body past the initial
    // flow-control window would stall forever.
    Http3Conn* conn = (Http3Conn*)conn_user_data;
    ngtcp2_conn_extend_max_stream_offset(conn->qconn, stream_id, datalen);
    ngtcp2_conn_extend_max_offset(conn->qconn, datalen);
    return 0;
}

static int http3_nghttp3_end_stream(nghttp3_conn* h3conn, int64_t stream_id,
                                     void* conn_user_data, void* stream_user_data) {
    (void)h3conn; (void)stream_id; (void)conn_user_data;
    Http3ReqSlot* slot = (Http3ReqSlot*)stream_user_data;
    if (slot) slot->endStreamSeen = true;
    return 0;
}

static int http3_nghttp3_stream_close(nghttp3_conn* h3conn, int64_t stream_id, uint64_t app_error_code,
                                       void* conn_user_data, void* stream_user_data) {
    (void)h3conn; (void)stream_id; (void)app_error_code; (void)conn_user_data;
    Http3ReqSlot* slot = (Http3ReqSlot*)stream_user_data;
    // This is the ONE place a slot's memory is actually freed. It would
    // be premature to free it as soon as Tinox calls http3ReleaseRequest
    // (right after http3SubmitResponse): submit_response only QUEUES the
    // response, the response body is pulled later via the read_data
    // callback during a subsequent write-pump pass, which needs
    // slot->respBody/respBodyLen/respBodySent to still be valid --
    // freeing eagerly there caused the response body to silently vanish
    // (chunk=0 read back) while curl still got a 200 with headers but an
    // empty body. So http3ReleaseRequest/http3_reject_early_data only
    // unregister the slot from allReqs (stopping it from being
    // re-dispatched); actual deallocation waits for nghttp3 itself to
    // report the stream fully closed, here.
    if (slot) {
        http3_req_unregister(slot->conn->server, slot);
        if (slot->headersMap) tinox_map_free(slot->headersMap);
        if (slot->method) free(slot->method);
        if (slot->path) free(slot->path);
        if (slot->body) free(slot->body);
        if (slot->respBody) free(slot->respBody);
        free(slot);
    }
    return 0;
}

static nghttp3_ssize http3_read_data_cb(nghttp3_conn* h3conn, int64_t stream_id, nghttp3_vec* vec,
                                        size_t veccnt, uint32_t* pflags, void* conn_user_data,
                                        void* stream_user_data) {
    (void)h3conn; (void)stream_id; (void)veccnt; (void)conn_user_data;
    Http3ReqSlot* slot = (Http3ReqSlot*)stream_user_data;
    #define HTTP3_RESP_CHUNK (64 * 1024)
    size_t remaining = slot->respBodyLen - slot->respBodySent;
    size_t chunk = remaining < HTTP3_RESP_CHUNK ? remaining : HTTP3_RESP_CHUNK;
    vec[0].base = (uint8_t*)slot->respBody + slot->respBodySent;
    vec[0].len = chunk;
    slot->respBodySent += chunk;
    if (slot->respBodySent >= slot->respBodyLen) {
        *pflags |= NGHTTP3_DATA_FLAG_EOF;
    }
    return chunk > 0 || (*pflags & NGHTTP3_DATA_FLAG_EOF) ? 1 : 0;
}

static void http3_alpn_select_cb_impl(unsigned char** out, unsigned char* outlen,
                                       const unsigned char* in, unsigned int inlen) {
    unsigned int i = 0;
    while (i + 1 < inlen) {
        unsigned char len = in[i];
        if (i + 1 + len > inlen) break;
        if (len == 2 && in[i + 1] == 'h' && in[i + 2] == '3') {
            *out = (unsigned char*)(in + i + 1);
            *outlen = 2;
            return;
        }
        i += 1 + len;
    }
    *out = NULL;
    *outlen = 0;
}

static int http3_alpn_select_cb(SSL* ssl, const unsigned char** out, unsigned char* outlen,
                                 const unsigned char* in, unsigned int inlen, void* arg) {
    (void)ssl; (void)arg;
    unsigned char* o = NULL;
    http3_alpn_select_cb_impl(&o, outlen, in, inlen);
    if (!o) return SSL_TLSEXT_ERR_NOACK;
    *out = o;
    return SSL_TLSEXT_ERR_OK;
}

static ngtcp2_conn* http3_crypto_get_conn(ngtcp2_crypto_conn_ref* ref) {
    Http3Conn* conn = (Http3Conn*)ref->user_data;
    return conn->qconn;
}

// Creates a fresh Http3Conn + TLS session for a brand-new client, given
// the decoded first-Initial header |hd| and the datagram's source
// address. |odcid|/|retryScid| are only meaningful if a Retry round trip
// already happened (Phase 3); pass odcid=hd->dcid and
// retryScidPresent=false for the plain (no-retry) path.
static Http3Conn* http3_create_conn(Http3Server* srv, const ngtcp2_pkt_hd* hd,
                                     const struct sockaddr* remoteAddr, socklen_t remoteAddrLen,
                                     const ngtcp2_cid* odcid, const ngtcp2_cid* retryScid, bool retryScidPresent) {
    Http3Conn* conn = (Http3Conn*)malloc(sizeof(Http3Conn));
    memset(conn, 0, sizeof(Http3Conn));
    conn->server = srv;
    conn->controlStreamId = -1;
    conn->qencStreamId = -1;
    conn->qdecStreamId = -1;
    memcpy(&conn->remoteAddr, remoteAddr, remoteAddrLen);
    conn->remoteAddrLen = remoteAddrLen;

    conn->ssl = SSL_new(srv->sslCtx);
    conn->connRef.get_conn = http3_crypto_get_conn;
    conn->connRef.user_data = conn;
    SSL_set_app_data(conn->ssl, &conn->connRef);
    SSL_set_accept_state(conn->ssl);
    if (srv->earlyDataEnabled) {
        SSL_set_quic_tls_early_data_enabled(conn->ssl, 1);
    }
    if (ngtcp2_crypto_ossl_configure_server_session(conn->ssl) != 0) {
        SSL_free(conn->ssl);
        free(conn);
        return NULL;
    }
    if (ngtcp2_crypto_ossl_ctx_new(&conn->tlsCtx, conn->ssl) != 0) {
        SSL_free(conn->ssl);
        free(conn);
        return NULL;
    }

    ngtcp2_settings settings;
    ngtcp2_settings_default(&settings);
    settings.initial_ts = http3_now_ns();
    settings.max_tx_udp_payload_size = HTTP3_MAX_UDP_PAYLOAD;
    settings.rand_ctx.native_handle = NULL;

    ngtcp2_transport_params params;
    ngtcp2_transport_params_default(&params);
    params.initial_max_data = 4 * 1024 * 1024;
    params.initial_max_stream_data_bidi_local = 1024 * 1024;
    params.initial_max_stream_data_bidi_remote = 1024 * 1024;
    params.initial_max_stream_data_uni = 1024 * 1024;
    params.initial_max_streams_bidi = 128;
    params.initial_max_streams_uni = 8;
    params.max_idle_timeout = 30ULL * NGTCP2_SECONDS;
    params.max_udp_payload_size = HTTP3_MAX_UDP_PAYLOAD;
    params.active_connection_id_limit = 4;
    params.original_dcid = *odcid;
    params.original_dcid_present = 1;
    if (retryScidPresent) {
        params.retry_scid = *retryScid;
        params.retry_scid_present = 1;
    }

    ngtcp2_callbacks callbacks;
    memset(&callbacks, 0, sizeof(callbacks));
    callbacks.recv_client_initial = ngtcp2_crypto_recv_client_initial_cb;
    callbacks.recv_crypto_data = ngtcp2_crypto_recv_crypto_data_cb;
    callbacks.handshake_completed = http3_ngtcp2_handshake_completed;
    callbacks.encrypt = ngtcp2_crypto_encrypt_cb;
    callbacks.decrypt = ngtcp2_crypto_decrypt_cb;
    callbacks.hp_mask = ngtcp2_crypto_hp_mask_cb;
    callbacks.recv_stream_data = http3_ngtcp2_recv_stream_data;
    callbacks.acked_stream_data_offset = http3_ngtcp2_acked_stream_data_offset;
    callbacks.stream_open = http3_ngtcp2_stream_open;
    callbacks.stream_close2 = http3_ngtcp2_stream_close2;
    callbacks.rand = http3_ngtcp2_rand;
    callbacks.get_new_connection_id2 = http3_ngtcp2_get_new_connection_id2;
    callbacks.remove_connection_id = http3_ngtcp2_remove_connection_id;
    callbacks.update_key = ngtcp2_crypto_update_key_cb;
    callbacks.delete_crypto_aead_ctx = ngtcp2_crypto_delete_crypto_aead_ctx_cb;
    callbacks.delete_crypto_cipher_ctx = ngtcp2_crypto_delete_crypto_cipher_ctx_cb;
    callbacks.get_path_challenge_data2 = ngtcp2_crypto_get_path_challenge_data2_cb;
    callbacks.version_negotiation = ngtcp2_crypto_version_negotiation_cb;

    ngtcp2_cid myScid;
    uint8_t scidBuf[HTTP3_CIDLEN];
    RAND_bytes(scidBuf, HTTP3_CIDLEN);
    ngtcp2_cid_init(&myScid, scidBuf, HTTP3_CIDLEN);

    struct sockaddr_in local = srv->localAddr;
    ngtcp2_path path;
    path.local.addr = (ngtcp2_sockaddr*)&local;
    path.local.addrlen = sizeof(local);
    path.remote.addr = (ngtcp2_sockaddr*)&conn->remoteAddr;
    path.remote.addrlen = conn->remoteAddrLen;
    path.user_data = NULL;

    int rv = ngtcp2_conn_server_new(&conn->qconn, &hd->scid, &myScid, &path,
                                     hd->version, &callbacks, &settings, &params,
                                     NULL, conn);
    if (rv != 0) {
        ngtcp2_crypto_ossl_ctx_del(conn->tlsCtx);
        SSL_free(conn->ssl);
        free(conn);
        return NULL;
    }
    ngtcp2_conn_set_tls_native_handle(conn->qconn, conn->tlsCtx);

    nghttp3_callbacks h3callbacks;
    memset(&h3callbacks, 0, sizeof(h3callbacks));
    h3callbacks.recv_data = http3_nghttp3_recv_data;
    h3callbacks.begin_headers = http3_nghttp3_begin_headers;
    h3callbacks.recv_header = http3_nghttp3_recv_header;
    h3callbacks.end_stream = http3_nghttp3_end_stream;
    h3callbacks.stream_close = http3_nghttp3_stream_close;

    nghttp3_settings h3settings;
    nghttp3_settings_default(&h3settings);
    h3settings.qpack_max_dtable_capacity = 4096;
    h3settings.qpack_encoder_max_dtable_capacity = 4096;
    h3settings.qpack_blocked_streams = 16;

    if (nghttp3_conn_server_new(&conn->h3conn, &h3callbacks, &h3settings, NULL, conn) != 0) {
        ngtcp2_conn_del(conn->qconn);
        ngtcp2_crypto_ossl_ctx_del(conn->tlsCtx);
        SSL_free(conn->ssl);
        free(conn);
        return NULL;
    }

    // Encode our local transport params and hand them to the TLS layer so
    // they ride in the QUIC transport parameters extension.
    uint8_t tpBuf[256];
    ngtcp2_ssize tpLen = ngtcp2_conn_encode_local_transport_params2(conn->qconn, tpBuf, sizeof(tpBuf));
    if (tpLen < 0 || SSL_set_quic_tls_transport_params(conn->ssl, tpBuf, (size_t)tpLen) != 1) {
        nghttp3_conn_del(conn->h3conn);
        ngtcp2_conn_del(conn->qconn);
        ngtcp2_crypto_ossl_ctx_del(conn->tlsCtx);
        SSL_free(conn->ssl);
        free(conn);
        return NULL;
    }

    http3_conn_register_cid(srv, &myScid, conn);
    conn->nextActive = srv->activeConns;
    srv->activeConns = conn;
    return conn;
}

int64_t http3ServerCreate(int64_t port, const char* certPath, const char* keyPath,
                           int64_t requireRetry, int64_t earlyDataEnabled, int64_t maxEarlyDataSize) {
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) return -1;
    int opt = 1;
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &opt, sizeof(opt));
    struct sockaddr_in addr = {0};
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = INADDR_ANY;
    addr.sin_port = htons((uint16_t)port);
    if (bind(fd, (struct sockaddr*)&addr, sizeof(addr)) < 0) { close(fd); return -1; }

    SSL_CTX* ctx = SSL_CTX_new(TLS_server_method());
    if (!ctx) { close(fd); return -1; }
    SSL_CTX_set_min_proto_version(ctx, TLS1_3_VERSION);
    SSL_CTX_set_max_proto_version(ctx, TLS1_3_VERSION);
    if (SSL_CTX_use_certificate_chain_file(ctx, certPath) != 1 ||
        SSL_CTX_use_PrivateKey_file(ctx, keyPath, SSL_FILETYPE_PEM) != 1 ||
        SSL_CTX_check_private_key(ctx) != 1) {
        SSL_CTX_free(ctx);
        close(fd);
        return -1;
    }
    SSL_CTX_set_alpn_select_cb(ctx, http3_alpn_select_cb, NULL);
    if (earlyDataEnabled) {
        // KNOWN LIMITATION: actually calling SSL_CTX_set_max_early_data or
        // its per-connection sibling SSL_set_max_early_data (either one,
        // isolated via bisection) makes every connection silently stall
        // right after the TLS 1.3 handshake completes under this
        // OpenSSL 3.6.3 + ngtcp2_crypto_ossl 1.25.0 combination -- curl
        // completes the handshake and sends its request, but no response
        // (not even the ones queued before this stall) ever comes back.
        // Root cause not isolated further in the time available (no
        // upstream ngtcp2/nghttp3 example server exists on this system to
        // diff against). Session-ticket issuance is enabled below (so a
        // resumed session is at least possible), but max_early_data is
        // deliberately left unset (OpenSSL's default, 0) -- meaning the
        // TLS layer will never actually accept 0-RTT application data,
        // so a client attempting early data transparently falls back to
        // a normal 1-RTT round trip instead of failing. enableEarlyData()
        // /wasEarlyData/the 425-for-non-idempotent-early-data policy are
        // still wired end-to-end and are safe to ship as-is; they will
        // just never observe wasEarlyData=true until this is fixed.
        SSL_CTX_set_session_cache_mode(ctx, SSL_SESS_CACHE_SERVER);
    }

    Http3Server* srv = (Http3Server*)malloc(sizeof(Http3Server));
    memset(srv, 0, sizeof(Http3Server));
    srv->udpFd = fd;
    srv->localAddr = addr;
    srv->sslCtx = ctx;
    srv->requireRetry = requireRetry != 0;
    srv->earlyDataEnabled = earlyDataEnabled != 0;
    srv->maxEarlyDataSize = maxEarlyDataSize;
    RAND_bytes(srv->statelessResetSecret, sizeof(srv->statelessResetSecret));
    RAND_bytes(srv->retrySecret, sizeof(srv->retrySecret));
    tinox_service_listener_ready(httpServerBoundPort(srv->udpFd));
    return (int64_t)(intptr_t)srv;
}

// Sends a Retry packet for a client Initial that had no (or an invalid)
// address-validation token, per RFC 9000 SS8.1. No connection state is
// created -- the client is expected to retry with the returned token.
static void http3_send_retry(Http3Server* srv, const ngtcp2_pkt_hd* hd,
                              const struct sockaddr* remoteAddr, socklen_t remoteAddrLen) {
    ngtcp2_cid retryScid;
    uint8_t scidBuf[HTTP3_CIDLEN];
    RAND_bytes(scidBuf, HTTP3_CIDLEN);
    ngtcp2_cid_init(&retryScid, scidBuf, HTTP3_CIDLEN);

    uint8_t token[256];
    ngtcp2_ssize tokenLen = ngtcp2_crypto_generate_retry_token2(
        token, srv->retrySecret, sizeof(srv->retrySecret), hd->version,
        remoteAddr, remoteAddrLen, &retryScid, &hd->dcid, http3_now_ns());
    if (tokenLen < 0) return;

    uint8_t pkt[512];
    ngtcp2_ssize pktLen = ngtcp2_crypto_write_retry(pkt, sizeof(pkt), hd->version,
                                                     &hd->scid, &retryScid, &hd->dcid,
                                                     token, (size_t)tokenLen);
    if (pktLen < 0) return;
    sendto(srv->udpFd, pkt, (size_t)pktLen, 0, remoteAddr, remoteAddrLen);
}

// Sends a Stateless Reset for a short-header packet whose DCID doesn't
// match any known connection (e.g. server restarted, or connection state
// already torn down) -- re-derives the token from the CID + our secret
// rather than needing per-connection persisted state (RFC 9000 SS10.3).
static void http3_send_stateless_reset(Http3Server* srv, const ngtcp2_cid* dcid,
                                        const struct sockaddr* remoteAddr, socklen_t remoteAddrLen,
                                        size_t recvPktLen) {
    if (recvPktLen < 21) return; // avoid replying to obvious noise/tiny packets
    ngtcp2_stateless_reset_token token;
    if (ngtcp2_crypto_generate_stateless_reset_token(token.data, srv->statelessResetSecret,
                                                      sizeof(srv->statelessResetSecret), dcid) != 0) {
        return;
    }
    uint8_t rnd[64];
    RAND_bytes(rnd, sizeof(rnd));
    size_t randLen = recvPktLen > sizeof(rnd) ? sizeof(rnd) : recvPktLen - NGTCP2_STATELESS_RESET_TOKENLEN;
    if (randLen < NGTCP2_MIN_STATELESS_RESET_RANDLEN) randLen = NGTCP2_MIN_STATELESS_RESET_RANDLEN;
    uint8_t pkt[128];
    ngtcp2_ssize pktLen = ngtcp2_pkt_write_stateless_reset2(pkt, sizeof(pkt), &token, rnd, randLen);
    if (pktLen < 0) return;
    sendto(srv->udpFd, pkt, (size_t)pktLen, 0, remoteAddr, remoteAddrLen);
}

// Marks a request as rejected under the 0-RTT anti-replay policy (Phase
// 5): a non-GET/HEAD request that arrived as early data is replayable by
// a network attacker (no round trip proves the client isn't replaying a
// captured ClientHello+request), so by default it never reaches Tinox
// dispatch -- it gets 425 Too Early (RFC 8470) immediately, forcing the
// client to retry once the 1-RTT handshake completes.
static void http3_reject_early_data(Http3ReqSlot* slot) {
    nghttp3_nv nva[1];
    const char* status = "425";
    nva[0].name = (const uint8_t*)":status";
    nva[0].value = (const uint8_t*)status;
    nva[0].namelen = 7;
    nva[0].valuelen = 3;
    nva[0].flags = NGHTTP3_NV_FLAG_NONE;
    nghttp3_conn_submit_response(slot->conn->h3conn, slot->streamId, nva, 1, NULL);
    // Same lifecycle rule as http3ReleaseRequest: only unregister from
    // the enumeration list here. nghttp3 still owns stream_user_data
    // until it reports the stream closed (stream_close still fires even
    // for a body-less/dr=NULL response), so the actual free happens
    // there, not here.
    http3_req_unregister(slot->conn->server, slot);
    tinoxHttpClearBodyBridge();
}

// Drives the nghttp3<->ngtcp2 write pump for every active connection --
// pulls whatever nghttp3 has queued (headers, response body chunks,
// control/QPACK stream bytes) and pushes it out over the UDP socket.
// Factored out of http3ServerPumpOnce so http3ServerClose's shutdown
// drain can call it directly without going through poll() first (poll()
// waiting on new *inbound* data is pointless when the only thing left to
// do is flush already-queued *outbound* data, e.g. a final response
// right before the connection is torn down).
static void http3_flush_writes(Http3Server* srv) {
    for (Http3Conn* c = srv->activeConns; c; c = c->nextActive) {
        if (c->draining) continue;

        while (1) {
            // One UDP datagram's worth of packet-being-built. Per
            // ngtcp2_conn_writev_stream's docs, every call that
            // participates in coalescing ONE packet (i.e. every call
            // until a non-NGTCP2_ERR_WRITE_MORE result) must pass the
            // exact same conn/path/pi/dest/destlen/ts -- hence dest/path/
            // pi are declared ONCE per packet here, outside the
            // coalescing loop below (a bug during development: an
            // earlier version re-declared `dest` on every coalescing
            // iteration, silently corrupting the in-progress packet so
            // only the first STREAM frame -- e.g. HEADERS -- ever made
            // it out and the response body was lost).
            uint8_t dest[HTTP3_MAX_UDP_PAYLOAD];
            struct sockaddr_in local = c->server->localAddr;
            ngtcp2_path path;
            path.local.addr = (ngtcp2_sockaddr*)&local;
            path.local.addrlen = sizeof(local);
            path.remote.addr = (ngtcp2_sockaddr*)&c->remoteAddr;
            path.remote.addrlen = c->remoteAddrLen;
            path.user_data = NULL;
            ngtcp2_pkt_info pi = { .ecn = 0 };
            ngtcp2_ssize nwrite = 0;
            bool aborted = false;

            while (1) {
                int64_t streamId = -1;
                int fin = 0;
                nghttp3_vec vec[16];
                nghttp3_ssize vecCount = c->h3conn
                    ? nghttp3_conn_writev_stream(c->h3conn, &streamId, &fin, vec, 16) : 0;
                if (vecCount < 0) { c->draining = true; aborted = true; break; }

                uint32_t wflags = NGTCP2_WRITE_STREAM_FLAG_MORE;
                if (fin) wflags |= NGTCP2_WRITE_STREAM_FLAG_FIN;
                ngtcp2_ssize wdatalen = -1;
                nwrite = ngtcp2_conn_writev_stream(
                    c->qconn, &path, &pi, dest, sizeof(dest), &wdatalen, wflags,
                    streamId, (ngtcp2_vec*)vec, (size_t)vecCount, http3_now_ns());

                if (nwrite == NGTCP2_ERR_WRITE_MORE) {
                    if (wdatalen >= 0 && streamId >= 0) {
                        nghttp3_conn_add_write_offset(c->h3conn, streamId, (size_t)wdatalen);
                    }
                    if (vecCount == 0) {
                        // Nothing queued right now but ngtcp2 is still
                        // willing to coalesce more -- per the docs, call
                        // once more with stream_id=-1 to stop coalescing
                        // and finalize whatever is already in the packet.
                        nwrite = ngtcp2_conn_writev_stream(
                            c->qconn, &path, &pi, dest, sizeof(dest), &wdatalen,
                            NGTCP2_WRITE_STREAM_FLAG_NONE, -1, NULL, 0, http3_now_ns());
                        break;
                    }
                    continue;
                }
                if (wdatalen >= 0 && streamId >= 0) {
                    nghttp3_conn_add_write_offset(c->h3conn, streamId, (size_t)wdatalen);
                }
                break;
            }
            if (aborted) break;
            if (nwrite < 0) { c->draining = true; break; }
            ngtcp2_conn_update_pkt_tx_time(c->qconn, http3_now_ns());
            if (nwrite == 0) break; // nothing more to send this round
            ssize_t sr;
            do {
                sr = sendto(c->server->udpFd, dest, (size_t)nwrite, 0,
                            (struct sockaddr*)&c->remoteAddr, c->remoteAddrLen);
            } while (sr < 0 && errno == EINTR);
        }
    }
}

// One unit of native work: drains ready datagrams, services expired
// ngtcp2 timers, drives the nghttp3<->ngtcp2 write pump for every
// connection with pending output, and returns the id of the first fully
// -arrived HTTP/3 request (end_stream fired), -1 if nothing to dispatch
// this tick, or -2 on a fatal socket error.
int64_t http3ServerPumpOnce(int64_t serverHandle) {
    if (serviceIsStopping()) return -2;
    Http3Server* srv = (Http3Server*)(intptr_t)serverHandle;

    // 1. Compute the poll() timeout from the soonest ngtcp2 timer expiry
    // across all active connections (capped so an idle server still
    // wakes periodically), then wait for the UDP socket to be readable.
    uint64_t now = http3_now_ns();
    uint64_t soonest = now + 1000ULL * 1000000ULL; // 1000ms cap
    for (Http3Conn* c = srv->activeConns; c; c = c->nextActive) {
        ngtcp2_tstamp exp = ngtcp2_conn_get_expiry2(c->qconn);
        if (exp != UINT64_MAX && exp < soonest) soonest = exp;
    }
    int timeoutMs = soonest > now ? (int)((soonest - now) / 1000000ULL) : 0;
    if (timeoutMs > (service_enabled() ? 10 : 1000)) timeoutMs = service_enabled() ? 10 : 1000;

    struct pollfd pfd = { .fd = srv->udpFd, .events = POLLIN, .revents = 0 };
    int pr;
    do { pr = poll(&pfd, 1, timeoutMs); } while (pr < 0 && errno == EINTR);
    if (pr < 0) return -2;

    // 2. Drain every ready datagram in this wake.
    if (pr > 0 && (pfd.revents & POLLIN)) {
        uint8_t buf[65536];
        while (1) {
            struct sockaddr_storage peer;
            socklen_t peerLen = sizeof(peer);
            ssize_t n;
            do { n = recvfrom(srv->udpFd, buf, sizeof(buf), MSG_DONTWAIT, (struct sockaddr*)&peer, &peerLen); }
            while (n < 0 && errno == EINTR);
            if (n < 0) {
                if (errno == EAGAIN || errno == EWOULDBLOCK) break;
                return -2;
            }
            if (n == 0) continue;

            ngtcp2_version_cid vc;
            int vcRv = ngtcp2_pkt_decode_version_cid(&vc, buf, (size_t)n, HTTP3_CIDLEN);
            if (vcRv != 0) continue; // unparseable / needs version negotiation -- drop (out of scope)

            ngtcp2_cid dcid;
            ngtcp2_cid_init(&dcid, vc.dcid, vc.dcidlen);
            Http3Conn* conn = http3_conn_find(srv, &dcid);

            if (!conn) {
                if (vc.version == 0) {
                    // Short header, unknown connection -- either a stray
                    // packet or a client whose state we've lost (e.g. we
                    // restarted). Reply with a Stateless Reset rather
                    // than silently dropping (RFC 9000 SS10.3).
                    http3_send_stateless_reset(srv, &dcid, (struct sockaddr*)&peer, peerLen, (size_t)n);
                    continue;
                }
                ngtcp2_pkt_hd hd;
                if (ngtcp2_accept(&hd, buf, (size_t)n) != 0) continue; // not an acceptable first packet

                ngtcp2_cid odcid = hd.dcid;
                ngtcp2_cid retryScid;
                bool retryScidPresent = false;
                if (srv->requireRetry) {
                    if (hd.tokenlen == 0) {
                        http3_send_retry(srv, &hd, (struct sockaddr*)&peer, peerLen);
                        continue;
                    }
                    if (ngtcp2_crypto_verify_retry_token2(&odcid, hd.token, hd.tokenlen,
                                                           srv->retrySecret, sizeof(srv->retrySecret),
                                                           hd.version, (struct sockaddr*)&peer, peerLen,
                                                           &hd.dcid, HTTP3_RETRY_TIMEOUT_NS, http3_now_ns()) != 0) {
                        continue; // bad/expired token -- drop
                    }
                    retryScid = hd.dcid;
                    retryScidPresent = true;
                }

                conn = http3_create_conn(srv, &hd, (struct sockaddr*)&peer, peerLen,
                                          &odcid, &retryScid, retryScidPresent);
                if (!conn) continue;
            } else {
                // Migration-safety (Phase 4): always refresh the remote
                // address from the datagram that just arrived rather
                // than trusting whatever was cached at connection
                // creation -- a NAT rebind changes the peer's 4-tuple
                // without any explicit signal.
                memcpy(&conn->remoteAddr, &peer, peerLen);
                conn->remoteAddrLen = peerLen;
            }

            struct sockaddr_in local = srv->localAddr;
            ngtcp2_path path;
            path.local.addr = (ngtcp2_sockaddr*)&local;
            path.local.addrlen = sizeof(local);
            path.remote.addr = (ngtcp2_sockaddr*)&conn->remoteAddr;
            path.remote.addrlen = conn->remoteAddrLen;
            path.user_data = NULL;
            ngtcp2_pkt_info pi = { .ecn = 0 };

            int rv = ngtcp2_conn_read_pkt(conn->qconn, &path, &pi, buf, (size_t)n, http3_now_ns());
            if (rv != 0) {
                conn->draining = true;
            }
        }
    }

    // 3. Service expired ngtcp2 timers (retransmission, idle timeout, ...).
    now = http3_now_ns();
    for (Http3Conn* c = srv->activeConns; c; c = c->nextActive) {
        if (c->draining) continue;
        if (ngtcp2_conn_get_expiry2(c->qconn) <= now) {
            if (ngtcp2_conn_handle_expiry(c->qconn, now) != 0) {
                c->draining = true;
            }
        }
    }

    // 4. Drive the nghttp3<->ngtcp2 write pump for every connection.
    int64_t readyRequestId = -1;
    http3_flush_writes(srv);

    // 5. Surface the first fully-arrived request whose response hasn't
    // been submitted yet. 0-RTT anti-replay policy (Phase 5) is applied
    // here, before Tinox ever sees the request: a non-GET/HEAD request
    // that arrived as early data is replayable, so it's rejected with
    // 425 Too Early right here rather than being handed to a route
    // handler (no override flag for accepting non-idempotent early data
    // is implemented yet -- see http3_reject_early_data's comment).
    for (Http3ReqSlot* s = srv->allReqs; s; s = s->nextReq) {
        if (!s->endStreamSeen || s->responseSubmitted) continue;
        bool isSafeMethod = s->method && (strcmp(s->method, "GET") == 0 || strcmp(s->method, "HEAD") == 0);
        if (s->wasEarlyData && !isSafeMethod) {
            s->responseSubmitted = true;
            http3_reject_early_data(s);
            continue;
        }
        readyRequestId = s->id;
        break;
    }

    return readyRequestId;
}

char* http3RequestMethod(int64_t requestId) {
    Http3ReqSlot* slot = (Http3ReqSlot*)(intptr_t)requestId;
    return slot->method ? strdup(slot->method) : strdup("");
}

char* http3RequestPath(int64_t requestId) {
    Http3ReqSlot* slot = (Http3ReqSlot*)(intptr_t)requestId;
    return slot->path ? strdup(slot->path) : strdup("");
}

void* http3RequestHeaders(int64_t requestId) {
    Http3ReqSlot* slot = (Http3ReqSlot*)(intptr_t)requestId;
    return slot->headersMap;
}

char* http3RequestBody(int64_t requestId) {
    Http3ReqSlot* slot = (Http3ReqSlot*)(intptr_t)requestId;
    char *body=malloc(slot->bodyLen+1);
    if (slot->bodyLen) memcpy(body,slot->body,slot->bodyLen);
    body[slot->bodyLen]=0;
    g_json_body_ptr=body;g_json_body_len=slot->bodyLen;
    g_req_body_ptr=body;g_req_body_len=(int64_t)slot->bodyLen;
    return body;
}

int64_t http3RequestWasEarlyData(int64_t requestId) {
    Http3ReqSlot* slot = (Http3ReqSlot*)(intptr_t)requestId;
    return slot->wasEarlyData ? 1 : 0;
}

void http3SubmitResponse(int64_t requestId, int64_t statusCode, void* headersMap, const char* body) {
    Http3ReqSlot* slot = (Http3ReqSlot*)(intptr_t)requestId;
    if (slot->responseSubmitted) return;
    slot->responseSubmitted = true;

    char statusBuf[8];
    int statusLen = snprintf(statusBuf, sizeof(statusBuf), "%lld", (long long)statusCode);

    int64_t* keysHandle = tinox_map_keys(headersMap);
    TinoxArray* keys = (TinoxArray*)keysHandle;
    size_t nvCount = (size_t)keys->len + 1;
    nghttp3_nv* nva = (nghttp3_nv*)malloc(sizeof(nghttp3_nv) * nvCount);
    nva[0].name = (const uint8_t*)":status";
    nva[0].namelen = 7;
    nva[0].value = (const uint8_t*)strdup(statusBuf);
    nva[0].valuelen = (size_t)statusLen;
    nva[0].flags = NGHTTP3_NV_FLAG_NONE;
    for (int64_t i = 0; i < keys->len; i++) {
        const char* keyStr = (const char*)(intptr_t)keys->data[i];
        char* lowerKey = strdup(keyStr);
        for (char* p = lowerKey; *p; p++) *p = (char)tolower((unsigned char)*p);
        { char* sp = strchr(lowerKey, ' '); if (sp) *sp = '\0'; } // repeated field "<Name> <n>" (#528)
        const char* valStr = (const char*)(intptr_t)tinox_map_get(headersMap, keyStr);
        nva[i + 1].name = (const uint8_t*)lowerKey;
        nva[i + 1].namelen = strlen(lowerKey);
        nva[i + 1].value = (const uint8_t*)(valStr ? valStr : "");
        nva[i + 1].valuelen = valStr ? strlen(valStr) : 0;
        nva[i + 1].flags = NGHTTP3_NV_FLAG_NONE;
    }

    size_t bodyLen = (size_t)tinoxHttpBodyLength(body);
    slot->respBody = bodyLen > 0 ? malloc(bodyLen) : NULL;
    if (bodyLen) memcpy(slot->respBody, body, bodyLen);
    g_resp_bytes_ptr = NULL;
    g_resp_bytes_len = 0;
    slot->respBodyLen = bodyLen;
    slot->respBodySent = 0;

    nghttp3_data_reader dr;
    dr.read_data = http3_read_data_cb;
    nghttp3_conn_submit_response(slot->conn->h3conn, slot->streamId, nva, nvCount, bodyLen > 0 ? &dr : NULL);
    free(nva);
}

// Only unregisters the slot from the pump loop's enumeration list -- the
// response body is still pulled from it later via the read_data
// callback, so the memory itself must stay alive until nghttp3 reports
// the stream fully closed (http3_nghttp3_stream_close, which does the
// actual free).
void http3ReleaseRequest(int64_t requestId) {
    Http3ReqSlot* slot = (Http3ReqSlot*)(intptr_t)requestId;
    http3_req_unregister(slot->conn->server, slot);
}

// Graceful shutdown: notify every active connection (RFC 9114 SS5.2
// GOAWAY-equivalent -- nghttp3_conn_submit_shutdown_notice) so in-flight
// requests/responses get a chance to finish instead of being abruptly
// reset, then flush queued writes a few times before closing the socket.
// Calls http3_flush_writes directly rather than the full
// http3ServerPumpOnce -- the latter's poll() would needlessly wait (up to
// ~1s per call) for new *inbound* data that isn't the point here; a
// final queued response (e.g. from a handler that just called
// Http3Server.stop()) needs to go out immediately, not after an
// unrelated read timeout. A short sleep between flushes gives the peer's
// ACK a chance to arrive so any still-pending retransmission also gets a
// shot at going out.
void http3ServerClose(int64_t serverHandle) {
    Http3Server* srv = (Http3Server*)(intptr_t)serverHandle;
    for (Http3Conn* c = srv->activeConns; c; c = c->nextActive) {
        if (!c->draining && c->h3conn) nghttp3_conn_submit_shutdown_notice(c->h3conn);
    }
    for (int i = 0; i < 10; i++) {
        http3_flush_writes(srv);
        struct timespec ts = { .tv_sec = 0, .tv_nsec = 20 * 1000000L }; // 20ms
        nanosleep(&ts, NULL);
    }
    close(srv->udpFd);
    SSL_CTX_free(srv->sslCtx);
}
#endif // TINOX_HTTP3

// ---- AES-256-GCM (Issue 74) ----
//
// Behind the same TINOX_TLS switch as the rest of OpenSSL (no new
// build dependency) -- without the flag, aesEncryptRaw/aesDecryptRaw return ""
// instead of a link error, analogous to httpConnFromFdTls & co.
//
// AES-GCM instead of CBC: authenticated encryption (integrity +
// confidentiality in one), no padding-oracle risk. The key is derived
// via SHA-256 from the arbitrary-length `key` string (the same trick
// as in hmacSha256Hash for keys > 64 bytes) -- ALWAYS a valid
// 256-bit key, no silent truncation/padding. The nonce is 12
// random bytes PER call via RAND_bytes (cryptographically secure, not
// the simple PRNG behind randomInt) -- nonce reuse under
// the same key is catastrophic for GCM (breaks authenticity).
// Return format: hex(nonce[12] || ciphertext[N] || tag[16]) -- hex, because
// Tinox strings are internally C strings (strlen-based) and raw
// binary bytes (including possible 0 bytes) would be silently truncated;
// an "" return is the error sentinel (a genuine success is already
// 56 hex characters long even for empty plaintext, never empty).
#ifdef TINOX_TLS
#include <openssl/evp.h>
#include <openssl/rand.h>
#endif

char* aesEncryptRaw(const char* data, const char* key) {
#ifdef TINOX_TLS
    unsigned char aes_key[32];
    sha256_raw((const unsigned char*)key, strlen(key), aes_key);

    unsigned char nonce[12];
    if (RAND_bytes(nonce, sizeof(nonce)) != 1) {
        fprintf(stderr, "aesEncryptRaw: RAND_bytes failed\n");
        return GC_strdup("");
    }

    size_t data_len = strlen(data);
    unsigned char* ciphertext = (unsigned char*)GC_malloc(data_len > 0 ? data_len : 1);
    unsigned char tag[16];

    EVP_CIPHER_CTX* ctx = EVP_CIPHER_CTX_new();
    int out_len = 0, total_len = 0;
    int ok = ctx != NULL
        && EVP_EncryptInit_ex(ctx, EVP_aes_256_gcm(), NULL, NULL, NULL) == 1
        && EVP_CIPHER_CTX_ctrl(ctx, EVP_CTRL_GCM_SET_IVLEN, sizeof(nonce), NULL) == 1
        && EVP_EncryptInit_ex(ctx, NULL, NULL, aes_key, nonce) == 1
        && EVP_EncryptUpdate(ctx, ciphertext, &out_len, (const unsigned char*)data, (int)data_len) == 1;
    if (ok) {
        total_len = out_len;
        ok = EVP_EncryptFinal_ex(ctx, ciphertext + total_len, &out_len) == 1;
        total_len += out_len;
    }
    if (ok) {
        ok = EVP_CIPHER_CTX_ctrl(ctx, EVP_CTRL_GCM_GET_TAG, sizeof(tag), tag) == 1;
    }
    if (ctx) EVP_CIPHER_CTX_free(ctx);
    if (!ok) {
        ERR_print_errors_fp(stderr);
        return GC_strdup("");
    }

    size_t out_bytes_len = sizeof(nonce) + (size_t)total_len + sizeof(tag);
    unsigned char* out_bytes = (unsigned char*)GC_malloc(out_bytes_len);
    memcpy(out_bytes, nonce, sizeof(nonce));
    memcpy(out_bytes + sizeof(nonce), ciphertext, (size_t)total_len);
    memcpy(out_bytes + sizeof(nonce) + (size_t)total_len, tag, sizeof(tag));
    return tinox_bytes_to_hex(out_bytes, out_bytes_len);
#else
    (void)data; (void)key;
    fprintf(stderr, "aesEncryptRaw: runtime built without TLS/OpenSSL (TINOX_TLS=0)\n");
    return GC_strdup("");
#endif
}

char* aesDecryptRaw(const char* hexInput, const char* key) {
#ifdef TINOX_TLS
    size_t hex_len = strlen(hexInput);
    // Minimum: 12 Byte Nonce + 16 Byte Tag, 0 Byte Ciphertext erlaubt (leerer
    // Klartext) -- als Hex also mindestens (12+16)*2 = 56 Zeichen.
    if (hex_len < 56 || (hex_len % 2) != 0) {
        fprintf(stderr, "aesDecryptRaw: Eingabe zu kurz oder ungueltige Hex-Laenge\n");
        return GC_strdup("");
    }
    size_t raw_len = hex_len / 2;
    unsigned char* raw = (unsigned char*)GC_malloc(raw_len);
    for (size_t i = 0; i < raw_len; i++) {
        int hi = tinox_hex_nibble(hexInput[i*2]);
        int lo = tinox_hex_nibble(hexInput[i*2 + 1]);
        if (hi < 0 || lo < 0) {
            fprintf(stderr, "aesDecryptRaw: ungueltiges Hex-Zeichen\n");
            return GC_strdup("");
        }
        raw[i] = (unsigned char)((hi << 4) | lo);
    }

    const unsigned char* nonce = raw;
    size_t ct_len = raw_len - 12 - 16;
    const unsigned char* ciphertext = raw + 12;
    unsigned char* tag = raw + 12 + ct_len;

    unsigned char aes_key[32];
    sha256_raw((const unsigned char*)key, strlen(key), aes_key);

    // +2 instead of +1: room for the success marker (see below) BEFORE the plaintext.
    unsigned char* plaintext = (unsigned char*)GC_malloc(ct_len + 2);
    EVP_CIPHER_CTX* ctx = EVP_CIPHER_CTX_new();
    int out_len = 0, total_len = 0;
    int ok = ctx != NULL
        && EVP_DecryptInit_ex(ctx, EVP_aes_256_gcm(), NULL, NULL, NULL) == 1
        && EVP_CIPHER_CTX_ctrl(ctx, EVP_CTRL_GCM_SET_IVLEN, 12, NULL) == 1
        && EVP_DecryptInit_ex(ctx, NULL, NULL, aes_key, nonce) == 1
        && EVP_DecryptUpdate(ctx, plaintext + 1, &out_len, ciphertext, (int)ct_len) == 1;
    if (ok) {
        total_len = out_len;
        ok = EVP_CIPHER_CTX_ctrl(ctx, EVP_CTRL_GCM_SET_TAG, 16, tag) == 1;
    }
    if (ok) {
        // Return value <= 0 means: authentication failed (wrong
        // key OR tampered/corrupted ciphertext) -- a hard
        // error instead of silently wrong plaintext (no silent garbage).
        ok = EVP_DecryptFinal_ex(ctx, plaintext + 1 + total_len, &out_len) == 1;
        total_len += out_len;
    }
    if (ctx) EVP_CIPHER_CTX_free(ctx);
    if (!ok) {
        fprintf(stderr, "aesDecryptRaw: authentication failed (wrong key or tampered data)\n");
        return GC_strdup("");
    }
    // Success marker "1" prepended: an empty plaintext (total_len==0,
    // e.g. decrypting Crypto::aesEncrypt("", key)) would otherwise be
    // indistinguishable from the "" error sentinel -- the caller
    // (Crypto::aesDecrypt) checks result.len()==0 for an error, so it would
    // otherwise wrongly treat a valid empty plaintext as an error.
    plaintext[0] = '1';
    plaintext[1 + total_len] = '\0';
    return (char*)plaintext;
#else
    (void)hexInput; (void)key;
    fprintf(stderr, "aesDecryptRaw: runtime built without TLS/OpenSSL (TINOX_TLS=0)\n");
    return GC_strdup("");
#endif
}

// Cryptographically secure random bytes via RAND_bytes (issue #131, OAuth2
// `state`/PKCE `code_verifier`) -- distinct from `randomInt`/`randomFloat`
// (tinox.core.random), which are backed by `srandom(time^getpid)`: fine
// for jitter/test data, but predictable, which defeats the entire point of
// an OAuth2 CSRF `state` value or a PKCE verifier. Returns an empty array
// on any failure (TINOX_TLS=0, or RAND_bytes itself failing) rather than
// a short/zero-filled one -- the caller (Crypto::secureRandomBytes) throws
// on a length mismatch instead of silently using weak/absent randomness.
int64_t* secureRandomBytesRaw(int64_t n) {
#ifdef TINOX_TLS
    if (n <= 0) return tinox_array_new(0, 4);
    unsigned char* buf = (unsigned char*)malloc((size_t)n);
    if (RAND_bytes(buf, (int)n) != 1) {
        fprintf(stderr, "secureRandomBytesRaw: RAND_bytes failed\n");
        free(buf);
        return tinox_array_new(0, 4);
    }
    int64_t* nh = tinox_array_new(0, n);
    for (int64_t i = 0; i < n; i++) tinox_array_push(nh, buf[i]);
    free(buf);
    return nh;
#else
    (void)n;
    fprintf(stderr, "secureRandomBytesRaw: runtime built without TLS/OpenSSL (TINOX_TLS=0)\n");
    return tinox_array_new(0, 4);
#endif
}

#ifdef TINOX_TLS
#include <openssl/bn.h>
#include <openssl/core_names.h>
#include <openssl/param_build.h>
#endif

// RS256 (RSASSA-PKCS1-v1_5 using SHA-256) signature verification for OIDC
// ID-token / JWKS support (issue #138). `modulus`/`exponent` are an RSA
// public key's raw big-endian bytes (JWK "n"/"e" fields, base64url-decoded
// by the caller) -- constructed into an EVP_PKEY via OSSL3's
// EVP_PKEY_fromdata (no deprecated legacy RSA_new/RSA_set0_key calls).
// Returns false (never a garbage/partial "verified") on any failure,
// including TINOX_TLS=0 -- Jwt::decodeRs256/verifyRs256 treat that the
// same as any other failed signature check.
bool rsaVerifySha256(int64_t* msgArr, int64_t* sigArr, int64_t* nArr, int64_t* eArr) {
#ifdef TINOX_TLS
    TinoxArray* ma = (TinoxArray*)msgArr;
    TinoxArray* sa = (TinoxArray*)sigArr;
    TinoxArray* na = (TinoxArray*)nArr;
    TinoxArray* ea = (TinoxArray*)eArr;

    unsigned char* msg_buf = (unsigned char*)malloc(ma->len > 0 ? (size_t)ma->len : 1);
    for (int64_t i = 0; i < ma->len; i++) msg_buf[i] = (unsigned char)(ma->data[i] & 0xff);
    unsigned char* sig_buf = (unsigned char*)malloc(sa->len > 0 ? (size_t)sa->len : 1);
    for (int64_t i = 0; i < sa->len; i++) sig_buf[i] = (unsigned char)(sa->data[i] & 0xff);
    unsigned char* n_buf = (unsigned char*)malloc(na->len > 0 ? (size_t)na->len : 1);
    for (int64_t i = 0; i < na->len; i++) n_buf[i] = (unsigned char)(na->data[i] & 0xff);
    unsigned char* e_buf = (unsigned char*)malloc(ea->len > 0 ? (size_t)ea->len : 1);
    for (int64_t i = 0; i < ea->len; i++) e_buf[i] = (unsigned char)(ea->data[i] & 0xff);

    bool ok = false;
    BIGNUM* bn_n = BN_bin2bn(n_buf, (int)na->len, NULL);
    BIGNUM* bn_e = BN_bin2bn(e_buf, (int)ea->len, NULL);
    OSSL_PARAM_BLD* bld = NULL;
    OSSL_PARAM* params = NULL;
    EVP_PKEY_CTX* pctx = NULL;
    EVP_PKEY* pkey = NULL;
    EVP_MD_CTX* mctx = NULL;

    if (bn_n && bn_e) {
        bld = OSSL_PARAM_BLD_new();
        if (bld
            && OSSL_PARAM_BLD_push_BN(bld, OSSL_PKEY_PARAM_RSA_N, bn_n)
            && OSSL_PARAM_BLD_push_BN(bld, OSSL_PKEY_PARAM_RSA_E, bn_e)) {
            params = OSSL_PARAM_BLD_to_param(bld);
        }
    }

    if (params) {
        pctx = EVP_PKEY_CTX_new_from_name(NULL, "RSA", NULL);
        if (pctx && EVP_PKEY_fromdata_init(pctx) == 1
            && EVP_PKEY_fromdata(pctx, &pkey, EVP_PKEY_PUBLIC_KEY, params) == 1) {
            mctx = EVP_MD_CTX_new();
            if (mctx && EVP_DigestVerifyInit(mctx, NULL, EVP_sha256(), NULL, pkey) == 1) {
                ok = (EVP_DigestVerify(mctx, sig_buf, (size_t)sa->len, msg_buf, (size_t)ma->len) == 1);
            }
        }
    }

    if (mctx) EVP_MD_CTX_free(mctx);
    if (pkey) EVP_PKEY_free(pkey);
    if (pctx) EVP_PKEY_CTX_free(pctx);
    if (params) OSSL_PARAM_free(params);
    if (bld) OSSL_PARAM_BLD_free(bld);
    if (bn_n) BN_free(bn_n);
    if (bn_e) BN_free(bn_e);
    free(msg_buf); free(sig_buf); free(n_buf); free(e_buf);
    return ok;
#else
    (void)msgArr; (void)sigArr; (void)nArr; (void)eArr;
    fprintf(stderr, "rsaVerifySha256: runtime built without TLS/OpenSSL (TINOX_TLS=0)\n");
    return false;
#endif
}

// ---- HttpServer route-based API ----

#define TINOX_MAX_ROUTES 64

typedef void (*TinoxRouteHandler)(int64_t ctx);

typedef struct {
    char method[8];
    char path[256];
    TinoxRouteHandler handler;
    int64_t offload; // executor id of the @Blocking pool (#552), 0 = handled on the epoll worker
} TinoxRoute;

typedef struct {
    int64_t port;
    TinoxRoute routes[TINOX_MAX_ROUTES];
    int route_count;
    // NULL (the common case, every existing caller): binds 0.0.0.0/::, same
    // as always. Non-NULL: restricts to that address only -- used by the
    // dev-mode introspection server (tinox_HttpServer_new_bind) so it isn't
    // reachable off the local machine. Owned by this struct (strdup'd in
    // tinox_HttpServer_new_bind), never freed -- servers live for the
    // process lifetime, same as every other allocation in this file.
    char* bind_addr;
} TinoxHttpServer;

extern void* tinox_alloc(size_t size);
extern void* tinox_map_create(void);

int64_t* tinox_HttpServer_new(int64_t port) {
    TinoxHttpServer* srv = (TinoxHttpServer*)malloc(sizeof(TinoxHttpServer));
    memset(srv, 0, sizeof(TinoxHttpServer));
    srv->port = port;
    return (int64_t*)srv;
}

// Same as tinox_HttpServer_new, but the listening socket(s) only bind
// `bind_addr` (e.g. "127.0.0.1") instead of every interface. Compiler-
// generated only (the dev-mode introspection API); not part of the
// tinox.core.http_server surface user code imports.
int64_t* tinox_HttpServer_new_bind(int64_t port, const char* bind_addr) {
    TinoxHttpServer* srv = (TinoxHttpServer*)malloc(sizeof(TinoxHttpServer));
    memset(srv, 0, sizeof(TinoxHttpServer));
    srv->port = port;
    srv->bind_addr = bind_addr ? strdup(bind_addr) : NULL;
    return (int64_t*)srv;
}

static void http_server_add_route(int64_t* server, const char* method, char* path, int64_t handler) {
    TinoxHttpServer* srv = (TinoxHttpServer*)server;
    if (srv->route_count < TINOX_MAX_ROUTES) {
        strncpy(srv->routes[srv->route_count].method, method, 7);
        strncpy(srv->routes[srv->route_count].path, path, 255);
        srv->routes[srv->route_count].handler = (TinoxRouteHandler)(intptr_t)handler;
        srv->route_count++;
    }
}

// Gives the route registered last its own bounded handler pool (@Blocking(workers, queue), #552).
void tinox_HttpServer_offload_last(int64_t* server, int64_t workers, int64_t queue) {
    TinoxHttpServer* srv = (TinoxHttpServer*)server;
    if (srv->route_count > 0) srv->routes[srv->route_count - 1].offload = executorCreate(workers, queue);
}

void tinox_HttpServer_get(int64_t* server, char* path, int64_t handler) {
    http_server_add_route(server, "GET", path, handler);
}

void tinox_HttpServer_post(int64_t* server, char* path, int64_t handler) {
    http_server_add_route(server, "POST", path, handler);
}

void tinox_HttpServer_put(int64_t* server, char* path, int64_t handler) {
    http_server_add_route(server, "PUT", path, handler);
}

void tinox_HttpServer_patch(int64_t* server, char* path, int64_t handler) {
    http_server_add_route(server, "PATCH", path, handler);
}

void tinox_HttpServer_delete(int64_t* server, char* path, int64_t handler) {
    http_server_add_route(server, "DELETE", path, handler);
}

static const char* http_status_text(int64_t code) {
    switch (code) {
        case 200: return "OK";
        case 201: return "Created";
        case 204: return "No Content";
        case 400: return "Bad Request";
        case 401: return "Unauthorized";
        case 403: return "Forbidden";
        case 404: return "Not Found";
        case 405: return "Method Not Allowed";
        case 415: return "Unsupported Media Type";
        case 500: return "Internal Server Error";
        default:  return "OK";
    }
}

static int route_matches(const char* pattern, const char* path, void* params_map) {
    while (*pattern && *path) {
        if (*pattern == ':') {
            // Parse parameter name
            const char* pname = pattern + 1;
            const char* pend  = pname;
            while (*pend && *pend != '/') pend++;
            // Bug 176: this used to be `char pname_buf[64]` -- a stack
            // buffer reused on every loop iteration -- passed straight to
            // tinox_map_set(params_map, pname_buf, ...). params_map here is
            // always g_path_params_map (thread_local_init(), borrowed_keys=1),
            // and tinox_map_set only strdup()s the key when borrowed_keys==0;
            // with borrowed_keys==1 it stores the raw pointer as-is. So every
            // route with N>1 `:param`s ended up with every entry's key
            // pointing at the SAME reused stack slot, left holding whatever
            // the *last* parameter's name happened to be by the time a
            // handler actually read the map -- every earlier parameter's
            // getParam() lookup then silently failed (key no longer matched)
            // and returned "". Heap-allocating the name buffer here, exactly
            // like `val` right below already does, gives each parameter name
            // its own storage that outlives this loop iteration.
            size_t nlen = (size_t)(pend - pname);
            char* pname_buf = (char*)malloc(nlen + 1);
            memcpy(pname_buf, pname, nlen);
            pname_buf[nlen] = '\0';
            // Extract value from path
            const char* vend = path;
            while (*vend && *vend != '/') vend++;
            size_t vlen = (size_t)(vend - path);
            char* val = (char*)malloc(vlen + 1);
            memcpy(val, path, vlen);
            val[vlen] = '\0';
            if (params_map) tinox_map_set(params_map, pname_buf, (int64_t)(uintptr_t)val);
            pattern = pend;
            path    = vend;
        } else if (*pattern == *path) {
            pattern++; path++;
        } else {
            return 0;
        }
    }
    // Skip trailing slash on pattern
    while (*pattern == '/') pattern++;
    while (*path   == '/') path++;
    return *pattern == '\0' && *path == '\0';
}

// Thread-local response buffer — reused across requests per thread
static __thread char*  g_resp_buf = NULL;
static __thread size_t g_resp_cap = 0;

// Thread-local per-request structs — reused each request, one set per thread
static __thread int64_t  g_response[3];
static __thread int64_t  g_request[6];
static __thread int64_t  g_ctx[2];
static __thread char     g_empty_body[1];
static __thread TinoxMap g_req_headers_map;
static __thread TinoxMap g_resp_headers_map;
static __thread TinoxMap g_path_params_map;
static __thread int      g_thread_inited = 0;

static TinoxMap* make_static_map(TinoxMap* m, size_t cap) {
    m->entries      = (TinoxMapEntry*)calloc(cap, sizeof(TinoxMapEntry));
    m->cap          = cap;
    m->len          = 0;
    m->tombs        = 0;
    m->borrowed_keys = 1;
    TINOX_CK_REG(m, TINOX_KIND_MAP);
    return m;
}

static void thread_local_init(void) {
    tinox_gc_register_thread_roots(); // Bug 140 -- see definition near main()
    if (g_thread_inited) return;
    g_resp_cap = 4096; g_resp_buf = (char*)malloc(g_resp_cap);
    make_static_map(&g_req_headers_map,  16);
    make_static_map(&g_resp_headers_map, 8);
    make_static_map(&g_path_params_map,  8);
    g_empty_body[0] = '\0';
    g_thread_inited = 1;
}

// Normalize HTTP header key to canonical Title-Case-Hyphenated form in place.
// e.g. "content-type" → "Content-Type", "ACCEPT" → "Accept"
static void normalize_header_key(char* k) {
    int cap_next = 1;
    for (; *k; k++) {
        if (*k == '-') { cap_next = 1; }
        else if (cap_next) { *k = (char)toupper((unsigned char)*k); cap_next = 0; }
        else { *k = (char)tolower((unsigned char)*k); }
    }
}

char* serviceStatusJson(void) {
    char buf[512];
    snprintf(buf, sizeof(buf), "{\"state\":\"%s\",\"ready\":%s,\"live\":%s,\"startedMs\":%lld,\"readyMs\":%lld,\"shutdownMs\":%lld}",
        serviceState(), serviceIsReady() ? "true" : "false", serviceIsLive() ? "true" : "false",
        (long long)serviceStartedMs(), (long long)serviceReadyMs(), (long long)serviceShutdownMs());
    return GC_strdup(buf);
}
void tinox_service_health_response(int64_t context, int64_t readiness) {
    int64_t* response = (int64_t*)(intptr_t)((int64_t*)(intptr_t)context)[1];
    response[0] = (readiness ? serviceIsReady() : serviceIsLive()) ? 200 : 503;
    response[2] = (int64_t)(intptr_t)serviceStatusJson();
    tinox_map_set((void*)(intptr_t)response[1], "Content-Type", (int64_t)(intptr_t)"application/json");
}
void tinox_service_unavailable(int64_t context) {
    int64_t* response = (int64_t*)(intptr_t)((int64_t*)(intptr_t)context)[1];
    response[0] = 503; response[2] = (int64_t)(intptr_t)"Service draining";
}
// ---- Structured HTTP error mapping (#524) ----
// `@ErrorMap("CODE", status)` entries arrive as the compiler-emitted table "CODE=404;OTHER=409"
// (strong definition of this weak symbol; absent when the program declares none). A handler that
// throws "CODE: message" with a registered CODE becomes `status` + the envelope
//   {"error":{"code":"CODE","message":"message","correlationId":"..."}}
// Anything else (plain strings, unregistered codes) is a 500 INTERNAL with a fixed message:
// internal exception text never reaches the client, it goes to stderr with the correlation id.
extern const char tinox_error_table_data[] __attribute__((weak));

static int tinox_error_status_for(const char* code, size_t code_len) {
    if (!tinox_error_table_data) return 0;
    const char* p = tinox_error_table_data;
    while (*p) {
        const char* eq = strchr(p, '=');
        if (!eq) break;
        const char* end = strchr(eq, ';');
        size_t name_len = (size_t)(eq - p);
        if (name_len == code_len && strncmp(p, code, code_len) == 0) return atoi(eq + 1);
        if (!end) break;
        p = end + 1;
    }
    return 0;
}

static int tinox_error_code_len(const char* text) {
    if (!text || !(text[0] >= 'A' && text[0] <= 'Z')) return 0;
    size_t n = 0;
    while ((text[n] >= 'A' && text[n] <= 'Z') || (text[n] >= '0' && text[n] <= '9') || text[n] == '_') n++;
    return (text[n] == ':' && n > 0) ? (int)n : 0;
}

// Typed errors (#524): a thrown `@Error` value travels as the string "CODE: message".
// `codes` is a ';'-separated list (the caught class plus all its subclasses).
// Returns 1 when the thrown text carries one of those codes.
int64_t tinox_error_matches(const char* thrown, const char* codes) {
    int clen = tinox_error_code_len(thrown);
    if (clen <= 0 || !codes) return 0;
    const char* p = codes;
    while (*p) {
        const char* end = strchr(p, ';');
        size_t n = end ? (size_t)(end - p) : strlen(p);
        if (n == (size_t)clen && memcmp(p, thrown, n) == 0) return 1;
        if (!end) break;
        p = end + 1;
    }
    return 0;
}

// The message part of a thrown "CODE: message" (the whole text if it has no code).
char* tinox_error_message(const char* thrown) {
    if (!thrown) return GC_strdup("");
    int clen = tinox_error_code_len(thrown);
    const char* m = clen > 0 ? thrown + clen + 1 : thrown;
    while (clen > 0 && *m == ' ') m++;
    return GC_strdup(m);
}

// Writes the standard error envelope with an explicit status (shared by handler errors and the
// browser-security guards).
static void tinox_http_error_envelope(int64_t context, int status, const char* code, const char* message) {
    int64_t* ctx = (int64_t*)(intptr_t)context;
    int64_t* response = (int64_t*)(intptr_t)ctx[1];
    int64_t* request = (int64_t*)(intptr_t)ctx[0];
    char correlation[64];
    const char* incoming = request && request[3] ? (const char*)(uintptr_t)tinox_map_get((void*)(intptr_t)request[3], "X-Request-Id") : NULL;
    if (incoming && *incoming && strlen(incoming) <= 64 && !strpbrk(incoming, "\r\n\"\\")) {
        snprintf(correlation, sizeof(correlation), "%s", incoming);
    } else {
        snprintf(correlation, sizeof(correlation), "req-%08x%08x", (unsigned)random(), (unsigned)random());
    }
    if (status == 500) fprintf(stderr, "tinox: request handler error [%s]: %s\n", correlation, message ? message : "");
    if (status == 500) message = "Internal error";
    char* encoded = tinox_json_encode_string(message); // includes the surrounding quotes
    size_t cap = strlen(encoded) + strlen(code) + strlen(correlation) + 96;
    char* body = (char*)GC_malloc(cap);
    snprintf(body, cap, "{\"error\":{\"code\":\"%s\",\"message\":%s,\"correlationId\":\"%s\"}}", code, encoded, correlation);
    response[0] = status;
    response[2] = (int64_t)(intptr_t)body;
    void* headers = (void*)(intptr_t)response[1];
    if (headers) {
        tinox_map_set(headers, "Content-Type", (int64_t)(intptr_t)"application/json; charset=utf-8");
        tinox_map_set(headers, "X-Request-Id", (int64_t)(intptr_t)GC_strdup(correlation));
    }
}

static void tinox_http_error_response(int64_t context, const char* err) {
    const char* code = "INTERNAL";
    const char* message = err ? err : "";
    int status = 500;
    char code_buf[64];
    int clen = tinox_error_code_len(err);
    if (clen > 0 && clen < (int)sizeof(code_buf)) {
        int mapped = tinox_error_status_for(err, (size_t)clen);
        if (mapped >= 400 && mapped <= 599) {
            memcpy(code_buf, err, (size_t)clen);
            code_buf[clen] = '\0';
            code = code_buf;
            status = mapped;
            message = err + clen + 1;
            while (*message == ' ') message++;
        }
    }
    tinox_http_error_envelope(context, status, code, message);
}

// Unmanaged (no @Service) request boundary: an uncaught handler exception is a
// failed request, never a 200 with a default value, and must not stay pending.
void tinox_request_failed(int64_t context) {
    // Cancellation keeps propagating to the owning task instead of becoming a 500.
    if (__tinox_err && !task_is_cancellation(__tinox_err)) {
        tinox_http_error_response(context, (const char*)(intptr_t)__tinox_err);
        __tinox_err = 0;
    }
}
void tinox_service_request_done(int64_t context) {
    if (__tinox_err) {
        tinox_http_error_response(context, (const char*)(intptr_t)__tinox_err);
        __tinox_err = 0;
    }
    tinox_service_work_exit();
}

// ---- Browser security: @Cors / @Csrf (#529) ------------------------------------------------------
// Program-wide policy compiled into `tinox_security_policy_data` (lines "key<TAB>value"). Enforced
// at the HTTP/1 request boundary before routing, so a preflight (which has no route) and every
// guarded request follow the same order: origin check -> CORS headers -> CSRF check -> handler.
// Other transports (HTTP/2 in Tinox code, HTTP/3) call tinox_security_pre from their route shims
// where available; see docs/browser-security.md for what each transport covers.
extern const char tinox_security_policy_data[] __attribute__((weak));

typedef struct {
    int      loaded, cors, csrf, credentials;
    int64_t  max_age;
    int      n_origins, n_trusted;
    char*    origins[32];
    char*    trusted[32];
    char*    headers;   // comma separated allow-list
} TinoxSecurityPolicy;
static TinoxSecurityPolicy g_sec;
static pthread_mutex_t g_sec_mu = PTHREAD_MUTEX_INITIALIZER;

static const TinoxSecurityPolicy* tinox_security_policy(void) {
    if (g_sec.loaded) return &g_sec;
    pthread_mutex_lock(&g_sec_mu);
    if (!g_sec.loaded) {
        if (tinox_security_policy_data) {
            char* text = strdup(tinox_security_policy_data);
            char* save = NULL;
            for (char* line = strtok_r(text, "\n", &save); line; line = strtok_r(NULL, "\n", &save)) {
                char* tab = strchr(line, '\t');
                if (!tab) continue;
                *tab = '\0';
                const char* v = tab + 1;
                if (!strcmp(line, "cors-origin") && g_sec.n_origins < 32) { g_sec.cors = 1; g_sec.origins[g_sec.n_origins++] = strdup(v); }
                else if (!strcmp(line, "cors-credentials")) { g_sec.cors = 1; g_sec.credentials = atoi(v); }
                else if (!strcmp(line, "cors-maxage")) g_sec.max_age = atoll(v);
                else if (!strcmp(line, "cors-headers")) g_sec.headers = strdup(v);
                else if (!strcmp(line, "csrf")) g_sec.csrf = 1;
                else if (!strcmp(line, "csrf-origin") && g_sec.n_trusted < 32) g_sec.trusted[g_sec.n_trusted++] = strdup(v);
            }
            free(text);
        }
        __sync_synchronize();
        g_sec.loaded = 1;
    }
    pthread_mutex_unlock(&g_sec_mu);
    return &g_sec;
}

static int sec_origin_in(char* const* list, int n, const char* origin) {
    for (int i = 0; i < n; i++) if (!strcmp(list[i], "*") || !strcmp(list[i], origin)) return 1;
    return 0;
}

// Same-origin = the Origin's authority equals the Host header (scheme is not visible here).
static int sec_same_origin(const char* origin, const char* host) {
    if (!host || !*host) return 0;
    const char* auth = strstr(origin, "://");
    return auth && !strcmp(auth + 3, host);
}

// Origin of a Referer URL ("scheme://authority") into out; 0 when unparsable.
static int sec_referer_origin(const char* referer, char* out, size_t cap) {
    const char* auth = strstr(referer, "://");
    if (!auth) return 0;
    const char* end = auth + 3;
    while (*end && *end != '/' && *end != '?' && *end != '#') end++;
    size_t n = (size_t)(end - referer);
    if (n == 0 || n >= cap) return 0;
    memcpy(out, referer, n);
    out[n] = '\0';
    return 1;
}

static int sec_header_listed(const char* allowed_csv, const char* name, size_t len) {
    const char* p = allowed_csv;
    while (p && *p) {
        const char* e = strchr(p, ',');
        size_t n = e ? (size_t)(e - p) : strlen(p);
        if (n == len && strncasecmp(p, name, len) == 0) return 1;
        if (!e) break;
        p = e + 1;
    }
    return 0;
}

static const char* sec_req_header(void* hdrs, const char* name) {
    const char* v = (const char*)(uintptr_t)tinox_map_get(hdrs, name);
    if (v) return v;
    char lower[64];
    size_t n = strlen(name);
    if (n >= sizeof(lower)) return NULL;
    for (size_t i = 0; i < n; i++) lower[i] = (char)tolower((unsigned char)name[i]);
    lower[n] = '\0';
    return (const char*)(uintptr_t)tinox_map_get(hdrs, lower);
}

static void sec_reject(int64_t context, int status, const char* code, const char* message) {
    tinox_http_error_envelope(context, status, code, message);
}

// Returns 1 when the guard already produced the response (rejection or preflight answer).
// route_exists: 1 if a route is registered for the method a preflight asks about.
static int tinox_security_pre(int64_t context, int route_exists) {
    const TinoxSecurityPolicy* pol = tinox_security_policy();
    if (!pol->cors && !pol->csrf) return 0;
    int64_t* ctx = (int64_t*)(intptr_t)context;
    int64_t* request = (int64_t*)(intptr_t)ctx[0];
    int64_t* response = (int64_t*)(intptr_t)ctx[1];
    const char* method = (const char*)(intptr_t)request[0];
    void* hdrs = (void*)(intptr_t)request[3];
    void* rhdrs = (void*)(intptr_t)response[1];
// HTTP/1 normalises header names to Title-Case; HTTP/2 and HTTP/3 keep them lowercase.
#define REQ_HDR(name) sec_req_header(hdrs, name)
    const char* origin = REQ_HDR("Origin");
    const char* host = REQ_HDR("Host");
    int unsafe = strcmp(method, "GET") && strcmp(method, "HEAD") && strcmp(method, "OPTIONS");
    int same_origin = origin && sec_same_origin(origin, host);

    if (origin && !same_origin) {
        int allowed = pol->cors && strcmp(origin, "null") != 0 && sec_origin_in(pol->origins, pol->n_origins, origin);
        // Origins trusted only for CSRF (form posts from a sibling site) get no CORS headers but are not refused.
        if (pol->cors && !allowed && !(pol->csrf && sec_origin_in(pol->trusted, pol->n_trusted, origin))) {
            sec_reject(context, 403, "CORS_ORIGIN_DENIED", "Origin is not allowed");
            return 1;
        }
        if (allowed) {
            const char* want_method = REQ_HDR("Access-Control-Request-Method");
            int preflight = !strcmp(method, "OPTIONS") && want_method;
            const char* want_headers = preflight ? REQ_HDR("Access-Control-Request-Headers") : NULL;
            // Refusals first: a rejected preflight must not carry any allow-* header.
            if (preflight) {
                if (!route_exists) { sec_reject(context, 403, "CORS_METHOD_DENIED", "No route accepts the requested method"); return 1; }
                const char* p = want_headers ? want_headers : "";
                while (*p) {
                    while (*p == ' ' || *p == ',') p++;
                    const char* e = p;
                    while (*e && *e != ',' && *e != ' ') e++;
                    if (e > p && !sec_header_listed(pol->headers, p, (size_t)(e - p))) {
                        sec_reject(context, 403, "CORS_HEADER_DENIED", "A requested header is not allowed");
                        return 1;
                    }
                    p = e;
                }
            }
            int wildcard = sec_origin_in(pol->origins, pol->n_origins, "*") && !sec_origin_in(pol->origins, pol->n_origins, origin);
            tinox_map_set(rhdrs, "Access-Control-Allow-Origin", (int64_t)(uintptr_t)(wildcard ? "*" : GC_strdup(origin)));
            tinox_map_set(rhdrs, "Vary", (int64_t)(uintptr_t)"Origin");
            if (pol->credentials) tinox_map_set(rhdrs, "Access-Control-Allow-Credentials", (int64_t)(uintptr_t)"true");
            if (preflight) {
                if (want_headers && *want_headers) tinox_map_set(rhdrs, "Access-Control-Allow-Headers", (int64_t)(uintptr_t)GC_strdup(want_headers));
                tinox_map_set(rhdrs, "Access-Control-Allow-Methods", (int64_t)(uintptr_t)GC_strdup(want_method));
                char age[24];
                snprintf(age, sizeof(age), "%lld", (long long)pol->max_age);
                tinox_map_set(rhdrs, "Access-Control-Max-Age", (int64_t)(uintptr_t)GC_strdup(age));
                response[0] = 204;
                return 1;
            }
        }
    }

    if (pol->csrf && unsafe) {
        const char* cookie = REQ_HDR("Cookie");
        if (cookie && *cookie) {
            const char* fetch_site = REQ_HDR("Sec-Fetch-Site");
            char ref_origin[256];
            const char* effective = origin;
            if (!effective) {
                const char* ref = REQ_HDR("Referer");
                if (ref && sec_referer_origin(ref, ref_origin, sizeof(ref_origin))) effective = ref_origin;
            }
            if (!effective) {
                sec_reject(context, 403, "CSRF_REJECTED", "A request with cookies needs an Origin or Referer header");
                return 1;
            }
            int trusted = sec_same_origin(effective, host) || sec_origin_in(pol->trusted, pol->n_trusted, effective) ||
                          (pol->cors && sec_origin_in(pol->origins, pol->n_origins, effective) && strcmp(effective, "null") != 0);
            if (!trusted || (fetch_site && !strcmp(fetch_site, "cross-site") && !sec_origin_in(pol->trusted, pol->n_trusted, effective) &&
                             !(pol->cors && sec_origin_in(pol->origins, pol->n_origins, effective)))) {
                sec_reject(context, 403, "CSRF_REJECTED", "Cross-site request rejected");
                return 1;
            }
        }
    }
#undef REQ_HDR
    return 0;
}

// Entry for transports that do not go through tinox_handle_one (HTTP/2 in Tinox code, HTTP/3 route
// shims). 1 = the guard already produced the response, the handler must not run.
int64_t tinoxSecurityGuard(int64_t context, int64_t route_exists) {
    return tinox_security_pre(context, (int)route_exists);
}

// Policy summary for introspection (Dev UI /security).
char* tinox_security_json(void) {
    const TinoxSecurityPolicy* pol = tinox_security_policy();
    char buf[8192];
    int n = snprintf(buf, sizeof(buf), "{\"cors\":%s,\"csrf\":%s,\"credentials\":%s,\"maxAge\":%lld,\"origins\":[",
        pol->cors ? "true" : "false", pol->csrf ? "true" : "false", pol->credentials ? "true" : "false", (long long)pol->max_age);
    for (int i = 0; i < pol->n_origins && n < 6000; i++) n += snprintf(buf + n, sizeof(buf) - (size_t)n, "%s\"%s\"", i ? "," : "", pol->origins[i]);
    n += snprintf(buf + n, sizeof(buf) - (size_t)n, "],\"trustedOrigins\":[");
    for (int i = 0; i < pol->n_trusted && n < 7000; i++) n += snprintf(buf + n, sizeof(buf) - (size_t)n, "%s\"%s\"", i ? "," : "", pol->trusted[i]);
    snprintf(buf + n, sizeof(buf) - (size_t)n, "],\"allowedHeaders\":\"%s\"}", pol->headers ? pol->headers : "");
    return GC_strdup(buf);
}

// @Blocking routes (#552): the epoll worker that read the request hands the connection to the route's own bounded
// pool and goes back to its other connections. The pool thread parses and answers the request exactly like an epoll
// worker would (all per-request state is thread-local), then closes the connection: an offloaded response is always
// `Connection: close`, so the epoll worker never has to take a connection back.
typedef struct {
    TinoxHttpServer* srv;
    int fd;
    char* raw;
    size_t len;
} TinoxOffloadJob;

static void tinox_process_request(TinoxHttpServer* srv, int64_t client_fd, char* raw_req, int can_offload, int force_close,
                                  int* keep_alive_out, int* offloaded_out);

static void* tinox_offload_run(void* arg) {
    TinoxOffloadJob* job = arg;
    thread_local_init();
    if (!g_recv_buf || g_recv_cap < job->len + 1) {
        free(g_recv_buf);
        g_recv_cap = job->len + 4096;
        g_recv_buf = (char*)malloc(g_recv_cap);
    }
    memcpy(g_recv_buf, job->raw, job->len + 1);
    g_recv_used = job->len;
    int keep_alive = 0, offloaded = 0;
    tinox_process_request(job->srv, job->fd, g_recv_buf, 0, 1, &keep_alive, &offloaded);
    close(job->fd);
    atomic_fetch_sub(&g_open_conns, 1);
    service_work_release_token();
    free(job->raw);
    free(job);
    return NULL;
}

static void tinox_handle_one(TinoxHttpServer* srv, int64_t client_fd, int* keep_alive_out, int* offloaded_out) {
    *keep_alive_out = 0;
    *offloaded_out = 0;
    char* raw_req = httpServerReadRequest(client_fd);
    if (!raw_req || !raw_req[0]) return;
    tinox_process_request(srv, client_fd, raw_req, 1, 0, keep_alive_out, offloaded_out);
}

static void tinox_process_request(TinoxHttpServer* srv, int64_t client_fd, char* raw_req, int can_offload, int force_close,
                                  int* keep_alive_out, int* offloaded_out) {
    *keep_alive_out = 0;
    *offloaded_out = 0;
    char method[8];
    char path[256];
    char* query = (char*)"";
    {
        const char* sp = strchr(raw_req, ' ');
        if (sp) {
            int mlen = (int)(sp - raw_req); if (mlen > 7) mlen = 7;
            memcpy(method, raw_req, mlen); method[mlen] = '\0';
            sp++;
            const char* ep = sp;
            while (*ep && *ep != ' ' && *ep != '\r' && *ep != '\n' && (ep - sp) < 255) ep++;
            int plen = (int)(ep - sp);
            memcpy(path, sp, plen); path[plen] = '\0';
        } else {
            method[0] = '\0'; path[0] = '\0';
        }
    }
    char* qmark = strchr(path, '?');
    if (qmark) { query = qmark + 1; *qmark = '\0'; }

    tinox_map_reset(&g_path_params_map);
    TinoxRouteHandler handler = NULL;
    int64_t route_offload = 0;
    for (int i = 0; i < srv->route_count; i++) {
        if (strcmp(srv->routes[i].method, method) != 0) continue;
        if (route_matches(srv->routes[i].path, path, &g_path_params_map)) {
            handler = srv->routes[i].handler;
            route_offload = srv->routes[i].offload;
            break;
        }
        tinox_map_reset(&g_path_params_map);
    }

    // The header pass below rewrites the buffer in place, so an offloaded request is copied before it.
    if (can_offload && route_offload && service_work_take_token()) {
        size_t raw_len = (raw_req == g_recv_buf) ? g_recv_used : strlen(raw_req);
        TinoxOffloadJob* job = (TinoxOffloadJob*)malloc(sizeof(*job));
        job->srv = srv;
        job->fd = (int)client_fd;
        job->len = raw_len;
        job->raw = (char*)malloc(raw_len + 1);
        memcpy(job->raw, raw_req, raw_len);
        job->raw[raw_len] = '\0';
        if (executor_submit_raw(route_offload, tinox_offload_run, job)) {
            atomic_fetch_add(&g_offload_accepted, 1);
            *offloaded_out = 1;
            return;
        }
        // Queue full (or pool shut down): say so at once instead of queueing without bound.
        service_work_release_token();
        free(job->raw);
        free(job);
        atomic_fetch_add(&g_offload_rejected, 1);
        static const char body[] = "{\"error\":\"overloaded: the blocking route's queue is full\"}";
        char reply[256];
        int n = snprintf(reply, sizeof reply,
            "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nRetry-After: 1\r\n"
            "Connection: close\r\nContent-Length: %zu\r\n\r\n%s", sizeof(body) - 1, body);
        (void)!send((int)client_fd, reply, (size_t)n, MSG_NOSIGNAL | MSG_DONTWAIT);
        return;
    }

    // Parse HTTP headers — normalize keys to Title-Case, track Connection header
    tinox_map_reset(&g_req_headers_map);
    int trace_headers = 0, request_headers = 0;
    int req_close = force_close; // HTTP/1.1 default: keep-alive; offloaded requests always close
    char* hdr_line = strchr(raw_req, '\n');
    while (hdr_line) {
        hdr_line++;
        if (*hdr_line == '\r' || *hdr_line == '\n' || *hdr_line == '\0') break;
        char* colon = strchr(hdr_line, ':');
        char* eol = strchr(hdr_line, '\n');
        if (colon && eol && colon < eol) {
            *colon = '\0';
            char* hkey = hdr_line;
            normalize_header_key(hkey);
            char* vstart = colon + 1;
            while (*vstart == ' ') vstart++;
            size_t vlen = (size_t)(eol - vstart);
            while (vlen > 0 && (vstart[vlen-1] == '\r' || vstart[vlen-1] == ' ')) vlen--;
            vstart[vlen] = '\0';
            if (strcmp(hkey, "Connection") == 0 && strcmp(vstart, "close") == 0) req_close = 1;
            if (telemetry_enabled) {
                if (!strcmp(hkey, "Traceparent")) trace_headers++;
                if (!strcmp(hkey, "X-Request-Id")) request_headers++;
            }
            tinox_map_set_borrow(&g_req_headers_map, hkey, (int64_t)vstart);
        }
        hdr_line = eol;
    }

    char* req_body = "";
    if (hdr_line) {
        if (*hdr_line == '\r') req_body = hdr_line + 2;
        else if (*hdr_line == '\n') req_body = hdr_line + 1;
    }
    g_req_body_ptr = req_body;
    g_req_body_len = 0;
    if (raw_req == g_recv_buf && req_body > raw_req && req_body <= raw_req + g_recv_used) {
        g_req_body_len = (int64_t)(g_recv_used - (size_t)(req_body - raw_req));
    } else {
        g_req_body_len = (int64_t)strlen(req_body);
    }
    g_resp_bytes_ptr = NULL;
    g_resp_bytes_len = 0;

    tinox_map_reset(&g_resp_headers_map);
    g_response[0] = handler ? 200 : 404;
    g_response[1] = (int64_t)&g_resp_headers_map;
    g_response[2] = (int64_t)g_empty_body;

    g_request[0] = (int64_t)method;
    g_request[1] = (int64_t)path;
    g_request[2] = (int64_t)req_body;
    g_request[3] = (int64_t)&g_req_headers_map;
    g_request[4] = (int64_t)query;
    g_request[5] = (int64_t)&g_path_params_map;

    g_ctx[0] = (int64_t)g_request;
    g_ctx[1] = (int64_t)g_response;

    int64_t span_token = 0;
    if (telemetry_enabled) {
        const char* incoming_parent = (const char*)(uintptr_t)tinox_map_get(&g_req_headers_map, "Traceparent");
        const char* incoming_request = (const char*)(uintptr_t)tinox_map_get(&g_req_headers_map, "X-Request-Id");
        span_token = telemetry_enter("http.server", trace_headers == 1 && incoming_parent ? incoming_parent : "",
                                    request_headers == 1 && incoming_request ? incoming_request : "", 2);
        if (span_token) {
            telemetry_event("request.start");
            tinox_map_set(&g_resp_headers_map, "X-Request-Id", (int64_t)(uintptr_t)telemetryRequestId());
        }
    }
    int service_admitted = 0;
    if (service_enabled() && strcmp(method, "GET") == 0 &&
        (!strcmp(path, "/health/ready") || !strcmp(path, "/health/live"))) {
        tinox_service_health_response((int64_t)g_ctx, !strcmp(path, "/health/ready"));
    } else if (!(service_admitted = (int)tinox_service_work_enter())) {
        tinox_service_unavailable((int64_t)g_ctx);
        req_close = 1;
    } else {
        int route_exists = 0;
        {
            const char* want = (const char*)(uintptr_t)tinox_map_get(&g_req_headers_map, "Access-Control-Request-Method");
            if (want && !strcmp(method, "OPTIONS")) {
                for (int i = 0; i < srv->route_count; i++) {
                    if (strcmp(srv->routes[i].method, want) != 0) continue;
                    if (route_matches(srv->routes[i].path, path, &g_path_params_map)) { route_exists = 1; break; }
                    tinox_map_reset(&g_path_params_map);
                }
            }
        }
        if (tinox_security_pre((int64_t)g_ctx, route_exists)) {
            // answered by the browser-security guard
        } else if (handler) handler((int64_t)g_ctx);
        if (__tinox_err && (service_enabled() || !task_is_cancellation(__tinox_err))) {
            tinox_http_error_response((int64_t)g_ctx, (const char*)(intptr_t)__tinox_err);
            __tinox_err = 0;
        }
    }
    if (span_token) {
        while (telemetry_depth > span_token) telemetryEnd(telemetry_depth);
        telemetry_event("request.end");
    }
    telemetry_leave(span_token);

    char* body = (char*)g_response[2];
    if (!body) body = "";
    int64_t status = g_response[0];
    void* resp_hdr_map = (void*)g_response[1];
    char hdr_buf[4096];
    size_t hdr_off = 0;
    int bad_header = 0;
    TinoxMap* rhm = (TinoxMap*)resp_hdr_map;
    if (rhm && rhm->len > 0) {
        int64_t* hkeys_h = tinox_map_keys(resp_hdr_map);
        int64_t* hkeys = ((TinoxArray*)hkeys_h)->data;
        int64_t hklen = ((TinoxArray*)hkeys_h)->len;
        for (int64_t hi = 0; hi < hklen; hi++) {
            const char* hk = (const char*)(uintptr_t)hkeys[hi];
            const char* hv = (const char*)(uintptr_t)tinox_map_get(resp_hdr_map, hk);
            if (hk && hv) {
                // Repeated fields (#528): HttpResponse.addHeader stores the 2nd..nth value under
                // "<Name> <n>"; a space cannot occur in a real header name, so the wire name is the
                // part before the first space.
                const char* sp = strchr(hk, ' ');
                size_t kl = sp ? (size_t)(sp - hk) : strlen(hk), vl = strlen(hv);
                // Framing safety: CR, LF or NUL in a name/value would inject headers or split the response.
                if (memchr(hk, '\r', kl) || memchr(hk, '\n', kl) || strpbrk(hv, "\r\n")) {
                    bad_header = 1;
                    continue;
                }
                if (hdr_off + kl + vl + 4 < sizeof(hdr_buf)) {
                    memcpy(hdr_buf + hdr_off, hk, kl); hdr_off += kl;
                    hdr_buf[hdr_off++] = ':'; hdr_buf[hdr_off++] = ' ';
                    memcpy(hdr_buf + hdr_off, hv, vl); hdr_off += vl;
                    hdr_buf[hdr_off++] = '\r'; hdr_buf[hdr_off++] = '\n';
                }
            }
        }
    }
    if (bad_header) {
        // Refuse to emit a response with an invalid header instead of sending a corrupted one.
        fprintf(stderr, "tinox: response header contains CR/LF; replying 500\n");
        status = 500;
        body = "Invalid response header";
        hdr_off = 0;
    }
    if (status != 204 && (tinox_map_contains(resp_hdr_map, "Content-Type") == 0 || bad_header)) {
        static const char ct[] = "Content-Type: application/json\r\n";
        // Bug 95: this fallback copy was unconditional, unlike every other
        // write into hdr_buf above (each bounds-checked against
        // sizeof(hdr_buf)). A handler that sets enough response headers to
        // bring hdr_off close to 4096 while omitting Content-Type overflowed
        // the stack buffer here. Apply the same bounds check as the loop
        // above instead — if it doesn't fit, skip the fallback header
        // (matches the existing per-header behavior of silently dropping a
        // header that doesn't fit, rather than corrupting the stack).
        if (hdr_off + (sizeof(ct) - 1) < sizeof(hdr_buf)) {
            memcpy(hdr_buf + hdr_off, ct, sizeof(ct) - 1);
            hdr_off += sizeof(ct) - 1;
        }
    }
    // Connection header
    static const char conn_ka[]    = "Connection: keep-alive\r\n";
    static const char conn_close[] = "Connection: close\r\n";
    const char* conn_hdr     = req_close ? conn_close : conn_ka;
    size_t      conn_hdr_len = req_close ? (sizeof(conn_close) - 1) : (sizeof(conn_ka) - 1);

    size_t body_len = status == 204 ? 0
        : (g_resp_bytes_ptr && body == g_resp_bytes_ptr) ? (size_t)g_resp_bytes_len : strlen(body);
    g_resp_bytes_ptr = NULL;
    const char* status_text = http_status_text(status);
    size_t st_len = strlen(status_text);
    size_t resp_cap = 9 + 3 + 1 + st_len + 2 + hdr_off + 16 + 20 + 2 + conn_hdr_len + 2 + body_len + 1;
    if (resp_cap > g_resp_cap) {
        while (g_resp_cap < resp_cap) g_resp_cap *= 2;
        g_resp_buf = (char*)realloc(g_resp_buf, g_resp_cap);
    }
    char* http_resp = g_resp_buf;
    char* out = http_resp;
    memcpy(out, "HTTP/1.1 ", 9); out += 9;
    out[0] = '0' + (char)(status / 100);
    out[1] = '0' + (char)(status / 10 % 10);
    out[2] = '0' + (char)(status % 10);
    out[3] = ' '; out += 4;
    memcpy(out, status_text, st_len); out += st_len;
    out[0] = '\r'; out[1] = '\n'; out += 2;
    memcpy(out, hdr_buf, hdr_off); out += hdr_off;
    if (status != 204) {
        memcpy(out, "Content-Length: ", 16); out += 16;
        out += fast_i64_write((int64_t)body_len, out);
        out[0] = '\r'; out[1] = '\n'; out += 2;
    }
    memcpy(out, conn_hdr, conn_hdr_len); out += conn_hdr_len;
    out[0] = '\r'; out[1] = '\n'; out += 2;
    // RFC 9110 §9.3.2: the answer to HEAD carries the headers (incl. Content-Length) but never a body.
    if (strcmp(method, "HEAD") != 0) { memcpy(out, body, body_len); out += body_len; }
    size_t resp_total = (size_t)(out - http_resp);

    // Send with pre-computed length (no strlen). Bug 175: this loop used to
    // give up on ANY n <= 0, including errno == EINTR -- a blocking send()
    // can be interrupted by the GC's SIGPWR stop-the-world signal like any
    // other blocking syscall (see the Runtime Quirks note on conn_recv/
    // conn_send, the template this loop failed to follow), silently
    // truncating the HTTP response mid-write under GC pressure instead of
    // finishing it. Retry on EINTR instead of aborting, matching conn_send's
    // own discipline.
    size_t sent_bytes = 0;
    while (sent_bytes < resp_total) {
        ssize_t n = send((int)client_fd, http_resp + sent_bytes, resp_total - sent_bytes, MSG_NOSIGNAL);
        if (n < 0) {
            if (errno == EINTR) continue;
            break;
        }
        if (n == 0) break;
        sent_bytes += (size_t)n;
    }
    if (service_admitted) tinox_service_work_exit();
    *keep_alive_out = !req_close && !serviceIsStopping();
}

// Per-connection state for epoll-based multi-connection handler
#define EPOLL_KEEP_ALIVE_MS 500      // close genuinely idle keep-alive connections after 500ms
#define EPOLL_FIRST_REQUEST_GRACE_MS 5000 // see `served` below; matches the SO_RCVTIMEO
                                          // "zombie guard" in the accept path, so a
                                          // connection that truly never sends anything
                                          // is still bounded by the same 5s either way

typedef struct {
    int      fd;          // -1 = unused slot
    uint64_t last_ms;     // last activity timestamp (milliseconds)
    int      served;      // 0 until this connection's first request has completed --
                           // see the stale-scan below for why this needs to be tracked
                           // separately from a genuinely-idle keep-alive connection
} EpollConnSlot;

// Slot table indexed directly by fd (no modulo, so two live fds can never share a slot), grown on
// demand to the next power of two above the highest fd seen. Memory cost is 24 bytes per slot per
// worker thread: 1M open fds * 24 B = ~24 MB per worker.
static __thread EpollConnSlot* g_epoll_slots = NULL;
static __thread int           g_epoll_cap = 0;
static __thread int           g_epoll_nconns = 0;  // number of active client connections

static void epoll_slot_reserve(int fd) {
    if (fd < g_epoll_cap) return;
    int cap = g_epoll_cap ? g_epoll_cap : 1024;
    while (cap <= fd) cap *= 2;
    EpollConnSlot* grown = realloc(g_epoll_slots, (size_t)cap * sizeof(EpollConnSlot));
    if (!grown) { fprintf(stderr, "tinox: out of memory growing the connection table\n"); exit(1); }
    for (int i = g_epoll_cap; i < cap; i++) { grown[i].fd = -1; grown[i].last_ms = 0; grown[i].served = 0; }
    g_epoll_slots = grown;
    g_epoll_cap = cap;
}

static uint64_t epoll_now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC_COARSE, &ts);
    return (uint64_t)ts.tv_sec * 1000 + (uint64_t)(ts.tv_nsec / 1000000);
}

// Bug 175 (root cause): this used to call epoll_now_ms() itself instead of
// taking the caller's already-captured `now_ms`. tinox_handle_connections'
// main loop captures `now_ms` ONCE per iteration (right after epoll_wait
// returns) and reuses that single value for both the accept-handling section
// (which used to call this function) and the stale-connection scan later in
// the SAME iteration. CLOCK_MONOTONIC_COARSE never goes backward, but it can
// tick forward between two separate reads -- so a connection accepted mid-
// iteration could get a `last_ms` a few coarse-clock ticks LATER than the
// iteration's own `now_ms`. The stale-scan's `now_ms - last_ms` is unsigned;
// with last_ms > now_ms that subtraction underflows to roughly UINT64_MAX,
// which is >= any timeout threshold -- so a connection accepted this exact
// iteration could be force-closed by the SAME iteration's stale-scan pass,
// before it was ever read even once. Confirmed live: temporary instrumen-
// tation caught this exact underflow (age_ms values like 18446744073709551599,
// i.e. (uint64_t)-16) on every observed spurious close during a 300-way
// concurrent burst. Passing the loop's own `now_ms` through removes the
// possibility of two inconsistent clock reads within one iteration entirely.
static void epoll_slot_add(int fd, uint64_t now_ms) {
    epoll_slot_reserve(fd);
    int idx = fd;
    if (g_epoll_slots[idx].fd < 0) g_epoll_nconns++;
    else atomic_fetch_sub(&g_open_conns, 1); // slot collision: that fd is already counted
    g_epoll_slots[idx].fd = fd;
    g_epoll_slots[idx].last_ms = now_ms;
    g_epoll_slots[idx].served = 0;
}

static void epoll_slot_remove(int fd) {
    if (fd < 0 || fd >= g_epoll_cap) return;
    int idx = fd;
    if (g_epoll_slots[idx].fd >= 0) {
        g_epoll_nconns--;
        atomic_fetch_sub(&g_open_conns, 1);
    }
    g_epoll_slots[idx].fd = -1;
}

static void tinox_handle_connections(TinoxHttpServer* srv, int64_t server_fd) {
    thread_local_init();

    epoll_slot_reserve(1023);

    int epfd = epoll_create1(EPOLL_CLOEXEC);
    if (epfd < 0) { perror("epoll_create1"); return; }

    // Register server socket for incoming connections
    struct epoll_event ev;
    ev.events  = EPOLLIN;
    ev.data.fd = (int)server_fd;
    epoll_ctl(epfd, EPOLL_CTL_ADD, (int)server_fd, &ev);

    struct epoll_event events[64];

    while (!service_quiescing()) {
        // EINTR-Retry (Bug 68/140 discipline, s. conn_recv/conn_send): a
        // blocking epoll_wait can be interrupted by the GC's SIGPWR stop-
        // the-world signal like any other blocking syscall -- without a
        // retry, a spurious EINTR here was previously indistinguishable
        // from "no events fired", silently skipping this wakeup instead of
        // properly retrying it.
        int n;
        do { n = epoll_wait(epfd, events, 64, 50); } while (n < 0 && errno == EINTR); // 50ms timeout for stale-connection scan

        uint64_t now_ms = epoll_now_ms();

        for (int i = 0; i < n; i++) {
            int fd = events[i].data.fd;

            if (fd == (int)server_fd) {
                // Accept one connection per epoll event (level-triggered: fires again if more pending)
                struct sockaddr_in client = {0};
                socklen_t len = sizeof(client);
                int cfd = accept(fd, (struct sockaddr*)&client, &len);
                if (cfd >= 0 && !tinox_budget_acquire(&g_open_conns, &g_peak_conns, tinox_limits()->max_connections)) {
                    // Over the connection budget (#302): answer with a
                    // visible, distinguishable overload response instead of
                    // queueing without bound or dropping silently.
                    static const char overload[] =
                        "HTTP/1.1 503 Service Unavailable\r\n"
                        "Content-Type: application/json\r\n"
                        "Retry-After: 1\r\n"
                        "Connection: close\r\n"
                        "Content-Length: 51\r\n\r\n"
                        "{\"error\":\"overloaded: connection budget exhausted\"}";
                    atomic_fetch_add(&g_rejected_conns, 1);
                    (void)!send(cfd, overload, sizeof(overload) - 1, MSG_NOSIGNAL | MSG_DONTWAIT);
                    close(cfd);
                } else if (cfd >= 0) {
                    int one = 1;
                    setsockopt(cfd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof(one));
                    struct timeval tv = { .tv_sec = 5 }; // zombie guard
                    setsockopt(cfd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
                    struct epoll_event cev;
                    cev.events  = EPOLLIN;
                    cev.data.fd = cfd;
                    epoll_ctl(epfd, EPOLL_CTL_ADD, cfd, &cev);
                    epoll_slot_add(cfd, now_ms);
                }
            } else {
                // Handle one request on this client connection
                int keep_alive = 0, offloaded = 0;
                tinox_handle_one(srv, (int64_t)fd, &keep_alive, &offloaded);
                if (offloaded) {
                    // The pool thread owns the connection now (it closes it and releases the connection budget).
                    epoll_ctl(epfd, EPOLL_CTL_DEL, fd, NULL);
                    if (fd >= 0 && fd < g_epoll_cap && g_epoll_slots[fd].fd >= 0) {
                        g_epoll_slots[fd].fd = -1;
                        g_epoll_nconns--;
                    }
                } else if (keep_alive) {
                    int idx = fd;
                    g_epoll_slots[idx].last_ms = now_ms;
                    g_epoll_slots[idx].served = 1;
                } else {
                    epoll_ctl(epfd, EPOLL_CTL_DEL, fd, NULL);
                    epoll_slot_remove(fd);
                    close(fd);
                }
            }
        }

        // Scan for stale connections (only when we have active clients).
        //
        // Bug 175: a freshly-accepted connection's `last_ms` is set once, at
        // accept() time (epoll_slot_add), and only touched again once its
        // FIRST request has actually completed (the `served = 1` above). If
        // this worker thread is backlogged handling other connections'
        // events -- entirely possible under a concurrent burst, since one
        // thread's epoll instance can have many connections ready at once --
        // a connection that already has its request sitting in the kernel
        // receive buffer, just not yet read by tinox_handle_one, was
        // indistinguishable here from a genuinely idle keep-alive connection
        // waiting for its NEXT request. Both used the same tight
        // EPOLL_KEEP_ALIVE_MS (500ms) threshold, so a busy-but-healthy
        // connection could get force-closed by this scan before it was ever
        // serviced -- the client sees an abrupt close/reset (CURLE_RECV_ERROR)
        // for a request the server never even attempted to read. Give a
        // connection that hasn't been served yet the same longer grace period
        // as the SO_RCVTIMEO "zombie guard" already set on accept (5s) --
        // that guard already bounds a connection that truly never sends
        // anything, so this scan doesn't need a tighter deadline for the
        // not-yet-served case; only a genuinely idle, already-served
        // keep-alive connection uses the tight 500ms.
        if (g_epoll_nconns > 0) {
            for (int i = 0; i < g_epoll_cap; i++) {
                if (g_epoll_slots[i].fd < 0) continue;
                uint64_t timeout_ms = g_epoll_slots[i].served
                    ? EPOLL_KEEP_ALIVE_MS
                    : EPOLL_FIRST_REQUEST_GRACE_MS;
                if (now_ms - g_epoll_slots[i].last_ms >= timeout_ms) {
                    int fd = g_epoll_slots[i].fd;
                    epoll_ctl(epfd, EPOLL_CTL_DEL, fd, NULL);
                    epoll_slot_remove(fd);
                    close(fd);
                }
            }
        }
    }
    for (int i = 0; i < g_epoll_cap; i++) {
        if (g_epoll_slots[i].fd >= 0) {
            int fd = g_epoll_slots[i].fd;
            epoll_slot_remove(fd);
            close(fd);
        }
    }
    free(g_epoll_slots);
    g_epoll_slots = NULL;
    g_epoll_cap = 0;
    g_epoll_nconns = 0;
    close(epfd);
}

struct TinoxWorkerArgs { TinoxHttpServer* srv; int64_t port; const char* bind_addr; };

static void* tinox_worker_run(void* arg) {
    struct TinoxWorkerArgs* wa = (struct TinoxWorkerArgs*)arg;
    // Each worker creates its own SO_REUSEPORT socket for zero-contention accept()
    int64_t server_fd = httpServerCreateOn(wa->port, wa->bind_addr);
    if (server_fd >= 0) tinox_handle_connections(wa->srv, server_fd);
    return NULL;
}

// Managed HTTP workers own joinable threads and their own listener/clients.
struct ServiceHttpWorkerArgs {
    TinoxHttpServer* server;
    int64_t port;
    int service_id;
    atomic_int ready;
    atomic_bool failed;
};
static void* service_http_worker(void* raw) {
    struct ServiceHttpWorkerArgs* args = raw;
    service_task_id = args->service_id;
    int64_t fd = httpServerCreateOn(args->port, args->server->bind_addr);
    if (fd < 0) {
        atomic_store(&args->failed, true);
        service_record_error("HTTP worker listener bind failed", 0);
    }
    atomic_fetch_add(&args->ready, 1);
    if (fd >= 0) { tinox_handle_connections(args->server, fd); httpServerClose(fd); }
    if (task_frame_scope) { task_close_scope(task_frame_scope, 0, 1); if (__tinox_err) service_record_error("HTTP worker task failed", 1); }
    tinox_gc_unregister_thread_roots();
    return NULL;
}
static void service_http_listen(TinoxHttpServer* server) {
    int64_t fd = httpServerCreateOn(server->port, server->bind_addr);
    if (fd < 0) { service_record_error("HTTP listener bind failed", 0); return; }
    int64_t port = httpServerBoundPort(fd);
    int workers = (int)tinox_limits()->http_workers;
    // Same one-thread-per-CPU model, with bounded stack usage for thread IDs.
    pthread_t* threads = GC_malloc(sizeof(*threads) * (size_t)workers);
    struct ServiceHttpWorkerArgs args = { .server = server, .port = port, .service_id = service_task_id };
    atomic_init(&args.ready, 0); atomic_init(&args.failed, false);
    int created = 0;
    for (int i = 1; i < workers; i++) {
        int rc = pthread_create(&threads[created], NULL, service_http_worker, &args);
        if (rc) { service_record_error("HTTP worker creation failed", 0); break; }
        created++;
    }
    while (atomic_load(&args.ready) < created) {
        struct timespec delay = {0, 1000000}; nanosleep(&delay, NULL);
    }
    if (!atomic_load(&args.failed) && !serviceIsStopping()) tinox_service_listener_ready(port);
    tinox_handle_connections(server, fd);
    httpServerClose(fd);
    for (int i = 0; i < created; i++) task_pthread_check(pthread_join(threads[i], NULL));
}

void tinox_HttpServer_listen(int64_t* server) {
    signal(SIGPIPE, SIG_IGN); // writev/send to closed connection should not kill process
    TinoxHttpServer* srv = (TinoxHttpServer*)server;
    if (service_enabled()) { service_http_listen(srv); return; }
    int64_t port = srv->port;
    fprintf(stderr, "HttpServer listening on port %lld\n", (long long)port);

    // One epoll thread per effective CPU (affinity/cgroup-aware) unless
    // TINOX_HTTP_WORKERS overrides it; each handles many conns via epoll.
    int nthreads = (int)tinox_limits()->http_workers;

    // Bug found while adding the dev-mode introspection server (the first
    // caller to ever run a SECOND HttpServer::listen() in one process):
    // this used to be `static`, shared storage across every call to this
    // function. With only ever one HttpServer instance per process, that
    // was harmless; with two, the second call's worker_args overwrites the
    // first's srv/port while its still-running worker threads keep reading
    // it, silently serving the wrong server's routes on the wrong port.
    // Plain stack storage is safe here instead -- this function only
    // returns once the main thread's own tinox_handle_connections loop
    // does, which in practice is never (same as the workers' lifetime).
    struct TinoxWorkerArgs worker_args;
    worker_args.srv       = srv;
    worker_args.port      = port;
    worker_args.bind_addr = srv->bind_addr;

    for (int i = 1; i < nthreads; i++) {
        pthread_t tid;
        pthread_create(&tid, NULL, tinox_worker_run, &worker_args);
        pthread_detach(tid);
    }

    // Main thread creates its own SO_REUSEPORT socket
    int64_t server_fd = httpServerCreateOn(port, srv->bind_addr);
    if (server_fd < 0) {
        // A failed bind used to just log and return here, leaving the
        // process running with its worker threads spawned above (which
        // fail the same bind and quietly return too) but no listener ever
        // actually up -- silent garbage: the program looks alive while
        // serving nothing, and any already-listening process on the same
        // port silently absorbs every request instead (see issue #226,
        // where this masked as a false HTTP-server/GC regression report).
        // Hard-fail instead, matching this runtime's convention for a
        // fatal setup error (see e.g. processSpawnInteractive above).
        int saved_errno = errno;
        fprintf(stderr, "runtime error: HttpServer::listen(): failed to bind port %lld (%s)\n",
                (long long)port, strerror(saved_errno));
        exit(1);
    }
    tinox_handle_connections(srv, server_fd);
    httpServerClose(server_fd);
}

// ---- JSON ----

#define JSON_NULL      0
#define JSON_BOOL      1
#define JSON_INT       2
#define JSON_FLOAT     3
#define JSON_STRING    4
#define JSON_ARRAY     5
#define JSON_OBJECT    6
#define JSON_INT_ARRAY 7  // fast-path: arr_val points to int64 values, arr_val[-1]=len

// The JsonValue nodes + string/map data of a parse live on the normal
// GC heap (malloc()/GC_malloc(), see the redirect macros above) instead of a
// separate arena buffer. This used to be a `__thread`-local arena
// allocator whose "used" pointer got reset to 0 on EVERY jsonParse()
// call ("valid until the next call", per the comment in the
// original code) — any JsonValue/map from an EARLIER
// parse whose reference lived longer (e.g. several parses combined
// into one object) got overwritten by new allocations on the
// SAME thread on the next jsonParse() call -- silent
// garbage/use-after-free (bug 24 finding). The GC now manages the
// lifetime individually per object like everywhere else in the runtime —
// no more special "valid only until the next parse" behavior.
static void* json_arena_alloc(size_t size) {
    return malloc(size);
}

typedef struct TinoxJsonValue {
    int64_t type;
    union {
        int64_t  bool_val;
        int64_t  int_val;
        double   float_val;
        char*    str_val;
        int64_t* arr_val;  // tinox-style array (len at [-1])
        void*    obj_val;  // TinoxMap*
    };
} TinoxJsonValue;

static TinoxJsonValue* json_alloc(int64_t type) {
    TinoxJsonValue* v = (TinoxJsonValue*)json_arena_alloc(sizeof(TinoxJsonValue));
    v->type = type;
    return v;
}

#define json_skip_ws(p) ({ const char* _p = (p); while (*_p == ' ' || *_p == '\t' || *_p == '\r' || *_p == '\n') _p++; _p; })

// JSON-object map: keys kommen bereits als frische, individuelle
// json_arena_alloc()-Allokationen aus json_parse_string_raw() -- kein
// erneutes strdup() noetig, borrowed_keys=1 ist reine Vermeidung
// redundanter Kopien, kein Lebensdauer-Trick mehr (s. Kommentar bei
// json_arena_alloc oben).
static void* json_obj_map_create(void) {
    TinoxMap* m = (TinoxMap*)json_arena_alloc(sizeof(TinoxMap));
    m->cap = 4;
    m->len = 0;
    m->tombs = 0;
    m->entries = (TinoxMapEntry*)json_arena_alloc(4 * sizeof(TinoxMapEntry));
    memset(m->entries, 0, 4 * sizeof(TinoxMapEntry));
    m->borrowed_keys = 1;
    TINOX_CK_REG(m, TINOX_KIND_MAP);
    return m;
}

static void json_obj_map_set(void* map, const char* key, int64_t value) {
    TinoxMap* m = (TinoxMap*)map;
    if (m->len * TINOX_MAP_LOAD_DEN >= m->cap * TINOX_MAP_LOAD_NUM) {
        size_t new_cap = m->cap * 2;
        TinoxMapEntry* ne = (TinoxMapEntry*)json_arena_alloc(new_cap * sizeof(TinoxMapEntry));
        memset(ne, 0, new_cap * sizeof(TinoxMapEntry));
        for (size_t i = 0; i < m->cap; i++) {
            char* k = m->entries[i].key;
            if (!k || k == (char*)1) continue;
            size_t idx = map_hash(k, new_cap);
            while (ne[idx].key) idx = (idx + 1) & (new_cap - 1);
            ne[idx] = m->entries[i];
        }
        m->entries = ne;
        m->cap = new_cap;
    }
    size_t idx = map_hash(key, m->cap);
    while (1) {
        char* k = m->entries[idx].key;
        if (!k || k == (char*)1) {
            m->entries[idx].key   = (char*)key; // arena key, no strdup
            m->entries[idx].value = value;
            m->len++;
            return;
        }
        if (strcmp(k, key) == 0) { m->entries[idx].value = value; return; }
        idx = (idx + 1) & (m->cap - 1);
    }
}


static TinoxJsonValue* json_parse_value(const char** p);

// Issue #603: parse exactly four hex digits at `s`; -1 if any is not hex.
static int json_hex4(const char* s) {
    int v = 0;
    for (int i = 0; i < 4; i++) {
        char c = s[i];
        int d;
        if (c >= '0' && c <= '9') d = c - '0';
        else if (c >= 'a' && c <= 'f') d = c - 'a' + 10;
        else if (c >= 'A' && c <= 'F') d = c - 'A' + 10;
        else return -1;
        v = v * 16 + d;
    }
    return v;
}

// Append code point `cp` (<= 0x10FFFF, not 0) to `buf` as UTF-8; returns bytes written.
static size_t json_put_utf8(char* buf, uint32_t cp) {
    if (cp < 0x80) { buf[0] = (char)cp; return 1; }
    if (cp < 0x800) { buf[0] = (char)(0xC0 | (cp >> 6)); buf[1] = (char)(0x80 | (cp & 0x3F)); return 2; }
    if (cp < 0x10000) {
        buf[0] = (char)(0xE0 | (cp >> 12)); buf[1] = (char)(0x80 | ((cp >> 6) & 0x3F)); buf[2] = (char)(0x80 | (cp & 0x3F));
        return 3;
    }
    buf[0] = (char)(0xF0 | (cp >> 18)); buf[1] = (char)(0x80 | ((cp >> 12) & 0x3F));
    buf[2] = (char)(0x80 | ((cp >> 6) & 0x3F)); buf[3] = (char)(0x80 | (cp & 0x3F));
    return 4;
}

static char* json_parse_string_raw(const char** p) {
    (*p)++; // skip '"'
    // Pre-scan to get exact length — avoids malloc+realloc
    const char* scan = *p;
    size_t max_len = 0;
    while (*scan && *scan != '"') {
        if (*scan == '\\') {
            scan++;
            // Bug 97: a trailing backslash right at end-of-input (a
            // truncated escape, e.g. malformed JSON `"abc\` with no closing
            // quote and nothing after the backslash) used to fall through
            // to the unconditional scan++ below, advancing past the NUL
            // terminator into out-of-bounds memory and continuing the scan
            // there. Stop instead.
            if (!*scan) break;
        }
        scan++;
        max_len++;
    }
    char* buf = (char*)json_arena_alloc(max_len + 1);
    size_t len = 0;
    while (**p && **p != '"') {
        if (**p == '\\') {
            (*p)++;
            // Bug 97: same trailing-backslash-at-EOF case as the pre-scan
            // loop above -- stop before reading/advancing past the NUL.
            if (!**p) break;
            char esc = **p;
            if      (esc == 'n')  buf[len++] = '\n';
            else if (esc == 't')  buf[len++] = '\t';
            else if (esc == 'r')  buf[len++] = '\r';
            else if (esc == '"')  buf[len++] = '"';
            else if (esc == '\\') buf[len++] = '\\';
            else if (esc == '/')  buf[len++] = '/';
            else if (esc == 'b')  buf[len++] = '\b';
            else if (esc == 'f')  buf[len++] = '\f';
            else if (esc == 'u' && json_hex4(*p + 1) >= 0) {
                // Issue #603: \uXXXX -> UTF-8, surrogate pairs combined. A
                // lone surrogate or \u0000 (cannot live in a C string) becomes
                // U+FFFD instead of silently producing invalid UTF-8.
                uint32_t cp = (uint32_t)json_hex4(*p + 1);
                *p += 4;
                if (cp >= 0xD800 && cp <= 0xDBFF) {
                    int lo = ((*p)[1] == '\\' && (*p)[2] == 'u') ? json_hex4(*p + 3) : -1;
                    if (lo >= 0xDC00 && lo <= 0xDFFF) {
                        cp = 0x10000 + ((cp - 0xD800) << 10) + ((uint32_t)lo - 0xDC00);
                        *p += 6;
                    } else {
                        cp = 0xFFFD;
                    }
                } else if ((cp >= 0xDC00 && cp <= 0xDFFF) || cp == 0) {
                    cp = 0xFFFD;
                }
                len += json_put_utf8(buf + len, cp);
            }
            else                  buf[len++] = esc;
        } else {
            buf[len++] = **p;
        }
        (*p)++;
    }
    if (**p == '"') (*p)++;
    buf[len] = '\0';
    return buf;
}

static TinoxJsonValue* json_parse_value(const char** p) {
    *p = json_skip_ws(*p);
    if (!**p) return json_alloc(JSON_NULL);

    if (**p == '"') {
        TinoxJsonValue* v = json_alloc(JSON_STRING);
        v->str_val = json_parse_string_raw(p);
        return v;
    }
    if (**p == '{') {
        TinoxJsonValue* v = json_alloc(JSON_OBJECT);
        v->obj_val = json_obj_map_create(); // GC-heap-allocated, no strdup for keys
        (*p)++; // skip '{'
        *p = json_skip_ws(*p);
        while (**p && **p != '}') {
            *p = json_skip_ws(*p);
            if (**p != '"') break;
            char* key = json_parse_string_raw(p);
            *p = json_skip_ws(*p);
            if (**p == ':') (*p)++;
            TinoxJsonValue* val = json_parse_value(p);
            json_obj_map_set(v->obj_val, key, (int64_t)(uintptr_t)val);
            *p = json_skip_ws(*p);
            if (**p == ',') (*p)++;
        }
        if (**p == '}') (*p)++;
        return v;
    }
    if (**p == '[') {
        (*p)++; // skip '['
        *p = json_skip_ws(*p);
        // Single-pass fast-path: parse integers directly with malloc+doubling.
        // If a non-integer is found, free and fall through to generic parser.
        if (**p == ']') {
            (*p)++;
            TinoxJsonValue* v = json_alloc(JSON_INT_ARRAY);
            int64_t* raw = (int64_t*)json_arena_alloc(sizeof(int64_t));
            raw[0] = 0;
            v->arr_val = raw + 1;
            return v;
        }
        const char* saved_p = *p;
        // Stack buffer covers typical arrays without any malloc
        int64_t stack_buf[256];
        size_t fast_cap = 256, fast_len = 0;
        int64_t* fast_arr = stack_buf;
        int64_t* heap_buf = NULL;
        int is_int_array = 1;
        while (**p && **p != ']') {
            *p = json_skip_ws(*p);
            int neg = (**p == '-');
            if (neg) (*p)++;
            if (**p < '0' || **p > '9') { is_int_array = 0; break; }
            int64_t val = 0;
            while (**p >= '0' && **p <= '9') val = val * 10 + (*(*p)++ - '0');
            *p = json_skip_ws(*p);
            if (**p != ',' && **p != ']') { is_int_array = 0; break; }
            if (fast_len >= fast_cap) {
                fast_cap *= 2;
                if (!heap_buf) {
                    heap_buf = (int64_t*)malloc(fast_cap * sizeof(int64_t));
                    memcpy(heap_buf, stack_buf, fast_len * sizeof(int64_t));
                } else {
                    heap_buf = (int64_t*)realloc(heap_buf, fast_cap * sizeof(int64_t));
                }
                fast_arr = heap_buf;
            }
            fast_arr[fast_len++] = neg ? -val : val;
            if (**p == ',') (*p)++;
        }
        if (is_int_array && **p == ']') {
            (*p)++;
            TinoxJsonValue* v = json_alloc(JSON_INT_ARRAY);
            int64_t* raw = (int64_t*)json_arena_alloc((fast_len + 1) * sizeof(int64_t));
            raw[0] = (int64_t)fast_len;
            memcpy(raw + 1, fast_arr, fast_len * sizeof(int64_t));
            if (heap_buf) free(heap_buf);
            v->arr_val = raw + 1;
            return v;
        }
        if (heap_buf) free(heap_buf);
        *p = saved_p; // restore position for generic fallback
        // Generic fallback for mixed-type arrays
        TinoxJsonValue* v = json_alloc(JSON_ARRAY);
        size_t cap = 8, len = 0;
        int64_t* raw = (int64_t*)malloc((cap + 1) * sizeof(int64_t));
        int64_t* arr = raw + 1;
        while (**p && **p != ']') {
            TinoxJsonValue* elem = json_parse_value(p);
            if (len >= cap) {
                cap *= 2;
                raw = (int64_t*)realloc(raw, (cap + 1) * sizeof(int64_t));
                arr = raw + 1;
            }
            arr[len++] = (int64_t)(uintptr_t)elem;
            *p = json_skip_ws(*p);
            if (**p == ',') (*p)++;
            *p = json_skip_ws(*p);
        }
        if (**p == ']') (*p)++;
        raw[0] = (int64_t)len;
        v->arr_val = arr;
        return v;
    }
    if (strncmp(*p, "true", 4) == 0) {
        TinoxJsonValue* v = json_alloc(JSON_BOOL);
        v->bool_val = 1; *p += 4; return v;
    }
    if (strncmp(*p, "false", 5) == 0) {
        TinoxJsonValue* v = json_alloc(JSON_BOOL);
        v->bool_val = 0; *p += 5; return v;
    }
    if (strncmp(*p, "null", 4) == 0) { *p += 4; return json_alloc(JSON_NULL); }
    // Number
    const char* start = *p;
    int is_float = 0;
    if (**p == '-') (*p)++;
    while (**p >= '0' && **p <= '9') (*p)++;
    if (**p == '.') { is_float = 1; (*p)++; while (**p >= '0' && **p <= '9') (*p)++; }
    if (**p == 'e' || **p == 'E') { is_float = 1; (*p)++; if (**p == '+' || **p == '-') (*p)++; while (**p >= '0' && **p <= '9') (*p)++; }
    // Bug 97: an unrecognized token (not a string/object/array/true/false/
    // null, and not even the start of a number) used to fall through to
    // here with *p left unchanged, silently returning JSON_INT(0) without
    // advancing the cursor. A caller looping until it sees the closing
    // bracket/brace (the array parser above in particular) would then call
    // json_parse_value() on the exact same position forever, appending a
    // new dummy element every iteration -- an infinite loop + unbounded
    // memory growth on malformed input like `[x]`. Force forward progress:
    // if nothing number-like was actually consumed, treat this byte as a
    // malformed token and skip it instead of looping in place.
    if (*p == start) {
        (*p)++;
        return json_alloc(JSON_NULL);
    }
    if (is_float) {
        TinoxJsonValue* v = json_alloc(JSON_FLOAT);
        v->float_val = atof(start);
        return v;
    } else {
        TinoxJsonValue* v = json_alloc(JSON_INT);
        // Inline fast int parse — avoids strtol machinery of atoll
        int64_t val = 0;
        const char* s = start;
        int neg = (*s == '-');
        if (neg) s++;
        while (*s >= '0' && *s <= '9') val = val * 10 + (*s++ - '0');
        v->int_val = neg ? -val : val;
        return v;
    }
}

int64_t* jsonParse(const char* text) {
    if (!text) return (int64_t*)json_alloc(JSON_NULL);
    const char* p = text;
    return (int64_t*)json_parse_value(&p);
}

static size_t fast_i64_write(int64_t val, char* buf);
static void json_stringify_value(TinoxJsonValue* v, char** out, size_t* len, size_t* cap);

static void json_append(char** out, size_t* len, size_t* cap, const char* s, size_t slen) {
    while (*len + slen + 1 >= *cap) { *cap *= 2; *out = (char*)realloc(*out, *cap); }
    memcpy(*out + *len, s, slen);
    *len += slen;
    (*out)[*len] = '\0';
}

static void json_append_str(char** out, size_t* len, size_t* cap, const char* s) {
    // Escape and append a string value (with surrounding quotes)
    json_append(out, len, cap, "\"", 1);
    for (const char* p = s; *p; p++) {
        if      (*p == '"')  json_append(out, len, cap, "\\\"", 2);
        else if (*p == '\\') json_append(out, len, cap, "\\\\", 2);
        else if (*p == '\n') json_append(out, len, cap, "\\n",  2);
        else if (*p == '\r') json_append(out, len, cap, "\\r",  2);
        else if (*p == '\t') json_append(out, len, cap, "\\t",  2);
        else                 json_append(out, len, cap, p,       1);
    }
    json_append(out, len, cap, "\"", 1);
}

static void json_stringify_value(TinoxJsonValue* v, char** out, size_t* len, size_t* cap) {
    if (!v) { json_append(out, len, cap, "null", 4); return; }
    char buf[64];
    int n;
    switch (v->type) {
        case JSON_NULL:   json_append(out, len, cap, "null",  4); break;
        case JSON_BOOL:   json_append(out, len, cap, v->bool_val ? "true" : "false", v->bool_val ? 4 : 5); break;
        case JSON_INT:    n = snprintf(buf, sizeof(buf), "%lld", (long long)v->int_val); json_append(out, len, cap, buf, n); break;
        case JSON_FLOAT:  n = snprintf(buf, sizeof(buf), "%g",   v->float_val); json_append(out, len, cap, buf, n); break;
        case JSON_STRING: json_append_str(out, len, cap, v->str_val ? v->str_val : ""); break;
        case JSON_INT_ARRAY: {
            json_append(out, len, cap, "[", 1);
            if (v->arr_val) {
                int64_t alen = v->arr_val[-1];
                char nbuf[24];
                for (int64_t i = 0; i < alen; i++) {
                    if (i > 0) json_append(out, len, cap, ",", 1);
                    size_t nlen = fast_i64_write(v->arr_val[i], nbuf);
                    json_append(out, len, cap, nbuf, nlen);
                }
            }
            json_append(out, len, cap, "]", 1);
            break;
        }
        case JSON_ARRAY: {
            json_append(out, len, cap, "[", 1);
            if (v->arr_val) {
                int64_t alen = v->arr_val[-1];
                for (int64_t i = 0; i < alen; i++) {
                    if (i > 0) json_append(out, len, cap, ",", 1);
                    json_stringify_value((TinoxJsonValue*)(uintptr_t)v->arr_val[i], out, len, cap);
                }
            }
            json_append(out, len, cap, "]", 1);
            break;
        }
        case JSON_OBJECT: {
            json_append(out, len, cap, "{", 1);
            if (v->obj_val) {
                int64_t* keys_h = tinox_map_keys(v->obj_val);
                int64_t* keys = ((TinoxArray*)keys_h)->data;
                int64_t klen = ((TinoxArray*)keys_h)->len;
                for (int64_t i = 0; i < klen; i++) {
                    if (i > 0) json_append(out, len, cap, ",", 1);
                    const char* k = (const char*)(uintptr_t)keys[i];
                    json_append_str(out, len, cap, k);
                    json_append(out, len, cap, ":", 1);
                    int64_t vptr = tinox_map_get(v->obj_val, k);
                    json_stringify_value((TinoxJsonValue*)(uintptr_t)vptr, out, len, cap);
                }
            }
            json_append(out, len, cap, "}", 1);
            break;
        }
        default: json_append(out, len, cap, "null", 4); break;
    }
}

char* jsonStringify(int64_t* value) {
    size_t cap = 256, len = 0;
    char* out = (char*)malloc(cap);
    out[0] = '\0';
    json_stringify_value((TinoxJsonValue*)value, &out, &len, &cap);
    return out;
}

int64_t jsonGetInt(int64_t* value) {
    TinoxJsonValue* v = (TinoxJsonValue*)value;
    if (!v) return 0;
    if (v->type == JSON_FLOAT) return (int64_t)v->float_val;
    return v->int_val;
}

double jsonGetFloat(int64_t* value) {
    TinoxJsonValue* v = (TinoxJsonValue*)value;
    if (!v) return 0.0;
    if (v->type == JSON_INT) return (double)v->int_val;
    return v->float_val;
}

char* jsonGetString(int64_t* value) {
    TinoxJsonValue* v = (TinoxJsonValue*)value;
    if (!v || v->type != JSON_STRING || !v->str_val) return "";
    return v->str_val;
}

int64_t jsonGetBool(int64_t* value) {
    TinoxJsonValue* v = (TinoxJsonValue*)value;
    if (!v) return 0;
    return v->bool_val;
}

void* jsonGetObject(int64_t* value) {
    TinoxJsonValue* v = (TinoxJsonValue*)value;
    if (!v || v->type != JSON_OBJECT) return tinox_map_create();
    return v->obj_val;
}

// Wrap an existing Map<String, JsonValue> handle as a JsonValue object.
// TinoxMap has the same layout whether allocated via tinox_map_create()
// (used by user Map<K,V> values) or json_obj_map_create() (the JSON
// parser) — both GC-heap-allocated (s. json_arena_alloc oben), safe to
// reuse the caller's map directly.
int64_t* jsonFromMap(void* map) {
    TinoxJsonValue* v = (TinoxJsonValue*)malloc(sizeof(TinoxJsonValue));
    v->type = JSON_OBJECT;
    v->obj_val = map;
    return (int64_t*)v;
}

int64_t* jsonGetArray(int64_t* value) {
    TinoxJsonValue* v = (TinoxJsonValue*)value;
    if (!v || v->type != JSON_ARRAY || !v->arr_val) {
        int64_t* empty = (int64_t*)malloc(sizeof(int64_t));
        empty[0] = 0; return empty + 1;
    }
    return v->arr_val;
}

// Index-based JSON_ARRAY accessors (issue #138, OIDC JWKS "keys" array) --
// NOT a direct JsonValue-array-to-List<JsonValue> conversion, because
// arr_val's internal layout (length at arr_val[-1], elements directly in
// the buffer) is a different, single-indirection format from the
// {len,cap,data} 3-word handle a Tinox List<T> value actually is (see
// TinoxArray above): returning arr_val itself as a List<JsonValue> would
// have Tinox read arr_val[0] (a JsonValue pointer) as a length. Tinox-side
// (JsonValue::asList()) builds a real List<JsonValue> by looping these two.
int64_t jsonArrayLen(int64_t* value) {
    TinoxJsonValue* v = (TinoxJsonValue*)value;
    if (!v || (v->type != JSON_ARRAY && v->type != JSON_INT_ARRAY) || !v->arr_val) return 0;
    return v->arr_val[-1];
}

int64_t* jsonArrayGet(int64_t* value, int64_t index) {
    TinoxJsonValue* v = (TinoxJsonValue*)value;
    int is_arr = v && (v->type == JSON_ARRAY || v->type == JSON_INT_ARRAY) && v->arr_val;
    int64_t len = is_arr ? v->arr_val[-1] : 0;
    if (!v || index < 0 || index >= len) {
        fprintf(stderr, "runtime error: JSON array index out of bounds: %lld (length %lld)\n",
                (long long)index, (long long)len);
        exit(1);
    }
    if (v->type == JSON_INT_ARRAY) {
        // Fast-path arrays store raw int64s; box the element as a JSON_INT value.
        TinoxJsonValue* e = json_alloc(JSON_INT);
        e->int_val = v->arr_val[index];
        return (int64_t*)e;
    }
    return (int64_t*)(uintptr_t)v->arr_val[index];
}

int64_t jsonIsNull(int64_t* value)   { return (!value || ((TinoxJsonValue*)value)->type == JSON_NULL)  ? 1 : 0; }
int64_t jsonIsString(int64_t* value) { return (value && ((TinoxJsonValue*)value)->type == JSON_STRING) ? 1 : 0; }
int64_t jsonIsInt(int64_t* value)    { return (value && ((TinoxJsonValue*)value)->type == JSON_INT)    ? 1 : 0; }
int64_t jsonIsFloat(int64_t* value)  { return (value && ((TinoxJsonValue*)value)->type == JSON_FLOAT)  ? 1 : 0; }
int64_t jsonIsBool(int64_t* value)   { return (value && ((TinoxJsonValue*)value)->type == JSON_BOOL)   ? 1 : 0; }
int64_t jsonIsObject(int64_t* value) { return (value && ((TinoxJsonValue*)value)->type == JSON_OBJECT) ? 1 : 0; }
int64_t jsonIsArray(int64_t* value)  { return (value && (((TinoxJsonValue*)value)->type == JSON_ARRAY || ((TinoxJsonValue*)value)->type == JSON_INT_ARRAY)) ? 1 : 0; }

int64_t* jsonGetField(int64_t* obj, const char* key) {
    TinoxJsonValue* v = (TinoxJsonValue*)obj;
    if (!v || v->type != JSON_OBJECT || !v->obj_val) return NULL;
    int64_t vptr = tinox_map_get(v->obj_val, key);
    return (int64_t*)(uintptr_t)vptr;
}

// Request DTO validation (issue #523).
int64_t jsonGetIntField(int64_t*, const char*);
double jsonGetFloatField(int64_t*, const char*);
int jsonGetBoolField(int64_t*, const char*);
char* jsonGetStringField(int64_t*, const char*);
/* Strict, bounded request-only JSON parsing. The general Json API retains its
 * compatibility behavior. A parsed tree is bound only after schema validation. */
typedef struct { const char *p; const char *error; size_t nodes; } RequestJson;
static TinoxJsonValue *request_value(RequestJson *r, int depth);
static int request_hex(RequestJson *r) {
    int n = 0;
    for (int i = 0; i < 4; i++) {
        unsigned char c = (unsigned char)*r->p;
        int x = c >= '0' && c <= '9' ? c-'0' : c >= 'a' && c <= 'f' ? c-'a'+10 : c >= 'A' && c <= 'F' ? c-'A'+10 : -1;
        if (x < 0) { r->error = "malformed"; return -1; }
        r->p++; n = n*16+x;
    }
    return n;
}
static char *request_string(RequestJson *r) {
    r->p++;
    const char *string_end=r->p;
    while (*string_end && *string_end != '"') { if (*string_end == '\\' && string_end[1]) string_end++; string_end++; }
    char *out = malloc((size_t)(string_end-r->p)+1); size_t n = 0;
    while (*r->p && *r->p != '"') {
        unsigned char c = (unsigned char)*r->p++;
        if (c < 32) { r->error = "malformed"; break; }
        if (c == '\\') {
            if (!*r->p) { r->error="malformed"; break; }
            c = (unsigned char)*r->p++;
            if (c == 'u') {
                int cp = request_hex(r);
                if (cp >= 0xd800 && cp <= 0xdbff) {
                    if (strncmp(r->p,"\\u",2)) { r->error="malformed"; break; }
                    r->p += 2; int low = request_hex(r);
                    if (low < 0xdc00 || low > 0xdfff) { r->error="malformed"; break; }
                    cp=0x10000+((cp-0xd800)<<10)+(low-0xdc00);
                } else if (cp >= 0xdc00 && cp <= 0xdfff) { r->error="malformed"; break; }
                /* Tinox strings are NUL terminated: reject embedded NUL. */
                if (cp <= 0 || r->error) { r->error="malformed"; break; }
                if (cp < 0x80) out[n++]=(char)cp;
                else if (cp < 0x800) { out[n++]=(char)(0xc0|(cp>>6)); out[n++]=(char)(0x80|(cp&63)); }
                else if (cp < 0x10000) { out[n++]=(char)(0xe0|(cp>>12)); out[n++]=(char)(0x80|((cp>>6)&63)); out[n++]=(char)(0x80|(cp&63)); }
                else { out[n++]=(char)(0xf0|(cp>>18)); out[n++]=(char)(0x80|((cp>>12)&63)); out[n++]=(char)(0x80|((cp>>6)&63)); out[n++]=(char)(0x80|(cp&63)); }
                continue;
            }
            const char *esc = "\"\\/bfnrt", *decoded = "\"\\/\b\f\n\r\t";
            const char *found = c ? strchr(esc,c) : NULL;
            if (!found) { r->error="malformed"; break; }
            out[n++]=decoded[found-esc];
        } else if (c >= 0x80) {
            r->p--; size_t w=tinox_utf8_scalar_width((const unsigned char*)r->p,(size_t)(string_end-r->p));
            if (!w) { r->error="malformed"; break; }
            memcpy(out+n,r->p,w); n+=w; r->p+=w;
        } else out[n++]=(char)c;
    }
    if (*r->p != '"') r->error="malformed"; else r->p++;
    out[n]=0; return out;
}
static TinoxJsonValue *request_value(RequestJson *r, int depth) {
    if (r->error) return NULL;
    if (depth > 32) { r->error="depth"; return NULL; }
    if (++r->nodes > 100000) { r->error="size"; return NULL; }
    r->p=json_skip_ws(r->p);
    TinoxJsonValue *v;
    if (*r->p == '"') { v=json_alloc(JSON_STRING); v->str_val=request_string(r); return v; }
    if (*r->p == '{') {
        r->p++; v=json_alloc(JSON_OBJECT); v->obj_val=tinox_map_create(); r->p=json_skip_ws(r->p);
        if (*r->p == '}') { r->p++; return v; }
        while (!r->error) {
            if (*r->p != '"') break;
            char *key=request_string(r); r->p=json_skip_ws(r->p);
            if (*r->p != ':') break;
            r->p++;
            TinoxJsonValue *item=request_value(r,depth+1);
            if (r->error) return v;
            if (tinox_map_get(v->obj_val,key)) { r->error="duplicate"; return v; }
            tinox_map_set(v->obj_val,key,(int64_t)(intptr_t)item);
            r->p=json_skip_ws(r->p);
            if (*r->p == '}') { r->p++; return v; }
            if (*r->p != ',') break;
            r->p++; r->p=json_skip_ws(r->p);
        }
        r->error="malformed"; return v;
    }
    if (*r->p == '[') {
        r->p++; v=json_alloc(JSON_ARRAY); size_t n=0,cap=8;
        int64_t *storage=malloc((cap+1)*sizeof(int64_t)); r->p=json_skip_ws(r->p);
        if (*r->p != ']') while (!r->error) {
            TinoxJsonValue *item=request_value(r,depth+1); if (r->error) break;
            if (n == cap) { cap*=2; storage=realloc(storage,(cap+1)*sizeof(int64_t)); }
            storage[1+n++]=(int64_t)(intptr_t)item; r->p=json_skip_ws(r->p);
            if (*r->p == ']') break;
            if (*r->p != ',') { r->error="malformed"; break; }
            r->p++;
        }
        storage[0]=(int64_t)n; v->arr_val=storage+1;
        if (*r->p == ']') r->p++; else if (!r->error) r->error="malformed";
        return v;
    }
    for (int i=0;i<3;i++) {
        const char *word=i==0?"null":i==1?"true":"false";
        if (!strncmp(r->p,word,strlen(word))) { r->p+=strlen(word); v=json_alloc(i==0?JSON_NULL:JSON_BOOL); v->bool_val=i==1; return v; }
    }
    const char *start=r->p;
    if (*r->p == '-') r->p++;
    if (*r->p == '0') r->p++;
    else if (*r->p >= '1' && *r->p <= '9') { while (*r->p >= '0' && *r->p <= '9') r->p++; }
    else { r->error="malformed"; return NULL; }
    int floating=0;
    if (*r->p == '.') { floating=1; r->p++; if (*r->p < '0' || *r->p > '9') { r->error="malformed"; return NULL; } while (*r->p >= '0' && *r->p <= '9') r->p++; }
    if (*r->p == 'e' || *r->p == 'E') { floating=1; r->p++; if (*r->p == '+' || *r->p == '-') r->p++; if (*r->p < '0' || *r->p > '9') { r->error="malformed"; return NULL; } while (*r->p >= '0' && *r->p <= '9') r->p++; }
    errno=0; char *end;
    if (floating) { v=json_alloc(JSON_FLOAT); v->float_val=strtod(start,&end); if (errno || !isfinite(v->float_val)) r->error="number"; }
    else { v=json_alloc(JSON_INT); v->int_val=strtoll(start,&end,10); if (errno) r->error="number"; }
    if (end != r->p) r->error="malformed";
    return v;
}
static char *request_error(const char *path,const char *code) {
    char *encoded=tinox_json_encode_string(path);
    size_t n=strlen(encoded)+strlen(code)+100; char *out=malloc(n);
    snprintf(out,n,"{\"error\":\"validation_failed\",\"details\":[{\"path\":%s,\"code\":\"%s\"}]}",encoded,code);
    return out;
}
static int request_key_compare(const void *a,const void *b) { return strcmp(*(const char* const*)a,*(const char* const*)b); }
static char *request_validate(TinoxJsonValue *value,TinoxJsonValue *schema,TinoxJsonValue *defs,const char *path,int depth) {
    if (depth > 32) return request_error(path,"depth");
    if (!value || value->type == JSON_NULL) {
        if (jsonGetBoolField((int64_t*)schema,"required")) return request_error(path,value?"non_null":"required");
        return NULL;
    }
    const char *cls=jsonGetStringField((int64_t*)schema,"class");
    if (cls && *cls) {
        if (value->type != JSON_OBJECT) return request_error(path,"type");
        TinoxJsonValue *fs=(TinoxJsonValue*)jsonGetField((int64_t*)defs,cls);
        if (!fs) return request_error(path,"unsupported");
        for (int64_t i=0;i<jsonArrayLen((int64_t*)fs);i++) {
            int64_t *f=jsonArrayGet((int64_t*)fs,i); const char *name=jsonGetStringField(f,"name");
            size_t n=strlen(path)+strlen(name)+2; char *next=malloc(n); snprintf(next,n,"%s.%s",path,name);
            char *error=request_validate((TinoxJsonValue*)jsonGetField((int64_t*)value,name),(TinoxJsonValue*)jsonGetField(f,"schema"),defs,next,depth+1);
            if (error) return error;
        }
        return NULL;
    }
    const char *type=jsonGetStringField((int64_t*)schema,"type");
    if (!strcmp(type,"unsupported")) return request_error(path,"unsupported");
    int valid=!strcmp(type,"string")?value->type==JSON_STRING:!strcmp(type,"boolean")?value->type==JSON_BOOL:!strcmp(type,"integer")?value->type==JSON_INT:!strcmp(type,"number")?(value->type==JSON_INT||value->type==JSON_FLOAT):!strcmp(type,"array")?value->type==JSON_ARRAY:!strcmp(type,"object")?value->type==JSON_OBJECT:0;
    if (!valid) return request_error(path,"type");
    if (value->type==JSON_OBJECT) {
        TinoxMap *map=(TinoxMap*)value->obj_val; char **keys=malloc((map->len+1)*sizeof(char*));size_t count=0;
        for(size_t i=0;i<map->cap;i++) if(map->entries[i].key && map->entries[i].key != (char*)1) keys[count++]=map->entries[i].key;
        qsort(keys,count,sizeof(char*),request_key_compare);
        for(size_t i=0;i<count;i++) {
            char *encoded=tinox_json_encode_string(keys[i]);size_t n=strlen(path)+strlen(encoded)+3;char *next=malloc(n);snprintf(next,n,"%s[%s]",path,encoded);
            char *error=request_validate((TinoxJsonValue*)jsonGetField((int64_t*)value,keys[i]),(TinoxJsonValue*)jsonGetField((int64_t*)schema,"values"),defs,next,depth+1);if(error)return error;
        }
    }
    if (value->type == JSON_STRING || value->type == JSON_ARRAY) {
        size_t length=0;
        if (value->type==JSON_ARRAY) length=(size_t)jsonArrayLen((int64_t*)value);
        else { size_t n=strlen(value->str_val); for(size_t i=0;i<n;) { size_t w=tinox_utf8_scalar_width((unsigned char*)value->str_val+i,n-i); if(!w)return request_error(path,"malformed"); length++; i+=w; } }
        if (jsonGetField((int64_t*)schema,"minLength") && ((double)length<jsonGetFloatField((int64_t*)schema,"minLength") || (double)length>jsonGetFloatField((int64_t*)schema,"maxLength"))) return request_error(path,"length");
        if (value->type==JSON_ARRAY) for(int64_t i=0;i<(int64_t)length;i++) {
            size_t n=strlen(path)+32; char *next=malloc(n); snprintf(next,n,"%s[%lld]",path,(long long)i);
            char *error=request_validate((TinoxJsonValue*)jsonArrayGet((int64_t*)value,i),(TinoxJsonValue*)jsonGetField((int64_t*)schema,"items"),defs,next,depth+1); if(error)return error;
        }
    }
    if (value->type==JSON_INT) {
        int bits=(int)jsonGetIntField((int64_t*)schema,"bits");
        int unsign=jsonGetBoolField((int64_t*)schema,"unsigned");
        if ((unsign && value->int_val < 0) || (bits > 0 && bits < 64 && (unsign ? (uint64_t)value->int_val > ((UINT64_C(1)<<bits)-1) : (value->int_val < -(INT64_C(1)<<(bits-1)) || value->int_val > ((INT64_C(1)<<(bits-1))-1))))) return request_error(path,"range");
    }
    if (value->type==JSON_INT || value->type==JSON_FLOAT) {
        long double number=value->type==JSON_INT?(long double)value->int_val:(long double)value->float_val;
        TinoxJsonValue *min=(TinoxJsonValue*)jsonGetField((int64_t*)schema,"minimum"), *max=(TinoxJsonValue*)jsonGetField((int64_t*)schema,"maximum");
        if (min && max) {
            long double lo=min->type==JSON_INT?(long double)min->int_val:(long double)min->float_val;
            long double hi=max->type==JSON_INT?(long double)max->int_val:(long double)max->float_val;
            if (number<lo || number>hi) return request_error(path,"range");
        }
    }
    return NULL;
}
char *tinox_request_validate(const char *body,const char *definitions,const char *class_name,int64_t **parsed) {
    /* A request validator has explicit CPU/allocation bounds independent of
     * any transport's own configurable body limit. */
    size_t exact_length=body==g_req_body_ptr?(size_t)g_req_body_len:body==g_json_body_ptr?g_json_body_len:body?strnlen(body,1048577):0;
    if (exact_length>1048576) return request_error("$","size");
    if (body && memchr(body,0,exact_length)) return request_error("$","malformed");
    RequestJson r={body?body:"",NULL,0}; TinoxJsonValue *value=request_value(&r,0);
    if (!r.error && *json_skip_ws(r.p)) r.error="malformed";
    if (r.error) return request_error("$",r.error);
    TinoxJsonValue schema={.type=JSON_OBJECT}; schema.obj_val=tinox_map_create();
    TinoxJsonValue *cls=json_alloc(JSON_STRING); cls->str_val=(char*)class_name; tinox_map_set(schema.obj_val,"class",(int64_t)(intptr_t)cls);
    TinoxJsonValue *req=json_alloc(JSON_BOOL);req->bool_val=1;tinox_map_set(schema.obj_val,"required",(int64_t)(intptr_t)req);
    char *error=request_validate(value,&schema,(TinoxJsonValue*)jsonParse(definitions),"$",0);
    if (!error) *parsed=(int64_t*)value;
    return error;
}


int64_t* jsonIntArrayFromJson(int64_t* json_array) {
    TinoxJsonValue* v = (TinoxJsonValue*)json_array;
    if (!v) return tinox_array_new(0, 0);
    // Fast-path: pure int array — copy the arena data
    // (internal JSON arrays keep the arena layout with len at arr_val[-1])
    if (v->type == JSON_INT_ARRAY) {
        int64_t len = v->arr_val ? v->arr_val[-1] : 0;
        int64_t* nh = tinox_array_new(len, 0);
        if (len > 0) memcpy(((TinoxArray*)nh)->data, v->arr_val, (size_t)len * sizeof(int64_t));
        return nh;
    }
    // Generic JSON_ARRAY path
    int64_t len = (v->type == JSON_ARRAY && v->arr_val) ? v->arr_val[-1] : 0;
    int64_t* nh = tinox_array_new(len, 0);
    int64_t* buf = ((TinoxArray*)nh)->data;
    for (int64_t i = 0; i < len; i++) {
        TinoxJsonValue* elem = (TinoxJsonValue*)(uintptr_t)v->arr_val[i];
        if (elem) {
            if      (elem->type == JSON_INT)   buf[i] = elem->int_val;
            else if (elem->type == JSON_FLOAT) buf[i] = (int64_t)elem->float_val;
            else                               buf[i] = 0;
        } else {
            buf[i] = 0;
        }
    }
    return nh;
}

static const char g_digit_pairs[201] =
    "00010203040506070809"
    "10111213141516171819"
    "20212223242526272829"
    "30313233343536373839"
    "40414243444546474849"
    "50515253545556575859"
    "60616263646566676869"
    "70717273747576777879"
    "80818283848586878889"
    "90919293949596979899";

__attribute__((noinline)) static size_t fast_i64_write(int64_t val, char* buf) {
    if ((uint64_t)val < 10) { buf[0] = '0' + (char)val; return 1; }
    if ((uint64_t)val < 100) {
        int d = (int)val * 2;
        buf[0] = g_digit_pairs[d]; buf[1] = g_digit_pairs[d + 1];
        return 2;
    }
    char tmp[21];
    int neg = val < 0;
    uint64_t uval = neg ? -(uint64_t)val : (uint64_t)val;
    int n = 0;
    while (uval >= 100) {
        int d = (int)(uval % 100) * 2;
        tmp[n++] = g_digit_pairs[d + 1];
        tmp[n++] = g_digit_pairs[d];
        uval /= 100;
    }
    if (uval >= 10) {
        int d = (int)uval * 2;
        tmp[n++] = g_digit_pairs[d + 1];
        tmp[n++] = g_digit_pairs[d];
    } else {
        tmp[n++] = '0' + (int)uval;
    }
    if (neg) tmp[n++] = '-';
    for (int i = 0; i < n; i++) buf[i] = tmp[n - 1 - i];
    return (size_t)n;
}

static __thread char*  g_wrap_buf = NULL;
static __thread size_t g_wrap_cap = 0;

// Builds {"key":[val,...]} into a thread-local buffer — zero malloc per call
char* jsonIntArrayWrap(const char* key, int64_t* h) {
    TinoxArray* a = (TinoxArray*)h;
    int64_t len = a ? a->len : 0;
    const int64_t* arr = a ? a->data : NULL;
    size_t klen = strlen(key);
    size_t need = 5 + klen + (size_t)len * 22 + 3;
    if (need > g_wrap_cap) {
        size_t nc = g_wrap_cap ? g_wrap_cap * 2 : 4096;
        while (nc < need) nc *= 2;
        g_wrap_buf = (char*)realloc(g_wrap_buf, nc);
        g_wrap_cap = nc;
    }
    char* out = g_wrap_buf;
    size_t pos = 0;
    out[pos++] = '{';
    out[pos++] = '"';
    memcpy(out + pos, key, klen); pos += klen;
    out[pos++] = '"';
    out[pos++] = ':';
    out[pos++] = '[';
    if (arr) {
        for (int64_t i = 0; i < len; i++) {
            if (i > 0) out[pos++] = ',';
            pos += fast_i64_write(arr[i], out + pos);
        }
    }
    out[pos++] = ']';
    out[pos++] = '}';
    out[pos] = '\0';
    return out;
}

char* jsonIntArrayToString(int64_t* h) {
    if (!h) return strdup("[]");
    TinoxArray* a = (TinoxArray*)h;
    int64_t len = a->len;
    const int64_t* arr = a->data;
    size_t cap = (size_t)(len * 21 + 4);
    if (cap < 4) cap = 4;
    char* out = (char*)malloc(cap);
    size_t pos = 0;
    out[pos++] = '[';
    for (int64_t i = 0; i < len; i++) {
        if (i > 0) out[pos++] = ',';
        pos += fast_i64_write(arr[i], out + pos);
    }
    out[pos++] = ']';
    out[pos] = '\0';
    return out;
}

// ---- JsonBuilder — fast @JsonSerializable serialization ----

typedef struct {
    char*  buf;
    size_t len;
    size_t cap;
    int    first;
} JsonBuilder;

static void jb_grow(JsonBuilder* b, size_t need) {
    if (b->len + need <= b->cap) return;
    while (b->cap < b->len + need) b->cap *= 2;
    b->buf = (char*)realloc(b->buf, b->cap);
}

static void jb_key(JsonBuilder* b, const char* key) {
    size_t kl = strlen(key);
    jb_grow(b, kl + 4); // comma + quote + key + quote + colon
    if (!b->first) b->buf[b->len++] = ',';
    b->first = 0;
    b->buf[b->len++] = '"';
    memcpy(b->buf + b->len, key, kl); b->len += kl;
    b->buf[b->len++] = '"';
    b->buf[b->len++] = ':';
}

char* jsonBuilderCreate(void) {
    JsonBuilder* b = (JsonBuilder*)malloc(sizeof(JsonBuilder));
    b->cap = 256;
    b->buf = (char*)malloc(b->cap);
    b->len = 0;
    b->first = 1;
    b->buf[b->len++] = '{';
    return (char*)b;
}

void jsonBuilderAddInt(char* handle, const char* key, int64_t val) {
    JsonBuilder* b = (JsonBuilder*)handle;
    jb_key(b, key);
    jb_grow(b, 21);
    b->len += fast_i64_write(val, b->buf + b->len);
}

void jsonBuilderAddFloat(char* handle, const char* key, double val) {
    JsonBuilder* b = (JsonBuilder*)handle;
    jb_key(b, key);
    char tmp[32];
    int n = snprintf(tmp, sizeof(tmp), "%g", val);
    jb_grow(b, (size_t)n);
    memcpy(b->buf + b->len, tmp, (size_t)n); b->len += (size_t)n;
}

void jsonBuilderAddBool(char* handle, const char* key, int val) {
    JsonBuilder* b = (JsonBuilder*)handle;
    jb_key(b, key);
    if (val) { jb_grow(b, 4); memcpy(b->buf + b->len, "true",  4); b->len += 4; }
    else      { jb_grow(b, 5); memcpy(b->buf + b->len, "false", 5); b->len += 5; }
}

void jsonBuilderAddString(char* handle, const char* key, const char* val) {
    JsonBuilder* b = (JsonBuilder*)handle;
    jb_key(b, key);
    size_t vl = val ? strlen(val) : 0;
    jb_grow(b, vl * 2 + 2); // worst-case: every char escaped
    b->buf[b->len++] = '"';
    if (val) {
        for (size_t i = 0; i < vl; i++) {
            unsigned char c = (unsigned char)val[i];
            if      (c == '"')  { b->buf[b->len++] = '\\'; b->buf[b->len++] = '"'; }
            else if (c == '\\') { b->buf[b->len++] = '\\'; b->buf[b->len++] = '\\'; }
            else if (c == '\n') { b->buf[b->len++] = '\\'; b->buf[b->len++] = 'n'; }
            else if (c == '\r') { b->buf[b->len++] = '\\'; b->buf[b->len++] = 'r'; }
            else if (c == '\t') { b->buf[b->len++] = '\\'; b->buf[b->len++] = 't'; }
            else                { b->buf[b->len++] = (char)c; }
        }
    }
    b->buf[b->len++] = '"';
}

void jsonBuilderAddIntList(char* handle, const char* key, int64_t* h) {
    JsonBuilder* b = (JsonBuilder*)handle;
    jb_key(b, key);
    TinoxArray* a = (TinoxArray*)h;
    int64_t len = a ? a->len : 0;
    const int64_t* arr = a ? a->data : NULL;
    jb_grow(b, (size_t)(len * 21 + 4));
    b->buf[b->len++] = '[';
    for (int64_t i = 0; i < len; i++) {
        if (i > 0) b->buf[b->len++] = ',';
        b->len += fast_i64_write(arr[i], b->buf + b->len);
    }
    b->buf[b->len++] = ']';
}

char* jsonBuilderFinish(char* handle) {
    JsonBuilder* b = (JsonBuilder*)handle;
    jb_grow(b, 2);
    b->buf[b->len++] = '}';
    b->buf[b->len] = '\0';
    char* result = b->buf;
    free(b); // free the builder header only; result owns the buffer
    return result;
}

// Inserts an already-valid raw JSON value (object/array/`null`) as-is --
// unlike jsonBuilderAddString, does NOT quote/escape it. A NULL rawJson
// writes the JSON `null` literal (used by /components' "state" field: no
// persistent instance to dump yet, see tinox_devui_components_json below).
void jsonBuilderAddRaw(char* handle, const char* key, const char* rawJson) {
    JsonBuilder* b = (JsonBuilder*)handle;
    jb_key(b, key);
    if (!rawJson) {
        jb_grow(b, 4);
        memcpy(b->buf + b->len, "null", 4); b->len += 4;
        return;
    }
    size_t vl = strlen(rawJson);
    jb_grow(b, vl);
    memcpy(b->buf + b->len, rawJson, vl); b->len += vl;
}

// Builds the dev-mode introspection API's /components response: one JSON
// object per @ApplicationComponent-scoped class -- name, scope, whether a
// singleton currently exists, and (when it does) its full field-value
// state (states[i], a pre-built raw JSON object string from the
// compiler-generated, null-safe `ClassName_devui_state_json`, or NULL for
// "nothing to show" -- see emit_devui_component_state_handlers,
// codegen.rs). `instantiated` is always 0 for HttpRequest-scoped
// components: they have no persistent singleton to check (emit_di_code's
// _di_create() caches only inside the active request context, never in a
// process-wide instance global), so the compiler-generated caller passes
// a constant 0 (and a NULL state)
// for those rather than loading a global that was never emitted for them.
// N is always small (the program's own @ApplicationComponent count) and
// this is a dev-only introspection endpoint, not a hot path, so the
// per-component tinox_string_concat chain here (same primitive used
// throughout the runtime; GC-tracked, nothing to free by hand) is fine.
char* tinox_devui_components_json(char** names, char** scopes, int64_t* instantiated, char** states, int64_t count) {
    char* result = strdup("[");
    for (int64_t i = 0; i < count; i++) {
        if (i > 0) result = tinox_string_concat(result, ",");
        char* b = jsonBuilderCreate();
        jsonBuilderAddString(b, "name", names[i]);
        jsonBuilderAddString(b, "scope", scopes[i]);
        jsonBuilderAddBool(b, "instantiated", instantiated[i] != 0);
        jsonBuilderAddRaw(b, "state", states[i]);
        char* obj = jsonBuilderFinish(b);
        result = tinox_string_concat(result, obj);
    }
    return tinox_string_concat(result, "]");
}

// Runs `cmd` via the shell (popen), capturing its combined output (the
// caller redirects stderr itself, e.g. "... 2>&1", same as any
// interactive shell use) into a dynamically-grown buffer, and returns
// {"exitCode":N,"output":"<captured text>"} -- backs the dev-mode
// introspection API's /tests/run endpoint (emit_devui_code, codegen.rs),
// which shells out to `tinox test` in the connected project's own
// directory. `cmd` is always a compiler-generated, compile-time-constant
// string (the project root and the tinox binary's own path, both
// captured at build time -- see main.rs's dev_test_command) -- never
// influenced by request input, so there's no injection surface despite
// this being reachable from an HTTP handler.
char* tinox_run_command_json(const char* cmd) {
    FILE* fp = popen(cmd, "r");
    if (!fp) {
        char* b = jsonBuilderCreate();
        jsonBuilderAddInt(b, "exitCode", -1);
        jsonBuilderAddString(b, "output", "failed to start command");
        return jsonBuilderFinish(b);
    }

    size_t cap = 4096;
    size_t len = 0;
    char* buf = (char*)malloc(cap);
    char chunk[4096];
    size_t n;
    while ((n = fread(chunk, 1, sizeof(chunk), fp)) > 0) {
        if (len + n + 1 > cap) {
            while (len + n + 1 > cap) cap *= 2;
            buf = (char*)realloc(buf, cap);
        }
        memcpy(buf + len, chunk, n);
        len += n;
    }
    buf[len] = '\0';

    int status = pclose(fp);
    int64_t exitCode = -1;
    if (status != -1) {
        if (WIFEXITED(status)) {
            exitCode = WEXITSTATUS(status);
        } else if (WIFSIGNALED(status)) {
            exitCode = 128 + WTERMSIG(status);
        }
    }

    char* b = jsonBuilderCreate();
    jsonBuilderAddInt(b, "exitCode", exitCode);
    jsonBuilderAddString(b, "output", buf);
    return jsonBuilderFinish(b);
}

// ---- fromJson field helpers — avoid two runtime calls per field ----

int64_t jsonGetIntField(int64_t* obj, const char* key) {
    return jsonGetInt(jsonGetField(obj, key));
}

double jsonGetFloatField(int64_t* obj, const char* key) {
    return jsonGetFloat(jsonGetField(obj, key));
}

int jsonGetBoolField(int64_t* obj, const char* key) {
    return (int)jsonGetBool(jsonGetField(obj, key));
}

char* jsonGetStringField(int64_t* obj, const char* key) {
    return jsonGetString(jsonGetField(obj, key));
}

int64_t* jsonGetIntListField(int64_t* obj, const char* key) {
    return jsonIntArrayFromJson(jsonGetField(obj, key));
}

// ---- @JsonSerializable field kinds beyond scalars/int-lists ----
// emit_json_serialize_code/emit_json_deserialize_code (codegen.rs) used to
// route EVERY i64*-typed field through jsonBuilderAddIntList/
// jsonIntArrayFromJson regardless of what it actually was -- correct only
// for List<Int>. A Map<String,String> field (every resource's
// labels/annotations, ConfigMap/Secret's own data) or a List<String>/
// List<@JsonSerializable-class> field silently produced garbage (raw
// pointer values reinterpreted as ints) or crashed. These four helpers,
// paired with the container-marker dispatch codegen.rs now does in both
// directions, are the fix. Mirrors the existing tinox_json_list_serialize
// (List<class>, above) and jsonGetObject (JSON_OBJECT -> TinoxMap*, further
// up) patterns exactly rather than inventing new machinery.

// List<String> -> JSON array of strings. Same "[" + parts + "]" shape as
// tinox_json_list_serialize, just string-encoding each element directly
// instead of calling a per-class toJson.
char* tinox_json_string_list_serialize(int64_t* h) {
    TinoxArray* a = (TinoxArray*)h;
    int64_t n = a ? a->len : 0;
    char** parts = (char**)malloc(sizeof(char*) * (n > 0 ? (size_t)n : 1));
    size_t total = 2; // "[" + "]"
    for (int64_t i = 0; i < n; i++) {
        const char* s = (const char*)(intptr_t)a->data[i];
        parts[i] = tinox_json_encode_string(s ? s : "");
        total += strlen(parts[i]) + 1; // + ","
    }
    char* out = (char*)malloc(total + 1);
    size_t pos = 0;
    out[pos++] = '[';
    for (int64_t i = 0; i < n; i++) {
        if (i > 0) out[pos++] = ',';
        size_t l = strlen(parts[i]);
        memcpy(out + pos, parts[i], l);
        pos += l;
    }
    out[pos++] = ']';
    out[pos] = '\0';
    return out;
}

// Map<String, String> -> JSON object. `map` may be NULL (a never-assigned
// field) -- serializes as "{}", the same "nothing to iterate" convention
// empty/absent collections get elsewhere in this runtime.
char* tinox_json_string_map_serialize(void* map) {
    char* b = jsonBuilderCreate();
    if (map) {
        int64_t* keysHandle = tinox_map_keys(map);
        TinoxArray* ka = (TinoxArray*)keysHandle;
        int64_t n = ka ? ka->len : 0;
        for (int64_t i = 0; i < n; i++) {
            const char* k = (const char*)(intptr_t)ka->data[i];
            const char* v = (const char*)(intptr_t)tinox_map_get(map, k);
            jsonBuilderAddString(b, k, v ? v : "");
        }
    }
    return jsonBuilderFinish(b);
}

// JSON object field -> Map<String, String>. Missing/wrong-typed field ->
// an empty map (jsonGetObject already gives that for free), not a crash --
// same defensive-read convention jsonGetStringField/jsonGetIntListField
// already use for a missing/wrong-typed field.
void* jsonGetStringMapField(int64_t* obj, const char* key) {
    void* srcMap = jsonGetObject(jsonGetField(obj, key));
    void* outMap = tinox_map_create();
    int64_t* keysHandle = tinox_map_keys(srcMap);
    TinoxArray* ka = (TinoxArray*)keysHandle;
    int64_t n = ka ? ka->len : 0;
    for (int64_t i = 0; i < n; i++) {
        const char* k = (const char*)(intptr_t)ka->data[i];
        char* v = jsonGetString((int64_t*)(intptr_t)tinox_map_get(srcMap, k));
        tinox_map_set(outMap, k, (int64_t)(intptr_t)v);
    }
    return outMap;
}

// JSON array field -> List<String>.
int64_t* jsonGetStringListField(int64_t* obj, const char* key) {
    TinoxJsonValue* v = (TinoxJsonValue*)jsonGetField(obj, key);
    int64_t n = (v && v->type == JSON_ARRAY && v->arr_val) ? v->arr_val[-1] : 0;
    TinoxArray* out = (TinoxArray*)GC_malloc(sizeof(TinoxArray));
    out->len = n;
    out->cap = n;
    out->data = n > 0 ? (int64_t*)GC_malloc((size_t)n * sizeof(int64_t)) : NULL;
    for (int64_t i = 0; i < n; i++) {
        TinoxJsonValue* elem = (TinoxJsonValue*)(uintptr_t)v->arr_val[i];
        out->data[i] = (int64_t)(intptr_t)jsonGetString((int64_t*)elem);
    }
    return (int64_t*)out;
}

// JSON array value (NOT a field lookup -- the array TinoxJsonValue* itself,
// same convention as tinox_json_list_serialize taking the List handle
// directly) -> List<T> for a @JsonSerializable T, via T's own generated
// `T_fromJson`. The per-element function pointer mirrors
// tinox_json_list_serialize's `to_json` parameter for the same reason: T
// varies per call site (per field), so this can't be hardcoded to one class.
int64_t* tinox_json_list_deserialize(int64_t* arr_val, int64_t* (*from_json)(int64_t*)) {
    TinoxJsonValue* v = (TinoxJsonValue*)arr_val;
    int64_t n = (v && v->type == JSON_ARRAY && v->arr_val) ? v->arr_val[-1] : 0;
    TinoxArray* out = (TinoxArray*)GC_malloc(sizeof(TinoxArray));
    out->len = n;
    out->cap = n;
    out->data = n > 0 ? (int64_t*)GC_malloc((size_t)n * sizeof(int64_t)) : NULL;
    for (int64_t i = 0; i < n; i++) {
        int64_t* elem = (int64_t*)(uintptr_t)v->arr_val[i];
        out->data[i] = (int64_t)(intptr_t)from_json(elem);
    }
    return (int64_t*)out;
}

// ---- Config (@Config annotation) ----
// Reads key=value pairs from application.properties in the current directory. Values may contain
// ${ENV_VAR} / ${ENV_VAR:default} placeholders, expanded once at load (see tinox_config_expand).

#define TINOX_CONFIG_MAX_ENTRIES 256
#define TINOX_CONFIG_MAX_LINE    1024

typedef struct { char* key; char* value; } TinoxConfigEntry;

static TinoxConfigEntry tinox_config_entries[TINOX_CONFIG_MAX_ENTRIES];
static int              tinox_config_count = -1; // -1 = not loaded

// Expands ${NAME} and ${NAME:default} placeholders in a properties value. NAME is read from the
// process environment; a variable that is set but empty counts as set (explicit empty value). A
// placeholder with no default whose variable is unset, an unterminated or empty `${}`, is a
// configuration error. `\${` yields a literal `${`. Placeholders are not nested and the
// expansion result is never rescanned. Errors name the key and the variable, never a value.
// Returns a heap string (never freed: the entries live for the whole process), or NULL on error.
static char* tinox_config_expand(const char* key, const char* raw) {
    size_t cap = strlen(raw) + 1, len = 0;
    char* out = (char*)malloc(cap);
    if (!out) { fprintf(stderr, "tinox: out of memory expanding configuration\n"); exit(1); }
    for (const char* c = raw; *c; ) {
        const char* piece = NULL;
        size_t plen = 0, adv = 1;
        if (c[0] == '\\' && c[1] == '$' && c[2] == '{') { piece = "${"; plen = 2; adv = 3; }
        else if (c[0] == '$' && c[1] == '{') {
            const char* end = strchr(c + 2, '}');
            if (!end) {
                fprintf(stderr, "tinox: invalid configuration: key '%s' has an unterminated ${...} placeholder\n", key);
                free(out); return NULL;
            }
            const char* colon = memchr(c + 2, ':', (size_t)(end - (c + 2)));
            size_t nlen = (size_t)((colon ? colon : end) - (c + 2));
            char name[256];
            if (nlen == 0 || nlen >= sizeof(name)) {
                fprintf(stderr, "tinox: invalid configuration: key '%s' has a ${...} placeholder with an empty or overlong variable name\n", key);
                free(out); return NULL;
            }
            memcpy(name, c + 2, nlen);
            name[nlen] = '\0';
            const char* v = getenv(name);
            if (v) { piece = v; plen = strlen(v); }
            else if (colon) { piece = colon + 1; plen = (size_t)(end - (colon + 1)); }
            else {
                fprintf(stderr, "tinox: invalid configuration: key '%s' refers to environment variable '%s', which is not set and has no default\n", key, name);
                free(out); return NULL;
            }
            adv = (size_t)(end - c) + 1;
        }
        size_t add = piece ? plen : 1;
        if (len + add + 1 > cap) {
            cap = (len + add + 1) * 2;
            out = (char*)realloc(out, cap);
            if (!out) { fprintf(stderr, "tinox: out of memory expanding configuration\n"); exit(1); }
        }
        if (piece) memcpy(out + len, piece, plen); else out[len] = *c;
        len += add;
        c += adv;
    }
    out[len] = '\0';
    return out;
}

static void tinox_config_load(void) {
    tinox_config_count = 0;
    FILE* f = fopen("application.properties", "r");
    if (!f) return;
    char line[TINOX_CONFIG_MAX_LINE];
    int expand_problems = 0;
    while (fgets(line, sizeof(line), f)) {
        // strip newline
        size_t len = strlen(line);
        while (len > 0 && (line[len-1] == '\n' || line[len-1] == '\r')) line[--len] = '\0';
        // skip empty lines and comments
        if (len == 0 || line[0] == '#' || line[0] == '!') continue;
        char* eq = strchr(line, '=');
        if (!eq) continue;
        *eq = '\0';
        char* key = line;
        char* val = eq + 1;
        // trim trailing whitespace from key
        char* kend = eq - 1;
        while (kend >= key && (*kend == ' ' || *kend == '\t')) *kend-- = '\0';
        // trim leading whitespace from value
        while (*val == ' ' || *val == '\t') val++;
        if (tinox_config_count < TINOX_CONFIG_MAX_ENTRIES) {
            char* expanded = tinox_config_expand(key, val);
            if (!expanded) { expand_problems++; continue; }
            tinox_config_entries[tinox_config_count].key   = strdup(key);
            tinox_config_entries[tinox_config_count].value = expanded;
            tinox_config_count++;
        }
    }
    fclose(f);
    if (expand_problems) {
        fprintf(stderr, "tinox: %d configuration problem(s); refusing to start\n", expand_problems);
        exit(1);
    }
}

// Lookup in the file only (used by limits/telemetry, which read the environment themselves).
static const char* tinox_config_lookup(const char* key) {
    if (tinox_config_count < 0) tinox_config_load();
    for (int i = 0; i < tinox_config_count; i++) {
        if (strcmp(tinox_config_entries[i].key, key) == 0)
            return tinox_config_entries[i].value;
    }
    return "";
}

#define TINOX_CONFIG_MAX_VALUE 4096

// Deterministic precedence for @Config: environment variable (the key upper-cased, every
// non-alphanumeric character turned into `_`: server.port -> SERVER_PORT) over application.properties.
// Returns NULL when the key is configured nowhere. An environment variable that is set but empty
// counts as configured (an explicit empty value), so a deployment can blank a file value.
static const char* tinox_config_resolve(const char* key) {
    char env[256];
    size_t n = 0;
    for (const char* c = key; *c && n < sizeof(env) - 1; c++)
        env[n++] = isalnum((unsigned char)*c) ? (char)toupper((unsigned char)*c) : '_';
    env[n] = '\0';
    const char* v = getenv(env);
    if (v) return v;
    if (tinox_config_count < 0) tinox_config_load();
    for (int i = 0; i < tinox_config_count; i++)
        if (strcmp(tinox_config_entries[i].key, key) == 0) return tinox_config_entries[i].value;
    return NULL;
}

// Configuration errors never echo the offending value: it may be a secret.
static void tinox_config_fail(const char* key, const char* reason) {
    fprintf(stderr, "tinox: invalid configuration: key '%s' %s\n", key, reason);
    exit(1);
}

int64_t tinox_config_present(const char* key) {
    return tinox_config_resolve(key) != NULL;
}

void tinox_config_missing(const char* key) {
    tinox_config_fail(key, "is required but not set (environment variable or application.properties)");
}

// Startup check over every @Config key ("key<TAB>kind<TAB>required\n", kind s/i/b): reports all
// problems together, never echoes values, and exits non-zero before any component starts.
void tinox_config_validate(const char* table) {
    int problems = 0;
    const char* p = table;
    while (p && *p) {
        const char* nl = strchr(p, '\n');
        size_t n = nl ? (size_t)(nl - p) : strlen(p);
        char line[512];
        if (n >= sizeof(line)) n = sizeof(line) - 1;
        memcpy(line, p, n);
        line[n] = '\0';
        p = nl ? nl + 1 : p + n;
        char* t1 = strchr(line, '\t');
        if (!t1) continue;
        *t1 = '\0';
        char kind = t1[1];
        int required = t1[3] == '1';
        const char* v = tinox_config_resolve(line);
        if (!v) {
            if (required) { fprintf(stderr, "tinox: invalid configuration: key '%s' is required but not set (environment variable or application.properties)\n", line); problems++; }
            continue;
        }
        if (strlen(v) > TINOX_CONFIG_MAX_VALUE) { fprintf(stderr, "tinox: invalid configuration: key '%s' exceeds the 4096-byte value limit\n", line); problems++; continue; }
        if (kind == 'i') {
            int64_t out = 0;
            if (!tinox_parse_int_checked(v, &out)) { fprintf(stderr, "tinox: invalid configuration: key '%s' is not a valid integer (empty, trailing characters or out of range)\n", line); problems++; }
        } else if (kind == 'b') {
            if (strcmp(v, "true") && strcmp(v, "1") && strcmp(v, "yes") && strcmp(v, "on") && strcmp(v, "false") && strcmp(v, "0") && strcmp(v, "no") && strcmp(v, "off")) {
                fprintf(stderr, "tinox: invalid configuration: key '%s' is not a valid boolean (true/false/1/0/yes/no/on/off)\n", line); problems++;
            }
        }
    }
    if (problems) {
        fprintf(stderr, "tinox: %d configuration problem(s); refusing to start\n", problems);
        exit(1);
    }
}

char* tinox_config_get(const char* key) {
    const char* v = tinox_config_resolve(key);
    if (!v) return "";
    if (strlen(v) > TINOX_CONFIG_MAX_VALUE) tinox_config_fail(key, "exceeds the 4096-byte value limit");
    return (char*)v;
}

int64_t tinox_config_get_int(const char* key) {
    const char* v = tinox_config_resolve(key);
    if (!v) return 0;
    int64_t out = 0;
    if (!tinox_parse_int_checked(v, &out)) tinox_config_fail(key, "is not a valid integer (empty, trailing characters or out of range)");
    return out;
}

int64_t tinox_config_get_bool(const char* key) {
    const char* v = tinox_config_resolve(key);
    if (!v) return 0;
    if (!strcmp(v, "true") || !strcmp(v, "1") || !strcmp(v, "yes") || !strcmp(v, "on")) return 1;
    if (!strcmp(v, "false") || !strcmp(v, "0") || !strcmp(v, "no") || !strcmp(v, "off")) return 0;
    tinox_config_fail(key, "is not a valid boolean (expected true/false, 1/0, yes/no or on/off)");
    return 0;
}

// Dumps every application.properties key/value as a flat JSON object --
// unlike tinox_config_get*, which only ever looks up a single key a
// @Config field already declared, this backs the dev-mode introspection
// API's /config endpoint (it needs to show what's actually loaded, not
// just what the program happens to read). Values are always emitted as
// JSON strings (application.properties has no type info of its own --
// tinox_config_get_int/_bool parse on demand per @Config field, not at
// load time).
char* tinox_config_dump_json(void) {
    if (tinox_config_count < 0) tinox_config_load();
    char* b = jsonBuilderCreate();
    for (int i = 0; i < tinox_config_count; i++) {
        // Introspection never discloses values of keys that look like credentials.
        const char* k = tinox_config_entries[i].key;
        int secret = 0;
        static const char* const needles[] = {"password", "passwd", "secret", "token", "credential", "apikey", "api_key", "api-key", "private", "key"};
        for (size_t nn = 0; nn < sizeof(needles) / sizeof(needles[0]) && !secret; nn++) {
            size_t len = strlen(needles[nn]);
            for (const char* c = k; *c && !secret; c++)
                if (strncasecmp(c, needles[nn], len) == 0) secret = 1;
        }
        jsonBuilderAddString(b, k, secret ? "***" : tinox_config_entries[i].value);
    }
    return jsonBuilderFinish(b);
}

// ---- CLI argument parsing (@Command / @Option / @Argument) ----

int    _tinox_argc = 0;
char** _tinox_argv = NULL;

// Scans argv for --long-name or -s and returns the following value, or NULL.
char* tinox_cli_get_string(const char* long_name, const char* short_name) {
    for (int i = 1; i < _tinox_argc - 1; i++) {
        if ((long_name  && strcmp(_tinox_argv[i], long_name)  == 0) ||
            (short_name && *short_name && strcmp(_tinox_argv[i], short_name) == 0)) {
            return _tinox_argv[i + 1];
        }
    }
    return NULL;
}

// Returns 1 if the flag is present, 0 otherwise.
int64_t tinox_cli_has_flag(const char* long_name, const char* short_name) {
    for (int i = 1; i < _tinox_argc; i++) {
        if ((long_name  && strcmp(_tinox_argv[i], long_name)  == 0) ||
            (short_name && *short_name && strcmp(_tinox_argv[i], short_name) == 0))
            return 1;
    }
    return 0;
}

// Returns integer value after --long-name/-s, or default_val if absent.
int64_t tinox_cli_get_int(const char* long_name, const char* short_name, int64_t default_val) {
    char* s = tinox_cli_get_string(long_name, short_name);
    if (!s) return default_val;
    return (int64_t)atoll(s);
}

// Returns the positional argument at position `index` (0-based, skipping option tokens).
char* tinox_cli_get_positional(int32_t index) {
    int pos = 0;
    int i = 1;
    while (i < _tinox_argc) {
        char* arg = _tinox_argv[i];
        if (arg[0] == '-') {
            // skip option token; if next token is not a flag treat it as the value
            if (i + 1 < _tinox_argc && _tinox_argv[i + 1][0] != '-')
                i += 2;
            else
                i += 1;
        } else {
            if (pos == index) return arg;
            pos++;
            i++;
        }
    }
    return NULL;
}

// Prints a single help line "  -s, --long-name   description"
void tinox_cli_print_option(const char* names, const char* description) {
    printf("  %-22s  %s\n", names, description ? description : "");
}

// ---- Metrics ----

#define TINOX_MAX_METRICS 512

typedef struct {
    char   name[256];
    int64_t value;
} TinoxCounter;

// Fixed, mergeable latency buckets (upper bounds in nanoseconds), Prometheus `le` style. The
// resolution is the bucket width: a quantile derived with histogram_quantile() is only known to
// lie inside its bucket (e.g. a p99 between 100 ms and 250 ms is reported by interpolation).
#define TINOX_HIST_BUCKETS 14
static const int64_t TINOX_HIST_BOUNDS_NS[TINOX_HIST_BUCKETS] = {
    500000LL, 1000000LL, 2500000LL, 5000000LL, 10000000LL, 25000000LL, 50000000LL,
    100000000LL, 250000000LL, 500000000LL, 1000000000LL, 2500000000LL, 5000000000LL, 10000000000LL,
};
static const char* const TINOX_HIST_LE[TINOX_HIST_BUCKETS] = {
    "0.0005", "0.001", "0.0025", "0.005", "0.01", "0.025", "0.05",
    "0.1", "0.25", "0.5", "1", "2.5", "5", "10",
};

typedef struct {
    char    name[256];
    int64_t count;
    int64_t sum_ns;
    int64_t min_ns;
    int64_t max_ns;
    int64_t buckets[TINOX_HIST_BUCKETS]; // non-cumulative; values above the last bound only in count
} TinoxHistogram;

static void tinox_hist_observe_bucket(TinoxHistogram* h, int64_t duration_ns) {
    for (int b = 0; b < TINOX_HIST_BUCKETS; b++) {
        if (duration_ns <= TINOX_HIST_BOUNDS_NS[b]) { h->buckets[b]++; return; }
    }
}

typedef struct {
    char    name[256];
    int64_t value;
} TinoxGauge;

static TinoxCounter   _tinox_counters[TINOX_MAX_METRICS];
static TinoxHistogram _tinox_histograms[TINOX_MAX_METRICS];
static TinoxGauge     _tinox_gauges[TINOX_MAX_METRICS];
static int _tinox_counter_n   = 0;
static int _tinox_histogram_n = 0;
static int _tinox_gauge_n     = 0;
static pthread_mutex_t _tinox_metrics_mu = PTHREAD_MUTEX_INITIALIZER;

int64_t tinox_clock_nanos(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000000000LL + ts.tv_nsec;
}

void tinox_counter_inc(const char* name) {
    pthread_mutex_lock(&_tinox_metrics_mu);
    for (int i = 0; i < _tinox_counter_n; i++) {
        if (strcmp(_tinox_counters[i].name, name) == 0) {
            _tinox_counters[i].value++;
            pthread_mutex_unlock(&_tinox_metrics_mu);
            return;
        }
    }
    if (_tinox_counter_n < TINOX_MAX_METRICS) {
        strncpy(_tinox_counters[_tinox_counter_n].name, name, 255);
        _tinox_counters[_tinox_counter_n].name[255] = '\0';
        _tinox_counters[_tinox_counter_n].value = 1;
        _tinox_counter_n++;
    }
    pthread_mutex_unlock(&_tinox_metrics_mu);
}

void tinox_histogram_record(const char* name, int64_t duration_ns) {
    pthread_mutex_lock(&_tinox_metrics_mu);
    for (int i = 0; i < _tinox_histogram_n; i++) {
        if (strcmp(_tinox_histograms[i].name, name) == 0) {
            _tinox_histograms[i].count++;
            tinox_hist_observe_bucket(&_tinox_histograms[i], duration_ns);
            _tinox_histograms[i].sum_ns += duration_ns;
            if (duration_ns < _tinox_histograms[i].min_ns) _tinox_histograms[i].min_ns = duration_ns;
            if (duration_ns > _tinox_histograms[i].max_ns) _tinox_histograms[i].max_ns = duration_ns;
            pthread_mutex_unlock(&_tinox_metrics_mu);
            return;
        }
    }
    if (_tinox_histogram_n < TINOX_MAX_METRICS) {
        strncpy(_tinox_histograms[_tinox_histogram_n].name, name, 255);
        _tinox_histograms[_tinox_histogram_n].name[255] = '\0';
        _tinox_histograms[_tinox_histogram_n].count   = 1;
        _tinox_histograms[_tinox_histogram_n].sum_ns  = duration_ns;
        _tinox_histograms[_tinox_histogram_n].min_ns  = duration_ns;
        _tinox_histograms[_tinox_histogram_n].max_ns  = duration_ns;
        memset(_tinox_histograms[_tinox_histogram_n].buckets, 0, sizeof(_tinox_histograms[_tinox_histogram_n].buckets));
        tinox_hist_observe_bucket(&_tinox_histograms[_tinox_histogram_n], duration_ns);
        _tinox_histogram_n++;
    }
    pthread_mutex_unlock(&_tinox_metrics_mu);
}

void tinox_gauge_set(const char* name, int64_t value) {
    pthread_mutex_lock(&_tinox_metrics_mu);
    for (int i = 0; i < _tinox_gauge_n; i++) {
        if (strcmp(_tinox_gauges[i].name, name) == 0) {
            _tinox_gauges[i].value = value;
            pthread_mutex_unlock(&_tinox_metrics_mu);
            return;
        }
    }
    if (_tinox_gauge_n < TINOX_MAX_METRICS) {
        strncpy(_tinox_gauges[_tinox_gauge_n].name, name, 255);
        _tinox_gauges[_tinox_gauge_n].name[255] = '\0';
        _tinox_gauges[_tinox_gauge_n].value = value;
        _tinox_gauge_n++;
    }
    pthread_mutex_unlock(&_tinox_metrics_mu);
}

// Returns a heap-allocated Prometheus-format string; caller need not free (GC-managed).
// Bug 99: appends `text` (of length `n`, as returned by snprintf -- the
// would-be length, NOT necessarily what actually fit) to `*pos`, clamped to
// `cap`. snprintf's return value can exceed the space it was given
// (`cap - *pos`) when the formatted text doesn't fit -- metric names are
// caller-controlled (up to 255 bytes each) and a histogram line repeats the
// name 5 times, easily exceeding the old flat 512-byte-per-entry estimate.
// The old code did `pos += (size_t)snprintf(...)` unconditionally: once pos
// overshot cap this way, the next call's `cap - pos` (both size_t) underflowed
// to a huge value, and `buf + pos` could already be past the allocation --
// a heap out-of-bounds write. Clamping pos to cap after every append makes
// every subsequent `cap - pos` well-defined (falls back to a safe,
// no-op-but-correct snprintf(..., 0, ...) once full) at the cost of
// silently truncating the output if the buffer genuinely runs out.
// ---- Resilience: @Retry / @CircuitBreaker support (#527) ------------------------------------------
// The compiler wraps annotated methods in a loop that calls these. State is per call site
// ("Class.method", a compile-time constant), so metrics labels are bounded by the program text.
#define TINOX_RES_SITES 256
typedef struct {
    int     used;
    char    site[96];
    int     state;            // 0 closed, 1 open, 2 half-open
    int64_t failures;         // consecutive eligible failures while closed
    int64_t opened_at_ms;
    int64_t inflight_probes;
    int64_t retries, rejections, opened_total, calls_failed, calls_ok;
} TinoxResSite;
static TinoxResSite g_res[TINOX_RES_SITES];
static pthread_mutex_t g_res_mu = PTHREAD_MUTEX_INITIALIZER;

static TinoxResSite* res_site(const char* name) {
    TinoxResSite* free_slot = NULL;
    for (int i = 0; i < TINOX_RES_SITES; i++) {
        if (g_res[i].used) { if (strcmp(g_res[i].site, name) == 0) return &g_res[i]; }
        else if (!free_slot) free_slot = &g_res[i];
    }
    if (!free_slot) { fprintf(stderr, "tinox: too many @Retry/@CircuitBreaker sites (max %d)\n", TINOX_RES_SITES); exit(1); }
    memset(free_slot, 0, sizeof(*free_slot));
    free_slot->used = 1;
    snprintf(free_slot->site, sizeof(free_slot->site), "%s", name);
    return free_slot;
}

static int res_is_control(const char* err) {
    return err && (strncmp(err, "Task cancelled", 14) == 0 || strncmp(err, "Task deadline exceeded", 22) == 0 ||
                   strncmp(err, "Task service shutdown", 21) == 0);
}

// `eligible` = "A|B|C" error prefixes, empty = every failure.
static int res_eligible(const char* err, const char* eligible) {
    if (!eligible || !*eligible) return 1;
    if (!err) return 0;
    const char* p = eligible;
    while (*p) {
        const char* e = strchr(p, '|');
        size_t n = e ? (size_t)(e - p) : strlen(p);
        if (n > 0 && strncmp(err, p, n) == 0) return 1;
        if (!e) break;
        p = e + 1;
    }
    return 0;
}

int64_t resilienceNow(void) { return http_clock_ms(); }

// Breaker admission. 0 = rejected (circuit open), 1 = normal call, 2 = half-open probe.
int64_t resilienceAllow(const char* site, int64_t threshold, int64_t open_ms, int64_t probes, const char* eligible) {
    (void)threshold; (void)eligible;
    pthread_mutex_lock(&g_res_mu);
    TinoxResSite* s = res_site(site);
    int64_t verdict = 1;
    int64_t now = http_clock_ms();
    if (s->state == 1 && now - s->opened_at_ms >= open_ms) { s->state = 2; s->inflight_probes = 0; }
    if (s->state == 1) { s->rejections++; verdict = 0; }
    else if (s->state == 2) {
        if (s->inflight_probes < probes) { s->inflight_probes++; verdict = 2; }
        else { s->rejections++; verdict = 0; }
    }
    pthread_mutex_unlock(&g_res_mu);
    return verdict;
}

void resilienceRecord(const char* site, int64_t ticket, int64_t failed, const char* err, int64_t threshold, const char* eligible) {
    pthread_mutex_lock(&g_res_mu);
    TinoxResSite* s = res_site(site);
    if (ticket == 2 && s->inflight_probes > 0) s->inflight_probes--;
    if (failed && res_is_control(err)) { pthread_mutex_unlock(&g_res_mu); return; } // cancellation says nothing about the service
    int bad = failed && res_eligible(err, eligible);
    if (bad) s->calls_failed++; else s->calls_ok++;
    if (ticket == 2) {
        if (bad) { s->state = 1; s->opened_at_ms = http_clock_ms(); s->opened_total++; s->failures = 0; }
        else if (s->state == 2) { s->state = 0; s->failures = 0; s->inflight_probes = 0; }
    } else if (s->state == 0) {
        if (!bad) s->failures = 0;
        else if (++s->failures >= threshold) { s->state = 1; s->opened_at_ms = http_clock_ms(); s->opened_total++; s->failures = 0; }
    }
    pthread_mutex_unlock(&g_res_mu);
}

static uint64_t res_rand(void) {
    static __thread uint64_t x = 0;
    if (!x) x = (uint64_t)http_clock_ms() * 0x9E3779B97F4A7C15ULL ^ (uint64_t)(uintptr_t)&x;
    x ^= x << 13; x ^= x >> 7; x ^= x << 17;
    return x;
}

// Decides whether to run another attempt and, if so, waits out the backoff (cancellable).
// `attempt` = attempts made so far. 1 = retry now, 0 = give up (caller rethrows the error).
int64_t resilienceRetry(const char* site, int64_t attempt, const char* err, const char* eligible,
                        int64_t max_attempts, int64_t backoff_ms, int64_t max_backoff_ms,
                        int64_t jitter_pct, int64_t budget_ms, int64_t start_ms) {
    if (res_is_control(err) || (err && strncmp(err, "CIRCUIT_OPEN", 12) == 0)) return 0;
    if (!res_eligible(err, eligible)) return 0;
    if (attempt >= max_attempts) return 0;
    int64_t wait = backoff_ms;
    for (int64_t i = 1; i < attempt && wait < max_backoff_ms; i++) wait *= 2;
    if (max_backoff_ms > 0 && wait > max_backoff_ms) wait = max_backoff_ms;
    if (jitter_pct > 0 && wait > 0) {
        int64_t span = wait * jitter_pct / 100;
        if (span > 0) wait += (int64_t)(res_rand() % (uint64_t)(2 * span + 1)) - span;
        if (wait < 0) wait = 0;
    }
    int64_t now = http_clock_ms();
    if (budget_ms > 0 && now - start_ms + wait >= budget_ms) return 0;
    int64_t until = now + wait;
    while (http_clock_ms() < until) {
        if (task_cancellation(task_scope)) return 0;
        int64_t left = until - http_clock_ms();
        struct timespec ts = { .tv_sec = 0, .tv_nsec = (left > 20 ? 20 : left) * 1000000L };
        nanosleep(&ts, NULL);
    }
    if (task_cancellation(task_scope)) return 0;
    pthread_mutex_lock(&g_res_mu);
    res_site(site)->retries++;
    pthread_mutex_unlock(&g_res_mu);
    return 1;
}

static int tinox_resilience_metrics_text(char* buf, size_t cap) {
    size_t pos = 0;
    int any = 0;
    pthread_mutex_lock(&g_res_mu);
    for (int i = 0; i < TINOX_RES_SITES && pos + 1024 < cap; i++) {
        TinoxResSite* s = &g_res[i];
        if (!s->used) continue;
        if (!any) {
            pos += (size_t)snprintf(buf + pos, cap - pos,
                "# TYPE tinox_resilience_retries_total counter\n# TYPE tinox_resilience_rejections_total counter\n"
                "# TYPE tinox_resilience_breaker_opened_total counter\n# TYPE tinox_resilience_breaker_state gauge\n"
                "# TYPE tinox_resilience_calls_failed_total counter\n# TYPE tinox_resilience_calls_ok_total counter\n");
            any = 1;
        }
        pos += (size_t)snprintf(buf + pos, cap - pos,
            "tinox_resilience_retries_total{site=\"%s\"} %lld\ntinox_resilience_rejections_total{site=\"%s\"} %lld\n"
            "tinox_resilience_breaker_opened_total{site=\"%s\"} %lld\ntinox_resilience_breaker_state{site=\"%s\"} %d\n"
            "tinox_resilience_calls_failed_total{site=\"%s\"} %lld\ntinox_resilience_calls_ok_total{site=\"%s\"} %lld\n",
            s->site, (long long)s->retries, s->site, (long long)s->rejections, s->site, (long long)s->opened_total,
            s->site, s->state, s->site, (long long)s->calls_failed, s->site, (long long)s->calls_ok);
    }
    pthread_mutex_unlock(&g_res_mu);
    return (int)pos;
}

static void tinox_metrics_append(size_t* pos, size_t cap, int n) {
    if (n < 0) return;
    *pos += (size_t)n;
    if (*pos > cap) *pos = cap;
}

char* tinox_metrics_prometheus(void) {
    pthread_mutex_lock(&_tinox_metrics_mu);
    // Upper bound per entry: histogram lines repeat a (up to 255-byte)
    // name 5 times plus ~200 bytes of fixed text -- comfortably under 2048.
    size_t cap = (size_t)(_tinox_counter_n + _tinox_gauge_n + 1) * 2048 + (size_t)_tinox_histogram_n * 8192 + 4096 + 64 + (size_t)TINOX_RES_SITES * 1024;
    char* buf = (char*)GC_malloc(cap);
    size_t pos = 0;

    for (int i = 0; i < _tinox_counter_n; i++) {
        int n = snprintf(buf + pos, cap - pos,
            "# TYPE %s_total counter\n%s_total %lld\n",
            _tinox_counters[i].name, _tinox_counters[i].name,
            (long long)_tinox_counters[i].value);
        tinox_metrics_append(&pos, cap, n);
    }
    for (int i = 0; i < _tinox_histogram_n; i++) {
        double sum_s   = (double)_tinox_histograms[i].sum_ns / 1e9;
        double min_s   = (double)_tinox_histograms[i].min_ns / 1e9;
        double max_s   = (double)_tinox_histograms[i].max_ns / 1e9;
        int64_t count  = _tinox_histograms[i].count;
        const char* n  = _tinox_histograms[i].name;
        // A real Prometheus histogram. `_count`/`_sum` keep the names they had as a summary, so
        // existing rate()/average queries are unaffected; `_min`/`_max` stay as gauges.
        int written = snprintf(buf + pos, cap - pos, "# TYPE %s_duration_seconds histogram\n", n);
        tinox_metrics_append(&pos, cap, written);
        int64_t cumulative = 0;
        for (int b = 0; b < TINOX_HIST_BUCKETS; b++) {
            cumulative += _tinox_histograms[i].buckets[b];
            written = snprintf(buf + pos, cap - pos, "%s_duration_seconds_bucket{le=\"%s\"} %lld\n",
                n, TINOX_HIST_LE[b], (long long)cumulative);
            tinox_metrics_append(&pos, cap, written);
        }
        written = snprintf(buf + pos, cap - pos,
            "%s_duration_seconds_bucket{le=\"+Inf\"} %lld\n"
            "%s_duration_seconds_count %lld\n"
            "%s_duration_seconds_sum %.9f\n"
            "# TYPE %s_duration_seconds_min gauge\n%s_duration_seconds_min %.9f\n"
            "# TYPE %s_duration_seconds_max gauge\n%s_duration_seconds_max %.9f\n",
            n, (long long)count, n, (long long)count, n, sum_s, n, n, min_s, n, n, max_s);
        tinox_metrics_append(&pos, cap, written);
    }
    for (int i = 0; i < _tinox_gauge_n; i++) {
        int n = snprintf(buf + pos, cap - pos,
            "# TYPE %s gauge\n%s %lld\n",
            _tinox_gauges[i].name, _tinox_gauges[i].name,
            (long long)_tinox_gauges[i].value);
        tinox_metrics_append(&pos, cap, n);
    }
    tinox_metrics_append(&pos, cap, tinox_limits_metrics_text(buf + pos, cap - pos));
    tinox_metrics_append(&pos, cap, tinox_resilience_metrics_text(buf + pos, cap - pos));
    pthread_mutex_unlock(&_tinox_metrics_mu);
    return buf;
}

// Copy native diagnostics while the driver still owns them. Never replace an
// exception already being unwound (notably when ROLLBACK also loses its connection).
static void tinox_db_statement_error(const char* detail) {
    if (!__tinox_err) {
        __tinox_err = (int64_t)(intptr_t)GC_strdup(detail && *detail ? detail : "database statement failed");
    }
}

// ---- Database / ORM runtime ----
// Compiled only when libpq is available (postgres driver).
// SQLite and MySQL variants follow the same interface.

#if defined(TINOX_DB_POSTGRES) || defined(TINOX_DB_SQLITE) || defined(TINOX_DB_MYSQL)
// Upper bound for waiting on a database lock held by another transaction (SQLite).
static int64_t _tinox_db_lock_timeout_ms(void) {
    const char* v = getenv("TINOX_DB_LOCK_TIMEOUT_MS");
    if (v && *v) {
        char* end = NULL;
        long long n = strtoll(v, &end, 10);
        if (*end || n <= 0) { fprintf(stderr, "TINOX_DB_LOCK_TIMEOUT_MS must be a positive integer, got '%s'\n", v); exit(1); }
        return n;
    }
    return 30000;
}
#endif

#if defined(TINOX_DB_POSTGRES) || defined(TINOX_DB_SQLITE) || defined(TINOX_DB_MYSQL)
// The connection owned by this thread's active transaction frame (NULL outside one).
static __thread void* _tinox_db_tx_conn = NULL;
static void* _tinox_drv_tx_conn_acquire(void);
static void _tinox_drv_tx_conn_release(void* c);
static int _tinox_drv_exec_simple(void* c, const char* sql);
static const char* _tinox_drv_begin_sql(void);
static const char* _tinox_drv_requires_new_problem(void);

// ---- Transactions: propagation modes and savepoints (issue #532) ----
// Every @Transactional method brackets its body with tinox_db_tx_enter(mode) /
// tinox_db_tx_leave(token, ok). The token says what THIS call did, so leave()
// undoes exactly that:
//   TX_JOINED (0)  REQUIRED inside an open transaction: nothing to undo; a failure
//                  only marks the shared transaction rollback-only
//   TX_OWNER  (1)  began the transaction (and owns its pooled connection)
//   TX_SAVEPT (2)  NESTED inside an open transaction: a savepoint
//   TX_NEWTX  (3)  REQUIRES_NEW inside an open transaction: the outer transaction is
//                  suspended (its frame stays on the stack) and a second connection runs
//                  an independent transaction
//   -1             already left (the early-return path leaves before the catch path runs)
// Modes: 0 REQUIRED, 1 NESTED, 2 REQUIRES_NEW. A joined call that fails marks the frame
// rollback-only; the owner then refuses to commit (leave() returns 0 and sets the thrown
// error) instead of silently committing half of a failed unit of work.
#define TINOX_TX_MAX_DEPTH 8
typedef struct { void* conn; int sp; int rollback_only; } TinoxTxFrame;
static __thread TinoxTxFrame _tinox_tx_frames[TINOX_TX_MAX_DEPTH];
static __thread int _tinox_tx_n = 0;

static int _tinox_tx_exec(void* c, const char* sql) {
    return _tinox_drv_exec_simple(c, sql);
}

static void _tinox_tx_sync_current(void) {
    _tinox_db_tx_conn = _tinox_tx_n > 0 ? _tinox_tx_frames[_tinox_tx_n - 1].conn : NULL;
}

static int64_t _tinox_tx_push(void) {
    if (_tinox_tx_n >= TINOX_TX_MAX_DEPTH) {
        fprintf(stderr, "transaction nesting too deep (max %d REQUIRES_NEW levels)\n", TINOX_TX_MAX_DEPTH);
        exit(1);
    }
    void* c = _tinox_drv_tx_conn_acquire();
    if (!c) return -1;
    if (!_tinox_tx_exec(c, _tinox_drv_begin_sql())) {
        _tinox_drv_tx_conn_release(c);
        return -1;
    }
    _tinox_tx_frames[_tinox_tx_n].conn = c;
    _tinox_tx_frames[_tinox_tx_n].sp = 0;
    _tinox_tx_frames[_tinox_tx_n].rollback_only = 0;
    _tinox_tx_n++;
    _tinox_tx_sync_current();
    return 0;
}

int64_t tinox_db_tx_enter(int64_t mode) {
    if (_tinox_tx_n == 0) return _tinox_tx_push() < 0 ? -1 : 1;
    TinoxTxFrame* f = &_tinox_tx_frames[_tinox_tx_n - 1];
    if (mode == 1) {
        char sql[48];
        snprintf(sql, sizeof sql, "SAVEPOINT tinox_sp%d", f->sp + 1);
        if (!_tinox_tx_exec(f->conn, sql)) return -1;
        f->sp++;
        return 2;
    }
    if (mode == 2) {
        // The suspended transaction keeps its connection, so a pool of one could never
        // serve the second one: wait forever. Fail loudly instead.
        const char* problem = _tinox_drv_requires_new_problem();
        if (problem) { fprintf(stderr, "%s\n", problem); exit(1); }
        return _tinox_tx_push() < 0 ? -1 : 3;
    }
    return 0;
}

// enter returns the token (see above), or -1 with the error set when no connection could be
// obtained (the caller throws before running the body).
// leave returns 1 on success. Returns 0 (with the error set for the caller to throw) when the
// transaction had to be rolled back although the body completed normally.
int64_t tinox_db_tx_leave(int64_t token, bool ok) {
    if (token < 0 || _tinox_tx_n == 0) return 1;
    TinoxTxFrame* f = &_tinox_tx_frames[_tinox_tx_n - 1];
    if (token == 0) {
        if (!ok) f->rollback_only = 1;
        return 1;
    }
    if (token == 2) {
        char sql[96];
        if (!ok) {
            snprintf(sql, sizeof sql, "ROLLBACK TO SAVEPOINT tinox_sp%d", f->sp);
            _tinox_tx_exec(f->conn, sql);
        }
        snprintf(sql, sizeof sql, "RELEASE SAVEPOINT tinox_sp%d", f->sp);
        int released = _tinox_tx_exec(f->conn, sql);
        f->sp--;
        return released;
    }
    int refused = ok && f->rollback_only;
    int completed = _tinox_tx_exec(f->conn, (ok && !refused) ? "COMMIT" : "ROLLBACK");
    _tinox_drv_tx_conn_release(f->conn);
    _tinox_tx_n--;
    _tinox_tx_sync_current();
    if (refused && !__tinox_err) {
        __tinox_err = (int64_t)(intptr_t)"transaction rolled back: a joined call of the same transaction failed (rollback-only); handle the error or use NESTED/REQUIRES_NEW";
        return 0;
    }
    return completed && !refused;
}

bool tinox_db_tx_active(void) {
    return _tinox_tx_n > 0;
}

#endif

#ifdef TINOX_DB_POSTGRES
#include <libpq-fe.h>

// Connection pool (issue #191): a fixed-size array of PGconn*, all
// connected eagerly at startup, checked out exclusively per statement (or
// for the duration of an @Transactional method) and returned when done.
// Fixed-size and eagerly connected rather than grow-on-demand -- keeps
// acquire/release wait-free once warm, and gives a hard, visible failure
// at startup if the configured pool can't actually be established instead
// of a lazy first query silently discovering a broken DB much later.
#define TINOX_DB_POOL_MAX 64

static PGconn* _tinox_pg_pool[TINOX_DB_POOL_MAX];
static int64_t _tinox_pg_pool_size = 0;
static int     _tinox_pg_pool_free[TINOX_DB_POOL_MAX];
static int     _tinox_pg_pool_free_count = 0;
static pthread_mutex_t _tinox_pg_pool_mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t  _tinox_pg_pool_cond = PTHREAD_COND_INITIALIZER;

// The connection currently owned by this thread's active @Transactional
// method, if any -- NULL outside a transaction. Deliberately a plain
// __thread PGconn* (a foreign, non-GC-heap pointer allocated by libpq's
// own malloc), not something needing tinox_gc_register_thread_roots: the
// GC-root-scanning gap documented near the top of this file only matters
// for GC-managed pointers reachable ONLY via __thread storage, and a
// PGconn* is never GC memory.

void tinox_db_pool_init(const char* url, int64_t pool_size) {
    const char* diff_mode = getenv("TINOX_MIGRATE_MODE");
    if (diff_mode && strcmp(diff_mode, "diff-shadow") == 0) {
#if defined(TINOX_DB_SQLITE)
        url = ":memory:";
#else
        url = getenv("TINOX_DB_DIFF_URL");
        if (!url || !*url) { fprintf(stderr, "diff: TINOX_DB_DIFF_URL must name an empty disposable shadow database\n"); exit(1); }
#endif
    }

    if (pool_size <= 0) pool_size = 1;
    if (pool_size > TINOX_DB_POOL_MAX) {
        fprintf(stderr, "DB pool size %lld exceeds maximum of %d\n",
            (long long)pool_size, TINOX_DB_POOL_MAX);
        exit(1);
    }
    for (int64_t i = 0; i < pool_size; i++) {
        PGconn* c = PQconnectdb(url);
        if (PQstatus(c) != CONNECTION_OK) {
            fprintf(stderr, "DB connection failed: %s\n", PQerrorMessage(c));
            PQfinish(c);
            if (service_enabled()) { service_record_error("PostgreSQL initialization failed", 0); return; }
            exit(1);
        }
        _tinox_pg_pool[i] = c;
        _tinox_pg_pool_free[i] = (int)i;
    }
    _tinox_pg_pool_size = pool_size;
    _tinox_pg_pool_free_count = (int)pool_size;
}

static PGconn* _tinox_pg_pool_acquire(void) {
    pthread_mutex_lock(&_tinox_pg_pool_mu);
    while (_tinox_pg_pool_free_count == 0) {
        pthread_cond_wait(&_tinox_pg_pool_cond, &_tinox_pg_pool_mu);
    }
    PGconn* c = _tinox_pg_pool[_tinox_pg_pool_free[--_tinox_pg_pool_free_count]];
    pthread_mutex_unlock(&_tinox_pg_pool_mu);
    // A server restart or network cut leaves the pooled connection dead. The failing statement already reported the
    // error to its caller; reconnect here (the connection is exclusively ours until released) so the pool recovers
    // instead of failing every later statement forever. If the server is still down the statement fails visibly again.
    if (PQstatus(c) != CONNECTION_OK) PQreset(c);
    return c;
}

static void _tinox_pg_pool_release(PGconn* conn) {
    pthread_mutex_lock(&_tinox_pg_pool_mu);
    for (int64_t i = 0; i < _tinox_pg_pool_size; i++) {
        if (_tinox_pg_pool[i] == conn) {
            _tinox_pg_pool_free[_tinox_pg_pool_free_count++] = (int)i;
            break;
        }
    }
    pthread_mutex_unlock(&_tinox_pg_pool_mu);
    pthread_cond_signal(&_tinox_pg_pool_cond);
}

// Connection to use for a single ORM statement: the active transaction's
// connection if this thread is inside one, otherwise a freshly checked-out
// pool connection (which the caller must pair with
// tinox_db_release_stmt_conn once the statement is done).
void* tinox_db_acquire_stmt_conn(void) {
    if (_tinox_db_tx_conn != NULL) return _tinox_db_tx_conn;
    return _tinox_pg_pool_acquire();
}

// No-op if conn is the thread's active transaction connection (still owned
// by the transaction, released by tinox_db_tx_commit/_rollback instead);
// otherwise returns it to the pool.
void tinox_db_release_stmt_conn(void* conn) {
    if (conn == (void*)_tinox_db_tx_conn) return;
    _tinox_pg_pool_release((PGconn*)conn);
}

// Driver hooks for the shared transaction code (see tinox_db_tx_enter above).
static void* _tinox_drv_tx_conn_acquire(void) { return _tinox_pg_pool_acquire(); }
static void _tinox_drv_tx_conn_release(void* c) { _tinox_pg_pool_release((PGconn*)c); }
static int _tinox_drv_exec_simple(void* c, const char* sql) {
    PGresult* res = PQexec((PGconn*)c, sql);
    int ok = PQresultStatus(res) == PGRES_COMMAND_OK;
    if (!ok) tinox_db_statement_error(PQerrorMessage((PGconn*)c));
    // PostgreSQL answers COMMIT in an aborted transaction with a successful
    // ROLLBACK command tag. Report that outcome rather than claiming a commit.
    if (ok && strcmp(sql, "COMMIT") == 0 && strcmp(PQcmdStatus(res), "ROLLBACK") == 0) {
        tinox_db_statement_error("transaction rolled back after a failed statement");
        ok = 0;
    }
    PQclear(res);
    return ok;
}
static const char* _tinox_drv_begin_sql(void) { return "BEGIN"; }
static const char* _tinox_drv_requires_new_problem(void) {
    return _tinox_pg_pool_size < 2 ? "REQUIRES_NEW needs [database] pool >= 2 (the outer transaction keeps its connection)" : NULL;
}

void* tinox_db_exec(void* conn, const char* sql, const char** params, int64_t n_params) {
    // Bug 103 (fixed by locking a mutex around this call) no longer
    // applies: that bug existed because every thread shared the SAME
    // single PGconn. Since issue #191's connection pool, each conn handed
    // out by tinox_db_acquire_stmt_conn/tinox_db_tx_begin is exclusively
    // owned by exactly one thread until it's released/committed/rolled
    // back -- no mutex is needed here anymore, and keeping one would have
    // served no purpose beyond re-serializing the pool it was meant to
    // parallelize.
    PGresult* res = PQexecParams(
        (PGconn*)conn, sql,
        (int)n_params, NULL,
        params, NULL, NULL, 0
    );
    ExecStatusType status = PQresultStatus(res);
    if (status != PGRES_TUPLES_OK && status != PGRES_COMMAND_OK) {
        tinox_db_statement_error(res ? PQresultErrorMessage(res) : PQerrorMessage((PGconn*)conn));
    }
    return (void*)res;
}

int64_t tinox_db_nrows(void* result) { return (int64_t)PQntuples((PGresult*)result); }
int64_t tinox_db_ncols(void* result) { return (int64_t)PQnfields((PGresult*)result); }

char* tinox_db_getval(void* result, int64_t row, int64_t col) {
    return GC_strdup(PQgetvalue((PGresult*)result, (int)row, (int)col));
}

int64_t tinox_db_getval_int(void* result, int64_t row, int64_t col) {
    char* v = PQgetvalue((PGresult*)result, (int)row, (int)col);
    if (!v) return 0LL;
    // PostgreSQL sends boolean columns in text format as 't' / 'f'; Bool entity fields are read through here.
    if (v[0] == 't') return 1LL;
    if (v[0] == 'f') return 0LL;
    return (int64_t)atoll(v);
}

bool tinox_db_is_null(void* result, int64_t row, int64_t col) {
    return (bool)PQgetisnull((PGresult*)result, (int)row, (int)col);
}

void tinox_db_free(void* result) { PQclear((PGresult*)result); }

bool tinox_db_result_failed(void* result) {
    if (!result) return true;
    ExecStatusType st = PQresultStatus((PGresult*)result);
    return st != PGRES_TUPLES_OK && st != PGRES_COMMAND_OK;
}

char* tinox_db_error(void* conn) {
    return GC_strdup(PQerrorMessage((PGconn*)conn));
}

// DB.raw()'s row marshaling. Unlike an @Entity query, an arbitrary raw SQL
// result has no static column schema for the codegen to generate a
// per-class `_fromRow` function from, so (unlike emit_entity_from_row) the
// whole `List<Map<String,String>>` result is built here in one C helper
// using the existing Map/Array primitives, keyed by PQfname's column names.
// A NULL column is simply omitted from its row's Map (sparse) rather than
// given a sentinel value -- Tinox code checks `.contains(col)`. Per-row
// TinoxMap*s are stored as plain int64 payloads in a GC-managed TinoxArray,
// same as every other Tinox List<T> of boxed values (e.g. gen_orm_query's
// own row loop storing entity-object pointers) -- storing a non-GC (malloc'd)
// TinoxMap* pointer inside a GC-scanned block is safe (Boehm's conservative
// scan never collects non-GC memory regardless of what points to it), and
// matches every other Map<K,V> value's existing memory-management story
// (tinox_map_create is a plain malloc, not GC_malloc, same as any other
// Tinox map literal already relies on).
void* tinox_db_result_to_row_maps(void* result) {
    int64_t nrows = tinox_db_nrows(result);
    int64_t ncols = tinox_db_ncols(result);
    int64_t* rows = tinox_array_new(nrows, 0);
    int64_t* data = ((TinoxArray*)rows)->data;
    for (int64_t r = 0; r < nrows; r++) {
        void* row_map = tinox_map_create();
        for (int64_t c = 0; c < ncols; c++) {
            if (tinox_db_is_null(result, r, c)) continue;
            char* col_name = PQfname((PGresult*)result, (int)c);
            char* val = GC_strdup(PQgetvalue((PGresult*)result, (int)r, (int)c));
            tinox_map_set(row_map, col_name, (int64_t)(intptr_t)val);
        }
        data[r] = (int64_t)(intptr_t)row_map;
    }
    return (void*)rows;
}

#elif defined(TINOX_DB_SQLITE)

// ---- SQLite driver ----
#include <sqlite3.h>

static sqlite3* _tinox_sqlite_db = NULL;

typedef struct {
    int n_cols;
    int n_rows;
    char** data;  // row-major: data[row * n_cols + col]
    char** names; // column names (GC copies), n_cols entries; used by DB::raw
} TinoxSqliteResult;

void tinox_db_pool_init(const char* url, int64_t pool_size) {
    const char* diff_mode = getenv("TINOX_MIGRATE_MODE");
    if (diff_mode && strcmp(diff_mode, "diff-shadow") == 0) {
#if defined(TINOX_DB_SQLITE)
        url = ":memory:";
#else
        url = getenv("TINOX_DB_DIFF_URL");
        if (!url || !*url) { fprintf(stderr, "diff: TINOX_DB_DIFF_URL must name an empty disposable shadow database\n"); exit(1); }
#endif
    }

    // One serialized connection: pool_size does not apply. A transaction holds the
    // connection lock (see _tinox_drv_tx_conn_acquire) for its whole duration.
    (void)pool_size;
    // url may be a path or sqlite:///path
    const char* path = url;
    if (strncmp(path, "sqlite:///", 10) == 0) path += 9;
    else if (strncmp(path, "sqlite://", 9) == 0) path += 9;
    if (sqlite3_open_v2(path, &_tinox_sqlite_db,
            diff_mode && !strcmp(diff_mode, "diff-live") ? SQLITE_OPEN_READONLY : SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE, NULL) != SQLITE_OK) {
        fprintf(stderr, "SQLite error: %s\n", sqlite3_errmsg(_tinox_sqlite_db));
        if (service_enabled()) { service_record_error("SQLite initialization failed", 0); return; }
        exit(1);
    }
    // Another process holding the write lock is waited for, bounded, instead of failing at once.
    sqlite3_busy_timeout(_tinox_sqlite_db, (int)_tinox_db_lock_timeout_ms());
    // SQLite ignores REFERENCES unless asked: enforce the declared foreign keys.
    sqlite3_exec(_tinox_sqlite_db, "PRAGMA foreign_keys = ON", NULL, NULL, NULL);
}

// Single connection, so a transaction must exclude every other thread's statements until it
// ends: statements outside the owner take this lock for their duration, the owner holds it from
// BEGIN to COMMIT/ROLLBACK. Waiting is bounded (TINOX_DB_LOCK_TIMEOUT_MS, default 30000).
static pthread_mutex_t _tinox_sqlite_tx_mu = PTHREAD_MUTEX_INITIALIZER;

static int _tinox_sqlite_lock(void) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    int64_t ms = _tinox_db_lock_timeout_ms();
    ts.tv_sec += ms / 1000;
    ts.tv_nsec += (ms % 1000) * 1000000L;
    if (ts.tv_nsec >= 1000000000L) { ts.tv_sec++; ts.tv_nsec -= 1000000000L; }
    return pthread_mutex_timedlock(&_tinox_sqlite_tx_mu, &ts) == 0;
}

void* tinox_db_acquire_stmt_conn(void) {
    if (_tinox_sqlite_db == NULL) {
        fprintf(stderr, "sqlite: database connection was never initialized (no [database] config?)\n");
        exit(1);
    }
    if (_tinox_db_tx_conn != NULL) return _tinox_db_tx_conn;
    if (!_tinox_sqlite_lock()) {
        fprintf(stderr, "sqlite: waited %lld ms for the connection held by a transaction (TINOX_DB_LOCK_TIMEOUT_MS)\n", (long long)_tinox_db_lock_timeout_ms());
        exit(1);
    }
    return _tinox_sqlite_db;
}
void tinox_db_release_stmt_conn(void* conn) {
    if (conn == _tinox_db_tx_conn) return;
    pthread_mutex_unlock(&_tinox_sqlite_tx_mu);
}

static void* _tinox_drv_tx_conn_acquire(void) {
    if (_tinox_sqlite_db == NULL) {
        fprintf(stderr, "sqlite: database connection was never initialized (no [database] config?)\n");
        exit(1);
    }
    if (!_tinox_sqlite_lock()) {
        __tinox_err = (int64_t)(intptr_t)"transaction not started: the SQLite connection is held by another transaction (TINOX_DB_LOCK_TIMEOUT_MS)";
        return NULL;
    }
    return _tinox_sqlite_db;
}
static void _tinox_drv_tx_conn_release(void* c) { (void)c; pthread_mutex_unlock(&_tinox_sqlite_tx_mu); }
static int _tinox_drv_exec_simple(void* c, const char* sql) {
    char* err = NULL;
    int ok = sqlite3_exec((sqlite3*)c, sql, NULL, NULL, &err) == SQLITE_OK;
    if (!ok) tinox_db_statement_error(err);
    sqlite3_free(err);
    return ok;
}
static const char* _tinox_drv_begin_sql(void) { return "BEGIN"; }
static const char* _tinox_drv_requires_new_problem(void) {
    return "REQUIRES_NEW is not supported for SQLite (one serialized connection)";
}

// ---- Statement cache (Optimization 1) ----
#define STMT_CACHE_SIZE 64
typedef struct { const char* sql; sqlite3_stmt* stmt; } StmtCacheEntry;
static StmtCacheEntry _stmt_cache[STMT_CACHE_SIZE];

static sqlite3_stmt* _stmt_cache_get(const char* sql) {
    unsigned h = 0;
    for (const char* p = sql; *p; p++) h = h * 31 + (unsigned char)*p;
    h %= STMT_CACHE_SIZE;
    for (int i = 0; i < STMT_CACHE_SIZE; i++) {
        int slot = (h + i) % STMT_CACHE_SIZE;
        if (!_stmt_cache[slot].sql) return NULL;
        if (strcmp(_stmt_cache[slot].sql, sql) == 0) return _stmt_cache[slot].stmt;
    }
    return NULL;
}

static void _stmt_cache_put(const char* sql, sqlite3_stmt* stmt) {
    unsigned h = 0;
    for (const char* p = sql; *p; p++) h = h * 31 + (unsigned char)*p;
    h %= STMT_CACHE_SIZE;
    for (int i = 0; i < STMT_CACHE_SIZE; i++) {
        int slot = (h + i) % STMT_CACHE_SIZE;
        if (!_stmt_cache[slot].sql || strcmp(_stmt_cache[slot].sql, sql) == 0) {
            _stmt_cache[slot].sql = sql;
            _stmt_cache[slot].stmt = stmt;
            return;
        }
    }
    // Cache full: evict slot h (simple strategy)
    sqlite3_finalize(_stmt_cache[h].stmt);
    _stmt_cache[h].sql = sql;
    _stmt_cache[h].stmt = stmt;
}

// Bug 102: the statement cache above is a plain global array with no
// locking, and tinox_db_exec's reset/bind/step sequence operates on a
// cached sqlite3_stmt* shared across calls. The HTTP server runs request
// handlers concurrently (one worker pthread per CPU) -- two concurrent
// requests hitting the same cached query could interleave binding and
// stepping on the same statement (corrupting each other's parameters/
// results), and if the cache fills, one thread could finalize a statement
// (_stmt_cache_put's eviction path) while another is still using it.
static pthread_mutex_t _tinox_sqlite_mu = PTHREAD_MUTEX_INITIALIZER;

void* tinox_db_exec(void* conn, const char* sql_in, const char** params, int64_t n_params) {
    // The ORM writes PostgreSQL-style `$n` placeholders. SQLite treats `$name` as a NAME and numbers names in order
    // of first appearance, which only matches the ORM's numbering while the placeholders appear in order. `?NNN` is
    // SQLite's explicitly numbered form (same length, so the rewrite is a plain character swap).
    size_t sql_len = strlen(sql_in);
    char* sql_buf = (char*)malloc(sql_len + 1);
    memcpy(sql_buf, sql_in, sql_len + 1);
    char quote = 0;
    for (size_t i = 0; i < sql_len; i++) {
        char ch = sql_buf[i];
        if (quote) { if (ch == quote) quote = 0; }
        else if (ch == '\'' || ch == '"' || ch == '`') quote = ch;
        else if (ch == '$' && sql_buf[i + 1] >= '0' && sql_buf[i + 1] <= '9') sql_buf[i] = '?';
    }
    const char* sql = sql_buf;
    pthread_mutex_lock(&_tinox_sqlite_mu);
    sqlite3* db = (sqlite3*)conn;
    sqlite3_stmt* stmt = _stmt_cache_get(sql);
    if (stmt) {
        sqlite3_reset(stmt);
        sqlite3_clear_bindings(stmt);
    } else {
        if (sqlite3_prepare_v2(db, sql, -1, &stmt, NULL) != SQLITE_OK) {
            tinox_db_statement_error(sqlite3_errmsg(db));
            pthread_mutex_unlock(&_tinox_sqlite_mu);
            free(sql_buf);
            return NULL;
        }
        // The statement cache keeps the SQL pointer: give it a copy that lives forever (one per distinct statement).
        sql = strdup(sql_buf);
        _stmt_cache_put(sql, stmt);
    }
    for (int i = 0; i < (int)n_params; i++) {
        sqlite3_bind_text(stmt, i + 1, params[i], -1, SQLITE_STATIC);
    }

    // First pass: count rows
    int n_rows = 0, n_cols = sqlite3_column_count(stmt);
    // Collect all rows into a temporary list
    char*** rows = NULL;
    int rows_cap = 0;
    int rc;
    while ((rc = sqlite3_step(stmt)) == SQLITE_ROW) {
        if (n_rows >= rows_cap) {
            rows_cap = rows_cap ? rows_cap * 2 : 16;
            rows = (char***)realloc(rows, sizeof(char**) * (size_t)rows_cap);
        }
        rows[n_rows] = (char**)GC_malloc(sizeof(char*) * (size_t)(n_cols > 0 ? n_cols : 1));
        for (int c = 0; c < n_cols; c++) {
            const char* val = (const char*)sqlite3_column_text(stmt, c);
            rows[n_rows][c] = val ? GC_strdup(val) : NULL;
        }
        n_rows++;
    }
    if (rc != SQLITE_DONE) {
        // A failed step (e.g. a NOT NULL / UNIQUE constraint) used to be
        // swallowed and look like an empty result. Make it visible.
        tinox_db_statement_error(sqlite3_errmsg(db));
        sqlite3_reset(stmt);
        if (rows) free(rows);
        pthread_mutex_unlock(&_tinox_sqlite_mu);
        free(sql_buf);
        return NULL;
    }
    // Do NOT finalize — statement is cached for reuse

    TinoxSqliteResult* res = (TinoxSqliteResult*)GC_malloc(sizeof(TinoxSqliteResult));
    res->n_cols = n_cols;
    res->n_rows = n_rows;
    res->names = NULL;
    if (n_cols > 0) {
        res->names = (char**)GC_malloc(sizeof(char*) * (size_t)n_cols);
        for (int c = 0; c < n_cols; c++) {
            const char* nm = sqlite3_column_name(stmt, c);
            res->names[c] = GC_strdup(nm ? nm : "");
        }
    }
    if (n_rows > 0 && n_cols > 0) {
        res->data = (char**)GC_malloc(sizeof(char*) * (size_t)(n_rows * n_cols));
        for (int r = 0; r < n_rows; r++) {
            for (int c = 0; c < n_cols; c++) {
                res->data[r * n_cols + c] = rows[r][c];
            }
        }
    } else {
        res->data = NULL;
    }
    if (rows) free(rows);
    pthread_mutex_unlock(&_tinox_sqlite_mu);
    free(sql_buf);
    return (void*)res;
}

int64_t tinox_db_nrows(void* r)                       { return r ? ((TinoxSqliteResult*)r)->n_rows : 0; }
int64_t tinox_db_ncols(void* r)                       { return r ? ((TinoxSqliteResult*)r)->n_cols : 0; }
char*   tinox_db_getval(void* r, int64_t row, int64_t col) {
    TinoxSqliteResult* res = (TinoxSqliteResult*)r;
    if (!res || !res->data) return "";
    char* v = res->data[(int)row * res->n_cols + (int)col];
    return v ? v : "";
}
int64_t tinox_db_getval_int(void* result, int64_t row, int64_t col) {
    TinoxSqliteResult* res = (TinoxSqliteResult*)result;
    if (!res || !res->data) return 0;
    char* v = res->data[(int)row * res->n_cols + (int)col];
    if (!v) return 0;
    return (int64_t)atoll(v);
}
bool    tinox_db_is_null(void* r, int64_t row, int64_t col) {
    TinoxSqliteResult* res = (TinoxSqliteResult*)r;
    if (!res || !res->data) return true;
    return res->data[(int)row * res->n_cols + (int)col] == NULL;
}
void    tinox_db_free(void* r) { (void)r; }
bool    tinox_db_result_failed(void* r) { return r == NULL; }
char*   tinox_db_error(void* c) { return GC_strdup(sqlite3_errmsg((sqlite3*)c)); }

// DB.raw() row marshaling for SQLite: same contract as the PostgreSQL helper (a
// NULL column is absent from its row's Map; a statement without a result set,
// e.g. INSERT/UPDATE, gives an empty list).
void* tinox_db_result_to_row_maps(void* result) {
    TinoxSqliteResult* res = (TinoxSqliteResult*)result;
    int64_t nrows = tinox_db_nrows(result);
    int64_t ncols = tinox_db_ncols(result);
    int64_t* rows = tinox_array_new(nrows, 0);
    int64_t* data = ((TinoxArray*)rows)->data;
    for (int64_t r = 0; r < nrows; r++) {
        void* row_map = tinox_map_create();
        for (int64_t c = 0; c < ncols; c++) {
            if (tinox_db_is_null(result, r, c)) continue;
            tinox_map_set(row_map, res->names[c], (int64_t)(intptr_t)GC_strdup(res->data[r * ncols + c]));
        }
        data[r] = (int64_t)(intptr_t)row_map;
    }
    return (void*)rows;
}

#elif defined(TINOX_DB_MYSQL)

// ---- MySQL driver ----
#include <mysql/mysql.h>

// Connection pool, same shape as the PostgreSQL one: fixed size, connected eagerly, each
// connection used by one thread at a time (a statement, or a whole transaction).
#define TINOX_DB_POOL_MAX 64
static MYSQL* _tinox_mysql_pool[TINOX_DB_POOL_MAX];
static int64_t _tinox_mysql_pool_size = 0;
static int _tinox_mysql_pool_free[TINOX_DB_POOL_MAX];
static int _tinox_mysql_pool_free_count = 0;
static pthread_mutex_t _tinox_mysql_pool_mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t _tinox_mysql_pool_cond = PTHREAD_COND_INITIALIZER;

// URL format: mysql://user:pass@host:port/database
static void _parse_mysql_url(const char* url,
    char* host, char* user, char* pass, char* db, unsigned int* port) {
    // Defaults
    strcpy(host, "127.0.0.1");
    strcpy(user, "root");
    strcpy(pass, "");
    strcpy(db,   "");
    *port = 3306;

    // Skip "mysql://"
    const char* p = url;
    if (strncmp(p, "mysql://", 8) == 0) p += 8;

    // user:pass@
    const char* at = strchr(p, '@');
    if (at) {
        char userinfo[256];
        strncpy(userinfo, p, (size_t)(at - p));
        userinfo[at - p] = '\0';
        const char* colon = strchr(userinfo, ':');
        if (colon) {
            strncpy(user, userinfo, (size_t)(colon - userinfo));
            user[colon - userinfo] = '\0';
            strcpy(pass, colon + 1);
        } else {
            strcpy(user, userinfo);
        }
        p = at + 1;
    }

    // host:port/db
    const char* slash = strchr(p, '/');
    if (slash) {
        char hostport[256];
        strncpy(hostport, p, (size_t)(slash - p));
        hostport[slash - p] = '\0';
        strcpy(db, slash + 1);
        const char* portcolon = strchr(hostport, ':');
        if (portcolon) {
            strncpy(host, hostport, (size_t)(portcolon - hostport));
            host[portcolon - hostport] = '\0';
            *port = (unsigned int)atoi(portcolon + 1);
        } else {
            strcpy(host, hostport);
        }
    } else {
        strcpy(host, p);
    }
}

void tinox_db_pool_init(const char* url, int64_t pool_size) {
    const char* diff_mode = getenv("TINOX_MIGRATE_MODE");
    if (diff_mode && strcmp(diff_mode, "diff-shadow") == 0) {
#if defined(TINOX_DB_SQLITE)
        url = ":memory:";
#else
        url = getenv("TINOX_DB_DIFF_URL");
        if (!url || !*url) { fprintf(stderr, "diff: TINOX_DB_DIFF_URL must name an empty disposable shadow database\n"); exit(1); }
#endif
    }

    if (pool_size <= 0) pool_size = 1;
    if (pool_size > TINOX_DB_POOL_MAX) {
        fprintf(stderr, "DB pool size %lld exceeds maximum of %d\n", (long long)pool_size, TINOX_DB_POOL_MAX);
        exit(1);
    }
    char host[256], user[256], pass[256], db[256];
    unsigned int port;
    _parse_mysql_url(url, host, user, pass, db, &port);
    for (int64_t i = 0; i < pool_size; i++) {
        MYSQL* c = mysql_init(NULL);
        if (!c) {
            fprintf(stderr, "MySQL init failed\n");
            if (service_enabled()) { service_record_error("MySQL initialization failed", 0); return; }
            exit(1);
        }
        if (!mysql_real_connect(c, host, user, pass, db, port, NULL, 0)) {
            fprintf(stderr, "MySQL connection failed: %s\n", mysql_error(c));
            mysql_close(c);
            if (service_enabled()) { service_record_error("MySQL initialization failed", 0); return; }
            exit(1);
        }
        _tinox_mysql_pool[i] = c;
        _tinox_mysql_pool_free[i] = (int)i;
    }
    _tinox_mysql_pool_size = pool_size;
    _tinox_mysql_pool_free_count = (int)pool_size;
}

typedef struct {
    int n_cols;
    int n_rows;
    char** data;   // row-major: data[row * n_cols + col]; NULL for an SQL NULL
    char** names;  // column names (GC copies), n_cols entries; used by DB::raw
} TinoxMysqlResult;

static MYSQL* _tinox_mysql_pool_acquire(void) {
    if (_tinox_mysql_pool_size == 0) {
        fprintf(stderr, "MySQL: database connection was never initialized (no [database] config?)\n");
        exit(1);
    }
    pthread_mutex_lock(&_tinox_mysql_pool_mu);
    while (_tinox_mysql_pool_free_count == 0) {
        pthread_cond_wait(&_tinox_mysql_pool_cond, &_tinox_mysql_pool_mu);
    }
    MYSQL* c = _tinox_mysql_pool[_tinox_mysql_pool_free[--_tinox_mysql_pool_free_count]];
    pthread_mutex_unlock(&_tinox_mysql_pool_mu);
    return c;
}

static void _tinox_mysql_pool_release(MYSQL* conn) {
    pthread_mutex_lock(&_tinox_mysql_pool_mu);
    for (int64_t i = 0; i < _tinox_mysql_pool_size; i++) {
        if (_tinox_mysql_pool[i] == conn) { _tinox_mysql_pool_free[_tinox_mysql_pool_free_count++] = (int)i; break; }
    }
    pthread_mutex_unlock(&_tinox_mysql_pool_mu);
    pthread_cond_signal(&_tinox_mysql_pool_cond);
}

void* tinox_db_acquire_stmt_conn(void) {
    if (_tinox_db_tx_conn != NULL) return _tinox_db_tx_conn;
    return _tinox_mysql_pool_acquire();
}
void tinox_db_release_stmt_conn(void* conn) {
    if (conn == _tinox_db_tx_conn) return;
    _tinox_mysql_pool_release((MYSQL*)conn);
}

static void* _tinox_drv_tx_conn_acquire(void) { return _tinox_mysql_pool_acquire(); }
static void _tinox_drv_tx_conn_release(void* c) { _tinox_mysql_pool_release((MYSQL*)c); }
static int _tinox_drv_exec_simple(void* c, const char* sql) {
    int ok = mysql_query((MYSQL*)c, sql) == 0;
    if (!ok) tinox_db_statement_error(mysql_error((MYSQL*)c));
    return ok;
}
static const char* _tinox_drv_begin_sql(void) { return "START TRANSACTION"; }
static const char* _tinox_drv_requires_new_problem(void) {
    return _tinox_mysql_pool_size < 2 ? "REQUIRES_NEW needs [database] pool >= 2 (the outer transaction keeps its connection)" : NULL;
}

// The ORM emits PostgreSQL-style SQL: `$n` placeholders and `INSERT ... RETURNING id`. MySQL has
// neither, so translate here: placeholders become `?` with the parameters re-ordered by first
// use (outside string literals), and a trailing ` RETURNING id` is dropped and answered with
// the statement's insert id instead.
static char* _tinox_mysql_translate(const char* sql, const char** params, int64_t n_params,
                                    const char*** out_params, int64_t* out_n, int* wants_insert_id) {
    size_t len = strlen(sql);
    static const char tail[] = " RETURNING id";
    size_t tl = sizeof(tail) - 1;
    *wants_insert_id = 0;
    if (len >= tl && strcmp(sql + len - tl, tail) == 0) { len -= tl; *wants_insert_id = 1; }
    char* out = (char*)malloc(len + 1);
    const char** ordered = (const char**)calloc((size_t)(n_params > 0 ? n_params : 1), sizeof(char*));
    int64_t n = 0;
    size_t o = 0;
    char quote = 0;
    for (size_t i = 0; i < len; i++) {
        char ch = sql[i];
        if (quote) {
            out[o++] = ch;
            if (ch == quote) quote = 0;
        } else if (ch == '\'' || ch == '"' || ch == '`') {
            quote = ch;
            out[o++] = ch;
        } else if (ch == '$' && i + 1 < len && sql[i + 1] >= '0' && sql[i + 1] <= '9') {
            size_t j = i + 1;
            int64_t idx = 0;
            while (j < len && sql[j] >= '0' && sql[j] <= '9') { idx = idx * 10 + (sql[j] - '0'); j++; }
            if (idx < 1 || idx > n_params) {
                char message[128];
                snprintf(message, sizeof message, "MySQL: placeholder $%lld has no parameter (have %lld)", (long long)idx, (long long)n_params);
                tinox_db_statement_error(message);
                free(out); free(ordered);
                return NULL;
            }
            ordered[n++] = params[idx - 1];
            out[o++] = '?';
            i = j - 1;
        } else {
            out[o++] = ch;
        }
    }
    out[o] = '\0';
    // Native `?` SQL (the migration engine's) has no `$n`: keep its parameters as given.
    if (n == 0 && n_params > 0) { memcpy(ordered, params, sizeof(char*) * (size_t)n_params); n = n_params; }
    *out_params = ordered;
    *out_n = n;
    return out;
}

static void* _tinox_mysql_exec_translated(void* conn, const char* sql, const char** params, int64_t n_params, int wants_insert_id);

void* tinox_db_exec(void* conn, const char* sql, const char** params, int64_t n_params) {
    const char** ordered = NULL;
    int64_t n = 0;
    int wants_id = 0;
    char* translated = _tinox_mysql_translate(sql, params, n_params, &ordered, &n, &wants_id);
    if (!translated) return NULL;
    void* res = _tinox_mysql_exec_translated(conn, translated, ordered, n, wants_id);
    free(translated);
    free(ordered);
    return res;
}

static void* _tinox_mysql_exec_translated(void* conn, const char* sql, const char** params, int64_t n_params, int wants_insert_id) {
    MYSQL_STMT* stmt = mysql_stmt_init((MYSQL*)conn);
    if (mysql_stmt_prepare(stmt, sql, (unsigned long)strlen(sql)) != 0) {
        tinox_db_statement_error(mysql_stmt_error(stmt));
        mysql_stmt_close(stmt);
        return NULL;
    }

    MYSQL_BIND* bind = NULL;
    unsigned long* lengths = NULL;
    if (n_params > 0) {
        bind    = (MYSQL_BIND*)calloc((size_t)n_params, sizeof(MYSQL_BIND));
        lengths = (unsigned long*)calloc((size_t)n_params, sizeof(unsigned long));
        for (int i = 0; i < (int)n_params; i++) {
            lengths[i] = params[i] ? (unsigned long)strlen(params[i]) : 0;
            bind[i].buffer_type   = MYSQL_TYPE_STRING;
            bind[i].buffer        = (char*)params[i];
            bind[i].buffer_length = lengths[i];
            bind[i].length        = &lengths[i];
        }
        mysql_stmt_bind_param(stmt, bind);
    }

    if (mysql_stmt_execute(stmt) != 0) {
        tinox_db_statement_error(mysql_stmt_error(stmt));
        if (bind)    free(bind);
        if (lengths) free(lengths);
        mysql_stmt_close(stmt);
        return NULL;
    }

    if (wants_insert_id) {
        TinoxMysqlResult* idres = (TinoxMysqlResult*)GC_malloc(sizeof(TinoxMysqlResult));
        idres->n_cols = 1;
        idres->n_rows = 1;
        idres->names = NULL;
        idres->data = (char**)GC_malloc(sizeof(char*));
        char buf[32];
        snprintf(buf, sizeof buf, "%llu", (unsigned long long)mysql_stmt_insert_id(stmt));
        idres->data[0] = GC_strdup(buf);
        if (bind)    free(bind);
        if (lengths) free(lengths);
        mysql_stmt_close(stmt);
        return (void*)idres;
    }
    MYSQL_RES* meta = mysql_stmt_result_metadata(stmt);
    int n_cols = meta ? mysql_num_fields(meta) : 0;
    mysql_stmt_store_result(stmt);
    int n_rows = (int)mysql_stmt_num_rows(stmt);

    TinoxMysqlResult* res = (TinoxMysqlResult*)GC_malloc(sizeof(TinoxMysqlResult));
    res->n_cols = n_cols;
    res->n_rows = n_rows;
    res->names  = NULL;
    if (n_cols > 0) {
        MYSQL_FIELD* fields = mysql_fetch_fields(meta);
        res->names = (char**)GC_malloc(sizeof(char*) * (size_t)n_cols);
        for (int c = 0; c < n_cols; c++) res->names[c] = GC_strdup(fields[c].name ? fields[c].name : "");
    }
    res->data   = n_cols * n_rows > 0
        ? (char**)GC_malloc(sizeof(char*) * (size_t)(n_cols * n_rows))
        : NULL;

    if (n_cols > 0 && n_rows > 0) {
        // MySQL 8 declares MYSQL_BIND.is_null as bool*, MariaDB as my_bool*: take the type from the header.
        typedef __typeof__(*((MYSQL_BIND*)0)->is_null) TinoxMysqlNull;
        MYSQL_BIND* out_bind = (MYSQL_BIND*)calloc((size_t)n_cols, sizeof(MYSQL_BIND));
        char** bufs    = (char**)calloc((size_t)n_cols, sizeof(char*));
        unsigned long* out_len = (unsigned long*)calloc((size_t)n_cols, sizeof(unsigned long));
        TinoxMysqlNull* out_null = (TinoxMysqlNull*)calloc((size_t)n_cols, sizeof(TinoxMysqlNull));
        for (int c = 0; c < n_cols; c++) {
            bufs[c] = (char*)GC_malloc(512);
            out_bind[c].buffer_type   = MYSQL_TYPE_STRING;
            out_bind[c].buffer        = bufs[c];
            out_bind[c].buffer_length = 511;
            out_bind[c].length        = &out_len[c];
            out_bind[c].is_null       = &out_null[c];
        }
        mysql_stmt_bind_result(stmt, out_bind);
        for (int r = 0; r < n_rows; r++) {
            int frc = mysql_stmt_fetch(stmt);
            if (frc != 0 && frc != MYSQL_DATA_TRUNCATED) {
                tinox_db_statement_error(mysql_stmt_error(stmt));
                free(out_bind); free(bufs); free(out_len); free(out_null);
                if (meta)    mysql_free_result(meta);
                if (bind)    free(bind);
                if (lengths) free(lengths);
                mysql_stmt_close(stmt);
                return NULL;
            }
            for (int c = 0; c < n_cols; c++) {
                if (out_null[c]) { res->data[r * n_cols + c] = NULL; continue; }
                if (out_len[c] > 511) {
                    // The value did not fit the 512-byte column buffer: fetch the whole column instead of
                    // silently cutting it off.
                    unsigned long full = out_len[c];
                    char* big = (char*)GC_malloc(full + 1);
                    unsigned long got = 0;
                    MYSQL_BIND one;
                    memset(&one, 0, sizeof one);
                    one.buffer_type   = MYSQL_TYPE_STRING;
                    one.buffer        = big;
                    one.buffer_length = full;
                    one.length        = &got;
                    if (mysql_stmt_fetch_column(stmt, &one, (unsigned int)c, 0) != 0) {
                        tinox_db_statement_error(mysql_stmt_error(stmt));
                        free(out_bind); free(bufs); free(out_len); free(out_null);
                        if (meta)    mysql_free_result(meta);
                        if (bind)    free(bind);
                        if (lengths) free(lengths);
                        mysql_stmt_close(stmt);
                        return NULL;
                    }
                    big[full] = '\0';
                    res->data[r * n_cols + c] = big;
                } else {
                    bufs[c][out_len[c]] = '\0';
                    res->data[r * n_cols + c] = GC_strdup(bufs[c]);
                }
            }
        }
        free(out_bind);
        free(bufs);
        free(out_len);
        free(out_null);
    }

    if (meta)    mysql_free_result(meta);
    if (bind)    free(bind);
    if (lengths) free(lengths);
    mysql_stmt_close(stmt);
    return (void*)res;
}

int64_t tinox_db_nrows(void* r)              { return r ? ((TinoxMysqlResult*)r)->n_rows : 0; }
int64_t tinox_db_ncols(void* r)              { return r ? ((TinoxMysqlResult*)r)->n_cols : 0; }
char*   tinox_db_getval(void* r, int64_t row, int64_t col) {
    TinoxMysqlResult* res = (TinoxMysqlResult*)r;
    if (!res || !res->data) return "";
    char* v = res->data[(int)row * res->n_cols + (int)col];
    return v ? v : "";
}
int64_t tinox_db_getval_int(void* r, int64_t row, int64_t col) {
    TinoxMysqlResult* res = (TinoxMysqlResult*)r;
    if (!res || !res->data) return 0;
    char* v = res->data[(int)row * res->n_cols + (int)col];
    if (!v) return 0;
    return (int64_t)atoll(v);
}
bool    tinox_db_is_null(void* r, int64_t row, int64_t col) {
    TinoxMysqlResult* res = (TinoxMysqlResult*)r;
    if (!res || !res->data) return true;
    return res->data[(int)row * res->n_cols + (int)col] == NULL;
}
void    tinox_db_free(void* r) { (void)r; /* GC managed */ }
bool    tinox_db_result_failed(void* r) { return r == NULL; }
char*   tinox_db_error(void* c) { return GC_strdup(mysql_error((MYSQL*)c)); }

// DB.raw() row marshaling for MySQL: same contract as the PostgreSQL helper (a
// NULL column is absent from its row's Map; a statement without a result set,
// e.g. INSERT/UPDATE, gives an empty list).
void* tinox_db_result_to_row_maps(void* result) {
    TinoxMysqlResult* res = (TinoxMysqlResult*)result;
    int64_t nrows = tinox_db_nrows(result);
    int64_t ncols = tinox_db_ncols(result);
    int64_t* rows = tinox_array_new(nrows, 0);
    int64_t* data = ((TinoxArray*)rows)->data;
    for (int64_t r = 0; r < nrows; r++) {
        void* row_map = tinox_map_create();
        for (int64_t c = 0; c < ncols; c++) {
            if (tinox_db_is_null(result, r, c)) continue;
            tinox_map_set(row_map, res->names[c], (int64_t)(intptr_t)GC_strdup(res->data[r * ncols + c]));
        }
        data[r] = (int64_t)(intptr_t)row_map;
    }
    return (void*)rows;
}

#else
// Stub implementations when no DB driver is selected — prevent link errors.
void  tinox_db_pool_init(const char* url, int64_t pool_size)                  { (void)url;(void)pool_size; }
void* tinox_db_acquire_stmt_conn(void)                                        { return NULL; }
void  tinox_db_release_stmt_conn(void* conn)                                  { (void)conn; }
int64_t tinox_db_tx_enter(int64_t mode)                                       { (void)mode; return 0; }
int64_t tinox_db_tx_leave(int64_t token, bool ok)                             { (void)token;(void)ok; return 1; }
bool  tinox_db_tx_active(void)                                                { return false; }
void* tinox_db_exec(void* c, const char* s, const char** p, int64_t n)        { (void)c;(void)s;(void)p;(void)n; return NULL; }
int64_t tinox_db_nrows(void* r)                                                { (void)r; return 0; }
int64_t tinox_db_ncols(void* r)                                                { (void)r; return 0; }
char*   tinox_db_getval(void* r, int64_t row, int64_t col)                    { (void)r;(void)row;(void)col; return ""; }
int64_t tinox_db_getval_int(void* r, int64_t row, int64_t col)                { (void)r;(void)row;(void)col; return 0; }
bool    tinox_db_is_null(void* r, int64_t row, int64_t col)                   { (void)r;(void)row;(void)col; return true; }
void    tinox_db_free(void* r)                                                 { (void)r; }
bool    tinox_db_result_failed(void* r)                                        { (void)r; return false; }
char*   tinox_db_error(void* c)                                                { (void)c; return ""; }

#endif /* DB driver */

// ---- Declarative schema migrations (issue #410) ----
//
// The compiler parses the YAML changelog, translates every change into SQL for the
// configured driver and passes the result here as one payload string (format: see
// serialize_payload in crates/tinox/src/migrate.rs): change sets separated by 0x1d,
// fields by 0x1f (id, author, file, checksum, flags, statements joined by 0x1e, rollback statements, irreversible
// reason, tag, preconditions, context names, label names).
// This engine is driver-agnostic: it only uses the tinox_db_* ABI above.
//
// Messages go to stderr so a program's stdout stays untouched; `status` prints its
// table to stdout. TINOX_MIGRATE_MODE = update (default) | status | baseline | release-lock | rollback
// (with TINOX_MIGRATE_ROLLBACK_COUNT=N or TINOX_MIGRATE_ROLLBACK_TAG=T).
// `baseline` records every pending change set as applied WITHOUT running it, to adopt a
// database whose schema already exists.
#include <time.h>

#if defined(TINOX_DB_POSTGRES)
#define TINOX_MIG_PH(n) "$" #n
#else
#define TINOX_MIG_PH(n) "?"
#endif

typedef struct {
    char* id; char* author; char* file; char* checksum;
    int run_on_change, run_always;
    char** stmts; int n_stmts;
    char** rb_stmts; int n_rb_stmts;
    char* irreversible;  // reason, empty when the change set can be rolled back
    char* context; char* labels; // comma-separated names
    char* tag;           // empty when untagged
    // preConditions: on_fail is NULL when the changeSet has none; each check is
    // { negate, kind, a, b, desc } (see serialize_payload in migrate.rs).
    char* on_fail; char*** checks; int n_checks;
} TinoxMigSet;

static void tinox_mig_die(const char* msg) {
    fprintf(stderr, "migrate: %s\n", msg);
    exit(1);
}

// Native migration code handles status values itself (including lock-contention
// retries). Keep its diagnostics and do not leak a handled failure into Main.
static void* tinox_mig_query(void* conn, const char* sql, const char** params, int64_t n) {
    int64_t saved_error = __tinox_err;
    __tinox_err = 0;
    void* result = tinox_db_exec(conn, sql, params, n);
    if (__tinox_err) fprintf(stderr, "Query error: %s\nSQL: %s\n", (char*)(intptr_t)__tinox_err, sql);
    __tinox_err = saved_error;
    return result;
}

static int tinox_mig_exec(void* conn, const char* sql, const char** params, int64_t n) {
    void* r = tinox_mig_query(conn, sql, params, n);
    int failed = tinox_db_result_failed(r);
    if (r) tinox_db_free(r);
    return !failed;
}

static int64_t tinox_mig_now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static char* tinox_mig_dup(const char* s, size_t len) {
    char* p = (char*)malloc(len + 1);
    if (!p) tinox_mig_die("out of memory");
    memcpy(p, s, len);
    p[len] = 0;
    return p;
}

// Splits `s` at every `sep`; returns a malloc'd array of malloc'd strings. Strings are
// never freed on purpose: the SQLite driver's statement cache keeps the SQL pointer.
static char** tinox_mig_split(const char* s, char sep, int* count) {
    int n = 1;
    for (const char* p = s; *p; p++) if (*p == sep) n++;
    char** out = (char**)malloc(sizeof(char*) * (size_t)n);
    int i = 0;
    const char* start = s;
    for (const char* p = s;; p++) {
        if (*p == sep || *p == 0) {
            out[i++] = tinox_mig_dup(start, (size_t)(p - start));
            start = p + 1;
            if (*p == 0) break;
        }
    }
    *count = n;
    return out;
}

static TinoxMigSet* tinox_mig_parse(const char* payload, int* n_sets) {
    if (!payload || !*payload) { *n_sets = 0; return NULL; }
    int n = 0;
    char** recs = tinox_mig_split(payload, '\x1d', &n);
    TinoxMigSet* sets = (TinoxMigSet*)calloc((size_t)n, sizeof(TinoxMigSet));
    for (int i = 0; i < n; i++) {
        int nf = 0;
        char** f = tinox_mig_split(recs[i], '\x1f', &nf);
        if (nf != 12) tinox_mig_die("corrupt changelog payload");
        sets[i].id = f[0]; sets[i].author = f[1]; sets[i].file = f[2]; sets[i].checksum = f[3];
        sets[i].run_on_change = strchr(f[4], 'C') != NULL;
        sets[i].run_always = strchr(f[4], 'A') != NULL;
        sets[i].stmts = tinox_mig_split(f[5], '\x1e', &sets[i].n_stmts);
        if (sets[i].n_stmts == 1 && sets[i].stmts[0][0] == 0) sets[i].n_stmts = 0;
        sets[i].rb_stmts = tinox_mig_split(f[6], '\x1e', &sets[i].n_rb_stmts);
        if (sets[i].n_rb_stmts == 1 && sets[i].rb_stmts[0][0] == 0) sets[i].n_rb_stmts = 0;
        sets[i].irreversible = f[7]; sets[i].tag = f[8];
        sets[i].context = f[10]; sets[i].labels = f[11];
        if (f[9][0]) {
            int np = 0;
            char** parts = tinox_mig_split(f[9], '\x1c', &np);
            sets[i].on_fail = parts[0];
            sets[i].n_checks = np - 1;
            sets[i].checks = (char***)calloc((size_t)np, sizeof(char**));
            for (int k = 1; k < np; k++) {
                int cf = 0;
                sets[i].checks[k - 1] = tinox_mig_split(parts[k], '\x1b', &cf);
                if (cf != 5) tinox_mig_die("corrupt changelog payload (precondition)");
            }
        }
    }
    *n_sets = n;
    return sets;
}

// Every non-empty qualifier list must intersect the active contexts.
static int tinox_mig_matches(const char* names, const char* active) {
    if (!*names) return 1;
    if (!active || !*active) return 0;
    for (const char* n = names; *n;) {
        const char* end = strchr(n, ',');
        size_t len = end ? (size_t)(end - n) : strlen(n);
        for (const char* a = active; *a;) {
            const char* ae = strchr(a, ',');
            const char* stop = ae ? ae : a + strlen(a);
            while (a < stop && isspace((unsigned char)*a)) a++;
            while (stop > a && isspace((unsigned char)stop[-1])) stop--;
            if ((size_t)(stop - a) == len && !strncmp(n, a, len)) return 1;
            if (!ae) break;
            a = ae + 1;
        }
        if (!end) break;
        n = end + 1;
    }
    return 0;
}

static void tinox_mig_sleep_ms(long ms) {
    struct timespec ts = { ms / 1000, (ms % 1000) * 1000000L };
    nanosleep(&ts, NULL);
}

static int tinox_mig_table_exists(void* conn, const char* table) {
#if defined(TINOX_DB_SQLITE)
    const char* sql = "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?";
#elif defined(TINOX_DB_POSTGRES)
    const char* sql = "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = current_schema() AND table_name = $1";
#else
    const char* sql = "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE() AND table_name = ?";
#endif
    const char* params[1] = { table };
    void* r = tinox_mig_query(conn, sql, params, 1);
    int exists = (r && !tinox_db_result_failed(r)) ? tinox_db_getval_int(r, 0, 0) > 0 : 0;
    if (r) tinox_db_free(r);
    return exists;
}

// Runs a query that returns one COUNT-like value; a failing query is a hard error, never "false".
static int64_t tinox_mig_scalar_int(void* conn, const char* sql, const char** params, int64_t n) {
    void* r = tinox_mig_query(conn, sql, params, n);
    if (!r || tinox_db_result_failed(r)) tinox_mig_die("a precondition query failed");
    int64_t v = tinox_db_getval_int(r, 0, 0);
    tinox_db_free(r);
    return v;
}

// Evaluates one precondition check (without `negate`). 1 = holds.
static int tinox_mig_check_holds(void* conn, char** c) {
    const char* kind = c[1]; const char* a = c[2]; const char* b = c[3];
    if (!strcmp(kind, "const")) return a[0] == '1';
    if (!strcmp(kind, "tableExists")) return tinox_mig_table_exists(conn, a);
    if (!strcmp(kind, "sqlCheck")) {
        void* r = tinox_mig_query(conn, a, NULL, 0);
        if (!r || tinox_db_result_failed(r)) tinox_mig_die("the sqlCheck precondition query failed");
        int ok = tinox_db_nrows(r) > 0 && tinox_db_ncols(r) > 0 && !tinox_db_is_null(r, 0, 0)
                 && strcmp(tinox_db_getval(r, 0, 0), b) == 0;
        tinox_db_free(r);
        return ok;
    }
    const char* params[2] = { a, b };
    if (!strcmp(kind, "columnExists")) {
#if defined(TINOX_DB_SQLITE)
        return tinox_mig_scalar_int(conn, "SELECT COUNT(*) FROM pragma_table_info(?) WHERE name = ?", params, 2) > 0;
#elif defined(TINOX_DB_POSTGRES)
        return tinox_mig_scalar_int(conn, "SELECT COUNT(*) FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = $1 AND column_name = $2", params, 2) > 0;
#else
        return tinox_mig_scalar_int(conn, "SELECT COUNT(*) FROM information_schema.columns WHERE table_schema = DATABASE() AND table_name = ? AND column_name = ?", params, 2) > 0;
#endif
    }
    if (!strcmp(kind, "indexExists")) {
#if defined(TINOX_DB_SQLITE)
        return tinox_mig_scalar_int(conn, "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND tbl_name = ? AND name = ?", params, 2) > 0;
#elif defined(TINOX_DB_POSTGRES)
        return tinox_mig_scalar_int(conn, "SELECT COUNT(*) FROM pg_indexes WHERE schemaname = current_schema() AND tablename = $1 AND indexname = $2", params, 2) > 0;
#else
        return tinox_mig_scalar_int(conn, "SELECT COUNT(*) FROM information_schema.statistics WHERE table_schema = DATABASE() AND table_name = ? AND index_name = ?", params, 2) > 0;
#endif
    }
    tinox_mig_die("unknown precondition kind");
    return 0;
}

// Returns the first failed check's description, or NULL when every check holds.
static const char* tinox_mig_first_failed(void* conn, TinoxMigSet* s) {
    for (int k = 0; k < s->n_checks; k++) {
        char** c = s->checks[k];
        int holds = tinox_mig_check_holds(conn, c);
        if (c[0][0] == '1') holds = !holds;
        if (!holds) return c[4];
    }
    return NULL;
}

static void tinox_mig_ensure_tables(void* conn) {
    // Skip the CREATE when the tables exist (avoids a driver notice on every start). On PostgreSQL
    // two instances creating them at once can still collide in the catalog, so serialize that.
#if defined(TINOX_DB_POSTGRES)
    tinox_mig_exec(conn, "SELECT pg_advisory_lock(727274)", NULL, 0);
#endif
    if (tinox_mig_table_exists(conn, "tinox_changelog") && tinox_mig_table_exists(conn, "tinox_changelog_lock")) {
#if defined(TINOX_DB_POSTGRES)
        tinox_mig_exec(conn, "SELECT pg_advisory_unlock(727274)", NULL, 0);
#endif
        return;
    }
    if (!tinox_mig_exec(conn,
            "CREATE TABLE IF NOT EXISTS tinox_changelog ("
            "changeset_id VARCHAR(190) NOT NULL, author VARCHAR(190) NOT NULL, source_file VARCHAR(255) NOT NULL, "
            "checksum VARCHAR(64) NOT NULL, executed_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP, "
            "execution_ms BIGINT NOT NULL, exec_order BIGINT NOT NULL, PRIMARY KEY (changeset_id, author))", NULL, 0))
        tinox_mig_die("cannot create the tinox_changelog table");
    if (!tinox_mig_exec(conn,
            "CREATE TABLE IF NOT EXISTS tinox_changelog_lock ("
            "id INTEGER NOT NULL PRIMARY KEY, locked_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP, locked_by VARCHAR(255))", NULL, 0))
        tinox_mig_die("cannot create the tinox_changelog_lock table");
#if defined(TINOX_DB_POSTGRES)
    tinox_mig_exec(conn, "SELECT pg_advisory_unlock(727274)", NULL, 0);
#endif
}

// The lock is a row in tinox_changelog_lock; a second starter waits for it (bounded), a lock
// left behind by a crashed run is NEVER broken automatically (`tinox db release-lock`).
static void tinox_mig_lock(void* conn) {
    long wait_ms = 30000;
    const char* env = getenv("TINOX_MIGRATE_LOCK_WAIT_MS");
    if (env && *env) wait_ms = atol(env);
    char who[64];
    snprintf(who, sizeof who, "pid %ld", (long)getpid());
    const char* params[1] = { who };
    long waited = 0;
    for (;;) {
        void* r = tinox_mig_query(conn, "SELECT COUNT(*) FROM tinox_changelog_lock", NULL, 0);
        int64_t held = (r && !tinox_db_result_failed(r)) ? tinox_db_getval_int(r, 0, 0) : 1;
        if (r) tinox_db_free(r);
        if (held == 0 && tinox_mig_exec(conn,
                "INSERT INTO tinox_changelog_lock (id, locked_by) VALUES (1, " TINOX_MIG_PH(1) ")", params, 1))
            return;
        if (waited >= wait_ms) {
            tinox_mig_die("the changelog lock is held by another process. If no migration is running "
                          "(e.g. it crashed), release it with `tinox db release-lock`");
        }
        tinox_mig_sleep_ms(250);
        waited += 250;
    }
}

static void tinox_mig_unlock(void* conn) {
    tinox_mig_exec(conn, "DELETE FROM tinox_changelog_lock WHERE id = 1", NULL, 0);
}

#if defined(TINOX_DB_MYSQL)
// MySQL commits DDL implicitly, so a change set is not atomic there.
#define TINOX_MIG_BEGIN(c)    (1)
#define TINOX_MIG_COMMIT(c)   (1)
#define TINOX_MIG_ROLLBACK(c) ((void)0)
#else
#define TINOX_MIG_BEGIN(c)    tinox_mig_exec((c), "BEGIN", NULL, 0)
#define TINOX_MIG_COMMIT(c)   tinox_mig_exec((c), "COMMIT", NULL, 0)
#define TINOX_MIG_ROLLBACK(c) ((void)tinox_mig_exec((c), "ROLLBACK", NULL, 0))
#endif

typedef struct { char* id; char* author; char* checksum; } TinoxMigApplied;

// Catalog rows have four fields: object kind, table, object name, definition.
// Hex framing keeps arbitrary SQL identifiers/expressions unambiguous across processes.
static void* tinox_diff_catalog(void* conn) {
#if defined(TINOX_DB_SQLITE)
    const char* sql =
        "SELECT 'table', name, '', '' FROM sqlite_master WHERE type='table' AND name NOT GLOB 'sqlite_*' "
        "UNION ALL SELECT 'column', m.name, p.name, upper(replace(p.type,' ','')) "
        "FROM sqlite_master m, pragma_table_xinfo(m.name) p WHERE m.type='table' AND m.name NOT GLOB 'sqlite_*' "
        "UNION ALL SELECT 'index', m.name, i.name, "
        "CAST(i.\"unique\" AS TEXT)||':'||i.origin||':'||x.seqno||':'||coalesce(x.name,'<expression>')||':'||x.desc||':'||coalesce(x.coll,'')||':'|| "
        "CASE WHEN i.partial OR x.cid=-2 THEN coalesce((SELECT sql FROM sqlite_master WHERE name=i.name),'') ELSE '' END "
        "FROM sqlite_master m, pragma_index_list(m.name) i, pragma_index_xinfo(i.name) x "
        "WHERE m.type='table' AND m.name NOT GLOB 'sqlite_*' AND x.key=1";
#elif defined(TINOX_DB_POSTGRES)
    const char* sql =
        "SELECT 'table', c.relname, '', '' FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace "
        "WHERE n.nspname=current_schema() AND c.relkind IN ('r','p') "
        "UNION ALL SELECT 'column', c.relname, a.attname, format_type(a.atttypid,a.atttypmod) "
        "FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace JOIN pg_attribute a ON a.attrelid=c.oid "
        "WHERE n.nspname=current_schema() AND c.relkind IN ('r','p') AND a.attnum>0 AND NOT a.attisdropped "
        "UNION ALL SELECT 'index', t.relname, c.relname, "
        "i.indisunique::text||':'||coalesce(to_jsonb(i)->>'indnullsnotdistinct','false')||':'||i.indnkeyatts::text||':'||i.indisvalid::text||':'||am.amname||':'||k::text||':'||pg_get_indexdef(i.indexrelid,k,true)||':'||coalesce(pg_get_expr(i.indpred,i.indrelid),'') "
        "||':'||coalesce(i.indoption[k-1]::text,'')||':'||coalesce((SELECT collname FROM pg_collation WHERE oid=i.indcollation[k-1]),'') "
        "||':'||coalesce((SELECT opcname FROM pg_opclass WHERE oid=i.indclass[k-1]),'') "
        "FROM pg_index i JOIN pg_class t ON t.oid=i.indrelid JOIN pg_namespace n ON n.oid=t.relnamespace "
        "JOIN pg_class c ON c.oid=i.indexrelid JOIN pg_am am ON am.oid=c.relam, generate_series(1,i.indnatts) k "
        "WHERE n.nspname=current_schema()";
#elif defined(TINOX_DB_MYSQL)
    const char* sql =
        "SELECT 'table', table_name, '', '' FROM information_schema.tables WHERE table_schema=DATABASE() AND table_type='BASE TABLE' "
        "UNION ALL SELECT 'column', table_name, column_name, column_type FROM information_schema.columns WHERE table_schema=DATABASE() "
        "UNION ALL SELECT CASE WHEN column_name IS NULL THEN 'unsupported-index' ELSE 'index' END, table_name, index_name, "
        "CONCAT(non_unique,':',index_type,':',seq_in_index,':',COALESCE(column_name,'<expression>'),':',COALESCE(sub_part,0),':',COALESCE(collation,'')) "
        "FROM information_schema.statistics WHERE table_schema=DATABASE()";
#else
    const char* sql = "";
#endif
    void* r = tinox_mig_query(conn, sql, NULL, 0);
    if (!r || tinox_db_result_failed(r)) tinox_mig_die("cannot inspect schema catalogs");
    for (int64_t row = 0; row < tinox_db_nrows(r); row++) {
        if (!strcmp(tinox_db_getval(r, row, 0), "unsupported-index"))
            tinox_mig_die("MySQL functional indexes are not supported by schema diff");
    }
    return r;
}

static void tinox_diff_snapshot(void* conn, TinoxMigSet* sets, int n_sets, int shadow) {
    void* r = tinox_diff_catalog(conn);
    if (shadow) {
        if (tinox_db_nrows(r) != 0) tinox_mig_die("shadow database must be empty; use a disposable database, never the live database");
        tinox_db_free(r);
#if defined(TINOX_DB_POSTGRES)
        if (!tinox_mig_exec(conn, "BEGIN", NULL, 0)) tinox_mig_die("cannot start shadow transaction");
#endif
        for (int i = 0; i < n_sets; i++) {
            for (int j = 0; j < sets[i].n_stmts; j++) {
                if (!tinox_mig_exec(conn, sets[i].stmts[j], NULL, 0)) {
                    fprintf(stderr, "diff: shadow replay failed in changeSet '%s' by '%s' (%s)\n", sets[i].id, sets[i].author, sets[i].file);
                    tinox_mig_die("cannot produce expected schema");
                }
            }
        }
        r = tinox_diff_catalog(conn);
    }
    for (int64_t row = 0; row < tinox_db_nrows(r); row++) {
        const char* table = tinox_db_getval(r, row, 1);
        if (!strcmp(table, "tinox_changelog") || !strcmp(table, "tinox_changelog_lock")) continue;
        for (int col = 0; col < 4; col++) {
            const unsigned char* p = (const unsigned char*)tinox_db_getval(r, row, col);
            for (; *p; p++) printf("%02x", *p);
            putchar(col == 3 ? '\n' : '\t');
        }
    }
    tinox_db_free(r);
#if defined(TINOX_DB_POSTGRES)
    if (shadow && !tinox_mig_exec(conn, "ROLLBACK", NULL, 0)) tinox_mig_die("cannot roll back shadow transaction");
#endif
    fflush(stdout);
}

// Undo the newest applied change sets (a count, or everything after a tag). Everything is
// checked before the first statement runs; each undo is one transaction together with the
// removal of its history row.
static void tinox_mig_rollback(void* conn, TinoxMigSet* sets, int n_sets, TinoxMigApplied* applied, int64_t n_applied) {
    const char* count_env = getenv("TINOX_MIGRATE_ROLLBACK_COUNT");
    const char* tag_env = getenv("TINOX_MIGRATE_ROLLBACK_TAG");
    int64_t count = 0;
    if ((count_env && *count_env) == (tag_env && *tag_env)) {
        tinox_mig_unlock(conn);
        tinox_mig_die("rollback needs exactly one of --count N or --tag T");
    }
    if (count_env && *count_env) {
        count = atoll(count_env);
        if (count < 1) { tinox_mig_unlock(conn); tinox_mig_die("rollback --count must be a positive number"); }
        if (count > n_applied) {
            fprintf(stderr, "migrate: only %lld changeSet(s) are applied\n", (long long)n_applied);
            tinox_mig_unlock(conn);
            tinox_mig_die("rollback --count exceeds the applied changeSets, nothing was rolled back");
        }
    } else {
        int tagged = -1;
        for (int i = 0; i < n_sets; i++) if (!strcmp(sets[i].tag, tag_env)) { tagged = i; break; }
        if (tagged < 0) { tinox_mig_unlock(conn); tinox_mig_die("rollback --tag: no changeSet carries that tag"); }
        int64_t pos = -1;
        for (int64_t j = 0; j < n_applied; j++)
            if (!strcmp(applied[j].id, sets[tagged].id) && !strcmp(applied[j].author, sets[tagged].author)) { pos = j; break; }
        if (pos < 0) { tinox_mig_unlock(conn); tinox_mig_die("rollback --tag: the tagged changeSet is not applied, nothing was rolled back"); }
        count = n_applied - 1 - pos;
    }
    // Validate the whole selection first.
    int bad = 0;
    for (int64_t k = 0; k < count; k++) {
        TinoxMigApplied* a = &applied[n_applied - 1 - k];
        int idx = -1;
        for (int i = 0; i < n_sets; i++) if (!strcmp(a->id, sets[i].id) && !strcmp(a->author, sets[i].author)) { idx = i; break; }
        if (idx < 0) {
            fprintf(stderr, "migrate: applied changeSet '%s' by '%s' is not in the changelog, so it cannot be rolled back\n", a->id, a->author);
            bad++;
        } else if (strcmp(a->checksum, sets[idx].checksum) != 0) {
            fprintf(stderr, "migrate: changeSet '%s' by '%s' was modified after it was applied, so its rollback may not match\n", a->id, a->author);
            bad++;
        } else if (sets[idx].irreversible[0]) {
            fprintf(stderr, "migrate: %s\n", sets[idx].irreversible);
            bad++;
        }
    }
    if (bad) { tinox_mig_unlock(conn); tinox_mig_die("rollback refused, nothing was rolled back"); }
    for (int64_t k = 0; k < count; k++) {
        TinoxMigApplied* a = &applied[n_applied - 1 - k];
        TinoxMigSet* cs = NULL;
        for (int i = 0; i < n_sets; i++) if (!strcmp(a->id, sets[i].id) && !strcmp(a->author, sets[i].author)) { cs = &sets[i]; break; }
        if (!TINOX_MIG_BEGIN(conn)) { tinox_mig_unlock(conn); tinox_mig_die("cannot start a transaction"); }
        for (int s = 0; s < cs->n_rb_stmts; s++) {
            if (!tinox_mig_exec(conn, cs->rb_stmts[s], NULL, 0)) {
                TINOX_MIG_ROLLBACK(conn);
                fprintf(stderr, "migrate: rollback of changeSet '%s' by '%s' (%s) failed on: %s\n", cs->id, cs->author, cs->file, cs->rb_stmts[s]);
                tinox_mig_unlock(conn);
                tinox_mig_die("rollback failed; this changeSet stays applied, earlier rollbacks stay done");
            }
        }
        const char* p[2] = { cs->id, cs->author };
        if (!tinox_mig_exec(conn, "DELETE FROM tinox_changelog WHERE changeset_id = " TINOX_MIG_PH(1) " AND author = " TINOX_MIG_PH(2), p, 2)
                || !TINOX_MIG_COMMIT(conn)) {
            TINOX_MIG_ROLLBACK(conn);
            tinox_mig_unlock(conn);
            tinox_mig_die("cannot remove the changeSet from tinox_changelog");
        }
        fprintf(stderr, "migrate: rolled back changeSet '%s' by '%s'\n", cs->id, cs->author);
    }
    if (count == 0) fprintf(stderr, "migrate: nothing to roll back\n");
    tinox_mig_unlock(conn);
    tinox_db_release_stmt_conn(conn);
}

void tinox_migrate_run(const char* payload) {
    int n_sets = 0;
    TinoxMigSet* sets = tinox_mig_parse(payload, &n_sets);
    const char* mode = getenv("TINOX_MIGRATE_MODE");
    if (!mode || !*mode) mode = "update";
    if (!strcmp(mode, "diff-live") || !strcmp(mode, "diff-shadow")) {
        void* conn = tinox_db_acquire_stmt_conn();
        if (!conn) tinox_mig_die("no database connection");
        tinox_diff_snapshot(conn, sets, n_sets, !strcmp(mode, "diff-shadow"));
        tinox_db_release_stmt_conn(conn);
        return;
    }
    int status_only = strcmp(mode, "status") == 0;
    int release_only = strcmp(mode, "release-lock") == 0;
    int baseline = strcmp(mode, "baseline") == 0;
    int rollback = strcmp(mode, "rollback") == 0;
    if (!status_only && !release_only && !baseline && !rollback && strcmp(mode, "update") != 0)
        tinox_mig_die("TINOX_MIGRATE_MODE must be update, status, baseline, release-lock or rollback");

    void* conn = tinox_db_acquire_stmt_conn();
    if (!conn) tinox_mig_die("no database connection (is [database] configured?)");
    tinox_mig_ensure_tables(conn);
    if (release_only) {
        tinox_mig_unlock(conn);
        fprintf(stderr, "migrate: changelog lock released\n");
        tinox_db_release_stmt_conn(conn);
        return;
    }
    if (!status_only) tinox_mig_lock(conn);

    // What the database already ran.
    void* r = tinox_mig_query(conn, "SELECT changeset_id, author, checksum FROM tinox_changelog ORDER BY exec_order", NULL, 0);
    if (!r || tinox_db_result_failed(r)) tinox_mig_die("cannot read tinox_changelog");
    int64_t n_applied = tinox_db_nrows(r);
    TinoxMigApplied* applied = (TinoxMigApplied*)calloc((size_t)(n_applied + 1), sizeof(TinoxMigApplied));
    for (int64_t i = 0; i < n_applied; i++) {
        const char* a = tinox_db_getval(r, i, 0); const char* b = tinox_db_getval(r, i, 1); const char* c = tinox_db_getval(r, i, 2);
        applied[i].id = tinox_mig_dup(a, strlen(a)); applied[i].author = tinox_mig_dup(b, strlen(b));
        applied[i].checksum = tinox_mig_dup(c, strlen(c));
    }
    tinox_db_free(r);

    if (rollback) {
        tinox_mig_rollback(conn, sets, n_sets, applied, n_applied);
        return;
    }

    // Pass 1: classify. A changed change set that already ran is an error, before anything is applied.
    int* state = (int*)calloc((size_t)(n_sets + 1), sizeof(int)); // 0 applied, 1 pending, 2 re-run, 3 changed, 4 excluded
    int changed = 0, pending = 0;
    const char* contexts = getenv("TINOX_MIGRATE_CONTEXTS");
    for (int i = 0; i < n_sets; i++) {
        int found = -1;
        for (int64_t j = 0; j < n_applied; j++)
            if (!strcmp(applied[j].id, sets[i].id) && !strcmp(applied[j].author, sets[i].author)) { found = (int)j; break; }
        if (found < 0) { state[i] = 1; pending++; }
        else if (strcmp(applied[found].checksum, sets[i].checksum) != 0) {
            if (sets[i].run_on_change) { state[i] = 2; pending++; }
            else { state[i] = 3; changed++; }
        } else if (sets[i].run_always) { state[i] = 2; pending++; }
    }
    for (int i = 0; i < n_sets; i++) {
        if ((state[i] == 1 || state[i] == 2) &&
            (!tinox_mig_matches(sets[i].context, contexts) || !tinox_mig_matches(sets[i].labels, contexts))) {
            state[i] = 4; pending--;
        }
    }
    for (int64_t j = 0; j < n_applied; j++) {
        int known = 0;
        for (int i = 0; i < n_sets; i++)
            if (!strcmp(applied[j].id, sets[i].id) && !strcmp(applied[j].author, sets[i].author)) { known = 1; break; }
        if (!known) fprintf(stderr, "migrate: warning: applied changeSet '%s' by '%s' is not in the changelog\n", applied[j].id, applied[j].author);
    }

    if (status_only) {
        for (int i = 0; i < n_sets; i++) {
            const char* label = state[i] == 0 ? "applied" : state[i] == 1 ? "pending" : state[i] == 2 ? "re-run" : state[i] == 4 ? "excluded" : "CHANGED";
            printf("%-8s %s by %s (%s)", label, sets[i].id, sets[i].author, sets[i].file);
            if (*sets[i].tag) printf(" [tag: %s]", sets[i].tag);
            printf("\n");
        }
        printf("%d changeSet(s): %d pending, %d changed\n", n_sets, pending, changed);
        fflush(stdout);
        tinox_db_release_stmt_conn(conn);
        if (changed) exit(1);
        return;
    }

    if (changed) {
        for (int i = 0; i < n_sets; i++)
            if (state[i] == 3)
                fprintf(stderr, "migrate: changeSet '%s' by '%s' (%s) was modified after it was applied "
                                "(add 'runOnChange: true' if re-running is intended, otherwise add a new changeSet instead)\n",
                        sets[i].id, sets[i].author, sets[i].file);
        tinox_mig_unlock(conn);
        tinox_mig_die("checksum validation failed, nothing was applied");
    }

    // Pass 2: apply, each change set in its own transaction together with its history row.
    static const char* ins_sql = NULL;
    static const char* upd_sql = NULL;
    if (!ins_sql) {
        ins_sql = "INSERT INTO tinox_changelog (changeset_id, author, source_file, checksum, execution_ms, exec_order) "
                  "VALUES (" TINOX_MIG_PH(1) ", " TINOX_MIG_PH(2) ", " TINOX_MIG_PH(3) ", " TINOX_MIG_PH(4) ", "
                  TINOX_MIG_PH(5) ", " TINOX_MIG_PH(6) ")";
#if defined(TINOX_DB_POSTGRES)
        upd_sql = "UPDATE tinox_changelog SET checksum = $1, execution_ms = $2, executed_at = CURRENT_TIMESTAMP "
                  "WHERE changeset_id = $3 AND author = $4";
#else
        upd_sql = "UPDATE tinox_changelog SET checksum = ?, execution_ms = ?, executed_at = CURRENT_TIMESTAMP "
                  "WHERE changeset_id = ? AND author = ?";
#endif
    }
    int64_t order = n_applied;
    int ran = 0;
    for (int i = 0; i < n_sets; i++) {
        if (state[i] == 0 || state[i] == 4) continue;
        int skip_stmts = baseline;
        if (!baseline && sets[i].on_fail) {
            const char* failed = tinox_mig_first_failed(conn, &sets[i]);
            if (failed) {
                const char* how = sets[i].on_fail;
                if (!strcmp(how, "HALT")) {
                    fprintf(stderr, "migrate: precondition failed for changeSet '%s' by '%s' (%s): %s\n",
                            sets[i].id, sets[i].author, sets[i].file, failed);
                    tinox_mig_unlock(conn);
                    tinox_mig_die("precondition failed (onFail: HALT); nothing further was applied "
                                  "(use onFail: CONTINUE, MARK_RAN or WARN to tolerate it)");
                } else if (!strcmp(how, "CONTINUE")) {
                    fprintf(stderr, "migrate: skipped changeSet '%s' by '%s': precondition failed (%s); it is retried on the next run\n",
                            sets[i].id, sets[i].author, failed);
                    continue;
                } else if (!strcmp(how, "MARK_RAN")) {
                    fprintf(stderr, "migrate: precondition failed for changeSet '%s' by '%s' (%s): marking it as applied without running it\n",
                            sets[i].id, sets[i].author, failed);
                    skip_stmts = 1;
                } else {
                    fprintf(stderr, "migrate: warning: precondition failed for changeSet '%s' by '%s' (%s): running it anyway\n",
                            sets[i].id, sets[i].author, failed);
                }
            }
        }
        int64_t t0 = tinox_mig_now_ms();
        if (!TINOX_MIG_BEGIN(conn)) { tinox_mig_unlock(conn); tinox_mig_die("cannot start a transaction"); }
        for (int s = 0; !skip_stmts && s < sets[i].n_stmts; s++) {
            if (!tinox_mig_exec(conn, sets[i].stmts[s], NULL, 0)) {
                TINOX_MIG_ROLLBACK(conn);
                fprintf(stderr, "migrate: changeSet '%s' by '%s' (%s) failed on: %s\n",
                        sets[i].id, sets[i].author, sets[i].file, sets[i].stmts[s]);
                tinox_mig_unlock(conn);
#if defined(TINOX_DB_MYSQL)
                // MySQL commits DDL implicitly: the statements before this one are already in the
                // database, but the changeSet has no tinox_changelog row.
                if (s > 0)
                    fprintf(stderr, "migrate: MySQL cannot roll back DDL: statements 1-%d of %d of changeSet '%s' by '%s' "
                                    "stay applied and the changeSet is NOT recorded. Repair the schema by hand "
                                    "(or `tinox db baseline` once it matches), then run the migration again\n",
                            s, sets[i].n_stmts, sets[i].id, sets[i].author);
                tinox_mig_die("migration failed; earlier changeSets stay applied, this changeSet may be partially applied");
#else
                tinox_mig_die("migration failed; this changeSet was rolled back, earlier ones stay applied");
#endif
            }
        }
        char ms[32], ord[32];
        snprintf(ms, sizeof ms, "%lld", (long long)(tinox_mig_now_ms() - t0));
        int ok;
        if (state[i] == 2) {
            const char* p[4] = { sets[i].checksum, ms, sets[i].id, sets[i].author };
            ok = tinox_mig_exec(conn, upd_sql, p, 4);
        } else {
            snprintf(ord, sizeof ord, "%lld", (long long)(++order));
            const char* p[6] = { sets[i].id, sets[i].author, sets[i].file, sets[i].checksum, ms, ord };
            ok = tinox_mig_exec(conn, ins_sql, p, 6);
        }
        if (!ok || !TINOX_MIG_COMMIT(conn)) {
            TINOX_MIG_ROLLBACK(conn);
            tinox_mig_unlock(conn);
            tinox_mig_die("cannot record the changeSet in tinox_changelog");
        }
        if (skip_stmts && !baseline) fprintf(stderr, "migrate: marked changeSet '%s' by '%s' as applied (not executed)\n", sets[i].id, sets[i].author);
        else if (baseline) fprintf(stderr, "migrate: baselined changeSet '%s' by '%s' (not executed)\n", sets[i].id, sets[i].author);
        else fprintf(stderr, "migrate: applied changeSet '%s' by '%s' (%s ms)\n", sets[i].id, sets[i].author, ms);
        ran++;
    }
    tinox_mig_unlock(conn);
    tinox_db_release_stmt_conn(conn);
    if (ran == 0 && n_sets > 0) fprintf(stderr, "migrate: database is up to date (%d changeSets)\n", n_sets);
}

static void service_close_database(void) {
#ifdef TINOX_DB_POSTGRES
    for (int i = 0; i < TINOX_DB_POOL_MAX; i++) {
        if (_tinox_pg_pool[i]) { PQfinish(_tinox_pg_pool[i]); _tinox_pg_pool[i] = NULL; }
    }
    _tinox_pg_pool_size = _tinox_pg_pool_free_count = 0;
#elif defined(TINOX_DB_SQLITE)
    if (_tinox_sqlite_db) {
        for (int i = 0; i < STMT_CACHE_SIZE; i++) {
            if (_stmt_cache[i].stmt) sqlite3_finalize(_stmt_cache[i].stmt);
            _stmt_cache[i].sql = NULL; _stmt_cache[i].stmt = NULL;
        }
        if (sqlite3_close(_tinox_sqlite_db) != SQLITE_OK) service_record_error("SQLite disposal failed", 0);
        _tinox_sqlite_db = NULL;
    }
#elif defined(TINOX_DB_MYSQL)
    for (int i = 0; i < TINOX_DB_POOL_MAX; i++) {
        if (_tinox_mysql_pool[i]) { mysql_close(_tinox_mysql_pool[i]); _tinox_mysql_pool[i] = NULL; }
    }
    _tinox_mysql_pool_size = _tinox_mysql_pool_free_count = 0;
#endif
}

// Codegen calls these only on the failed-result branch, then frees the result,
// releases the statement connection and unwinds through the normal catch/defer path.
static void tinox_orm_statement_failed(const char* operation, const char* table) {
    const char* detail = __tinox_err ? (const char*)(intptr_t)__tinox_err : "database statement failed";
    size_t size = strlen(operation) + strlen(table) + strlen(detail) + 40;
    char* message = (char*)GC_malloc(size);
    snprintf(message, size, "ORM %s failed for table '%s': %s", operation, table, detail);
    __tinox_err = (int64_t)(intptr_t)message;
}

void tinox_orm_save_failed(const char* table) {
    tinox_orm_statement_failed("save", table);
}

void tinox_orm_query_failed(const char* table) {
    tinox_orm_statement_failed("query", table);
}

// Param helpers (always available)
char** tinox_params_alloc(int64_t n) {
    return (char**)GC_malloc(sizeof(char*) * (size_t)n);
}

void tinox_params_set(char** params, int64_t idx, const char* val) {
    params[idx] = (char*)val;
}

char* tinox_int_to_param(int64_t val) {
    char* buf = (char*)GC_malloc(32);
    snprintf(buf, 32, "%ld", (long)val);
    return buf;
}

// ---- `list.contains(x.field)` in ORM filters (issue #534) ----
// A runtime-sized `col IN (...)`: ONE query for a whole batch of keys, which is how a list/detail view loads its
// related rows without a query per row. The list is capped (hard error above the cap) and padded to the next power
// of two with its last element, so only a handful of distinct SQL texts ever exist -- they are interned forever
// because the SQLite driver keeps the SQL pointer of cached statements.
#define TINOX_ORM_IN_MAX 1000

char** tinox_orm_in_params(int64_t* list, int64_t is_string, int64_t* out_k) {
    TinoxArray* arr = (TinoxArray*)list;
    int64_t len = arr ? arr->len : 0;
    if (len > TINOX_ORM_IN_MAX) {
        fprintf(stderr, "runtime error: ORM list.contains(...) with %lld keys exceeds the limit of %d; load the keys in chunks\n", (long long)len, TINOX_ORM_IN_MAX);
        exit(1);
    }
    int64_t k = 0;
    if (len > 0) { k = 1; while (k < len) k *= 2; }
    char** params = (char**)GC_malloc(sizeof(char*) * (size_t)(k > 0 ? k : 1));
    for (int64_t i = 0; i < k; i++) {
        int64_t v = arr->data[i < len ? i : len - 1];
        params[i] = is_string ? (char*)(intptr_t)v : tinox_int_to_param(v);
    }
    *out_k = k;
    return params;
}

char** tinox_orm_params_merge(char** a, int64_t na, char** b, int64_t nb) {
    char** out = (char**)GC_malloc(sizeof(char*) * (size_t)(na + nb > 0 ? na + nb : 1));
    for (int64_t i = 0; i < na; i++) out[i] = a[i];
    for (int64_t i = 0; i < nb; i++) out[na + i] = b[i];
    return out;
}

static struct { const char* tmpl; int64_t first; int64_t k; char* sql; } g_orm_in_cache[1024];
static int g_orm_in_cache_n = 0;
static pthread_mutex_t g_orm_in_mu = PTHREAD_MUTEX_INITIALIZER;

const char* tinox_orm_expand_in(const char* tmpl, int64_t first_no, int64_t k) {
    pthread_mutex_lock(&g_orm_in_mu);
    for (int i = 0; i < g_orm_in_cache_n; i++) {
        if (g_orm_in_cache[i].tmpl == tmpl && g_orm_in_cache[i].first == first_no && g_orm_in_cache[i].k == k) {
            const char* sql = g_orm_in_cache[i].sql;
            pthread_mutex_unlock(&g_orm_in_mu);
            return sql;
        }
    }
    if (g_orm_in_cache_n == 1024) {
        pthread_mutex_unlock(&g_orm_in_mu);
        fprintf(stderr, "runtime error: too many distinct ORM IN-list query shapes\n");
        exit(1);
    }
    const char* marker = strstr(tmpl, "@IN@");
    size_t head = marker ? (size_t)(marker - tmpl) : strlen(tmpl);
    size_t cap = strlen(tmpl) + (size_t)(k > 0 ? k : 1) * 24 + 16;
    char* sql = (char*)malloc(cap);
    memcpy(sql, tmpl, head);
    size_t pos = head;
    if (k == 0) {
        pos += (size_t)snprintf(sql + pos, cap - pos, "NULL");
    } else {
        for (int64_t i = 0; i < k; i++) {
            pos += (size_t)snprintf(sql + pos, cap - pos, "%s$%lld", i ? "," : "", (long long)(first_no + i));
        }
    }
    if (marker) snprintf(sql + pos, cap - pos, "%s", marker + 4);
    g_orm_in_cache[g_orm_in_cache_n].tmpl = tmpl;
    g_orm_in_cache[g_orm_in_cache_n].first = first_no;
    g_orm_in_cache[g_orm_in_cache_n].k = k;
    g_orm_in_cache[g_orm_in_cache_n].sql = sql;
    g_orm_in_cache_n++;
    pthread_mutex_unlock(&g_orm_in_mu);
    return sql;
}

// ---- Entry point ----

extern int64_t tinox_main(void);

// Error slot from the generated IR (@__tinox_err = thread_local global i64
// 0, since bug 101 -- previously a plain global that HTTP worker threads
// shared). A `throw` with no enclosing `try` parks the error value here
// and the throwing function returns with a default value;
// a `try` further up the stack consumes the slot and resets it to 0. If
// a value is still set after tinox_main returns (on the main thread),
// the throw was caught NOWHERE — that must not pass silently (bug
// 35): report it loudly on stderr and abort with a non-zero exit code. The
// storage class here MUST match the `thread_local` declaration in the generated
// IR (codegen.rs), or there's a TLS relocation mismatch at link time.
extern __thread int64_t __tinox_err;

// Definition of the function forward-declared near tinox_alloc/tinox_free
// above (Bug 140) -- placed here, after every `__thread` variable it
// registers has been declared. `GC_add_roots` on a `__thread` variable
// only adds the CALLING thread's TLS slot, so this must run once per
// thread (the `registered` guard below is itself `__thread`, so each
// thread gets its own independent one-shot check).
// Every `__thread` GC pointer a thread running Tinox code may hold (see
// Bug 140 above) -- one list, shared by register and unregister below.
#define TINOX_GC_THREAD_ROOTS(X) \
    X(_tinox_http_req_headers) \
    X(g_recv_buf) \
    X(g_resp_buf) \
    X(g_wrap_buf) \
    X(g_response) \
    X(g_request) \
    X(g_ctx) \
    X(g_json_body_ptr) \
    X(g_req_body_ptr) \
    X(g_resp_bytes_ptr) \
    X(g_req_headers_map) \
    X(g_resp_headers_map) \
    X(g_path_params_map) \
    X(g_epoll_slots) \
    X(request_instances) \
    X(task_scope) \
    X(task_frame_scope) \
    X(current_task) \
    X(service_hook_stack) \
    X(__tinox_err)

#ifndef TINOX_NO_GC
// Per-thread one-shot guard; file scope (not a function-local static) so
// tinox_gc_unregister_thread_roots can clear it and a thread can be
// attached again after a detach (issue #487).
static __thread int tinox_gc_roots_registered = 0;
#endif

static void tinox_gc_register_thread_roots(void) {
#ifndef TINOX_NO_GC
    // GC_add_roots is a Boehm-GC-specific API (<gc.h>, only included in
    // the #else branch of the TINOX_NO_GC switch near the top of this
    // file) — under TINOX_NO_GC (make asan, and every fuzz/*/build.sh
    // harness) there is no collector to register roots with in the first
    // place (plain malloc, nothing ever collected), so this is
    // unconditionally a no-op there, matching GC_INIT()'s own no-op
    // definition in that mode. Found via fuzz/*/build.sh failing to
    // compile at all ("call to undeclared function 'GC_add_roots'") —
    // this function's body was written assuming <gc.h> is always
    // included, which stopped being true the moment TINOX_NO_GC gained
    // its own code path (this function predates it).
    if (tinox_gc_roots_registered) return;
    tinox_gc_roots_registered = 1;
#define TINOX_GC_ROOT(var) GC_add_roots(&(var), (char*)&(var) + sizeof(var) + 1);
    TINOX_GC_THREAD_ROOTS(TINOX_GC_ROOT)
#undef TINOX_GC_ROOT
#endif
}

// Counterpart to tinox_gc_register_thread_roots, for a thread that is
// about to exit. The roots it registered point into THIS thread's TLS
// block, which glibc frees together with the thread's stack once the
// thread is gone (munmap'd outright once its stack cache is full, ~40MB
// -- i.e. after a few 8MB-stack threads). Left registered, the next
// collection's root scan reads that unmapped range and SIGSEGVs inside
// libgc's mark code. Found via zox (tinox-compress) spawning 32 short-
// lived block-decoder threads per batch: ~40% of runs of a 20MB
// extraction crashed in GC_malloc -> mark; 0% with GC_DONT_GC=1. Boehm
// may merge adjacent root segments, so the whole [lowest, highest) span
// of this thread's variables is removed in one call rather than one
// var at a time (GC_remove_roots only drops segments fully inside the
// given range).
static void tinox_gc_unregister_thread_roots(void) {
#ifndef TINOX_NO_GC
    char* lo = (char*)UINTPTR_MAX;
    char* hi = NULL;
#define TINOX_GC_ROOT_SPAN(var) \
    if ((char*)&(var) < lo) lo = (char*)&(var); \
    if ((char*)&(var) + sizeof(var) + 1 > hi) hi = (char*)&(var) + sizeof(var) + 1;
    TINOX_GC_THREAD_ROOTS(TINOX_GC_ROOT_SPAN)
#undef TINOX_GC_ROOT_SPAN
    GC_remove_roots(lo, hi);
    tinox_gc_roots_registered = 0;
#endif
}

// ---- Foreign threads (issue #487) ----
//
// A thread NOT created by this runtime (a GUI toolkit's worker, a C
// library's callback thread) is unknown to the collector. Before it may
// allocate Tinox heap memory, hold a pointer into it, or call anything
// that does, it must be attached; otherwise a collection neither scans its
// stack nor stops it, which is undefined behaviour. Calls nest: every
// attach that returned 0 needs one detach.
//
// Returns 0 when the calling thread was attached by this call, 1 when the
// thread was already known to the collector (the main thread, a `spawn`ed
// task) -- nothing to undo then, and detach is a harmless no-op -- and a
// negative value when the collector has no thread support.
#ifndef TINOX_NO_GC
static __thread int tinox_foreign_attach_depth = 0;
#endif

int tinox_thread_attach(void) {
#ifndef TINOX_NO_GC
    if (tinox_foreign_attach_depth > 0) {
        tinox_foreign_attach_depth++;
        return 0;
    }
    struct GC_stack_base sb;
    if (GC_get_stack_base(&sb) != GC_SUCCESS) return -1;
    int rc = GC_register_my_thread(&sb);
    if (rc == GC_DUPLICATE) return 1;
    if (rc != GC_SUCCESS) return -1;
    tinox_foreign_attach_depth = 1;
    tinox_gc_register_thread_roots();
    return 0;
#else
    return 1;
#endif
}

void tinox_thread_detach(void) {
#ifndef TINOX_NO_GC
    if (tinox_foreign_attach_depth == 0) return;
    if (--tinox_foreign_attach_depth > 0) return;
    tinox_gc_unregister_thread_roots();
    GC_unregister_my_thread();
#endif
}

int main(int argc, char** argv) {
    GC_INIT();
#ifndef TINOX_NO_GC
    // Lets foreign threads join the collector via tinox_thread_attach().
    GC_allow_register_threads();
#endif
    tinox_gc_register_thread_roots();
    // Issue #224: conn_close() now calls shutdown(fd, SHUT_RDWR) on every
    // connection teardown (not just tinox_HttpServer_listen's epoll loop,
    // which already set this individually) to unblock a concurrent
    // reader/writer on the same fd. That makes an EPIPE-on-write race
    // reachable from EVERY connection type (plain HttpServer::listenTls,
    // WsServer, AMQP, SMTP, ...), not just the one listener that happened
    // to guard against it before -- e.g. a client shutting down its end
    // right as the server's own thread is mid-write (a TLS close_notify,
    // a final response chunk) now reliably surfaces as a write() getting
    // EPIPE. The correct, standard fix for a server process is to ignore
    // SIGPIPE globally and let write()/send()/SSL_write() report EPIPE
    // via their normal return value instead of killing the process --
    // every write path here already treats a negative return as "give up
    // on this connection", so no caller needed to change.
    signal(SIGPIPE, SIG_IGN);
    // stdout is fully buffered (~4KB) by default when not attached to a
    // TTY (e.g. piped to `docker logs`, `journalctl`, `tee`, a log
    // aggregator). For long-running processes that print periodically
    // (services, loggers) this delays output indefinitely until the
    // buffer fills or the process exits — force line buffering so every
    // println()'d line reaches the pipe promptly.
    setvbuf(stdout, NULL, _IOLBF, BUFSIZ);
    _tinox_argc = argc;
    _tinox_argv = argv;
    telemetryInit();
    if (telemetry_enabled) {
        telemetry_event("service.start");
        tinox_counter_inc("tinox_service_starts_total");
    }
    task_ensure_scope();
    int64_t rc = tinox_main();
    // Process shutdown owns every remaining joinable child.
    if (__tinox_err == (int64_t)(intptr_t)task_shutdown_error) __tinox_err = 0;
    task_close_scope(task_frame_scope, 0, 1);
    atomic_store(&task_shutdown_complete, true);
    if (telemetry_enabled) {
        telemetry_event("service.stop");
        tinox_counter_inc("tinox_service_stops_total");
    }
    telemetryFlush();
    if (__tinox_err != 0) {
        // throw is type-checked as String-or-Error; usually a String.
        const char* msg = (const char*)(intptr_t)__tinox_err;
        fprintf(stderr, "Uncaught error: %s\n", msg ? msg : "(unknown)");
        return 1;
    }
    return (int)rc;
}

// Float classification and constants
int64_t mathIsNan(double x) { return isnan(x) ? 1 : 0; }
int64_t mathIsInfinite(double x) { return isinf(x) ? 1 : 0; }
int64_t mathIsNormal(double x) { return isnormal(x) ? 1 : 0; }
double mathNan(void) { return NAN; }
double mathInf(void) { return INFINITY; }

// Env listing
char* envDump(void) {
    extern char** environ;
    size_t total = 1;
    for (int i = 0; environ[i]; i++) total += strlen(environ[i]) + 1;
    char* buf = GC_malloc(total);
    char* p = buf;
    for (int i = 0; environ[i]; i++) {
        size_t len = strlen(environ[i]);
        memcpy(p, environ[i], len); p[len] = '\n'; p += len + 1;
    }
    *p = '\0';
    return buf;
}

// Time
int64_t currentTimeSecs(void) { return (int64_t)time(NULL); }

int64_t now(void) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    return (int64_t)ts.tv_sec * 1000LL + ts.tv_nsec / 1000000LL;
}

void sleep_ms(int64_t ms) {
    if (ms < 0 || ms > INT64_MAX - task_clock_ms()) task_fatal("sleep requires a nonnegative duration");
    int64_t end = task_clock_ms() + ms;
    do {
        taskCheckCancellation();
        if (__tinox_err) return;
        int64_t remaining = end - task_clock_ms();
        if (remaining <= 0) return;
        if (remaining > 10) remaining = 10;
        struct timespec ts = { 0, remaining * 1000000 };
        while (nanosleep(&ts, &ts) < 0 && errno == EINTR) {}
    } while (1);
}

char* strftimeStr(const char* fmt, int64_t t) {
    time_t ts = (time_t)t;
    struct tm tm_buf;
    gmtime_r(&ts, &tm_buf);
    char buf[256];
    strftime(buf, sizeof(buf), fmt, &tm_buf);
    return GC_strdup(buf);
}

int64_t fromdateStr(const char* s) {
    struct tm tm_buf = {0};
    // Try "%Y-%m-%dT%H:%M:%SZ" and "%Y-%m-%dT%H:%M:%S"
    char* r = strptime(s, "%Y-%m-%dT%H:%M:%SZ", &tm_buf);
    if (!r) r = strptime(s, "%Y-%m-%dT%H:%M:%S", &tm_buf);
    if (!r) r = strptime(s, "%Y-%m-%d", &tm_buf);
    if (!r) return 0;
    return (int64_t)timegm(&tm_buf);
}

void printStderr(const char* msg) { fputs(msg, stderr); fputc('\n', stderr); }

int64_t isStdinTty(void) { return isatty(STDIN_FILENO) ? 1 : 0; }

int64_t isStdoutTty(void) { return isatty(STDOUT_FILENO) ? 1 : 0; }
