#!/usr/bin/env python3
"""TLS, slow real HTTP upstream and real PostgreSQL pool-saturation profiles."""
import argparse
import http.server
import json
import os
from pathlib import Path
import socket
import shutil
import tempfile
import threading
import time

import container_suite as suite

class Upstream(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        time.sleep(.05)
        body = b'{"upstream":"ok"}'
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        try:
            self.wfile.write(body)
        except ConnectionError:
            pass
    def log_message(self, *args):
        pass

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--tinox', type=Path, default=suite.REPO / 'target/release/tinox')
parser.add_argument('--out', type=Path, required=True)
parser.add_argument('--repeats', type=int, default=3)
parser.add_argument('--duration', type=float, default=60)
parser.add_argument('--cpus', type=float, default=2)
parser.add_argument('--memory-mib', type=int, default=512)
parser.add_argument('--rate', type=float, default=20)
parser.add_argument('--overload-seconds', type=float, default=15)
parser.add_argument('--overload-rate', type=float, default=1000)
parser.add_argument('--recovery-seconds', type=float, default=10)
parser.add_argument('--profiles', nargs='+', choices=['tls', 'slow-upstream', 'db-pool'], default=['tls', 'slow-upstream', 'db-pool'])
args = parser.parse_args()
args.out.mkdir(parents=True, exist_ok=False)
args.loadgen = args.out / 'loadgen.snapshot.py'
shutil.copy2(suite.HERE / 'loadgen.py', args.loadgen)
args.cache_items = 0
upstream = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Upstream)
threading.Thread(target=upstream.serve_forever, daemon=True).start()
pg_name = f'tinox-service-312-db-{os.getpid()}'
pg_started = False
success = True
try:
    for profile in args.profiles:
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        args.service_env = [f'TINOX_BENCH_UPSTREAM=http://127.0.0.1:{upstream.server_port}/']
        url = None
        args.observe = None
        if profile == 'db-pool':
            suite.command(['docker', 'run', '--pull=never', '-d', '--rm', '--tmpfs', '/var/lib/postgresql/data:rw,size=256m', '--name', pg_name, '-P', '-e', 'POSTGRES_HOST_AUTH_METHOD=trust', 'postgres:17'])
            pg_started = True
            args.service_env.append('TINOX_HTTP_WORKERS=8')
            pg_port = int(suite.command(['docker', 'port', pg_name, '5432/tcp']).splitlines()[0].rsplit(':', 1)[1])
            url = f'postgres://postgres@127.0.0.1:{pg_port}/postgres'
            for attempt in range(100):
                try:
                    suite.command(['psql', '-XAt', url, '-c', 'SELECT 1'], timeout=3)
                    break
                except RuntimeError:
                    time.sleep(.1)
            else:
                raise RuntimeError('PostgreSQL readiness deadline')
            def observe():
                query = "SELECT json_build_object('active_sleep_queries',count(*) FILTER (WHERE state='active' AND query LIKE '%pg_sleep%'),'pool_backends',count(*) FILTER (WHERE query LIKE '%pg_sleep%')) FROM pg_stat_activity WHERE pid <> pg_backend_pid()"
                return json.loads(suite.command(['psql', '-XAt', url, '-c', query], timeout=3))
            args.observe = observe
        tag = f'tinox-dependency-measurement:run-{os.getpid()}-{profile}'
        with tempfile.TemporaryDirectory(prefix='tinox-dependency-image-') as work:
            build = suite.image(args.tinox.resolve(), Path(work), tag, port, profile, url)
        (args.out / f'{profile}-metadata.json').write_text(json.dumps({
            'build': build, 'profile': profile, 'pool_size': 2 if url else None,
            'upstream_delay_ms': 50 if profile == 'slow-upstream' else None,
            'postgres_image_id': suite.command(['docker', 'image', 'inspect', 'postgres:17', '--format', '{{.Id}}']) if url else None,
            'http_workers': 8 if url else 2,
            'machine_shared': True, 'tls_certificate': 'repository self-signed test fixture',
            'command': __import__('sys').argv, 'time_utc': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())}, indent=2))
        try:
            for repeat in range(1, args.repeats + 1):
                success = suite.run(args, tag, port, profile, repeat) and success
        finally:
            suite.command(['docker', 'image', 'rm', tag])
finally:
    upstream.shutdown()
    upstream.server_close()
    if pg_started:
        suite.command(['docker', 'rm', '-fv', pg_name])
raise SystemExit(0 if success else 1)
