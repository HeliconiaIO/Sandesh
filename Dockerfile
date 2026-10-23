# Sandesh as a container image (`ROADMAP.md` Phase 10).
#
# Three stages, and the ordering is forced rather than stylistic: `rust-embed`
# compiles `web/dist` *into* the binary (`crates/sandesh-api/src/assets.rs`), so
# the frontend must be built before `cargo build` runs. A Rust-first image would
# embed whatever `web/dist` happened to be lying around — or fail the build when
# it is absent, which is the friendlier of the two wrong answers.
#
# ## Why a static musl binary
#
# The runtime stage has no libc, no shell and no package manager, so the binary
# must carry everything it needs. Linking against glibc would mean shipping a
# distro base image and its update cadence along with a mail server whose whole
# appeal is that it starts in a second and does one thing.

# ---------------------------------------------------------------- frontend ---
FROM node:22-alpine AS web

WORKDIR /build

# Manifests first: this layer is rebuilt only when dependencies change, not on
# every source edit, which is most of the image's build time.
COPY web/package.json web/package-lock.json ./
RUN npm ci

COPY web/ ./
# The generated contract, which `src/api/openapi.test.ts` imports — so `tsc -b`
# needs it even though nothing at runtime does. Placed at the path the test
# expects, relative to the web root.
COPY docs/openapi.json /docs/openapi.json

RUN npm run build

# -------------------------------------------------------------------- rust ---
FROM rust:1-alpine AS build

# musl-dev provides the C toolchain `ring` and `sqlx`'s bundled SQLite need;
# the rest is what those crates' build scripts invoke.
RUN apk add --no-cache musl-dev pkgconfig perl make

WORKDIR /src

# The whole workspace: `tests/architecture.rs` reasons about the dependency
# graph, so a partial copy would change what is being built.
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY migrations/ migrations/
COPY src/ src/
COPY xtask/ xtask/
COPY testdata/ testdata/

# The frontend build, where `rust-embed` expects it.
COPY --from=web /build/dist/ web/dist/

# Only the server binary. `sandesh-desktop` is deliberately excluded: it needs
# GTK and WebKit, which have no place in a headless image, and the workspace
# member would otherwise pull them into the build.
RUN cargo build --release --bin sandesh --locked \
    && strip target/release/sandesh

# ----------------------------------------------------------------- runtime ---
# `scratch` rather than `alpine`: nothing in the image is executable except
# Sandesh itself, so a compromised process has no shell to escalate with and no
# package manager to fetch one.
FROM scratch AS runtime

# Certificate roots, for the outbound webhook client (ADR-085). Copied rather
# than assumed: on `scratch` there is no system trust store at all, and TLS to a
# webhook endpoint would fail with an opaque error.
COPY --from=build /etc/ssl/cert.pem /etc/ssl/cert.pem

# Licence texts. Apache-2.0 §4 and the dependencies' licences require them to
# travel with the binary, and an image is a redistribution. The bracket glob
# makes the generated third-party file optional for a local build.
COPY LICENSE NOTICE THIRD_PARTY_LICENSES.tx[t] /licenses/

LABEL org.opencontainers.image.source="https://github.com/HeliconiaIO/Sandesh" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.vendor="Heliconia Solutions Pvt. Ltd." \
      org.opencontainers.image.title="Sandesh"

COPY --from=build /src/target/release/sandesh /sandesh

# The data directory. Declared as a volume so a `docker run` with no `-v` still
# keeps mail for the container's life, and an operator who wants it to survive
# has an obvious mount point.
ENV SANDESH_DATA_DIR=/data
VOLUME ["/data"]

# Bound to 0.0.0.0 inside the container, which is the one place it is correct:
# the container's network namespace *is* the boundary, and a loopback bind would
# make every published port unreachable. The desktop app binds loopback for the
# opposite reason (ADR-092).
ENV SANDESH_HTTP_ADDR=0.0.0.0:1080 \
    SANDESH_SMTP_ADDR=0.0.0.0:1025 \
    SANDESH_IMAP_ADDR=0.0.0.0:1143 \
    SANDESH_POP3_ADDR=0.0.0.0:1110

# SMTP, HTTP/UI, IMAP, POP3.
EXPOSE 1025 1080 1143 1110

# No `HEALTHCHECK`: it would need a shell or a curl binary, and adding either to
# `scratch` costs the property this base image exists for. Orchestrators should
# probe `GET /api/v1/health`, which is exempt from authentication precisely so
# that a liveness check has somewhere to point (ADR-075).

ENTRYPOINT ["/sandesh"]
