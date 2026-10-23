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
- Not implemented yet: rollback, preconditions, contexts.

## CLI

```
tinox db generate      # derive new changeSets from entity classes (no database needed)
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

## Generating from entities

Run `tinox db generate` from a project with a `[database]` configuration. It parses all `.tnx` files under
`src/` recursively (including namespace blocks), creates the configured changelog if missing, or appends a new
changeSet for new tables and columns. It does not connect to the database. Review the result with
`tinox db update-sql`, then apply it with `tinox db update`. Repeating generation without model changes writes nothing.

```tinox
@Entity
@Table("person")
class Person {
    @Id @GeneratedValue
    id: Int64;
    @Column("display_name") @NotNull
    name: String;
    state: State;
}

enum State { ACTIVE, INACTIVE }
```

`@Entity` or `@Table` marks a persistent class. The default table name is the lowercase class name; the default column
name is the field name. `@Table("name")` and `@Column("name")` override those names. Every field is persisted.
`@Id` generates a primary key; `@GeneratedValue` requires an integer `@Id`. `@NotNull` generates `nullable: false`;
other columns are nullable. Field initializers are application defaults, not database defaults.

| Entity type | Changelog type |
| --- | --- |
| Int8, Int16, Int32, UInt8, UInt16 | int |
| Int64, UInt32 | bigint |
| Float32 / Float64 | float / double |
| Bool | boolean |
| String / payload-free enum | text |
| Char | char(1) |

Enum names are stored as text, matching the ORM; changing enum variants does not require a schema change.
Unsupported types (including UInt64, nullable wrappers, collections, references to other entities and payload enums),
generic entities, inheritance, duplicate table/column names and composite keys are errors.

The comparison replays existing declarative changes, following includes and filtering for the configured driver.
Generated changeSets use author `tinox`, IDs `entity-<driver>-<number>` and a `dbms` filter for that driver. Generate
separately for each driver when maintaining one changelog for multiple databases. Existing changeSets, comments,
formatting and included files remain untouched; new entries are appended to the root block-style
`databaseChangeLog` list. Flow-style lists (`databaseChangeLog: [...]`) and YAML document end markers cannot be
appended to safely and produce an error.

Generation is additive: existing entity columns with changed types, primary keys, generation or nullability, and
removed columns require a manual changeSet. Extra tables may coexist with entities and are left alone. Defaults,
unique constraints, indexes and foreign keys remain manually maintained. Raw `sql` changes prevent automatic
comparison because their schema effects cannot be inferred reliably. Driver restrictions still apply, such as
SQLite refusing a new `NOT NULL` column without a database default. All validation completes before writing.
