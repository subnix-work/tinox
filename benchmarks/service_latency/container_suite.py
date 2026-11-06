#!/usr/bin/env python3
"""Repeated current-build service measurements under verified Docker cgroup limits.

No provider/model calls. Builds a local scratch image containing the measured
executable and its exact host shared libraries; never pushes/publishes it.
Raw results include every latency, cgroup samples, allocation counters,
post-load recovery and Docker OOM/exit state. See README.md for limits.
"""
import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import socket
import signal
import ssl
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent


def command(argv, **kwargs):
    timeout = kwargs.pop('timeout', None)
    with subprocess.Popen([str(x) for x in argv], stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, text=True, start_new_session=True, **kwargs) as process:
        try:
            out, error = process.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.communicate()
            raise
        if process.returncode:
            raise RuntimeError(f'{argv}: {error[-4000:]}')
        return out.strip()


def get(port, path='/stats', method='GET', tls=False):
    with urllib.request.urlopen(urllib.request.Request(f'{"https://localhost" if tls else "http://127.0.0.1"}:{port}{path}', method=method), timeout=5,
                                context=ssl.create_default_context(cafile=str(REPO / 'tests/fixtures/tls/selfsigned_cert.pem')) if tls else None) as response:
        return json.loads(response.read())


def limits(pid):
    """Read the effective tightest v2 limits, including parent cgroups."""
    cgroup = next(line[3:] for line in Path(f'/proc/{pid}/cgroup').read_text().splitlines() if line.startswith('0::'))
    mount = Path('/sys/fs/cgroup')
    current = mount / cgroup.lstrip('/')
    values = {'cpu_cores': [], 'memory_bytes': [], 'paths': [], 'current': str(current)}
    while current.is_relative_to(mount):
        row = {'path': str(current)}
        for name in ('cpu.max', 'memory.max'):
            path = current / name
            if path.exists():
                row[name] = path.read_text().strip()
        if row.get('memory.max', 'max') != 'max':
            values['memory_bytes'].append(int(row['memory.max']))
        quota = row.get('cpu.max', 'max').split()
        if quota[0] != 'max':
            values['cpu_cores'].append(int(quota[0]) / int(quota[1]))
        values['paths'].append(row)
        if current == mount:
            break
        current = current.parent
    values['cpu_cores'] = min(values['cpu_cores'], default=None)
    values['memory_bytes'] = min(values['memory_bytes'], default=None)
    return values


def sample(pid, cgroup, port, tls=False, observe=None):
    result = {'monotonic': time.monotonic()}
    try:
        for line in Path(f'/proc/{pid}/status').read_text().splitlines():
            key, _, value = line.partition(':')
            if key in ('VmRSS', 'VmHWM', 'Threads'):
                result[key] = int(value.split()[0])
        try:
            result['fds'] = len(list(Path(f'/proc/{pid}/fd').iterdir()))
        except OSError as error:
            # A root-owned container may deny proc-fd inspection. Preserve
            # that limitation without skipping cgroup/runtime/DB samples.
            result['fd_sample_error'] = str(error)
        for name in ('memory.current', 'memory.peak', 'memory.events', 'cpu.stat', 'pids.current', 'memory.stat'):
            path = Path(cgroup) / name
            if path.exists():
                result[name] = path.read_text().strip()
        if observe is not None: result['database'] = observe()
        result['runtime'] = get(port, tls=tls)
    except (OSError, ValueError, TimeoutError, RuntimeError, subprocess.SubprocessError) as error:
        result['sample_error'] = str(error)
    return result


def load(port, path, seconds, rate, connections, raw, tls=False, client=None):
    argv = [sys.executable, client or HERE / 'loadgen.py', '--port', port, '--path', path,
            '--duration', seconds, '--rate', rate, '--conns', connections,
            '--procs', 2, '--timeout', 2]
    if tls:
        argv += ['--tls', '--ca-cert', REPO / 'tests/fixtures/tls/selfsigned_cert.pem']
    data = json.loads(command(argv, timeout=seconds + 30))
    raw.write_text(json.dumps(data))
    return {k: v for k, v in data.items() if k != 'latencies_ms'}


