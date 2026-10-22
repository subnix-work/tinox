#!/usr/bin/env python3
"""Service tail-latency / RSS benchmark driver (#312).

Builds benchmarks/service/app with a chosen TINOX_PORT, starts it, waits for
readiness, drives one workload profile with an independent (non-Tinox) load
client for a fixed duration, samples RSS / thread count / Boehm heap size once
per second, and writes raw results + machine metadata as JSON.

Standard library only. Run `bench_service.py --help`; see README.md.
"""
import argparse, http.client, json, os, platform, signal, socket, subprocess
import sys, threading, time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))

# profile -> (request path, default connections, description)
PROFILES = {
    "stateless_json": ("/hello", 16, "tiny fixed JSON response"),
    "serialize_heavy": ("/items/200", 16, "allocate + serialize 200 objects per request"),
    "long_lived_state": ("/hello", 16, "stateless reads on top of a pre-filled live set (--fill)"),
    "concurrent_conn": ("/hello", 256, "many simultaneous keep-alive connections"),
}


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


def read_proc(pid):
    rss_kb = threads = None
    try:
        with open(f"/proc/{pid}/status") as f:
            for line in f:
                if line.startswith("VmRSS:"):
                    rss_kb = int(line.split()[1])
                elif line.startswith("Threads:"):
                    threads = int(line.split()[1])
    except OSError:
        pass
    return rss_kb, threads


def get(port, path, timeout=5):
    c = http.client.HTTPConnection("127.0.0.1", port, timeout=timeout)
    try:
        c.request("GET", path)
        r = c.getresponse()
        return r.status, r.read()
    finally:
        c.close()


def percentile(sorted_vals, q):
    if not sorted_vals:
        return None
    i = min(len(sorted_vals) - 1, int(round(q * (len(sorted_vals) - 1))))
    return sorted_vals[i]


def load_worker(port, path, deadline, out, errors, expect_status):
    lat = []
    err = {"connect_or_io": 0, "bad_status": 0}
    conn = None
    while time.monotonic() < deadline:
        t0 = time.monotonic()
        try:
            if conn is None:
                conn = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
            conn.request("GET", path)
            r = conn.getresponse()
            r.read()
            lat.append(time.monotonic() - t0)
            if r.status != expect_status:
                err["bad_status"] += 1
        except Exception:
            err["connect_or_io"] += 1
            try:
                conn and conn.close()
            except Exception:
                pass
            conn = None
    out.append(lat)
    errors.append(err)


