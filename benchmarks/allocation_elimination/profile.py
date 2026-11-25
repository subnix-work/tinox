"""Compare the two serializers with the same compiler, dependencies and inputs."""
import argparse
import json
import os
import platform
import re
import shutil
import socket
import statistics
import subprocess
import tempfile
import time
from pathlib import Path
from urllib.request import urlopen

HERE = Path(__file__).resolve().parent
SERVICE = HERE.parent / "service_latency"
SWITCH = "TINOX_EXPERIMENT_LOCAL_JSON_BUILDER"


def item(i):
    return dict(id=i, name=f"item-{i}", tags=f"alpha,beta,gamma-{i % 17}",
                score=round(0.5 + i / 1000, 3))


def build(compiler, project, mode, enabled, port):
    env = dict(os.environ, TINOX_PORT=str(port), **{SWITCH: str(int(enabled))})
    exe = project / ("app-local" if enabled else "app-baseline")
    started = time.perf_counter()
    subprocess.run([compiler, "build", "--force", *mode, "Main.tnx", "-o", exe],
                   cwd=project, env=env, check=True, stdout=subprocess.DEVNULL)
    seconds = time.perf_counter() - started
    ir = exe.with_suffix(".ll").read_text()
    for cls, local in [("Item", enabled), ("Report", False)]:
        body = re.search(rf'define i8\* @{cls}_toJson\([^\n]+\n(.*?)^}}', ir, re.S | re.M)
        assert body and ("call i8* @tinox_json_with_local_builder" in body[1]) == local
        assert ("call i8* @jsonBuilderCreate" in body[1]) != local
    return exe, seconds


def focused(exe):
    start = time.perf_counter()
    output = subprocess.check_output([exe], text=True, timeout=30).splitlines()
    elapsed = time.perf_counter() - start
    total, allocated, used, saved, report = output
    assert int(total) == 100000 * len(json.dumps(item(42), separators=(",", ":")))
    assert json.loads(saved) == dict(item(42), name="a" * 1024 + '"')
    assert json.loads(report) == dict(count=1, items=[dict(item(42), name="changed")])
    return dict(process_s=elapsed, allocated_bytes=int(allocated), post_gc_used_estimate=int(used))


def service(exe, port):
    def get(path):
        with urlopen(f"http://127.0.0.1:{port}/{path}", timeout=5) as response:
            return response.read()

    expected = dict(count=200, items=[item(i) for i in range(200)])
    with subprocess.Popen([exe], stdout=subprocess.DEVNULL,
                          env=dict(os.environ, TINOX_HTTP_WORKERS="2")) as proc:
        try:
            deadline = time.monotonic() + 15
            while True:
                if proc.poll() is not None:
                    raise RuntimeError(f"service exited with {proc.returncode}")
                try:
                    get("stats")
                    break
                except OSError:
                    if time.monotonic() >= deadline:
                        raise
                    time.sleep(0.05)
            for _ in range(10):
                assert json.loads(get("report/200")) == expected
            before = json.loads(get("stats"))
            latencies = []
            for _ in range(100):
                start = time.perf_counter_ns()
                payload = get("report/200")
                latencies.append((time.perf_counter_ns() - start) / 1e6)
                assert json.loads(payload) == expected
            after = json.loads(get("stats"))
            rss = re.search(r"VmRSS:\s+(\d+) kB", Path(f"/proc/{proc.pid}/status").read_text())
            return dict(allocated_bytes=after["allocatedBytes"] - before["allocatedBytes"],
                        reserved_heap_bytes=after["id"], rss_kib=int(rss[1]),
                        median_ms=statistics.median(latencies), latency_ms=latencies)
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("compiler", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    compiler = args.compiler.resolve()
    if not __debug__:
        raise SystemExit("Run without -O so correctness checks are enabled")
    cpu = re.search(r"^model name\s*:\s*(.+)$", Path("/proc/cpuinfo").read_text(), re.M)
    results = dict(kernel=platform.release(), machine=platform.machine(),
                   cpu=cpu[1] if cpu else platform.machine(), available_cpus=len(os.sched_getaffinity(0)),
                   compiler=subprocess.check_output([compiler, "--version"], text=True).strip(),
                   base=subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=HERE, text=True).strip(),
                   builds=[], runs=[])
    with tempfile.TemporaryDirectory() as tmp:
        for workload in ("focused", "service"):
            project = Path(tmp) / workload
            project.mkdir()
            for name in ("tinox.toml", "tinox.lock", "Item.tnx", "Report.tnx", "Main.tnx", "BenchController.tnx", "Stats.tnx"):
                shutil.copy(SERVICE / name, project)
            if workload == "focused":
                shutil.copy(HERE / "Main.tnx", project)
            subprocess.run([compiler, "install"], cwd=project, check=True)
            with socket.socket() as sock:
                sock.bind(("127.0.0.1", 0))
                port = sock.getsockname()[1]
            modes = {"release": ["--release"]}
            if workload == "focused":
                modes.update(debug=["--debug"], checked=["--debug", "--checked"])
            for name, flags in modes.items():
                # Warm the runtime object cache. --force still compiles the application.
                build(compiler, project, flags, False, port)
                for trial in range(4):
                    for enabled in ((True, False) if trial % 2 else (False, True)):
                        exe, seconds = build(compiler, project, flags, enabled, port)
                        results["builds"].append(dict(workload=workload, mode=name, local=enabled,
                                                      trial=trial, seconds=seconds))
                        sample = focused(exe) if workload == "focused" else service(exe, port)
                        results["runs"].append(dict(workload=workload, mode=name, local=enabled, trial=trial, **sample))
    args.output.write_text(json.dumps(results, indent=2) + "\n")


if __name__ == "__main__":
    main()
