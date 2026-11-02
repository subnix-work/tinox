# Supported service stack and release manifest (#558)

One exact compiler version cannot tell you that a whole set of packages works together. This page defines the
combination Tinox supports for services, and how a release records it.

## The supported stack

`docs/supported-stack.toml` names it: **compiler** + **`http_server`** (HTTP/1.1 REST, `@Service` lifecycle) +
**`json`** + **`db`** on **PostgreSQL** + **`jwt` / `oauth2` / `oidc`** (login and tokens) + **`websocket`**, plus every
package these depend on (`http`, `crypto`, ...). That closure is what the integration matrix exercises
(`service_composition`, `postgres_*`, `oidc_web_sessions`, `request_*`, `executor`, ...) and what a release
manifest pins. Anything outside it (MySQL/SQLite beyond the documented contracts, HTTP/2 and HTTP/3, AMQP, Redis,
Kubernetes) is supported package by package, but not as part of the pinned stack.

## Compatibility and deprecation policy

* **A compiler release and its packages move together.** `[package] compiler = "x.y.z"` must equal the running
  compiler; the compiler rejects anything else. Every stack package is therefore re-released (patch bump) with each
  compiler version.
* **A published version is immutable.** Changing a package's sources requires a new version, a new immutable docs
  page (`make docs-stage`) and a new manifest. `scripts/release_manifest.py verify` fails on a changed archive
  without a version bump and on an edited docs page.
* **Within one compiler version**, stack packages follow semver: a patch release does not change public signatures;
  a minor release only adds; a major release may remove what was deprecated.
* **Deprecation:** a feature is marked deprecated in the changelog and docs for at least one compiler release before
  removal, with the replacement named. Removal happens only in a compiler release with a "Notable change" entry.
* **Old releases stay installable.** Published archives and their docs are never deleted or rewritten.

## The release manifest

`docs/releases/<compiler>.json` (written by `make release-manifest`) records, deterministically:

| Field | Meaning |
|---|---|
| `compiler.version`, `runtime_c_sha256`, `runtime_h_sha256`, `cargo_lock_sha256` | the compiler build inputs that decide runtime behavior |
| `compiler.builder_apt_packages` | native packages of the pinned builder image (`docker/builder/Dockerfile`) |
| `compiler.source` | informational: commit and whether the tree was clean (not compared) |
| `packages[]` | per stack package: version, `content_sha256` of the exact release archive (gzip timestamps ignored), `docs` page and its hash, dependency pins |

Checks (`scripts/release_manifest.py verify`, part of `make docs-release-check`):

* the tree matches the manifest: each difference is reported with the action to take (bump the version, stage docs,
  regenerate the manifest, restore an edited docs page);
* `--strict` also compares the compiler/runtime inputs;
* `--registry` downloads every recorded archive from tinox-central and compares file contents, so a published stack can
  be shown to be reproducible from recorded artifacts. Run it after publishing, before announcing a release.

`tests/release_manifest_test.py` (part of `make docs-check`) covers the comparison logic and fails when the committed
manifest is out of date with the tree, so a package edit cannot slip through without a version bump.

## Releasing

```sh
make docs-stage docs-generate        # new immutable docs pages, regenerated references
python3 scripts/publish-stdlib.py    # compile a consumer of every module from the exact archives
python3 scripts/publish-stdlib.py --publish   # needs TINOX_CENTRAL_ADMIN_KEY
make release-manifest                # write docs/releases/<compiler>.json
python3 scripts/release_manifest.py verify --strict --registry
```

Fresh-cache consumers: `publish-stdlib.py` already builds each module as a consumer from the extracted archives with
local overrides; `--registry` closes the loop by proving the registry serves the same bytes.

## Limits

* The manifest records inputs; it does not make builds byte-identical (see "Reproducibility guarantee" in
  `docs/docker.md`).
* The stack's integration matrix is the existing test suite; there is no separate "stack acceptance" job yet.
* A stale compiler/package combination is rejected by the existing compiler-version check at `tinox install` and
  build time; the manifest adds the whole-stack view on top.
