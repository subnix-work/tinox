# Core package releases

The compiler bundles sources from `../tinox/core/<module>/`. These manifests
version the standalone copies on tinox-central; they do not affect bundled
module resolution or duplicate the source tree. Only modules already listed
on tinox-central have standalone release metadata here.

For a compiler migration, bump the affected versions here and under
`crates/tinox-core-ext`, set their exact `[package].compiler`, and update
dependency versions together. Restore any missing example sources from the
historical documentation into `docs/tinox-core/<module>/examples/`.

From the repository root:

```sh
cargo build --release -p tinox
make docs-stage docs-generate
python3 scripts/publish-stdlib.py
# Set TINOX_CENTRAL_ADMIN_KEY in the environment before publication.
python3 scripts/publish-stdlib.py --publish
python3 scripts/publish-stdlib.py --verify-only
make check
```

Preparation compiles and runs a consumer of every public module from the exact
archives, including HTTP/3 (requires the optional ngtcp2/nghttp3 libraries).
Publication orders dependencies first, includes the manifest with its compiler
version, and downloads each published artifact to compare every file. Existing
versions are accepted only when their complete file contents match the release.
Archives contain only the module sources and manifest, excluding tests and
examples. `scripts/publish-stdlib-ext.sh` remains an extended-tier entry point.
`scripts/verify-published-stdlib.sh` checks both tiers without authentication.

Commit each new immutable documentation directory with its manifest. Deliver
the documentation through feature → develop → main PRs because tinox-central's
documentation proxy reads `main`. Historical archives and pages stay unchanged.
