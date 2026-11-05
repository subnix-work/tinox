#!/usr/bin/env python3
"""Warm compiler feedback on densely connected projects (#559).

The existing frontend benchmark uses independent leaves (every file imports only Main's list). This one generates
topologies where files depend on each other and measures, through the real CLI with TINOX_TIMINGS/TINOX_CACHE_TRACE,
what an edit costs:

  chain    module i imports module i+1 (deep dependency chain)
  dense    module i imports the next DEGREE modules (heavily connected DAG)
  generic  dense, with every module exchanging generic Box<T>/Pair<T, U> values in its signatures

Scenarios per topology and command (`check`, `build`): clean (all caches off), cold, warm (nothing changed),
leaf-edit (method body of the module nothing depends on... except Main), body-edit of the most popular module
(signature unchanged) and signature-edit of the most popular module (a new parameter). Each sample records wall
time, the per-stage timings and the frontend cache hits/misses.
"""
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

TOPOLOGIES = ('chain', 'dense', 'generic')


def module_source(topology, index, count, degree, signature=None, body=0):
    """Source of module `index`. `signature` (an int) adds a uniquely named extra parameter to the most
    popular module and makes its direct callers pass it; `body` changes only a literal in a method body."""
    name = f'M{index}'
    deps = ([index + 1] if index + 1 < count else []) if topology == 'chain' else list(range(index + 1, min(count, index + 1 + degree)))
    imports = ''.join(f'import M{d}\n' for d in deps)
    popular = index == count - 1  # every other module depends on it, directly or transitively
    param = f', extra{signature}: Int64' if popular and signature is not None else ''
    bump = 1 + (body if popular else 0)
    pass_extra = ', 0' if deps and deps[0] == count - 1 and signature is not None else ''
    code = 'var v: Int64 = n\n' + 'v = v + 1\n' * 40
    if topology == 'generic':
        pair = (f'fnc pair(n: Int64) -> Pair<Int64, Box<Int64>> {{\nreturn Pair<Int64, Box<Int64>> '
                f'{{ first: n, second: Box<Int64> {{ value: n }} }}\n}}\n')
        call = f'v = v + M{deps[0]}::make(n{pass_extra}).value\n' if deps else ''
        return (imports + f'import Box\nimport Pair\nclass {name} {{\nfnc make(n: Int64{param}) -> Box<Int64> {{\n{code}{call}'
                f'return Box<Int64> {{ value: v + {bump} }}\n}}\n{pair}}}\n')
    call = f'v = v + M{deps[0]}::get(n{pass_extra})\n' if deps else ''
    return imports + f'class {name} {{\nfnc get(n: Int64{param}) -> Int64 {{\n{code}{call}return v + {bump}\n}}\n}}\n'


BOX = 'class Box<T> {\n    var value: T\n}\n'
PAIR = 'class Pair<T, U> {\n    var first: T\n    var second: U\n}\n'


def write_project(project, topology, count, degree, signature=None, body=0):
    project.mkdir(parents=True, exist_ok=True)
    (project / 'tinox.toml').write_text('[package]\ncompiler = "2.9.1"\nname = "connected-benchmark"\nversion = "0.1.0"\n')
    if topology == 'generic':
        (project / 'Box.tnx').write_text(BOX)
        (project / 'Pair.tnx').write_text(PAIR)
    for index in range(count):
        (project / f'M{index}.tnx').write_text(module_source(topology, index, count, degree, signature, body))
    first = '.value' if topology == 'generic' else ''
    (project / 'Main.tnx').write_text('import M0\nclass Main {\nfnc main() -> Int32 {\n'
                                      + f'println(M0::{"make" if topology == "generic" else "get"}(1){first}.toString())\nreturn 0\n}}\n}}\n')


def parse(stderr):
    stages, queries = {}, {}
    for line in stderr.splitlines():
        timing = re.fullmatch(r'\[(?:frontend-)?timing\] (\S+) ([0-9.]+) ms', line)
        if timing:
            stage = stages.setdefault(timing[1], {'calls': 0, 'total_ms': 0.0, 'max_ms': 0.0})
            stage['calls'] += 1
            stage['total_ms'] += float(timing[2])
            stage['max_ms'] = max(stage['max_ms'], float(timing[2]))
        query = re.match(r'\[(?:frontend|build|runtime)-cache\] (\S+) (\S+)', line)
        if query:
            key = f'{query[1]}:{query[2]}'
            queries[key] = queries.get(key, 0) + 1
    return stages, queries


