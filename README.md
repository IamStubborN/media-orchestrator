# media-orchestrator

Private Rust media orchestration for Hermes, Rezka, Prowlarr, qBittorrent, and Plex.

Project documentation:

- `docs/superpowers/specs/2026-07-10-media-orchestrator-mvp-design.md`
  is the canonical product design.
- `docs/ARCHITECTURE.md` defines Rust boundaries and dependency direction.
- `docs/superpowers/plans/2026-07-10-media-orchestrator-mvp-roadmap.md`
  defines the MVP delivery sequence.
- `docs/superpowers/plans/2026-07-10-rust-domain-foundation.md`
  is the first executable implementation plan.

This repository will contain:

- The first-party Rust Rezka client.
- `media-service`, `download-runner`, and the `media` CLI.
- PostgreSQL entities and explicit SeaORM migrations.
- Docker images, API contracts, and tests.

Hermes profiles and skills belong in the separate private `hermes-home` repository. Homelab deployment wiring belongs in the existing `homelab` repository.

## Development

The repository uses `mise` as its only supported developer entry point:

```bash
mise trust
mise install
mise run format
mise run check
mise run lint
mise run test
mise run test-integration
mise run audit
mise run build
mise run docker-lint
mise run docker-build
mise run docker-smoke
mise run extract-linux-cli
```

`mise run test` uses default Cargo features and is Docker-independent.
`mise run test-integration` requires a running Docker daemon and enables only
the opt-in `integration-tests` features. It runs the full
PostgreSQL/Testcontainers suite once with the repository's pinned PostgreSQL 17
Alpine image. Provider and live-network tests are not part of either gate.

## Container Packaging

`Dockerfile` builds the same locked `media` binary into separate `service` and
`runner` runtime targets. The service target contains only CA certificates;
the runner additionally contains ffmpeg/ffprobe with VAAPI support. Both run
as UID/GID 65532 and support a read-only root filesystem.

`compose.yaml` is a local-only stack with pinned PostgreSQL, example secrets,
private service/database networking, and an opt-in runner profile. Override
`MEDIA_SERVICE_IMAGE`, `MEDIA_RUNNER_IMAGE`, and `MEDIA_POSTGRES_IMAGE` when
testing homelab image references. The runner requires a Linux `/dev/dri` host:

```bash
docker compose up --detach --wait service
# Available when the media runner subcommand is present in the application release.
docker compose --profile runner up --detach runner
```

`mise run extract-linux-cli` writes a pinned Linux binary and SHA-256 file to
`dist/` for consumption by `hermes-home`. `MEDIA_CLI_PLATFORM`,
`MEDIA_CLI_ARCH`, and `MEDIA_CLI_OUTPUT_DIR` control the target and output.

## Local PostgreSQL Service

Development requires PostgreSQL 17 and Docker. Start a disposable local
database and prepare secret files with dummy local credentials:

```bash
docker run --rm --name media-postgres \
  -e POSTGRES_DB=media_orchestrator \
  -e POSTGRES_USER=media \
  -e POSTGRES_PASSWORD=media-local-password \
  -p 127.0.0.1:5432:5432 \
  postgres:17-alpine

export MEDIA_SECRETS_DIR="${TMPDIR:-/tmp}/media-orchestrator-secrets"
mkdir -p "$MEDIA_SECRETS_DIR"
printf '%s\n' \
  'postgres://media:media-local-password@127.0.0.1:5432/media_orchestrator' \
  > "$MEDIA_SECRETS_DIR/database-url"
openssl rand -hex 32 > "$MEDIA_SECRETS_DIR/primary-token"
openssl rand -hex 32 > "$MEDIA_SECRETS_DIR/secondary-token"
openssl rand -hex 32 > "$MEDIA_SECRETS_DIR/runner-token"
```

In another shell, apply explicit migrations and start the service:

```bash
export MEDIA_SECRETS_DIR="${TMPDIR:-/tmp}/media-orchestrator-secrets"
export MEDIA_DATABASE_URL_FILE="$MEDIA_SECRETS_DIR/database-url"
export MEDIA_PRIMARY_TOKEN_FILE="$MEDIA_SECRETS_DIR/primary-token"
export MEDIA_SECONDARY_TOKEN_FILE="$MEDIA_SECRETS_DIR/secondary-token"
export MEDIA_RUNNER_TOKEN_FILE="$MEDIA_SECRETS_DIR/runner-token"
export MEDIA_LISTEN_ADDR=127.0.0.1:8080

mise exec -- cargo run -p media -- migrate
mise exec -- cargo run -p media -- serve
```

The CLI reads its own service URL and token file. For example:

```bash
export MEDIA_SERVICE_URL=http://127.0.0.1:8080
export MEDIA_SECRETS_DIR="${TMPDIR:-/tmp}/media-orchestrator-secrets"
export MEDIA_TOKEN_FILE="$MEDIA_SECRETS_DIR/primary-token"

mise exec -- cargo run -p media -- jobs create \
  --provider rezka \
  --result-ref rezka:series:42:season:1 \
  --json
mise exec -- cargo run -p media -- jobs get JOB_ID --json
mise exec -- cargo run -p media -- queue status --json
```

> **Warning:** `media serve` has no public route or public-ingress security
> contract. Bind it to loopback or a private container network only; do not
> expose it to the internet.

The current Rust foundation includes pure domain types, versioned transport
DTOs, explicit PostgreSQL migrations and repositories, the authenticated HTTP
service and CLI, and executable architecture checks. Network providers,
filesystem access, ffmpeg, and other runtime behavior remain scoped to later
plans in the
[MVP roadmap](docs/superpowers/plans/2026-07-10-media-orchestrator-mvp-roadmap.md).
