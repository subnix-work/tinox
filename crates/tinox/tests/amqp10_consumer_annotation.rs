//! @Amqp10Consumer/@OnMessage annotation (Issue #81).
//!
//! Compiles examples/amqp10_consumer_annotated/Main.tnx (a generated
//! auto-run connect/begin/attach/grantCredit/nextMessage/ack loop, spawned
//! by the mandatory Main.tnx bootstrap — never returns) and a small
//! standalone fake-broker helper program (its own separate process, own
//! class Main), runs both as background processes, and asserts on their
//! captured stdout. Not part of the golden-test harness (e2e.rs): those
//! cases must exit on their own within RUN_TIMEOUT, which an auto-run
//! AMQP-1.0 consumer never does.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// Reads the current published version straight out of the module's own
/// `crates/tinox-core-ext/<module>/tinox.toml` instead of a hardcoded
/// string -- a stale hardcoded version silently keeps testing whatever was
/// published under that old number forever, even after the module's real
/// content (and its own manifest) moves on. Exactly this bit
/// amqp10_heartbeat_reconnect (e2e.rs, via common/mod.rs's own copy of this
/// helper) on 2026-08-10: amqp10 got republished as 1.0.1 to fix a real
/// SASL handshake bug, but a hardcoded "1.0.0" here would have kept
/// exercising the broken pre-fix package.
fn extended_module_version(module: &str) -> String {
    let manifest = repo_root().join("crates/tinox-core-ext").join(module).join("tinox.toml");
    std::fs::read_to_string(&manifest)
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|l| l.trim().strip_prefix("version"))
                .and_then(|rest| rest.trim_start().strip_prefix('='))
                .map(|rest| rest.trim().trim_matches('"').to_string())
        })
        .unwrap_or_else(|| "1.0.0".to_string())
}

/// Kills the child on drop, so a failed assertion doesn't leak a
/// long-running process (or a listening socket) across test runs.
struct KillOnDrop(Child);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// Port must match the literal baked into @Amqp10Consumer(...) in
// examples/amqp10_consumer_annotated/DemoConsumer.tnx.
const FAKE_BROKER_SRC: &str = r#"
import tinox.core.amqp10;
import tinox.core.socket;

class Main
{
fnc main() -> Int32 {
    let srv = httpServerCreate(5873);
    if (srv < 0) {
        println("bind failed");
        return 1;
    }
    let sconn = httpServerAcceptConnHandle(srv);

    httpConnReadN(sconn, 8);
    httpConnWriteBytes(sconn, [65, 77, 81, 80, 3, 1, 0, 0]);
    let mechBody = Amqp10::encodePerformative(0x40, [Amqp10Value::ListVal([Amqp10Value::SymbolVal("PLAIN")])]);
    Amqp10::writeFrame(sconn, 1, 0, mechBody);
    Amqp10::readFrame(sconn);
    let outcomeBody = Amqp10::encodePerformative(0x44, [Amqp10Value::UByteVal(0)]);
    Amqp10::writeFrame(sconn, 1, 0, outcomeBody);

    httpConnReadN(sconn, 8);
    httpConnWriteBytes(sconn, [65, 77, 81, 80, 0, 1, 0, 0]);

    Amqp10::readFrame(sconn);
    var openFields: List<Amqp10Value> = [
        Amqp10Value::StrVal("fake-broker"),
        Amqp10Value::NullVal,
        Amqp10Value::UIntVal(4294967295),
        Amqp10Value::UShortVal(65535)
    ];
    Amqp10::writeFrame(sconn, 0, 0, Amqp10::encodePerformative(0x10, openFields));

    let fBegin = Amqp10::readFrame(sconn);
    var beginFields: List<Amqp10Value> = [
        Amqp10Value::UShortVal(fBegin.chanId), Amqp10Value::UIntVal(0),
        Amqp10Value::UIntVal(2147483647), Amqp10Value::UIntVal(2147483647)
    ];
    Amqp10::writeFrame(sconn, 0, fBegin.chanId, Amqp10::encodePerformative(0x11, beginFields));

    let fAttach = Amqp10::readFrame(sconn);
    let pAttach = Amqp10::decodePerformative(fAttach.body);
    var linkHandle: Int64 = 0;
    if (pAttach.fields.len() > 1) {
        match pAttach.fields[1] { UIntVal(n) => { linkHandle = n; } _ => {} }
    }
    Amqp10::writeFrame(sconn, 0, fBegin.chanId, Amqp10::encodePerformative(0x12, pAttach.fields));

    // flow from the client (credit grant for grantCredit(1))
    Amqp10::readFrame(sconn);

    let replyMsg = Amqp10::encodeMessageBody([65, 66, 67], "text/plain");
    var transferFields: List<Amqp10Value> = [
        Amqp10Value::UIntVal(linkHandle), Amqp10Value::UIntVal(0),
        Amqp10Value::BinaryVal([1]), Amqp10Value::UIntVal(0),
        Amqp10Value::BoolVal(true), Amqp10Value::BoolVal(false)
    ];
    let transferBody = Amqp10::encodePerformative(0x14, transferFields);
    var frame: List<Int64> = [];
    for i in 0..transferBody.len() { frame.push(transferBody[i]); }
    for i in 0..replyMsg.len() { frame.push(replyMsg[i]); }
    Amqp10::writeFrame(sconn, 0, fBegin.chanId, frame);

    let fDisp = Amqp10::readFrame(sconn);
    let pDisp = Amqp10::decodePerformative(fDisp.body);
    if (pDisp.descriptor == 0x15) {
        println("ok-broker-saw-ack");
    }

    httpConnClose(sconn);
    httpServerClose(srv);
    return 0;
}
}
"#;

