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
