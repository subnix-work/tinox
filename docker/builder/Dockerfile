# Pinned builder image for `tinox docker --source-build` (issue #309).
#
# Contains the Tinox compiler built from THIS checkout, the LLVM tools it
# shells out to (opt, llc), a C toolchain, and the native -dev packages for
# every optional runtime feature (TLS, Postgres/MySQL/SQLite). The runtime
# image `tinox docker` pairs with it is the same Debian release's slim base,
# so the binary is linked against the glibc it will run on.
#
# Build (from the repository root, tag = compiler version + Debian release):
#   docker build -f docker/builder/Dockerfile \
#     -t ghcr.io/subnix-work/tinox-builder:$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)-trixie .
#
# HTTP/3 (ngtcp2/nghttp3) is deliberately not provisioned: Debian trixie has
# no matching ngtcp2 OpenSSL-crypto package, so TINOX_HTTP3=1 with
# --source-build fails at link time with a clear error instead of shipping a
# half-working image.
FROM rust:1-trixie AS compiler
RUN apt-get update && apt-get install -y --no-install-recommends \
        llvm clang libgc-dev zlib1g-dev libssl-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY . .
RUN cargo build --release -p tinox --locked

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates llvm clang libc6-dev \
        libgc-dev zlib1g-dev libssl-dev \
        libpq-dev libsqlite3-dev libmariadb-dev-compat \
    && rm -rf /var/lib/apt/lists/*
COPY --from=compiler /build/target/release/tinox /usr/local/bin/tinox
COPY --from=compiler /build/runtime/runtime.c /usr/share/tinox/runtime.c
COPY --from=compiler /build/runtime/runtime.h /usr/share/tinox/runtime.h
COPY --from=compiler /build/runtime/tinox-gdb.py /usr/share/tinox/tinox-gdb.py
COPY --from=compiler /build/crates/tinox-core /usr/share/tinox/core
# Fail the image build, not a user's later build, if the tools tinox shells out
# to aren't reachable under the unversioned names it invokes.
RUN tinox --help >/dev/null && opt --version >/dev/null && llc --version >/dev/null && cc --version >/dev/null && pg_config --includedir >/dev/null
LABEL org.opencontainers.image.source="https://github.com/subnix-work/tinox"
