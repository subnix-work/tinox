#!/usr/bin/env python3
"""Validate the new held-out checkers with known good/bad code; no model calls.

These are checker regression results, never AI model baselines or efficiency
measurements. Disposable PostgreSQL/RabbitMQ containers are removed by each
checker, including on assertion failure.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time

HERE = Path(__file__).resolve().parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--tinox', type=Path, default=HERE.parent.parent / 'target/release/tinox')
parser.add_argument('--out', type=Path, required=True)
parser.add_argument('--repeats', type=int, default=15)
args = parser.parse_args()
args.out.mkdir(parents=True, exist_ok=False)
image_ids = {name: subprocess.check_output(['docker','image','inspect',name,'--format','{{.Id}}'], text=True).strip()
             for name in ['postgres:17','rabbitmq:4-management']}
checker_env = dict(__import__('os').environ, TINOX_EVAL_POSTGRES_IMAGE=image_ids['postgres:17'],
                   TINOX_EVAL_RABBITMQ_IMAGE=image_ids['rabbitmq:4-management'])
records = []
for task, file in [('concurrent', 'Main.tnx'), ('transactional', 'BankService.tnx'), ('broker', 'Processor.tnx')]:
    for repeat in range(args.repeats + 1):
        negative = repeat == args.repeats
        with tempfile.TemporaryDirectory(prefix='tinox-checker-control-') as directory:
            work = Path(directory) / 'work'
            shutil.copytree(HERE / 'tasks' / task / 'starter', work)
            source = (HERE / 'checks/controls' / task / file).read_text()
            if negative:
                if task == 'concurrent':
                    source = (HERE / 'tasks/concurrent/starter/Main.tnx').read_text()
                elif task == 'transactional':
                    source = source.replace('@Transactional', '')
                else:
                    source = source.replace('ch.qos(1)', 'ch.qos(4)').replace('            ch.ack(message.deliveryTag)\n', '')
            (work / file).write_text(source)
            start = time.monotonic()
            result = subprocess.run([sys.executable, HERE / 'checks/accept.py', task, work, args.tinox.resolve()],
                                    capture_output=True, text=True, timeout=120, env=checker_env)
            expected = result.returncode != 0 if negative else result.returncode == 0
            record = {'task': task, 'repeat': repeat + 1, 'negative': negative,
                      'checker_behaved_as_expected': expected, 'exit_code': result.returncode,
                      'wall_seconds': time.monotonic() - start, 'stderr': result.stderr,
                      'control_sha256': hashlib.sha256(source.encode()).hexdigest()}
            records.append(record)
            (args.out / 'controls.json').write_text(json.dumps({
                'kind': 'checker_controls_no_model_calls', 'records': records,
                'compiler': subprocess.check_output([args.tinox, '--version'], text=True).strip(),
                'acceptance_sha256': hashlib.sha256((HERE / 'checks/accept.py').read_bytes()).hexdigest(),
                'service_image_ids': image_ids,
                'native_checker_sha256': hashlib.sha256((HERE / 'checks/concurrent_job.c').read_bytes()).hexdigest()}, indent=2))
            print(task, repeat + 1, 'negative' if negative else 'positive', 'pass' if expected else 'FAIL', flush=True)
            if not expected:
                raise SystemExit(result.stderr or 'checker accepted invalid code')
