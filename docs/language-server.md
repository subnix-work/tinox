# tinox-lsp: language server capabilities

`tinox-lsp` speaks LSP over stdio (`cargo build --release -p tinox-lsp`, or
`editors/install-lsp.sh`). It analyses with the same lexer, parser and
typechecker as `tinox check`, so diagnostics carry the compiler's codes and
the JSON in `Diagnostic.data` (schema in [compiler-diagnostics.md](compiler-diagnostics.md)).

| Capability | Notes |
|---|---|
| `publishDiagnostics` | Exact ranges (UTF-16), `code`, `source = tinox`, severity, `relatedInformation`. Re-analysis starts 120 ms after the last edit; superseded results are dropped, late older versions ignored, diagnostics are cleared on close. Open (unsaved) buffers of imported project files are used, and open documents that import a changed file are re-analysed. |
| `hover`, `definition`, `documentSymbol`, `completion` | Syntactic, per file plus standard library. |
| `references` | Project-wide, token based. A member name declared by several types is narrowed with the checker's resolved receiver types (issue #554): accesses and declarations of unrelated types are dropped, overrides and calls through a base type stay (same class or ancestor/descendant). Accesses whose receiver the checker could not resolve are still reported. |
| `prepareRename` / `rename` | Project-wide. Refused, with a reason, when it cannot be proven safe: ambiguous or shadowed names, members declared by several types unless the checker resolved every access (a single unresolved receiver, e.g. an untyped lambda parameter, refuses the rename and names the first location), names that also exist on standard-library or builtin types, fields of classes with member-generating annotations (`@Getter`, ...), `extern fn`. Renaming a type also renames its file (`RenameFile` when the client supports it, a visible warning otherwise). |
| `workspace/symbol` | Fuzzy search over types, functions, methods and fields of the project. |
| `formatting` / `rangeFormatting` | Identical to `tinox fmt` (editor tab options are ignored: the formatter is opinionated). Returns line-level edits; a file with syntax errors is never touched (`RequestFailed` with the reason). |
| `semanticTokens` (`full`, `full/delta`, `range`) | Types, members, locals, parameters, enum members, type parameters, decorators, namespaces; modifiers `declaration`, `readonly`, `static`, `defaultLibrary`. Types declared in other files are coloured by naming convention. |
| `signatureHelp` | Triggers `(` and `,`. `Type::name(` and project functions resolve exactly; `expr.name(` offers every candidate. |
| `codeAction` | Quick fix `Import <path>` for "unresolved ... (missing import?)", and `source.organizeImports`. The compiler emits no suggested edits (schema v1), so these are derived in the server. |

## Limits worth knowing

* Member navigation is token based. Only when a member name is declared by
  several project types does it ask the compiler's own checker (resolved calls
  and field accesses, run per file with the other project files as
  declarations, open buffers included) which class each `x.member` reaches;
  it is not a second analyzer. Unresolved accesses stay conservative: they are
  listed by `references` and refuse a rename. Cost: one checker run per file
  that mentions the name, per request (not cached). Receivers of `a.b.c`
  chains and generic receivers are only as precise as the checker's own result.
* Columns are UTF-16 units on the wire. The compiler counts Unicode scalar
  values; conversion happens at the boundary.
* The project is every `.tnx` file below the nearest `tinox.toml`
  (hidden directories, `target/` and `node_modules/` are skipped).
* **Scan budget (issue #555):** at most 5000 files and 256 MiB of source are
  indexed by default; set `TINOX_LSP_MAX_FILES` / `TINOX_LSP_MAX_BYTES` in the
  server's environment to change that (an unusable value keeps the default and
  is reported with a warning). Open documents are always indexed. The rest is
  taken in path order, so the indexed set is stable between requests. When
  files are left out the index is *partial*: `rename` and `prepareRename` are
  refused with the number of skipped files (an unseen file could reference the
  symbol), while references, definitions and workspace symbols still answer
  but show one warning ("workspace index incomplete: N of M files ...").
* Unchanged files (same modified time and length) are served from an in-memory
  cache instead of being read again; the project is still re-listed and
  re-tokenized per navigation request, which measured 20-35 ms for 6000 small
  files (dense and deep layouts, `large_workspace_timings` in
  `crates/tinox-lsp/tests/navigation.rs`). A cached token index and file
  watching are not implemented.
