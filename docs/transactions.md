# Transactions: `@Transactional` propagation and savepoints

`@Transactional` (class or method level; needs `[database] driver` = `postgres`, `mysql` or `sqlite`, otherwise
a compile error) takes an optional propagation mode. A method-level annotation overrides the
class-level one. Without an open transaction every mode simply begins one.

| Mode | Inside an open transaction | Failure inside |
|---|---|---|
| `"REQUIRED"` (default) | joins it | marks the shared transaction **rollback-only** |
| `"NESTED"` | `SAVEPOINT`; released on success | `ROLLBACK TO SAVEPOINT`, the outer transaction continues if the caller catches the error |
| `"REQUIRES_NEW"` | suspends it and runs an independent transaction on a second pooled connection | rolls back only itself |

```tinox
@Transactional("NESTED")
fn importRow(row: Row) -> Nothing { ... }
```

Rules worth knowing:

- **Rollback-only is enforced.** If a joined (`REQUIRED`) call fails and the owner catches the error
  and returns normally, the owner does not commit: it rolls back and throws
  `transaction rolled back: a joined call ... (rollback-only)`. Use `NESTED` when partial failure is
  expected.
- Errors are always re-thrown; `@Transactional` never swallows. Explicit `return`, `throw` and task
  cancellation all go through the same leave path (a savepoint is released / a transaction ends exactly once).
- `REQUIRES_NEW` keeps the outer connection, so it needs `[database] pool >= 2` (checked at runtime,
  hard failure otherwise) and an independent transaction cannot see the outer one's uncommitted rows.
  Nesting depth is limited to 8 `REQUIRES_NEW` levels.

## Drivers

| | PostgreSQL | MySQL | SQLite |
|---|---|---|---|
| Connections | pool (`[database] pool`) | pool (`[database] pool`) | one serialized connection |
| REQUIRED / NESTED | yes | yes (InnoDB) | yes |
| REQUIRES_NEW | yes (pool >= 2) | yes (pool >= 2) | **compile error** |
| A transaction blocks others | no (own connection) | no (own connection) | **yes**: every other statement waits for the connection |

SQLite: while a transaction is open on one thread, statements of other threads wait (they never
run inside it). The wait is bounded by `TINOX_DB_LOCK_TIMEOUT_MS` (default 30000): a plain statement
that times out aborts the process with a visible message, a transaction that cannot start throws.
Keep SQLite transactions short. The same value is SQLite's busy timeout for locks held by other
processes.

MySQL: the ORM emits PostgreSQL-style SQL; the driver translates `$n` placeholders to `?` and
answers `INSERT ... RETURNING id` with the statement's insert id.

Application transactions (`@Transactional`) are separate from schema changesets
(`docs/database-changesets.md`), which have their own locking and transactions.

Verified against a real PostgreSQL with an independent `psql` reading the final table:
`crates/tinox/tests/postgres_propagation.rs`.
