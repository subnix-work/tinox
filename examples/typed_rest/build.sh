#!/usr/bin/env bash
# Stage the modified extended modules without changing immutable registry packages.
set -euo pipefail
cd "$(dirname "$0")"
repo_root="$(cd ../.. && pwd)"
tinox_bin="${TINOX:-$repo_root/target/release/tinox}"
for module in http_server json; do
    stage_dir=".tinox/deps/tinox.core/$module/checkout"
    mkdir -p "$stage_dir"
    cp -R "$repo_root/crates/tinox-core-ext/$module/tinox" "$stage_dir/"
done
"$tinox_bin" build Main.tnx -o "${1:-.tinox/server}"
