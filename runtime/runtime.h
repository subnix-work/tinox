// Tinox Runtime Header
#ifndef TINOX_RUNTIME_H
#define TINOX_RUNTIME_H

#include <stdint.h>
#include <stdbool.h>
#include <stddef.h>

// Memory management
void* tinox_alloc(size_t size);
void tinox_free(void* ptr);

// Foreign threads (issue #487). A thread not created by the Tinox runtime
// must call tinox_thread_attach() before it allocates Tinox heap memory or
// keeps a pointer into it, and tinox_thread_detach() before it exits.
// attach returns 0 (attached by this call), 1 (already known to the
// collector: nothing to undo) or a negative value (no thread support).
// Calls nest. Never store a pointer to Tinox heap memory only in memory the
// collector does not scan (malloc, C++ objects): copy the data or use handles.
int  tinox_thread_attach(void);
void tinox_thread_detach(void);

// Print functions
void tinox_print_int(int64_t val);
void tinox_print_float(double val);
void tinox_print_string(const char* val);
void tinox_print_bool(bool val);
void tinox_print_newline(void);

// Panic handling
void tinox_panic(const char* msg);

// Array operations
void* tinox_array_alloc(size_t element_size, size_t length);

// String operations
int64_t tinox_string_length(const char* str);
char* tinox_string_concat(const char* a, const char* b);

// Map operations (opaque i8* handle)
void* tinox_map_create(void);
void  tinox_map_set(void* map, const char* key, int64_t value);
int64_t tinox_map_get(void* map, const char* key);
int64_t tinox_map_contains(void* map, const char* key);
void  tinox_map_remove(void* map, const char* key);
int64_t tinox_map_len(void* map);
void  tinox_map_free(void* map);

// Config functions (@Config annotation)
char*   tinox_config_get(const char* key);
int64_t tinox_config_get_int(const char* key);
int64_t tinox_config_get_bool(const char* key);

// Observability (configuration initialized once at process startup).
void telemetryInit(void);
int64_t telemetryEnabled(void);
int64_t telemetryBegin(const char* name, const char* parent, const char* request_id);
int64_t telemetryEnd(int64_t token);
char* telemetryTraceparent(void);
char* telemetryRequestId(void);
int64_t telemetryFlush(void);
char* telemetryLogJson(const char* timestamp, const char* level, const char* logger, const char* message);
void telemetryWriteLogJson(const char* timestamp, const char* level, const char* logger, const char* message);

#endif // TINOX_RUNTIME_H