def machine_metadata(tinox):
    md = {
        "platform": platform.platform(),
        "python": sys.version.split()[0],
        "cpu_count": os.cpu_count(),
        "date_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    }
    try:
        for line in open("/proc/cpuinfo"):
            if line.startswith("model name"):
                md["cpu_model"] = line.split(":", 1)[1].strip()
                break
        for line in open("/proc/meminfo"):
            if line.startswith("MemTotal"):
                md["mem_total_kb"] = int(line.split()[1])
                break
    except OSError:
        pass
    for k, cmd in (("tinox_version", [tinox, "--version"]),
                   ("git_commit", ["git", "-C", REPO, "rev-parse", "--short", "HEAD"])):
        try:
            md[k] = subprocess.check_output(cmd, text=True, stderr=subprocess.STDOUT).strip()
        except Exception as e:
            md[k] = f"unavailable ({e})"
    return md


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("profile", choices=sorted(PROFILES))
    ap.add_argument("--tinox", default=os.path.join(REPO, "target/release/tinox"))
    ap.add_argument("--duration", type=float, default=30, help="steady-state seconds (default 30)")
    ap.add_argument("--warmup", type=float, default=5, help="discarded warm-up seconds (default 5)")
    ap.add_argument("--connections", type=int, help="override profile default")
    ap.add_argument("--fill", type=int, default=0,
                    help="long_lived_state: strings to pre-load into the cache (e.g. 1000000)")
    ap.add_argument("--limits", default="",
                    help="space-separated systemd-run properties, e.g. "
                         "'MemoryMax=512M CPUQuota=200%%' (needs systemd --user)")
    ap.add_argument("--out", help="result JSON path (default: stdout summary only)")
    a = ap.parse_args()

    path, default_conns, desc = PROFILES[a.profile]
    conns = a.connections or default_conns
    port = free_port()
    exe = os.path.join(HERE, f".bench_service_{os.getpid()}")
    env = dict(os.environ, TINOX_PORT=str(port))

    t_build = time.monotonic()
    subprocess.run([a.tinox, "install"], cwd=os.path.join(HERE, "app"), check=True, env=env)
    subprocess.run([a.tinox, "build", os.path.join(HERE, "app", "Main.tnx"), "-o", exe],
                   cwd=os.path.join(HERE, "app"), check=True, env=env)
    build_s = time.monotonic() - t_build

    cmd = [exe]
    if a.limits:
        cmd = ["systemd-run", "--user", "--scope", "--quiet"] + \
              [x for p in a.limits.split() for x in ("-p", p)] + ["--", exe]
    t_start = time.monotonic()
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    result = {"profile": a.profile, "description": desc, "path": path,
              "connections": conns, "duration_s": a.duration, "warmup_s": a.warmup,
              "fill": a.fill, "limits": a.limits or None, "port": port,
              "build_s": build_s}
    try:
        # Readiness: first successful response (cold start is reported separately).
        while True:
            if proc.poll() is not None:
                sys.exit(f"server exited early (rc={proc.returncode}): "
                         f"{proc.stderr.read().decode(errors='replace')}")
            try:
                if get(port, "/hello")[0] == 200:
                    break
            except Exception:
                pass
            if time.monotonic() - t_start > 30:
                sys.exit("server not ready after 30s")
            time.sleep(0.02)
        result["ready_s"] = time.monotonic() - t_start
        result["cold_first_requests"] = []
        for _ in range(5):
            t0 = time.monotonic()
            get(port, path)
            result["cold_first_requests"].append(time.monotonic() - t0)

        if a.profile == "long_lived_state" and a.fill:
            # Single connection on purpose: the controller's cache is unsynchronized.
            left = a.fill
            while left > 0:
                n = min(left, 50000)
                get(port, f"/fill/{n}", timeout=60)
                left -= n
            st, body = get(port, "/cache/size")
            result["cache_size_after_fill"] = int(body)

        # Sampler: RSS / threads / Boehm heap, once per second.
        samples, stop = [], threading.Event()

        def sampler():
            t0 = time.monotonic()
            while not stop.wait(1.0):
                rss, thr = read_proc(proc.pid)
                try:
                    heap = int(get(port, "/stats", timeout=2)[1])
                except Exception:
                    heap = None
                samples.append({"t": time.monotonic() - t0, "rss_kb": rss,
                                "threads": thr, "gc_heap_bytes": heap})
        th = threading.Thread(target=sampler, daemon=True)
        th.start()

        def run_phase(seconds):
            out, errs, ts = [], [], []
            deadline = time.monotonic() + seconds
            for _ in range(conns):
                t = threading.Thread(target=load_worker,
                                     args=(port, path, deadline, out, errs, 200))
                t.start()
                ts.append(t)
            for t in ts:
                t.join()
            return [x for l in out for x in l], errs

        if a.warmup > 0:
            run_phase(a.warmup)
        mark = len(samples)
        t0 = time.monotonic()
        lat, errs = run_phase(a.duration)
        elapsed = time.monotonic() - t0
        stop.set()
        th.join()

        lat.sort()
        ms = lambda v: None if v is None else round(v * 1000, 3)
        result["steady_state"] = {
            "requests": len(lat),
            "rps": round(len(lat) / elapsed, 1),
            "p50_ms": ms(percentile(lat, .50)), "p95_ms": ms(percentile(lat, .95)),
            "p99_ms": ms(percentile(lat, .99)), "max_ms": ms(lat[-1] if lat else None),
            "errors_io": sum(e["connect_or_io"] for e in errs),
            "errors_bad_status": sum(e["bad_status"] for e in errs),
        }
        ss = samples[mark:]
        rss = [s["rss_kb"] for s in ss if s["rss_kb"]]
        thr = [s["threads"] for s in ss if s["threads"]]
        heap = [s["gc_heap_bytes"] for s in ss if s["gc_heap_bytes"]]
        result["memory"] = {
            "rss_peak_mb": round(max(rss) / 1024, 1) if rss else None,
            "rss_final_mb": round(rss[-1] / 1024, 1) if rss else None,
            "threads_peak": max(thr) if thr else None,
            "gc_heap_peak_mb": round(max(heap) / 1048576, 1) if heap else None,
        }
        result["alive_at_end"] = proc.poll() is None
        result["samples"] = samples
        result["raw_latencies_s"] = lat
    finally:
        try:
            proc.send_signal(signal.SIGTERM)
            proc.wait(timeout=5)
        except Exception:
            proc.kill()
        if os.path.exists(exe):
            os.remove(exe)

    result["machine"] = machine_metadata(a.tinox)
    summary = {k: result[k] for k in ("profile", "connections", "limits", "ready_s",
                                      "steady_state", "memory", "alive_at_end")}
    print(json.dumps(summary, indent=2))
    if a.out:
        with open(a.out, "w") as f:
            json.dump(result, f)
        print(f"raw results: {a.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
