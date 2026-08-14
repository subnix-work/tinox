#!/usr/bin/env bash
# Publishes every crates/tinox-core-ext/<module> package to tinox-central.
# Requires TINOX_CENTRAL_ADMIN_KEY (an admin bearer token only the site
# operator has) -- NOT something to run unattended or from CI. Run this by
# hand after `scripts/verify-published-stdlib.sh` shows drift and you've
# bumped the affected modules' tinox.toml [package] version accordingly
# (tinox-central enforces immutable versions: republishing the SAME
# group:artifactId:version is a 409, not an overwrite).
#
# API contract (tinox-central PLAN.md §POST /api/v1/{group}/{artifactId}/
# {version}): Authorization: Bearer <key>, body
# {"filename": "...", "contentBase64": "..."}, 201 on success, 409 if that
# exact version already exists, 400 on invalid input.
set -uo pipefail
cd "$(dirname "$0")/.."

if [ -z "${TINOX_CENTRAL_ADMIN_KEY:-}" ]; then
    echo "error: TINOX_CENTRAL_ADMIN_KEY is not set (admin bearer token for tinox-central)" >&2
    exit 1
fi

REPO_URL="${TINOX_CENTRAL_URL:-https://central.tinox-lang.de}"
GROUP="tinox.core"

TINOX_BIN="target/release/tinox"
if [ ! -x "$TINOX_BIN" ]; then
    TINOX_BIN="target/debug/tinox"
fi
if [ ! -x "$TINOX_BIN" ]; then
    echo "error: no tinox binary at target/{release,debug}/tinox -- run 'cargo build -p tinox' first" >&2
    exit 1
fi
TINOX_BIN="$(cd "$(dirname "$TINOX_BIN")" && pwd)/$(basename "$TINOX_BIN")"

PUBLISHED=0
SKIPPED=0
FAILED=0

for dir in crates/tinox-core-ext/*/; do
    module=$(basename "$dir")
    toml="$dir/tinox.toml"
    if [ ! -f "$toml" ]; then
        echo "  skip $module (no tinox.toml)"
        continue
    fi
    version=$(grep -m1 '^version' "$toml" | sed -E 's/version = "(.*)"/\1/')
    printf '  %-16s %-8s ' "$module" "$version"

    # Build the archive by hand instead of `tinox package`: that command
    # expects a normal project's src/ layout and archives entries relative
    # to src/ (see its own doc comment in pm.rs) -- crates/tinox-core-ext/
    # <module>/ has no src/, and the live packages on central preserve the
    # FULL `tinox/core/<module>/...` path inside the archive (verified by
    # extracting a live artifact by hand), which `tinox package`'s
    # src/-relative stripping wouldn't produce even if src/ existed.
    #
    # Since issue #185's namespace-mirroring migration, the local source
    # tree at `$dir` ALREADY has this exact `tinox/core/<module>/...` shape
    # (it's what makes the new namespace-path compiler check pass for these
    # files in the first place) -- so staging is now a direct copy of that
    # existing subtree, not a synthesis of it the way it used to be before
    # the migration (this used to `find ... | cp --parents` the module's
    # flat files into a freshly-fabricated `tinox/core/<module>/` prefix
    # that didn't exist on disk anywhere else).
    archive="$module-$version.tar.gz"
    archive_path="$PWD/$dir$archive"
    staging=$(mktemp -d)
    cp -r "$dir/tinox" "$staging/tinox"
    cp "$toml" "$staging/tinox.toml"
    rm -f "$archive_path"
    ( cd "$staging" && tar -czf "$archive_path" tinox tinox.toml )
    rm -rf "$staging"
    if [ ! -f "$archive_path" ]; then
        echo "PACKAGE FAILED"
        FAILED=$((FAILED + 1))
        continue
    fi

    content_base64=$(base64 -w0 "$archive_path")
    body=$(jq -n --arg filename "$archive" --arg content "$content_base64" \
        '{filename: $filename, contentBase64: $content}')

    code=$(curl -sS -m 60 -o /tmp/publish-resp.json -w '%{http_code}' \
        -X POST "$REPO_URL/api/v1/$GROUP/$module/$version" \
        -H "Authorization: Bearer $TINOX_CENTRAL_ADMIN_KEY" \
        -H "Content-Type: application/json" \
        -d "$body")
    rm -f "$archive_path"

    case "$code" in
        201)
            echo "published"
            PUBLISHED=$((PUBLISHED + 1))
            ;;
        409)
            echo "already exists (bump version to republish)"
            SKIPPED=$((SKIPPED + 1))
            ;;
        *)
            echo "FAILED (HTTP $code): $(cat /tmp/publish-resp.json 2>/dev/null)"
            FAILED=$((FAILED + 1))
            ;;
    esac
done
rm -f /tmp/publish-resp.json

echo
echo "== Summary =="
echo "published: $PUBLISHED, already existed: $SKIPPED, failed: $FAILED"
if [ "$FAILED" -gt 0 ]; then
    exit 1
fi
