#!/usr/bin/env python3
"""Independent HTTP/1.1 load client for the service-latency benchmark (#312).

Deliberately NOT written in Tinox: a self-consistent Tinox client against a
Tinox server would share bugs with it. Pure Python stdlib, raw keep-alive
sockets via asyncio, one process per --procs, --conns connections in total.

Two pacing modes:
  closed loop (default)  each connection sends its next request as soon as the
                         previous response arrived. Measures service time under
                         a fixed concurrency; suffers coordinated omission
                         (a stall hides the requests that "would have" been sent).
  open loop (--rate R)   (a request that is already older than --timeout when its
                         turn comes is counted as `expired`, not sent)
                         requests are scheduled at R req/s total on a fixed
                         per-connection timetable and latency is measured from
                         the INTENDED send time, so a stall (e.g. a GC pause)
                         is charged to every request queued behind it.

Output: one JSON object on stdout with the raw per-request latencies (ms,
rounded to 3 decimals) and counters, plus derived percentiles.
"""
import argparse, asyncio, json, multiprocessing as mp, random, re, sys, time


def pct(sorted_vals, p):
    if not sorted_vals:
        return None
    k = min(len(sorted_vals) - 1, max(0, int(round(p / 100.0 * len(sorted_vals) + 0.5)) - 1))
    return sorted_vals[k]


async def read_response(reader):
    head = await reader.readuntil(b"\r\n\r\n")
    status = int(head.split(b" ", 2)[1])
    clen = 0
    for line in head.split(b"\r\n")[1:]:
        if line[:15].lower() == b"content-length:":
            clen = int(line[15:])
    if clen:
        await reader.readexactly(clen)
    return status


def build_req(args):
    # "{rand:N}" in --path is replaced per request with a random int in [0, N).
    m = re.search(r"\{rand:(\d+)\}", args.path)
    if not m:
        fixed = (f"{args.method} {args.path} HTTP/1.1\r\nHost: {args.host}\r\n"
                 f"Content-Length: 0\r\n\r\n").encode()
        return lambda: fixed
    n = int(m.group(1))
    pre, post = args.path[:m.start()], args.path[m.end():]
    return lambda: (f"{args.method} {pre}{random.randrange(n)}{post} HTTP/1.1\r\n"
                    f"Host: {args.host}\r\nContent-Length: 0\r\n\r\n").encode()


async def conn_worker(args, req, t_start, t_end, interval, lat, counters):
    reader = writer = None
    next_t = t_start
    while True:
        now = time.monotonic()
        if interval:
            if next_t >= t_end:
                break
            if now < next_t:
                await asyncio.sleep(next_t - now)
            intended = next_t
            next_t += interval
            if time.monotonic() - intended > args.timeout:
                # Client-side deadline: this request is already older than the
                # per-request timeout before it could even be sent (the service
                # fell behind the offered load). Count it as failed instead of
                # sending it late; otherwise an overloaded run keeps draining its
                # backlog long after the measured window and reports nonsense.
                counters["expired"] += 1
                continue
        else:
            if now >= t_end:
                break
            intended = now
        try:
            if writer is None:
                reader, writer = await asyncio.wait_for(
                    asyncio.open_connection(args.host, args.port), args.timeout)
            writer.write(req())
            status = await asyncio.wait_for(read_response(reader), args.timeout)
            ms = (time.monotonic() - intended) * 1000.0
            if 200 <= status < 300:
                lat.append(round(ms, 3))
            else:
                counters["http_error"] += 1
        except asyncio.TimeoutError:
            counters["timeout"] += 1
            writer = None
        except (OSError, asyncio.IncompleteReadError, ConnectionError):
            counters["conn_error"] += 1
            writer = None
    if writer is not None:
        writer.close()


async def run_proc(args, conns, phase_s, interval):
    req = build_req(args)
    lat, counters = [], {"http_error": 0, "timeout": 0, "conn_error": 0, "expired": 0}
    t0 = time.monotonic() + 0.2
    await asyncio.gather(*[
        conn_worker(args, req, t0 + (i * interval / max(conns, 1) if interval else 0),
                    t0 + phase_s, interval, lat, counters)
        for i in range(conns)])
    return lat, counters


def proc_main(args, conns, phase_s, interval, q):
    lat, counters = asyncio.run(run_proc(args, conns, phase_s, interval))
    q.put((lat, counters))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--path", required=True, help="may contain {rand:N}")
    ap.add_argument("--method", default="GET")
    ap.add_argument("--conns", type=int, default=16, help="total connections")
    ap.add_argument("--procs", type=int, default=2, help="client processes")
    ap.add_argument("--duration", type=float, default=20.0, help="seconds")
    ap.add_argument("--rate", type=float, default=0.0, help="total req/s (0 = closed loop)")
    ap.add_argument("--timeout", type=float, default=5.0, help="per-request timeout, s")
    ap.add_argument("--no-raw", action="store_true", help="omit raw latencies from output")
    args = ap.parse_args()

    procs = max(1, min(args.procs, args.conns))
    per = [args.conns // procs + (1 if i < args.conns % procs else 0) for i in range(procs)]
    q = mp.Queue()
    ps = []
    for n in per:
        interval = (n / (args.rate / procs)) if args.rate else 0.0  # per-connection period
        # rate is split evenly across procs, then across that proc's connections
        p = mp.Process(target=proc_main, args=(args, n, args.duration, interval, q))
        p.start()
        ps.append(p)
    lat, counters = [], {"http_error": 0, "timeout": 0, "conn_error": 0, "expired": 0}
    for _ in ps:
        l, c = q.get()
        lat.extend(l)
        for k, v in c.items():
            counters[k] += v
    for p in ps:
        p.join()
    lat.sort()
    out = {
        "path": args.path, "method": args.method, "conns": args.conns, "procs": procs,
        "duration_s": args.duration, "target_rate": args.rate or None,
        "mode": "open" if args.rate else "closed",
        "ok": len(lat), "rps": round(len(lat) / args.duration, 1),
        "offered": len(lat) + sum(counters.values()), **counters,
        "p50_ms": pct(lat, 50), "p95_ms": pct(lat, 95), "p99_ms": pct(lat, 99),
        "p999_ms": pct(lat, 99.9), "max_ms": lat[-1] if lat else None,
    }
    if not args.no_raw:
        out["latencies_ms"] = lat
    json.dump(out, sys.stdout)
    print()


if __name__ == "__main__":
    main()
