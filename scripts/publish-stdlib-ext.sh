#!/usr/bin/env bash
# Compatibility entry point for publishing extended-tier packages only.
# Both tiers: python3 scripts/publish-stdlib.py --publish
# Offline preparation: python3 scripts/publish-stdlib.py
set -euo pipefail
cd "$(dirname "$0")/.."
exec python3 scripts/publish-stdlib.py --publish --tier extended "$@"
