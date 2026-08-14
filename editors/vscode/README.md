# Tinox VS Code Extension

VS Code extension that integrates the `tinox-lsp` language server.

## Features (via LSP)

- Error underlining (diagnostics)
- Hover → types and function signatures
- Ctrl+Space → autocomplete
- F12 / Ctrl+Click → go to definition
- Outline view → document symbols
- Syntax highlighting (TextMate grammar)
- "Tinox: Run File" command (Ctrl+F11, or the editor toolbar/context menu) → compiles and runs the current file, output in the "Tinox" output channel

## Setup

### 1. Install tinox-lsp

```bash
../install-lsp.sh
# Installs to ~/.cargo/bin/tinox-lsp -- shared by every editor under editors/
```

### 2. Build the extension

```bash
npm install
npm run compile
```

### 3. Try it without packaging (Extension Development Host)

```bash
code --extensionDevelopmentPath="$(pwd)"
```

Opens a new VS Code window with the extension active. Open a `.tnx` file to try it.

### 4. Package and install for real

```bash
npx @vscode/vsce package
```

Produces `tinox-1.0.0.vsix`. In VS Code: **Extensions → "..." menu → Install from VSIX...** and pick the generated file.

### 5. Configure the binary path (only if auto-detection doesn't find it)

Settings → search "Tinox" → `tinox.lsp.path`. Auto-detected in this order if left empty: `~/.cargo/bin/tinox-lsp`, `/usr/local/bin/tinox-lsp`, `/usr/bin/tinox-lsp`.

## Project structure

```
editors/vscode/
├── package.json                # Extension manifest (contributes.*)
├── language-configuration.json # Brackets, comments, auto-closing pairs
├── syntaxes/tinox.tmLanguage.json  # TextMate grammar (kept in sync by
│                                    # hand with editors/eclipse's copy --
│                                    # see CLAUDE.md's "Editor Support"
│                                    # section)
└── src/extension.ts            # LSP client + "Run File" command
```

No CI build, no Marketplace publish -- distribute the packaged `.vsix` directly, same as the Eclipse plugin's own manual-JAR-into-`dropins/` distribution.
