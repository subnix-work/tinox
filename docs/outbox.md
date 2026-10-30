# Transactional outbox (`tinox.core.outbox`)

A database update and a broker publish cannot commit together. The outbox makes the *intent to publish* part of the
database transaction and publishes afterwards from a separate dispatcher.

```tinox
import tinox.core.outbox

class Orders {
    @Transactional
    fn place(code: String) -> Nothing {
        DB::of(Shop).save(Shop { id: 0, code: code })
        Outbox::add("orders.q", "order-" + code, "{\"code\":\"" + code + "\"}")   // same transaction
    }
}

// elsewhere (own thread/task or a loop in main):
const d = OutboxDispatcher::create("broker", 5672, "/", "user", "pass", 5, 200)   // maxAttempts, backoffMs
d.run(500)          // until d.stop() or cancellation; drains the batch in hand, always closes the connection
```

Table (add it to your changelog; `tinox db generate` only sees entities of your own `src/`):

```sql
CREATE TABLE tinox_outbox (id BIGSERIAL PRIMARY KEY, destination TEXT NOT NULL, message_id TEXT NOT NULL,
  payload TEXT NOT NULL, status BIGINT NOT NULL, attempts BIGINT NOT NULL, last_error TEXT NOT NULL,
  created_at BIGINT NOT NULL, next_attempt_at BIGINT NOT NULL, sent_at BIGINT NOT NULL);
```

**Guarantees and limits**

- `Outbox::add` runs on the connection of the caller's `@Transactional` method: the record commits or rolls back
  together with the business data (a rolled-back order leaves no message).
- The dispatcher publishes with **publisher confirms** and marks a record sent only after the broker confirmed it.
  A committed record is never lost: it stays `status 0` until a confirm was seen.
- Delivery is **at-least-once, not exactly-once.** If the process dies after the broker confirmed but before the mark
  (or a confirm is lost), the record is published again after the restart **with the same message-id**. Consumers must
  deduplicate on `messageId` (`IdempotencyWindow091`, or a unique key in their database).
- Failures: retried with `backoffMs * 2^attempts` (cap 60 s); after `maxAttempts` the record is **parked**
  (`status 2`, `lastError` says why) instead of retrying forever. Order is by id, but a record in backoff is overtaken.
- Backlog and lag: `Outbox::backlog()`, `Outbox::lagMs()` (age of the oldest pending record), `Outbox::parked()`;
  the dispatcher counts `confirmed` / `failures`.
- Batches of 50, one connection, one record in flight (publish, wait for the confirm, mark). The confirm wait is
  bounded by `confirmTimeoutMs` (10 s); then the connection is dropped and the record retried.
- Several dispatchers on the same table would publish the same record twice; run one per table, or accept the duplicates.

Verified against a real PostgreSQL and RabbitMQ (`crates/tinox/tests/outbox_delivery.rs`, 5 runs): commit/rollback with
the business row, a writer that is gone before anything was published (a later dispatcher delivers everything), and a
crash between the broker's confirm and the mark (a trigger makes the mark fail; after the restart the same message-id is
in the queue twice).

Not covered: an annotation (`Outbox::add` is the write boundary), a lifecycle-managed dispatcher with `@Service`
(call `run` from a task and `stop()` on shutdown), AMQP 1.0, payloads other than text, and automatic pruning of sent rows.
