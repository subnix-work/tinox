#!/usr/bin/env python3
"""permessage-deflate measurements for issue #560.

Part 1 (model): zlib stand-in for the two strategies on realistic small-message corpora --
the current stateless one (fresh DEFLATE context per message, what Ws.tnx negotiates with
client/server_no_context_takeover) and context takeover (one context per connection).
Part 2 (live): a compiled Tinox echo server (server/) driven by an independent client
(the `websockets` library) through a byte-counting TCP proxy; checks the negotiated
parameters, echo correctness and the real server->client bytes for the stateless baseline.

usage: measure.py <compiled-server-binary>
"""
import asyncio, json, os, random, resource, subprocess, sys, time, zlib

N = 2000
random.seed(560)
WORDS = "the quick brown fox jumps over lazy dog build test deploy service request response user session token error warning started stopped retry timeout ok".split()


def chat(i):
    return json.dumps({"type": "chat", "user": f"user{random.randint(1, 40)}", "room": "general",
                       "text": " ".join(random.choice(WORDS) for _ in range(random.randint(4, 14)))})


def telemetry(i):
    return json.dumps({"sensor": f"temp-{i % 8}", "ts": 1760000000 + i, "value": round(20 + random.random() * 5, 2), "unit": "C"})


def log(i):
    return f"2026-11-01T12:{i // 60 % 60:02d}:{i % 60:02d}Z INFO service=orders request_id={random.randint(10**6, 10**7)} status={random.choice([200, 200, 200, 404, 500])} took_ms={random.randint(1, 300)}"


def noise(i):
    return os.urandom(60).hex()


CORPORA = {"chat json": chat, "telemetry json": telemetry, "log lines": log, "random hex (control)": noise}


def deflate_stateless(msg):
    c = zlib.compressobj(wbits=-15)
    return (c.compress(msg) + c.flush(zlib.Z_SYNC_FLUSH))[:-4]


def model():
    print("## Model (zlib, %d messages per corpus)\n" % N)
    print("| corpus | avg raw B | stateless B | takeover B | stateless ratio | takeover ratio | stateless us/msg | takeover us/msg |")
    print("|---|---|---|---|---|---|---|---|")
    for name, gen in CORPORA.items():
        msgs = [gen(i).encode() for i in range(N)]
        raw = sum(map(len, msgs))
        t = time.perf_counter(); st = sum(len(deflate_stateless(m)) for m in msgs); ts = (time.perf_counter() - t) / N * 1e6
        c = zlib.compressobj(wbits=-15)
        t = time.perf_counter(); tk = 0
        for m in msgs:
            tk += len((c.compress(m) + c.flush(zlib.Z_SYNC_FLUSH))[:-4])
        tt = (time.perf_counter() - t) / N * 1e6
        print(f"| {name} | {raw / N:.0f} | {st / N:.0f} | {tk / N:.0f} | {st / raw:.2f} | {tk / raw:.2f} | {ts:.1f} | {tt:.1f} |")
    # memory held per connection by a persistent compressor + decompressor pair
    base = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    keep = []
    for _ in range(500):
        c = zlib.compressobj(wbits=-15); c.compress(b"x" * 1000); d = zlib.decompressobj(wbits=-15)
        keep.append((c, d))
    grown = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss - base
    print(f"\nPersistent deflate+inflate state: about {grown / 500:.0f} KiB per connection "
          f"(500 connections raised peak RSS by {grown / 1024:.0f} MiB; at the default 1024 WebSocket budget: ~{grown * 1024 / 500 / 1024:.0f} MiB)\n")


async def counting_proxy(listen, target, counts):
    async def pipe(r, w, key):
        try:
            while data := await r.read(65536):
                counts[key] += len(data); w.write(data); await w.drain()
        except (ConnectionError, asyncio.CancelledError):
            pass
        finally:
            w.close()

    async def handle(cr, cw):
        sr, sw = await asyncio.open_connection("127.0.0.1", target)
        await asyncio.gather(pipe(cr, sw, "up"), pipe(sr, cw, "down"))
    return await asyncio.start_server(handle, "127.0.0.1", listen)


async def live(binary):
    import websockets
    server = subprocess.Popen([binary], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(100):
            try:
                await asyncio.open_connection("127.0.0.1", 8794); break
            except OSError:
                await asyncio.sleep(0.05)
        print("## Live (compiled Tinox echo server, `websockets` client, counting proxy)\n")
        print("| corpus | compression | messages | echo ok | server->client B | vs stateless model B | negotiated |")
        print("|---|---|---|---|---|---|---|")
        for name, gen in CORPORA.items():
            msgs = [gen(i) for i in range(500)]
            for comp in ("deflate", None):
                counts = {"up": 0, "down": 0}
                proxy = await counting_proxy(8795, 8794, counts)
                async with websockets.connect("ws://127.0.0.1:8795/echo", compression=comp) as ws:
                    ok = True
                    for m in msgs:
                        await ws.send(m)
                        ok &= (await ws.recv()) == m
                    neg = ws.response.headers.get("Sec-WebSocket-Extensions", "none")
                proxy.close(); await proxy.wait_closed()
                down = counts["down"]
                model_b = sum(len(deflate_stateless(m.encode())) + 2 for m in msgs) if comp else sum(len(m.encode()) + 2 for m in msgs)
                print(f"| {name} | {comp or 'off'} | {len(msgs)} | {ok} | {down} | {model_b} | {neg} |")
    finally:
        server.kill(); server.wait()


if __name__ == "__main__":
    model()
    if len(sys.argv) > 1:
        asyncio.run(live(sys.argv[1]))
