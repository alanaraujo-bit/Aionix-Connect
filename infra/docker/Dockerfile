# syntax=docker/dockerfile:1.7
#
# Aionix Connect server image: aionix-api, aionix-admin, aionix-signaling,
# aionix-relay and aionix-healthcheck in one image; each Compose service picks its
# binary with `command`. Build context = repository root:
#
#   docker build -f infra/docker/Dockerfile -t aionix-connect-server:dev .
#
# Builds only the Linux server crates; the Windows desktop crates are never compiled.

ARG RUST_IMAGE=rust:1.98-bookworm
ARG RUNTIME_IMAGE=gcr.io/distroless/cc-debian12:nonroot

FROM ${RUST_IMAGE} AS build
WORKDIR /src
ENV CARGO_TERM_COLOR=never \
    CARGO_NET_RETRY=5
COPY . .
# rust-toolchain.toml asks for "stable": use the toolchain already in the image
# instead of downloading another one.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    export RUSTUP_TOOLCHAIN="$(rustup default | cut -d' ' -f1)" \
 && cargo build --release --locked \
        -p aionix-control-plane -p aionix-signaling -p aionix-relay -p aionix-server-common \
        --bins \
 && mkdir -p /out \
 && cp target/release/aionix-api target/release/aionix-admin target/release/aionix-signaling \
       target/release/aionix-relay target/release/aionix-healthcheck /out/

FROM ${RUNTIME_IMAGE}
ARG AIONIX_VERSION=dev
LABEL org.opencontainers.image.title="aionix-connect-server" \
      org.opencontainers.image.description="Aionix Connect Control Plane, signaling and relay" \
      org.opencontainers.image.version="${AIONIX_VERSION}"
COPY --from=build /out/ /usr/local/bin/
# distroless "nonroot" = uid/gid 65532. Secret files mounted into the container must
# be readable by this uid (the deployment scripts chown them).
USER 65532:65532
ENV AIONIX_LOG_FORMAT=json
ENTRYPOINT []
CMD ["aionix-api"]
