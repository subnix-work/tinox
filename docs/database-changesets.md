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
- Not implemented yet: rollback, contexts, generating a changelog from `@Entity` classes.

## Preconditions

A changeSet may carry `preConditions`, checked against the live database right before it would run (not by
`validate`, `update-sql`, `status` or `baseline`):

```yaml
- changeSet:
    id: 7
    author: tino
    preConditions:
      onFail: MARK_RAN          # HALT (default) | CONTINUE | MARK_RAN | WARN
      checks:
        - not: { tableExists: { tableName: audit_log } }
        - dbms: postgres,sqlite
    changes:
      - createTable: ...
```

Checks (all must hold): `tableExists`, `columnExists` (`tableName`, `columnName`), `indexExists` (`tableName`,
`indexName`), `sqlCheck` (`sql`: one query, `expectedResult`: compared as text with the first column of the first row),
`dbms` (comma separated driver list) and `not:` around any one of them. A `sqlCheck` query that itself fails is always
a hard error.

`onFail` decides what happens when a check does not hold:

- `HALT` (default): print which check failed, release the lock and exit non-zero; nothing further is applied.
- `CONTINUE`: skip the changeSet for now, record nothing; it is checked again on the next run.
- `MARK_RAN`: record the changeSet as applied without running it (the change is already there).
- `WARN`: print a warning and run it anyway.

`preConditions` are not part of the checksum, so they can be added to an already applied changeSet. Unlike a changeSet-level
`dbms:`, a failing `dbms` precondition goes through `onFail`.

## CLI

```
tinox db validate      # parse + check the changelog, no database needed
tinox db update-sql    # print the SQL that `update` would run
tinox db status        # applied / pending / changed changeSets (exit 1 on a checksum mismatch)
tinox db update        # apply pending changeSets
tinox db baseline      # record pending changeSets as applied WITHOUT running them (adopt an existing schema)
tinox db release-lock  # remove a stale lock
```

`status`, `update`, `baseline` and `release-lock` compile a tiny runner that carries the changelog and execute the same
engine as `migrate_on_startup` (`tinox_migrate_run` in `runtime/runtime.c`); relative SQLite URLs resolve against the
project root.

## Adopting an existing database

If the schema already exists (created by hand or by an earlier tool), write the changelog to match it and run
`tinox db baseline` once; later changeSets then apply normally.
