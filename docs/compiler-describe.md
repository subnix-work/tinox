# Compact semantic API descriptions

`tinox describe` answers API questions using the compiler's current source and
dependency resolution. It does not read a bundled copy of an extended package,
download dependencies, or maintain a separate API catalog.

```sh
tinox describe MyService
tinox describe model.Box --member map
tinox describe 'model.Box::map' --signatures-only
tinox describe tinox.core.array.Arrays --member containsInt --json
tinox describe '@GET'
tinox describe Person --file src/Person.tnx --limit 10
```

Quote symbols containing `@` or `::` as required by your shell.

## Resolution and checking

By default, application queries start at the entry selected by `tinox.toml`,
using the same `entry`/`src/Main.tnx`/`src/main.tnx` rules as `check` and `build`.
`--file <entry.tnx>` selects another source, including a library source without
`Main`. The compiler resolves imports, implied imports and namespace siblings,
checks file layout and explicit imports, synthesizes accessors, and typechecks
the resulting program. Failures are reported rather than describing an API
that failed to resolve or typecheck.

A short name searches that resolved graph. A qualified name first searches the
graph, then resolves the name as an import using the compiler's normal local,
installed-package and core-library rules. Thus `tinox.core.array.Arrays` works
without a project, while an extended package such as `tinox.core.crypto.Crypto`
must be installed and resolvable in the selected project. The package coordinate
and source path in the result identify the exact installed source. Conflicting
installed sources fail under the compiler's usual ambiguity rules. Local package
overrides are supported whenever the compiler resolver supports them; this
command does not introduce an independent override mechanism.

Built-in `@Annotation` queries read the validator's targets, argument counts and
description. Project-defined annotations use the same registration rules as the
validator, including a source definition overriding a built-in name. Annotation
arguments are described to the extent the validator records them: arity and
its explanatory text. The command does not invent parameter names or argument
types that the registry does not specify.

## Output and bounds

The default text view includes the canonical signature, qualified name, source,
required import, package coordinate when applicable, documentation, actual
annotation usages, and declared members. Class and method type parameters,
callback signatures, integer widths, references, nullable and composite types
retain declaration-level detail. Backends' erased type markers are not used as
public signatures. Fields expose visibility; static methods returning their
own class are marked as factories in JSON. Constructor/factory methods are
shown when declared; an implicit constructor contract is not fabricated.

`--member <name>` or `Symbol::member` selects declared members with that name.
`--signatures-only` omits documentation and contextual text. `--limit N` limits
members to 1–200 (default 40); the number omitted is always reported. Each
included documentation string is limited to 1,200 Unicode scalar values and
has an explicit truncation flag. Bounds apply to both output formats.

The member scope is **declared**: inherited APIs are identified through
`extends`/`implements`; query those types for their declarations. Compiler-
generated accessors are included and marked `generated: true`, with `range:
null` because a synthetic source range would not point into the actual file.
Source declarations retain compiler spans. Private and protected members remain
visible in the description with their actual visibility; their presence does
not grant access to them.

No method bodies are printed. Documentation and annotation usages come from
resolved source. The command does not generate unverified usage examples or
copy unrelated example programs into a response.

## JSON protocol, version 1

`--json` writes exactly one JSON object to stdout. Progress and timing remain on
stderr. Success returns status 0; invalid flags, unknown/ambiguous symbols,
unavailable members, missing dependencies and compiler failures return status 1.
Text and JSON report the same query outcome.

A successful response has:

```json
{
  "schema_version": 1,
  "kind": "symbol_description",
  "success": true,
  "symbol": { "qualified_name": "model.Box", "members_scope": "declared" },
  "annotation": null,
  "checked": true,
  "signatures_only": false
}
```

The abbreviated `symbol` above also contains `name`, `namespace`, `kind`,
`signature`, `source`, `required_import`, nullable `package`, `type_parameters`,
`extends`, `implements`, nullable `documentation`, `documentation_truncated`,
`annotations`, `range`, `members`, and `omitted_members`.

Members contain `name`, `kind` (`field`, `method`, `factory` or `variant`),
`signature`, `visibility`, `is_static`, `type_parameters`, `parameters`, nullable
`return_type`/`type_name`, documentation/truncation, annotations, `generated`,
and nullable `range`. Parameters contain `name`, `type_name`, and annotations.
Ranges use the compiler's one-based lines/columns and zero-based Unicode scalar
offsets. End positions are exclusive; declaration spans can be point spans.

An annotation result contains `name`, `valid_targets`, `min_args`, nullable
`max_args` (`null` means unbounded), and `description`. A built-in has `symbol:
null`; a project annotation additionally includes its source symbol. `checked`
means the source graph was typechecked, or the built-in metadata came directly
from the compiler validator.

Failure responses contain `error: {code, message, candidates}` in place of the
success payload. `candidates` contains `{qualified_name, source, required_import}`
records for symbol ambiguity and is otherwise empty. Codes are:

- `TINOX-DESCRIBE-UNKNOWN`: unknown symbol.
- `TINOX-DESCRIBE-AMBIGUOUS`: multiple symbol declarations match.
- `TINOX-DESCRIBE-MEMBER`: no matching declared member.
- `TINOX-DESCRIBE-RESOLUTION`: qualified import resolution failed.
- `TINOX-DESCRIBE`: invalid command arguments or a compiler graph failure.

This is the **symbol-description** protocol. The compiler diagnostics protocol
introduced separately by #297 describes compiler failures rather than API
records. Consumers should inspect `kind` and `schema_version`, ignore unknown
fields, and handle new error codes as failures. Removing or renaming fields,
changing coordinate units, or changing existing code meanings requires a new
schema version.

The signature/type formatting is shared with LSP hover/completion and generated
reference documentation. LSP annotation hover reads validator target and arity
metadata as well as its richer explanatory documentation.