def run(compiler, project, env, command):
    args = [str(compiler), command, 'Main.tnx'] + (['-o', 'app'] if command == 'build' else [])
    start = time.perf_counter()
    result = subprocess.run(args, cwd=project, env=env, capture_output=True, text=True)
    elapsed = time.perf_counter() - start
    if result.returncode:
        raise RuntimeError(f'{" ".join(args)} failed in {project}:\n{result.stderr[-2000:]}')
    stages, queries = parse(result.stderr)
    return {'seconds': elapsed, 'stages': stages, 'queries': queries}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, default=Path('target/release/tinox'))
    parser.add_argument('--topologies', nargs='+', choices=TOPOLOGIES, default=list(TOPOLOGIES))
    parser.add_argument('--modules', type=int, default=150)
    parser.add_argument('--degree', type=int, default=8)
    parser.add_argument('--runs', type=int, default=5)
    parser.add_argument('--commands', nargs='+', choices=('check', 'build'), default=['check', 'build'])
    args = parser.parse_args()
    if args.modules < 2 or args.degree < 1 or args.runs < 1:
        parser.error('modules must be >= 2, degree and runs >= 1')
    compiler = args.compiler.resolve(strict=True)
    results = {'platform': platform.platform(), 'machine': platform.machine(), 'compiler': str(compiler),
               'compiler_sha256': hashlib.sha256(compiler.read_bytes()).hexdigest(), 'modules': args.modules,
               'degree': args.degree, 'runs': args.runs, 'cc': subprocess.check_output(['cc', '--version'], text=True).splitlines()[0],
               'samples': []}
    with tempfile.TemporaryDirectory(prefix='tinox-connected-benchmark-') as temporary:
        root = Path(temporary)
        for topology in args.topologies:
            for command in args.commands:
                project = root / f'{topology}-{command}'
                write_project(project, topology, args.modules, args.degree)
                env = dict(os.environ, TINOX_TIMINGS='1', TINOX_CACHE_TRACE='1',
                           TINOX_FRONTEND_CACHE_DIR=str(project / 'fcache'), TINOX_RUNTIME_CACHE_DIR=str(root / 'rcache'),
                           TINOX_BUILD_CACHE='1', TINOX_FRONTEND_CACHE='1', TINOX_RUNTIME_CACHE='1')
                run(compiler, project, env, 'build')  # warm the C runtime once; excluded from every sample
                scenarios = {}
                clean = dict(env, TINOX_FRONTEND_CACHE='0', TINOX_BUILD_CACHE='0')
                scenarios['clean'] = [run(compiler, project, clean, command) for _ in range(args.runs)]
                write_project(project, topology, args.modules, args.degree)
                scenarios['cold'] = []
                subprocess.run(['rm', '-rf', str(project / 'fcache')])
                env['TINOX_BUILD_CACHE'] = '1'
                scenarios['cold'].append(run(compiler, project, dict(env, TINOX_BUILD_CACHE='0'), command))
                scenarios['warm'] = [run(compiler, project, env, command) for _ in range(args.runs)]
                popular = project / f'M{args.modules - 1}.tnx'
                leaf = project / 'M0.tnx'
                for scenario in ('leaf-edit', 'popular-body-edit', 'popular-signature-edit'):
                    samples = []
                    for run_index in range(args.runs):
                        # Every run writes content no earlier run had, so each sample is a real invalidation.
                        n = run_index + 1
                        if scenario == 'leaf-edit':
                            text = leaf.read_text()
                            leaf.write_text(re.sub(r'((?:return|value:) v \+ )\d+', lambda m: m.group(1) + str(n + 100), text, count=1))
                        elif scenario == 'popular-body-edit':
                            popular.write_text(module_source(topology, args.modules - 1, args.modules, args.degree, None, n))
                        else:
                            # Callers of the popular module pass the new parameter, so every direct caller changes too.
                            for index in range(args.modules):
                                (project / f'M{index}.tnx').write_text(module_source(topology, index, args.modules, args.degree, n, 0))
                        samples.append(run(compiler, project, env, command))
                    scenarios[scenario] = samples
                for scenario, samples in scenarios.items():
                    results['samples'].append({'topology': topology, 'command': command, 'scenario': scenario,
                                               'median_seconds': statistics.median(s['seconds'] for s in samples), 'runs': samples})
    print(json.dumps(results, indent=2))


if __name__ == '__main__':
    main()
