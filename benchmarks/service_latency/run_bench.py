#!/usr/bin/env python3
"""Orchestrates one service-latency measurement run (#312).

Builds the benchmark service, starts it (optionally pinned / wrapped / with
Boehm env tuning), measures startup-to-readiness, then drives it with the
independent load client (loadgen.py) through a cold phase, a warm-up and a
fixed-duration steady-state phase while sampling RSS / thread count / Boehm
heap from outside. Everything (raw latencies, samples, machine metadata,
exact command lines) is written to one JSON file so a result can be
re-derived and audited later.

Example:
  python3 benchmarks/service_latency/run_bench.py --profile report --duration 30 \
      --conns 32 --cpus 0-3 --out /tmp/svc_bench/report_baseline.json
"""
import argparse, json, os, platform, re, shlex, signal, subprocess, sys, threading, time, urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))

# profile -> (prefill request or None, [(method, path template)] driven concurrently)
PROFILES = {
    "small": (None, [("GET", "/small")]),
    "report": (None, [("GET", "/report/{n}")]),
    "cache": (("POST", "/cache/fill/{cache_items}"),
              [("GET", "/cache/{rand:{cache_items}}"), ("POST", "/cache/{rand:{cache_items}}/churn")]),
}


def sh(cmd):
    try:
        return subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=20).stdout.strip()
    except Exception:
        return ""


def metadata(args, tinox):
    cpu = sh("grep -m1 'model name' /proc/cpuinfo | cut -d: -f2").strip()
    mem_kb = sh("grep MemTotal /proc/meminfo | awk '{print $2}'")
    return {
        "date_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "git_commit": sh(f"git -C {shlex.quote(REPO)} rev-parse --short HEAD"),
        "git_dirty": bool(sh(f"git -C {shlex.quote(REPO)} status --porcelain")),
        "tinox_version": sh(f"{shlex.quote(tinox)} --version").splitlines()[0],
        "libgc": sh("pacman -Q gc 2>/dev/null") or sh("dpkg -s libgc1 2>/dev/null | grep ^Version"),
        "cpu_model": cpu, "logical_cores": os.cpu_count(),
        "mem_total_mib": int(mem_kb) // 1024 if mem_kb else None,
        "kernel": platform.release(), "python": platform.python_version(),
        "service_cpus": args.cpus or "unpinned", "client_cpus": args.client_cpus or "unpinned",
        "wrapper": args.wrapper or None,
        "memory_limit_mib": args.mem_limit_mib,
        "memory_limit_enforced": bool(args.wrapper) and args.mem_limit_mib is not None,
        "env_tuning": dict(kv.split("=", 1) for kv in args.env),
    }


def parse_gc_log(path):
    """Summarize Boehm GC_PRINT_STATS output (whole process lifetime, incl. warm-up).
    Present only when the run set --env GC_PRINT_STATS=1; that env var adds log I/O to
    every collection, so latency numbers from such runs carry that observer effect."""
    n, allocd, tot_ns, max_ns = 0, 0, 0, 0
    for line in open(path, errors="replace"):
        m = re.match(r"--> Marking for collection #\d+ after (\d+) allocated bytes", line)
        if m:
            allocd += int(m.group(1))
        m = re.match(r"Complete collection took (\d+) ms (\d+) ns", line)
        if m:
            ns = int(m.group(1)) * 1_000_000 + int(m.group(2))
            n, tot_ns, max_ns = n + 1, tot_ns + ns, max(max_ns, ns)
    if n == 0:
        return None
    return {"collections": n, "bytes_allocated_since_gc_total": allocd,
            "collection_total_ms": round(tot_ns / 1e6, 2), "collection_max_ms": round(max_ns / 1e6, 3),
            "collection_avg_ms": round(tot_ns / n / 1e6, 3)}


def http(port, method, path, timeout=30):
    req = urllib.request.Request(f"http://127.0.0.1:{port}{path}", method=method,
                                 data=b"" if method == "POST" else None)
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return r.read()


def proc_status(pid):
    out = {}
    try:
        for line in open(f"/proc/{pid}/status"):
            k, _, v = line.partition(":")
            if k in ("VmRSS", "VmHWM", "Threads"):
                out[k] = int(v.split()[0])
    except OSError:
        pass
    return out


class Sampler(threading.Thread):
    def __init__(self, pid, port, period=0.5):
        super().__init__(daemon=True)
        self.pid, self.port, self.period = pid, port, period
        self.samples, self.stop = [], threading.Event()

    def run(self):
        t0 = time.monotonic()
        while not self.stop.wait(self.period):
            s = {"t": round(time.monotonic() - t0, 2), **proc_status(self.pid)}
            try:
                s["boehm_heap_bytes"] = json.loads(http(self.port, "GET", "/stats", 2))["id"]
            except Exception:
                s["boehm_heap_bytes"] = None
            self.samples.append(s)