/// Runs `tinox install` in `src`'s own directory if it has a tinox.toml —
/// core/extended stdlib split (see CLAUDE.md): `src` imports
/// `tinox.core.amqp10`, an extended-tier module that needs a declared+
/// installed dependency now, not just an unconditional stdlib import.
/// `tinox install` must run with its cwd (or, since the `find_project_root`
/// fix, any ancestor) at `src`'s directory, NOT `workdir` — `find_project_
/// root_from` (crates/tinox/src/main.rs) walks up from the FILE BEING
/// BUILT's own directory to find its tinox.toml, unrelated to `workdir`.
fn install_deps_if_needed(tinox: &str, src: &Path) {
    let src_dir = src.parent().expect("src has a parent dir");
    if !src_dir.join("tinox.toml").exists() {
        return;
    }
    let install = Command::new(tinox)
        .arg("install")
        .current_dir(src_dir)
        .output()
        .expect("spawn install");
    assert!(
        install.status.success(),
        "tinox install in {} failed:\nstdout: {}\nstderr: {}",
        src_dir.display(),
        String::from_utf8_lossy(&install.stdout),
        String::from_utf8_lossy(&install.stderr)
    );
}

fn build(tinox: &str, src: &Path, workdir: &Path, out_name: &str) -> PathBuf {
    install_deps_if_needed(tinox, src);
    let exe = workdir.join(out_name);
    let build = Command::new(tinox)
        .arg("build")
        .arg(src)
        .arg("-o")
        .arg(&exe)
        .current_dir(workdir)
        .output()
        .expect("spawn build");
    assert!(
        build.status.success(),
        "build of {} failed:\nstdout: {}\nstderr: {}",
        out_name,
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    exe
}

/// Spawns a child through `stdbuf -oL` so its line output isn't stuck in
/// libc's fully-buffered mode (stdout is a pipe here, not a tty) — both
/// binaries in this test run forever / block on I/O and never exit on their
/// own, so without forced line buffering their marker lines would never be
/// observed before the test kills them. Returns receivers for stdout and
/// stderr lines, read on background threads.
fn spawn_line_buffered(
    exe: &Path,
    workdir: &Path,
) -> (Child, mpsc::Receiver<String>, mpsc::Receiver<String>) {
    let mut child = Command::new("stdbuf")
        .arg("-oL")
        .arg(exe)
        .current_dir(workdir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn child (requires `stdbuf` from GNU coreutils)");
    let stdout = child.stdout.take().expect("child stdout");
    let stderr = child.stderr.take().expect("child stderr");
    let (out_tx, out_rx) = mpsc::channel();
    let (err_tx, err_rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if out_tx.send(line).is_err() {
                break;
            }
        }
    });
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if err_tx.send(line).is_err() {
                break;
            }
        }
    });
    (child, out_rx, err_rx)
}

