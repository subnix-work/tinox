# Checked references and release documentation

The parser AST owns source syntax/signatures. `tinox doc` and `tinox reference
<file-or-directory>` project the same documentation model; the JSON export
also includes enum variants and trait declarations. The type checker's symbol
table owns builtin diagnostic signatures. `AnnotationProcessor` owns annotation
targets, argument-count bounds and descriptions. Neither the export nor the
script registers a second set of types or annotations. The LSP continues to
consume compiler parser/typechecker data; no new editor-specific registry is
introduced. `tinox describe` queries resolved APIs through the compiler dependency graph;
its annotation queries use the same `AnnotationProcessor` registry.

`docs/SPEC.md` and the translated HTML explanations supply human context.
`CLAUDE.md` contains project conventions and historical debugging notes; it
is not the compact language reference. Historical package docs describe their
published versions and are never regenerated from current source.

## Current references

With Rust, Python 3.11+, LLVM/clang and the usual runtime development libraries:

```sh
make docs-generate   # rebuild compiler, check/run examples, write current outputs
make docs-check      # repeat checks and fail on stale generated files
```

The generated outputs are `docs/AI_GUIDE.md`, `docs/current/reference.json`,
`docs/current/packages.json`, and `docs/current/guide-size.json`. Commit them
with changes to their inputs. `make check` and the existing CI aggregate include
`docs-check`. `docs-stage` is separate because staging creates new version pages.

The guide's template is `docs/guide/guide.md.in`. Example sources and exact
expected stdout are in `docs/guide/examples/` and `docs/guide/examples.json`.
Every canonical example must appear exactly once in the manifest. Generation
and verification both typecheck, compile and run each example, compare stdout,
and execute requested `@Test` methods. These examples have no network, timing
or external-package dependencies. The accessor example uses annotations instead
of handwritten getter/setter methods. Adding another `.tnx` without declaring
its expected behavior fails the gate.

The guide is limited to 8192 UTF-8 bytes. Its byte count and whitespace-delimited
word count are recorded reproducibly; bytes provide a conservative token upper
bound, not a tokenizer-specific estimate. Current compiler and package versions
are recorded separately: package inventory does not assert universal binary or
source compatibility. The canonical examples establish compiler compatibility
for the syntax they actually exercise.

The bilingual inventory check compares module section/navigation multisets and
verifies anchors, including deliberate aliases such as `mod-rest-server`.
It preserves translated prose. It does not claim that every translated sentence
or historical example has been semantically checked.

## Package release preparation

After deliberately bumping a package version:

```sh
make docs-stage          # stage source/manifest/examples and generate absent pages
make docs-generate       # update current package/version inventory
make docs-release-check  # require matching pages and valid dependency links
make check
```

Review and commit the added pages with the version bump before publishing.
`scripts/publish-stdlib-ext.sh` runs the release documentation preflight before
any upload. This change does not publish packages or modify package versions.

Staging copies the package's `tinox/core/<module>/` content into a temporary
project's `src/`, copies its manifest, and copies editable examples from
`docs/tinox-core/<module>/examples/`. `tinox doc` generates the HTML;
`tinox reference` generates a companion source metadata snapshot. Lexing or
parsing errors prevent publication of a partial export. Generation checks local
dependency links before creating the destination version directory. Dependency
pages are staged first; unresolved dependencies or cycles fail explicitly.

Existing version directories are skipped without modification. An incomplete
existing directory is an error; staging will not repair it by overwriting it.
If an earlier page had examples, missing editable example sources block staging.
Historical examples copied into package pages remain explanatory publication
content; only examples in the canonical manifest are guaranteed executable by
this gate. Source API export is a parser-backed reference, not evidence that
every method or every runtime feature was executed.

Use `tinox reference crates/tinox-core-ext/<module>/tinox/core/<module>` for
on-demand current API queries. File keys are relative and deterministic;
source declaration types retain generics, whereas builtin signatures reflect
the diagnostic model's intentional type erasure and runtime variadic exceptions.
The JSON `schema_version` is independent of the compiler/package versions.
