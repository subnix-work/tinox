// Compile separately: this is a real native pthread without runtime redirects.
#include <errno.h>
#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "runtime.h"

extern void gcCollect(void);
extern int64_t gcStat(int64_t kind);
extern size_t tinox_gc_swept_bytes(void);
static _Thread_local char* slice;
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
    tinox_thread_detach();
    tinox_thread_detach();
    return NULL;
}

int64_t native_roots(void) {
    if (pipe(ready) || pipe(release_worker)) abort();
    pthread_t tid;
    if (pthread_create(&tid, NULL, worker, NULL)) abort();
    transfer(ready[0], 0);
    gcCollect();
    if (gcStat(0) < 1024 * 1024) abort(); // the worker still owns its block
    transfer(release_worker[1], 1);
    if (pthread_join(tid, NULL)) abort();
    size_t before = tinox_gc_swept_bytes();
    gcCollect(); // no thread-local or stack ranges from the exited worker
    close(ready[0]); close(ready[1]); close(release_worker[0]); close(release_worker[1]);
    return tinox_gc_swept_bytes() - before >= 256 * 1024;
}
