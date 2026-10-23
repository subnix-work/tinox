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
migrate_on_test = true                    # optional, default false
```

With `migrate_on_startup = true` the program applies pending changeSets right after the database connection is up and
before `main` runs. A failing migration stops the program with a non-zero exit code. Programs that do not opt in are
unchanged. Messages go to stderr, stdout stays untouched. `tinox test` binaries never migrate unless `migrate_on_test = true` is set (independent of `migrate_on_startup`):
every test binary then applies pending changeSets before its test method runs, so a test run against a fresh SQLite
file or Postgres database needs no hand-written setup script. Point `url` at a dedicated test database; the history
table makes repeated test runs cheap no-ops, and a failing migration fails the test.

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
`comment`, `tag` (a name for the state after this changeSet, used by `rollback --tag`) and `rollback`.

Change types: `createTable`, `dropTable`, `renameTable`, `addColumn`, `dropColumn`, `renameColumn`, `modifyEnumCheck`, `createIndex`
(`unique: true` allowed), `dropIndex`, `insert` and `sql`. `sql` is the escape hatch for what the declarative model
cannot express (views, triggers, functions). It must declare its dialect explicitly:
`sql: { dbms: postgres, statements: "..." }` (`statements` is a string or a list of strings, one statement each, no `;`
inside; `dbms` may list several drivers). A missing `dbms` or a `dbms` that does not match the configured driver is a
**hard error**. For per-dialect variants use separate changeSets with a changeSet-level `dbms:`. Combine with
`runOnChange: true` to re-create a view/function whenever its definition changes.

Column types are neutral and mapped per driver: `bigint`, `int`, `varchar(n)`, `char(n)`, `text`, `boolean`, `float`,
`double`, `timestamp`, `date`, `blob`. An unknown type, an unknown key, an unsafe identifier or a change a driver cannot
perform (e.g. SQLite `addColumn` with `NOT NULL` and no default) is a **hard error naming the changeSet**.

## Tinox enum columns

A column can use `type: enum(Name)`, where `Name` is a payload-less `enum` declared in the project's `.tnx` sources
(entity enums, #409). It is stored as `VARCHAR(255)` with a named CHECK constraint `chk_<table>_<column>` listing the
variants, so the schema and the Tinox enum stay consistent. `defaultValue` must be one of the variants. Set
`enumCheck: false` on the column to skip the CHECK (plain text column). An unknown enum, or one with payload variants,
is a hard error naming the changeSet.

```yaml
- column: { name: status, type: enum(Status), defaultValue: Open, constraints: { nullable: false } }
```

Adding a variant is a normal new changeSet: add it to the Tinox enum, then re-create the constraint with
`modifyEnumCheck` (PostgreSQL, MySQL 8.0.16+). SQLite cannot alter a CHECK constraint: use `enumCheck: false`, or rebuild
the table with `sql:`.

```yaml
- changeSet:
    id: 8
    author: tino
    changes:
      - modifyEnumCheck: { tableName: task, columnName: status, type: enum(Status) }
```

Enums are resolved when the changelog loads, so a changeSet's generated SQL reflects the current variants (the checksum
covers the changelog text, not the variants). `modifyEnumCheck` has no automatic rollback; give it a `rollback:` block.

## XML changelogs

A file ending in `.xml` is read as XML instead of YAML (set `changelog = "migrations/changelog.xml"`, or `include` it
from a YAML file and vice versa; formats can be mixed freely). It is converted to the same internal model, so SQL
generation is identical and the checksum of a changeSet equals that of the equivalent YAML changeSet whose keys are in
the order `id`, `author`, other attributes, `comment`, `changes`.

```xml
<databaseChangeLog>
  <include file="2026-10-person.yaml"/>
  <changeSet id="1" author="tino">
    <comment>person table</comment>
    <createTable tableName="person">
      <column name="id" type="bigint" autoIncrement="true"><constraints primaryKey="true"/></column>
      <column name="name" type="varchar(100)"><constraints nullable="false"/></column>
    </createTable>
    <createIndex tableName="person" indexName="idx_person_name"><column name="name"/></createIndex>
    <insert tableName="person"><column name="name" value="Ada"/></insert>
    <sql dbms="postgres">UPDATE person SET name = 'x'</sql>
    <sql><statement>DELETE FROM a</statement><statement>DELETE FROM b</statement></sql>
  </changeSet>
