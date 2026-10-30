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
    if (g_tinox_ws_debug < 0) {
        const char* v = getenv("TINOX_WS_DEBUG");
        g_tinox_ws_debug = (v && v[0] == '1') ? 1 : 0;
    }
    return g_tinox_ws_debug == 1;
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
    if (neg) val = -val;
    do { buf[len++] = '0' + (val % 10); val /= 10; } while (val > 0);
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
} TinoxLimits;

static TinoxLimits g_limits;
static pthread_once_t g_limits_once = PTHREAD_ONCE_INIT;
static atomic_llong g_active_tasks, g_peak_tasks, g_rejected_tasks;
static atomic_llong g_open_conns, g_peak_conns, g_rejected_conns;
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
        "\"queueWaiting\":%lld,\"queueWaitMaxMs\":%lld,\"queueTimeouts\":%lld}",
        (long long)l->cpus, l->cpu_source, (long long)l->http_workers,
        (long long)l->max_connections, (long long)l->max_tasks,
        (long long)l->max_ws_connections, (long long)l->queue_wait_ms,
        (long long)atomic_load(&g_open_conns), (long long)atomic_load(&g_rejected_conns),
        (long long)atomic_load(&g_active_tasks), (long long)atomic_load(&g_rejected_tasks),
        (long long)atomic_load(&g_active_ws), (long long)atomic_load(&g_rejected_ws),
        (long long)atomic_load(&g_waiting), (long long)atomic_load(&g_wait_max_ms),
        (long long)atomic_load(&g_wait_timeouts));
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
static void* task_spawn_impl(void* (*fn)(void*), void* args, int budgeted) {
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
void* tinox_task_spawn(void* (*fn)(void*), void* args) { return task_spawn_impl(fn, args, 1); }
int64_t tinox_task_await(void* handle) {
    TinoxTask* task = task_lookup((uintptr_t)handle);
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
    taskDetach((int64_t)(uintptr_t)task_spawn_impl(fn, args, 0));
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
typedef struct { int fd; void* ssl; pthread_mutex_t writeLock; bool wsCompressed; atomic_int inflight; atomic_bool closing; } TinoxConn;   // ssl==NULL => plaintext
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
#define TINOX_MAX_BODY   (4 * 1024 * 1024)  /* 4 MB */

// Reads a full HTTP/1.1 request from the connection into g_recv_buf (static, not freed by caller).
// Works for both plaintext (ssl==NULL) and TLS connections via conn_recv.
static char* conn_read_request(TinoxConn* c) {
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
        // Check for Content-Length — scan line by line, no strcasestr overhead
        char* cl = NULL;
        for (char* s = buf; *s; ) {
            while (*s && *s != '\n') s++;
            if (*s) s++;
            if ((s[0]=='C'||s[0]=='c') && (s[1]=='o'||s[1]=='O') && (s[2]=='n'||s[2]=='N') &&
                (s[3]=='t'||s[3]=='T') && (s[4]=='e'||s[4]=='E') && (s[5]=='n'||s[5]=='N') &&
                (s[6]=='t'||s[6]=='T') &&  s[7]=='-' &&
                (s[8]=='L'||s[8]=='l') && (s[9]=='e'||s[9]=='E') && (s[10]=='n'||s[10]=='N') &&
                (s[11]=='g'||s[11]=='G') && (s[12]=='t'||s[12]=='T') && (s[13]=='h'||s[13]=='H') &&
                s[14]==':') { cl = s; break; }
        }
        if (cl) {
            long body_len = atol(cl + 15);
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
            if (body_len < 0 || body_len > TINOX_MAX_BODY) {
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

static TinoxHttpResponse* http_request(const char* method, const char* url, const char* body) {
    TinoxHttpResponse* resp = (TinoxHttpResponse*)malloc(sizeof(TinoxHttpResponse));
    resp->status = 0;
    resp->body = GC_strdup("");
    resp->headers = GC_strdup("");

    char host[256], path[2048];
    int port;
    int is_https;
    if (!http_parse_url(url, host, sizeof(host), &port, path, sizeof(path), &is_https)) return resp;

#ifndef TINOX_TLS
    if (is_https) return resp; // TINOX_TLS=0: https:// not available, same "empty response" contract as any other connect failure
#endif

    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return resp;

    char port_str[16];
    snprintf(port_str, sizeof(port_str), "%d", port);
    struct addrinfo hints, *res = NULL;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_INET;
    hints.ai_socktype = SOCK_STREAM;
    if (getaddrinfo(host, port_str, &hints, &res) != 0 || !res) { close(fd); return resp; }
    if (connect(fd, res->ai_addr, res->ai_addrlen) != 0) { freeaddrinfo(res); close(fd); return resp; }
    freeaddrinfo(res);

    size_t body_len = body ? strlen(body) : 0;
    const char* extra = _tinox_http_req_headers ? _tinox_http_req_headers : "";
    char correlation[256] = "";
    if (telemetry_enabled && telemetry_context.trace[0]) {
        snprintf(correlation, sizeof(correlation), "traceparent: 00-%s-%s-%02x\r\nx-request-id: %s\r\n",
                 telemetry_context.trace, telemetry_context.span, telemetry_context.sampled, telemetry_context.request);
    }
    size_t req_cap = strlen(method) + strlen(path) + strlen(host) + strlen(extra) + strlen(correlation) + body_len + 256;
    char* req = (char*)malloc(req_cap);
    int req_len = snprintf(req, req_cap,
        "%s %s HTTP/1.1\r\nHost: %s\r\nConnection: close\r\n%s%s"
        "Content-Length: %zu\r\n\r\n",
        method, path, host, extra, correlation, body_len);
    if (body_len) {
        memcpy(req + req_len, body, body_len);
        req_len += (int)body_len;
    }

    char* raw;
#ifdef TINOX_TLS
    if (is_https) {
        // A custom CA/client-cert/insecure-skip-verify config (set via
        // httpSetTlsCaCertFile/httpSetTlsClientCertFile/
        // httpSetTlsInsecureSkipVerify -- tinox.core.kubernetes' auth
        // layer is the first real caller) needs its own SSL_CTX per
        // request rather than the shared g_tls_client_ctx: that ctx is
        // lazily built once with the system trust store and no client
        // identity, and is (deliberately) shared across every plain
        // https:// caller in the process, so mutating it per-call would
        // leak one call's cluster CA/client cert into every other
        // caller's requests on the same thread/process.
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
                        fprintf(stderr, "http_request: failed to load CA cert file %s\n", _tinox_tls_ca_cert_path);
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
                        fprintf(stderr, "http_request: failed to load client cert/key (%s / %s)\n",
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
        if (!ctx) { close(fd); free(req); return resp; }
        SSL* ssl = SSL_new(ctx);
        if (!ssl) { if (use_custom_ctx) SSL_CTX_free(ctx); close(fd); free(req); return resp; }
        SSL_set_tlsext_host_name(ssl, host); // SNI
        if (_tinox_tls_insecure_skip_verify) {
            SSL_set_verify(ssl, SSL_VERIFY_NONE, NULL);
        } else {
            SSL_set_verify(ssl, SSL_VERIFY_PEER, NULL);
            SSL_set1_host(ssl, host); // hostname must match the peer certificate
        }
        SSL_set_fd(ssl, fd);
        if (SSL_connect(ssl) <= 0) {
            ERR_print_errors_fp(stderr);
            SSL_free(ssl);
            if (use_custom_ctx) SSL_CTX_free(ctx);
            close(fd);
            free(req);
            return resp;
        }
        int wr_total = 0;
        while (wr_total < req_len) {
            int w = SSL_write(ssl, req + wr_total, req_len - wr_total);
            if (w <= 0) break;
            wr_total += w;
        }
        raw = http_recv_all_tls(ssl);
        SSL_shutdown(ssl);
        SSL_free(ssl);
        if (use_custom_ctx) SSL_CTX_free(ctx);
        close(fd);
    } else
#endif
    {
        ssize_t sent_total = 0;
        while (sent_total < req_len) {
            ssize_t s = send(fd, req + sent_total, (size_t)(req_len - sent_total), 0);
            if (s <= 0) break;
            sent_total += s;
        }
        raw = http_recv_all(fd);
        close(fd);
    }
    free(req);

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
    return resp;
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
        const char* valStr = (const char*)(intptr_t)tinox_map_get(headersMap, keyStr);
        nva[i + 1].name = (const uint8_t*)lowerKey;
        nva[i + 1].namelen = strlen(lowerKey);
        nva[i + 1].value = (const uint8_t*)(valStr ? valStr : "");
        nva[i + 1].valuelen = valStr ? strlen(valStr) : 0;
        nva[i + 1].flags = NGHTTP3_NV_FLAG_NONE;
    }

    size_t bodyLen = body ? strlen(body) : 0;
    slot->respBody = bodyLen > 0 ? strdup(body) : NULL;
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
// Unmanaged (no @Service) request boundary: an uncaught handler exception is a
// failed request, never a 200 with a default value, and must not stay pending.
void tinox_request_failed(int64_t context) {
    // Cancellation keeps propagating to the owning task instead of becoming a 500.
    if (__tinox_err && !task_is_cancellation(__tinox_err)) {
        fprintf(stderr, "tinox: request handler error: %s\n", (char*)(intptr_t)__tinox_err);
        int64_t* response = (int64_t*)(intptr_t)((int64_t*)(intptr_t)context)[1];
        response[0] = 500; response[2] = (int64_t)(intptr_t)"Request failed";
        __tinox_err = 0;
    }
}
void tinox_service_request_done(int64_t context) {
    if (__tinox_err) {
        fprintf(stderr, "tinox: request handler error: %s\n", (char*)(intptr_t)__tinox_err);
        int64_t* response = (int64_t*)(intptr_t)((int64_t*)(intptr_t)context)[1];
        response[0] = 500; response[2] = (int64_t)(intptr_t)"Request failed";
        __tinox_err = 0;
    }
    tinox_service_work_exit();
}

static void tinox_handle_one(TinoxHttpServer* srv, int64_t client_fd, int* keep_alive_out) {
    *keep_alive_out = 0;
    char* raw_req = httpServerReadRequest(client_fd);
    if (!raw_req || !raw_req[0]) return;

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
    for (int i = 0; i < srv->route_count; i++) {
        if (strcmp(srv->routes[i].method, method) != 0) continue;
        if (route_matches(srv->routes[i].path, path, &g_path_params_map)) {
            handler = srv->routes[i].handler;
            break;
        }
        tinox_map_reset(&g_path_params_map);
    }

    // Parse HTTP headers — normalize keys to Title-Case, track Connection header
    tinox_map_reset(&g_req_headers_map);
    int trace_headers = 0, request_headers = 0;
    int req_close = 0; // HTTP/1.1 default: keep-alive
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
        if (handler) handler((int64_t)g_ctx);
        if (__tinox_err && (service_enabled() || !task_is_cancellation(__tinox_err))) {
            fprintf(stderr, "tinox: request handler error: %s\n", (char*)(intptr_t)__tinox_err);
            g_response[0] = 500; g_response[2] = (int64_t)(intptr_t)"Request failed"; __tinox_err = 0;
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
    TinoxMap* rhm = (TinoxMap*)resp_hdr_map;
    if (rhm && rhm->len > 0) {
        int64_t* hkeys_h = tinox_map_keys(resp_hdr_map);
        int64_t* hkeys = ((TinoxArray*)hkeys_h)->data;
        int64_t hklen = ((TinoxArray*)hkeys_h)->len;
        for (int64_t hi = 0; hi < hklen; hi++) {
            const char* hk = (const char*)(uintptr_t)hkeys[hi];
            const char* hv = (const char*)(uintptr_t)tinox_map_get(resp_hdr_map, hk);
            if (hk && hv) {
                size_t kl = strlen(hk), vl = strlen(hv);
                if (hdr_off + kl + vl + 4 < sizeof(hdr_buf)) {
                    memcpy(hdr_buf + hdr_off, hk, kl); hdr_off += kl;
                    hdr_buf[hdr_off++] = ':'; hdr_buf[hdr_off++] = ' ';
                    memcpy(hdr_buf + hdr_off, hv, vl); hdr_off += vl;
                    hdr_buf[hdr_off++] = '\r'; hdr_buf[hdr_off++] = '\n';
                }
            }
        }
    }
    if (status != 204 && tinox_map_contains(resp_hdr_map, "Content-Type") == 0) {
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
    memcpy(out, body, body_len); out += body_len;
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
                int keep_alive = 0;
                tinox_handle_one(srv, (int64_t)fd, &keep_alive);
                if (keep_alive) {
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
// Reads key=value pairs from application.properties in the current directory.

#define TINOX_CONFIG_MAX_ENTRIES 256
#define TINOX_CONFIG_MAX_LINE    1024

typedef struct { char* key; char* value; } TinoxConfigEntry;

static TinoxConfigEntry tinox_config_entries[TINOX_CONFIG_MAX_ENTRIES];
static int              tinox_config_count = -1; // -1 = not loaded

static void tinox_config_load(void) {
    tinox_config_count = 0;
    FILE* f = fopen("application.properties", "r");
    if (!f) return;
    char line[TINOX_CONFIG_MAX_LINE];
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
            tinox_config_entries[tinox_config_count].key   = strdup(key);
            tinox_config_entries[tinox_config_count].value = strdup(val);
            tinox_config_count++;
        }
    }
    fclose(f);
}

static const char* tinox_config_lookup(const char* key) {
    if (tinox_config_count < 0) tinox_config_load();
    for (int i = 0; i < tinox_config_count; i++) {
        if (strcmp(tinox_config_entries[i].key, key) == 0)
            return tinox_config_entries[i].value;
    }
    return "";
}

char* tinox_config_get(const char* key) {
    return (char*)tinox_config_lookup(key);
}

int64_t tinox_config_get_int(const char* key) {
    const char* v = tinox_config_lookup(key);
    if (!v || *v == '\0') return 0;
    return (int64_t)atoll(v);
}

int64_t tinox_config_get_bool(const char* key) {
    const char* v = tinox_config_lookup(key);
    if (!v || *v == '\0') return 0;
    return (strcmp(v, "true") == 0 || strcmp(v, "1") == 0 || strcmp(v, "yes") == 0) ? 1 : 0;
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
        jsonBuilderAddString(b, tinox_config_entries[i].key, tinox_config_entries[i].value);
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
static void tinox_metrics_append(size_t* pos, size_t cap, int n) {
    if (n < 0) return;
    *pos += (size_t)n;
    if (*pos > cap) *pos = cap;
}

char* tinox_metrics_prometheus(void) {
    pthread_mutex_lock(&_tinox_metrics_mu);
    // Upper bound per entry: histogram lines repeat a (up to 255-byte)
    // name 5 times plus ~200 bytes of fixed text -- comfortably under 2048.
    size_t cap = (size_t)(_tinox_counter_n + _tinox_gauge_n + 1) * 2048 + (size_t)_tinox_histogram_n * 8192 + 4096 + 64;
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
    pthread_mutex_unlock(&_tinox_metrics_mu);
    return buf;
}

// ---- Database / ORM runtime ----
// Compiled only when libpq is available (postgres driver).
// SQLite and MySQL variants follow the same interface.

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
static __thread PGconn* _tinox_db_tx_conn = NULL;

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

void* tinox_db_tx_begin(void) {
    PGconn* c = _tinox_pg_pool_acquire();
    PGresult* res = PQexec(c, "BEGIN");
    if (PQresultStatus(res) != PGRES_COMMAND_OK) {
        fprintf(stderr, "BEGIN failed: %s\n", PQerrorMessage(c));
    }
    PQclear(res);
    _tinox_db_tx_conn = c;
    return c;
}

void tinox_db_tx_commit(void) {
    if (_tinox_db_tx_conn == NULL) return;
    PGresult* res = PQexec(_tinox_db_tx_conn, "COMMIT");
    if (PQresultStatus(res) != PGRES_COMMAND_OK) {
        fprintf(stderr, "COMMIT failed: %s\n", PQerrorMessage(_tinox_db_tx_conn));
    }
    PQclear(res);
    _tinox_pg_pool_release(_tinox_db_tx_conn);
    _tinox_db_tx_conn = NULL;
}

void tinox_db_tx_rollback(void) {
    if (_tinox_db_tx_conn == NULL) return;
    PGresult* res = PQexec(_tinox_db_tx_conn, "ROLLBACK");
    if (PQresultStatus(res) != PGRES_COMMAND_OK) {
        fprintf(stderr, "ROLLBACK failed: %s\n", PQerrorMessage(_tinox_db_tx_conn));
    }
    PQclear(res);
    _tinox_pg_pool_release(_tinox_db_tx_conn);
    _tinox_db_tx_conn = NULL;
}

bool tinox_db_tx_active(void) {
    return _tinox_db_tx_conn != NULL;
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
        fprintf(stderr, "Query error: %s\nSQL: %s\n", PQresultErrorMessage(res), sql);
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
    return v ? (int64_t)atoll(v) : 0LL;
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

    // SQLite keeps its pre-#191 single-connection model -- pool_size is
    // accepted (uniform driver-layer signature, see the Postgres block
    // above) but ignored; no pooling/transactions for this driver yet,
    // see the tinox_db_tx_* stubs below.
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
}

void* tinox_db_acquire_stmt_conn(void) {
    if (_tinox_sqlite_db == NULL) {
        fprintf(stderr, "sqlite: database connection was never initialized (no [database] config?)\n");
        exit(1);
    }
    return _tinox_sqlite_db;
}
void  tinox_db_release_stmt_conn(void* conn) { (void)conn; }

// @Transactional is a hard compile error for this driver (see the driver
// check in tinox/src/main.rs) -- these exist only so a fully-linked binary
// never has an undefined symbol. A real call here would mean that check
// was bypassed, so fail loudly rather than silently no-op.
void* tinox_db_tx_begin(void) {
    fprintf(stderr, "@Transactional is not supported for the sqlite driver\n");
    exit(1);
}
void tinox_db_tx_commit(void) {
    fprintf(stderr, "@Transactional is not supported for the sqlite driver\n");
    exit(1);
}
void tinox_db_tx_rollback(void) {
    fprintf(stderr, "@Transactional is not supported for the sqlite driver\n");
    exit(1);
}
bool tinox_db_tx_active(void) { return false; }

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

void* tinox_db_exec(void* conn, const char* sql, const char** params, int64_t n_params) {
    pthread_mutex_lock(&_tinox_sqlite_mu);
    sqlite3* db = (sqlite3*)conn;
    sqlite3_stmt* stmt = _stmt_cache_get(sql);
    if (stmt) {
        sqlite3_reset(stmt);
        sqlite3_clear_bindings(stmt);
    } else {
        if (sqlite3_prepare_v2(db, sql, -1, &stmt, NULL) != SQLITE_OK) {
            fprintf(stderr, "SQLite prepare error: %s\n", sqlite3_errmsg(db));
            pthread_mutex_unlock(&_tinox_sqlite_mu);
            return NULL;
        }
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
        fprintf(stderr, "SQLite error: %s\nSQL: %s\n", sqlite3_errmsg(db), sql);
        sqlite3_reset(stmt);
        if (rows) free(rows);
        pthread_mutex_unlock(&_tinox_sqlite_mu);
        return NULL;
    }
    // Do NOT finalize — statement is cached for reuse

    TinoxSqliteResult* res = (TinoxSqliteResult*)GC_malloc(sizeof(TinoxSqliteResult));
    res->n_cols = n_cols;
    res->n_rows = n_rows;
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

#elif defined(TINOX_DB_MYSQL)

// ---- MySQL driver ----
#include <mysql/mysql.h>

static MYSQL* _tinox_mysql_conn = NULL;
static pthread_mutex_t _tinox_mysql_mu = PTHREAD_MUTEX_INITIALIZER;

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

    // MySQL keeps its pre-#191 single-connection model -- pool_size is
    // accepted (uniform driver-layer signature, see the Postgres block
    // above) but ignored; no pooling/transactions for this driver yet,
    // see the tinox_db_tx_* stubs below.
    (void)pool_size;
    _tinox_mysql_conn = mysql_init(NULL);
    if (!_tinox_mysql_conn) {
        fprintf(stderr, "MySQL init failed\n");
        if (service_enabled()) { service_record_error("MySQL initialization failed", 0); return; }
        exit(1);
    }
    char host[256], user[256], pass[256], db[256];
    unsigned int port;
    _parse_mysql_url(url, host, user, pass, db, &port);
    if (!mysql_real_connect(_tinox_mysql_conn, host, user, pass, db, port, NULL, 0)) {
        fprintf(stderr, "MySQL connection failed: %s\n", mysql_error(_tinox_mysql_conn));
        mysql_close(_tinox_mysql_conn); _tinox_mysql_conn = NULL;
        if (service_enabled()) { service_record_error("MySQL initialization failed", 0); return; }
        exit(1);
    }
}

typedef struct {
    int n_cols;
    int n_rows;
    char** data;   // row-major: data[row * n_cols + col]
} TinoxMysqlResult;

void* tinox_db_acquire_stmt_conn(void) {
    if (_tinox_mysql_conn == NULL) {
        fprintf(stderr, "MySQL: database connection was never initialized (no [database] config?)\n");
        exit(1);
    }
    return _tinox_mysql_conn;
}
void  tinox_db_release_stmt_conn(void* conn) { (void)conn; }

// @Transactional is a hard compile error for this driver (see the driver
// check in tinox/src/main.rs) -- these exist only so a fully-linked binary
// never has an undefined symbol. A real call here would mean that check
// was bypassed, so fail loudly rather than silently no-op.
void* tinox_db_tx_begin(void) {
    fprintf(stderr, "@Transactional is not supported for the mysql driver\n");
    exit(1);
}
void tinox_db_tx_commit(void) {
    fprintf(stderr, "@Transactional is not supported for the mysql driver\n");
    exit(1);
}
void tinox_db_tx_rollback(void) {
    fprintf(stderr, "@Transactional is not supported for the mysql driver\n");
    exit(1);
}
bool tinox_db_tx_active(void) { return false; }

void* tinox_db_exec(void* conn, const char* sql, const char** params, int64_t n_params) {
    MYSQL_STMT* stmt = mysql_stmt_init((MYSQL*)conn);
    if (mysql_stmt_prepare(stmt, sql, (unsigned long)strlen(sql)) != 0) {
        fprintf(stderr, "MySQL prepare error: %s\n", mysql_stmt_error(stmt));
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
        fprintf(stderr, "MySQL execute error: %s\n", mysql_stmt_error(stmt));
        if (bind)    free(bind);
        if (lengths) free(lengths);
        mysql_stmt_close(stmt);
        return NULL;
    }

    MYSQL_RES* meta = mysql_stmt_result_metadata(stmt);
    int n_cols = meta ? mysql_num_fields(meta) : 0;
    mysql_stmt_store_result(stmt);
    int n_rows = (int)mysql_stmt_num_rows(stmt);

    TinoxMysqlResult* res = (TinoxMysqlResult*)GC_malloc(sizeof(TinoxMysqlResult));
    res->n_cols = n_cols;
    res->n_rows = n_rows;
    res->data   = n_cols * n_rows > 0
        ? (char**)GC_malloc(sizeof(char*) * (size_t)(n_cols * n_rows))
        : NULL;

    if (n_cols > 0 && n_rows > 0) {
        MYSQL_BIND* out_bind = (MYSQL_BIND*)calloc((size_t)n_cols, sizeof(MYSQL_BIND));
        char** bufs    = (char**)calloc((size_t)n_cols, sizeof(char*));
        unsigned long* out_len = (unsigned long*)calloc((size_t)n_cols, sizeof(unsigned long));
        for (int c = 0; c < n_cols; c++) {
            bufs[c] = (char*)GC_malloc(512);
            out_bind[c].buffer_type   = MYSQL_TYPE_STRING;
            out_bind[c].buffer        = bufs[c];
            out_bind[c].buffer_length = 511;
            out_bind[c].length        = &out_len[c];
        }
        mysql_stmt_bind_result(stmt, out_bind);
        for (int r = 0; r < n_rows; r++) {
            mysql_stmt_fetch(stmt);
            for (int c = 0; c < n_cols; c++) {
                bufs[c][out_len[c]] = '\0';
                res->data[r * n_cols + c] = GC_strdup(bufs[c]);
            }
        }
        free(out_bind);
        free(bufs);
        free(out_len);
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
    return res->data[(int)row * res->n_cols + (int)col];
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

#else
// Stub implementations when no DB driver is selected — prevent link errors.
void  tinox_db_pool_init(const char* url, int64_t pool_size)                  { (void)url;(void)pool_size; }
void* tinox_db_acquire_stmt_conn(void)                                        { return NULL; }
void  tinox_db_release_stmt_conn(void* conn)                                  { (void)conn; }
void* tinox_db_tx_begin(void)                                                 { return NULL; }
void  tinox_db_tx_commit(void)                                                { }
void  tinox_db_tx_rollback(void)                                              { }
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

static int tinox_mig_exec(void* conn, const char* sql, const char** params, int64_t n) {
    void* r = tinox_db_exec(conn, sql, params, n);
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
    void* r = tinox_db_exec(conn, sql, params, 1);
    int exists = (r && !tinox_db_result_failed(r)) ? tinox_db_getval_int(r, 0, 0) > 0 : 0;
    if (r) tinox_db_free(r);
    return exists;
}

// Runs a query that returns one COUNT-like value; a failing query is a hard error, never "false".
static int64_t tinox_mig_scalar_int(void* conn, const char* sql, const char** params, int64_t n) {
    void* r = tinox_db_exec(conn, sql, params, n);
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
        void* r = tinox_db_exec(conn, a, NULL, 0);
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
        void* r = tinox_db_exec(conn, "SELECT COUNT(*) FROM tinox_changelog_lock", NULL, 0);
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
    void* r = tinox_db_exec(conn, sql, NULL, 0);
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
    void* r = tinox_db_exec(conn, "SELECT changeset_id, author, checksum FROM tinox_changelog ORDER BY exec_order", NULL, 0);
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
    if (_tinox_mysql_conn) { mysql_close(_tinox_mysql_conn); _tinox_mysql_conn = NULL; }
#endif
}

// ORM save() hit a failed INSERT/UPDATE (the driver already logged the
// SQL error). Never continue with a half-saved entity.
void tinox_orm_save_failed(const char* table) {
    fprintf(stderr, "ORM save failed for table '%s' (see database error above); "
                    "unassigned String fields are bound as NULL\n", table);
    exit(1);
}

void tinox_orm_query_failed(const char* table) {
    fprintf(stderr, "ORM query failed for table '%s' (see database error above)\n", table);
    exit(1);
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
    X(g_req_headers_map) \
    X(g_resp_headers_map) \
    X(g_path_params_map) \
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
