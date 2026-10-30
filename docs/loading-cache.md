# `LoadingCache` (`tinox.core.cache`)

```tinox
import tinox.core.cache

const prices = LoadingCache::create(500, 30000)              // at most 500 entries, 30 s time-to-live
const price = prices.get("sku-7", key => loadPriceFromDb(key))
...
prices.evict("sku-7")        // after updating that row; prices.evictAll() for everything
```

- **Bounded memory:** at most `capacity` entries (1..100000); the least recently used one is dropped when full.
  Expired entries are never served.
- **Single-flight:** concurrent identical misses run the loader **once**; the other callers wait for that result.
  The wait is bounded by `TINOX_LC_WAIT_MS` (default 60 s); a waiter that times out loads on its own and is counted in
  `waitedOut()`. So the bound on loads per key and miss is: one, plus one per timed-out waiter.
- **Failures are not cached:** a loader that throws caches nothing, its caller gets the error, waiting callers retry (one of
  them becomes the loader). `failures()` counts them. A stale or unrelated value is never returned instead.
- **Eviction vs. concurrent loads:** `evict`/`evictAll` also invalidate loads in flight. Such a load still returns its value
  to its own caller, but it is **not stored**, so a value read before an update cannot outlive the eviction that followed
  it. Evict *after* committing the update.
- **Serialization is explicit:** values are Strings; encode other types yourself (JSON, `toString`, ...). Nothing is
  converted implicitly.
- **Inspectable:** `hits()`, `misses()`, `loads()`, `evictions()`, `failures()`, `waitedOut()`, `size()`.
- **Scope:** in-process only. Each instance of a program has its own cache; use short TTLs for data that other writers
  change. A shared backend is the Redis client (`tinox.core.redis`), not hidden behind this class.

## Annotations: `@Cacheable` / `@CacheEvict`

```tinox
class Catalog {
    @Cacheable(60, 1000)                       // ttlSeconds, maxEntries
    fn price(id: Int64) -> Int64 { ... }

    @Transactional
    @CacheEvict("Catalog.price")               // "Class.method" of a @Cacheable method; several allowed
    fn setPrice(id: Int64, price: Int64) -> Nothing { ... }
}
```

- The cache key is built from the argument values (strings length-prefixed, numbers tagged), so `("a","bc")` and
  `("ab","c")` never collide. Parameters and the result must be `String`, `Int64`, `Int32` or `Bool` (a compile error
  otherwise: nothing is serialized implicitly). A `String` result of `null` is not cached.
- Backend: **in-process only**, one `LoadingCache` per annotated method, shared by all instances of the class -- so the
  method must not depend on instance state. There is no hidden Redis backend: when several instances must share cached
  data use `tinox.core.redis` explicitly (or accept TTL-bounded divergence between instances).
- A method that throws caches nothing; the error reaches the caller untouched.
- `@CacheEvict` empties the named caches only after the method **completed normally** -- after its own `@Transactional`
  commit, because the evict wrapper sits outside the transaction wrapper. A rolled-back update leaves cache and data alone.
  Loads that started before the eviction cannot store their (possibly stale) value afterwards.
- A `@CacheEvict` naming a method that is not `@Cacheable` is a compile error; a method cannot be both.

Verified in `tests/e2e/loading_cache` and `tests/e2e/cacheable_annotation` (annotation: repeat/argument keys/16 concurrent callers = 1 call/failure not cached/eviction), and `crates/tinox/tests/cache_transactions.rs` (SQLite: eviction after commit, rollback keeps the cache, six hammering readers never keep the old value, 10 rounds); `LoadingCache` (sixteen concurrent callers cause one load -- repeated 20x with identical output --,
TTL expiry, LRU bound, failing loader, eviction racing a load).