def image(tinox, work, tag, port, variant=None, database_url=None):
    project = work / 'project'
    shutil.copytree(HERE, project, ignore=shutil.ignore_patterns('results', '__pycache__', '.tinox'))
    for module in ('http_server', 'json', 'http', 'db'):
        target = project / f'.tinox/deps/tinox.core/{module}/checkout/tinox'
        shutil.copytree(REPO / f'crates/tinox-core-ext/{module}/tinox', target, dirs_exist_ok=True)
    (project / 'tinox.toml').write_text('[package]\ncompiler="2.9.2"\nname="service_latency"\nversion="0.1.0"\n')
    if database_url:
        with (project / 'tinox.toml').open('a') as manifest:
            manifest.write('[database]\ndriver="postgres"\nurl="' + database_url + '"\npool=2\n')
    if variant in ('slow-upstream', 'db-pool'):
        source = (project / 'BenchController.tnx').read_text()
        if variant == 'slow-upstream':
            source = 'import tinox.core.http\n' + source
            method = '@GET @Path("/slow") fn slow() -> String { return Http::get(envGet("TINOX_BENCH_UPSTREAM")).body() }'
        else:
            source = 'import tinox.core.db\n' + source
            method = '@GET @Path("/database") @Transactional fn database() -> String { DB::raw("SELECT pg_sleep(0.05)", [])\nreturn "ok" }'
        offset = source.rfind('}')
        source = source[:offset] + method + '\n' + source[offset:]
        (project / 'BenchController.tnx').write_text(source)
    if variant == 'tls':
        source = (project / 'BenchController.tnx').read_text()
        source = re.sub(r'^\s*@(GET|POST|Path)[^\n]*\n', '\n', source, flags=re.M)
        source = source.replace('@PathParam ', '').replace('@HttpContext ', '')
        (project / 'BenchController.tnx').write_text(source)
        (project / 'Main.tnx').write_text('''import BenchController
import tinox.core.http_server
import tinox.core.json
class Main {
 fnc main() -> Int32 {
  const controller = BenchController { cache: [] }
  const server = HttpServer::new(PORT)
  server.get("/small", ctx => { ctx.response.json(Json::serialize(controller.small())) })
  server.get("/stats", ctx => { ctx.response.json(Json::serialize(controller.stats())) })
  server.listenTls("/benchmark/cert.pem", "/benchmark/key.pem")
  return 0
 }
}
'''.replace('PORT', str(port)))
    binary = work / 'root/benchmark/service'
    binary.parent.mkdir(parents=True)
    command([tinox, 'build', 'Main.tnx', '-o', binary], cwd=project,
            env=dict(os.environ, TINOX_PORT=str(port)), timeout=120)
    if variant == 'tls':
        for source, name in [('selfsigned_cert.pem', 'cert.pem'), ('selfsigned_key.pem', 'key.pem')]:
            shutil.copy2(REPO / 'tests/fixtures/tls' / source, binary.parent / name)
    ldd = command(['ldd', binary])
    libraries = sorted(set(re.findall(r'(/[^\s()]+)', ldd)))
    for source in libraries:
        destination = work / 'root' / source.lstrip('/')
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, destination)
    (work / 'Dockerfile').write_text('FROM scratch\nCOPY root/ /\nENV LD_LIBRARY_PATH=/usr/lib\nENTRYPOINT ["/benchmark/service"]\n')
    command(['docker', 'build', '-q', '-t', tag, work], timeout=120)
    return {'image_id': command(['docker', 'image', 'inspect', tag, '--format', '{{.Id}}']),
            'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(), 'ldd': ldd,
            'compiler_sha256': hashlib.sha256(Path(tinox).read_bytes()).hexdigest(),
            'runtime_sha256': hashlib.sha256((REPO / 'runtime/runtime.c').read_bytes()).hexdigest(),
            'sources': {p.name: p.read_text() for p in sorted(project.glob('*.tnx'))},
            'manifest': (project / 'tinox.toml').read_text()}