</databaseChangeLog>
```

Attribute names are the YAML keys. `true`/`false` and plain numbers in attributes (`value`, `defaultValue`, `id`) are
typed like YAML scalars; anything else is text. `xmlns`/`xsi:` attributes on the root are ignored. Unknown elements and
attributes are hard errors, as in YAML. DOCTYPE internal subsets are not supported.

## Behaviour

- Applied changeSets are recorded in `tinox_changelog` (id, author, file, checksum, time, order). The checksum covers
  the changeSet's content, not its formatting or the driver.
- **A changeSet that was edited after it ran is an error** (checksum mismatch); nothing is applied. Add a new changeSet
  instead, or set `runOnChange: true`.
- Each changeSet runs in its own transaction together with its history row (PostgreSQL, SQLite). A failing statement rolls
  back the whole changeSet; earlier changeSets stay applied. MySQL commits DDL implicitly, so a changeSet is not atomic
  there: if statement N fails, statements 1..N-1 stay applied and the changeSet gets **no** `tinox_changelog` row. The
  error names the number of statements that already ran. Re-running would repeat them, so repair the schema by hand
  (drop what was created, or finish the rest) and run `update` again, or `tinox db baseline` if it now matches. On MySQL
  prefer one DDL statement per changeSet so a failure is all-or-nothing.
- A lock row in `tinox_changelog_lock` serializes instances: a second starter waits (up to
  `TINOX_MIGRATE_LOCK_WAIT_MS`, default 30000) and then finds nothing left to do. A lock left behind by a crashed run is
  never broken automatically: `tinox db release-lock`. Concurrent starters are verified for PostgreSQL and MySQL (docker); SQLite is a
  single-file database and not meant for several migrating processes.
- Not implemented yet: contexts.

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
tinox db new "Create person table"     # append a skeleton; no database needed
tinox db new "Add email" --include changes/add-email.yaml  # new file + append its include
tinox db validate      # parse + check the changelog, no database needed
tinox db update-sql    # print the SQL that `update` would run
tinox db status        # applied / pending / changed changeSets (exit 1 on a checksum mismatch)
tinox db update        # apply pending changeSets
tinox db baseline      # record pending changeSets as applied WITHOUT running them (adopt an existing schema)
tinox db rollback --count N   # undo the newest N applied changeSets
tinox db rollback --tag T     # undo everything applied after the changeSet tagged T
tinox db validate --rollback  # additionally require every changeSet to be reversible
tinox db release-lock  # remove a stale lock
```

`new` uses the configured changelog path (creating it and its parent directories if missing),
generates an ID from a zero-padded Unix timestamp in nanoseconds plus the description as a slug,
and reads the author from `git config user.name` in the project directory. Configure that name
first; a missing name is an error. The description becomes the changeSet's `comment`.
Generated IDs increase across the changelog and all its includes, and new entries are appended
after existing entries. Existing changelog text, comments and changeSet checksums are preserved.

`--include` takes a new `.yaml`, `.yml` or `.xml` filename relative to the changelog's directory;
it creates parent directories and refuses to overwrite an existing file. YAML and XML may be
mixed. Without `--include`, the skeleton uses the changelog's format. Run the command from the
project directory or any subdirectory. It requires `[database] driver` configuration, but no
database URL, connection or application build.

The skeleton intentionally has no changes. Fill in `changes: []` (YAML) or add change elements
inside `<changeSet>` (XML), then run `tinox db validate`. Validation and migration reject an
unfinished skeleton rather than recording an empty migration.

`status`, `update`, `baseline` and `release-lock` compile a tiny runner that carries the changelog and execute the same
engine as `migrate_on_startup` (`tinox_migrate_run` in `runtime/runtime.c`); relative SQLite URLs resolve against the
project root.

## Rollback

A changeSet can declare how to undo itself with an explicit `rollback:` block (a list of changes, same syntax as
`changes:`, including `sql`). Without one, the inverse is derived for `createTable` (dropTable), `addColumn`
(dropColumn), `createIndex` (dropIndex), `renameTable` and `renameColumn`. `dropTable`, `dropColumn`, `dropIndex`,
`insert` and `sql` have no automatic inverse: rolling back a changeSet containing one without a `rollback:` block is a
**hard error**, reported before anything is undone (`tinox db validate --rollback` finds them ahead of time). Neither
`rollback` nor `tag` is part of the checksum, so they can be added to an already applied changeSet.

```yaml
- changeSet:
    id: 7
    author: tino
    tag: before-cleanup
    changes:
      - dropColumn: { tableName: person, columnName: legacy }
    rollback:
      - sql: ALTER TABLE person ADD COLUMN legacy varchar(50)
```

`rollback` undoes changeSets newest first, each in its own transaction together with the removal of its history row
(MySQL: not atomic, DDL commits implicitly). It uses the same lock as `update`. It refuses, changing nothing, when
`--count` exceeds the applied changeSets, the tag is unknown or not applied, or a selected changeSet is irreversible,
was modified after it was applied, or is missing from the changelog. A failing rollback statement leaves that
changeSet applied; the ones already undone stay undone.

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
