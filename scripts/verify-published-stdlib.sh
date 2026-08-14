#!/usr/bin/env bash
# Drift detector for the extended-tier stdlib packages (core/extended split,
# see CLAUDE.md): downloads each crates/tinox-core-ext/<module> package as
# currently published on tinox-central and diffs it against the local
# source. Read-only, no API key needed -- safe to run in CI too.
#
# A module is "drifted" when the local source no longer matches what's live
# (e.g. a translation/bugfix landed locally after the last publish). Since
# tinox-central enforces immutable versions (no overwrite of an existing
# group:artifactId:version), a drifted module needs a version bump before
# `scripts/publish-stdlib-ext.sh` can republish it.
set -uo pipefail
cd "$(dirname "$0")/.."

REPO_URL="${TINOX_CENTRAL_URL:-https://central.tinox-lang.de}"
GROUP="tinox.core"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

DRIFTED=()
MISSING=()
CLEAN=0

for dir in crates/tinox-core-ext/*/; do
    module=$(basename "$dir")
    toml="$dir/tinox.toml"
    if [ ! -f "$toml" ]; then
        echo "  skip $module (no tinox.toml)"
        continue
    fi
    version=$(grep -m1 '^version' "$toml" | sed -E 's/version = "(.*)"/\1/')
    printf '  %-16s %-8s ' "$module" "$version"

    resp="$TMP/$module.json"
    code=$(curl -sS -m 15 -o "$resp" -w '%{http_code}' "$REPO_URL/api/v1/$GROUP/$module/$version")
    # A version that was never published can come back as either 404 or 500
    # depending on the exact lookup path on the server side -- treat both as
    # "not published" rather than a hard fetch error.
    if [ "$code" = "404" ] || [ "$code" = "500" ]; then
        echo "NOT PUBLISHED"
        MISSING+=("$module")
        continue
    fi
    if [ "$code" != "200" ]; then
        echo "FETCH ERROR (HTTP $code)"
        DRIFTED+=("$module (fetch error)")
        continue
    fi

    jq -r '.contentBase64' "$resp" | base64 -d > "$TMP/$module.tar.gz"
    extract="$TMP/$module-extracted"
    mkdir -p "$extract"
    tar -xzf "$TMP/$module.tar.gz" -C "$extract"

    # Published archives are rooted at tinox/core/<module>/... plus a
    # top-level tinox.toml (see build_tar_gz's archive-root convention in
    # pm.rs). Since issue #185's namespace-mirroring migration, local
    # source now has the SAME nested shape at
    # crates/tinox-core-ext/<module>/tinox/core/<module>/... (previously it
    # was flat directly under $dir, with no tinox/core/<module> prefix) --
    # diff the matching subtrees.
    published_src="$extract/tinox/core/$module"
    local_src="$dir/tinox/core/$module"
    if [ ! -d "$published_src" ]; then
        echo "UNEXPECTED ARCHIVE LAYOUT"
        DRIFTED+=("$module (archive layout)")
        continue
    fi

    if diff -rq "$published_src" "$local_src" >/dev/null 2>&1; then
        echo "clean"
        CLEAN=$((CLEAN + 1))
    else
        echo "DRIFTED"
        DRIFTED+=("$module")
    fi
done

echo
echo "== Summary =="
echo "clean: $CLEAN"
if [ ${#MISSING[@]} -gt 0 ]; then
    echo "never published: ${MISSING[*]}"
fi
if [ ${#DRIFTED[@]} -gt 0 ]; then
    echo "drifted (local source differs from what's live): ${DRIFTED[*]}"
    echo
    echo "Drifted/missing modules need a version bump in their tinox.toml"
    echo "before scripts/publish-stdlib-ext.sh can (re)publish them --"
    echo "tinox-central enforces immutable versions, no in-place overwrite."
    exit 1
fi
echo "All published extended-tier modules match local source."