def run(args, tag, port, profile, repeat):
    label = f'{profile}-r{repeat}'
    name = f'tinox-service-312-{os.getpid()}-{label}'
    directory = args.out / label
    directory.mkdir()
    start = time.monotonic()
    command(['docker', 'run', '-d', '--name', name, '--network', 'host',
             '--cpus', args.cpus, '--memory', f'{args.memory_mib}m', '--memory-swap', f'{args.memory_mib}m',
             '--pids-limit', 256, '--user', f'{os.getuid()}:{os.getgid()}', *[part for entry in getattr(args, 'service_env', []) for part in ('-e', entry)], '--read-only', '--security-opt', 'no-new-privileges', tag], timeout=20)
    tls = profile == 'tls'
    result = {'profile': profile, 'repeat': repeat, 'port': port, 'phases': []}
    stop = threading.Event()
    samples = []
    sampler = None
    try:
        inspect = json.loads(command(['docker', 'inspect', name]))[0]
        pid = inspect['State']['Pid']
        effective = limits(pid)
        result['limits'] = effective
        assert effective['cpu_cores'] is not None and effective['cpu_cores'] <= args.cpus
        assert effective['memory_bytes'] is not None and effective['memory_bytes'] <= args.memory_mib * 1048576
        result['memory_limit_enforced'] = True
        result['docker_host_config'] = {key: inspect['HostConfig'][key] for key in ('Memory', 'MemorySwap', 'NanoCpus', 'PidsLimit')}
        deadline = start + 30
        while True:
            try:
                result['initial_runtime'] = get(port, tls=tls)
                break
            except Exception:
                if time.monotonic() > deadline:
                    raise RuntimeError('readiness deadline')
                time.sleep(.01)
        result['startup_to_ready_ms'] = (time.monotonic() - start) * 1000
        curl = ['curl', '--fail', '--silent', '--show-error', '--max-time', '5']
        if tls: curl += ['--cacert', REPO / 'tests/fixtures/tls/selfsigned_cert.pem']
        curl += [f'{"https://localhost" if tls else "http://127.0.0.1"}:{port}/small']
        result['native_curl_crosscheck'] = json.loads(command(curl, timeout=8))
        assert result['native_curl_crosscheck']['id'] == 42
        if profile == 'large-live-set':
            result['prefill'] = get(port, f'/cache/fill/{args.cache_items}', 'POST')
        def sampling():
            while not stop.wait(.5):
                samples.append(sample(pid, effective['current'], port, tls, getattr(args, 'observe', None)))
        sampler = threading.Thread(target=sampling, daemon=True)
        sampler.start()
        path = '/report/200' if profile == 'serialization' else '/cache/{rand:' + str(args.cache_items) + '}' if profile == 'large-live-set' else '/small'
        if profile == 'slow-upstream': path = '/slow'
        if profile == 'db-pool': path = '/database'
        dependency = profile in ('slow-upstream', 'db-pool')
        for phase, duration, rate, connections in [
            ('cold', 2, 20 if dependency else 100, 4), ('warmup', 3, 20 if dependency else 1000, 16),
            ('steady', args.duration, args.rate, 32),
            ('overload', args.overload_seconds, args.overload_rate, 128),
            ('recovery', args.recovery_seconds, 5 if dependency else 100, 4)]:
            phase_start = time.monotonic()
            phase_result = load(port, path, duration, rate, connections, directory / f'{phase}.json', tls, getattr(args, 'loadgen', None))
            result['phases'].append({'phase': phase, 'started_monotonic': phase_start, 'finished_monotonic': time.monotonic(), **phase_result})
        try:
            result['final_runtime'] = get(port, tls=tls)
        except Exception as error:
            result['recovery_probe_error'] = str(error)
        result['final_sample'] = sample(pid, effective['current'], port, tls)
    except Exception as error:
        result['failure'] = str(error)
    finally:
        stop.set()
        if sampler:
            sampler.join(10)
        result['samples'] = samples
        inspect = json.loads(command(['docker', 'inspect', name]))[0]
        if inspect['State'].get('OOMKilled') and inspect['State'].get('Running'):
            try:
                command(['docker', 'wait', name], timeout=3)
                inspect = json.loads(command(['docker', 'inspect', name]))[0]
            except subprocess.TimeoutExpired:
                pass
        result['exit_state_before_cleanup'] = inspect['State']
        logs = subprocess.run(['docker', 'logs', name], capture_output=True, text=True, timeout=10)
        (directory / 'service.log').write_text(logs.stdout + logs.stderr)
        command(['docker', 'rm', '-f', name])
        (directory / 'result.json').write_text(json.dumps(result, indent=2))
    print(label, 'failed: ' + result['failure'] if 'failure' in result else 'complete', flush=True)
    return 'failure' not in result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--keep-image', action='store_true')
    parser.add_argument('--tinox', type=Path, default=REPO / 'target/release/tinox')
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--cpus', type=float, default=2)
    parser.add_argument('--memory-mib', type=int, default=512)
    parser.add_argument('--repeats', type=int, default=3)
    parser.add_argument('--duration', type=float, default=120)
    parser.add_argument('--rate', type=float, default=1000)
    parser.add_argument('--cache-items', type=int, default=1000000)
    parser.add_argument('--overload-seconds', type=float, default=15)
    parser.add_argument('--overload-rate', type=float, default=60000)
    parser.add_argument('--recovery-seconds', type=float, default=10)
    parser.add_argument('--profiles', nargs='+', choices=['small', 'serialization', 'large-live-set'], default=['small', 'serialization', 'large-live-set'])
    args = parser.parse_args()
    if args.out.exists():
        parser.error('output must be a new directory')
    args.out.mkdir(parents=True)
    args.loadgen = args.out / 'loadgen.snapshot.py'
    shutil.copy2(HERE / 'loadgen.py', args.loadgen)
    tag = f'tinox-service-measurement:run-{os.getpid()}'
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    with tempfile.TemporaryDirectory(prefix='tinox-service-image-') as directory:
        build = image(args.tinox.resolve(), Path(directory), tag, port)
    meta = {'build': build, 'args': {k: str(v) if isinstance(v, Path) else v for k, v in vars(args).items()},
            'git_commit': command(['git', '-C', REPO, 'rev-parse', 'HEAD']),
            'dirty_diff_sha256': hashlib.sha256(command(['git', '-C', REPO, 'diff']).encode()).hexdigest(),
            'compiler_version': command([args.tinox, '--version']), 'platform': platform.platform(),
            'machine_shared': True, 'gc_defaults_changed': False,
            'command': sys.argv, 'time_utc': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())}
    (args.out / 'metadata.json').write_text(json.dumps(meta, indent=2))
    success = True
    try:
        for repeat in range(1, args.repeats + 1):
            for profile in args.profiles:
                success = run(args, tag, port, profile, repeat) and success
    finally:
        if not args.keep_image:
            command(['docker', 'image', 'rm', tag])
    return 0 if success else 1


if __name__ == '__main__':
    sys.exit(main())
