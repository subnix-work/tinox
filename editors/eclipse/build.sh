#!/usr/bin/env bash
# Builds a plain, installable OSGi bundle JAR for the Tinox Eclipse
# plugin from the command line -- no Eclipse GUI, no PDE Export wizard,
# no "import as project + Run As Eclipse Application" needed just to USE
# the plugin (that workflow is still the right one for DEVELOPING it,
# see SETUP.md -- this script is for people who just want to install it,
# the same "build once, drop the artifact in, done" experience as any
# other Eclipse plugin installed from an update site or the Marketplace).
#
# Compiles against whatever Eclipse installation's bundle pool is found
# (LSP4E + TM4E + the core platform bundles this plugin's MANIFEST.MF
# Require-Bundle list needs must already be installed there -- see
# SETUP.md step 2 if they aren't), packages a real OSGi bundle JAR, and
# tells you where to copy it.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PLUGIN_DIR="$SCRIPT_DIR/tinox-eclipse"
BUILD_DIR="$SCRIPT_DIR/.build"
DIST_DIR="$SCRIPT_DIR/dist"

# --- Locate an Eclipse bundle pool to compile against ---------------------
find_plugins_dir() {
    if [ -n "${ECLIPSE_PLUGINS_DIR:-}" ]; then
        echo "$ECLIPSE_PLUGINS_DIR"
        return
    fi
    # Eclipse Installer / Oomph's shared bundle pool (the common case for
    # anything provisioned via the official Eclipse Installer -- verified
    # against a real local install: ~/.p2/pool/plugins, NOT
    # <eclipse-home>/plugins, which the installer leaves nearly empty).
    if [ -d "$HOME/.p2/pool/plugins" ]; then
        echo "$HOME/.p2/pool/plugins"
        return
    fi
    # A traditional all-in-one install (plugins/ directly populated).
    for candidate in "$HOME"/eclipse/*/eclipse/plugins /opt/eclipse/plugins /usr/share/eclipse/plugins /usr/lib/eclipse/plugins; do
        if [ -d "$candidate" ] && [ "$(find "$candidate" -maxdepth 1 -name '*.jar' | head -1)" ]; then
            echo "$candidate"
            return
        fi
    done
    echo ""
}

PLUGINS_DIR="$(find_plugins_dir)"
if [ -z "$PLUGINS_DIR" ]; then
    echo "error: could not find an Eclipse bundle pool to compile against." >&2
    echo "Set ECLIPSE_PLUGINS_DIR to your Eclipse installation's plugins directory" >&2
    echo "(for an Eclipse-Installer-provisioned install, this is usually ~/.p2/pool/plugins)." >&2
    exit 1
fi
echo "Using Eclipse bundles from: $PLUGINS_DIR"

REQUIRED_BUNDLES="org.eclipse.ui org.eclipse.core.runtime org.eclipse.core.resources org.eclipse.ui.ide org.eclipse.ui.console org.eclipse.lsp4e org.eclipse.tm4e.core org.eclipse.tm4e.registry"
for b in $REQUIRED_BUNDLES; do
    if ! find "$PLUGINS_DIR" -maxdepth 1 -name "${b}_*.jar" -print -quit | grep -q .; then
        echo "error: required bundle '$b' not found in $PLUGINS_DIR" >&2
        echo "Install it first (Help -> Install New Software... in Eclipse), see SETUP.md step 2." >&2
        exit 1
    fi
done

# --- Validate plugin.xml ----------------------------------------------------
# A real bug hit live: a malformed XML comment (a bare "--" inside a
# <!-- --> body, which the XML spec forbids anywhere in a comment, not
# just at its boundaries) silently disabled the ENTIRE plugin.xml at
# Eclipse's OWN parse time -- every extension point in the file, not
# just the one near the mistake -- with no signal anywhere in this
# script (javac/jar don't parse plugin.xml at all) or in Eclipse's UI
# beyond a buried Error Log entry the user had to go find. Catch this
# class of mistake here instead, before it ever reaches a real Eclipse
# install.
if command -v xmllint >/dev/null 2>&1; then
    xmllint --noout "$PLUGIN_DIR/plugin.xml"
else
    echo "warning: xmllint not found, skipping plugin.xml validation" >&2
fi

# --- Compile ---------------------------------------------------------------
rm -rf "$BUILD_DIR"
mkdir -p "$BUILD_DIR/bin"

CLASSPATH="$(find "$PLUGINS_DIR" -maxdepth 1 -name '*.jar' | tr '\n' ':')"

echo "Compiling..."
javac -nowarn --release 17 -d "$BUILD_DIR/bin" -cp "$CLASSPATH" \
    "$PLUGIN_DIR"/src/tinox/eclipse/*.java

# --- Package as an OSGi bundle JAR ------------------------------------------
# A raw manual build has no real build timestamp to substitute for
# MANIFEST.MF's "1.0.0.qualifier" PDE/Tycho placeholder -- replace it with
# an actual build-time qualifier so the result is a normal, valid bundle
# version instead of a literal ".qualifier" suffix.
QUALIFIER="$(date -u +%Y%m%d%H%M%S)"
VERSION="1.0.0.${QUALIFIER}"

mkdir -p "$BUILD_DIR/stage/META-INF"
sed "s/1\\.0\\.0\\.qualifier/${VERSION}/" "$PLUGIN_DIR/META-INF/MANIFEST.MF" \
    > "$BUILD_DIR/stage/META-INF/MANIFEST.MF"
cp "$PLUGIN_DIR/plugin.xml" "$BUILD_DIR/stage/"
cp -r "$PLUGIN_DIR/grammars" "$BUILD_DIR/stage/"
cp -r "$PLUGIN_DIR/icons" "$BUILD_DIR/stage/"
cp -r "$BUILD_DIR/bin/." "$BUILD_DIR/stage/"

mkdir -p "$DIST_DIR"
JAR_NAME="tinox.eclipse_${VERSION}.jar"
( cd "$BUILD_DIR/stage" && jar cfm "$DIST_DIR/$JAR_NAME" META-INF/MANIFEST.MF . )
rm -rf "$BUILD_DIR"

echo
echo "Built: dist/$JAR_NAME"
echo
echo "To install: copy it into your Eclipse installation's dropins/ folder"
echo "and restart Eclipse -- e.g.:"
echo "  cp dist/$JAR_NAME <eclipse-install-dir>/dropins/"
echo
echo "(Not sure where that is? Help -> About Eclipse IDE -> Installation"
echo "Details -> Configuration tab -> look for 'eclipse.home.location'.)"
