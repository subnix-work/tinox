// Compile separately: this is a real native pthread without runtime redirects.
#include <errno.h>
#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/wait.h>
#include "runtime.h"

extern void gcCollect(void);
extern char* tinox_json_render(int64_t* object, void (*write_fields)(int64_t*, char*));
extern void jsonBuilderAddRaw(char* handle, const char* key, const char* raw);
extern void* tinox_alloc_typed(size_t bytes, uint64_t trace);
extern int64_t gcStat(int64_t kind);
extern size_t tinox_gc_swept_bytes(void);
static _Thread_local char* slice;
static int* startup_root;

// Generated DI startup uses the default ELF constructor priority as well.
__attribute__((constructor)) static void allocate_before_main(void) {
    startup_root = tinox_alloc(sizeof(*startup_root));
    *startup_root = 712;
}
static int ready[2], release_worker[2];

static void transfer(int fd, int send) {
    char byte = 'x';
    ssize_t rc;
    do { rc = send ? write(fd, &byte, 1) : read(fd, &byte, 1); } while (rc < 0 && errno == EINTR);
    if (rc != 1) abort();
}

__attribute__((noinline)) static void install_slice(void) {
    char* base = tinox_alloc(1024 * 1024);
    slice = base + 31;
    strcpy(slice, "native interior TLS root");
}

static void* worker(void* unused) {
    (void)unused;
    if (tinox_thread_attach() != 0 || tinox_thread_attach() != 0) abort();
    install_slice();
    transfer(ready[1], 1);
    transfer(release_worker[0], 0); // stopped by the collector inside a syscall
    if (strcmp(slice, "native interior TLS root")) abort();
    slice = NULL;
    // The entire cycle becomes unreachable when this thread detaches and exits.
    void** a = tinox_alloc(128 * 1024);
    void** b = tinox_alloc(128 * 1024);
    *a = b;
    *b = a;
    // Fill multiple slabs with traceable objects reachable only from the cycle.
    for (unsigned i = 0; i < 256; i++) {
        void** node = tinox_alloc_typed(512, 1);
        *node = a[1];
        a[1] = node;
    }
    tinox_thread_detach();
    tinox_thread_detach();
    return NULL;
}

// Fill several slabs, reopen full ones by freeing alternating slots, then
// allocate into their holes. Test the exact size boundaries and large fallback.
static void exercise_slots(void) {
    static const size_t sizes[] = {1, 16, 17, 31, 32, 63, 64, 127, 128, 255, 256, 511, 512, 513};
    enum { count = 3000 };
    unsigned char** roots = tinox_alloc(count * sizeof(*roots));
    for (int pass = 0; pass < 2; pass++) {
        for (int i = 0; i < count; i++) {
            size_t n = sizes[i % (sizeof(sizes)/sizeof(*sizes))];
            if (pass && i % 2) continue;
            roots[i] = tinox_alloc(n);
            for (size_t j = 0; j < n; j++) if (roots[i][j]) abort();
            memset(roots[i], 0x5a, n);
        }
        gcCollect();
        for (int i = 0; i < count; i++) {
            size_t n = sizes[i % (sizeof(sizes)/sizeof(*sizes))];
            if (roots[i][0] != 0x5a || roots[i][n-1] != 0x5a) abort();
            if (pass || !(i % 2)) {
                tinox_free(roots[i]);
                roots[i] = NULL;
            }
        }
    }
    tinox_free(roots);
}

