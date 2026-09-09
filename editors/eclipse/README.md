# Tinox Eclipse Plugin

Eclipse plugin that integrates the `tinox-lsp` language server.

## Features

Via LSP:
- Error underlining (diagnostics)
- Hover → types and function signatures
- Ctrl+Space → autocomplete
- F3 / Ctrl+Click → go to definition
- Outline view → document symbols

Project import:
- `File → Import → Tinox → Import Existing Tinox Project` — pick a
  `tinox.toml`, get a real project pointed at that directory (no file
  copy), with `src/`/`tests/` shown with a distinct source-folder icon

## Install (just want to use it)

**Prerequisite:** Eclipse IDE with [LSP4E](https://download.eclipse.org/lsp4e/releases/latest/) and [TM4E](https://download.eclipse.org/tm4e/releases/latest/) installed (`Help → Install New Software…`, work with those update sites — most Eclipse "for Java/C++/... Developers" packages already include both).

```bash
# 1. Install tinox-lsp (shared by every editor under editors/):
../install-lsp.sh

# 2. Build the plugin JAR:
./build.sh
```

`build.sh` compiles against whatever Eclipse installation's bundle pool it
finds (auto-detected; override with `ECLIPSE_PLUGINS_DIR` if it guesses
wrong) and prints the resulting JAR's path, e.g. `dist/tinox.eclipse_1.0.0.<timestamp>.jar`.

**3. Install it** — copy that JAR into your Eclipse installation's `dropins/`
folder and restart Eclipse:

```bash
cp dist/tinox.eclipse_*.jar <eclipse-install-dir>/dropins/
```

Not sure where that is? In Eclipse: `Help → About Eclipse IDE → Installation
Details → Configuration` tab, look for `eclipse.home.location`.

That's it — no importing the project, no "Run As → Eclipse Application".
Open a `.tnx` file and the language server starts automatically. If it
doesn't find `tinox-lsp` on its own: `Window → Preferences → Tinox`, set
the path explicitly.

## Importing a project

`File → Import → Tinox → Import Existing Tinox Project` — pick the
project's `tinox.toml`. The project name is pre-filled from
`[package]` `name` in the TOML (editable); the directory is imported in
place, no files are copied or moved. `src/` is required (matches
`tinox`'s own rule that a project always has one); `tests/` is picked
up automatically if present, but isn't required — most real Tinox
projects don't have one. Both get a distinct folder icon once imported.

## Develop the plugin

If you're changing the plugin's own code (not just using it), work with it
as a live PDE project instead of rebuilding+reinstalling the JAR every
time:

1. File → Import → Existing Projects into Workspace → root directory:
   this directory (`editors/eclipse/tinox-eclipse`)
2. Right-click the `tinox-eclipse` project → Run As → Eclipse Application
   — opens a second, nested Eclipse window running your in-progress
   changes live
3. In that window: create/open a `*.tnx` file — the language server
   starts automatically
4. `Window → Preferences → Tinox` to point at a specific `tinox-lsp`
   binary if needed

## Project structure

```
tinox-eclipse/
├── META-INF/MANIFEST.MF       # OSGi bundle manifest
├── plugin.xml                 # Extension points
├── build.properties
├── grammars/tinox.tmLanguage.json  # Syntax highlighting
├── icons/                          # Source-folder icons
└── src/tinox/eclipse/
    ├── Activator.java                  # Plugin lifecycle
    ├── TinoxLanguageServer.java        # LSP server process
    ├── TinoxPreferencePage.java        # Settings UI
    ├── TinoxPreferenceInitializer.java # Default values
    ├── RunTinoxHandler.java            # "Run Tinox File" command
    ├── TinoxImportWizard.java          # Import Existing Tinox Project
    ├── TinoxImportWizardPage.java      # ...its one page
    ├── TinoxProjectNature.java         # Marks a project as Tinox
    ├── TinoxSourceFolderDecorator.java # src/ + tests/ icon
    └── TinoxToml.java                  # Minimal tinox.toml reader
```