def loadgen(args, specs, duration, rate):
    """Run one loadgen per spec concurrently; returns list of result dicts."""
    procs = []
    for method, path in specs:
        cmd = (["taskset", "-c", args.client_cpus] if args.client_cpus else []) + [sys.executable, "-I", os.path.join(HERE, "loadgen.py"), "--port", str(args.port),
               "--method", method, "--path", path, "--conns", str(max(1, args.conns // len(specs))),
               "--procs", str(args.client_procs), "--duration", str(duration)]
        if rate:
            cmd += ["--rate", str(rate / len(specs))]
        procs.append((cmd, subprocess.Popen(cmd, stdout=subprocess.PIPE, text=True)))
    res = []
    for cmd, p in procs:
        out, _ = p.communicate()
        r = json.loads(out)
        r["cmd"] = shlex.join(cmd)
        res.append(r)
    return res


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    ap.add_argument("--profile", choices=sorted(PROFILES), required=True)
    ap.add_argument("--tinox", default=os.path.join(REPO, "target", "release", "tinox"))
    ap.add_argument("--port", type=int, default=18312)
    ap.add_argument("--duration", type=float, default=30.0, help="steady-state seconds")
    ap.add_argument("--warmup", type=float, default=10.0)
    ap.add_argument("--cold", type=float, default=5.0, help="first-seconds-after-ready phase, reported separately")
    ap.add_argument("--conns", type=int, default=32)
    ap.add_argument("--client-procs", type=int, default=4)
    ap.add_argument("--rate", type=float, default=0.0, help="total req/s, open loop (0 = closed loop)")
    ap.add_argument("--n", type=int, default=200, help="items per /report response")
    ap.add_argument("--cache-items", type=int, default=200000, help="live entries for the cache profile")
    ap.add_argument("--cpus", help="taskset cpu list for the service, e.g. 0-3")
    ap.add_argument("--client-cpus", help="taskset cpu list for the load clients (keep disjoint from --cpus)")
    ap.add_argument("--wrapper", help="command prefix that must EXEC the service, e.g. a docker/cgroup launcher; "
                    "its pid is sampled, so it has to be the service process")
    ap.add_argument("--mem-limit-mib", type=int, help="documented limit the wrapper enforces (metadata only)")
    ap.add_argument("--env", action="append", default=[], metavar="K=V", help="env var for the service (Boehm tuning)")
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    exe = args.out + ".bin"
    env = dict(os.environ, TINOX_PORT=str(args.port))
    t = time.monotonic()
    b = subprocess.run([args.tinox, "build", "Main.tnx", "-o", os.path.abspath(exe)], cwd=HERE, env=env,
                       capture_output=True, text=True)
    if b.returncode != 0:
        sys.exit("build failed:\n" + b.stdout + b.stderr)
    build_s = time.monotonic() - t

    prefill, specs = PROFILES[args.profile]
    subst = lambda s: s.replace("{n}", str(args.n)).replace("{cache_items}", str(args.cache_items))
    specs = [(m, subst(p)) for m, p in specs]

    cmd = ([] if not args.wrapper else shlex.split(args.wrapper)) + \
          (["taskset", "-c", args.cpus] if args.cpus else []) + [os.path.abspath(exe)]
    svc_env = dict(os.environ, **dict(kv.split("=", 1) for kv in args.env))
    logf = open(args.out + ".service.log", "w")
    t_start = time.monotonic()
    svc = subprocess.Popen(cmd, env=svc_env, stdout=logf, stderr=subprocess.STDOUT)
    try:
        ready_ms = None
        while time.monotonic() - t_start < 30:
            if svc.poll() is not None:
                sys.exit(f"service exited early (rc={svc.returncode}), see {args.out}.service.log")
            try:
                http(args.port, "GET", "/stats", 1)
                ready_ms = (time.monotonic() - t_start) * 1000
                break
            except Exception:
                time.sleep(0.005)
        if ready_ms is None:
            sys.exit("service never became ready")
        rss_at_ready = proc_status(svc.pid)

        t = time.monotonic()
        if prefill:
            http(args.port, prefill[0], subst(prefill[1]), 300)
        prefill_s = time.monotonic() - t
        rss_after_prefill = proc_status(svc.pid)

        result = {"meta": metadata(args, args.tinox), "profile": args.profile, "args": vars(args),
                  "service_cmd": shlex.join(cmd), "build_s": round(build_s, 1),
                  "startup_to_ready_ms": round(ready_ms, 1), "prefill_s": round(prefill_s, 2),
                  "rss_at_ready": rss_at_ready, "rss_after_prefill": rss_after_prefill}
        if args.cold > 0:
            result["cold"] = loadgen(args, specs, args.cold, args.rate)
        if args.warmup > 0:
            loadgen(args, specs, args.warmup, args.rate)
        sampler = Sampler(svc.pid, args.port)
        sampler.start()
        result["steady"] = loadgen(args, specs, args.duration, args.rate)
        sampler.stop.set()
        sampler.join()
        result["samples"] = sampler.samples
        result["service_alive_at_end"] = svc.poll() is None
        result["rss_peak_kib"] = proc_status(svc.pid).get("VmHWM")
        result["steady_rss_kib_max"] = max((s.get("VmRSS", 0) for s in sampler.samples), default=None)
        result["steady_threads_max"] = max((s.get("Threads", 0) for s in sampler.samples), default=None)
    finally:
        if svc.poll() is None:
            svc.send_signal(signal.SIGTERM)
            try:
                svc.wait(5)
            except subprocess.TimeoutExpired:
                svc.kill()
        logf.close()
        for leftover in (exe, exe + ".ll"):
            if os.path.exists(leftover):
                os.remove(leftover)
    result["gc_log"] = parse_gc_log(args.out + ".service.log")
    json.dump(result, open(args.out, "w"))
    for phase in ("cold", "steady"):
        for r in result.get(phase, []):
            print(f"{phase:6} {r['method']:4} {r['path']:32} ok={r['ok']} rps={r['rps']} "
                  f"offered={r['offered']} err={r['http_error']+r['timeout']+r['conn_error']+r['expired']} p50={r['p50_ms']} p95={r['p95_ms']} "
                  f"p99={r['p99_ms']} p99.9={r['p999_ms']} max={r['max_ms']} ms")
    print(f"ready={result['startup_to_ready_ms']}ms rss_at_ready={rss_at_ready.get('VmRSS')}KiB "
          f"gc={result['gc_log']} peak_rss={result['rss_peak_kib']}KiB threads_max={result['steady_threads_max']} "
          f"alive={result['service_alive_at_end']}")


if __name__ == "__main__":
    main()
