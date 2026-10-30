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

Not covered (yet): `@Cacheable` / `@CacheEvict` annotations, a Redis-backed variant, per-key TTLs.

Verified in `tests/e2e/loading_cache` (sixteen concurrent callers cause one load -- repeated 20x with identical output --,
TTL expiry, LRU bound, failing loader, eviction racing a load).
