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
`comment`, `tag` (a name for the state after this changeSet, used by `rollback --tag`) and `rollback`.

Change types: `createTable`, `dropTable`, `renameTable`, `addColumn`, `dropColumn`, `renameColumn`, `createIndex`
(`unique: true` allowed), `dropIndex`, `insert` and `sql`. `sql` is the escape hatch for what the declarative model
cannot express (views, triggers, functions). It must declare its dialect explicitly:
`sql: { dbms: postgres, statements: "..." }` (`statements` is a string or a list of strings, one statement each, no `;`
inside; `dbms` may list several drivers). A missing `dbms` or a `dbms` that does not match the configured driver is a
**hard error**. For per-dialect variants use separate changeSets with a changeSet-level `dbms:`. Combine with
`runOnChange: true` to re-create a view/function whenever its definition changes.

Column types are neutral and mapped per driver: `bigint`, `int`, `varchar(n)`, `char(n)`, `text`, `boolean`, `float`,
`double`, `timestamp`, `date`, `blob`. An unknown type, an unknown key, an unsafe identifier or a change a driver cannot
perform (e.g. SQLite `addColumn` with `NOT NULL` and no default) is a **hard error naming the changeSet**.

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
  there.
- A lock row in `tinox_changelog_lock` serializes instances: a second starter waits (up to
  `TINOX_MIGRATE_LOCK_WAIT_MS`, default 30000) and then finds nothing left to do. A lock left behind by a crashed run is
  never broken automatically: `tinox db release-lock`. Concurrent starters are verified for PostgreSQL; SQLite is a
  single-file database and not meant for several migrating processes.
- Not implemented yet: preconditions, contexts, generating a changelog from `@Entity` classes.

## CLI

```
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
