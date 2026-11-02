#!/usr/bin/env bash
# Runs the given `cargo test` arguments with output captured and fails when any test reported SKIP.
# Several integration tests skip themselves silently when an optional tool is missing (docker, psql, sqlite3,
# curl, ...). A lane that claims coverage (for example the native ARM64 service lane, #557) must not pass by
# skipping, so this wrapper lists every skip and turns it into a failure.
#
# usage: scripts/check-no-skips.sh <cargo test args...>
set -uo pipefail
log=$(mktemp)
trap 'rm -f "$log"' EXIT
cargo test "$@" -- --nocapture --test-threads=1 2>&1 | tee "$log"
status=${PIPESTATUS[0]}
skips=$(grep -E '^SKIP |\bSKIP ' "$log" || true)
if [ -n "$skips" ]; then
  echo
  echo "error: mandatory tests were skipped:" >&2
  echo "$skips" >&2
  exit 1
fi
exit "$status"
