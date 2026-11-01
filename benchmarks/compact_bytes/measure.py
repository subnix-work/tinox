#!/usr/bin/env python3
"""Independent process-RSS/allocation and byte-exact gzip comparisons (#311)."""
import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import platform
import random
import statistics
import subprocess
import sys
import tempfile
import time

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--tinox', type=Path, default=REPO / 'target/release/tinox')
parser.add_argument('--out', type=Path, required=True)
parser.add_argument('--mib', type=int, default=16)
parser.add_argument('--repeats', type=int, default=5)
args = parser.parse_args()
args.out.mkdir(parents=True, exist_ok=False)
with tempfile.TemporaryDirectory(prefix='tinox-bytes-measure-') as directory:
    work = Path(directory)
    payload = random.Random(311).randbytes(args.mib * 1048576)
    (work / 'input').write_bytes(payload)
    binary = work / 'probe'
    subprocess.run([args.tinox, 'build', HERE / 'Main.tnx', '-o', binary], check=True, timeout=120)
    records = []
    modes = ['read-list', 'read-buffer', 'gzip-list', 'gzip-buffer']
    # Rotate order so a mode does not always inherit the same warm-cache position.
    for repeat in range(args.repeats):
        for mode in modes[repeat % len(modes):] + modes[:repeat % len(modes)]:
            env = dict(os.environ, TINOX_BYTES_MODE=mode, TINOX_BYTES_INPUT=str(work / 'input'),
                       TINOX_BYTES_OUTPUT=str(work / 'output'))
            start = time.monotonic()
            result = subprocess.run([sys.executable, '-c',
                                     "import resource,subprocess,sys;subprocess.run([sys.argv[1]],check=True);print('peak_rss_kib='+str(resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss))", str(binary)],
                                    env=env, capture_output=True, text=True, timeout=60, check=True)
            values = {key: int(value) for key, value in (line.split('=') for line in result.stdout.splitlines())}
            assert values['length'] == len(payload) and values['last'] == payload[-1]
            if mode.startswith('gzip'):
                assert gzip.decompress((work / 'output').read_bytes()) == payload
            records.append({'mode': mode, 'repeat': repeat + 1, 'wall_seconds': time.monotonic() - start,
                            **values})
    summary = {mode: {metric: {'mean': statistics.mean(values), 'stdev': statistics.stdev(values),
                              'min': min(values), 'max': max(values)}
                      for metric in ('allocated', 'peak_rss_kib', 'wall_seconds')
                      for values in [[r[metric] for r in records if r['mode'] == mode]]} for mode in modes}
    meta = {'records': records, 'summary': summary, 'args': vars(args) | {'out': str(args.out), 'tinox': str(args.tinox)},
            'payload_sha256': hashlib.sha256(payload).hexdigest(), 'payload_seed': 311,
            'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(), 'platform': platform.platform(),
            'compiler': subprocess.check_output([args.tinox, '--version'], text=True).strip(),
            'commit': subprocess.check_output(['git', '-C', REPO, 'rev-parse', 'HEAD'], text=True).strip(),
            'dirty': bool(subprocess.check_output(['git', '-C', REPO, 'status', '--porcelain'])),
            'copy_contract': {'read-list': 'one GC-managed scratch payload plus widening to int64 slots',
                              'read-buffer': 'fread directly into compact GC payload',
                              'gzip-list': 'widen input, narrow into zlib scratch, widen output, narrow to Bytes',
                              'gzip-buffer': 'zlib reads compact input directly and writes compact output; growth can copy'},
            'limitations': ['shared machine', 'warm file cache', 'wall time includes process startup',
                            'GC allocation counter includes redirected runtime malloc but excludes allocations inside native libraries such as zlib',
                            'copy contracts are source-inspected, not hardware copy counters', 'no shared views or zero-copy claim']}
    (args.out / 'results.json').write_text(json.dumps(meta, indent=2))
    print(json.dumps(summary, indent=2))
