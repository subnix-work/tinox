"""Measure the existing service controller and the two smallest GC pause workloads."""
import argparse
import concurrent.futures
import http.client
import json
import math
import os
import platform
import shutil
import socket
import subprocess
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
BENCH = HERE.parent
ADDRESS_SPACE_LIMIT = 1024**3


def command(binary, cores):
    return ["prlimit", f"--as={ADDRESS_SPACE_LIMIT}", "--", "taskset", "-c", cores, binary]


def build(compiler, root, name, gc, port):
    exe = root / f"{name}-{gc}"
    subprocess.run([compiler, "build", "Main.tnx", "--release", "-o", exe], cwd=root, check=True,
                   env=dict(os.environ, TINOX_GC=gc, TINOX_PORT=str(port)))
    libs = subprocess.check_output(["ldd", exe], text=True)
    if ("libgc.so" in libs) != (gc == "boehm"):
        raise ValueError(libs)
    return exe


def cpu_seconds(pid):
    ticks = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return (int(ticks[11]) + int(ticks[12])) / os.sysconf("SC_CLK_TCK")


def request(port, path):
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=20)
    try:
        conn.request("GET", path)
        response = conn.getresponse()
        if response.status != 200:
            raise RuntimeError(f"{path}: HTTP {response.status}")
        return response.read()
    finally:
        conn.close()


def service(binary, port, cores):
    env = dict(os.environ, TINOX_HTTP_WORKERS="2")
    with subprocess.Popen(command(binary, cores), env=env, stdout=subprocess.DEVNULL) as proc:
        try:
            deadline = time.monotonic() + 15
            while True:
                if proc.poll() is not None:
                    raise RuntimeError(f"service exited {proc.returncode}")
                try:
                    request(port, "/stats")
                    break
                except ConnectionRefusedError:
                    if time.monotonic() > deadline:
                        raise
                    time.sleep(0.02)
            expected = dict(count=200, items=[dict(id=i, name=f"item-{i}",
                            tags=f"alpha,beta,gamma-{i % 17}", score=round(.5+i/1000, 3)) for i in range(200)])

            def client(count):
                times = []
                for _ in range(count):
                    start = time.perf_counter_ns()
                    body = request(port, "/report/200")
                    times.append((time.perf_counter_ns()-start)/1e6)
                    if json.loads(body) != expected:
                        raise ValueError("incorrect report")
                return times

            client(30)
            before = json.loads(request(port, "/stats"))
            cpu = cpu_seconds(proc.pid)
            start = time.perf_counter()
            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                batches = list(pool.map(client, [500, 500]))
            elapsed = time.perf_counter()-start
            cpu = cpu_seconds(proc.pid)-cpu
            after = json.loads(request(port, "/stats"))
            peak = next(line for line in Path(f"/proc/{proc.pid}/status").read_text().splitlines()
                        if line.startswith("VmHWM:"))
            times = [ms for batch in batches for ms in batch]
            return dict(seconds=elapsed, requests_per_s=len(times)/elapsed,
                        p99_ms=sorted(times)[math.ceil(.99*len(times))-1], latency_ms=batches,
                        cpu_seconds=cpu, peak_rss_kib=int(peak.split()[1]),
                        allocated_delta=after["allocatedBytes"]-before["allocatedBytes"])
        finally:
            proc.terminate()
            try:
                proc.wait(5)
            except subprocess.TimeoutExpired:
                proc.kill()


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("compiler", type=Path)
    ap.add_argument("output", type=Path)
    args = ap.parse_args()
    compiler = args.compiler.resolve()
    cores = ",".join(map(str, sorted(os.sched_getaffinity(0))[:2]))
    results = dict(base=subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
                   kernel=platform.release(), machine=platform.machine(), cores=cores, address_space_limit=ADDRESS_SPACE_LIMIT,
                   compiler=subprocess.check_output([compiler, "--version"], text=True).strip(), runs=[])
    with tempfile.TemporaryDirectory() as temp:
        root = Path(temp)
        for name in ("pause", "service"):
            if name == "pause":
                source = (BENCH / "bench_gc_pause/Main.tnx").read_text()
                lines = [line for line in source.splitlines()
                         if "benchAtSize(" not in line or "fnc benchAtSize" in line or
                         line.strip() in ("benchAtSize(10000)", "benchAtSize(50000)")]
                (root / "Main.tnx").write_text("\n".join(lines))
            else:
                for p in (BENCH / "service_latency").iterdir():
                    if p.suffix in (".tnx", ".lock", ".toml"):
                        shutil.copy(p, root)
                subprocess.run([compiler, "install"], cwd=root, check=True)
            with socket.socket() as sock:
                sock.bind(("127.0.0.1", 0))
                port = sock.getsockname()[1]
            binaries = {gc: build(compiler, root, name, gc, port) for gc in ("boehm", "tinox")}
            for trial in range(4):
                for gc in (list(reversed(binaries)) if trial % 2 else binaries):
                    if name == "pause":
                        output = subprocess.check_output(command(binaries[gc], cores), text=True, timeout=60)
                        if not all(f"live_objects={n} (checked {n})" in output for n in (10000, 50000)):
                            raise ValueError(output)
                        sample = dict(stdout=output.splitlines())
                    else:
                        sample = service(binaries[gc], port, cores)
                    results["runs"].append(dict(workload=name, collector=gc, trial=trial, **sample))
                    args.output.write_text(json.dumps(results, indent=2)+"\n")


if __name__ == "__main__":
    main()
