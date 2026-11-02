//! Bounded executor for many small jobs (issue #551): `executorCreate`, `spawn f(x) on executor`,
//! `executorShutdown`, `executorStat`. One fixture program runs the scenarios selected by `MODE`; the
//! thread count is read from outside through /proc.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn build() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tinox-executor-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let app = dir.join("app");
    let out = Command::new(env!("CARGO_BIN_EXE_tinox"))
        .arg("build")
        .arg(repo_root().join("tests/fixtures/executor/Main.tnx"))
        .arg("-o")
        .arg(&app)
        .output()
        .unwrap();
    assert!(out.status.success(), "build failed:\n{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    app
}

fn run(app: &PathBuf, mode: &str) -> (String, String, bool) {
    let out: Output = Command::new(app).env("MODE", mode).output().unwrap();
    (String::from_utf8_lossy(&out.stdout).to_string(), String::from_utf8_lossy(&out.stderr).to_string(), out.status.success())
}

fn threads_of(pid: u32) -> usize {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    status.lines().find_map(|l| l.strip_prefix("Threads:")).unwrap().trim().parse().unwrap()
}

#[test]
fn executor_scenarios() {
    let app = build();

    // Typed results (Int64 and String) across a thousand jobs on three reusable workers.
    let (out, err, ok) = run(&app, "typed");
    assert!(ok, "{out}{err}");
    assert_eq!(out.lines().collect::<Vec<_>>(), [
        "sum=999000",
        "labels=job-0,job-1,job-2,job-3,job-4,",
        "workers=3 completed=1005 rejected=0",
    ]);

    // An error thrown inside a job reaches the awaiting caller; the worker keeps serving.
    let (out, err, ok) = run(&app, "errors");
    assert!(ok, "{out}{err}");
    assert_eq!(out.lines().collect::<Vec<_>>(), ["caught: boom 7", "after failure: 8"]);

    // A full queue rejects visibly (no caller-runs, no unbounded growth).
    let (out, err, ok) = run(&app, "overload");
    assert!(ok, "{out}{err}");
    assert!(out.contains("rejection: executor queue full: too many waiting jobs"), "{out}");
    assert!(out.contains("ok=2 rejected=4"), "{out}");
    assert!(out.contains("stat rejected=4 peak=2"), "{out}");

    // Cancelling a running job and a job that is still queued.
    let (out, err, ok) = run(&app, "cancel");
    assert!(ok, "{out}{err}");
    assert_eq!(out.lines().collect::<Vec<_>>(), ["cancelled: Task cancelled", "queued: Task cancelled"]);

    // Shutdown without drain rejects waiting jobs, cancels running ones and refuses later submissions.
    let (out, err, ok) = run(&app, "shutdown");
    assert!(ok, "{out}{err}");
    assert_eq!(out.lines().collect::<Vec<_>>(), [
        "waiting: executor shut down before the job started",
        "running: Task cancelled",
        "late: executor is shut down",
    ]);

    // Shutdown with drain runs everything that was accepted.
    let (out, err, ok) = run(&app, "drain");
    assert!(ok, "{out}{err}");
    assert_eq!(out.trim(), "drained 4");

    // A job may neither submit to nor await its own executor (nested waits would deadlock).
    let started = Instant::now();
    let (out, err, ok) = run(&app, "nested");
    assert!(ok, "{out}{err}");
    assert!(out.contains("nested: executor job cannot await a job of its own executor"), "{out}");
    assert!(started.elapsed() < Duration::from_secs(4), "nested wait must be refused, not time out");

    // Jobs run under a structured scope like any other task.
    let (out, err, ok) = run(&app, "scoped");
    assert!(ok, "{out}{err}");
    assert_eq!(out.trim(), "scoped=60");
}

#[test]
fn executor_reuses_a_bounded_number_of_threads() {
    let app = build();
    // Baseline: an idle Tinox program already owns the runtime's helper threads (GC markers etc.).
    let base_dir = std::env::temp_dir().join(format!("tinox-executor-base-{}", std::process::id()));
    std::fs::create_dir_all(&base_dir).unwrap();
    std::fs::write(base_dir.join("Main.tnx"), "import tinox.core.process\nclass Main\n{\n    fnc main() -> Int32\n    {\n        println(\"ready\")\n        Process::sleep(2500)\n        return 0\n    }\n}\n").unwrap();
    let base_app = base_dir.join("app");
    let built = Command::new(env!("CARGO_BIN_EXE_tinox")).arg("build").arg(base_dir.join("Main.tnx")).arg("-o").arg(&base_app).output().unwrap();
    assert!(built.status.success());

    fn sample(app: &PathBuf, mode: Option<&str>) -> usize {
        use std::io::{BufRead, BufReader};
        let mut cmd = Command::new(app);
        if let Some(m) = mode {
            cmd.env("MODE", m);
        }
        let mut child = cmd.stdout(Stdio::piped()).spawn().unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.as_mut().unwrap()).read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "ready");
        let n = threads_of(child.id());
        let _ = child.kill();
        let _ = child.wait();
        n
    }

    let baseline = sample(&base_app, None);
    // 3000 jobs ran through a 4-worker executor: the process holds the baseline plus the workers, not 3000 threads.
    let with_executor = sample(&app, Some("threads"));
    assert!(with_executor <= baseline + 4 + 1, "baseline {baseline} threads, with executor {with_executor}");
    let _ = std::fs::remove_dir_all(&base_dir);
}
