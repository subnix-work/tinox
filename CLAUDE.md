# Project Conventions for Claude Code

## Bug/Feature Tracking Runs Through GitHub Issues

Since 2026-07-25, **all** bugs and completed feature implementations are
tracked as [GitHub Issues](https://github.com/subnix-work/tinox/issues)
on `subnix-work/tinox` — no longer in local Markdown files
(`bugs.md`/`bugs_fixed.md` were removed, their full content lives 1:1
in issues #1–74).

**Binding rule for every new find/fix from now on:**

- **New bug found** (whether fixed immediately or not): open a GitHub
  issue directly (`gh issue create --repo subnix-work/tinox`).
  Title in the style `Bug NN — short description` (sequential number,
  continuing from the last assigned issue number) or `Feature: Name`
  for completed feature work. Label `bug` or `enhancement`.
- **Bug is fixed:** the issue body contains (as previously used in
  bugs.md/bugs_fixed.md) Status/Root Cause/Fix/Verified — then close the
  issue (`gh issue close <NR> --reason completed`).
- **Bug is still open** (deliberately deferred or unresolved): the issue
  stays open, the body describes the current state + why it's open.
- **Language: English** (title + body) — the issues were deliberately
  translated to English and are meant to stay that way (since
  2026-07-26 this applies project-wide to commit messages and code too,
  see below anyway).
- Cross-references between related issues as previously used between
  bug entries (e.g. "closes what Bug 40 left open" with a link to the
  issue number).

**Looking up history:** search/filter in the GitHub issues (open vs.
closed, label, full text), not in a local file. **Careful:** the old
"Bug NN" number from before the migration does **NOT** reliably match
the same issue number (e.g. "Bug 40" is actually issue #41 — an
in-between note heading without a bug number shifted the count, and
several bugs were sometimes merged into a single issue, e.g. "Bugs
64–65" including the embedded bugs 66–71 as ONE issue). Always search
by title (`gh issue list --repo subnix-work/tinox --state all --search
"Bug 40"`), not by assumed number.

## Branching Model (since 2026-08-13)

Three-tier flow: `main` → `develop` → `feature/*`.

- **`main`** is always the stable, releasable state. Nothing is pushed
  to it directly — it only advances via a merge (PR) from `develop`
  once a batch of features is verified there.
- **`develop`** is the integration branch. Finished features land here
  first, via PR from a `feature/*` branch. This is the default base
  branch for day-to-day work — branch new feature work off `develop`,
  not `main`.
- **`feature/<name>`** branches are cut from `develop` for individual
  pieces of work (e.g. `feature/rest-param-binding`). Merge back into
  `develop` via PR once `make check` is green; delete the feature
  branch after merge.
- Both `main` and `develop` have GitHub branch protection enabled (PR
  required, no direct pushes) — configured via `gh api repos/
  subnix-work/tinox/branches/<branch>/protection`.
- Releasing to `main` = opening a PR `develop` → `main` once `develop`
  is in a shippable state; no separate release-branch tier for now.

## Core Philosophy (distilled from 70+ documented bugs)

- **No silent garbage.** Every error case gets a hard, visible failure
  instead of silent data corruption or a quiet default value. When in
  doubt: abort hard with a clear message instead of "mostly works
  somehow". This is by far the most common root-cause category across
  the whole bug log.
- **Verify against real, independent systems, not just
  self-consistent tests.** Simulated broker/server tests (via
  `spawn`/`await`) are necessary and good, but structurally find NO
  bugs where the implementation is self-consistent but wrong (e.g. bug
  70/71: the `initial-delivery-count` mandatory field and the
  `amqp-value`-vs-`data` encoding were only found through live tests
  against real RabbitMQ and an independent Python client). For
  network/protocol features: whenever at all possible, additionally
  verify against a real, third-party implementation.
- **Fix narrowly, not broadly.** For a found bug, choose the smallest,
  well-scoped fix instead of forcing a larger, riskier rewrite — even
  if the "clean" rewrite would be theoretically appealing. Known,
  documented design limits (open issues) are an acceptable outcome
  when the full fix would be disproportionately invasive.
- **Before tackling an "open" item: check whether a LATER fix already
  closed it.** Several times in the history, an entry said "still
  open" when it had already been resolved by the very next entry in
  the same log (e.g. Bug 35's remaining weakness → fixed in Bug 40; a
  `.toString()` finding in Bug 38 → fixed in Bug 39). Reproduce first,
  then invest time.

## Design preference: annotations over manual boilerplate

When working on tinox — whether extending the language itself or writing
examples/docs — prefer a declarative, annotation-based solution over
hand-written imperative wiring, if both are reasonably possible. This
applies to both directions:

- **Language design:** when adding a new capability that involves
  boilerplate-y setup/wiring (routing, lifecycle hooks, auth/role checks,
  serialization, endpoint registration, etc.), design it as an annotation
  (`@Http3RestController`, `@WebsocketEndpoint`, `@OnOpen`, OIDC role
  guards, …) that generates/wires the code, rather than requiring the
  user to write that plumbing by hand. Existing annotation-driven features
  are the template to follow.
- **Examples:** when writing or updating example code (`examples/**`),
  demonstrate the annotation-based way of doing something rather than the
  manual/imperative equivalent, whenever an annotation for it exists.

Only fall back to the manual/imperative approach when no annotation
exists for the capability yet, the annotation route would be
disproportionately invasive for the size of the change, or the example's
whole point is to show the manual/low-level mechanism itself.

## Build & Test

- `make check` (clippy + unit tests + e2e/matrix/boundary/stdlib_smoke +
  dogfood incl. `jgrep-tinox`) must be fully green before every commit.
  Run it in the background (`nohup ... & disown`, poll the log file),
  don't wait on it blockingly. **Since 2026-08-12, `check` no longer
  depends on the separate `e2e` target** -- it used to (`check: clippy
  test e2e dogfood`), but unrestricted `cargo test --release` (the
  `test` target) already auto-discovers and runs every
  `crates/tinox/tests/*.rs` integration test binary on its own,
  including e2e.rs/matrix.rs/boundary.rs/stdlib_smoke.rs, so those four
  suites were silently running twice on every `make check` -- an
  accidental leftover from e2e.rs originally being a standalone `bash`
  script with no such overlap, never revisited after it was migrated to
  a plain cargo integration test. Measured directly (a dedicated
  timestamped run): this alone was ~35% of total wall time. `e2e` is
  still available standalone (`make e2e`) for quickly iterating on just
  those suites; it's just no longer part of the aggregate gate. A
  failure is a REAL regression by default, not assumed flakiness.
- **Since 2026-08-12, e2e test fixtures bind OS-assigned dynamic ports**
  (`httpServerCreate(0)` + `httpServerBoundPort(fd)`, see the dedicated
  section below) instead of hardcoded literals — port collisions between
  two test files are no longer a thing to check for by hand. If a bind/
  port-related e2e failure still shows up, it's much more likely a
  leftover process from a manually-run `tinox dev`/example session still
  holding a port than an actual collision between two test files (bit
  this project ran into directly: a stale `tinox dev` session sharing
  port 8080's `SO_REUSEPORT` group with a test server made ~half of that
  test's requests silently land on the wrong process and get a 404) --
  `ss -ltnp | grep :<port>` to check for a stray listener before assuming
  a real regression.
- `make asan` (AddressSanitizer, `-DTINOX_NO_GC`) and `make checked`
  (heap-kind registry, `-DTINOX_CHECKED`) are NOT part of `make check`,
  but useful when memory errors/dispatch bugs on the wrong heap-object
  type are suspected — per the Makefile comment, intended for
  weekly/pre-release runs.
- New e2e tests under `tests/e2e/*.tnx` with `// expect:` directives
  (line-by-line comparison of stdout output). For tests that bind a
  port: use `httpServerCreate(0)` + `httpServerBoundPort(fd)` (dynamic),
  not a hardcoded literal — see the dedicated section below for the
  exact pattern, including the one real edge case (rebinding to the same
  port for a reconnect test).
- Tests that use `spawn`/`await` (simulated broker/server via loopback)
  should be run 15–40× repeatedly before being trusted as green — the
  async runtime has had several timing-dependent bugs (Bug 68 among
  others) that only showed up on repeated runs.
- **Since 2026-07-26: commit messages AND code (incl. comments,
  identifiers, doc strings) are in English** — both in this repo and in
  downstream projects like jgrep-tinox. Older commits/comments stay in
  German (not changed retroactively, only new work is affected). The
  previous convention (commit messages in German in the style of the
  old bugs.md entries: Root Cause, Fix, Verified) is thereby replaced —
  the structure/content of the commit message stays the same, only the
  language changes.
- **`docs.html` (German) and `docs_en.html` (English) are deliberately
  maintained as parallel duplicates** — whenever a new `<div
  class="mod-section">` is added to `docs.html` (a new stdlib module),
  ALWAYS also add it to `docs_en.html` (nav link, overview card if
  present, translated module section). This was already out of sync
  for weeks since May once (WebSocket/AMQP-091/AMQP-1.0 were missing
  from the EN version until 2026-07-25) — don't let it happen again.
  Quick check when in doubt: `grep -oE 'id="mod-[a-z0-9_]+"' docs.html
  | sort -u` diffed against the same line for `docs_en.html`, must be
  empty.

## Every tinox-central Publish Needs a Matching Per-Version Doc Page

Whenever a `crates/tinox-core-ext/<module>` (or `tinox-core`) package is
published to tinox-central as a new version (`scripts/publish-stdlib-ext.sh`
or any manual `POST /api/v1/{group}/{artifactId}/{version}`), generate its
`tinox doc` page into `docs/<group-with-dots-as-dashes>/<artifactId>/
<version-with-dots-as-dashes>/docs.html` in THIS repo (e.g. `tinox.core` +
`amqp091` + `1.0.2` → `docs/tinox-core/amqp091/1-0-2/docs.html`) and commit
it alongside the version bump. **Never overwrite an existing version's
docs.html** — old versions stay published and browsable, so their doc page
must stay too; only ADD the new version's directory.

**Why:** tinox-central's frontend (`registry-frontend/.../
DocsProxyResource.java` + `RegistryClient.java` in the `tinox-central` repo)
has no docs of its own — it fetches this exact path from
`raw.githubusercontent.com/subnix-work/tinox/refs/heads/main/docs/...` and
proxies it into the package detail page's iframe. A published version
without a matching doc directory here just 404s in that iframe — this
already happened for 13 modules bumped in the 2026-08-10 core/extended
split's republish (amqp091, amqp10, crypto, http, http2_server,
http3_server, http_server, jwt, oauth2, oidc, rest, websocket, zip all
gained a new version with no doc page to match, silently, since nothing
enforces this link).

**How to generate one:** `tinox doc` only auto-discovers files under a
project's `src/` next to its `tinox.toml` (for the Description/Dependencies
sections) — but `crates/tinox-core-ext/<module>/` has no `src/` layer of
its own (`tinox.toml` sits directly in the module dir; since issue #185's
namespace-mirroring migration, its `.tnx` files live one level further in,
under the module's own `tinox/core/<module>/` subtree, matching the live
archive layout `publish-stdlib-ext.sh` uploads). So stage a throwaway
project first: create a temp dir, copy the module's `tinox.toml` in as-is
and copy the CONTENTS of its `tinox/core/<module>/` subtree — not the
module dir itself, or the `tinox/core/<module>/` prefix would end up
literally inside `src/` — into a `src/` subdirectory (recursively for
multi-directory modules like `rest`'s `tinox/core/rest/client/`/`server/`,
which should land at plain `src/client/`/`src/server/`), then run
`tinox doc --out <path-to-repo>/docs/<group>/<artifactId>/<version>/
docs.html` from inside that staged dir. The Dependencies section is read
straight from the copied `tinox.toml`'s `[[dependencies]]` and links to
`../../<artifactId>/<version>/docs.html` — verify those targets actually
exist (they should, since dependencies are published/versioned first).

**Examples live in `docs/tinox-core/<module>/examples/*.tnx`, NOT inside
`crates/tinox-core-ext/<module>/`.** They used to sit in an `examples/`
dir next to the module's own source pre-split, but `publish-stdlib-ext.sh`
archives every `.tnx` it finds recursively under the module dir straight
into the published package — an `examples/` folder placed there would ship
inside the artifact itself and get pulled into every consumer's import
(and an example's own `class Main` would collide with the consumer's).
Copy from `docs/tinox-core/<module>/examples/` into the staged project's
`examples/` subdirectory before running `tinox doc` (same one-directory-
per-module location regardless of which version you're currently
generating docs for — examples aren't re-versioned per release, only
updated by hand if they go stale against a new version's actual API). As
of 2026-08-10 only the 13 modules bumped that day
(amqp091/amqp10/crypto/http/http2_server/http3_server/http_server/jwt/
oauth2/oidc/rest/websocket/zip) have this `docs/tinox-core/<module>/
examples/` directory restored (recovered by stripping the syntax-
highlighting markup back out of each module's previous docs.html, since
the original example sources were never committed as standalone files,
only their rendered HTML) — the other extended-tier modules' examples are
still only baked into their existing 1.0.0 docs.html with no editable
source anywhere; restore theirs the same way before their next version
bump, or that module's next docs.html will silently lose its Examples
section.

## File Structure: One Class/Interface/Enum per File

Since 2026-07-26 this is hard-enforced at the compiler level (a hard
compile error, not a lint/warning): **every `.tnx` file contains at
most ONE top-level `class`/`interface`/`enum` declaration**, and if it
contains one, the file name MUST exactly (case-sensitively) match the
type name (`class Player` → must be `Player.tnx`). Files with no type
at all (plain `fn`/`main` scripts, e.g. most `tests/e2e/*.tnx`) are
unaffected — the rule is "at most one", not "exactly one".

- **Modules with multiple types become directories.** `import
  tinox.core.amqp10;` (the namespace segment stays unchanged, e.g.
  still lowercase) now resolves to a directory
  `crates/tinox-core/amqp10/` that contains exactly one
  `<TypeName>.tnx` file per type (`Amqp10Connection.tnx`,
  `Amqp10Session.tnx`, …) — ONE `import` statement still pulls in every
  file in the directory, nothing changes for callers. This applies
  uniformly to both stdlib AND project-local imports (`import
  mymodule.foo;` works identically with a `foo/` directory instead of a
  `foo.tnx` file) — resolved in `resolve_imports()`
  (`crates/tinox/src/main.rs`): first `<name>.tnx` (legacy single-file
  case), otherwise `<name>/*.tnx` (all files in the directory merged).
- **Driver/entry-point files (with `main()` or `// expect:`
  directives) keep their name.** Their embedded types move into sibling
  files (flat in the same directory, or in a subdirectory
  `<original-name>/` if type names would collide with another file),
  the driver instead gets `import <TypeName>;` lines. This keeps
  `scripts/dogfood.sh` and e2e-harness paths stable (see the 2026-07-26
  migration example: `examples/vtable_dispatch.tnx` stayed the entry
  point, its three types moved to `examples/vtable_dispatch/*.tnx`).
- **Watch out for sibling imports within the same (sub)directory:
  ALWAYS use the short, unqualified name** (`import IDrawable;`),
  NEVER the full dotted path the OUTER driver uses (`import
  vtable_dispatch.IDrawable;`) — the full path is relative to the
  directory of the IMPORTING file, so from inside the directory itself
  it would look for a non-existent, doubly-nested subfolder level
  (`vtable_dispatch/vtable_dispatch/IDrawable.tnx`) and fail with "file
  not found".
- **Finding from the migration (2026-07-26, affected effectively every
  program with a `main()` that upcasts an imported class against an
  equally imported interface):** `resolve_imports()` appended imported
  declarations to the END of the decl list, but the typechecker only
  fills `interface_implementations` lazily during the sequential pass
  (`check_class` in `tinox-typecheck/src/lib.rs`) — if `main()` (from
  the driver file) came before the imported interface/class
  declarations, the implements table was still empty when checking
  `main()`'s body ("expected IDrawable, found Circle"). Fix: imported
  declarations are now placed BEFORE the importing file's own
  top-level declarations (`resolve_imports` collects them separately
  and prepends instead of appending). For any future rework of the
  import-merge logic: don't break this ordering invariant, or this
  exact pattern breaks again silently (a silent-garbage trap: compiles
  unchanged for single-file programs, only multi-file programs with an
  interface upcast are affected).

## Namespace-Mirroring Folder Structure (issue #185, since 2026-08-14)

Finishes what the one-type-per-file convention above started: previously
only the LAST namespace segment became a directory (`tinox.core.amqp10` →
`crates/tinox-core-ext/amqp10/`); now a type declared inside a
`namespace a.b.c { ... }` block must live at a file path that mirrors the
**full** dotted namespace, hard-enforced at the compiler level exactly like
the one-type-per-file rule (`check_namespace_path_matches`,
`crates/tinox/src/main.rs`, wired at the same 5 call sites as
`check_one_type_per_file`).

- **Strictly opt-in, keyed off the `namespace {}` block, not a separate
  annotation.** A type declared with no enclosing `namespace` block is
  exempt — this matched 0% adoption in project-local code at migration
  time (every file under `examples/**` skips `namespace` entirely), so the
  check only ever fires for stdlib-style code that already declares one.
  There is no `namespace a.b.c;` semicolon form — the parser has a
  `module a.b.c;` statement (parsed but completely discarded, never
  affects resolution or this check) and a real `namespace a.b.c { ... }`
  block (`ast.rs`'s `Namespace` struct); only the latter carries any
  meaning.
- **Root resolution walks up for the nearest `tinox.toml`**
  (`pm::find_project_root_from`), then checks the mirrored path against
  whichever of `<manifest_dir>/src`, `<manifest_dir>/tests`, or
  `<manifest_dir>` itself the file actually resolves under (most specific
  first). `tests/` has to be a recognized root in its own right, not just
  a `src/` fallback — a `namespace`-wrapped test file legitimately lives
  under `tests/<namespace-path>/<TypeName>Test.tnx`, and checking it only
  against `src/...` rejects it outright (hit live while adding the first
  example test below: the check initially only knew about `src/`/bare
  `manifest_dir`, so a correctly-placed `tests/tinox/core/array/
  ArraysTest.tnx` was hard-rejected with "must be located at
  crates/tinox-core/tinox/core/array/ArraysTest.tnx" — i.e. `src/`'s
  fallback path — until `tests/` was added as its own candidate root). No
  `tinox.toml` ancestor at all, or the file isn't under any of the three
  candidates → the check is skipped, nothing to validate against.
- **Never applied to a file inside an INSTALLED dependency** (detected by
  a literal `.tinox` path component anywhere in the file's canonicalized
  path — covers both project-local `.tinox/deps/...` and the global
  `~/.tinox/repository/...` cache, `pm::dep_install_dir`/
  `global_dep_install_dir`'s own layout). Hit live during `make check`,
  not hypothetical: `socket` (core-tier, `CORE_MODULES`) is ALSO declared
  as an explicit dependency by an older e2e fixture and gets installed as
  a real package — but that published package predates this migration and
  has no `tinox.toml` of its own inside its installed directory, so
  `find_project_root_from` walked straight past it and anchored on the
  CONSUMING e2e project's own manifest instead, producing a nonsensical
  "must be located at `<consumer project root>/tinox/core/socket/
  Socket.tnx`" error for a file that project doesn't even own. Installed
  dependencies are pre-vetted, address-scoped, immutable content this
  check has no business re-validating in the first place — only this
  project's own source is in scope.
- **Core tier and extended tier ended up with DIFFERENT physical shapes —
  this asymmetry is load-bearing, not an inconsistency to "fix" later.**
  - `crates/tinox-core/` (core tier) has no per-module directory identity
    anymore: ALL modules now live under one shared
    `crates/tinox-core/tinox/core/<module>/` tree, because
    `stdlib_dir()`/the `tinox.core.X` resolution branch in
    `resolve_imports()` (main.rs) always resolves EVERY core module from
    the same single root — there's no per-module scoping to preserve. A
    minimal `crates/tinox-core/tinox.toml` was added purely as the
    `find_project_root_from` anchor for the namespace check (it has no
    role in dependency resolution).
  - `crates/tinox-core-ext/<module>/` (extended tier) KEEPS its own
    per-module top-level directory (that's how `tinox.toml`+dependency
    resolution scopes each published package) — only the module's
    *content* moved one level deeper, to
    `crates/tinox-core-ext/<module>/tinox/core/<module>/...` (preserving
    any existing internal nesting, e.g. `rest`'s `client/`/`server/` →
    `tinox/core/rest/client/`, `tinox/core/rest/server/`). The module's
    own `tinox.toml` stays at the module root, a sibling of the new
    `tinox/` subtree, not inside it.
  - Getting this backwards for either tier silently breaks resolution:
    core-tier modules are found via `stdlib_dir()` (one shared root, no
    per-module directory), extended-tier modules are found via
    `resolve_in_dep_dirs` against each dependency's own install directory
    (necessarily per-module) — mixing the two shapes up during any future
    change here reproduces exactly the "Cannot resolve stdlib import"
    failure this migration hit and fixed once already.
  - **This local-tree change is invisible to consumers.** Published/
    downloaded extended-tier packages already shipped with this exact
    `tinox/core/<module>/` nesting inside the archive before this
    migration (`scripts/publish-stdlib-ext.sh` staged it that way, and
    `resolve_in_dep_dirs` already resolves full paths under each
    dependency dir) — only the *local dev* source tree was flat. So
    `examples/**/tinox.toml`'s coordinate-based `[[dependencies]]` on
    extended-tier packages (resolved against the real tinox-central
    registry / `~/.tinox/repository/...` cache, never against
    `crates/tinox-core-ext/` directly) needed zero changes.
- **Test convention**: `tests/<namespace-path>/<TypeName>Test.tnx`
  (distinct from the existing scenario-named e2e fixtures at
  `tests/e2e/<scenario>/Main.tnx`, which are unaffected and keep their own
  shape/location — they don't declare a namespace and this check doesn't
  apply to them). Two representative examples exist so far, one per tier,
  both verified passing:
  `crates/tinox-core/tests/tinox/core/array/ArraysTest.tnx` (`tinox test
  crates/tinox-core/tests/tinox/core/array/ArraysTest.tnx` — resolves and
  runs directly, since core-tier imports always resolve via `stdlib_dir()`
  unconditionally) and `crates/tinox-core-ext/crypto/tests/tinox/core/
  crypto/CryptoTest.tnx` (content verified passing the same way
  `stdlib_smoke.rs`/`amqp10_consumer_annotation.rs` already verify
  extended-tier code: copied into a throwaway project with a synthesized
  `[[dependencies]] group="tinox.core" artifactId="crypto"` entry,
  `tinox install`, then `tinox test`). Backfilling this convention across
  every stdlib module is a separate, larger test-coverage initiative, not
  part of this layout migration.
- **Extended-tier test files can't be run directly with `tinox test
  <path>` from inside this repo, and that's pre-existing, not something
  this migration introduced.** Unlike core-tier (always resolves via
  `stdlib_dir()`), an extended-tier module's own `tinox.toml` declares no
  dependency on itself, so `import tinox.core.<module>;` inside its own
  `tests/` file has nothing to resolve against locally — exactly the same
  gap `stdlib_smoke.rs`'s own doc comment already describes for its SMOKES
  cases ("no longer resolve via `stdlib_dir()`/`TINOX_PATH` at build
  time"). Verifying an extended-tier test's actual logic therefore always
  goes through an installed (published) version of the module, the same
  way `stdlib_smoke.rs` and `amqp10_consumer_annotation.rs` already do it
  — not against the workspace's own uncommitted edits to that module.
- `crates/tinox/tests/stdlib_smoke.rs`'s `scan_module_dir` (the
  per-module inventory scan behind `stdlib_smoke_completeness`) needed a
  matching update: it transparently unwraps each extended-tier module's
  own `tinox/core/<name>/` prefix before applying its existing "does this
  dir have its own `.tnx` files, or is it a pure grouping dir" logic, so
  module names it reports (`amqp10`, `rest.client`, `rest.server`, ...)
  are unchanged from before the migration. Core tier didn't need a
  `scan_module_dir` change at all — its `stdlib_dir()` test helper is
  simply repointed straight at the shared `crates/tinox-core/tinox/core/`
  tree, which has the exact same per-module-subdirectory shape the old
  `crates/tinox-core/` root used to have.

## Mandatory Entry Point: `class Main` + CDI-Style Bootstrap (since 2026-08-09)

Since 2026-08-09 this is hard-enforced at the compiler level
(`compile_file` in `crates/tinox/src/main.rs`, `has_class_named_main`):
**every program built via `tinox build`/`tinox run` needs `class Main {
fnc main() -> Int32 }`** in the entry file — otherwise a hard compile
error instead of the old, confusing "undefined reference to
tinox_main" linker error. Exempt are `@Command` CLI programs (their
own argv dispatch, their own generated `main`) and `tinox test` (its
own test-runner entry) — both unchanged. `tinox check` only checks
types and never invokes codegen, so it's unaffected too.

**Why:** previously, every auto-run annotation (`@Http3RestController`/
`@WebsocketEndpoint`/`@Amqp10Consumer`/`@Amqp091Consumer`/plain `@GET`/
`@Path`) generated its own `@tinox_main` — "whoever runs first wins"
(the `has_main` flag), and `class Main` ALWAYS won first (it ran first
in `gen()`), which meant other annotations in the same program were
silently NOT wired up — no error message, the routes simply never ran.
Now there's a single, uniformly structured bootstrap
(`emit_tinox_main_bootstrap` in `crates/tinox-codegen/src/codegen.rs`)
instead: it spawns every auto-run component found in the program on its
own real thread (`tinox_task_spawn`, the same mechanism `spawn` uses),
then calls `Main.main()`, and afterward joins every spawned thread
(blocks forever if any are running — exactly like a single, direct
`.listen()` call did before).

- **Cross-kind combinations are now allowed** (previously hard-blocked
  in `main.rs`): `@Http3RestController` + `@WebsocketEndpoint`/
  `@Amqp10Consumer`/`@Amqp091Consumer` in the same program, or any
  combination of those together with `class Main` — they no longer
  compete for the same `@tinox_main` symbol.
- **Since 2026-08-09 (phase 4), multiple instances of the SAME kind are
  also allowed** for `@WebsocketEndpoint`/`@Amqp10Consumer`/
  `@Amqp091Consumer` (not for `@Http3RestController` — it still routes
  ALL `@GET`/… in the program to a single server, multiple instances
  would be architecturally ambiguous, deliberately out of scope).
  `emit_ws_code`/`emit_amqp10_consumer_code`/
  `emit_amqp091_consumer_code` now iterate over every class found
  instead of hard-reading `[0]`, and generate a uniquely named
  `__tinox_run_<kind>_<idx>()` per instance. For `@WebsocketEndpoint`,
  `compile_file` additionally checks for duplicate ports (each one
  binds its own listening socket — two on the same port would
  otherwise be a silent bind failure only surfacing at runtime); for
  the two AMQP consumer kinds there is NO port-collision check, since
  multiple consumers against the same broker/port with different
  queues/addresses is the normal, expected case.
- **New concurrency trap that couldn't structurally exist before:**
  previously, only ONE auto-run kind ever ran per process, so a
  singleton shared via `@ApplicationComponent` was implicitly safe
  (only one event loop ever touched it). Now that, say, a REST
  controller AND a WebSocket endpoint can run at the same time on
  real, independent threads, a singleton field shared between the two
  is accessed genuinely concurrently for the first time. The compiler
  does NOT synchronize this automatically (disproportionately invasive
  for v1) — synchronize manually (`tinox.core.semaphore`) when sharing
  mutable state across component kinds.
- **Example migration (2026-08-09):** annotation-only files without
  their own `class Main` (`examples/rest_minimal`,
  `examples/rest_with_mini`) got a trivial `Main.tnx`; single-file
  demos sitting flat in `examples/` with no directory
  (`UserController.tnx`, `EchoEndpoint.tnx`, `DemoConsumer.tnx`,
  `DemoConsumer091.tnx`) each moved into their own directory with a
  `Main.tnx` (`examples/rest_auto/`, `examples/ws_echo_annotated/`,
  `examples/amqp10_consumer_annotated/`,
  `examples/amqp091_consumer_annotated/`). `examples/http3_rest_api/
  src/TaskController.tnx` couldn't get its own `Main.tnx` next to the
  existing imperative `src/Main.tnx` (name collision), so it moved
  into its own sibling example instead,
  `examples/http3_rest_api_annotated/`. `scripts/dogfood.sh` and the
  affected `crates/tinox/tests/*.rs` paths were updated accordingly.

## Runtime Quirks (not obvious from the code)

- **`spawn` starts a real POSIX thread** (`pthread_create` in
  `tinox_task_spawn`, runtime.c), not a compiled coroutine state
  machine — real parallelism, no cooperative scheduling.
- **The Boehm GC uses `SIGPWR` as its "stop the world" signal** on this
  system (verified via `gdb`, not the often-assumed `SIGRTMIN`). Every
  blocking syscall (`recv`/`send`/…) in runtime code that could run
  during a GC collision MUST retry on `EINTR` (already done this way in
  `conn_recv`/`conn_send` — the template for new blocking I/O code).
- **Debugging technique for hard-to-reproduce runtime bugs:**
  `coredumpctl` doesn't produce dumps in this environment (sandbox
  restriction). `gdb` with conditional breakpoints on hot paths (e.g.
  `tinox_array_get`, called on every byte access) is unusably slow;
  `gdb` also needs `handle SIGPWR nostop noprint pass`, otherwise it
  keeps stopping on the harmless GC-suspend signal. Instead: add a
  temporary `errno` debug print, or a minimal `signal(SIGSEGV,
  handler)` with `backtrace()`/`backtrace_symbols_fd()` in `runtime.c`,
  then resolve the raw `[0x...]` addresses from the log with
  `addr2line -f -C -e <binary> <address>`. Remove again after
  debugging.

## `tinox docker`: Minimal Docker Images from a Project (since 2026-08-11)

`tinox docker` (`crates/tinox/src/main.rs`, `docker_build`) compiles the
project (same pipeline as `tinox build`, Release by default) and packages
the resulting binary into a minimal, single-stage Docker image: install
only the runtime shared libraries actually linked, `COPY` the binary in,
`EXPOSE` the configured ports, run it as the entrypoint. Config lives in
a `[docker]` section in `tinox.toml`:

```toml
[docker]
ports = [8080, 9090]        # optional, EXPOSE only -- doesn't change how
                             # the program binds them (still HttpServer::new(port) etc.)
image = "myapp"              # optional, defaults to [package].name
base = "debian:trixie-slim"  # optional, defaults shown; must be apt-based (see below)
extra_packages = ["libpq5"]  # optional, appended to the auto-detected apt package list
```

`--tag name:tag` overrides the image name+tag outright (from either
`tinox.toml` or the derived default); `--debug` compiles Debug instead of
Release.

- **The compiled binary is copied in from the host, not rebuilt inside the
  container.** A full multi-stage build (matching-glibc builder image,
  Rust+LLVM+clang toolchain, vendoring the compiler source into the build
  context) would remove the glibc-compatibility caveat below entirely, but
  is disproportionately invasive for what was asked for — a lightweight,
  minimalistic mechanism. Documented limitation, not a bug: `[docker] base`
  needs a glibc new enough for the host-compiled binary (older host glibc
  than the image's is fine; a newer host glibc generally is not). Default
  is `debian:trixie-slim` (glibc 2.41, current Debian stable as of
  2026-08) rather than `bookworm-slim` (glibc 2.36) -- the older default
  tripped the `ldd` check below on the very first two real-world runs
  (this dev machine, Arch glibc 2.44, and a user's machine needing
  `GLIBC_2.38`), so it wasn't just a theoretical edge case.
- **This is exactly the kind of thing the project's "no silent garbage"
  philosophy exists for, so it isn't just documented — it's enforced at
  build time:** after `docker build`, `docker_build` runs `ldd` on the
  copied-in binary inside the freshly built image and greps for "not
  found". Any missing symbol/library hard-fails the command with the exact
  `ldd` line instead of silently tagging a broken image as built. Verified
  live on this dev machine (Arch, glibc 2.44): `debian:bookworm-slim`
  (glibc 2.36) correctly hard-failed with `GLIBC_2.38 not found`;
  switching `[docker] base` to `debian:trixie-slim` (2.41) then built,
  passed the `ldd` check, and `docker run`'s output was verified against
  `curl` end-to-end (a standalone annotation-based REST demo project, not
  part of this repo). This dev machine's
  glibc (2.44) is itself still ahead of every current apt-based image
  including `debian:sid-slim` (2.42) and `ubuntu:devel` (2.43) -- expect
  `tinox docker`'s default to occasionally need a newer `base` override on
  bleeding-edge rolling-release hosts even after this bump.
- **Only apt-based (Debian/Ubuntu-family) base images are supported.** The
  generated Dockerfile's package-install step is hardcoded to
  `apt-get` — `[docker] base` pointing at an Alpine/Arch/etc. image will
  fail at that `RUN` step, not silently produce a broken image, but it
  won't work either. Not handled: scope was "minimal apt-based runtime
  image", not multi-package-manager support.
- **Package selection mirrors `compile_ll_to_exe`'s own link flags exactly**
  (`docker_runtime_packages`/`compute_runtime_packages`), rather than a
  fixed guess: `libgc1`+`zlib1g` always (matches unconditional `-lgc -lz`),
  `libssl3` when TLS is on (default on, matches `-lssl -lcrypto`, opt-out
  via `TINOX_TLS=0` same as the compiler), `libpq5`/`libmariadb3`/
  `libsqlite3-0` from `[database] driver` when set. `TINOX_HTTP3=1` prints
  a warning instead of guessing ngtcp2/nghttp3 package names (they vary by
  distro) — add them via `extra_packages` if needed; the `ldd` check
  catches it either way if they're missing.

## Startup Banner for Auto-Run Programs (since 2026-08-11)

Every compiled program that has at least one auto-run endpoint (`@GET`/
`@Http3RestController`/`@WebsocketEndpoint`/`@Amqp10Consumer`/
`@Amqp091Consumer`) prints a startup banner by default — no `import
tinox.core.logger;` or annotation needed. Owned by
`emit_tinox_main_bootstrap` (`crates/tinox-codegen/src/codegen.rs`),
since that's already the one place that knows about every registered
auto-run kind and is guaranteed to run exactly once, first:

```
 _____ _
|_   _(_)_ __   _____  __
  | | | | '_ \ / _ \ \/ /
  | | | | | | | (_) >  <
  |_| |_|_| |_|\___/_/\_\
Loaded tinox.core modules: http_server, json
Endpoints:
  HTTP                   :8080
Started in 0 ms
```

- **Only fires when `background_run_fns` is non-empty AND `banner_enabled`
  is true** (`show_banner` in `emit_tinox_main_bootstrap`). A plain
  `class Main { fnc main() }` script with no auto-run annotation goes
  through the *same* function (`user_main_class` alone doesn't early-
  return) but must produce byte-identical output to before this feature
  — that's the shape virtually every e2e/example test with an exact `//
  expect:` stdout match uses. **Verify the `background_run_fns`-empty
  half of this gate whenever touching this function**: the very first
  implementation forgot it, and every single compiled program (including
  the entire e2e suite) grew this banner — caught immediately by
  compiling a trivial one-`println` `class Main` and diffing its output,
  not by `cargo test` (no e2e test happens to combine an auto-run
  annotation with an exact stdout match, so the suite itself wouldn't
  have caught this).
- **Explicit per-project opt-out: `[startup]` / `banner = false` in
  tinox.toml** (`read_startup_banner_config` in `crates/tinox/src/
  main.rs`, defaults `true`; `CodeGen::banner_enabled` /
  `set_startup_banner_enabled`). Added because jgrep-tinox/ygrep-tinox
  are plain argv-parsing CLI tools with no auto-run endpoint, so
  `background_run_fns` is already empty for them and the banner never
  fires regardless — this setting only matters for a program that DOES
  have an endpoint (so the banner would otherwise print) but still needs
  clean stdout, e.g. piped into another program.
- **"Loaded tinox.core modules"** is `tinox.toml`'s declared
  `[[dependencies]]` filtered to `group == "tinox.core"`
  (`loaded_tinox_core_modules` in `crates/tinox/src/main.rs`, read
  alongside `load_dep_dirs` in `compile_file` and passed to codegen via
  `CodeGen::set_loaded_modules`) — declared, not actually-imported.
  Simpler, and accurate enough: an unused declared dependency is already
  the unusual case, not the common one this needs to optimize for.
- **"Endpoints:"** is `(protocol, detail)` pairs pushed into
  `CodeGen::startup_endpoints` right alongside each `background_run_fns`
  push (same emit_*_code functions, so always in sync): `("HTTP",
  ":8080")`, `("HTTP/3 (QUIC)", ":8843")`, `("WebSocket", ":9001")`,
  `("AMQP 0-9-1 (consumer)", "host:port (queue: q)")`, `("AMQP 1.0
  (consumer)", "host:port (address)")`. AMQP consumers connect out
  rather than bind a port, hence the different (no leading `:`) shape.
- **"Started in N ms"** is wall-clock from the top of `@tinox_main`
  (before the banner print) to right after every auto-run kind has been
  `tinox_task_spawn`-ed (before calling `Main_main`) — via
  `tinox_now_ms()` (runtime.c, `clock_gettime(CLOCK_MONOTONIC, ...)`),
  diffed on the Tinox side (two IR-level calls + a `sub`, no runtime
  elapsed-time helper needed). This is "time to bring up the bootstrap",
  not "time until the first successful request" — `HttpServer::listen()`s
  actual bind happens asynchronously on its own spawned thread, so a
  slow/failing bind is invisible to this number, same tradeoff Spring
  Boot's own "Started Application in Xs" line makes.

## Dev UI Introspection API (since 2026-08-11)

`[dev] enabled = true` in `tinox.toml` (`DevConfig`/`read_dev_config`,
`crates/tinox/src/main.rs`) compiles in a background JSON introspection API
(`emit_devui_code`, `crates/tinox-codegen/src/codegen.rs`) for a *separate*
web dashboard (`tinox-devui`, a standalone Vaadin-on-Quarkus app,
`git@github.com:subnix-work/tinox-devui.git`, not part of this repo) to
consume — Quarkus-dev-mode-style: current config, REST/
WebSocket endpoints, live CDI component status, loaded `tinox.core`
modules. Enabling it works for `tinox build`/`run` too, not gated behind
`tinox dev` specifically (deliberate — `compile_file` prints a release-
build warning as the safety net instead of a hard gate).

- **`127.0.0.1`-only bind, unlike every other `HttpServer` in this
  codebase.** New runtime.c primitive `tinox_HttpServer_new_bind(port,
  addr)` (the public `HttpServer::new(port)` stays `0.0.0.0`/`::`
  unchanged) — this API exposes config and CDI internals, so it must never
  be reachable off the local machine. Verified live: `ss -ltnp` on a
  devui-enabled `demo` run shows the app's own port on `0.0.0.0`, the devui
  port on `127.0.0.1` only.
- **Found and fixed a real concurrency bug while adding this**: adding a
  *second* `HttpServer::listen()` call in the same process (previously
  impossible — before this feature, no program ever ran more than one) hit
  `struct TinoxWorkerArgs { ... }` being `static` in
  `tinox_HttpServer_listen` (runtime.c) — shared storage across every call
  to that function, not per-instance. With two listening servers, the
  second's worker args silently clobber the first's while its still-
  running worker threads keep reading it, serving the wrong server's
  routes on the wrong port. Fixed by making it a plain stack local
  (`tinox_HttpServer_listen` never returns while its server is up, so the
  stack frame outlives the spawned workers, same as `static` did — just
  scoped per-call instead of shared).
- **Found and fixed a real, unrelated pre-existing bug while studying this
  pattern**: the `/metrics` endpoint's `Content-Type` header
  (`emit_route_code`'s metrics shim) computed `%ct_hdr_val` and then never
  used it — `%body_i64` (the response body's own pointer) was passed as
  the header *value* instead, so every `/metrics` response's Content-Type
  header ended up set to the same string as its body. No test ever
  exercised the header specifically, so nothing caught it until this
  investigation.
- **`declare`-conflict landmine for anything emitted alongside
  `emit_route_code`**: `opt` hard-errors ("invalid redefinition") on a
  *second* `declare` for a symbol already declared elsewhere in the
  module, even with an identical signature -- contrary to what an earlier
  draft of this feature assumed from an unverified reading of the existing
  double-`declare` of `tinox_HttpServer_new` inside the metrics shim
  (which, it turns out, never actually co-occurs with `emit_route_code`'s
  own copy in any tested program — a genuinely separate, still-open,
  latent bug: a program with **both** `[metrics]` enabled **and** real
  `@GET`/etc. routes would hit the exact same "invalid redefinition"
  class of error the devui work below had to route around; not fixed
  here, out of scope for this feature). `emit_devui_code` mirrors
  `emit_route_code`'s own `route_entries.is_empty() ||
  http3_rest_controller.is_some()` guard to decide whether it's safe to
  declare `tinox_HttpServer_get`/`_listen` itself.
- **`/components`** (`emit_devui_components_handler`) is the one endpoint
  needing real per-request work: `@ApplicationComponent`/`@Startup`-scoped
  classes get a live look at their `@{class}_di_instance` global (null or
  not); `@HttpRequestScoped` ones report a constant `false` — they have no
  persistent singleton at all (`_di_create()` allocates fresh every call,
  never caches), so there's nothing to check.
- **`/config`** merges two genuinely separate sources at runtime via
  `tinox_string_concat`: a compile-time summary of `tinox.toml`'s
  `[docker]`/`[database]`/`[metrics]`/`[startup]` sections
  (`build_dev_config_summary_json`, main.rs — deliberately omits
  `[database] url`, which can carry credentials, even though this
  endpoint is loopback-only) and a live dump of `application.properties`
  (`tinox_config_dump_json`, new in runtime.c — the existing
  `tinox_config_get*` only ever look up one key a `@Config` field already
  declared, there was no "list everything" API before this).
- **`httpPort` on `/info`**: the app's plain-HTTP port (`self.startup_
  endpoints`'s `"HTTP"` entry, already registered by `emit_route_code`,
  which runs before `emit_devui_code`), `null` for an HTTP/3-only program.
  This is what `tinox-devui`'s REST "try it out" targets — deliberately
  NOT the introspection port itself, and NOT `"HTTP/3 (QUIC)"` (a plain
  `java.net.http.HttpClient` can't speak QUIC; HTTP/3-only apps just don't
  get try-it-out in v1).

## `tinox-devui` Dashboard + `tinox dev` Docker Orchestration (since 2026-08-12)

The consumer side of the introspection API above: a standalone Maven/
Quarkus/Vaadin app (`tinox-devui` repo, dark Lumo theme matching
tinox-central's `registry-frontend`) with an `AppLayout`+`SideNav` shell
and one view per introspection endpoint (Overview/Configuration/REST
Endpoints/WebSocket Endpoints/CDI Components/Modules). `TinoxDevUiClient`
(`@ApplicationScoped`, plain `java.net.http.HttpClient` + manual Jackson,
mirrors tinox-central's `RegistryClient.java` pattern) talks to the
connected app's `[dev] port` (`tinox.app.url` / `TINOX_APP_URL`,
default `http://localhost:9090`).

- **REST "try it out"** (`RestEndpointsView`): a dialog per route with a
  `TextField` per `:param` path segment (parsed via regex, substituted
  and URL-encoded before the call), a raw headers textarea (`"Name:
  value"` per line), a body textarea, and a "Send" button that calls
  `TinoxDevUiClient.invoke(httpPort, method, path, headers, body)` --
  server-side, against the app's OWN `httpPort` (from `/info`), never the
  introspection port and never directly from the browser. This is why
  there's no CORS story on the tinox side (decision made during planning,
  see the approved plan) -- the browser only ever talks to this Quarkus
  backend, which does the real HTTP call itself.
- **WebSocket "try it out"** (`WebSocketEndpointsView` + `DevUiWsClient`):
  same server-side-proxy shape, but for a persistent connection instead of
  one-shot calls. `DevUiWsClient` is a plain Jakarta WebSocket
  (`quarkus-websockets-client`) `Endpoint` connecting to the app's own WS
  port (`/websockets`' `port`, NOT the introspection port). Incoming
  messages arrive on the WS client's own thread, not Vaadin's request
  thread, so every UI update (transcript line, connected/disconnected
  status pill) goes through `UI.access(...)` -- requires `@Push` on
  `AppShellConfig`, the one piece of Vaadin server-push wiring this whole
  app needs, added specifically for this view.
- **`tinox dev` orchestration** (`launch_devui_container`/
  `stop_devui_container`, `crates/tinox/src/main.rs`): when the project's
  `[dev]` is `enabled`, `tinox dev` additionally `docker run -d --rm
  --network host` the `tinox-devui` image (tag from `[dev] devui_image`,
  default `tinox-devui:latest` -- a locally built image; override once a
  real registry tag exists) alongside the compiled program, with
  `TINOX_APP_URL` pointed at `127.0.0.1:<dev.port>`, then opens
  `http://localhost:9091` the same way `tinox doc --open` already does
  (`xdg-open`/`open` fallback). `--network host` is what lets the
  container reach the loopback-only introspection API directly -- no
  `host.docker.internal`, Linux-only, matches this whole toolchain's
  target. A missing/unbuildable image is a soft failure (a printed
  warning, `tinox dev` still runs the actual program fine) rather than a
  hard error, consistent with `[dev]` itself being an opt-in convenience
  feature, not a build-blocking dependency.
- **Found a real cleanup gap while wiring this up, not hypothetical:**
  `dev_mode`'s only exit path before this was the file-watcher channel
  closing (`rx.recv()` returning `Err`) -- which a plain Ctrl-C never
  triggers. The compiled child process happened to look cleaned-up anyway
  (the terminal's own SIGINT delivery kills it directly, since it's in the
  same foreground process group), which is presumably why this was never
  noticed before. A `docker run` container is NOT in that process group
  though, so every single ordinary `tinox dev` + Ctrl-C session would have
  silently leaked a running `tinox-devui` container -- verified live: with
  no signal handler, `kill -INT` on `tinox dev`'s pid terminated the
  process immediately without running any Rust cleanup code, leaving the
  container in `docker ps`. Fixed by adding a real `ctrlc::set_handler`
  (new `ctrlc` dependency) sharing `Arc<Mutex<...>>`-wrapped child-process
  and container-name state with the main loop, calling the same cleanup
  closure (`kill` child, remove temp exe files, `docker stop` the
  container) from both the normal loop-exit path and the signal handler.
  Re-verified live after the fix: `kill -INT` now cleanly stops and
  removes (`--rm`) the container and leaves no leftover `.tinox_dev_*`
  files.
- **Published to `ghcr.io/subnix-work/tinox-devui` (since the `tinox-devui`
  repo's `v1.0.0` tag, 2026-08-12).** The image was `docker build`+`docker
  run`-verified locally first (against a real `demo`-style app, and
  end-to-end through `tinox dev` itself) before the registry push, per the
  plan's "publish only after manual validation" note.
  `.github/workflows/publish.yml` (in the `tinox-devui` repo) builds and
  pushes on every `vX.Y.Z` tag, using the repo's own `GITHUB_TOKEN`
  (`packages: write` permission -- no separate PAT/secret needed) via
  `docker/login-action`. `launch_devui_container`'s default `[dev]
  devui_image` is now this published tag (`ghcr.io/subnix-work/
  tinox-devui:latest`) rather than a locally-built-only `tinox-devui:latest`
  -- `docker run` pulls it automatically on a machine that's never built
  the dashboard itself. Override to a local build via `[dev] devui_image`
  in `tinox.toml` when developing the dashboard.

## CDI Component Full-State Dump + Test Runner (since 2026-08-12)

Two follow-up additions to the Dev UI, both requested after the initial
dashboard was already live: `/components`' `state` field (the CDI
singleton's actual field values, not just name/scope/instantiated), and a
new `/tests/run` endpoint + Tests view that runs the connected project's
own `tinox test` suite from the dashboard.

**CDI state** (`emit_devui_component_state_handlers`, codegen.rs): a
SEPARATE serializer from `emit_json_serialize_code`'s `_toJson`
(@JsonSerializable-only, used for real REST response bodies), not an
extension of it -- the original plan for this whole feature explicitly
called out why: `List<Class>`/nested-class fields are i64* at the LLVM
level, indistinguishable from a plain int array, so `_toJson`'s existing
fallback (`jsonBuilderAddIntList`) would silently misread one as
consecutive int64s if reused naively. The new serializer instead reuses
`tinox_json_list_serialize` (the SAME dispatch the compiler's own
`List<C>.toJson()` call-site codegen already uses, so it's proven, not
new machinery) for `List<X>` where X is `@JsonSerializable`, and falls
back to an honest `"<unsupported field type>"` placeholder for anything
else pointer-shaped it can't identify this way (Map<K,V>,
List<String>/List<Float>, a List of a non-`@JsonSerializable` class, a
directly nested class field -- narrower than `List<X>`, deliberately
skipped rather than risking a null-pointer call into an arbitrary
class's `_toJson`). Verified live against `demo`'s `PersonController`
(`var people: List<Person>` -- exactly the case `_toJson` would have
gotten wrong): `/components` now returns the real `people` array with
every field, `null` before the singleton is first instantiated.
`ComponentInfo.state`'s null-safety is handled INSIDE the generated
`{class}_devui_state_json(i8* %self_i8)` function itself (an `icmp eq
... null` branch returning `i8* null` immediately) rather than at the
call site in `emit_devui_components_handler` -- lets that caller invoke
it unconditionally for every Application/Startup-scoped component,
matching how `HttpRequest`-scoped ones (no persistent instance to check)
just store a constant null pointer instead.

**Test runner** (`/tests/run`, `tinox_run_command_json` in runtime.c):
shells out (via `popen`) to a compile-time-constant command --
`cd <project root> && <tinox binary path> test 2>&1` -- built in main.rs
from `std::env::current_exe()` and a new `find_project_root()` helper
(same nearest-`tinox.toml` walk `read_dev_config`/etc. already do, just
returning the directory instead of parsed section contents). Never
influenced by request input, so there's no injection surface despite
running a shell command from an HTTP handler. `tinox test`'s own
human-readable stdout (PASS/FAIL lines, a final summary) comes back
as-is in the `output` JSON field -- no separate structured result format
needed on either side. The dashboard's Tests view runs this on its own
background thread (`TestsView.runTests()`, tinox-devui repo) and pushes
the result via `UI.access(...)` once done, the same `@Push` wiring
`WebSocketEndpointsView`'s `DevUiWsClient` needs -- a real test run can
take anywhere from a couple seconds to much longer, so unlike REST/
WebSocket "try it out" this can't just block the request thread; without
the background thread + push, the whole click would appear to freeze for
the run's duration with no progress indicator ever actually rendering.

**Two real, previously-undiscovered bugs found and fixed while building
the test runner** -- found purely by trying to write and run one real
test against `demo`'s own `PersonController`, exactly the kind of thing
this project's CLAUDE.md philosophy expects ("verify against real
systems... structurally find NO bugs where the implementation is
self-consistent but wrong"):
- **`tinox test` silently hung instead of running the test** when the
  test file imports a class carrying its own auto-run annotations (here:
  `PersonController`'s `@GET` routes) -- a completely ordinary thing to
  want to test (a helper method on a REST controller). Root cause:
  `compile_test_exe` runs a test file through the exact same `gen()`
  codegen path as a normal build, including whatever it `import`s --
  `PersonController`'s routes populate `background_run_fns` same as any
  real program. `emit_tinox_main_bootstrap` runs BEFORE `emit_test_code`
  (which only defines `@tinox_main` when `!self.has_main`) and
  unconditionally claimed `@tinox_main` for itself instead, since its own
  guard only checked `has_main`/`background_run_fns.is_empty()`, neither
  of which was true. The compiled "test" binary silently became the real
  app's auto-run bootstrap (spawned the HTTP server, blocked forever
  joining its listener thread) and the actual `@Test` method was never
  called -- no error, no wrong-answer failure, just a hang, which is
  arguably worse. Fixed with one added guard at the top of
  `emit_tinox_main_bootstrap`: `if self.test_entry.is_some() { return; }`
  -- `background_run_fns`/`route_entries` etc. still get populated and
  their functions still get emitted, just as harmless unused IR; only the
  "what does `@tinox_main` become" decision needed to defer to the test
  runner. Verified live: `tinox test` on `demo`'s new
  `src/PersonControllerTest.tnx` (which imports `PersonController`) now
  runs its 4 tests and exits normally instead of hanging.
- **`tinox test <project-root-dir>` doesn't work the way it looks like it
  should**: the CLI's single positional arg is a FILE path
  (`collect_test_files`), not a project directory -- passing a directory
  fails with "Is a directory" (Rust's `fs::read_to_string` on a dir).
  There's no dedicated "run against this directory" flag; the only way to
  get the tests/+src/ auto-discovery scan is to run `tinox test` bare
  from inside the project. `/tests/run`'s command construction accounts
  for this: `cd <project root> &&`, not a positional argument.

## REST Parameter Binding Annotations (since 2026-08-12)

Every `@GET`/`@POST`/`@PUT`/`@PATCH`/`@DELETE` handler parameter now needs
exactly one of `@PathParam`/`@QueryParam`/`@PostParam`/`@HttpContext`
(`tinox-typecheck/src/annotations.rs`, `tinox-codegen/src/codegen.rs`'s
`emit_route_handler_call`/`emit_route_auto_serialize`) instead of the old
implicit single `ctx: HttpContext` parameter -- replaces the
`ctx.request.getParam(...)`/`ctx.request.getQuery(...)`/
`Json::deserialize<T>(ctx.request.body)` boilerplate every handler used to
hand-write, in line with this repo's "annotations over manual boilerplate"
preference.

- **`@PathParam`/`@QueryParam`** take no argument -- they bind by the
  Tinox parameter's own name to the same-named `:name` path segment /
  query string key. Supported types: String/Int64/Int32/Bool/Float64/
  Float32. Missing (empty string) or, for non-String types, not parseable
  as the declared type -> the handler is never called, the shim
  short-circuits with HTTP 400 + a JSON error body (mirrors the existing
  `@Auth`/`@OIDCRolesAllowed` guard-and-early-return shape). New strict
  runtime.c helpers (`tinox_parse_int_checked`/`_float_checked`/
  `_bool_checked`) back this -- the existing `tinox_string_to_int`/
  `_to_float` silently return 0/0.0 on garbage input (no error signal),
  which would make a malformed value indistinguishable from a legitimate
  zero, exactly the kind of silent garbage this project's philosophy
  exists to prevent.
- **`@PostParam`** binds the whole deserialized JSON body to that
  parameter; the type must be `@JsonSerializable`. Codegen reuses the
  exact `Json::deserialize<T>` specialization machinery the compiler's
  own `List<C>.toJson()`/generic-static-method call-site codegen already
  uses (`ensure_generic_method_specialization`, extracted from
  `gen_generic_method_call` for this reuse -- see the bug note below for
  why a naive reuse attempt broke).
- **`@HttpContext`** opts a parameter into the request/response handle.
  Independent of the response mode below -- a handler can combine
  `@HttpContext` with `@PathParam`/etc. on other parameters freely (e.g.
  read a header AND still auto-serialize the return value), though the
  natural pairing is `@HttpContext` + `-> HttpContext` together for full
  manual control.
- **Response mode is decided entirely by the declared return type**, not
  a separate annotation: `-> HttpContext` = manual mode, the handler
  builds `ctx.response` itself exactly like before (the shim never
  dereferences the returned value -- `ctx.response` was already mutated
  in place through the `%ctx_ptr` the shim itself passed in, so an
  unused SSA capture is enough, no return-completeness check needed for
  this mode specifically). `-> AnyOtherType` (a `@JsonSerializable`
  class, `List<class>`, String, Int64, Int32, or Bool) = auto-serialize
  mode: the shim serializes the returned value as the JSON response body
  via the real, compiled `HttpResponse.json(String)` (which also sets
  `Content-Type: application/json; charset=utf-8` itself, matching what a
  manual handler's own `.json(...)` call already does -- nothing extra
  needed for that). Pointer-typed auto-serialize returns (class/List/
  String) get a null-check first (500 + error body on null) since this
  path DOES dereference the value, unlike manual mode; scalar returns
  (Int64/Int32/Bool) skip it (`0` is indistinguishable from a legitimate
  zero, the same narrow, pre-existing gap every non-Nothing Tinox
  function already has -- not worsened by this feature, see below).
- **No backward compatibility, deliberately (user-confirmed).** The old
  implicit shape (`fn handler(ctx: HttpContext) -> Nothing`, a single
  unannotated `HttpContext` parameter) is a hard compile error now, not a
  silently-preserved legacy path -- simplifies `emit_route_shim_body` to
  exactly one code path (no branching on "is this the old shape"). Every
  existing example/test using the compiler's auto-run `@GET`/`@POST`
  system was migrated: `examples/rest_auto`, `rest_minimal`,
  `rest_with_mini`, `http3_rest_api_annotated/TaskController`,
  `keycloak_oidc_api/HelloController`, `docs/tinox-core/rest/examples/
  02_Annotated_controller.tnx`, `demo`'s `PersonController` (the primary
  real-world proof, mixing manual mode for `getPerson`'s dynamic 404-vs-
  200 with auto-serialize for `listPersons`/`createPerson`), and the two
  Rust curl-based integration tests that compile their own fixtures
  (`http3_annotated_curl.rs`, `http_server_gc_stress_curl.rs`).
  `tinox.core.rest.server`'s separate, unrelated `RestController`/
  `RestApi` manual-registration framework (own `@GET`/`@POST` data-holder
  classes via `@annotation`, routes registered at runtime via `.route()`,
  never through `extract_route_from_method`'s auto-run extraction) is
  untouched -- confirmed structurally unaffected, not just "didn't happen
  to need changes".
- `docs.html`/`docs_en.html`'s `#annotationen` section got a "Migration
  von der alten REST-API"/"Migrating from the old REST API" subsection
  (old-vs-new side by side) alongside the four new annotations in the
  built-in annotations table -- same parallel-maintenance rule as this
  file's docs.html section above.
- **`emit_route_shim_body` (codegen.rs) is shared, unmodified in this
  regard, by both the plain-HttpServer path (`emit_route_code`) and
  HTTP/3 (`emit_http3_route_code`, `@Http3RestController`)** -- so this
  feature works identically for HTTP/3 routes "for free", verified live
  (not just assumed) via the migrated `http3_annotated_curl.rs`, which
  exercises `@PathParam`/`@PostParam`/`@HttpContext`/auto-serialize over
  real HTTP/3 end to end.
- **Found and fixed a real bug while wiring `@PostParam` up**: the first
  attempt called the existing specialization machinery
  (`gen_generic_method_call`'s internals) lazily from inside
  `emit_route_handler_call`, which builds the current route's shim body
  by writing directly into `self.lambda_ir`. That machinery ALSO appends
  newly-triggered specializations straight into `self.lambda_ir` --
  safe when called from ordinary expression codegen (where `self.ir` is
  the "current function" buffer and `self.lambda_ir` only ever holds
  complete, already-finished definitions), but not when the caller is
  ITSELF mid-way through writing into `self.lambda_ir`. Caught immediately
  by actually compiling a `@PostParam` handler: `opt` rejected the output
  with "expected instruction opcode" -- the generated `Json_deserialize__
  Person` function definition had landed textually INSIDE the enclosing
  route shim's own body, before its closing brace. Fixed by extracting
  the specialization-only part into `ensure_generic_method_specialization`
  and pre-triggering every `@PostParam` binding's specialization
  (`ensure_postparam_specializations`) once per route list, BEFORE the
  shim-emission loop starts -- so each specialization lands as its own
  clean top-level entry first, and the later in-shim call for the same
  (class, type) pair becomes a cheap no-op that just returns the mangled
  name.
- **Confirmed still accurate, not a regression**: this compiler DOES
  already have a real "missing return statement" check for both
  functions and methods (`tinox-typecheck/src/lib.rs`, `check_function`/
  the method-checking equivalent) -- an earlier concern that no such
  check existed at all (based on `TypeError::MissingReturn`/
  `ReturnTypeMismatch` being unused dead enum variants) turned out to be
  wrong; the real enforcement uses a plain `Error::new(...)` string
  directly, not those specific variants. This meaningfully de-risked
  auto-serialize mode: a handler that provably never returns on some path
  is already a compile error today, independent of this feature.

## REST "Try it out" Parameter Fields + Request/Response Example Schemas (since 2026-08-12)

`/routes`' JSON entries (`devui_routes_json`, codegen.rs) now additionally carry
`params` (every `@PathParam`/`@QueryParam`/`@PostParam`/`@HttpContext` binding, with
its Tinox type name) and `requestExample`/`responseExample` -- compiler-generated
example JSON *values* for the `@PostParam` body and the auto-serialized response body,
so `tinox-devui`'s "Try it out" dialog can show a user what to send/expect without
them going to read the controller source. Builds directly on the REST parameter
binding annotations feature above (`RouteEntry.params`/`.return_type` already existed
in codegen; this is purely an introspection-API + dashboard addition, no typecheck
changes).

- **`devui_json_example_for_type` walks the original AST `Type`, not the LLVM-level
  `struct_field_llvm_types`/`struct_field_class_types` maps** used elsewhere in this
  file for codegen proper -- those lose generic-argument fidelity (a `List<String>`
  field and a `List<Person>` field are both just `"i8*"`/`"List"` at that level), which
  would produce useless examples. `class_ast_map` (built once near the top of `gen()`,
  ~codegen.rs:1578) is threaded down as a plain parameter into `emit_devui_code`/
  `devui_routes_json` rather than stored as a new `self` field -- its only call site
  (`self.emit_devui_code()`) is later in the same `gen()` body, so a persistent field
  would be pure overhead.
- **Cycle safety is genuinely new in this file.** The closest existing analog,
  `emit_devui_component_state_handlers`'s CDI state dumper (see the section above),
  avoids the problem entirely by refusing to recurse into nested classes at all.
  Example generation DOES recurse (needed for realistic examples of real nested
  request/response bodies), so a self-referential `@JsonSerializable` class (e.g. a
  tree `Node` with a `List<Node> children` field) needs an explicit guard: a
  `visiting: HashSet<String>` of class names currently being expanded on the current
  path short-circuits a re-entrant one to a `"<ClassName>"` placeholder string instead
  of recursing forever, plus an independent `depth` cap (12) as defense-in-depth
  against long non-cyclic chains. Verified live: a `Node { id: Int64; children:
  List<Node>; }` fixture compiles instantly and produces
  `{"id":0,"children":["<Node>"]}`, not a hang or stack overflow.
- **`requestExample`/`responseExample` are embedded as raw, unescaped nested JSON**
  (`null` when absent), matching the existing precedent of `/components`'s `state`
  field (`tinox_devui_components_json`, runtime.c) rather than a JSON-escaped string
  -- also simply less code, since `devui_routes_json` already builds its JSON via
  plain `format!` splicing, not the runtime `jsonBuilder` API. `RouteInfo.java`
  (tinox-devui repo) declares these as `JsonNode`, consistent with `ComponentInfo
  .state`'s existing handling, not `String`.
- **Real, pre-existing gap fixed on the `tinox-devui` side while wiring this up, not
  hypothetical**: `RestEndpointsView`'s "Try it out" dialog previously created input
  fields ONLY for `:name` path segments (via a regex over `route.path`) -- `@QueryParam`
  -bound parameters got no field at all, and even a manually-appended `?name=value`
  would have gone nowhere, since the "Send" handler had no query-string-building logic
  whatsoever. Fixed alongside this feature (not a separate change) since `route.params`
  now exists and makes the omission obvious: both `PathParam` and `QueryParam` kinds
  get labeled fields (`":id (Int64)"` / `"loud (Bool)"`), and `Send` builds a real
  `?k=v&k2=v2` query string from the latter. Verified live end-to-end (not just
  compiled): filling `:name=Ada` + `loud=true` on a `GET /greet/:name` route and
  clicking Send returned a real `HTTP 200 {"message":"hi Ada"}` -- confirming the query
  param actually reached the handler, not just that the field rendered.
- **No separate type-only schema output.** A single populated example value (real
  field names + representative typed values) is enough to answer "what do I type
  here" -- matching Swagger's "Example Value" tab rather than adding a parallel
  "Schema" tab. `params`' per-field `{kind, name, type}` already covers the scalar
  path/query side separately.

## Dynamic Ports for e2e Test Fixtures (since 2026-08-12)

30 of the `tests/e2e/**/*.tnx` fixtures (every simulated-broker `spawn`+connect
test: AMQP 0-9-1/1.0, OIDC, OAuth2, SMTP, WS, ...) used to hardcode a literal TCP
port, tracked only by a manual "grep `httpServerCreate(4` before picking a new
one" convention -- which had already caused a real collision this session (a
manually-run `tinox dev` session on :8080 colliding with
`http_server_gc_stress_curl.rs`). Fixed with a new runtime builtin,
`httpServerBoundPort(fd) -> Int64` (`runtime/runtime.c`, `getsockname()` on the
listening socket), registered exactly like every sibling low-level HTTP builtin
(`tinox-typecheck/src/lib.rs`'s `httpServerCreate` entry, `tinox-codegen/src/
codegen.rs`'s matching `declare`) -- `httpServerCreate(0)` already made the OS
pick a free ephemeral port, the missing piece was just a way to ask which one it
picked. All 30 fixtures migrated: `httpServerCreate(LITERAL)` -> `httpServerCreate
(0)` + `let port = httpServerBoundPort(srv);`, threading `port` through every
place that used to repeat the literal (connect calls, and a few OIDC/OAuth2
fixtures that build a JWKS/token-endpoint URL as a string -- those splice via
`"http://127.0.0.1:" + port.toString() + "/jwks.json"` instead).

- **One real edge case, not hypothetical**: `amqp10_heartbeat_reconnect/Main.tnx`
  binds twice (close, then rebind to test `conn.reconnect()`, which targets the
  *original* host:port with no new port argument) -- the second bind must reuse
  the *same* port the first one got, not a fresh `httpServerCreate(0)` (which
  would hand back an unrelated port `reconnect()` never learns about). Fixed as
  `httpServerCreate(port)` (the variable) on the second call --
  `SO_REUSEADDR`/`SO_REUSEPORT` (already set unconditionally in
  `httpServerCreateOn`) make rebinding to the same port immediately after close
  safe. Verified live, 5x repeated: identical output every run.
- Deliberately **not** applied to the 4 Rust `*_curl.rs` integration tests
  (`crates/tinox/tests/`) -- two are already `TINOX_PORT`-driven (trivial to make
  dynamic later if desired), two build real, checked-in example files with the
  port as a literal *inside user-facing example source*
  (`examples/http3_rest_api_annotated/src/TaskController.tnx`'s
  `@Http3RestController(8843, ...)`, `examples/http3_hello/Main.tnx`'s
  `Http3Server::new(8493, ...)`) -- making those dynamic changes real example
  behavior, not just test plumbing, and each already uses one distinct,
  non-colliding port. Out of scope here (fix narrowly, not broadly) -- a
  separate follow-up if wanted.

## `tinox graph`: Mermaid Call-Graph From Entry Points (issue #186, since 2026-08-14)

`tinox graph [file] [--out <path>]` (`gen_call_graph` in `crates/tinox/src/
main.rs`, graph construction/rendering in the new `crates/tinox/src/
callgraph.rs`) statically analyzes a project and writes a Mermaid
`flowchart TD` call graph (default `docs/callgraph.mmd`) seeded from every
auto-run entry point: `@GET`/`@POST`/etc (including `@Http3RestController`
routes, since those flow through the same `route_entries`),
`@WebsocketEndpoint`'s `@OnOpen`/`@OnMessage`/`@OnClose`,
`@Amqp10Consumer`/`@Amqp091Consumer`, and `@Command` (CLI). v1 scope,
settled via AskUserQuestion before implementation: per-METHOD nodes (not
per-class), the full entry-point matrix including AMQP (not deferred as
the issue itself first suggested), and no `--from`/`--depth` filtering yet
(fast-follow once the base output is confirmed useful -- exactly the
"prototype first" step the issue asked for, done against
`examples/rest_with_mini`, see below).

- **No new discovery logic needed at all** -- `tinox_typecheck::
  annotations::process_annotations(&ast)` already returns every entry
  point's `class_name` + handler method name(s) (`RouteInfo`,
  `WsEndpointInfo`, `Amqp10ConsumerInfo`/`Amqp091ConsumerInfo`,
  `CliCommandInfo`), the exact same structs `codegen.rs` already consumes
  for the real annotation-driven bootstrap. `@Command`'s entry method has
  no per-method annotation to discover (unlike the other three kinds) --
  it's a fixed convention, `run` (verified against `codegen.rs`'s `call
  i64 @{class}_run(...)` and `examples/GreetCommand.tnx`), hardcoded in
  `build_call_graph`.
- **No `tinox-typecheck` coupling for interface fan-out beyond
  `TypeChecker::interface_info()`, already called on this same pipeline's
  AST anyway** (`gen_call_graph` runs the identical parse -> resolve_imports
  -> typecheck -> process_annotations sequence `compile_file` uses, since a
  real type-checked AST is needed either way). `interface_info()` returns
  `(iface_methods: HashMap<interface, Vec<method>>, class_implements:
  HashMap<class, Vec<interface>>)` -- inverted once into `interface ->
  implementing classes` for fan-out. `interface_implementations` itself
  does NOT walk the `extends` chain (each class's own direct `implements`
  only, confirmed by reading `check_class`'s population site) -- so this
  is exactly equivalent to what a hand-rolled AST walk over
  `Class.implements` would have given anyway, just reusing tested logic
  instead of duplicating it.
- **`MethodCall { obj, method, args }` represents BOTH `ClassName.method(...)`
  (static) and `var.method(...)` (instance) calls** -- disambiguated by
  resolving `obj`: `Ident` matching a known class = static call; `Ident`
  matching a local var/param with a STATICALLY declared type (explicit
  `var x: Foo = ...`, or inferred from a `var x = new Foo()` initializer
  only -- no real type inference, a single flat non-scope-aware pass over
  the method body, deliberate v1 simplification) = instance call through
  that type; `This` = self-call; `New { class, .. }` = a chained `new
  Foo().method(...)`. Anything else is **unresolved** -- shown via a
  shared `unresolved` sink node + `%%`-comment detail lines in the `.mmd`
  output, never silently dropped (this project's "no silent garbage"
  philosophy, applied to a read-only analysis tool same as everywhere
  else).
- **Real, load-bearing edge case, found via the issue's own suggested
  prototype target (`examples/rest_with_mini/UserController.tnx`)**:
  `getUser` calls `findUserIndex(users, id)` with NO receiver at all. Since
  top-level free functions are banned (issue #149), this parses as
  `ExprKind::Call { func: Ident("findUserIndex"), .. }`, not `MethodCall`
  -- syntactically identical to a lambda-variable invocation at parse
  time. The walker special-cases a bare `Call` to a same-class method
  name (checked via `find_method`, which also walks the `extends` chain)
  before falling back to "unresolved (lambda call)".
- **A second real, load-bearing edge case, found live while smoke-testing
  `examples/ws_echo_annotated` (not from the prototype target above)**:
  `Ws::sendText(conn, ...)` was completely invisible in the graph at
  first -- neither an edge nor an unresolved entry. `X::y(...)` is the
  SAME `ExprKind::EnumValue` AST node for a real enum-variant literal
  (`Color::Red`, `Option::Some(value)`) and this static-call-like
  reference -- there's no separate call syntax for it. Fixed by handling
  `EnumValue` as a call site too: a known project class -> a real static
  call; a known project enum -> not a call at all (skip, matches a real
  variant construction); anything else (e.g. `Ws`/`Json`, merged in via
  `import tinox.core.*`) -> unresolved rather than silently dropped, since
  it's genuinely ambiguous without cross-crate type info.
- **A third real bug, also found via the SAME `examples/ws_echo_annotated`
  smoke test, worse than the second one**: once `Ws::sendText` started
  resolving, the traversal recursed straight into `tinox.core.websocket`'s
  OWN internals (`Ws.sendText` -> `Ws.writeFrame` -> ... ->
  `httpConnWriteBytes`) -- exactly the "expanding into tinox.core.*
  internals" the issue explicitly asks to avoid, and the plan explicitly
  designed against. Root cause: `resolve_imports` merges every imported
  file's decls into the SAME flat list before this module ever sees the
  AST, so a `class_ast_map`-style lookup built from those merged decls
  can't tell "this project's own class" from "a class pulled in via
  import" apart on its own -- `Ws` was structurally indistinguishable
  from `UserController`. Fixed by `project_owned_classes`
  (`callgraph.rs`): since one-type-per-file guarantees every method of a
  class shares one originating file, and `stamp_file_identity` (main.rs)
  already stamps that onto every `Method.file` for both the entry file
  and every imported file uniformly, a class only counts as
  project-owned if its first method's file is a descendant of
  `project_root` and NOT inside a `.tinox` (installed-dependency)
  subtree -- the same `.tinox`-component heuristic issue #185's
  `check_namespace_path_matches` already uses to recognize installed
  dependencies. The FULL merged class map is still used for name
  resolution (so `Ws::sendText` still correctly resolves as a real call,
  not "unresolved") -- only the RECURSION decision (expand further or
  stop) consults the project-owned subset.
- **Cycle/depth safety is a single global `expanded: HashSet<"Class.method">`
  set plus a depth cap (40), not a fresh per-entry-point set** -- a
  deliberate simplification over the devui `visiting`-set pattern this
  feature was modeled on (see the REST "Try it out" Example Schemas
  section below): since the goal is ONE combined graph across every entry
  point (not a separate graph per entry point), reusing one global
  "already expanded" set both dedupes edges when multiple entry points
  reach the same subtree AND makes a genuine call cycle (A calls B calls
  A) safe for free -- the second visit to an already-expanded node just
  returns immediately, after its inbound edge was already recorded, so
  the cycle still shows correctly in the output without needing a
  separate on-stack/visiting-vs-expanded distinction.
- **Verified against 4 real example projects, one per entry-point kind**
  (`crates/tinox/tests/callgraph.rs`, spawns the real compiled `tinox`
  binary, not a direct call into `callgraph.rs`'s functions): REST
  (`rest_with_mini`, asserting on the two edge cases above by name), CLI
  (`GreetCommand`, the `run` convention), WebSocket (`ws_echo_annotated`,
  asserting the stdlib boundary actually stops expansion), AMQP 1.0
  (`amqp10_consumer_annotated`). Every generated `.mmd` in this
  investigation was also rendered with real `@mermaid-js/mermaid-cli`
  (`npx @mermaid-js/mermaid-cli`) to confirm valid Mermaid syntax, not
  just eyeballed -- the prototype step the issue's own "Suggested next
  step" asked for before committing to the full entry-point matrix.
- **A fourth real bug, caught by CI, not local testing**: `gen_call_graph`'s
  every failure path (unreadable file, lex/parse error, import error, type
  error, unwritable output) originally did `eprintln!(...); return;` --
  returning from the function normally instead of `std::process::exit(1)`
  the way `build()`'s equivalent paths already do. `run()`'s dispatch
  match doesn't propagate a callee's "did this fail" status at all, so
  this meant the PROCESS exited 0 even after printing an error and never
  writing the output file -- exactly the kind of silent-success failure
  this project's philosophy exists to prevent. Every one of the 3 example
  projects this feature is tested against (`rest_with_mini`,
  `ws_echo_annotated`, `amqp10_consumer_annotated`) declares an
  extended-tier dependency that needs `tinox install` first; this
  dev machine already had all three cached from earlier work, so the bug
  was invisible locally -- a genuinely fresh CI checkout (nothing in
  `~/.tinox/repository`) hit the "declare it in tinox.toml... then run
  tinox install" import error immediately, and with the missing
  `exit(1)`, that printed error was silently treated as success by the
  test harness's own `output.status.success()` check, which only then
  failed on the SEPARATE, more confusing symptom of the output file not
  existing. Fixed by switching every failure path to
  `std::process::exit(1)`, and by adding the same `install_deps_if_needed`
  step (`tinox install`, cwd'd at the entry file's own directory) the
  existing `amqp10_consumer_annotation.rs` test already uses, to
  `callgraph.rs`'s own test helper -- re-verified by temporarily moving
  `~/.tinox/repository` aside to force a real, fresh-machine install path
  locally, not just trusting the CI rerun.

## Editor Support: Eclipse + VS Code (`editors/`, since 2026-08-14)

`editors/eclipse/` (moved here from a top-level `eclipse/`, no other path
in the repo referenced the old location) and `editors/vscode/` are both
thin LSP CLIENTS over the same `tinox-lsp` binary (`crates/tinox-lsp`,
tower-lsp based) -- neither has its own language-analysis logic. Every
feature (diagnostics, hover, completion, go-to-definition, outline) comes
from `tinox-lsp` itself; each editor plugin is just wiring.

- **Shared `editors/install-lsp.sh`** (hoisted out of `editors/eclipse/`,
  which used to have its own copy — nothing in the script was
  Eclipse-specific): `cargo build --release -p tinox-lsp` +
  `cp target/release/tinox-lsp ~/.cargo/bin/tinox-lsp`. Both editors'
  READMEs point at this one copy.
- **Binary path resolution is identical in both editors, deliberately**:
  a user-configurable setting (Eclipse: `tinox.lsp.path` preference;
  VS Code: `tinox.lsp.path` setting), defaulting to probing
  `~/.cargo/bin/tinox-lsp`, `/usr/local/bin/tinox-lsp`,
  `/usr/bin/tinox-lsp` in that order
  (`TinoxPreferenceInitializer.java` / `defaultLspPath()` in
  `editors/vscode/src/extension.ts`). The "Run File" command in both
  derives the `tinox` compiler binary's path the same way, too: swap
  `tinox-lsp` for `tinox` in the resolved LSP path (assumes both
  binaries live in the same directory, true for a normal cargo
  build/install) -- `RunTinoxHandler.java`'s `getTinoxBinary()` and
  `resolveTinoxBinaryPath()` in `extension.ts` are the same logic in two
  languages.
- **The TextMate grammar (`tinox.tmLanguage.json`) is a genuine
  duplicate, not a shared file** —
  `editors/eclipse/tinox-eclipse/grammars/tinox.tmLanguage.json` and
  `editors/vscode/syntaxes/tinox.tmLanguage.json` are byte-identical as
  of this writing, but there is no build step or symlink keeping them
  that way. **Must be kept in sync by hand** — same deliberate
  duplication convention this repo already uses for `docs.html`/
  `docs_en.html`, chosen for the same reason: each editor ecosystem
  expects the grammar file living in its own conventional location
  (`grammars/` for TM4E, `syntaxes/` for VS Code), and a shared file
  outside either directory would need its own copy/build step for two
  genuinely small, rarely-changing files. (Aside, not acted on here,
  scope was "add VS Code support" not "improve the grammar": the shared
  grammar is missing `namespace`/`fnc` from `keyword_declaration` even
  though both are real Tinox keywords used throughout this repo — a
  pre-existing gap in the Eclipse-era grammar, inherited as-is by the
  VS Code copy rather than silently fixed as a drive-by change.)
- **Neither is published anywhere** — no Eclipse update site, no VS Code
  Marketplace listing (a deliberate choice, confirmed with the user:
  local-only distribution matches the Eclipse plugin's own existing
  precedent exactly). Eclipse: manual Export → deployable plug-in →
  `.jar` into `dropins/`. VS Code: `npx @vscode/vsce package` → `.vsix`
  → "Install from VSIX...". No CI wiring for either.
- **VS Code packaging note**: `package.json` needs a `repository` field
  or `vsce package` prints a (non-blocking) warning; added, pointing at
  this repo with `directory: "editors/vscode"`. A missing `LICENSE`
  file in `editors/vscode/` itself also warns (non-blocking) — not
  added, since the repo's root `LICENSE-APACHE`/`LICENSE-MIT` already
  cover the whole tree including this directory, and duplicating full
  license text into a third location would just be one more place to
  keep in sync for a cosmetic warning.
- **A real bug, found only by the user actually testing a real,
  installed VS Code window (build-time checks alone did NOT catch this
  — see below)**: syntax highlighting worked, but completion never
  showed anything object-specific (`ctx.` fell back to VS Code's own
  generic word-based suggestions, e.g. literal string fragments already
  present in the file, instead of `tinox-lsp`'s real, type-aware
  member list) — and the "Tinox Language Server" output channel didn't
  even exist in the Output panel dropdown, meaning the `LanguageClient`
  was never constructed at all. Root cause: `.vscodeignore`'s
  `node_modules/**` line strips `vscode-languageclient` (a real
  `dependencies` entry, not `devDependencies`) out of the packaged
  `.vsix` — but plain `tsc` compilation leaves
  `require("vscode-languageclient/node")` as a literal Node `require`
  call, which doesn't bundle anything. The require throws the instant
  VS Code tries to load the extension, so `activate()` never runs —
  with no visibly obvious error dialog for the user to notice, since a
  module-load failure in one extension doesn't interrupt anything else
  (syntax highlighting, being pure declarative grammar, works
  regardless, since it needs no JS to run at all — this is exactly why
  it looked "half-working" instead of "not working"). Fixed by bundling
  the extension with esbuild (`editors/vscode/esbuild.js`) into a
  single self-contained `out/extension.js` with only `vscode` itself
  left external (real VS Code injects that at runtime; every other
  dependency, including all of `vscode-languageclient`'s own transitive
  deps, gets inlined) — `node_modules/**` in `.vscodeignore` is now
  correct rather than the bug, since the bundle genuinely needs nothing
  from it at runtime. `npm run compile` is now `tsc --noEmit` (type
  -checking only) followed by the esbuild bundle step, not `tsc`'s own
  emit.
- **Verification note**: build-time correctness was checked twice —
  once before the bug above was found (compiles clean, packages into a
  `.vsix`, installs via CLI — none of which caught the missing-bundle
  bug, since none of those steps actually LOAD the extension inside a
  JS host the way VS Code itself does) and once after the fix, this
  time including a non-GUI load test: extracting the packaged `.vsix`
  and `require()`-ing the bundled `out/extension.js` under a real
  Node process with a minimal mocked `vscode` module, confirming zero
  "Cannot find module" errors for anything other than `vscode` itself
  (the mock's remaining gaps, e.g. `vscode.CodeLens` not being a real
  class, are artifacts of the mock's incompleteness, not the
  extension — real VS Code provides all of these for real). This is a
  meaningfully stronger check than the pre-fix verification, but still
  short of a full live GUI pass — the actual confirmation that
  highlighting/hover/completion/diagnostics/Run File all work came from
  the user testing a real installed build, not from anything automated
  in this repo. If touching this extension again: a `tsc`-compiles /
  `vsce`-packages / `--install-extension`-succeeds check is NOT
  sufficient on its own to catch an activation-time bundling bug like
  this one — either do the `require()`-under-mock check above, or get
  a real human to open a `.tnx` file and confirm completion/hover
  actually populate, not just that highlighting renders.
- **A second real bug, found in the SAME live-testing round, layered on
  top of the first**: after fixing the bundling bug above, completion
  populated but was drowned out by VS Code's own generic word-based
  suggester (literal word/string fragments already present in the file
  -- e.g. `"Alice"`/`"Bob"` from unrelated string literals elsewhere in
  the same file -- mixed in alongside, and vastly outnumbering, the
  real `tinox-lsp` member completions). VS Code adds these by default
  for every language unless a language extension opts out. Fixed via
  `contributes.configurationDefaults`: `"[tinox]": {
  "editor.wordBasedSuggestions": "off" }` in `package.json` -- ships as
  the DEFAULT for anyone installing the extension, but is only a
  default, so it can still be silently overridden by a pre-existing
  user/workspace setting (a real snag hit live during this same
  session: setting it a second time, explicitly, directly in the user's
  own `settings.json`, was needed to confirm the fix before the
  packaged default's effect could be verified against a genuinely clean
  reinstall).
- **Both bugs together are why a clean reinstall matters when verifying
  a fix to this extension, not just re-running `vsce package`**: VS
  Code does not always fully unload/reactivate an extension just
  because its `.vsix` was reinstalled over the same version number --
  confirming a fix required Uninstall -> Reload Window -> Install from
  VSIX -> Reload Window again, checked by confirming the "Tinox
  Language Server" entry actually appears in the Output panel's
  channel dropdown (compare against another real, working
  LSP-based extension's own entry, e.g. "JSON Language Server", as a
  sanity check that the mechanism itself is functioning) before
  re-testing completion.
- **`editors/eclipse/build.sh`** (added after the user pointed out that
  "install the plugin" originally meant "import a PDE project into
  Eclipse and Run As Eclipse Application" -- real friction compared to
  a normal Eclipse plugin install): builds a real, installable OSGi
  bundle JAR from the command line, no Eclipse GUI/PDE Export wizard
  needed. Auto-detects an Eclipse bundle pool to compile against --
  verified live on this dev machine that an Eclipse-Installer
  -provisioned install's actual bundle jars live in the SHARED pool at
  `~/.p2/pool/plugins`, not the installation's own near-empty
  `plugins/` directory (only 1 jar there); `ECLIPSE_PLUGINS_DIR`
  overrides the guess. Compiles with `--release 17` (matching
  `Bundle-RequiredExecutionEnvironment: JavaSE-17` in `MANIFEST.MF` --
  the system `javac` here is Java 26, so an unqualified compile would
  silently produce bytecode newer than what the manifest declares
  supported) against a classpath of every jar in the pool (a real,
  proper OSGi dependency resolution would only need the bundles listed
  in `Require-Bundle`, but globbing the whole pool is simpler and
  robust enough for a plugin this small). Substitutes a real build
  timestamp for `MANIFEST.MF`'s checked-in `1.0.0.qualifier` PDE/Tycho
  placeholder (`sed`), since a raw manual build has no Tycho build
  process to fill that in itself. Verified (without launching Eclipse,
  which needs the same live-desktop caution as the VS Code work above):
  the produced JAR is a well-formed bundle (`META-INF/MANIFEST.MF` +
  `plugin.xml` + `grammars/` + compiled `.class` files, matching
  `build.properties`'s `bin.includes` exactly) and its class files are
  genuinely Java 17 bytecode (`javap -verbose` -> `major version: 61`).
  Actually dropping the jar into a running Eclipse's `dropins/` and
  confirming it loads is the one step left to a real human, same
  caveat as the VS Code extension's own verification note above.
  **Found and fixed a real, pre-existing documentation gap while
  writing this**: both `README.md` and `SETUP.md` only ever mentioned
  LSP4E as a prerequisite, never TM4E -- but `MANIFEST.MF`'s
  `Require-Bundle` has always required both (TM4E renders the grammar;
  without it, syntax highlighting silently wouldn't have worked even
  though nothing else would have visibly failed). Not something this
  build.sh work introduced, just a gap noticed while reading
  `Require-Bundle` closely enough to know what to check for on the
  compile classpath -- fixed in both docs alongside this change.
- **`File → Import → Tinox → Import Existing Tinox Project` wizard**
  (`TinoxImportWizard`/`TinoxImportWizardPage`, `org.eclipse.ui.importWizards`)
  imports a project in place (no file copy -- `newProjectDescription` +
  `setLocation` + `create`/`open`, the same sequence "Import Existing
  Projects into Workspace" uses structurally) from a picked
  `tinox.toml`. `TinoxToml.parsePackageName` is a minimal line-scanning
  `[package]` reader mirroring `pm.rs`'s own hand-rolled parser (no TOML
  library dependency for one field). `src/` is required (matches
  `pm.rs:1117-1121`'s own "no src/, no package" rule); `tests/` is
  optional and rare in real Tinox projects (only `crates/tinox-core` has
  one anywhere in this repo) -- the wizard must not require it.
  `src/`/`tests/` get a real icon swap (not just a corner badge) via
  `TinoxSourceFolderDecorator` using `IDecoration.REPLACE`, gated on a
  new `TinoxProjectNature` (`org.eclipse.core.resources.natures`) so it
  never fires outside actual Tinox projects -- the decorator itself
  registers globally (no per-folder-name enablement exists in the
  extension point), so that gate has to live in code, not XML. Icons
  (`icons/source_folder.png`/`tests_folder.png`) are freshly generated
  (ImageMagick), not copies of JDT's own art -- there's no
  functional distinction to justify depending on JDT for this (Tinox
  isn't Java, `IClasspathEntry` doesn't apply, and there's no
  tinox-lsp/tooling concept of a "test source root" to hook into yet;
  confirmed with the user this is meant to be visual/organizational
  only for now). No new `Require-Bundle` entries needed -- `plugin.xml`
  additions are all additive, `org.eclipse.core.resources`/
  `org.eclipse.ui.ide` were already present. `icons/test_folder.png`
  was renamed to `tests_folder.png` after discovering the root
  `.gitignore`'s `test_*` rule (meant for excluded compiled test
  binaries elsewhere in the repo) was silently matching it too --
  more accurate anyway, since the real folder is named `tests`, not
  `test`.
- **A real bug, found only by the user actually dropping the JAR into a
  real running Eclipse and checking the Error Log (build-time checks --
  `javac`, `jar`, `unzip -l`, `javap` -- caught none of this, since
  none of them parse `plugin.xml` as Eclipse itself does)**: the new
  `importWizards`/`natures`/`decorators` extensions never showed up at
  all -- not just the import wizard, EVERYTHING in `plugin.xml`
  silently stopped registering (LSP, syntax highlighting, the Run
  command, all of it), yet the bundle itself still showed up as
  installed in `Installation Details -> Plug-ins`, giving no obvious
  signal anything was wrong from that view alone. Root cause: one of
  the new `<!-- -->` comments contained a bare `--` in its body (a
  prose double-hyphen, this project's usual "aside" punctuation) --
  which the XML spec forbids ANYWHERE inside a comment, not just at
  its boundaries. Confirmed via the actual Error Log entry the user
  found: `org.eclipse.equinox.registry` logs "Could not parse XML
  contribution... Any contributed extensions and extension points will
  be ignored" and fails the ENTIRE file's parse, not just the one
  malformed comment -- a single stray `--` anywhere in `plugin.xml`
  is a full-file outage, not a localized one. Fixed the comment, and
  -- so this exact mistake can't silently ship again -- added an
  `xmllint --noout plugin.xml` validation step to `build.sh` itself,
  verified live to actually catch it (reintroduced the bug in a
  throwaway copy, confirmed `xmllint` reports the exact line/column
  and a non-zero exit; confirmed it passes clean on the real, fixed
  file). Generalizes beyond this one mistake: `plugin.xml` is
  hand-written XML with no other syntax check anywhere in this
  project's tooling (Eclipse's own PDE editor would flag this
  instantly, but nothing does when editing the file directly) --
  `xmllint` is now the one thing standing between a typo here and a
  silent full-plugin outage that only surfaces in a real user's Error
  Log.
