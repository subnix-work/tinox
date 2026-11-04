// Observe the completion notification itself, before the awaiter can run.
// This makes the counter/notification ordering deterministic without sleeps.
#define pthread_cond_broadcast completion_broadcast
#include "runtime.c"
#undef pthread_cond_broadcast
extern int pthread_cond_broadcast(pthread_cond_t* condition);

__thread int64_t __tinox_err;
static _Atomic(pthread_cond_t*) watched_condition;
static int64_t executor;
static atomic_bool release_job;
static int64_t completed_at_notification, active_at_notification;

int completion_broadcast(pthread_cond_t* condition) {
    if (condition == atomic_load(&watched_condition)) {
        completed_at_notification = executorStat(executor, 3);
        active_at_notification = executorStat(executor, 2);
    }
    return pthread_cond_broadcast(condition);
}

static void* job(void* argument) {
    while (!atomic_load(&release_job)) sched_yield();
    if ((intptr_t)argument % 2) __tinox_err = (int64_t)(intptr_t)"job error";
    return argument;
}

int64_t tinox_main(void) {
    executor = executorCreate(1, 4);
    for (intptr_t i = 0; i < 32; i++) {
        atomic_store(&release_job, false);
        void* handle = tinox_task_spawn_on(executor, job, (void*)i);
        atomic_store(&watched_condition, &task_lookup((uintptr_t)handle)->cond);
        completed_at_notification = active_at_notification = -1;
        atomic_store(&release_job, true);
        int64_t result = tinox_task_await(handle);
        if ((i % 2 == 0 && result != i) || !!__tinox_err != !!(i % 2)) return 2;
        __tinox_err = 0;
        if (completed_at_notification != i + 1 || active_at_notification != 0) {
            fprintf(stderr, "notification %ld: completed=%ld active=%ld\n",
                (long)i + 1, (long)completed_at_notification, (long)active_at_notification);
            return 3;
        }
        if (executorStat(executor, 3) != i + 1 || executorStat(executor, 2) != 0) return 4;
    }
    executorShutdown(executor, true);
    puts("completion notifications follow executor accounting");
    return 0;
}
