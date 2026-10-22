//! Cooperative task deadlines against an independent TCP client (#301).
#[allow(dead_code)] // Reuse only the shared bounded process wait.
mod common;

use std::io::{BufRead, BufReader, Read};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::time::Duration;

const SOURCE: &str = r#"
class Main {
    fnc waitForSocket(fd: Int64, ready: Int64) -> Nothing {
        defer { httpServerClose(fd); println("closed"); }
        send ready -> 1;
        taskWaitReadable(fd);
        println("readable");
    }
    fnc main() -> Int32 {
        const fd = httpServerCreate(0);
        println(httpServerBoundPort(fd));
        const ready = channel;
        try {
            scope {
                spawn Main::waitForSocket(fd, ready);
                recv ready;
                taskSetDeadline(DEADLINE);
            }
        } catch (e: String) { println(e); }
        return 0;
    }
}
"#;

#[test]
fn socket_readiness_and_deadline_release_the_real_descriptor() {
    let dir = std::env::temp_dir().join(format!("tinox-task-cancel-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for connect in [true, false] {
        let source = SOURCE.replace("DEADLINE", if connect { "5000" } else { "30" });
        std::fs::write(dir.join("Main.tnx"), source).unwrap();
        let exe = dir.join("task_io");
        let build = Command::new(env!("CARGO_BIN_EXE_tinox"))
            .current_dir(&dir)
            .args(["build", "Main.tnx", "-o"])
            .arg(&exe)
            .output()
            .unwrap();
        assert!(
            build.status.success(),
            "{}",
            String::from_utf8_lossy(&build.stderr)
        );
        for _ in 0..20 {
            let mut child = Command::new(&exe)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let mut stdout = BufReader::new(child.stdout.take().unwrap());
            let mut line = String::new();
            stdout.read_line(&mut line).unwrap();
            let port: u16 = line.trim().parse().expect("dynamic server port");
            let client = connect.then(|| TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap());
            let status = common::wait_timeout(&mut child, Duration::from_secs(10))
                .expect("task must terminate");
            let mut output = String::new();
            stdout.read_to_string(&mut output).unwrap();
            let mut error = String::new();
            child
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut error)
                .unwrap();
            assert!(status.success(), "stdout: {output}; stderr: {error}");
            assert!(error.is_empty(), "stderr: {error}");
            assert_eq!(
                output,
                if connect {
                    "readable\nclosed\n"
                } else {
                    "closed\nTask deadline exceeded\n"
                }
            );
            drop(client);
            // A successful independent bind proves defer closed the descriptor.
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
                .expect("socket leaked after task cleanup");
            drop(listener);
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}
