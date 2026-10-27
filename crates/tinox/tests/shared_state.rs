//! Singleton publication checked by independent, barrier-synchronized native clients.
#[allow(dead_code)]
mod common;
use std::{fs, path::PathBuf, process::Command, time::Duration};

fn success(command: &mut Command) {
    let output = command.output().unwrap();
    assert!(output.status.success(), "{command:?}\n{output:?}");
}

#[test]
fn concurrent_first_access_publishes_one_initialized_dependency_graph() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = std::env::temp_dir().join(format!("tinox-singleton-publication-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("Main.tnx"), "import Service\nimport Eager\nclass Main { fnc main() -> Int32 { return 0 } }").unwrap();
    fs::write(directory.join("Leaf.tnx"), "@ApplicationComponent class Leaf { var ready: Int64 = 17 }").unwrap();
    fs::write(directory.join("Eager.tnx"), "import Leaf\n@Startup class Eager { @Inject var leaf: Leaf\nvar ready: Int64 = 29 }").unwrap();
    let fields: String = (0..2000).map(|i| format!("var field{i}: Int64 = 17\n")).collect();
    fs::write(directory.join("Service.tnx"), format!("import Leaf\n@ApplicationComponent class Service {{ {fields} @Inject var leaf: Leaf }}")).unwrap();
    success(Command::new(env!("CARGO_BIN_EXE_tinox")).current_dir(&directory).args(["build", "Main.tnx", "-o", "program"]));
    let ir = fs::read_to_string(directory.join("program.ll")).unwrap();
    assert!(ir.contains("define i32 @tinox_main("));
    // Keep the real generated providers and startup constructors, but substitute
    // an independent native entry point to coordinate first access precisely.
    fs::write(directory.join("program.ll"), ir.replace("define i32 @tinox_main(", "define i32 @original_tinox_main(")).unwrap();
    fs::write(directory.join("clients.c"), r#"
#define GC_THREADS
#define GC_PTHREADS
#include <gc.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
extern int64_t* Service_di_get(void);
extern int64_t* Leaf_di_get(void);
extern int64_t* Eager_di_get(void);
static pthread_barrier_t barrier;
static int64_t* instances[64];
static int failed[64];
static void* client(void* arg) {
    intptr_t index = (intptr_t)arg;
    pthread_barrier_wait(&barrier);
    int64_t* service = Service_di_get();
    instances[index] = service;
    for (int i = 0; i < 2000; i++) if (service[i] != 17) failed[index] = 1;
    int64_t* leaf = Leaf_di_get();
    int64_t* eager = Eager_di_get();
    if (leaf[0] != 17 || service[2000] != (int64_t)(intptr_t)leaf
        || eager[0] != (int64_t)(intptr_t)leaf || eager[1] != 29) failed[index] = 1;
    return NULL;
}
int64_t tinox_main(void) {
    pthread_t threads[64];
    if (pthread_barrier_init(&barrier, NULL, 64)) return 2;
    for (intptr_t i = 0; i < 64; i++) if (pthread_create(&threads[i], NULL, client, (void*)i)) return 3;
    for (int i = 0; i < 64; i++) if (pthread_join(threads[i], NULL)) return 4;
    for (int i = 0; i < 64; i++) if (instances[i] != instances[0] || failed[i]) return 5;
    pthread_barrier_destroy(&barrier);
    puts("one initialized singleton graph");
    return 0;
}
"#).unwrap();
    success(Command::new("llc").current_dir(&directory).args(["-O1", "-filetype=obj", "program.ll", "-o", "program.o"]));
    success(Command::new("cc").arg("-c").arg(root.join("runtime/runtime.c")).arg("-o").arg(directory.join("runtime.o")));
    success(Command::new("cc").current_dir(&directory).args(["clients.c", "program.o", "runtime.o", "-o", "clients", "-lm", "-lpthread", "-lgc", "-lz", "-no-pie"]));
    for _ in 0..20 {
        let mut child = Command::new(directory.join("clients")).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
        common::wait_timeout(&mut child, Duration::from_secs(15)).expect("singleton initialization deadlocked");
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"one initialized singleton graph\n");
        assert!(output.stderr.is_empty(), "{output:?}");
    }
    fs::remove_dir_all(directory).unwrap();
}

fn copy_tree(source: &std::path::Path, target: &std::path::Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if entry.path().is_dir() { copy_tree(&entry.path(), &target.join(entry.file_name())); }
        else { fs::copy(entry.path(), target.join(entry.file_name())).unwrap(); }
    }
}

