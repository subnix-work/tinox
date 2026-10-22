#!/usr/bin/env python3
"""Print a Markdown table summarizing every run_bench.py result JSON in a directory.

  summarize.py <results-dir>                 table on stdout
  summarize.py <results-dir> --strip <out>   additionally write copies of every result with the
                                             per-request latency arrays removed (they are tens of MB;
                                             everything else -- percentiles, counters, RSS/heap
                                             samples, metadata, command lines -- is kept)
"""
import glob, json, os, sys

rows = []
for path in sorted(glob.glob(os.path.join(sys.argv[1], "*.json"))):
    r = json.load(open(path))
    if "--strip" in sys.argv:
        out = sys.argv[sys.argv.index("--strip") + 1]
        os.makedirs(out, exist_ok=True)
        slim = json.loads(json.dumps(r))
        for ph in ("cold", "steady"):
            for e in slim.get(ph, []):
                e.pop("latencies_ms", None)
        json.dump(slim, open(os.path.join(out, os.path.basename(path)), "w"), indent=1)
    for s in r["steady"]:
        gc = r.get("gc_log") or {}
        rows.append((os.path.basename(path)[:-5], f"{s['method']} {s['path']}", s["rps"],
                     s["p50_ms"], s["p95_ms"], s["p99_ms"], s["p999_ms"], s["max_ms"],
                     sum(s.get(k, 0) for k in ("http_error", "timeout", "conn_error", "expired")), round((r["rss_peak_kib"] or 0) / 1024),
                     r["steady_threads_max"], r["startup_to_ready_ms"],
                     gc.get("collections", "-"), gc.get("collection_max_ms", "-")))
print("| run | route | req/s | p50 ms | p95 | p99 | p99.9 | max | failed | peak RSS MiB | threads | ready ms | GCs | max GC ms |")
print("|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|")
for row in rows:
    print("| " + " | ".join(str(c) for c in row) + " |")
