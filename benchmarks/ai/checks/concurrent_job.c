#include <stdatomic.h>
#include <stdint.h>
#include <time.h>
static atomic_int active, peak, calls;
int64_t evalWork(int64_t value) {
    int count = atomic_fetch_add(&active, 1) + 1;
    int previous = atomic_load(&peak);
    while (count > previous && !atomic_compare_exchange_weak(&peak, &previous, count)) {}
    atomic_fetch_add(&calls, 1);
    struct timespec pause = { .tv_nsec = 20000000 };
    nanosleep(&pause, 0);
    atomic_fetch_sub(&active, 1);
    return value * value;
}
int64_t evalActive(void) { return atomic_load(&active); }
int64_t evalPeak(void) { return atomic_load(&peak); }
int64_t evalCalls(void) { return atomic_load(&calls); }
void evalReset(void) { atomic_store(&peak, 0); atomic_store(&calls, 0); }
