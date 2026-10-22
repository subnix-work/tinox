# Compiler diagnostics protocol (version 1)

`tinox check --json [file]` and `tinox build --json [file]` emit exactly one
JSON object on stdout. `--error-format=json` and `--error-format json` are
aliases; `--error-format=text` selects the default human-readable output.
Flags can precede or follow the input path. Existing build flags, including
`-o`, `--debug`, `--release`, `--checked`, and `--force`, remain available.

```json
{
  "schema_version": 1,
  "success": false,
  "diagnostics": [
    {
      "code": "TINOX-TYPE-MISMATCH",
      "severity": "error",
      "category": "type",
      "file": "/project/Helper.tnx",
      "range": {
        "start": { "offset": 43, "line": 1, "column": 44 },
        "end": { "offset": 50, "line": 1, "column": 51 }
      },
      "message": "expected Int64, found String",
      "expected": "Int64",
      "actual": "String",
      "related": [],
      "notes": []
    }
  ]
}
```

The example illustrates the wire shape; ranges refer to the actual source in
a real response. Parse the complete stdout as JSON, without stripping terminal
escapes, prefixes, or progress lines. Warnings, routes, cache traces, timing,
and native tool progress go to stderr. On a successful check or build,
`success` is `true` and `diagnostics` is empty. No executable is run by `build`.

Both text and JSON modes return exit status **0** on success and **1** on a
reported compiler, input, or toolchain failure. An unexpected compiler panic
retains status **101** and the Rust panic report on stderr; abrupt termination
and panics are outside the completed-response protocol.

## Diagnostic fields

- `code` is a stable identifier, independent of English message wording.
- `severity` is currently `error`. Nonfatal warnings remain on stderr in v1.
- `category` identifies the originating phase or failure class (see below).
- `file` names the originating source, including imported sources. Existing
  paths are canonical absolute filesystem paths. Unreadable paths retain the
  supplied spelling; failures without a known source use `null`.
- `range` uses **one-based lines and columns**, measured in Unicode scalar
  values, with a **zero-based scalar offset** and an exclusive end. These are
  compiler coordinates, not UTF-8 byte offsets or LSP positions. When no source
  range is known, it is `null`; consumers must not invent a location from it.
- `message` is explanatory text and may evolve. Do not parse it for identity.
- `expected` and `actual` carry type names for typed mismatches when available;
  otherwise they are `null`. They reflect the compiler's semantic type model
  (for example, integer widths currently share the `Int64` semantic type).
- `related` is an array of `{file, range, message}` secondary locations,
  used, for example, to identify the other component in an annotation port
  collision. It is empty when there is no secondary location.
- `notes` contains contextual advice, such as the explicit-import requirement.

No suggested edits are emitted in version 1: these diagnostics do not yet
establish mechanically safe replacements.

## Stable codes and categories

| Code | Category | Meaning |
| --- | --- | --- |
| `TINOX-LEX` | `lex` | Tokenization failure |
| `TINOX-SYNTAX` | `syntax` | Parse failure |
| `TINOX-SOURCE-LAYOUT` | `source-layout` | File/type/namespace layout violation |
| `TINOX-IMPORT` | `import` | Import target cannot be resolved |
| `TINOX-TYPE` | `type` | General semantic checking failure |
| `TINOX-TYPE-MISMATCH` | `type` | Typed expected/actual mismatch |
| `TINOX-ANNOTATION` | `annotation` | Invalid annotation or program wiring |
| `TINOX-ENTRY` | `entry` | Missing executable entry point |
| `TINOX-CODEGEN` | `codegen` | Error reported during source lowering |
| `TINOX-LLVM-VERIFIER` | `internal` | Generated LLVM IR failed verification; compiler defect |
| `TINOX-INTERNAL` | `internal` | Compiler-generated source/setup failure |
| `TINOX-TOOLCHAIN` | `toolchain` | Native tool invocation, compilation, or linking failed |
| `TINOX-IO` | `io` | Source file could not be read |
| `TINOX-DRIVER` | `driver` | CLI, entry discovery, or build artifact management failure |

`check` and `build` share parsing, source validation, import resolution,
visibility/type checking, and program annotation validation. `build` additionally
requires an executable entry point and runs lowering and the native backend;
`check` remains usable for files without `Main`.

## Language server

The LSP converts the same common diagnostic records into LSP diagnostics.
`Diagnostic.code` carries the stable compiler code, and `Diagnostic.data`
contains `{schema_version: 1, diagnostic: <the common record>}`. The LSP's
public `range` converts scalar columns into zero-based UTF-16 positions; the
embedded record retains compiler coordinates. The language server analyzes
the edited buffer with its preludes; it does not run the executable backend.

## Compatibility

Consumers must check `schema_version`, ignore unknown object fields, and
handle unrecognized codes/categories as general errors. Additive fields and
new codes can be introduced within version 1. Removing/renaming fields,
changing coordinate units, or changing an existing code's meaning requires a
new schema version. Text output is intended for people and is not a protocol.
