# FlightDeck Remote relay — container image.
#
# IMPORTANT: the build context is the `remote/` workspace root, not
# `remote/relay/`, because this crate path-depends on `../protocol`
# (see relay/Cargo.toml). Build from the `remote/` directory:
#
#   cd remote && docker build -f relay/Dockerfile -t flightdeck-relay .
#
# or, equivalently, from the repo root:
#
#   docker build -f remote/relay/Dockerfile -t flightdeck-relay remote/

# --- Builder --------------------------------------------------------------
# Matches the glibc ABI of the debian12-based distroless runtime image below.
FROM rust:1-bookworm AS builder
WORKDIR /workspace

# Copy manifests only first, so the dependency-compilation layer is cached
# across application source changes.
COPY Cargo.toml Cargo.lock ./
COPY protocol/Cargo.toml protocol/Cargo.toml
COPY relay/Cargo.toml relay/Cargo.toml

# `apns-live` compiles in the real HTTP/2 APNs transport (and pulls in reqwest).
# It is off by default so the crate builds and unit-tests without it, which means
# the deployed image MUST opt in explicitly — without it the push sender is a
# silent no-op and an offline phone is simply never woken. Both build invocations
# need the same feature set or the second one recompiles from scratch, defeating
# the dependency cache this two-pass structure exists for.
#
# Cargo needs *some* source present to resolve and build the workspace, even
# for a dependencies-only pass. Stub it out, build once to warm the
# dependency cache, then discard the stubs — real source replaces them below
# without disturbing that cached layer.
RUN mkdir -p protocol/src relay/src \
    && echo "" > protocol/src/lib.rs \
    && echo "" > relay/src/lib.rs \
    && echo "fn main() {}" > relay/src/main.rs \
    && cargo build --release --locked -p flightdeck-relay --features apns-live \
    && rm -rf protocol/src relay/src

COPY protocol/src protocol/src
COPY relay/src relay/src

# Bump mtimes so Cargo's fingerprinting doesn't mistake the freshly-copied
# real source for the (older-content, but possibly same-or-newer-mtime) stubs
# it already compiled.
RUN touch protocol/src/lib.rs relay/src/lib.rs relay/src/main.rs \
    && cargo build --release --locked -p flightdeck-relay --features apns-live

# --- Runtime ----------------------------------------------------------------
# Distroless: no shell, no package manager, minimal attack surface. `:nonroot`
# runs as uid/gid 65532 out of the box, satisfying the non-root requirement
# without needing a package manager in this stage to create a user.
FROM gcr.io/distroless/cc-debian12:nonroot AS runtime

COPY --from=builder /workspace/target/release/flightdeck-relay /usr/local/bin/flightdeck-relay

# Azure Container Apps convention; overridable via the Container App's env
# vars. Must match whatever `PORT` the process is actually told to bind via
# its own environment at runtime — this ENV is the default, not an override.
ENV PORT=8080
EXPOSE 8080

USER nonroot
ENTRYPOINT ["/usr/local/bin/flightdeck-relay"]
