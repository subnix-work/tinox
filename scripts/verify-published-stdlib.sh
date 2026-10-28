#!/usr/bin/env bash
# Read-only archive drift and compiler compatibility checks for both tiers.
# No key is needed; every file, including package metadata, must match.
set -euo pipefail
cd "$(dirname "$0")/.."
exec python3 scripts/publish-stdlib.py --verify-only "$@"
