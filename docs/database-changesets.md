# Database schema changesets

Declarative, driver-independent schema migrations (issue #410). The schema is described as an ordered list of
**changeSets** in a YAML changelog; a changeSet that has not been applied to a database yet is applied, the others are
skipped. Tinox translates every change into SQL for the configured driver (`sqlite`, `postgres`, `mysql`).

## Configuration (`tinox.toml`)

```toml
[database]
driver = "postgres"
url = "postgres://..."
changelog = "migrations/changelog.yaml"   # optional, this is the default
migrate_on_startup = true                 # optional, default false
```

With `migrate_on_startup = true` the program applies pending changeSets right after the database connection is up and
before `main` runs. A failing migration stops the program with a non-zero exit code. Programs that do not opt in are
unchanged. Messages go to stderr, stdout stays untouched. `tinox test` binaries never migrate.

## Changelog format

```yaml
databaseChangeLog:
  - include: { file: 2026-10-person.yaml }     # relative to this file
  - changeSet:
      id: 1
      author: tino
      changes:
        - createTable:
            tableName: person
            columns:
              - column: { name: id, type: bigint, autoIncrement: true, constraints: { primaryKey: true } }
              - column: { name: name, type: varchar(100), constraints: { nullable: false } }
              - column: { name: email, type: varchar(255), constraints: { unique: true } }
```

A changeSet is identified by `id` + `author` (unique across all files). Optional keys: `runOnChange` (re-apply when
its content changed), `runAlways`, `dbms` (comma separated driver list; the changeSet does not exist for other drivers),
`comment`.

Change types: `createTable`, `dropTable`, `renameTable`, `addColumn`, `dropColumn`, `renameColumn`, `createIndex`
(`unique: true` allowed), `dropIndex`, `insert` and `sql`. `sql` is the escape hatch: a string, a list of strings
(one statement each, no `;` inside), or `{ dbms: postgres, statements: ... }` to limit it to one driver.

Column types are neutral and mapped per driver: `bigint`, `int`, `varchar(n)`, `char(n)`, `text`, `boolean`, `float`,
`double`, `timestamp`, `date`, `blob`. An unknown type, an unknown key, an unsafe identifier or a change a driver cannot
perform (e.g. SQLite `addColumn` with `NOT NULL` and no default) is a **hard error naming the changeSet**.

## Behaviour

- Applied changeSets are recorded in `tinox_changelog` (id, author, file, checksum, time, order). The checksum covers
  the changeSet's content, not its formatting or the driver.
- **A changeSet that was edited after it ran is an error** (checksum mismatch); nothing is applied. Add a new changeSet
  instead, or set `runOnChange: true`.
- Each changeSet runs in its own transaction together with its history row (PostgreSQL, SQLite). A failing statement rolls
  back the whole changeSet; earlier changeSets stay applied. MySQL commits DDL implicitly, so a changeSet is not atomic
  there.
- A lock row in `tinox_changelog_lock` serializes instances: a second starter waits (up to
  `TINOX_MIGRATE_LOCK_WAIT_MS`, default 30000) and then finds nothing left to do. A lock left behind by a crashed run is
  never broken automatically: `tinox db release-lock`. Concurrent starters are verified for PostgreSQL; SQLite is a
  single-file database and not meant for several migrating processes.
- Not implemented yet: rollback, preconditions, contexts, generating a changelog from `@Entity` classes.

## CLI

```
tinox db validate      # parse + check the changelog, no database needed
tinox db update-sql    # print the SQL that `update` would run
tinox db status        # applied / pending / changed changeSets (exit 1 on a checksum mismatch)
tinox db update        # apply pending changeSets
tinox db diff          # compare changelog schema with live tables, columns, types and indexes (exit 1 on drift)
tinox db baseline      # record pending changeSets as applied WITHOUT running them (adopt an existing schema)
tinox db release-lock  # remove a stale lock
```

`status`, `update`, `baseline` and `release-lock` compile a tiny runner that carries the changelog and execute the same
engine as `migrate_on_startup` (`tinox_migrate_run` in `runtime/runtime.c`); relative SQLite URLs resolve against the
project root.

## Adopting an existing database

If the schema already exists (created by hand or by an earlier tool), write the changelog to match it and run
`tinox db baseline` once; later changeSets then apply normally.

## Schema drift detection

Run `tinox db diff` before or after `baseline` to check that the actual schema matches the
**entire** changelog, including pending changeSets. It reports missing, unexpected and changed
objects in deterministic order. Exit status is 0 for a match and 1 for drift or an inspection/replay
error. Migration history and lock tables are excluded; no history rows or locks are written.
The live database is only inspected, and SQLite is opened read-only (it must already exist).
Only the current schema/database is compared. Row data, defaults, nullability, foreign keys, views
and triggers are outside the comparison scope. MySQL functional indexes produce an unsupported
index error rather than a potentially incomplete comparison.
Index comparison includes names, uniqueness, ordered columns and direction. SQLite expression and
partial indexes also compare their stored SQL text, so formatting differences can be reported.

The expected schema is produced by replaying all changeSets once, in order, in isolation. This
includes `insert` and raw `sql` changes, with the configured driver's filtering and type mappings.
A replay failure is an error, never a successful drift check. Changes that depend on existing
production data may need a self-contained changelog to replay successfully.

SQLite uses a fresh in-memory shadow database automatically. For PostgreSQL and MySQL, supply
`TINOX_DB_DIFF_URL` pointing to an **empty disposable database**, with permissions to replay the
changelog. Use credentials isolated from production; raw SQL executes as written. The command
rejects a shadow database that already contains tables. PostgreSQL replays in a transaction and
rolls it back; MySQL DDL commits implicitly, so recreate the disposable database between checks
(including after a failed replay). The command never creates or drops server databases itself.

```sh
TINOX_DB_DIFF_URL='postgres://localhost/tinox_shadow' tinox db diff
```

The optional server integration test `changesets_diff_server_catalogs` accepts
`TINOX_TEST_DIFF_POSTGRES_URL` / `TINOX_TEST_DIFF_POSTGRES_SHADOW_URL` and
`TINOX_TEST_DIFF_MYSQL_URL` / `TINOX_TEST_DIFF_MYSQL_SHADOW_URL`. These must point at empty
disposable databases; the test creates and drops its fixture tables. For the MySQL test client,
also set `TINOX_TEST_DIFF_MYSQL_SOCKET` to a local MariaDB server socket accessible as `root`.
