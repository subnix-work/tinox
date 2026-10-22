// Exercise the actual runtime with independent Python HTTP peers. No GC/TLS
// dependencies are needed; make check separately covers the normal GC build.
#define TINOX_NO_GC
#define main tinox_runtime_main
#include "../../runtime/runtime.c"
#undef main
#include <assert.h>
__thread int64_t __tinox_err;
int64_t tinox_main(void) { return 0; }

static const char* downstream;
static void* child(void* ignored) {
    (void)ignored;
    puts(telemetryLogJson("0", "INFO", "test", "child"));
    assert(httpStatusCode(httpGet(downstream)) == 200);
    return NULL;
}
static void probe(int64_t ctx) {
    (void)ctx;
    char before[17]; strcpy(before, telemetry_context.span);
    puts(telemetryLogJson("0", "INFO", "test", "handler"));
    tinox_task_await(tinox_task_spawn(child, NULL));
    assert(!__tinox_err);
    assert(!strcmp(before, telemetry_context.span));
    g_response[2] = (int64_t)(uintptr_t)"ok";
}
static void contexts(void) {
    const char* valid = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
    int64_t a = telemetryBegin("message", valid, "request-1");
    assert(a == 1 && telemetry_context.sampled);
    assert(!strcmp(telemetry_context.trace, "4bf92f3577b34da6a3ce929d0e0e4736"));
    assert(!strcmp(telemetry_context.parent, "00f067aa0ba902b7"));
    char parent[17]; strcpy(parent, telemetry_context.span);
    int64_t b = telemetryBegin("nested", "", "");
    assert(!strcmp(telemetry_context.parent, parent));
    assert(!strcmp(telemetry_context.request, "request-1"));
    assert(!telemetryEnd(a)); // reject out-of-order pop
    assert(telemetryEnd(b));
    assert(!strcmp(telemetry_context.span, parent));
    puts(telemetryLogJson("0", "INFO", "test", "quote\" slash\\ line\n tab\t control\001"));
    for (int i = 1; i < 32; i++) assert(telemetryBegin("nested", "", "") == i + 1);
    assert(!telemetryBegin("overflow", "", ""));
    for (int i = 32; i; i--) assert(telemetryEnd(i));
    assert(!*telemetryTraceparent() && !*telemetryRequestId());
    const char* bad[] = {
        "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01",
        "ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        "00-4BF92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-extra",
        "00-short", "", "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-0g"
    };
    for (size_t i = 0; i < sizeof(bad) / sizeof(*bad); i++) {
        a = telemetry_enter("incoming", bad[i], "bad\r\nrequest", 2);
        assert(strcmp(telemetry_context.trace, "4bf92f3577b34da6a3ce929d0e0e4736"));
        assert(!*telemetry_context.parent && strlen(telemetry_context.request) == 32);
        assert(telemetryEnd(a));
    }
    a = telemetryBegin("unsampled", "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00", "");
    assert(!telemetry_context.sampled);
    b = telemetryBegin("child", "", ""); assert(!telemetry_context.sampled);
    telemetryEnd(b); telemetryEnd(a);
    char long_message[5000]; memset(long_message, 'x', sizeof(long_message)); long_message[4999] = 0;
    puts(telemetryLogJson("0", "INFO", "test", long_message));
    a = telemetryBegin("boundary", "", "request-2");
    assert(telemetryBegin("abandoned", "", ""));
    telemetry_leave(a);
    assert(!*telemetryTraceparent());
    puts("contexts ok");
}
int main(int argc, char** argv) {
    assert(argc >= 2);
    setvbuf(stdout, NULL, _IOLBF, 0);
    telemetryInit();
    if (!strcmp(argv[1], "contexts")) contexts();
    else if (!strcmp(argv[1], "server")) {
        assert(argc == 4);
        downstream = argv[2];
        TinoxHttpServer srv = {0};
        http_server_add_route((int64_t*)&srv, "GET", "/probe", (int64_t)(uintptr_t)probe);
        int64_t listener = httpServerCreate(0);
        assert(listener >= 0);
        printf("PORT %lld\n", (long long)httpServerBoundPort(listener));
        thread_local_init();
        for (int i = 0; i < atoi(argv[3]); i++) {
            int64_t fd = httpServerAcceptConn(listener); assert(fd >= 0);
            int keep_alive;
            tinox_handle_one(&srv, fd, &keep_alive);
            assert(!*telemetryTraceparent());
            close((int)fd);
        }
        close((int)listener);
        assert(telemetryFlush());
    } else if (!strcmp(argv[1], "flood")) {
        int64_t start = telemetry_clock(CLOCK_MONOTONIC);
        for (int i = 0; i < 1000; i++) {
            int64_t a = telemetryBegin("flood", "", ""); telemetryEnd(a);
        }
        assert(telemetry_clock(CLOCK_MONOTONIC) - start < 500000000LL);
        start = telemetry_clock(CLOCK_MONOTONIC);
        telemetryFlush();
        assert(telemetry_clock(CLOCK_MONOTONIC) - start < 500000000LL);
        puts(tinox_metrics_prometheus());
    } else if (!strcmp(argv[1], "bench")) {
        volatile int64_t sink = 0;
        const int count = 100000;
        int64_t start = telemetry_clock(CLOCK_MONOTONIC);
        for (int i = 0; i < count; i++) sink += i;
        int64_t baseline = telemetry_clock(CLOCK_MONOTONIC) - start;
        start = telemetry_clock(CLOCK_MONOTONIC);
        for (int i = 0; i < count; i++) {
            int64_t token = telemetryBegin("bench", "", "");
            sink += i; telemetryEnd(token);
        }
        printf("enabled=%d baseline_ns_per_op=%.2f instrumented_ns_per_op=%.2f sink=%lld\n",
               telemetry_enabled, (double)baseline / count,
               (double)(telemetry_clock(CLOCK_MONOTONIC) - start) / count, (long long)sink);
    }
    return 0;
}
