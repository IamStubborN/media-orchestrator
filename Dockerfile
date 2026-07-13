# syntax=docker/dockerfile:1.18@sha256:dabfc0969b935b2080555ace70ee69a5261af8a8f1b4df97b9e7fbcf6722eddf

ARG RUST_IMAGE=rust:1.97.0-bookworm@sha256:7d0723df719e7f213b69dc7c8c595985c3f4b060cfbee4f7bc0e347a86fe3b6a
ARG RUNTIME_IMAGE=debian:bookworm-slim@sha256:60eac759739651111db372c07be67863818726f754804b8707c90979bda511df

FROM ${RUST_IMAGE} AS chef
RUN cargo install cargo-chef --version 0.1.77 --locked
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
ARG TARGETARCH
COPY --from=planner /app/recipe.json recipe.json
RUN --mount=type=cache,id=media-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=media-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=media-target-${TARGETARCH},target=/app/target,sharing=locked \
    cargo chef cook --release --locked --package media
COPY . .
RUN --mount=type=cache,id=media-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=media-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=media-target-${TARGETARCH},target=/app/target,sharing=locked \
    cargo build --release --locked --package media && \
    install -D -m 0755 target/release/media /out/media && \
    strip /out/media

FROM scratch AS cli-artifact
COPY --from=builder /out/media /media

FROM ${RUNTIME_IMAGE} AS certificates
RUN apt-get update && \
    apt-get install --yes --no-install-recommends ca-certificates=20230311+deb12u1 && \
    rm -rf /var/lib/apt/lists/*

FROM ${RUNTIME_IMAGE} AS runtime-common
ARG OCI_CREATED="unknown"
ARG OCI_REVISION="unknown"
ARG OCI_SOURCE="https://github.com/iamstubborn/media-orchestrator"
ARG OCI_VERSION="0.1.0-dev"
LABEL org.opencontainers.image.created=$OCI_CREATED \
      org.opencontainers.image.description="Personal media orchestration runtime" \
      org.opencontainers.image.licenses="LicenseRef-Proprietary" \
      org.opencontainers.image.revision=$OCI_REVISION \
      org.opencontainers.image.source=$OCI_SOURCE \
      org.opencontainers.image.title="media-orchestrator" \
      org.opencontainers.image.version=$OCI_VERSION
COPY --from=certificates /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
RUN groupadd --gid 65532 media && \
    useradd --uid 65532 --gid 65532 --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin media
WORKDIR /var/empty

FROM runtime-common AS service
COPY --from=builder --chown=65532:65532 /out/media /usr/local/bin/media
USER 65532:65532
EXPOSE 8080
HEALTHCHECK --interval=10s --timeout=5s --start-period=20s --retries=6 \
    CMD ["/usr/local/bin/media", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/media"]
CMD ["serve"]

FROM runtime-common AS runner-packages
RUN apt-get update && \
    apt-get install --yes --no-install-recommends \
      ffmpeg=7:5.1.9-0+deb12u1 \
      intel-media-va-driver=23.1.1+dfsg1-1 \
      libva-drm2=2.17.0-1 \
      libva2=2.17.0-1 && \
    rm -rf /var/lib/apt/lists/*

FROM runner-packages AS runner
COPY --from=builder --chown=65532:65532 /out/media /usr/local/bin/media
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/media"]
CMD ["runner"]
