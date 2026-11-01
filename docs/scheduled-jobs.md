# `@Scheduled` jobs

```tinox
import tinox.core.cron

class Reports {
    @Scheduled("*/15 9-17 * * 1-5")      // minute hour day-of-month month day-of-week, UTC
    fn refresh() -> Nothing { ... }
}
```

A method with no parameters and return type `Nothing` gets its own background thread, started with the program.
The class is created with field defaults (no constructor, no injection -- put shared state in an
`@ApplicationComponent` and use it from there).

- **Time zone:** always UTC, minute resolution, five numeric fields (see `tinox.core.cron`). There are no
  daylight-saving gaps or repeats; convert local schedules to UTC yourself.
- **Exactly once per occurrence:** the runner polls every 250 ms but claims a minute once; repeated polls and
  backward clock steps never run it twice.
- **No overlap, deterministic misfires:** one thread per job, so a run never overlaps itself. Minutes that pass
  while a run is still executing are **skipped, not replayed**. The minute in progress at startup is not run late; the
  first run is the next matching minute boundary.
- **Failures:** an exception is printed (`scheduled job Class.method failed: <message>`), counted and cleared; the job
  keeps its schedule. There is no automatic retry -- a failed occurrence is not repeated.
- **Metrics (per job, bounded):** `tinox_scheduled_<class>_<method>_runs_total`, `..._failures_total`, `..._lag_ms`
  (start delay behind the scheduled minute).
- **Shutdown:** with `@Service` the loop ends when the service stops and a running occurrence is admitted as in-flight
  work; without it the threads end with the process.
- **In memory only:** nothing is persisted. After a restart `lastMinute` is forgotten (see the startup rule above); use
  a database row or lock if an occurrence must be exactly-once across restarts or instances. Several instances of a
  program each run their own copy of every job.

Verified: the schedule against counts from Python's `datetime` (`tests/e2e/scheduled_job_schedule`: office hours,
leap day in 2023/2024/2100, day-of-month OR day-of-week) and the runner in a real process
(`crates/tinox/tests/scheduled_jobs.rs`: consecutive minute boundaries, no duplicates, failing job reported, SIGTERM).
