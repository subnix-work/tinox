//! Issue #487: the main thread blocks in a native "UI loop" while Tinox
//! logic runs on a spawned task and a foreign (non-runtime) pthread
//! allocates Tinox heap memory concurrently. The collector must keep every
//! live object of all three threads and never deadlock or crash.
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const C_SRC: &str = r#"
#include <pthread.h>
#include <stdint.h>
#include <runtime.h>

#define QCAP 8192
static int64_t q[QCAP];
static int qh, qt;
static int quit;
static pthread_mutex_t m = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t cv = PTHREAD_COND_INITIALIZER;

static void push(int64_t v) {
    pthread_mutex_lock(&m);
    q[qt++ % QCAP] = v;
    pthread_cond_broadcast(&cv);
    pthread_mutex_unlock(&m);
}

typedef struct node { struct node* next; int64_t v; } node;

static void* producer(void* arg) {
    int rounds = (int)(intptr_t)arg;
    if (tinox_thread_attach() < 0) { push(-3); return NULL; }
    for (int r = 0; r < rounds; r++) {
        node* head = NULL;  /* only referenced from this foreign stack */
        for (int i = 0; i < 2000; i++) {
            node* n = tinox_alloc(sizeof(node));
            n->next = head; n->v = i; head = n;
        }
        for (int i = 0; i < 2000; i++) tinox_alloc(512);  /* garbage: forces collections */
        int64_t sum = 0, cnt = 0;
        for (node* p = head; p; p = p->next) { sum += p->v; cnt++; }
        push(cnt == 2000 && sum == 1999 * 2000 / 2 ? r : -4);
    }
    tinox_thread_detach();
    push(-2);
    return NULL;
}

void ffi_start_producer(int64_t rounds) {
    pthread_t t;
    pthread_create(&t, NULL, producer, (void*)(intptr_t)rounds);
    pthread_detach(t);
}

int64_t ffi_next_event(int64_t timeout_ms) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    ts.tv_sec += timeout_ms / 1000;
    ts.tv_nsec += (timeout_ms % 1000) * 1000000L;
    if (ts.tv_nsec >= 1000000000L) { ts.tv_sec++; ts.tv_nsec -= 1000000000L; }
    pthread_mutex_lock(&m);
    while (qh == qt) {
        if (pthread_cond_timedwait(&cv, &m, &ts) != 0) { pthread_mutex_unlock(&m); return -1; }
    }
    int64_t v = q[qh++ % QCAP];
    pthread_mutex_unlock(&m);
    return v;
}

/* Stand-in for a GUI toolkit's exec(): blocks the calling thread. */
void ffi_run_loop(void) {
    pthread_mutex_lock(&m);
    while (!quit) pthread_cond_wait(&cv, &m);
    pthread_mutex_unlock(&m);
}

void ffi_quit(void) {
    pthread_mutex_lock(&m);
    quit = 1;
    pthread_cond_broadcast(&cv);
    pthread_mutex_unlock(&m);
}

int64_t ffi_probe_attach(void) {
    int rc = tinox_thread_attach();
    tinox_thread_detach();
    return rc;
}

static void* probe_thread(void* out) {
    int a = tinox_thread_attach();     /* 0: newly attached */
    int b = tinox_thread_attach();     /* 0: nested */
    tinox_thread_detach();
    tinox_thread_detach();
    int c = tinox_thread_attach();     /* 0: attach after a full detach works again */
    tinox_thread_detach();
    *(int64_t*)out = a * 100 + b * 10 + c;
    return NULL;
}

int64_t ffi_probe_foreign(void) {
    pthread_t t;
    int64_t out = -1;
    pthread_create(&t, NULL, probe_thread, &out);
    pthread_join(t, NULL);
    return out;
}
"#;

const MAIN: &str = r#"
extern fn ffi_start_producer(rounds: Int64)
extern fn ffi_next_event(timeoutMs: Int64) -> Int64
extern fn ffi_run_loop()
extern fn ffi_quit()
extern fn ffi_probe_attach() -> Int64
extern fn ffi_probe_foreign() -> Int64

class Main
{
    fnc logic() -> Int64 {
        var ok: Int64 = 0
        var bad: Int64 = 0
        var done: Bool = false
        while (!done) {
            // Churn the Tinox heap from this thread as well.
            var churn: List<String> = []
            var i: Int64 = 0
            while (i < 500) {
                churn.push("x" + i.toString())
                i = i + 1
            }
            const ev: Int64 = ffi_next_event(50)
            if (ev == -2) {
                done = true
            } else if (ev >= 0) {
                ok = ok + 1
            } else if (ev != -1) {
                bad = bad + 1
            }
        }
        println("producer-rounds-ok=" + ok.toString() + " bad=" + bad.toString())
        ffi_quit()
        return ok
    }

    fnc main() -> Int32 {
        println("probe-main=" + ffi_probe_attach().toString())
        println("probe-foreign=" + ffi_probe_foreign().toString())
        ffi_start_producer(300)
        const t = spawn Main::logic()
        ffi_run_loop()
        const ok: Int64 = await t
        println("loop-returned ok=" + ok.toString())
        return 0
    }
}
"#;

#[test]
fn blocked_main_thread_foreign_thread_and_task_share_the_collector() {
    let d: PathBuf = std::env::temp_dir().join(format!("tinox_foreign_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("tinox.toml"),
        format!(
            "[package]\nname = \"foreign_demo\"\nversion = \"0.1.0\"\ncompiler = \"{}\"\n\n[native]\nsources = [\"ffi.c\"]\nlibs = [\"pthread\"]\n",
            env!("CARGO_PKG_VERSION")
        ),
    )
    .unwrap();
    std::fs::write(d.join("ffi.c"), C_SRC).unwrap();
    std::fs::write(d.join("Main.tnx"), MAIN).unwrap();

    let out = d.join("app");
    let r = Command::new(env!("CARGO_BIN_EXE_tinox"))
        .current_dir(&d)
        .args(["build", "Main.tnx", "-o", out.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));

    // Several runs: collector races are timing dependent.
    for run in 0..5 {
        let mut child = Command::new(&out).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(120);
        let status = loop {
            if let Some(s) = child.try_wait().unwrap() {
                break s;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                panic!("run {run}: timed out (deadlock between the collector and a blocked thread?)");
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let o = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&o.stdout);
        assert!(status.success(), "run {run}: {status}\n{stdout}\n{}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(
            stdout.lines().collect::<Vec<_>>(),
            [
                "probe-main=1",
                "probe-foreign=0",
                "producer-rounds-ok=300 bad=0",
                "loop-returned ok=300",
            ],
            "run {run}"
        );
    }
}
