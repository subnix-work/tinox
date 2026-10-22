# `tinox docker`

Two modes, both producing an apt-based (Debian/Ubuntu) runtime image that runs
as a **non-root user** with an exec-form `ENTRYPOINT`.

| | host build (default) | `--source-build` |
|---|---|---|
| Compiles | on the host, binary copied in | inside the pinned builder image |
| libc requirement | host glibc <= image glibc (checked with `ldd` after build) | none — builder and runtime share a Debian release |
| Needs | host toolchain | only Docker (+ network for the builder/base pull and `tinox install`) |

## Configuration (`tinox.toml`)

```toml
[docker]
ports = [8080]               # EXPOSE only
image = "myapp"              # default: [package].name, lowercased
base = "debian:trixie-slim"  # runtime base, apt-based only
builder = "ghcr.io/subnix-work/tinox-builder:2.4.0-trixie"  # --source-build only; default tracks the compiler version
extra_packages = ["libpq5"]  # appended to the auto-detected runtime packages
include = ["config"]         # --source-build only: extra project-relative paths for the context
uid = 10001                  # default 10001; 0 = run as root (explicit opt-in)
```

## Non-root layout

* User/group `tinox` with `uid`/`gid` = `[docker] uid` (default 10001), no login shell.
* `/app/<binary>` and `/app/build-info.json` are root-owned and not writable by the app.
* `/app/data` is the one writable path (owned by the app user). Mount a volume there for state.
* Ports below 1024 may need `--sysctl net.ipv4.ip_unprivileged_port_start=0` or `--cap-add NET_BIND_SERVICE` depending on the Docker version; prefer ports >= 1024.
* After the build, `docker run --entrypoint id <image> -u` is checked against the configured uid.

## `--source-build`

```
tinox docker --source-build [--tag name:tag]
```

1. **Build context** (`.tinox/docker/ctx/project/`): only `tinox.toml`, `tinox.lock`,
   `src/`, a root `application.properties`, the entry file if outside `src/`, and `[docker] include`
   paths. Dotfiles/dot-directories (`.env`, `.git`, `.tinox`) and symlinks are never copied.
   The staged file list is printed and recorded in `build-info.json`.
2. **Build stage** runs `tinox install` (when dependencies are declared; checksums are verified against
   `tinox.lock` if present — a warning is printed if it is missing) then `tinox build --release`.
   `TINOX_TLS`/`TINOX_HTTP3` from the calling environment are forwarded.
3. **Runtime stage** installs only the runtime libraries for the enabled features (libgc, zlib, TLS,
   database driver — same selection as host mode) and copies the binary out of the build stage.
4. **Validation**: `ldd` inside the final image must report no missing library, and the container UID is checked.

### Builder image

`docker/builder/Dockerfile` builds the compiler from the checkout and bundles LLVM, a C toolchain and
the `-dev` packages for TLS, Postgres, MySQL and SQLite. Build and tag it as shown in that file, or push
it to a registry and point `[docker] builder` at it. HTTP/3 is not provisioned in the builder (no
matching ngtcp2 packages in Debian trixie); `TINOX_HTTP3=1` fails at link time rather than shipping a broken image.

### Provenance

`build-info.json` (in the image at `/app/build-info.json` and in `.tinox/docker/ctx/`) records compiler
version, target (`linux/amd64`), enabled features, DB driver, whether `tinox.lock` was present, the builder
and base image references and the source file list. Images are pinned by `name@sha256:` in the generated
Dockerfile when the daemon can resolve a registry digest; otherwise the mutable tag is used and
recorded as `unresolved (...)`. The same data is attached as OCI labels (`tinox.compiler.version`,
`tinox.builder.image`, `tinox.base.image`).

### Reproducibility guarantee

**Functional compatibility only, not byte-identical artifacts.** With digest-pinned images, a committed
`tinox.lock`, and the same sources, the resulting binary links against the same glibc/native libraries
and behaves the same on any Linux host. Byte-identical output is *not* promised: apt packages in the
runtime stage are installed from the live Debian mirror (versions float within the pinned base's release),
the build embeds no timestamp normalisation, and dependencies not pinned by `tinox.lock` resolve from the
registry at build time.

### Caching and offline use

Docker layer caching applies: the build layer is invalidated by any change in the staged context, the
runtime package layer only by package list/base changes. The builder/base pulls and `tinox install`
need network; for offline builds, pre-pull both images and pre-populate dependencies (`tinox install`
before staging is not reused inside the container today, so offline builds of projects with
dependencies are not supported yet).

## Status of verification

Unit tests cover Dockerfile generation, context allowlisting and provenance rendering. The end-to-end
flow (builder image build, two-host run, REST/DB/TLS smoke tests, measured image size) has **not** yet
been exercised against a real Docker daemon; image size, startup time and the two-host acceptance check
still need to be recorded.
