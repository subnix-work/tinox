# ORM query contract

`DB::of(Entity).filter(...).orderBy(...).limit(n).offset(m).list() / first() / count()`

- **Nothing is dropped silently.** A `filter` that cannot be lowered to SQL, an unknown `orderBy`
  field, a non-lambda argument, or a non-literal `limit`/`offset` is a compile error naming the entity
  (it used to be ignored, which returned rows the caller had excluded). Supported filters: comparing an
  entity field with a value (`== != < <= > >=`), `&&`, `||`, `!`, and `startsWith`/`endsWith`/`contains`
  on a String field.
- **Deterministic order.** Explicit `orderBy` columns come first, the primary key(s) always follow as a
  tiebreaker (also when no `orderBy` is given), so pages never overlap or skip rows. `count()` is unordered.
- **Bounded pages.** `limit(n)` must be an integer literal in `1..=10000` (the SQL text is fixed at
  compile time, which the SQLite statement cache relies on); `offset(m)` must be a literal `>= 0` and needs a
  `limit(...)`. `first()` issues `LIMIT 1`.
- **Absence is explicit.** `count() > 0` tells "no row"; `first()` on no row returns the default entity
  (empty strings, zeros) -- check with `count()` when the difference matters. A database error is never an
  empty result: it aborts with the driver's message.

Verified with an independent `sqlite3` in `crates/tinox/tests/orm_query_contract.rs`.

## Typed contract (type checker, `tinox describe`, editors)

The stub class `EntityQuery` can only say `Any`, because the compiler lowers `DB::of(Entity)` chains by shape. The type
checker therefore enforces the real signatures for an `@Entity` class `T`:

| Call | Type / check |
|---|---|
| `filter(x => ...)`, `orderBy(x => ...)`, `orderByDesc(x => ...)` | the lambda parameter is a `T`: fields and their types are checked, completion works |
| `limit(n)`, `offset(n)` | `n` must be an integer (and a literal, see above) |
| `list()` | `List<T>` -- assigning it to `List<Other>` is a compile error; loop variables are typed `T` |
| `first()` | `T` |
| `count()` | `Int64` |
| `save(e)` | `e` must be a `T` (`save expects T, found X`), returns `T` |
| `delete(e)` | `e` must be a `T` |
| `saveAll(es)` | `es` must be a `List<T>` |

`tinox describe <Entity>` (text or `--json`) lists these as generated `orm-query` members with their documentation
(`DB::of(Item).list() -> List<Item>`, ...), so tooling and AI clients get the accurate contract instead of `Any`.
Chain intermediates (the value between `filter(...)` and `list()`) are still `Any` internally; the contract above applies at
the terminals.

## Loading related rows: explicit batches, no hidden N+1

There are no lazy or eager relationship fields: nothing is loaded unless a query is written, and every query is visible.
Related rows are fetched in **batches** with `keys.contains(x.field)`, which becomes one `col IN (...)`:

```tinox
const books: List<Book> = DB::of(Book).orderBy(b => b.id).limit(50).list()          // query 1
var ids: List<Int64> = []
for b in books { ids.push(b.authorId) }
const authors = DB::of(Author).filter(a => ids.contains(a.id)).list()                // query 2, however many books
var byId: Map<String, Author> = Map::new()
for a in authors { byId[a.id.toString()] = a }
for b in books { println(b.title + " by " + byId[b.authorId.toString()].name) }
```

The other direction is the same shape: `DB::of(Book).filter(b => authorIds.contains(b.authorId)).orderBy(...).limit(n).list()`.

- **Query count:** a list/detail view costs `1 + number of related entity types` queries, independent of the number of rows
  (the loop that calls `.first()` per row costs one per row -- 12 vs 1 in the test).
- **Bounds:** at most **1000 keys** per `contains`; more is a hard runtime error ("load the keys in chunks"), so a
  statement can never grow without limit. At most one `contains` per query (a compile error otherwise). An empty list
  matches nothing -- also under `!` (`!keys.contains(x.id)` matches everything). The list is padded to a power of two
  so only a few distinct SQL texts exist.
- **Works on** PostgreSQL, MySQL and SQLite (placeholders follow the scalar ones; the SQLite driver now rewrites `$n` to
  `?NNN` so placeholder order can never mis-bind parameters).
- **Joins in SQL** are not offered: a joined row type would need generated result classes. The batch above returns the
  same data with a bounded, visible number of statements.
- **Schema agrees with access:** `@ForeignKey` (see `docs/database-changesets.md`) is checked against the program at
  compile time (missing target, wrong column/type, cycles are errors), generated parents-first by `tinox db generate`
  and enforced by the database, so a loaded `authorId` always has its author.

Verified against a real PostgreSQL (`crates/tinox/tests/orm_relations.rs`): schema from `db generate` + `db update`, the
server rejects orphans and parent deletes, and the server's own statement log shows 12 queries for the per-row loop vs 1
for the batch, with identical joined output. IN lists also run in the MySQL, PostgreSQL and SQLite transaction tests and
against sqlite3 (`orm_query_contract.rs`: duplicates, strings, empty list, `!`, 700 keys, over-limit failure).
