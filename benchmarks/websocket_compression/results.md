# permessage-deflate: stateless vs context takeover (issue #560)

Measured 2026-11-01, compiler 2.9.1, `tinox.core.websocket` 1.1.2, `websockets` 17.2 as the
independent client, zlib via Python 3.14. Reproduce: build `server/` and run
`python3 measure.py <binary>`. Byte counts are deterministic (seeded corpora); the CPU columns
are single-run and noisy (about +-20%).

Current behaviour: `Ws.handshake` always answers `permessage-deflate; client_no_context_takeover;
server_no_context_takeover`, so every message uses a fresh DEFLATE context.

## Model (zlib, 2000 messages per corpus)

| corpus | avg raw B | stateless B | takeover B | stateless ratio | takeover ratio | stateless us/msg | takeover us/msg |
|---|---|---|---|---|---|---|---|
| chat json | 120 | 91 | 20 | 0.76 | 0.16 | 4.4 | 3.6 |
| telemetry json | 67 | 60 | 11 | 0.90 | 0.17 | 2.7 | 1.4 |
| log lines | 82 | 83 | 16 | 1.02 | 0.20 | 3.3 | 1.8 |
| random hex (control) | 120 | 81 | 91 | 0.68 | 0.76 | 3.1 | 5.2 |

Persistent deflate+inflate state: about 99 KiB per connection (500 connections raised peak RSS by 48 MiB; at the default 1024 WebSocket budget: ~99 MiB)

## Live (compiled Tinox echo server, `websockets` client, counting proxy)

| corpus | compression | messages | echo ok | server->client B | vs stateless model B | negotiated |
|---|---|---|---|---|---|---|
| chat json | deflate | 500 | True | 46365 | 46130 | permessage-deflate; client_no_context_takeover; server_no_context_takeover |
| chat json | off | 500 | True | 61020 | 60471 | none |
| telemetry json | deflate | 500 | True | 31009 | 30774 | permessage-deflate; client_no_context_takeover; server_no_context_takeover |
| telemetry json | off | 500 | True | 34573 | 34440 | none |
| log lines | deflate | 500 | True | 42694 | 42459 | permessage-deflate; client_no_context_takeover; server_no_context_takeover |
| log lines | off | 500 | True | 41960 | 41827 | none |
| random hex (control) | deflate | 500 | True | 42021 | 41786 | permessage-deflate; client_no_context_takeover; server_no_context_takeover |
| random hex (control) | off | 500 | True | 61133 | 61000 | none |

## Reading the numbers

- **The model matches the real server.** Live server->client bytes for the stateless baseline are
  within 0.5% of the zlib model (the 235 B difference is frame headers), so the model's takeover
  numbers are a fair estimate of what context takeover would put on the wire.
- **Negotiation and framing are verified by an independent client**: `websockets` negotiated the
  parameters above and received every echoed message byte-exact in all eight runs.
- **Bandwidth:** for repeated small messages with shared structure, context takeover would be
  4.7x (chat JSON), 5.4x (telemetry JSON) and 5.2x (log lines) smaller than the stateless result.
  The stateless mode saves 24% on chat, 10% on telemetry, and **expands log lines by 2%** (the
  server compresses even when the result is not smaller; RFC 7692 lets a sender skip compression
  per message).
- **CPU:** takeover is not more expensive per message here (1.4-3.6 us vs 2.6-4.4 us), because
  it avoids re-initialising the compressor for every message. Only incompressible data costs more.
- **Memory:** a persistent deflate + inflate pair holds about 99 KiB per connection. At the
  default budget of 1024 WebSocket connections (`TINOX_MAX_WS_CONNECTIONS`) that is about 99 MiB
  in the worst case; zero when disabled.

## Decision input

The evidence supports an **opt-in** context-takeover mode (default unchanged): a large bandwidth win
for chatty small-message endpoints, bounded by the existing connection budget, at ~99 KiB per
connection. Not measured here: RSS of the full server under load and latency at the tail, which
belong with the implementation. A cheap, default-compatible improvement that needs no new state is
to send a message uncompressed (RSV1 clear) when deflate does not shrink it.

## Implemented: opt-in context takeover (`tinox.websocket.compression = context`)

`tinox.core.websocket` 1.1.3 keeps one DEFLATE and one INFLATE context per connection when the
setting is `context` (application.properties key or `TINOX_WEBSOCKET_COMPRESSION`); the default
`stateless` is unchanged. Same corpora, 500 messages, `websockets` client through the counting
proxy, server->client bytes:

| corpus | stateless | context takeover | smaller by |
|---|---|---|---|
| chat json | 46365 | 11377 | 4.1x |
| telemetry json | 31009 | 6890 | 4.5x |
| log lines | 42694 | 9528 | 4.5x |
| random hex (control) | 42018 | 46817 | 0.9x (slightly larger) |

Verified by `crates/tinox/tests/ws_context_takeover.rs` with the independent client: negotiation echoes
`client_no_context_takeover`, `server_no_context_takeover` and `server_max_window_bits` (9 and 10 tried),
300 messages plus a 100 KB and an empty message stay byte-exact on one context, the wire is at least
halved against the stateless default, 150 closed connections do not accumulate per-connection state
(RSS growth under 8 MiB; a leak would be ~15 MiB), and an unknown setting value aborts the process.

Limits: only the server side (WsServer / `@WebsocketEndpoint` / Tinox UI) honours the setting, `WsClient`
stays stateless; the first `permessage-deflate` offer is the one considered; a `server_max_window_bits`
below 9 declines the extension; incompressible payloads cost slightly more than stateless. The idea of
sending a message uncompressed when deflate does not shrink it is not part of this change: with context
takeover it would desynchronise the peer's window, and the stateless default keeps its behaviour.