struct Server(std::process::Child);
impl Drop for Server {
    fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
}

#[test]
fn concurrent_http_clients_share_a_guarded_counter_and_collection() {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    let directory = std::env::temp_dir().join(format!("tinox-shared-http-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let modules = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tinox-core-ext");
    let mut manifest = "[package]\ncompiler = \"2.8.0\"\nname=\"shared_http\"\nversion=\"0.0.0\"\n[startup]\nbanner=false\n".to_string();
    for module in ["http_server", "json"] {
        manifest.push_str(&format!("\n[[dependencies]]\ngroup=\"tinox.core\"\nartifactId=\"{module}\"\nversion=\"9.9.9\"\n"));
        copy_tree(&modules.join(module).join("tinox"), &directory.join(format!(".tinox/repository/central/tinox.core/{module}/9.9.9/tinox")));
    }
    fs::write(directory.join("tinox.toml"), manifest).unwrap();
    fs::write(directory.join("Main.tnx"), "import Api\nclass Main { fnc main() -> Int32 { return 0 } }").unwrap();
    fs::write(directory.join("Counter.tnx"), r#"
@ApplicationComponent @Synchronized class Counter {
    private var count: Int64 = 0
    private var entries: List<Int64> = []
    fn add() -> Int64 { this.count = this.count + 1
    this.entries.push(this.count)
    return this.read() }
    fn read() -> Int64 { return this.count }
    fn size() -> Int64 { return this.entries.len() }
}
"#).unwrap();
    fs::write(directory.join("Api.tnx"), r#"
import tinox.core.http_server
import Counter
@HttpRequestScoped @Path("/counter") class Api {
    @Inject var counter: Counter
    @GET @Path("/next") fn next() -> Int64 { return this.counter.add() }
    @GET @Path("/count") fn count() -> Int64 { return this.counter.read() }
    @GET @Path("/size") fn size() -> Int64 { return this.counter.size() }
}
"#).unwrap();
    // Reserve an OS-assigned port throughout compilation, then release it for
    // the annotation-generated listener. There is no fixed-port dependency.
    let reservation = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = reservation.local_addr().unwrap();
    success(Command::new(env!("CARGO_BIN_EXE_tinox")).current_dir(&directory)
        .env("TINOX_HOME", &directory).env("TINOX_PORT", address.port().to_string())
        .args(["build", "Main.tnx", "-o", "server"]));
    drop(reservation);
    let server = Command::new(directory.join("server")).current_dir(&directory)
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();
    let mut server = Server(server);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok() { break; }
        assert!(server.0.try_wait().unwrap().is_none(), "server exited before becoming ready");
        assert!(std::time::Instant::now() < deadline, "server failed to become ready");
        std::thread::sleep(Duration::from_millis(10));
    }
    let request = |path: &str| -> i64 {
        let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(3)).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        stream.set_write_timeout(Some(Duration::from_secs(3))).unwrap();
        write!(stream, "GET /counter/{path} HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n").unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        response.split_once("\r\n\r\n").unwrap().1.trim().parse().unwrap()
    };
    for batch in 0..20 {
        let barrier = std::sync::Barrier::new(16);
        let mut values = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..16).map(|_| {
                let barrier = &barrier;
                let request = &request;
                scope.spawn(move || { barrier.wait(); request("next") })
            }).collect();
            handles.into_iter().map(|handle| handle.join().unwrap()).collect::<Vec<_>>()
        });
        values.sort_unstable();
        assert_eq!(values, (batch * 16 + 1..=batch * 16 + 16).collect::<Vec<_>>());
        assert_eq!(request("count"), (batch + 1) * 16);
        assert_eq!(request("size"), (batch + 1) * 16);
    }
    drop(server);
    fs::remove_dir_all(directory).unwrap();
}
