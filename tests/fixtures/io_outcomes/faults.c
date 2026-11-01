// Deterministic libc fault injection, loaded only into the checked-I/O probe.
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static int named(FILE* file, const char* suffix) {
    char link[64], path[4096];
    snprintf(link, sizeof(link), "/proc/self/fd/%d", fileno(file));
    ssize_t size = readlink(link, path, sizeof(path) - 1);
    if (size < 0) return 0;
    path[size] = 0;
    size_t n = strlen(suffix);
    return (size_t)size >= n && !strcmp(path + size - n, suffix);
}
size_t fread(void* data, size_t size, size_t count, FILE* file) {
    size_t (*real_read)(void*, size_t, size_t, FILE*) = dlsym(RTLD_NEXT, "fread");
    if (named(file, "/partial")) {
        size_t got = real_read(data, size, count > 3 ? 3 : count, file);
        errno = EIO;
        return got;
    }
    return real_read(data, size, count, file);
}
int ferror(FILE* file) {
    int (*real_error)(FILE*) = dlsym(RTLD_NEXT, "ferror");
    if (named(file, "/partial")) { errno = EIO; return 1; }
    return real_error(file);
}
int fclose(FILE* file) {
    int (*real_close)(FILE*) = dlsym(RTLD_NEXT, "fclose");
    int fail = named(file, "/close-failure") || named(file, "/partial");
    int status = real_close(file);
    if (fail) { errno = ENOSPC; return EOF; }
    return status;
}
