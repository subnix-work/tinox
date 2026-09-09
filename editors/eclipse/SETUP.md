# Tinox Eclipse Plugin — Setup Guide

This guide explains step by step how to set up and use the Tinox Eclipse Plugin.

**Just want to install and use it, not develop it?** Skip to [Exporting
the plugin as a `.jar`](#exporting-the-plugin-as-a-jar-for-distribution)
near the bottom — `./build.sh` builds a real, installable plugin JAR from
the command line, no PDE project import or GUI export wizard needed. The
step-by-step walkthrough below (Steps 1-6) is the live "Run As → Eclipse
Application" development workflow, useful if you're changing the
plugin's own code.

---

## Prerequisites

| What | Version |
|-----|---------|
| Eclipse IDE (Java ≥ 17) with [LSP4E](https://download.eclipse.org/lsp4e/releases/latest/) + [TM4E](https://download.eclipse.org/tm4e/releases/latest/) | any recent release; most "for Java/C++/... Developers" packages already include both |
| Java | ≥ 17 |
| Rust / Cargo | ≥ 1.75 |

For plugin *development* specifically (Steps 3-4 below, not the `build.sh`
path): an Eclipse IDE for Plugin Development (≥ 2023-09) package, since
PDE itself is also needed there.

---

## Step 1: Install the tinox-lsp binary

The Eclipse plugin communicates with the `tinox-lsp` language server. It must be built and installed first.

```bash
# In the repo's root directory:
./editors/install-lsp.sh
```

The script builds `tinox-lsp` in release mode and copies the binary to `~/.cargo/bin/tinox-lsp`.

**Manually (alternative):**
```bash
cargo build --release -p tinox-lsp
cp target/release/tinox-lsp ~/.cargo/bin/tinox-lsp
```

---

## Step 2: Install LSP4E + TM4E in Eclipse

LSP4E is the framework that connects Eclipse to language servers; TM4E
renders the plugin's TextMate grammar for syntax highlighting. Both are
required (`MANIFEST.MF`'s `Require-Bundle` lists both) — most Eclipse
"for Java/C++/... Developers" packages already ship with them, so check
`Help → About Eclipse IDE → Installation Details → Plug-ins` (search for
"LSP4E"/"TM4E") before installing anything.

If either is missing:
1. Open Eclipse
2. **Help → Install New Software…**
3. Enter under "Work with":
   ```
   https://download.eclipse.org/lsp4e/releases/latest/
   ```
   (or `https://download.eclipse.org/tm4e/releases/latest/` for TM4E)
4. Select **"Language Server Protocol client for Eclipse"** (or "TM4E")
5. Next → Finish → restart Eclipse

---

## Step 3: Import the plugin into Eclipse

1. **File → Import…**
2. **General → Existing Projects into Workspace** → Next
3. Root directory: choose the path to the `editors/eclipse/tinox-eclipse` folder in the repo
4. `tinox-eclipse` should appear in the list → **Finish**

---

## Step 4: Start the plugin

1. In the Package Explorer: right-click `tinox-eclipse`
2. **Run As → Eclipse Application**
3. A second Eclipse window opens — that's the test instance with the plugin

---

## Step 5: Testing

In the second Eclipse window:

1. **File → New → Project → General → Project** → Finish
2. Create a new file: right-click the project → **New → File**, name: `test.tnx`
3. Enter the following:

```tinox
fn add(a: Int64, b: Int64) -> Int64 {
    return a + b;
}

fn main() -> Int64 {
    let x = add(1, 2);
    return x;
}
```

**What you'll see now:**

| Action | Result |
|--------|----------|
| Introduce a typo | Red underline appears |
| Cursor on `add` in line 6 | **Hover** shows `fn add(a: Int64, b: Int64) -> Int64` |
| Ctrl+Space | **Autocomplete** opens with keywords, builtins, functions |
| F3 on `add` | **Go to Definition** jumps to the function declaration |
| Window → Show View → Outline | **Outline view** shows all functions and classes |

---

## Step 6 (optional): Configure the binary path

If `tinox-lsp` isn't located at `~/.cargo/bin/tinox-lsp`:

1. **Window → Preferences → Tinox**
2. Enter the path to the `tinox-lsp` binary
3. OK → restart Eclipse

---

## Troubleshooting

**Language server doesn't start**
- Check whether the binary is executable: `ls -la ~/.cargo/bin/tinox-lsp`
- Check the path in Preferences (step 6)
- Look in **Window → Show View → Other → Language Servers** for errors

**No error underlines**
- The file extension must be `.tnx` (not `.tnx`)
- Wait a moment — the server needs 1-2 seconds on its first start

**`install-lsp.sh` fails**
- Make sure `cargo` is on the PATH: `which cargo`
- Build manually: `cargo build --release -p tinox-lsp`

---

## Exporting the plugin as a `.jar` (for distribution)

No Eclipse development environment needed for this — `build.sh` compiles
the plugin from the command line against whatever Eclipse installation's
bundle pool it finds:

```bash
cd editors/eclipse
./build.sh
```

Auto-detects the bundle pool to compile against (an Eclipse-Installer
-provisioned install's shared pool at `~/.p2/pool/plugins`, or a
traditional all-in-one install's own `plugins/` directory); override with
`ECLIPSE_PLUGINS_DIR=/path/to/plugins ./build.sh` if it guesses wrong, or
if the bundles listed in Step 2 above live in a different Eclipse
installation than the one you'll actually install the result into.

Prints the built JAR's path, e.g. `dist/tinox.eclipse_1.0.0.<timestamp>.jar`
(the `.qualifier` placeholder in the checked-in `MANIFEST.MF` gets
replaced with a real build timestamp). Copy it into the `dropins/` folder
of the Eclipse installation you actually want to use the plugin in, and
restart Eclipse:

```bash
cp dist/tinox.eclipse_*.jar <eclipse-install-dir>/dropins/
```
