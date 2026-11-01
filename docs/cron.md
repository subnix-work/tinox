# Cron scheduling

`tinox.core.cron` evaluates five-field schedules in **UTC**. Timestamps are
Unix milliseconds from `0` (1970-01-01) through `253402300799999`
(9999-12-31 23:59:59.999). Negative or later timestamps throw. Local timezone
settings and daylight saving time do not affect evaluation.

```tinox
const schedule: CronExpr = Cron::parse("*/15 9-17/2 * * 1-5")
const next: Int64 = Cron::nextRunAfter(schedule, 1793382300000)
```

Fields are minute (0–59), hour (0–23), day of month (1–31), month (1–12),
and day of week (0–7, with both 0 and 7 meaning Sunday). Supported syntax:

- `*`: all values in the field.
- `1,5,10`: a list of values or ranges.
- `1-5`: an inclusive, ascending range.
- `*/15` or `1-10/2`: a positive step anchored at the wildcard's minimum or
  range's first value. A step greater than the range selects its first value.

Spaces and tabs separate fields; leading, trailing, and repeated separators
are accepted. Signs, names, macros, seconds/year fields, descending/wrapping
ranges, single-number steps such as `5/10`, and extensions such as `L`, `W`,
`#` and `?` throw during parsing. Manually constructed `CronExpr` objects are
also validated when evaluated.

A literal `*` in either day field leaves that field unrestricted. When both
day fields differ from the literal `*`, **either** may match. Consequently
`0 0 13 * 5` means every Friday and every 13th, while `0 0 13 * *` means only
the 13th. Wildcard steps such as `*/2` count as restricted day fields.

`Cron::matches(expr, timestamp)` evaluates the minute containing the timestamp:
seconds and milliseconds are ignored. `Cron::nextRun(expr)` uses the current
system time. `Cron::nextRunAfter(expr, timestamp)` returns a scheduled minute
boundary **strictly after** the supplied timestamp. Both next-run functions
throw if no future date exists, including valid syntax describing impossible
calendar dates such as `0 0 31 4 *`. The search skips calendar days rather than
scanning every minute, and is bounded by one Gregorian 400-year cycle and the
year-9999 limit. Gregorian leap-year rules include the century exceptions.

`CronScheduler` is caller-driven and single-threaded: `start` enables it,
`stop` disables it, and the caller must invoke `tick`. `tickAt(scheduler,
timestamp)` is the deterministic variant. Each registered job runs at most
once per matching minute, even when a callback throws or re-enters the
scheduler. A job claims its scheduled minute before the callback starts;
callback errors propagate to the caller. `CronJob.lastRun` contains that minute
boundary, or `-1` before its first attempt. Missed minutes are not replayed.
Stopping and restarting retains this state. Clock rollback suppresses job
execution until a matching minute strictly later than its last attempt.
Concurrent ticks require caller synchronization.

This module supplies calendar evaluation and a synchronous polling primitive.
Declarative scheduled components, worker lifecycle, persistence, multi-process
coordination, overlap controls and configurable misfire policies are separate
work tracked in [issue #536](https://github.com/subnix-work/tinox/issues/536).
