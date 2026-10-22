# Testing unpublished packages

Declare ordinary exact-version dependencies in `tinox.toml`. To select a local
package for development, create `tinox.overrides.yaml` beside that manifest:

```yaml
overrides:
  "tinox.core:json:1.0.0": "../tinox/crates/tinox-core-ext/json"
```

Paths are relative to the consumer's manifest directory; absolute paths also
work. They point to a package root containing `tinox.toml`, with source under
`src/` or directly under the root. The namespace layout must contain
`tinox/core/json/` (or `tinox/core/json.tnx`). Package name and version must match
the coordinate; `package.group`, when present, must also match. Older stdlib
manifests omit `group`; their namespace layout establishes it.

Run `tinox install`, then your normal `tinox check`, `build`, `run`, `test` or
`describe` command. No download or copy is made for an overridden dependency.
An edit to the selected source is used by the next compilation, including
build-cache invalidation. `tinox sources` prints selected directories and the
coordinates and paths of mutable local sources. The LSP uses the same resolver,
reloads local sources on document updates, and reports their provenance as
informational diagnostics. Completion members also follow selected source types.

Overrides apply to direct and transitive dependencies. Only the root consumer's
configuration is read: a dependency cannot impose its own override file. Cycles
are visited once. Requesting another version of an overridden package is a hard
error; update the consumer/dependency manifests explicitly to a consistent set.
Missing paths, mismatched identities and invalid namespace layouts fail without
falling back to an installed copy. Unused overrides are validated but do not add
dependencies or make undeclared packages available.

`tinox.lock` records local coordinates in `local_sources`, without absolute paths
or artifact hashes. These sources are mutable, not reproducible artifact pins.
Existing immutable artifact pins remain available for when overrides are removed.
A successful `tinox install` refreshes the local-source records. The override file
is ignored by Git. `tinox package` and `tinox publish` reject active overrides;
remove the file, install and test published dependencies before distributing a
package. Overrides are never included in package archives or dependency manifests.
Without this file, ordinary installed-package resolution is unchanged.

## Extended stdlib lanes

`make workspace-stdlib` runs the stdlib smoke inventory against unpublished
workspace modules. It stages a temporary copy of the extended source tree and
aligns dependency versions in **staged manifests only** to the current workspace
versions. This makes source compatibility testable even when checked-in manifests
still pin older immutable releases. Every staged module is selected explicitly
through the consumer override file. Sources and manifests in the repository and
installed caches are left untouched.

CI runs this as a separate `workspace-stdlib` lane. The existing `test` lane keeps
its real registry install/archive behavior and the original checked-in dependency
pins. Both lanes must pass: workspace compatibility does not prove published
artifact compatibility, and registry tests do not prove local edits work.

For an individual unpublished module test, create a temporary consumer with the
module in `[[dependencies]]`, add the override above, and copy its namespace-based
test fixture into that consumer's `tests/` tree. Run `tinox test` on the copied
fixture. Add explicit transitive overrides for other locally edited packages.
