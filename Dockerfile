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

FROM ${RUST_IMAGE} AS certificates

FROM certificates AS yt-dlp
ARG TARGETARCH
ARG YT_DLP_VERSION=2026.07.04
RUN case "${TARGETARCH}" in \
      amd64) asset=yt-dlp_linux; checksum=6bbb3d314cde4febe36e5fa1d55462e29c974f63444e707871834f6d8cc210ae ;; \
      arm64) asset=yt-dlp_linux_aarch64; checksum=b6ce97646773070d7a7ffd6bbbdcaecb47c48483909c54c915bf08a7a9b5e0b1 ;; \
      *) echo "unsupported yt-dlp architecture: ${TARGETARCH}" >&2; exit 1 ;; \
    esac && \
    curl --fail --location --retry 3 \
      --output /usr/local/bin/yt-dlp \
      "https://github.com/yt-dlp/yt-dlp/releases/download/${YT_DLP_VERSION}/${asset}" && \
    echo "${checksum}  /usr/local/bin/yt-dlp" | sha256sum --check --strict && \
    chmod 0755 /usr/local/bin/yt-dlp

FROM ${RUNTIME_IMAGE} AS chrome-headless-shell
ARG TARGETARCH
RUN apt-get \
      -o Acquire::Retries=3 \
      -o Acquire::http::Timeout=20 \
      -o Acquire::https::Timeout=20 \
      update && \
    apt-get \
      -o Acquire::Retries=3 \
      -o Acquire::http::Timeout=20 \
      -o Acquire::https::Timeout=20 \
      install --yes --no-install-recommends ca-certificates curl unzip && \
    mkdir -p /usr/local/lib/chrome-headless-shell && \
    case "${TARGETARCH}" in \
      amd64) \
        version=152.0.7977.54; platform=linux64; checksum=11cedb5568cd374a76eb738e40bd434cd0c9956820fb406b8bd9edca53428d3e; \
        archive="chrome-headless-shell-${platform}.zip"; \
        curl --fail --location --retry 3 \
          --output "/tmp/${archive}" \
          "https://storage.googleapis.com/chrome-for-testing-public/${version}/${platform}/${archive}" && \
        echo "${checksum}  /tmp/${archive}" | sha256sum --check --strict && \
        unzip -q "/tmp/${archive}" -d /tmp && \
        rm -rf /usr/local/lib/chrome-headless-shell && \
        mv "/tmp/chrome-headless-shell-${platform}" /usr/local/lib/chrome-headless-shell && \
        chmod 0755 /usr/local/lib/chrome-headless-shell/chrome-headless-shell && \
        rm -f "/tmp/${archive}" ;; \
      arm64) \
        echo "chrome-headless-shell is amd64-only; Anubis browser fallback stays native on arm64" ;; \
      *) echo "unsupported chrome-headless-shell architecture: ${TARGETARCH}" >&2; exit 1 ;; \
    esac && \
    rm -rf /var/lib/apt/lists/*

FROM ${RUNTIME_IMAGE} AS runtime-base
COPY --from=certificates /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
RUN groupadd --gid 65532 media && \
    useradd --uid 65532 --gid 65532 --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin media
WORKDIR /var/empty

FROM runtime-base AS runtime-common
ARG OCI_CREATED="unknown"
ARG OCI_REVISION="unknown"
ARG OCI_RUNNER_BUILD_DIGEST="unknown"
ARG OCI_SOURCE="https://github.com/iamstubborn/media-orchestrator"
ARG OCI_SOURCE_TREE_DIGEST="unknown"
ARG OCI_VERSION="0.1.0-dev"
LABEL org.opencontainers.image.created=$OCI_CREATED \
      org.opencontainers.image.description="Personal media orchestration runtime" \
      org.opencontainers.image.licenses="LicenseRef-Proprietary" \
      org.opencontainers.image.revision=$OCI_REVISION \
      org.opencontainers.image.source=$OCI_SOURCE \
      org.opencontainers.image.title="media-orchestrator" \
      org.opencontainers.image.version=$OCI_VERSION \
      dev.iamstubborn.media.runner-build-digest=$OCI_RUNNER_BUILD_DIGEST \
      dev.iamstubborn.media.source-tree-digest=$OCI_SOURCE_TREE_DIGEST

FROM runtime-common AS service
COPY --from=builder --chown=65532:65532 /out/media /usr/local/bin/media
USER 65532:65532
EXPOSE 8080
HEALTHCHECK --interval=10s --timeout=5s --start-period=20s --retries=6 \
    CMD ["/usr/local/bin/media", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/media"]
CMD ["serve"]

FROM runtime-base AS runner-packages
ARG TARGETARCH
RUN apt-get \
      -o Acquire::Retries=3 \
      -o Acquire::http::Timeout=20 \
      -o Acquire::https::Timeout=20 \
      update && \
    case "${TARGETARCH}" in \
      amd64) vaapi_driver="intel-media-va-driver=23.1.1+dfsg1-1" ;; \
      arm64) vaapi_driver="" ;; \
      *) echo "unsupported runner architecture: ${TARGETARCH}" >&2; exit 1 ;; \
    esac && \
    apt-get \
      -o Acquire::Retries=3 \
      -o Acquire::http::Timeout=20 \
      -o Acquire::https::Timeout=20 \
      install --yes --no-install-recommends \
      ffmpeg=7:5.1.9-0+deb12u1 \
      libva-drm2=2.17.0-1 \
      libva2=2.17.0-1 \
      ${vaapi_driver} \
      fonts-liberation \
      libasound2 \
      libatk-bridge2.0-0 \
      libatk1.0-0 \
      libatspi2.0-0 \
      libcairo2 \
      libcups2 \
      libdbus-1-3 \
      libdrm2 \
      libexpat1 \
      libgbm1 \
      libglib2.0-0 \
      libnspr4 \
      libnss3 \
      libpango-1.0-0 \
      libudev1 \
      libx11-6 \
      libx11-xcb1 \
      libxcb1 \
      libxcomposite1 \
      libxdamage1 \
      libxext6 \
      libxfixes3 \
      libxkbcommon0 \
      libxrandr2 && \
    rm -rf /var/lib/apt/lists/*

FROM runner-packages AS runner
ARG OCI_CREATED="unknown"
ARG OCI_REVISION="unknown"
ARG OCI_RUNNER_BUILD_DIGEST="unknown"
ARG OCI_SOURCE="https://github.com/iamstubborn/media-orchestrator"
ARG OCI_SOURCE_TREE_DIGEST="unknown"
ARG OCI_VERSION="0.1.0-dev"
LABEL org.opencontainers.image.created=$OCI_CREATED \
      org.opencontainers.image.description="Personal media orchestration runtime" \
      org.opencontainers.image.licenses="LicenseRef-Proprietary" \
      org.opencontainers.image.revision=$OCI_REVISION \
      org.opencontainers.image.source=$OCI_SOURCE \
      org.opencontainers.image.title="media-orchestrator" \
      org.opencontainers.image.version=$OCI_VERSION \
      dev.iamstubborn.media.runner-build-digest=$OCI_RUNNER_BUILD_DIGEST \
      dev.iamstubborn.media.source-tree-digest=$OCI_SOURCE_TREE_DIGEST
COPY --from=builder --chown=65532:65532 /out/media /usr/local/bin/media
COPY --from=yt-dlp --chown=65532:65532 /usr/local/bin/yt-dlp /usr/local/bin/yt-dlp
COPY --from=chrome-headless-shell --chown=65532:65532 /usr/local/lib/chrome-headless-shell /usr/local/lib/chrome-headless-shell
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/media"]
CMD ["runner"]
