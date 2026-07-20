# syntax=docker/dockerfile:1.7

FROM rust:1.91-alpine@sha256:45c1c35cd364b8055e9e86f8ecd3e8c874b2dcb658d8a4f94b5d111aa0d651a2 AS builder

RUN apk add --no-cache musl-dev
WORKDIR /build

COPY Cargo.toml Cargo.lock ./
# Cargo.lock covers the whole workspace, so manifests for every workspace member
# must be present even though only spurfire-server and its dependencies compile.
COPY crates/ crates/
COPY vendor/boringtun/ vendor/boringtun/

ARG TARGETARCH
RUN --mount=type=cache,id=spurfire-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=spurfire-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=spurfire-target-${TARGETARCH},target=/build/target,sharing=locked \
    CARGO_PROFILE_RELEASE_LTO=thin \
    CARGO_PROFILE_RELEASE_STRIP=symbols \
    cargo build --release --locked -p spurfire-server \
    && mkdir -p /out \
    && cp /build/target/release/spurfire-server /out/spurfire-server \
    && chmod 0755 /out/spurfire-server

FROM alpine:3.22@sha256:14358309a308569c32bdc37e2e0e9694be33a9d99e68afb0f5ff33cc1f695dce

ARG SPURFIRE_VERSION=dev
ARG SPURFIRE_REVISION=unknown
ARG SPURFIRE_CREATED=unknown

LABEL org.opencontainers.image.title="spurfire-server" \
      org.opencontainers.image.description="Spurfire lobby control service" \
      org.opencontainers.image.url="https://github.com/rajsinghtech/spurfire" \
      org.opencontainers.image.source="https://github.com/rajsinghtech/spurfire" \
      org.opencontainers.image.documentation="https://github.com/rajsinghtech/spurfire/blob/main/docs/deployment.md" \
      org.opencontainers.image.licenses="BSD-3-Clause" \
      org.opencontainers.image.version="${SPURFIRE_VERSION}" \
      org.opencontainers.image.revision="${SPURFIRE_REVISION}" \
      org.opencontainers.image.created="${SPURFIRE_CREATED}"

RUN apk add --no-cache ca-certificates \
    && addgroup -S -g 10001 spurfire \
    && adduser -S -D -H -u 10001 -G spurfire spurfire \
    && mkdir -p /app /var/lib/spurfire /usr/share/licenses/spurfire \
    && chown 10001:10001 /var/lib/spurfire \
    && chmod 0755 /app /usr/share/licenses/spurfire \
    && chmod 0750 /var/lib/spurfire

COPY --from=builder --chown=0:0 /out/spurfire-server /usr/local/bin/spurfire-server
COPY --chown=0:0 LICENSE /usr/share/licenses/spurfire/LICENSE

ENV SPURFIRE_BIND_ADDR=0.0.0.0:8080 \
    SPURFIRE_STATE_PATH=/var/lib/spurfire/server-state.json \
    TS_API_BASE=https://api.tailscale.com/api/v2

WORKDIR /app
USER 10001:10001
EXPOSE 8080
VOLUME ["/var/lib/spurfire"]
STOPSIGNAL SIGTERM

# This is process liveness only. Readiness must inspect provisioning_ready.
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD wget -q -O /dev/null http://127.0.0.1:8080/healthz || exit 1

ENTRYPOINT ["/usr/local/bin/spurfire-server"]
