#!/usr/bin/env bash
# Source validation is independent of immutable published dependency pins.
set -euo pipefail
cd "$(dirname "$0")/.."
workspace=$(mktemp -d)
trap 'rm -rf "$workspace"' EXIT
python3 - "$workspace" <<'PY'
from pathlib import Path
import shutil
import sys
import tomllib

source = Path('crates/tinox-core-ext')
destination = Path(sys.argv[1])
modules = {p.parent.name: tomllib.loads(p.read_text()) for p in source.glob('*/tinox.toml')}
for name, manifest in modules.items():
    staged = destination / name
    shutil.copytree(source / name, staged)
    # This lane tests the current source set. The artifact lane retains and
    # verifies the checked-in, immutable dependency versions without rewriting.
    lines = (staged / 'tinox.toml').read_text().splitlines()
    blocks = ('\n'.join(lines) + '\n').split('[[dependencies]]')
    for index in range(1, len(blocks)):
        dependency = tomllib.loads('[[dependencies]]' + blocks[index])['dependencies'][0]
        if dependency['group'] == 'tinox.core' and dependency['artifactId'] in modules:
            version = modules[dependency['artifactId']]['package']['version']
            blocks[index] = '\n'.join(
                f'version = "{version}"' if line.strip().startswith('version =') else line
                for line in blocks[index].split('\n')
            )
    (staged / 'tinox.toml').write_text('[[dependencies]]'.join(blocks))
PY
TINOX_TEST_WORKSPACE_STDLIB="$workspace" cargo test --release -p tinox --test stdlib_smoke -- --nocapture
