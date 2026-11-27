#!/usr/bin/env python3
"""Run the gc_stress workloads under both collectors and report medians.

Usage: run.py [--tinox target/release/tinox] [--runs 5] [--cpus 0-7] [--out results.json]
Each (workload, collector) is measured in fresh processes, alternating collector order per run.
Wall time, mutator stalls and the GC count come from the program; CPU seconds and peak RSS are
read from the kernel (wait4 rusage) so they include collector threads and allocator metadata.
"""
import argparse, json, os, re, statistics, subprocess, sys, tempfile, time
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
# name -> (workload, scale, threads)
CASES = {
    "churn": ("churn", 20_000_000, 1),
    "trees": ("trees", 400, 1),
    "liveheap-2M": ("liveheap", 10_000_000, 1),
    "mapchurn": ("mapchurn", 10_000_000, 1),
    "bigblocks": ("bigblocks", 200_000, 1),
    "fragment": ("fragment", 200_000, 1),
    "threads-1": ("threads", 20_000_000, 1),
    "threads-2": ("threads", 20_000_000, 2),
    "threads-4": ("threads", 20_000_000, 4),
    "threads-8": ("threads", 20_000_000, 8),
    "threads-16": ("threads", 20_000_000, 16),
}


def build(tinox, gc, out):
    env = dict(os.environ, TINOX_GC=gc)
    subprocess.run([tinox, "build", str(HERE / "Main.tnx"), "--release", "-o", str(out)], check=True,
                   env=env, stdout=subprocess.DEVNULL)
    libs = subprocess.check_output(["ldd", str(out)], text=True)
    if ("libgc.so" in libs) != (gc == "boehm"):
        sys.exit(f"{gc}: wrong collector linked\n{libs}")


def run_once(exe, case, cpus, timeout):
    workload, scale, threads = CASES[case]
    env = dict(os.environ, WORKLOAD=workload, SCALE=str(scale), THREADS=str(threads))
    cmd = ["taskset", "-c", cpus, str(exe)] if cpus else [str(exe)]
    t = time.monotonic()
    proc = subprocess.Popen(cmd, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        out, err = proc.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.communicate()
        return {"error": "timeout"}
    _, status, ru = os.wait4(proc.pid, 0) if False else (None, proc.returncode, None)
    elapsed = time.monotonic() - t
    if proc.returncode != 0:
        return {"error": f"exit {proc.returncode}: {err[-300:]}"}
    fields = dict(re.findall(r"(\w+)=(\S+)", out.strip().splitlines()[-1]))
    return {"elapsed_s": elapsed, **{k: int(v) for k, v in fields.items() if v.lstrip('-').isdigit()}}


def rusage_run(exe, case, cpus, timeout):
    """Like run_once but gets CPU and peak RSS from RUSAGE_CHILDREN deltas via a tiny wrapper."""
    workload, scale, threads = CASES[case]
    env = dict(os.environ, WORKLOAD=workload, SCALE=str(scale), THREADS=str(threads))
    cmd = (["taskset", "-c", cpus] if cpus else []) + [str(exe)]
    wrapper = ("import os,sys,json,resource,subprocess;"
               "p=subprocess.run(sys.argv[1:],capture_output=True,text=True);"
               "r=resource.getrusage(resource.RUSAGE_CHILDREN);"
               "print(json.dumps({'rc':p.returncode,'out':p.stdout,'err':p.stderr[-300:],"
               "'cpu':r.ru_utime+r.ru_stime,'rss_kib':r.ru_maxrss}))")
    try:
        p = subprocess.run([sys.executable, "-c", wrapper] + cmd, env=env, capture_output=True,
                           text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return {"error": "timeout"}
    r = json.loads(p.stdout)
    if r["rc"] != 0:
        return {"error": f"exit {r['rc']}: {r['err']}"}
    fields = dict(re.findall(r"(\w+)=(\S+)", r["out"].strip().splitlines()[-1]))
    return {"cpu_s": round(r["cpu"], 2), "rss_mib": round(r["rss_kib"] / 1024, 1),
            **{k: int(v) for k, v in fields.items() if v.lstrip('-').isdigit()}}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tinox", default=str(REPO / "target/release/tinox"))
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--cpus", default="0-15", help="taskset list for the workload, '' = no pinning")
    ap.add_argument("--cases", nargs="*", default=list(CASES))
    ap.add_argument("--scale", type=float, default=1.0, help="multiply workload sizes (smoke runs)")
    ap.add_argument("--timeout", type=int, default=900)
    ap.add_argument("--out", default="/tmp/gc_stress.json")
    a = ap.parse_args()
    for k, (w, s, t) in list(CASES.items()):
        CASES[k] = (w, max(1, int(s * a.scale)), t)
    tmp = Path(tempfile.mkdtemp(prefix="gcstress-"))
    exes = {}
    for gc in ("tinox", "boehm"):
        exes[gc] = tmp / f"stress-{gc}"
        build(a.tinox, gc, exes[gc])
    results = {c: {"tinox": [], "boehm": []} for c in a.cases}
    for run in range(a.runs):
        order = ["tinox", "boehm"] if run % 2 == 0 else ["boehm", "tinox"]
        for case in a.cases:
            checks = {}
            for gc in order:
                r = rusage_run(exes[gc], case, a.cpus, a.timeout)
                results[case][gc].append(r)
                checks[gc] = r.get("check")
                print(f"run {run} {case:12} {gc:6} {r}", flush=True)
            if checks["tinox"] != checks["boehm"]:
                print(f"!! CHECKSUM MISMATCH {case}: {checks}", flush=True)
    Path(a.out).write_text(json.dumps({"cpus": a.cpus, "runs": a.runs, "results": results}, indent=1))
    print(table(results))


def med(rs, k):
    v = [r[k] for r in rs if k in r]
    return statistics.median(v) if v else None


def table(results):
    rows = ["| Workload | Metric | Tinox | Boehm | Tinox / Boehm |", "|---|---|---:|---:|---:|"]
    for case, per in results.items():
        for key, label in [("wall_ms", "wall ms"), ("cpu_s", "CPU s"), ("rss_mib", "peak RSS MiB"),
                           ("lat_p99_us", "p99 stall µs"), ("lat_max_us", "max stall µs"),
                           ("collections", "collections")]:
            t, b = med(per["tinox"], key), med(per["boehm"], key)
            if t is None or b is None:
                continue
            ratio = f"{t / b:.2f}" if b else "n/a"
            rows.append(f"| {case} | {label} | {t:g} | {b:g} | {ratio} |")
    return "\n".join(rows)


if __name__ == "__main__":
    main()
