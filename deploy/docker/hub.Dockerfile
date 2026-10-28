# syntax=docker/dockerfile:1.7
#
# Wyrd Hub container image: a static-ish, non-root, read-only-rootfs image
# with no shell and no package manager. Built in CI only (.github/workflows/
# hub-image.yml), where it is scanned, SBOM'd, signed and attested.
#
# Base images are pinned by manifest-list digest; bump them deliberately.

ARG RUST_IMAGE=rust:1.99.0-bookworm@sha256:114c7a4425406451c2866b6aafe69fe29b1b298832db1277d411ac73c82d04d6
ARG RUNTIME_IMAGE=gcr.io/distroless/cc-debian13:nonroot@sha256:3ee00291cf386ec52a552e6a858d555debd74776eb4bfa53fa3df2cd6677515e

FROM ${RUST_IMAGE} AS build
WORKDIR /src
# The Cargo workspace (members = crates/*) must be complete for the lockfile
# to resolve; .dockerignore drops the Studio, docs and tooling trees.
COPY . .
# Same path remapping as the release build, so the binary carries no
# builder paths.
ENV RUSTFLAGS="--remap-path-prefix=/src=/wyrdline --remap-path-prefix=/usr/local/cargo=/cargo"
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    cargo build --locked --release -p wyrd-hub --bin wyrd-hub \
 && install -D -m 0755 target/release/wyrd-hub /out/usr/local/bin/wyrd-hub \
 && install -d -m 0750 /out/data

FROM ${RUNTIME_IMAGE}

LABEL org.opencontainers.image.title="wyrd-hub" \
      org.opencontainers.image.description="Wyrdline Hub: self-hosted team server for shared bundles" \
      org.opencontainers.image.source="https://github.com/Kunj157/wyrdline" \
      org.opencontainers.image.licenses="MIT"

# Listen on all interfaces inside the container (the binary defaults to
# loopback). Data dir is a writable volume; the rootfs stays read-only.
ENV WYRD_HUB_LISTEN=0.0.0.0:7730 \
    WYRD_HUB_DATA_DIR=/data

COPY --from=build --chown=65532:65532 /out/data /data
COPY --from=build /out/usr/local/bin/wyrd-hub /usr/local/bin/wyrd-hub

# distroless :nonroot is uid/gid 65532; stated explicitly for admission
# policies that check the USER directive.
USER 65532:65532
EXPOSE 7730
ENTRYPOINT ["/usr/local/bin/wyrd-hub"]
