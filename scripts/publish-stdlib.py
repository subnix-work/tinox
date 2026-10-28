#!/usr/bin/env python3
"""Build, validate and publish immutable registry archives for both stdlib tiers.

Run without --publish for offline preparation and compiler checks. Authentication
is read only from TINOX_CENTRAL_ADMIN_KEY; archives contain source, not binaries.
"""
import argparse
import base64
from concurrent.futures import ThreadPoolExecutor
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import tomllib
import urllib.error
import urllib.request

import reference


def archive(root, package):
    source = reference.package_source(root, package)
    files = sorted(source.rglob('*.tnx'))
    if not files:
        raise ValueError(f'empty package: {source}')
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode='w:gz') as tar:
        tar.add(root / package['manifest'], arcname='tinox.toml')
        for file in files:
            tar.add(file, arcname=str(Path('tinox/core') / package['name'] / file.relative_to(source)))
    return output.getvalue()


def consumers(binary, inventory, extracted, work, compiler):
    """Compile every module from the exact archives using local overrides."""
    def check(package):
        name = package['name']
        project = work / name
        project.mkdir()
        manifest = ('[package]\nname = "release-check"\nversion = "0.0.0"\n'
                    f'compiler = "{compiler}"\n\n[[dependencies]]\ngroup = "tinox.core"\n'
                    f'artifactId = "{name}"\nversion = "{package["version"]}"\n')
        by_name = {p['name']: p for p in inventory}
        required = {name}
        pending = [name]
        while pending:
            for dependency in by_name[pending.pop()]['dependencies']:
                child = dependency['artifactId']
                if child not in required:
                    required.add(child)
                    pending.append(child)
        for dependency in (by_name[n] for n in sorted(required)):
            manifest += ('\n[[overrides]]\ngroup = "tinox.core"\n'
                         f'artifactId = "{dependency["name"]}"\n'
                         f'path = {json.dumps(str(extracted / dependency["name"]))}\n')
        (project / 'tinox.toml').write_text(manifest)
        source = extracted / name / 'tinox/core' / name
        modules = sorted({file.parent.relative_to(source) for file in source.rglob('*.tnx')})
        environment = os.environ.copy()
        if 'http3_server' in required:
            environment['TINOX_HTTP3'] = '1'
        # Public submodules may intentionally use the same unqualified type
        # names (rest.client/rest.server); test each import independently.
        for module in modules:
            imported = 'tinox.core.' + name + ('.' + '.'.join(module.parts) if module.parts else '')
            (project / 'Main.tnx').write_text('import ' + imported + '\n\nclass Main\n{\n    fnc main() -> Int32\n    {\n        println("archive-ok")\n        return 0\n    }\n}\n')
            build = subprocess.run([str(binary), 'build', 'Main.tnx', '-o', str(project / 'consumer')],
                                   cwd=project, env=environment, capture_output=True, text=True, timeout=120)
            if build.returncode:
                raise ValueError(f'{imported}: archive consumer build failed:\n{build.stdout}{build.stderr}')
            result = subprocess.run([str(project / 'consumer')], capture_output=True, text=True, timeout=30)
            if result.returncode or result.stdout != 'archive-ok\n':
                raise ValueError(f'{imported}: archive consumer failed: {result.stdout} {result.stderr}')
        print(f'compiler/archive consumer: {name} {package["version"]} OK', flush=True)

    with ThreadPoolExecutor(max_workers=4) as pool:
        list(pool.map(check, inventory))


def request(url, data=None, key=None):
    headers = {'Content-Type': 'application/json'}
    if key:
        headers['Authorization'] = 'Bearer ' + key
    req = urllib.request.Request(url, data=data, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=60) as response:
            return response.status, json.load(response)
    except urllib.error.HTTPError as error:
        # Never include request headers or credentials in errors.
        raise ValueError(f'{req.get_method()} {url}: HTTP {error.code}') from None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    action = parser.add_mutually_exclusive_group()
    action.add_argument('--publish', action='store_true')
    action.add_argument('--verify-only', action='store_true', help='compare registry archives without uploading')
    parser.add_argument('--tier', choices=['all', 'extended'], default='all')
    parser.add_argument('--tinox', type=Path, default=reference.ROOT / 'target/release/tinox')
    args = parser.parse_args()
    root = reference.ROOT
    binary = args.tinox.resolve()
    inventory = reference.packages(root)
    reference.check_packages(root, inventory)
    compiler = json.loads(reference.run(binary, 'reference'))['compiler_version']
    for package in inventory:
        declared = tomllib.loads((root / package['manifest']).read_text())['package'].get('compiler')
        if declared != compiler:
            raise ValueError(f'{package["name"]}: compiler {declared!r} does not match {compiler}')
    payloads = {p['name']: archive(root, p) for p in inventory}
    with tempfile.TemporaryDirectory(prefix='tinox-release-') as temp:
        work = Path(temp)
        extracted = work / '.tinox/packages'
        for package in inventory:
            target = extracted / package['name']
            target.mkdir(parents=True)
            with tarfile.open(fileobj=io.BytesIO(payloads[package['name']])) as tar:
                tar.extractall(target, filter='data')
        consumers(binary, inventory, extracted, work, compiler)
    if not args.publish and not args.verify_only:
        print(f'Prepared and checked {len(inventory)} archives; no uploads requested.')
        return
    key = os.environ.get('TINOX_CENTRAL_ADMIN_KEY')
    if args.publish and not key:
        raise ValueError('TINOX_CENTRAL_ADMIN_KEY is required for --publish')
    base = os.environ.get('TINOX_CENTRAL_URL', 'https://central.tinox-lang.de').rstrip('/')
    selected = [p for p in inventory if args.tier == 'all' or 'tinox-core-ext/' in p['manifest']]
    # Dependencies must already be available before the dependent is uploaded.
    pending = selected[:]
    completed = set()
    while pending:
        ready = [p for p in pending if all(d['artifactId'] in completed or
                 not any(q['name'] == d['artifactId'] for q in selected)
                 for d in p['dependencies'])]
        if not ready:
            raise ValueError('dependency cycle in release inventory')
        for package in ready:
            name, version = package['name'], package['version']
            url = f'{base}/api/v1/{package["group"]}/{name}/{version}'
            payload = payloads[name]
            # A rerun accepts an existing version only when its complete file
            # contents match. Gzip timestamps alone are not source changes.
            try:
                _, existing = request(url)
            except ValueError as error:
                if not str(error).endswith('HTTP 404'):
                    raise
                existing = None
            if existing is None:
                if args.verify_only:
                    raise ValueError(f'{name} {version}: not published')
                body = json.dumps(dict(filename=f'{name}-{version}.tar.gz',
                                       contentBase64=base64.b64encode(payload).decode())).encode()
                status, _ = request(url, body, key)
                if status != 201:
                    raise ValueError(f'{name}: expected HTTP 201, got {status}')
                _, existing = request(url)
            actual = base64.b64decode(existing['contentBase64'], validate=True)
            def contents(data):
                with tarfile.open(fileobj=io.BytesIO(data)) as tar:
                    return {f.name: tar.extractfile(f).read() for f in tar.getmembers() if f.isfile()}
            if contents(actual) != contents(payload):
                raise ValueError(f'{name} {version}: immutable registry archive differs from this release')
            print(f'registry verified: {name} {version}', flush=True)
            completed.add(name)
            pending.remove(package)
    print(f'Published/verified {len(completed)} packages.')


if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        raise SystemExit(f'error: {error}')
