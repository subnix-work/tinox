---
name: release
description: Cut a Tinox compiler release (X.Y.Z) end to end - version bump, stdlib publish, release PR to main, tag, back-merge, GitHub release, jgrep-tinox, AUR packages, tinox-ide. Use when the user asks to release/tag a new compiler version.
---

# Tinox release process

Derived from release 2.9.6 (2026-11-18). Follow the order; each step has a reason.
Tell the user which step you are on. Pushing and publishing need the user's
go-ahead unless they authorized the whole chain. The tinox-central admin key is
given by the user in chat: pass it only as `TINOX_CENTRAL_ADMIN_KEY` for the one
command, never write it to a file, commit or memory.

## 0. Preconditions
- Feature work is merged to `develop` (PR from `feature/*`, CI green).
- `main` and `develop` are protected: EVERY merge, including the back-merge, is a PR.
- Open a GitHub issue for the feature (English, label `enhancement`), close it when merged.

## 1. Release branch (cut from `develop`)
`git checkout -b release/X.Y.Z origin/develop`, then:
1. Workspace `version` in `Cargo.toml` (+ `Cargo.lock`).
2. `python3 scripts/set_compiler_version.py` (all `tinox.toml` compiler stamps).
3. Patch-bump every package version in `crates/tinox-core*/**/tinox.toml` (75 packages in 2.9.6)
   and every dependency pin between them. No script exists: write a one-off.
   Also fix hardcoded versions in `crates/tinox/tests/*.rs` and the table in
   `service_composition.rs`; shell scripts may contain an escaped `compiler = \"..\"`
   that the first sed misses (grep for the old version afterwards).
4. `make docs-stage`, `make docs-generate`, `make release-manifest`
   (new `docs/releases/X.Y.Z.json`, `docs/supported-stack.toml`, per-version pages
   under `docs/tinox-core/<module>/<ver>/`). `docs/current/reference.json` must be
   regenerated or `docs-check` fails in CI.
5. `CHANGELOG.md` section `[X.Y.Z]`.
6. `.github/workflows/check.yml` pins a jgrep-tinox commit for the new compiler,
   so release jgrep-tinox first (step 4) and update the pin afterwards.

## 2. Publish stdlib packages BEFORE the release PR
The test suite downloads packages by version; unpublished versions give ~100 tests
failing with HTTP 404.
- `python3 scripts/publish-stdlib.py` (dry run, checks archives offline)
- `TINOX_CENTRAL_ADMIN_KEY=... python3 scripts/publish-stdlib.py --publish`
  (idempotent, content-checked; verify afterwards).

## 3. Verify, then PR to `main`
- `make check` in the background (`nohup ... & disown`, poll the log), 30+ min.
  It stops at the first failing target, so a flaky test can hide a stale `docs-check`.
- Push the branch with `--no-verify` after the green run (the pre-push hook repeats `make check`).
- PR `release/X.Y.Z` -> `main`, wait for CI, merge.
- Annotated tag `vX.Y.Z` on the merge commit, push (`--no-verify`).
- Back-merge PR `main` -> `develop`.

## 4. GitHub release with Linux tarball
Build locally with the release compiler: `tinox-X.Y.Z-linux-x86_64.tar.gz` containing
`tinox`, `runtime.c`, `runtime.h`, `LICENSE-*`, `README.md`, `core/` (copy of
`crates/tinox-core/tinox`). Sanity-test it with core modules only (extended modules
need `tinox install`). `gh release create vX.Y.Z` with the tarball.

## 5. jgrep-tinox (~/git/jgrep-tinox)
Bump `tinox.toml` compiler and version, version string in `src/Main.tnx`, run its tests,
tag `vA.B.C`, GitHub release with two archives. Then update the commit pin in
this repo's `check.yml` (via a follow-up PR if already merged).

## 6. AUR packages (repos in ~/git)
`tinox-bin`, `jgrep-aur`, `jgrep-bin`, `ygrep-aur`, `ygrep-bin`, `tinox-ide-aur`.
Edit `PKGBUILD` (pkgver, checksums, makedepends `tinox>=X.Y.Z`), then
`makepkg -f --nodeps` (new tinox first in PATH) and inspect the package,
`makepkg --printsrcinfo > .SRCINFO`, commit, push `HEAD:master`.
`libfakeroot internal error` during makepkg is harmless. `tinox-bin` also installs `runtime.h`.

## 7. tinox-ide (~/git/tinox-ide)
It pins the compiler exactly, so it needs its own release whenever tinox-bin moves:
compiler stamps in `tinox.toml`, scripts, docs, tests, `check-env` `MIN_TINOX`,
json pin, CHANGELOG, version bump; run its Tinox tests. Three C++ bridge tests
(widget, action, dialog) fail on the maintainer's machine (German UI / KDE), not a regression.

## Traps
- `pkill -f` / `kill $(pgrep -f ...)` kills your own shell: kill by explicit PID.
- `scheduled_jobs` and `blocking_route` are timing-sensitive; rerun before assuming a regression.
- `postgres_transactional` can fail right after a fresh docker pull; rerun.
- Clean up merged branches (local + remote) at the end.
