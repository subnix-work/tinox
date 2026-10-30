//! Request identity and cleanup checked through independent native HTTP clients.
#[allow(dead_code)]
mod common;
use std::{fs, path::PathBuf, process::Command, time::Duration};
fn success(command: &mut Command) {
    let output = command.output().unwrap();
    assert!(output.status.success(), "{command:?}\n{output:?}");
}
fn copy_tree(source: &std::path::Path, target: &std::path::Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target.join(entry.file_name()));
        } else {
            fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
        }
    }
}

struct Server(std::process::Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn requests_share_one_graph_and_release_it_after_success_error_and_binding_failure() {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    let directory = std::env::temp_dir().join(format!("tinox-request-http-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let modules = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tinox-core-ext");
    let mut manifest = "[package]\ncompiler=\"2.8.0\"\nname=\"request_context\"\nversion=\"0.0.0\"\n[startup]\nbanner=false\n".to_string();
    for module in ["http_server", "json"] {
        manifest.push_str(&format!("\n[[dependencies]]\ngroup=\"tinox.core\"\nartifactId=\"{module}\"\nversion=\"9.9.9\"\n"));
        copy_tree(
            &modules.join(module).join("tinox"),
            &directory.join(format!(
                ".tinox/repository/central/tinox.core/{module}/9.9.9/tinox"
            )),
        );
    }
    fs::write(directory.join("tinox.toml"), manifest).unwrap();
    fs::write(
        directory.join("Main.tnx"),
        "import Api\n@Service class Main { fnc main() -> Int32 { return 0 } }",
    )
    .unwrap();
    fs::write(
        directory.join("State.tnx"),
        "@HttpRequestScoped class State { var value: Int64 = 0 }",
    )
    .unwrap();
    fs::write(directory.join("Layer.tnx"), "import State\n@HttpRequestScoped class Layer { @Inject var state: State\nfn bump() -> Nothing { this.state.value = this.state.value + 1 } }").unwrap();
    fs::write(
        directory.join("Api.tnx"),
        r#"
import tinox.core.http_server
import State
import Layer
@HttpRequestScoped @Path("/scope") class Api {
    @Inject var direct: State
    @Inject var second: State
    @Inject var layer: Layer
    @GET @Path("/identity") fn identity() -> Int64 {
        this.direct.value = this.direct.value + 1
        this.layer.bump()
        this.second.value = this.second.value + 1
        return this.direct.value
    }
    @GET @Path("/failure") fn failure() -> Int64 { throw "request failed"
        return 0 }
    @GET @Path("/bound") fn bound(@QueryParam n: Int64) -> Int64 { return n }
    @GET @Path("/cancel") fn cancel() -> Int64 {
        while (true) { taskCheckCancellation() }
        return 0
    }
}
"#,
    )
    .unwrap();
    let reservation = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = reservation.local_addr().unwrap();
    success(
        Command::new(env!("CARGO_BIN_EXE_tinox"))
            .current_dir(&directory)
            .env("TINOX_HOME", &directory)
            .env("TINOX_PORT", address.port().to_string())
            .args(["build", "Main.tnx", "-o", "server"]),
    );
    drop(reservation);
    let server = Command::new(directory.join("server"))
        .current_dir(&directory)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut server = Server(server);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok() {
            break;
        }
        assert!(
            server.0.try_wait().unwrap().is_none(),
            "server exited before becoming ready"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "server failed to become ready"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let request = |path: &str| -> (u16, String) {
        let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(3)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        write!(
            stream,
            "GET /scope/{path} HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let status = response.split_whitespace().nth(1).unwrap().parse().unwrap();
        (
            status,
            response
                .split_once("\r\n\r\n")
                .unwrap()
                .1
                .trim()
                .to_string(),
        )
    };
    for _ in 0..20 {
        let barrier = std::sync::Barrier::new(16);
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..16)
                .map(|_| {
                    let barrier = &barrier;
                    let request = &request;
                    scope.spawn(move || {
                        barrier.wait();
                        assert_eq!(request("identity"), (200, "3".into()));
                    })
                })
                .collect();
            for handle in handles {
                handle.join().unwrap();
            }
        });
        assert_eq!(request("failure").0, 500);
        assert_eq!(request("identity"), (200, "3".into()));
        assert_eq!(request("bound?n=invalid").0, 400);
        assert_eq!(request("identity"), (200, "3".into()));
    }
    drop(server);
    // Exercise the generated unmanaged wrapper directly too. Its existing
    // exception status mapping is tracked separately in #573; ownership must
    // still close even while a pending error leaves the body.
    fs::write(
        directory.join("Main.tnx"),
        "import Api\nclass Main { fnc main() -> Int32 { return 0 } }",
    )
    .unwrap();
    success(
        Command::new(env!("CARGO_BIN_EXE_tinox"))
            .current_dir(&directory)
            .env("TINOX_HOME", &directory)
            .args(["build", "Main.tnx", "-o", "unmanaged"]),
    );
    let ir = fs::read_to_string(directory.join("unmanaged.ll")).unwrap();
    fs::write(
        directory.join("unmanaged.ll"),
        ir.replace(
            "define i32 @tinox_main(",
            "define i32 @original_tinox_main(",
        ),
    )
    .unwrap();
    fs::write(directory.join("cleanup.c"), r#"
#include <stdint.h>
#include <string.h>
#include <stdatomic.h>
#include <sched.h>
extern __thread int64_t __tinox_err;
extern void* tinox_map_create(void);
extern void __route_Api_identity(int64_t);
extern void __route_Api_failure(int64_t);
extern void __route_Api_bound(int64_t);
extern void __route_Api_cancel(int64_t);
extern void* tinox_task_spawn(void* (*fn)(void*), void*);
extern int64_t tinox_task_await(void*);
extern void taskCancel(int64_t);
extern void tinox_request_begin(void);
extern void tinox_request_end(void);
extern int64_t* State_di_create(void);
static int check_closed(void) {
    // begin rejects a still-active context; a fresh provider must be zeroed.
    tinox_request_begin();
    int failed = State_di_create()[0] != 0;
    tinox_request_end();
    return failed;
}
static atomic_int cancellation_entered, cancellation_closed;
static void* cancellation_client(void* ignored) {
    (void)ignored;
    int64_t response[3] = {200, (int64_t)tinox_map_create(), (int64_t)""};
    int64_t request[7] = {0};
    int64_t context[2] = {(int64_t)request, (int64_t)response};
    atomic_store(&cancellation_entered, 1);
    __route_Api_cancel((int64_t)context);
    int cancelled = __tinox_err != 0;
    __tinox_err = 0;
    atomic_store(&cancellation_closed, cancelled && !check_closed());
    return NULL;
}
int64_t tinox_main(void) {
    int64_t request[7] = {(int64_t)"GET", (int64_t)"/scope/identity", (int64_t)"", (int64_t)tinox_map_create(), (int64_t)"n=invalid", (int64_t)tinox_map_create(), 0};
    int64_t response[3] = {200, (int64_t)tinox_map_create(), (int64_t)""};
    int64_t context[2] = {(int64_t)request, (int64_t)response};
    for (int i = 0; i < 20; i++) {
        __route_Api_identity((int64_t)context);
        if (strcmp((char*)response[2], "3") || check_closed()) return 1;
        __route_Api_failure((int64_t)context);
        __tinox_err = 0;
        if (check_closed()) return 2;
        __route_Api_bound((int64_t)context);
        if (response[0] != 400 || check_closed()) return 3;
        atomic_store(&cancellation_entered, 0);
        atomic_store(&cancellation_closed, 0);
        void* task = tinox_task_spawn(cancellation_client, NULL);
        while (!atomic_load(&cancellation_entered)) sched_yield();
        taskCancel((int64_t)(intptr_t)task);
        tinox_task_await(task);
        if (!atomic_load(&cancellation_closed)) return 4;
    }
    return 0;
}
"#).unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    success(Command::new("llc").current_dir(&directory).args([
        "-O1",
        "-filetype=obj",
        "unmanaged.ll",
        "-o",
        "unmanaged.o",
    ]));
    success(
        Command::new("cc")
            .arg("-c")
            .arg(root.join("runtime/runtime.c"))
            .arg("-o")
            .arg(directory.join("runtime.o")),
    );
    success(Command::new("cc").current_dir(&directory).args([
        "cleanup.c",
        "unmanaged.o",
        "runtime.o",
        "-o",
        "cleanup",
        "-lm",
        "-lpthread",
        "-lgc",
        "-lz",
        "-no-pie",
    ]));
    let mut cleanup = Command::new(directory.join("cleanup"))
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
        .spawn().unwrap();
    common::wait_timeout(&mut cleanup, Duration::from_secs(30)).expect("request cancellation did not unwind");
    let output = cleanup.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn captive_fields_and_direct_task_escapes_fail_before_codegen() {
    let directory =
        std::env::temp_dir().join(format!("tinox-request-negative-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("State.tnx"),
        "@HttpRequestScoped class State { var value: Int64 = 0 }",
    )
    .unwrap();
    fs::write(
        directory.join("Main.tnx"),
        "import Owner\nclass Main { fnc main() -> Int32 { return 0 } }",
    )
    .unwrap();
    for field in [
        "@Inject var state: State",
        "var state: State?",
        "var states: List<State>",
    ] {
        fs::write(
            directory.join("Owner.tnx"),
            format!("import State\n@ApplicationComponent class Owner {{ {field} }}"),
        )
        .unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_tinox"))
            .current_dir(&directory)
            .args(["check", "Main.tnx"])
            .output()
            .unwrap();
        assert!(!output.status.success(), "accepted captive field {field}");
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(diagnostic.contains("request-scoped state"), "{diagnostic}");
    }
    fs::write(directory.join("Owner.tnx"), "@HttpRequestScoped class Owner { async fn work() -> Int64 { return 1 }\nfn launch() -> Nothing { const task = spawn this.work() } }").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tinox"))
        .current_dir(&directory)
        .args(["check", "Main.tnx"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("spawn is not allowed in @HttpRequestScoped"),
        "{output:?}"
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn native_threads_isolate_graphs_and_context_end_drops_the_cache() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory =
        std::env::temp_dir().join(format!("tinox-request-native-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("Main.tnx"),
        "import Owner\nclass Main { fnc main() -> Int32 { return 0 } }",
    )
    .unwrap();
    fs::write(
        directory.join("State.tnx"),
        "@HttpRequestScoped class State { var value: Int64 = 0 }",
    )
    .unwrap();
    fs::write(
        directory.join("Layer.tnx"),
        "import State\n@HttpRequestScoped class Layer { @Inject var state: State }",
    )
    .unwrap();
    fs::write(directory.join("Owner.tnx"), "import State\nimport Layer\n@HttpRequestScoped class Owner { @Inject var direct: State\n@Inject var second: State\n@Inject var layer: Layer }").unwrap();
    success(
        Command::new(env!("CARGO_BIN_EXE_tinox"))
            .current_dir(&directory)
            .args(["build", "Main.tnx", "-o", "program"]),
    );
    let ir = fs::read_to_string(directory.join("program.ll")).unwrap();
    fs::write(
        directory.join("program.ll"),
        ir.replace(
            "define i32 @tinox_main(",
            "define i32 @original_tinox_main(",
        ),
    )
    .unwrap();
    fs::write(directory.join("clients.c"), r#"
#include <gc.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
extern int tinox_thread_attach(void);
extern void tinox_thread_detach(void);
extern void tinox_request_begin(void);
extern void tinox_request_end(void);
extern int64_t* Owner_di_create(void);
extern int64_t* State_di_create(void);
extern void* tinox_task_spawn(void* (*fn)(void*), void*);
static pthread_barrier_t barrier;
static int64_t* instances[32];
static int failed[32];
static void* client(void* raw) {
    intptr_t index = (intptr_t)raw;
    if (tinox_thread_attach() < 0) { failed[index] = 1; return NULL; }
    tinox_request_begin();
    int64_t* owner = Owner_di_create();
    int64_t* state = State_di_create();
    state[0] = index + 1;
    instances[index] = owner;
    pthread_barrier_wait(&barrier);
    // These calls take place while every other native thread owns its graph.
    GC_gcollect();
    int64_t* layer = (int64_t*)(intptr_t)owner[2];
    if (Owner_di_create() != owner || State_di_create() != state || state[0] != index + 1
        || owner[0] != (int64_t)(intptr_t)state || owner[1] != owner[0] || layer[0] != owner[0]) failed[index] = 1;
    tinox_request_end();
    tinox_request_begin();
    int64_t* fresh = State_di_create();
    if (fresh == state || fresh[0] != 0) failed[index] = 1;
    tinox_request_end();
    tinox_thread_detach();
    return NULL;
}
int64_t tinox_main(void) {
    // Subprocess probes ensure unsupported lifetime paths fail visibly.
    if (getenv("PROBE_OUTSIDE")) { State_di_create(); return 9; }
    if (getenv("PROBE_TASK")) {
        tinox_request_begin(); State_di_create();
        tinox_task_spawn(NULL, NULL); return 9;
    }
    pthread_t threads[32];
    if (pthread_barrier_init(&barrier, NULL, 32)) return 2;
    for (intptr_t i = 0; i < 32; i++) if (pthread_create(&threads[i], NULL, client, (void*)i)) return 3;
    for (int i = 0; i < 32; i++) if (pthread_join(threads[i], NULL)) return 4;
    for (int i = 0; i < 32; i++) {
        if (failed[i]) return 5;
        for (int j = i + 1; j < 32; j++) if (instances[i] == instances[j]) return 6;
    }
    pthread_barrier_destroy(&barrier);
    puts("isolated request contexts released");
    return 0;
}
"#).unwrap();
    success(Command::new("llc").current_dir(&directory).args([
        "-O1",
        "-filetype=obj",
        "program.ll",
        "-o",
        "program.o",
    ]));
    success(
        Command::new("cc")
            .arg("-c")
            .arg(root.join("runtime/runtime.c"))
            .arg("-o")
            .arg(directory.join("runtime.o")),
    );
    success(Command::new("cc").current_dir(&directory).args([
        "-include",
        "stdlib.h",
        "clients.c",
        "program.o",
        "runtime.o",
        "-o",
        "clients",
        "-lm",
        "-lpthread",
        "-lgc",
        "-lz",
        "-no-pie",
    ]));
    for _ in 0..20 {
        let mut child = Command::new(directory.join("clients"))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        common::wait_timeout(&mut child, Duration::from_secs(30))
            .expect("request contexts deadlocked");
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"isolated request contexts released\n");
        assert!(output.stderr.is_empty(), "{output:?}");
    }
    for (variable, diagnostic) in [
        ("PROBE_OUTSIDE", "outside annotated REST dispatch"),
        ("PROBE_TASK", "spawn cannot escape"),
    ] {
        let output = Command::new(directory.join("clients"))
            .env(variable, "1")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(diagnostic),
            "{output:?}"
        );
    }
    fs::remove_dir_all(directory).unwrap();
}
