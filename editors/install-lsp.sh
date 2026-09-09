#!/usr/bin/env bash
# Builds tinox-lsp and installs it to ~/.cargo/bin/ -- shared by every
# editor integration under editors/ (Eclipse, VS Code, ...), since none
# of this is specific to any one of them.
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"

echo "Building tinox-lsp..."
cargo build --release -p tinox-lsp --manifest-path "$SCRIPT_DIR/Cargo.toml"

DEST="$HOME/.cargo/bin/tinox-lsp"
cp "$SCRIPT_DIR/target/release/tinox-lsp" "$DEST"
chmod +x "$DEST"

echo "Installed to $DEST"
echo "Configure this path in your editor's Tinox settings if it doesn't auto-detect it."
