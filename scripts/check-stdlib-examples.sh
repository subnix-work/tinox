#!/usr/bin/env bash
# Regression guard for issue #266: `tinox doc` silently emits no Examples
# section when the staged project it's run against has no `examples/`
# directory. Example sources live at
# docs/tinox-core/<module>/examples/*.tnx (see CLAUDE.md's "Every
# tinox-central Publish Needs a Matching Per-Version Doc Page" section for
# how they get staged) and, before this script existed, were never
# committed for most modules -- so a module whose CURRENTLY PUBLISHED
# docs.html has real Examples content, but whose examples/ source is
# missing, would silently lose that section on its next version bump: the
# old page stays correct and the new one is just quietly worse, with no
# error anywhere.
#
# This script makes that loud instead: for every crates/tinox-core-ext/
# <module>, it checks whether the module's newest existing
# docs/tinox-core/<module>/<version>/docs.html contains at least one
# Examples section (a `<pre><code>` block), and if so, requires
# docs/tinox-core/<module>/examples/ to exist with at least one `.tnx`
# file. Run this BEFORE regenerating/republishing a module's docs.html --
# a module that fails here needs its examples/ source recovered (or newly
# written) first, or the new docs.html will regress.
#
# Read-only, no network needed -- safe to run in CI too.
set -uo pipefail
cd "$(dirname "$0")/.."

MISSING=()
OK=0
NO_EXAMPLES_YET=0

for dir in crates/tinox-core-ext/*/; do
    module=$(basename "$dir")
    docs_dir="docs/tinox-core/$module"
    if [ ! -d "$docs_dir" ]; then
        continue
    fi

    latest_version_dir=""
    latest_version=""
    for vdir in "$docs_dir"/*/; do
        vname=$(basename "$vdir")
        [ "$vname" = "examples" ] && continue
        [ -f "$vdir/docs.html" ] || continue
        if [ -z "$latest_version" ] || [ "$(printf '%s\n%s\n' "$latest_version" "$vname" | sort -V | tail -1)" = "$vname" ]; then
            latest_version="$vname"
            latest_version_dir="$vdir"
        fi
    done
    if [ -z "$latest_version_dir" ]; then
        continue
    fi

    printf '  %-16s %-8s ' "$module" "$latest_version"

    if ! grep -q '<pre><code>' "$latest_version_dir/docs.html"; then
        echo "no Examples section published yet"
        NO_EXAMPLES_YET=$((NO_EXAMPLES_YET + 1))
        continue
    fi

    examples_dir="$docs_dir/examples"
    if [ -d "$examples_dir" ] && [ -n "$(find "$examples_dir" -maxdepth 1 -name '*.tnx' -print -quit 2>/dev/null)" ]; then
        echo "ok (examples/ present)"
        OK=$((OK + 1))
    else
        echo "MISSING examples/ SOURCE -- published docs.html HAS an Examples section that would be LOST on the next version bump"
        MISSING+=("$module")
    fi
done

echo
echo "== Summary =="
echo "ok: $OK, no examples published yet: $NO_EXAMPLES_YET"
if [ ${#MISSING[@]} -gt 0 ]; then
    echo "AT RISK (published Examples, no recoverable source): ${MISSING[*]}"
    echo
    echo "Recover each listed module's example source from its newest"
    echo "docs/tinox-core/<module>/<version>/docs.html (strip the syntax-"
    echo "highlighting markup, unescape HTML entities) and commit it under"
    echo "docs/tinox-core/<module>/examples/ before regenerating that"
    echo "module's docs.html -- otherwise the next version's page silently"
    echo "loses its Examples section (see issue #266)."
    exit 1
fi
echo "No module is at risk of silently losing its Examples section."
