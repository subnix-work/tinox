# Tinox Eclipse Plugin

Eclipse plugin that integrates the `tinox-lsp` language server.

## Features (via LSP)

- Error underlining (diagnostics)
- Hover → types and function signatures
- Ctrl+Space → autocomplete
- F3 / Ctrl+Click → go to definition
- Outline view → document symbols

## Setup

### 1. Install tinox-lsp

```bash
../install-lsp.sh
# Installs to ~/.cargo/bin/tinox-lsp -- shared by every editor under editors/
```

### 2. Load the plugin in Eclipse

**Prerequisite:** Eclipse IDE for Plugin Development (≥ 2023-09) with PDE and LSP4E.

Install LSP4E if not already present:
- Help → Install New Software
- Work with: `https://download.eclipse.org/lsp4e/releases/latest/`
- Install: "Language Server Protocol client for Eclipse"

Import the plugin:
1. File → Import → Existing Projects into Workspace
2. Root directory: this directory (`editors/eclipse/tinox-eclipse`)
3. Finish

### 3. Start the plugin

1. Right-click the `tinox-eclipse` project → Run As → Eclipse Application
2. In the new Eclipse window: create a new project, create a `*.tnx` file
3. The language server starts automatically

### 4. Configure the binary path

Window → Preferences → Tinox → set the path to the `tinox-lsp` binary

## Project structure

```
tinox-eclipse/
├── META-INF/MANIFEST.MF       # OSGi bundle manifest
├── plugin.xml                 # Extension points
├── build.properties
└── src/tinox/eclipse/
    ├── Activator.java                  # Plugin lifecycle
    ├── TinoxLanguageServer.java        # LSP server process
    ├── TinoxPreferencePage.java        # Settings UI
    └── TinoxPreferenceInitializer.java # Default values
```