/// Waits up to `timeout` for a line containing `needle`; on failure, drains
/// whatever else arrived (from `other_rx`, e.g. the same process's stderr)
/// into the panic message for debuggability.
fn wait_for_line(rx: &mpsc::Receiver<String>, needle: &str, timeout: Duration, label: &str, other_rx: &mpsc::Receiver<String>) {
    let deadline = Instant::now() + timeout;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(200));
        match rx.recv_timeout(remaining) {
            Ok(line) => {
                if line.contains(needle) {
                    return;
                }
                seen.push(line);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let stderr_tail: Vec<String> = std::iter::from_fn(|| other_rx.try_recv().ok()).collect();
    panic!(
        "{label}: never saw a line containing {needle:?} within {timeout:?}\nstdout seen: {seen:?}\nstderr: {stderr_tail:?}"
    );
}

#[test]
fn amqp10_consumer_annotation_receives_and_acks() {
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let root = repo_root();
    let consumer_src = root.join("examples/amqp10_consumer_annotated/Main.tnx");

    let workdir = std::env::temp_dir().join(format!(
        "tinox-amqp10-consumer-annotation-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(&workdir).expect("mkdir workdir");

    // Issue #149 stage 3: `class Main` (see FAKE_BROKER_SRC above) must
    // live in a file named exactly `Main.tnx` (one-class-per-file).
    let broker_src_dir = workdir.join("fake_broker_src");
    std::fs::create_dir_all(&broker_src_dir).expect("mkdir fake_broker src dir");
    let broker_src_path = broker_src_dir.join("Main.tnx");
    std::fs::write(&broker_src_path, FAKE_BROKER_SRC).expect("write fake broker source");
    // Core/extended stdlib split: FAKE_BROKER_SRC imports tinox.core.amqp10
    // (extended-tier), so it needs its own tinox.toml declaring it as a
    // dependency (socket is core-tier, needs no declaration) -- see
    // install_deps_if_needed/build above.
    std::fs::write(
        broker_src_dir.join("tinox.toml"),
        format!(
            "[package]\nname = \"fake_broker\"\nversion = \"0.0.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"amqp10\"\nversion = \"{}\"\n",
            extended_module_version("amqp10"),
        ),
    )
    .expect("write fake_broker tinox.toml");

    let broker_exe = build(tinox, &broker_src_path, &workdir, "fake_broker");
    let consumer_exe = build(tinox, &consumer_src, &workdir, "DemoConsumer");

    let (broker_child, broker_out, broker_err) = spawn_line_buffered(&broker_exe, &workdir);
    let _broker_guard = KillOnDrop(broker_child);

    // httpServerCreate binds synchronously right after the broker process
    // starts; the kernel backlog queues the consumer's connect() even if
    // accept() hasn't run yet, so a short fixed wait is enough. No TCP-level
    // readiness probe: this broker only ever accepts ONE connection via a
    // single blocking accept() call, so a stray probe connection would steal
    // that slot and hang the broker forever waiting on a SASL header that
    // never comes.
    std::thread::sleep(Duration::from_millis(500));

    let (consumer_child, consumer_out, consumer_err) = spawn_line_buffered(&consumer_exe, &workdir);
    let _consumer_guard = KillOnDrop(consumer_child);

    wait_for_line(
        &consumer_out,
        "received (text/plain): ABC",
        Duration::from_secs(10),
        "consumer stdout",
        &consumer_err,
    );
    wait_for_line(
        &broker_out,
        "ok-broker-saw-ack",
        Duration::from_secs(10),
        "broker stdout",
        &broker_err,
    );

    let _ = std::fs::remove_dir_all(&workdir);
}
