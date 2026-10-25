#!/usr/bin/env python3
"""Measure frontend scale for clean, warm, and leaf-edit checks through the real CLI."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import statistics
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, default=Path('target/release/tinox'))
    parser.add_argument('--sizes', type=int, nargs='+', default=[20, 100, 300])
    parser.add_argument('--runs', type=int, default=3)
    parser.add_argument('--baseline', action='store_true', help='Allow compilers without frontend queries')
    args = parser.parse_args()
    if args.runs < 1 or any(size < 1 for size in args.sizes):
        parser.error('runs and sizes must be positive')
    compiler = args.compiler.resolve(strict=True)
    results = {'platform': platform.platform(), 'compiler_sha256': hashlib.sha256(compiler.read_bytes()).hexdigest(), 'samples': []}
    with tempfile.TemporaryDirectory(prefix='tinox-frontend-benchmark-') as temporary:
        root = Path(temporary)
        env = dict(os.environ, TINOX_FRONTEND_CACHE_DIR=str(root / 'cache'), TINOX_TIMINGS='1', TINOX_CACHE_TRACE='1')
        for size in args.sizes:
            project = root / str(size)
            project.mkdir()
            env['TINOX_FRONTEND_CACHE_DIR'] = str(project / 'cache')
            (project / 'tinox.toml').write_text('[package]\ncompiler = \"2.5.1\"\nname="frontend-benchmark"\nversion="0.1.0"\n')
            imports = '\n'.join(f'import Leaf{i}' for i in range(size))
            (project / 'Main.tnx').write_text(imports + '\nclass Main {\nfnc main() -> Int32 {\nprintln(Leaf0::get(1).toString())\nreturn 0\n}\n}\n')
            def leaf(value):
                return 'class Leaf0 {\nfnc get(n: Int64) -> Int64 {\nvar v: Int64 = n\n' + 'v = v + 1\n' * 100 + f'return v + {value}\n}}\n}}\n'
            for index in range(size):
                (project / f'Leaf{index}.tnx').write_text(leaf(1).replace('Leaf0', f'Leaf{index}'))
            for mode in ('disabled', 'cold', 'warm', 'leaf-edit'):
                env['TINOX_FRONTEND_CACHE'] = '0' if mode == 'disabled' else '1'
                samples = []
                for run in range(1 if mode == 'cold' else args.runs):
                    if mode == 'leaf-edit':
                        (project / 'Leaf0.tnx').write_text(leaf(run + 2))
                    start = time.perf_counter()
                    result = subprocess.run([str(compiler), 'check', 'Main.tnx'], cwd=project, env=env, capture_output=True, text=True, check=True)
                    elapsed = time.perf_counter() - start
                    stages = {}
                    queries = {}
                    for line in result.stderr.splitlines():
                        timing = re.fullmatch(r'\[frontend-timing\] (\S+) ([0-9.]+) ms', line)
                        if timing:
                            stage = stages.setdefault(timing[1], {'calls': 0, 'total_ms': 0.0, 'max_ms': 0.0})
                            stage['calls'] += 1
                            stage['total_ms'] += float(timing[2])
                            stage['max_ms'] = max(stage['max_ms'], float(timing[2]))
                        query = re.match(r'\[frontend-cache\] (\S+) (\S+)', line)
                        if query:
                            key = query[1] + ':' + query[2]
                            queries[key] = queries.get(key, 0) + 1
                    if not args.baseline:
                        if mode == 'warm' and (queries.get('parse:miss', 0) or queries.get('program:hit') != 1):
                            raise RuntimeError(f'Expected warm query reuse: {queries}')
                        if mode == 'leaf-edit' and (queries.get('parse:miss') != 1 or queries.get('visibility:hit', 0) != size - 1 or queries.get('visibility:miss') != 2):
                            raise RuntimeError(f'Unexpected leaf invalidation: {queries}')
                    samples.append({'seconds': elapsed, 'queries': queries, 'stages': stages})
                results['samples'].append({'files': size + 1, 'mode': mode, 'median_seconds': statistics.median(s['seconds'] for s in samples), 'runs': samples})
    print(json.dumps(results, indent=2))


if __name__ == '__main__':
    main()
