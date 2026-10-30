//! @Scheduled jobs (issue #536) running in a real compiled process. The schedule evaluation itself
//! is verified against an independent reference in tests/e2e/scheduled_job_schedule; this checks the
//! runner: it fires on a real minute boundary, exactly once per minute (it polls 4x per second), a
//! failing job is reported without stopping anything, a job whose schedule never matches stays quiet,
//! and the process stops on SIGTERM.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const JOBS: &str = r#"import tinox.core.cron
import tinox.core.time

class Jobs
{
    @Scheduled("* * * * *")
    fn everyMinute() -> Nothing
    {
        println("JOB ok " + (Time::now() / 60000).toString())
    }

    @Scheduled("* * * * *")
    fn failing() -> Nothing
    {
        throw "boom"
    }

    @Scheduled("0 0 1 1 *")
    fn newYear() -> Nothing
    {
        println("JOB newyear")
    }
}
"#;

#[test]
fn jobs_fire_once_per_minute_report_failures_and_stop_with_the_process() {
    let dir = std::env::temp_dir().join(format!("tinox-scheduled-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Jobs.tnx"), JOBS).unwrap();
    std::fs::write(dir.join("Main.tnx"), "import Jobs\n\nclass Main\n{\n    fnc main() -> Int32 {\n        return 0\n    }\n}\n").unwrap();
    let build = Command::new(env!("CARGO_BIN_EXE_tinox")).args(["build", "Main.tnx", "-o", "app"]).current_dir(&dir).output().unwrap();
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));

    let mut child = Command::new(dir.join("app"))
        .current_dir(&dir)
        .stdout(Stdio::from(std::fs::File::create(dir.join("out.txt")).unwrap()))
        .stderr(Stdio::from(std::fs::File::create(dir.join("err.txt")).unwrap()))
        .spawn()
        .unwrap();
    // Wait for two consecutive minute boundaries (at most ~2 minutes).
    let started = Instant::now();
    loop {
        let out = std::fs::read_to_string(dir.join("out.txt")).unwrap_or_default();
        if out.matches("JOB ok").count() >= 2 || started.elapsed() > Duration::from_secs(150) {
            break;
        }
        assert!(child.try_wait().unwrap().is_none(), "the process exited early");
        std::thread::sleep(Duration::from_millis(500));
    }
    let out = std::fs::read_to_string(dir.join("out.txt")).unwrap();
    let err = std::fs::read_to_string(dir.join("err.txt")).unwrap();
    let minutes: Vec<&str> = out.lines().filter(|l| l.starts_with("JOB ok")).collect();
    assert!(minutes.len() >= 2, "expected two runs, got {minutes:?}");
    let mut sorted = minutes.clone();
    sorted.dedup();
    assert_eq!(sorted, minutes, "a minute ran twice: {minutes:?}");
    let first: u64 = minutes[0].rsplit(' ').next().unwrap().parse().unwrap();
    let second: u64 = minutes[1].rsplit(' ').next().unwrap().parse().unwrap();
    assert_eq!(second, first + 1, "runs must be on consecutive minute boundaries");
    assert!(!out.contains("newyear"), "a schedule that does not match must not run");
    assert!(err.matches("scheduled job Jobs.failing failed: boom").count() >= 2, "{err}");
    assert!(child.try_wait().unwrap().is_none(), "a failing job must not stop the process");

    unsafe { libc_kill(child.id() as i32, 15) };
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "the process did not stop on SIGTERM");
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}
