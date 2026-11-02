#!/usr/bin/env python3
"""Release manifest for the supported service stack (#558).

generate  write docs/releases/<compiler>.json: compiler identity, runtime and native inputs, and for every
          package of the stack (members plus dependency closure) its version, a content hash of the exact release
          archive, its immutable docs page hash and its dependency pins.
verify    compare the working tree against that manifest and report every difference with the action to take.
          --strict also compares compiler/runtime identity; --registry downloads each archive from tinox-central
          and compares file contents.

The manifest is deterministic (no timestamps); `source_commit` is informational and not compared.
"""
import argparse
import base64
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tomllib
import urllib.error

sys.path.insert(0, str(Path(__file__).resolve().parent))
import reference  # noqa: E402

ROOT = reference.ROOT
SCHEMA = 1


def _publish():
    spec = importlib.util.spec_from_file_location('publish_stdlib', ROOT / 'scripts/publish-stdlib.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def archive_contents(data):
    """{name: bytes} of an archive's regular files (gzip timestamps are not part of the identity)."""
    with tarfile.open(fileobj=io.BytesIO(data)) as tar:
        return {f.name: tar.extractfile(f).read() for f in tar.getmembers() if f.isfile()}


def content_hash(files):
    digest = hashlib.sha256()
    for name in sorted(files):
        digest.update(name.encode() + b'\0' + sha256(files[name]).encode() + b'\n')
    return digest.hexdigest()


def stack_members(root, inventory, stack):
    """Stack packages plus the closure of their dependencies, dependencies first."""
    by_name = {p['name']: p for p in inventory}
    order, seen = [], set()

    def visit(name, chain=()):
        if name in seen:
            return
        if name not in by_name:
            raise ValueError(f'supported stack names unknown package {name!r}')
        if name in chain:
            raise ValueError('dependency cycle: ' + ' -> '.join(chain + (name,)))
        for dependency in by_name[name]['dependencies']:
            visit(dependency['artifactId'], chain + (name,))
        seen.add(name)
        order.append(by_name[name])

    for name in stack['packages']:
        visit(name)
    return order


def builder_packages(root):
    text = (root / 'docker/builder/Dockerfile').read_text()
    stages = re.findall(r'apt-get install -y --no-install-recommends \\\n(.*?)&& rm -rf', text, re.S)
    return [sorted(re.findall(r'[a-z0-9][a-z0-9+.\-]+', block.replace('\\', ' '))) for block in stages]


def source_commit(root):
    try:
        commit = subprocess.run(['git', 'rev-parse', 'HEAD'], cwd=root, capture_output=True, text=True, check=True).stdout.strip()
        dirty = bool(subprocess.run(['git', 'status', '--porcelain', '--untracked-files=no'], cwd=root,
                                    capture_output=True, text=True, check=True).stdout.strip())
        return dict(commit=commit, tree_clean=not dirty)
    except (OSError, subprocess.SubprocessError):
        return dict(commit=None, tree_clean=None)


def build_manifest(root=ROOT, archive=None):
    archive = archive or _publish().archive
    stack = tomllib.loads((root / 'docs/supported-stack.toml').read_text())['stack']
    inventory = reference.packages(root)
    members = stack_members(root, inventory, stack)
    packages = []
    for package in members:
        declared = tomllib.loads((root / package['manifest']).read_text())['package'].get('compiler')
        if declared != stack['compiler']:
            raise ValueError(f"{package['name']} {package['version']} declares compiler {declared!r}, the stack uses {stack['compiler']}")
        docs = root / package['docs']
        if not docs.is_file():
            raise ValueError(f"{package['name']} {package['version']}: missing immutable docs page {package['docs']}; run make docs-stage")
        files = archive_contents(archive(root, package))
        packages.append(dict(
            group=package['group'], name=package['name'], version=package['version'],
            content_sha256=content_hash(files), files=len(files),
            docs=package['docs'], docs_sha256=sha256(docs.read_bytes()),
            dependencies=[dict(name=d['artifactId'], version=d['version']) for d in package['dependencies']],
        ))
    cargo = tomllib.loads((root / 'Cargo.toml').read_text())
    compiler_version = cargo['workspace']['package']['version']
    if compiler_version != stack['compiler']:
        raise ValueError(f"workspace version {compiler_version} does not match the stack's compiler {stack['compiler']}")
    return dict(
        schema=SCHEMA,
        stack=dict(name=stack['name'], database=stack['database'], members=stack['packages']),
        compiler=dict(
            version=compiler_version,
            runtime_c_sha256=sha256((root / 'runtime/runtime.c').read_bytes()),
            runtime_h_sha256=sha256((root / 'runtime/runtime.h').read_bytes()),
            cargo_lock_sha256=sha256((root / 'Cargo.lock').read_bytes()),
            builder_apt_packages=builder_packages(root),
            source=source_commit(root),
        ),
        packages=packages,
    )


def manifest_path(root, manifest):
    return root / 'docs/releases' / f"{manifest['compiler']['version']}.json"


def render(manifest):
    return json.dumps(manifest, indent=2, sort_keys=False) + '\n'


def diff(expected, actual, strict=False):
    """Human-readable differences (expected = recorded manifest, actual = rebuilt from the tree)."""
    problems = []
    if expected.get('schema') != SCHEMA:
        problems.append(f"manifest schema {expected.get('schema')} is not supported (expected {SCHEMA})")
    if expected['compiler']['version'] != actual['compiler']['version']:
        problems.append(f"compiler {actual['compiler']['version']} does not match the manifest's {expected['compiler']['version']}: "
                        f"use the matching release manifest")
    if strict:
        for key in ('runtime_c_sha256', 'runtime_h_sha256', 'cargo_lock_sha256', 'builder_apt_packages'):
            if expected['compiler'][key] != actual['compiler'][key]:
                problems.append(f'compiler input {key} changed since the manifest was written: regenerate it with `make release-manifest` '
                                f'(and publish new package versions if the runtime ABI changed)')
    old = {p['name']: p for p in expected['packages']}
    new = {p['name']: p for p in actual['packages']}
    for name in sorted(old.keys() - new.keys()):
        problems.append(f'package {name} {old[name]["version"]} is in the manifest but no longer in the stack')
    for name in sorted(new.keys() - old.keys()):
        problems.append(f'package {name} {new[name]["version"]} joined the stack: regenerate the manifest')
    for name in sorted(old.keys() & new.keys()):
        a, b = old[name], new[name]
        if a['version'] != b['version']:
            problems.append(f'package {name}: manifest has {a["version"]}, the tree has {b["version"]}: regenerate the manifest after publishing')
        elif a['content_sha256'] != b['content_sha256']:
            problems.append(f'package {name} {a["version"]} changed without a version bump (immutable once published): '
                            f'bump its version, stage docs (`make docs-stage`) and regenerate the manifest')
        if a['version'] == b['version'] and a['docs_sha256'] != b['docs_sha256']:
            problems.append(f'docs page {a["docs"]} changed; published version pages are immutable: restore it from git')
        if a['dependencies'] != b['dependencies']:
            problems.append(f'package {name}: dependency pins changed from {a["dependencies"]} to {b["dependencies"]}')
    return problems


def verify_registry(manifest, base=None, fetch=None):
    """Compare every recorded package with the archive tinox-central serves; returns problems."""
    publish = _publish()
    base = (base or os.environ.get('TINOX_CENTRAL_URL', 'https://central.tinox-lang.de')).rstrip('/')
    fetch = fetch or publish.request
    problems = []
    for package in manifest['packages']:
        url = f"{base}/api/v1/{package['group']}/{package['name']}/{package['version']}"
        try:
            _, body = fetch(url)
        except (ValueError, OSError) as error:
            problems.append(f"{package['name']} {package['version']}: not available from the registry ({error}); publish it first")
            continue
        files = archive_contents(base64.b64decode(body['contentBase64'], validate=True))
        if content_hash(files) != package['content_sha256']:
            problems.append(f"{package['name']} {package['version']}: the registry archive differs from the manifest "
                            f"(a published version is immutable: bump the version)")
    return problems


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('command', choices=['generate', 'verify'])
    parser.add_argument('--strict', action='store_true', help='also compare compiler/runtime inputs')
    parser.add_argument('--registry', action='store_true', help='also compare against tinox-central')
    args = parser.parse_args()
    try:
        actual = build_manifest()
        path = manifest_path(ROOT, actual)
        if args.command == 'generate':
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(render(actual))
            print(f'wrote {path.relative_to(ROOT)} ({len(actual["packages"])} packages)')
            return
        if not path.is_file():
            raise ValueError(f'{path.relative_to(ROOT)} is missing; run make release-manifest')
        recorded = json.loads(path.read_text())
        problems = diff(recorded, actual, strict=args.strict)
        if args.registry:
            problems += verify_registry(recorded)
        if problems:
            raise ValueError('release manifest mismatch:\n  - ' + '\n  - '.join(problems))
        print(f'release manifest OK: {len(recorded["packages"])} packages, compiler {recorded["compiler"]["version"]}')
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        raise SystemExit(f'error: {error}')


if __name__ == '__main__':
    main()