// Feed runs across slab boundaries, including vacant slots and one-past
// references. Then reverse the traversal and retain each block by its interior.
static void exercise_atomic_runs(void) {
    enum { count = 4096, stride = 32 };
    uintptr_t* roots = tinox_alloc(count * sizeof(*roots));
    for (unsigned i = 0; i < count; i++) {
        unsigned* value = tinox_alloc_typed(stride, 0);
        *value = i + 1;
        roots[i] = (uintptr_t)value;
    }
    for (unsigned i = 7; i < count; i += 13) {
        tinox_free((void*)roots[i]);
        // Keep the integer address in the scanned sequence.
    }
    for (unsigned i = 0; i < count; i += 19) {
        if (i % 13 != 7) roots[i] += stride;
    }
    gcCollect();
    for (unsigned i = 0; i < count; i++) {
        if (i % 13 == 7) {
            roots[i] = 0;
            continue;
        }
        uintptr_t base = roots[i] - (i % 19 == 0 ? stride : 0);
        if (*(unsigned*)base != i + 1) abort();
        roots[i] = base + 3;
    }
    for (unsigned i = 0; i < count / 2; i++) {
        uintptr_t tmp = roots[i];
        roots[i] = roots[count - 1 - i];
        roots[count - 1 - i] = tmp;
    }
    gcCollect();
    for (unsigned i = 0; i < count; i++) {
        if (roots[i] && *(unsigned*)(roots[i] - 3) != count - i) abort();
    }
    tinox_free(roots);
}

// Collect while the serializer frame holds its only reference to spilled
// storage, and again after the completed string has escaped the frame.
static void write_json_field(int64_t* length, char* builder) {
    char value[1027];
    value[0] = '"';
    memset(value + 1, 'x', *length);
    value[*length + 1] = '"';
    value[*length + 2] = 0;
    jsonBuilderAddRaw(builder, "v", value);
    gcCollect();
}

static void exercise_json_frames(void) {
    int64_t lengths[] = {0, 118, 119, 120, 121, 256, 1024};
    char* results[sizeof(lengths) / sizeof(*lengths)];
    for (size_t i = 0; i < sizeof(lengths) / sizeof(*lengths); i++)
        results[i] = tinox_json_render(&lengths[i], write_json_field);
    gcCollect();
    for (size_t i = 0; i < sizeof(lengths) / sizeof(*lengths); i++) {
        const char* value = results[i];
        if (strncmp(value, "{\"v\":\"", 6) ||
            strspn(value + 6, "x") != (size_t)lengths[i] ||
            strcmp(value + 6 + lengths[i], "\"}")) abort();
        tinox_free(results[i]);
    }
}

int64_t native_roots(void) {
    exercise_json_frames();
    gcCollect();
    if (*startup_root != 712) abort();
    exercise_atomic_runs();
    exercise_slots();
    pid_t child = fork();
    if (child < 0) abort();
    if (!child) { tinox_alloc(16); _exit(1); }
    int status;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 125) abort();
    if (pipe(ready) || pipe(release_worker)) abort();
    pthread_t tid;
    if (pthread_create(&tid, NULL, worker, NULL)) abort();
    transfer(ready[0], 0);
    gcCollect();
    if (gcStat(0) < 1024 * 1024) abort(); // the worker still owns its block
    transfer(release_worker[1], 1);
    if (pthread_join(tid, NULL)) abort();
    size_t before = tinox_gc_swept_bytes();
    int64_t heap_before = gcStat(0);
    gcCollect(); // no thread-local or stack ranges from the exited worker
    close(ready[0]); close(ready[1]); close(release_worker[0]); close(release_worker[1]);
    size_t swept = tinox_gc_swept_bytes() - before;
    return swept >= 384 * 1024 && gcStat(0) == heap_before - (int64_t)swept;
}

// Int64 is a documented FFI handle representation. Keep the only reference
// to a small slot one byte past its end, including at a slab slot boundary.
int64_t make_handle(void) {
    unsigned char* p = tinox_alloc(32);
    p[0] = 42;
    return (intptr_t)(p + 32);
}
int64_t check_handle(int64_t handle) {
    return *((unsigned char*)(intptr_t)handle - 32);
}
// A genuinely numeric slot must not retain a coincidental pointer bit pattern.
// Construct it on a worker so no stale native stack copy is present at collection.
double pointer_bits_as_score(void) {
    uintptr_t address = (uintptr_t)tinox_alloc(1024 * 1024);
    double number;
    memcpy(&number, &address, sizeof(number));
    return number;
}
