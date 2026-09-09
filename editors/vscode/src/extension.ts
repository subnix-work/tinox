// Tinox language support for VS Code -- a thin LSP client over the
// tinox-lsp binary, plus a "Run Tinox File" command. Mirrors
// editors/eclipse/tinox-eclipse's own TinoxLanguageServer.java /
// RunTinoxHandler.java exactly (same binary-path setting, same
// auto-detect probe order, same tinox-lsp -> tinox path derivation for
// the run command) -- see CLAUDE.md's "Editor Support" section for why.

import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import * as vscode from "vscode";
import { LanguageClient, LanguageClientOptions, ServerOptions } from "vscode-languageclient/node";

let client: LanguageClient | undefined;
let runOutputChannel: vscode.OutputChannel | undefined;

/** Same probe order as TinoxPreferenceInitializer.java. */
function defaultLspPath(): string | undefined {
  const candidates = [
    path.join(os.homedir(), ".cargo", "bin", "tinox-lsp"),
    "/usr/local/bin/tinox-lsp",
    "/usr/bin/tinox-lsp",
  ];
  for (const candidate of candidates) {
    if (fs.existsSync(candidate)) {
      return candidate;
    }
  }
  return undefined;
}

function resolveLspPath(): string {
  const configured = vscode.workspace.getConfiguration("tinox").get<string>("lsp.path");
  if (configured && configured.trim().length > 0) {
    return configured;
  }
  // Falls back to the first cargo-bin candidate even if it doesn't exist
  // yet, same as TinoxPreferenceInitializer.java -- the resulting spawn
  // failure is a clearer signal ("no such file") than silently doing
  // nothing.
  return defaultLspPath() ?? path.join(os.homedir(), ".cargo", "bin", "tinox-lsp");
}

/** Same derivation RunTinoxHandler.java uses: swap the binary name in
 * the configured/resolved tinox-lsp path -- both binaries are assumed to
 * live in the same directory (true for a cargo install/build output). */
function resolveTinoxBinaryPath(): string {
  return resolveLspPath().replace(/tinox-lsp(?=[^/\\]*$)/, "tinox");
}

function startLanguageClient(): void {
  const command = resolveLspPath();
  const serverOptions: ServerOptions = { command, args: [] };
  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: "file", language: "tinox" }],
  };
  client = new LanguageClient("tinox", "Tinox Language Server", serverOptions, clientOptions);
  client.start().then(undefined, (err) => {
    vscode.window.showErrorMessage(
      `Failed to start tinox-lsp at "${command}": ${err}. ` +
        `Run editors/install-lsp.sh, or set "tinox.lsp.path" in Settings.`
    );
  });
}

function runCurrentFile(): void {
  const editor = vscode.window.activeTextEditor;
  if (!editor || editor.document.languageId !== "tinox") {
    vscode.window.showWarningMessage("No Tinox file is active.");
    return;
  }
  const filePath = editor.document.uri.fsPath;
  const tinoxBinary = resolveTinoxBinaryPath();

  if (!runOutputChannel) {
    runOutputChannel = vscode.window.createOutputChannel("Tinox");
  }
  runOutputChannel.clear();
  runOutputChannel.show(true);
  runOutputChannel.appendLine(`Running: ${tinoxBinary} run ${filePath}`);

  const cp = require("child_process") as typeof import("child_process");
  const proc = cp.spawn(tinoxBinary, ["run", filePath]);
  proc.stdout.on("data", (data: Buffer) => runOutputChannel!.append(data.toString()));
  proc.stderr.on("data", (data: Buffer) => runOutputChannel!.append(data.toString()));
  proc.on("error", (err) => {
    runOutputChannel!.appendLine(`\nFailed to start tinox: ${err.message}`);
    runOutputChannel!.appendLine(`Binary path: ${tinoxBinary}`);
  });
  proc.on("close", (code) => runOutputChannel!.appendLine(`\nProcess exited with code ${code}`));
}

export function activate(context: vscode.ExtensionContext): void {
  startLanguageClient();
  context.subscriptions.push(vscode.commands.registerCommand("tinox.run", runCurrentFile));
}

export function deactivate(): Thenable<void> | undefined {
  return client?.stop();
}
