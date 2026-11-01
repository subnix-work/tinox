#!/usr/bin/env python3
"""Bundle immutable raw measurement inputs and render a scope-aware current report."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import statistics


def read_json(path):
    return json.loads(path.read_text())


def number(value):
    return 'n/a' if value is None else f'{value:.2f}'


def summarize(records):
    values = {'p99': [], 'rss': [], 'allocation_rate': [], 'db_active': []}
    offered = failed = recovered = recovery_offered = 0
    verified = True
    operational_failures = []
    for record in records:
        verified = verified and record.get('memory_limit_enforced', False)
        if 'failure' in record:
            operational_failures.append(record['failure'])
        samples = record.get('samples', [])
        rss = [s['VmHWM'] / 1024 for s in samples if s.get('VmHWM') is not None]
        if rss:
            values['rss'].append(max(rss))
        runtime = [(s['monotonic'], s['runtime']['allocatedBytes']) for s in samples
                   if s.get('runtime', {}).get('allocatedBytes') is not None]
        if len(runtime) >= 2 and runtime[-1][0] > runtime[0][0]:
            values['allocation_rate'].append((runtime[-1][1] - runtime[0][1]) / (runtime[-1][0] - runtime[0][0]) / 1048576)
        values['db_active'].extend(s['database']['active_sleep_queries'] for s in samples if 'database' in s)
        for phase in record.get('phases', []):
            if phase['phase'] == 'steady':
                offered += phase['offered']
                failed += phase['offered'] - phase['ok']
                if phase['p99_ms'] is not None:
                    values['p99'].append(phase['p99_ms'])
            if phase['phase'] == 'recovery':
                recovered += phase['ok']
                recovery_offered += phase['offered']
    p99 = values['p99']
    latency = 'n/a' if not p99 else f'{statistics.mean(p99):.2f} [{min(p99):.2f}, {max(p99):.2f}]'
    return {'runs': len(records), 'p99': latency,
            'peak_rss_mib': max(values['rss'], default=None),
            'allocation_mib_s': statistics.mean(values['allocation_rate']) if values['allocation_rate'] else None,
            'steady_offered': offered, 'steady_failed': failed,
            'recovery_ok': recovered, 'recovery_offered': recovery_offered,
            'limits_verified': verified, 'failures': operational_failures,
            'db_active_max': max(values['db_active'], default=None)}


parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--suite', action='append', required=True, metavar='LABEL=PATH')
parser.add_argument('--exploratory', action='append', default=[], metavar='LABEL=PATH')
parser.add_argument('--out', type=Path, required=True)
parser.add_argument('--report', type=Path, required=True)
args = parser.parse_args()
args.out.mkdir(parents=True, exist_ok=False)
manifest, groups = {}, {}
for exploratory, entries in [(False, args.suite), (True, args.exploratory)]:
    for entry in entries:
        label, location = entry.split('=', 1)
        root = Path(location)
        for source in sorted(root.rglob('*')):
            if not source.is_file() or source.suffix in ('.bin', '.ll'):
                continue
            relative = Path(label) / source.relative_to(root)
            target = args.out / (str(relative) + '.gz')
            target.parent.mkdir(parents=True, exist_ok=True)
            raw = source.read_bytes()
            target.write_bytes(gzip.compress(raw, mtime=0))
            manifest[str(target.relative_to(args.out))] = {
                'raw_sha256': hashlib.sha256(raw).hexdigest(), 'raw_bytes': len(raw),
                'compressed_sha256': hashlib.sha256(target.read_bytes()).hexdigest(),
                'exploratory': exploratory}
        if not exploratory:
            for result in sorted(root.glob('*/result.json')):
                record = read_json(result)
                groups.setdefault((label, record['profile']), []).append(record)
summary = {f'{label}/{profile}': summarize(records) for (label, profile), records in groups.items()}
(args.out / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
(args.out / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
lines = [
    '# Current service measurements (#312)', '',
    'Measured 2026-11-01 on the current compiler 2.8.0 checkout. Every primary run used',
    'a fresh local Docker image/process with a verified 2-CPU cgroup quota and 512 MiB',
    'memory limit (64 MiB for the deliberate OOM case), with swap bounded to the same',
    'total. Effective ancestor limits and Docker configuration are retained together.',
    'The desktop machine was shared with background applications and other checks;',
    'neither machine isolation nor universal latency/memory guarantees are claimed.', '',
    'p99 is conditional on successful responses and includes open-loop scheduling delay.',
    'Offered failures include HTTP errors, connection errors, timeouts and client-expired',
    'requests; they must not be interpreted as server-side rejection counts. The table',
    'shows the mean and range of per-run p99 values, with all failed runs retained.', '',
    '| Suite / profile | Runs | Steady p99 mean [range], ms | Peak RSS, MiB | Sample-window GC allocation, MiB/s | Steady failed/offered | Recovery ok/offered | Limits verified |',
    '| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |']
for name, row in summary.items():
    lines.append(f"| {name} | {row['runs']} | {row['p99']} | {number(row['peak_rss_mib'])} | {number(row['allocation_mib_s'])} | {row['steady_failed']}/{row['steady_offered']} | {row['recovery_ok']}/{row['recovery_offered']} | {row['limits_verified']} |")
lines += ['', '## Evidence and interpretation', '',
    '- Raw per-request latencies, samples, process/thread/fd counts, admission queue counters,',
    '  cgroup memory/CPU events, startup readiness, overload, recovery and exit/OOM state',
    '  are retained as individually compressed files under `results/2026-11-01/`.',
    '  `manifest.json` hashes both raw and compressed bytes. Native curl cross-checks',
    '  validate the service response independently of the Python load driver.',
    '- The frozen matrix has three repetitions per workload; the large live-set follow-up',
    '  holds one million entries through a 600-second steady phase. The separate',
    '  dependency matrix repeats TLS, a real HTTP upstream delayed by 50 ms, and a real',
    '  PostgreSQL pool of two connections (eight HTTP workers). Independent pg_stat_activity',
    '  samples retain observed active database queries; they are not a general DB queue gauge.',
    '- Admission queue counters are the runtime counters returned by tinox_limits_json,',
    '  not every internal worker/backend queue. Sampling can be delayed when the HTTP',
    '  observation endpoint itself saturates; absent samples are not zero usage.',
    '- GC allocation rates cover the sampled window, including cold/warm/steady/overload/',
    '  recovery traffic and observation overhead. They exclude allocations inside native',
    '  libraries. Prefill and exact heap/counter snapshots remain in the raw artifacts.',
    '- Compact payload copy/allocation profiling and the narrow gzip optimization are',
    '  measured separately in `../compact_bytes/results/2026-11-01.json`: five rotated-order',
    '  before/after runs, independent decompression and per-process RSS. Copy boundaries',
    '  are source-inspected; no hardware copy counter or zero-copy guarantee is inferred.',
    '- The 64-MiB case intentionally fails during large-live-set prefill. Docker OOMKilled',
    '  and exit 137 are retained. An OOM is not presented as a successful load result.',
    '- Exploratory earlier artifacts remain marked separately. Earlier TLS results used',
    '  a client that incorrectly reused Connection: close sockets; those error rates are',
    '  invalid as service evidence. Some earlier daemon checks encountered a full root',
    '  filesystem. They are not silently replaced with successful results.',
    '- Boehm defaults remain unchanged. These results do not establish that GC tuning,',
    '  serialization rewrites or a different collector would improve the service.', '']
for name, row in summary.items():
    if row['failures']:
        lines.append(f"Recorded failure in {name}: " + '; '.join(row['failures']) + '.')
    if row['recovery_offered'] > 0 and row['recovery_ok'] == 0:
        lines.append(f"{name} returned no successful responses in its configured 10-second recovery windows at reduced offered load. This bounds the observation; eventual recovery time was not measured and is not inferred to be infinite. Blocking-route isolation and outbound deadline/pooling work remain tracked by #552, #525 and #526.")
    if row['db_active_max'] is not None:
        lines.append(f"Observed maximum active PostgreSQL sleep queries in {name}: {row['db_active_max']}.")
args.report.write_text('\n'.join(lines) + '\n')
