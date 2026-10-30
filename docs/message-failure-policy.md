# AMQP 0-9-1 consumer failure policy (`@MessageFailure`)

```tinox
@Amqp091Consumer("broker", 5672, "/", "user", "pass", "orders")
class Orders {
    @OnMessage
    @MessageFailure(3, 200, "orders.dlq")      // maxAttempts, backoffMs, optional dead-letter queue
    fn onMessage(msg: AmqpMessage091) -> Nothing { ... }
}
```

A delivery is acknowledged **only if the handler returned normally**. If it throws:

1. The error is printed (`message handler Class.method failed: ...`) and cleared.
2. The handler runs again in the same process after `backoffMs * 2^(attempt-1)` (capped at 60 s), up to `maxAttempts`
   (1..20) in total. Retries are bounded; nothing retries forever.
3. After the last failed attempt the message is **settled**, never acknowledged as handled:
   - with a dead-letter queue: it is published there (default exchange, routing key = queue name; body and content
     type are kept, headers are not) and then acknowledged. If that publish fails the delivery is rejected **with**
     requeue so it stays in the broker.
   - without one: `basic.reject(requeue=false)`. The broker discards it, or dead-letters it through the queue's own
     `x-dead-letter-exchange` -- broker semantics are not replaced. A consumer without `@MessageFailure` behaves like
     `@MessageFailure(1, 0)`: one attempt, then reject.

Other situations:

- **Shutdown** (`@Service` stopping while a handler failed): the delivery is left unacknowledged, the connection
  closes and the broker redelivers it.
- **Crash / restart**: an unacknowledged delivery returns to the queue and arrives again with `msg.redelivered == true`.
  Delivery is **at-least-once; there is no exactly-once guarantee**. Handlers must tolerate duplicates.
- **Idempotency**: `IdempotencyWindow091.create(n)` keeps the last `n` keys in memory (`firstSeen(msg.messageId)`);
  it suppresses duplicates within one process only. Durable de-duplication needs a database unique key.
- Prefetch stays 1, so messages of one queue are handled one at a time.

Not covered: AMQP 1.0 consumers (`@Amqp10Consumer` is unchanged), a handler timeout, publishing headers on the
dead-letter copy, and `@Transactional`/tracing integration beyond what the handler itself does.

Verified against a real RabbitMQ with independent observers (`rabbitmqctl` queue depths, the management API for
publishing and reading the dead-letter queue): `crates/tinox/tests/amqp091_failure_policy.rs` covers transient failure
(3 attempts, one success), permanent failure (3 attempts, dead-lettered, original payload), a plain queue with its own
DLX, a consumer killed mid-handler (redelivered), and duplicate suppression.
