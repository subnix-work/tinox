# tinox-lsp: language server capabilities

`tinox-lsp` speaks LSP over stdio (`cargo build --release -p tinox-lsp`, or
`editors/install-lsp.sh`). It analyses with the same lexer, parser and
typechecker as `tinox check`, so diagnostics carry the compiler's codes and
the JSON in `Diagnostic.data` (schema in [compiler-diagnostics.md](compiler-diagnostics.md)).

| Capability | Notes |
|---|---|
| `publishDiagnostics` | Exact ranges (UTF-16), `code`, `source = tinox`, severity, `relatedInformation`. Re-analysis starts 120 ms after the last edit; superseded results are dropped, late older versions ignored, diagnostics are cleared on close. Open (unsaved) buffers of imported project files are used, and open documents that import a changed file are re-analysed. |
| `hover`, `definition`, `documentSymbol`, `completion` | Syntactic, per file plus standard library. |
| `references` | Project-wide, token based. For members the receiver type is unknown, so every same-named member is reported. |
| `prepareRename` / `rename` | Project-wide. Refused, with a reason, when it cannot be proven safe: ambiguous or shadowed names, members declared by several types, names that also exist on standard-library or builtin types, fields of classes with member-generating annotations (`@Getter`, ...), `extern fn`. Renaming a type also renames its file (`RenameFile` when the client supports it, a visible warning otherwise). |
| `workspace/symbol` | Fuzzy search over types, functions, methods and fields of the project. |
| `formatting` / `rangeFormatting` | Identical to `tinox fmt` (editor tab options are ignored: the formatter is opinionated). Returns line-level edits; a file with syntax errors is never touched (`RequestFailed` with the reason). |
| `semanticTokens` (`full`, `full/delta`, `range`) | Types, members, locals, parameters, enum members, type parameters, decorators, namespaces; modifiers `declaration`, `readonly`, `static`, `defaultLibrary`. Types declared in other files are coloured by naming convention. |
| `signatureHelp` | Triggers `(` and `,`. `Type::name(` and project functions resolve exactly; `expr.name(` offers every candidate. |
| `codeAction` | Quick fix `Import <path>` for "unresolved ... (missing import?)", and `source.organizeImports`. The compiler emits no suggested edits (schema v1), so these are derived in the server. |

## Limits worth knowing

* No type information at the LSP layer: member navigation is name based and
  conservative (see rename above).
* Columns are UTF-16 units on the wire. The compiler counts Unicode scalar
  values; conversion happens at the boundary.
* The project is every `.tnx` file below the nearest `tinox.toml`
  (hidden directories, `target/` and `node_modules/` are skipped, at most
  5000 files).
