#!/usr/bin/env python3
"""Compare warm-runtime builds with disabled/cold/warm application reuse and verify each executable."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compiler", type=Path, default=Path("target/release/tinox"))
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--debug", action="store_true")
    parser.add_argument("--methods", type=int, default=0, help="Add synthetic arithmetic methods to measure backend scale")
    args = parser.parse_args()
    if args.runs < 1 or args.methods < 0:
        parser.error("--runs must be positive and --methods must be nonnegative")
    compiler = args.compiler.resolve(strict=True)
    results = {
        "platform": platform.platform(),
        "machine": platform.machine(),
        "compiler": str(compiler),
        "compiler_sha256": hashlib.sha256(compiler.read_bytes()).hexdigest(),
        "cc": subprocess.check_output(["cc", "--version"], text=True).splitlines()[0],
        "synthetic_methods": args.methods,
        "application_optimization": "debug" if args.debug else "release",
        "environment": {k: os.environ.get(k) for k in ("TINOX_CFLAGS", "TINOX_TLS", "TINOX_HTTP3")},
        "samples": {},
    }
    with tempfile.TemporaryDirectory(prefix="tinox-application-benchmark-") as temporary:
        root = Path(temporary)
        methods = []
        for index in range(args.methods):
            body = "var value: Int64 = n; " + "value = value * 31 + 7; " * 30
            methods.append(f"fnc method{index}(n: Int64) -> Int64 {{ {body} return value; }}")
        source = 'class Main { fnc main() -> Int32 { println("42"); return 0; } '
        source += " ".join(methods) + " }\n"
        (root / "Main.tnx").write_text(source)
        env = os.environ.copy()
        env["TINOX_RUNTIME_CACHE_DIR"] = str(root / "cache")
        env["TINOX_CACHE_TRACE"] = "1"
        env["TINOX_RUNTIME_CACHE"] = "1"
        env["TINOX_BUILD_CACHE"] = "0"
        # Warm the C runtime once, excluding its compilation from the baseline.
        warmup = [str(compiler), "build", "Main.tnx", "-o", "app"]
        if args.debug:
            warmup.append("--debug")
        subprocess.run(warmup, cwd=root, env=env, capture_output=True, check=True)
        for mode in ("disabled", "cold", "warm"):
            env["TINOX_BUILD_CACHE"] = "0" if mode == "disabled" else "1"
            samples = []
            for _ in range(1 if mode == "cold" else args.runs):
                command = [str(compiler), "build", "Main.tnx", "-o", "app"]
                if args.debug:
                    command.append("--debug")
                start = time.perf_counter()
                build = subprocess.run(command, cwd=root, env=env, capture_output=True, text=True, check=True)
                elapsed = time.perf_counter() - start
                if mode == "warm" and "[build-cache] hit" not in build.stderr:
                    raise RuntimeError(f"Expected application reuse: {build.stderr}")
                run = subprocess.run([str(root / "app")], cwd=root, env=env, capture_output=True, text=True, check=True)
                if run.stdout != "42\n":
                    raise RuntimeError(f"Unexpected program output: {run.stdout!r}")
                samples.append({"seconds": elapsed, "trace": build.stderr.strip()})
            results["samples"][mode] = samples
        results["median_seconds"] = {
            mode: statistics.median(sample["seconds"] for sample in samples)
            for mode, samples in results["samples"].items()
        }
    print(json.dumps(results, indent=2))


if __name__ == "__main__":
    main()
