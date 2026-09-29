# media-orchestrator

Rust media orchestration for Hermes, Rezka, Prowlarr, qBittorrent, and Plex.

Project documentation:

- `docs/superpowers/specs/2026-07-10-media-orchestrator-mvp-design.md`
  is the canonical product design.
- `docs/ARCHITECTURE.md` defines Rust boundaries and dependency direction.
- `docs/superpowers/plans/2026-07-10-media-orchestrator-mvp-roadmap.md`
  defines the MVP delivery sequence.
- `docs/superpowers/plans/2026-07-10-rust-domain-foundation.md`
  is the first executable implementation plan.

This repository includes:

- The first-party Rust Rezka client.
- `media-service`, `download-runner`, and the `media` CLI.
- PostgreSQL entities and explicit SeaORM migrations.
- Docker images, API contracts, and tests.

Hermes profiles, skills, and deployment wiring live in the `homelab/hermes` module.

## Deployment Policy

Prefer manual deployment from a trusted operator workstation. GitHub Actions
workflows are retained for possible future use but must remain disabled; they
are not an accepted build, release, or deployment path for the homelab.

Use the guarded local deployment commands documented in `docs/RUNBOOK.md`.
They verify that no media job is active, build the images directly on the
Docker host, apply migrations, recreate services in dependency order, and run
post-deployment health checks.

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

For the normal edit-check-test loop, use the targeted tasks instead of paying
for every target and feature on each change:

```bash
mise run check:fast       # composed media binary and its dependency graph
mise run check:core       # domain, contracts, and client
mise run check:providers  # Rezka and other provider integrations
mise run check:runner     # download pipeline
mise run test:core
mise run test:providers
mise run test:runner
mise run cache:status
```

`mise install` provides a pinned prebuilt `sccache`, and commands run through
`mise` use it automatically. The cache is shared outside the repository, while
Cargo's incremental artifacts remain in `target/`. Third-party dependencies
omit debug information in local dev and test profiles to reduce compile time
and disk usage; workspace crates retain Cargo's normal debug information.

Use the targeted task matching the changed boundary while iterating, then run
the existing full `format`, `check`, `lint`, and `test` gates before deployment.
`mise run cache:status` reports both cache effectiveness and `target/` size.
When disk reclamation is actually needed, stop local Cargo processes and run
`mise exec -- cargo clean` explicitly; cleanup is intentionally never automatic.

`mise run test` uses default Cargo features and is Docker-independent.
`mise run test-integration` requires a running Docker daemon and enables only
the opt-in `integration-tests` features. It runs the full
PostgreSQL/Testcontainers suite once with the repository's pinned PostgreSQL 17
Alpine image. Provider and live-network tests are not part of either gate.

## Container Packaging

`Dockerfile` builds the same locked `media` binary into separate `service` and
`runner` runtime targets. The service target contains only CA certificates;
the runner additionally contains ffmpeg/ffprobe with VAAPI support and a
checksum-pinned `yt-dlp`. `yt-dlp` owns provider-neutral HTTP video transfer,
ffmpeg/VAAPI processes Rezka media, and qBittorrent remains the torrent engine.
Both images run as UID/GID 65532 and support a read-only root filesystem.

`compose.yaml` is a local-only stack with pinned PostgreSQL, `.env` configuration,
private service/database networking, and an opt-in runner profile. Copy
`.env.example` to `.env`, replace every placeholder, and keep the resulting file
mode at `0600`. Override
`MEDIA_SERVICE_IMAGE`, `MEDIA_RUNNER_IMAGE`, and `MEDIA_POSTGRES_IMAGE` when
testing homelab image references. The runner requires a Linux `/dev/dri` host:

```bash
docker compose up --detach --wait service
# Available when the media runner subcommand is present in the application release.
docker compose --profile runner up --detach runner
```

`mise run extract-linux-cli` writes a pinned Linux binary and SHA-256 file to
`dist/` for consumption by `homelab/hermes`. `MEDIA_CLI_PLATFORM`,
`MEDIA_CLI_ARCH`, and `MEDIA_CLI_OUTPUT_DIR` control the target and output.

## Hermes MCP and Human CLI

The authenticated Streamable HTTP MCP endpoint at `/internal/mcp` exposes
structured tools for provider search and pagination, exact downloads, jobs,
release schedules, trends, tracking, Plex library inspection, qBittorrent
status and controls, diagnostics within configured media roots, dependency
health, Plex library summaries, and media-root capacity.
The complete published toolset is recorded in
`config/media-capabilities.json`. Both Hermes profiles discover the surface
dynamically without a client-side tool allowlist. Owner identity, audit records,
and explicit confirmation for destructive actions remain enforced by the
service. Hermes uses this MCP boundary exclusively for conversational media
work. The CLI remains available to humans and deterministic notifier callbacks
through the REST API; neither adapter invokes the other.

The MCP endpoint supports current legacy Hermes negotiation and stateless MCP
`2026-07-28` requests. Search sessions, jobs, tracking subscriptions, and
destructive confirmations remain explicit durable application resources rather
than transport-session state. Tools publish structured output schemas and
read-only/destructive/idempotency annotations.

TMDB discovery is list-first and returns at most 10 cards per page. `media_best`
defaults to TMDB `top_rated`, with `popular` available explicitly.
`media_premieres` exposes `now_playing` and `upcoming` for movies, and
`on_the_air` and `airing_today` for TV. `media_genres` supplies localized IDs
for `media_discover`, whose results are ordered by descending popularity.
Full metadata remains available through `media_details`.

Secrets stay in `media-service`; Hermes has no Docker socket or direct provider
credentials. Both Hermes profiles can request Plex/qBittorrent mutations.
Deletions use preview and one-time confirmation, and direct file deletion is
replaced by quarantine.

## Local PostgreSQL Service

Development requires PostgreSQL 17 and Docker. Start a disposable local
database and export dummy local credentials:

```bash
docker run --rm --name media-postgres \
  -e POSTGRES_DB=media_orchestrator \
  -e POSTGRES_USER=media \
  -e POSTGRES_PASSWORD=media-local-password \
  -p 127.0.0.1:5432:5432 \
  postgres:17-alpine

export MEDIA_DATABASE_URL='postgres://media:media-local-password@127.0.0.1:5432/media_orchestrator'
export MEDIA_PRIMARY_TOKEN="$(openssl rand -hex 32)"
export MEDIA_SECONDARY_TOKEN="$(openssl rand -hex 32)"
export MEDIA_RUNNER_TOKEN="$(openssl rand -hex 32)"
```

In another shell, apply explicit migrations and start the service:

```bash
export MEDIA_LISTEN_ADDR=127.0.0.1:8080

mise exec -- cargo run -p media -- migrate
mise exec -- cargo run -p media -- serve
```

The CLI reads its own service URL and token. For example:

```bash
export MEDIA_SERVICE_URL=http://127.0.0.1:8080
export MEDIA_TOKEN="$MEDIA_PRIMARY_TOKEN"

mise exec -- cargo run -p media -- jobs create \
  --provider rezka \
  --result-ref rezka:series:42:season:1 \
  --json
mise exec -- cargo run -p media -- jobs get JOB_ID --json
mise exec -- cargo run -p media -- queue status --json
```

By default the CLI prints a concise human-readable view (aligned tables and
key-value blocks). Pass `--json` for the raw JSON response — the stable
machine contract consumed by `homelab/hermes`, unchanged byte-for-byte. Unknown
or absent fields degrade gracefully, and errors and exit codes are identical
in both modes.

The service also exposes an unauthenticated Prometheus `GET /metrics` endpoint
(text format) alongside `/v1/health` and `/v1/ready`, reporting job counts by
state, notification outbox gauges, HTTP request counters/latency histograms
labelled by matched route pattern, and build info. It is intended for the
private network only and carries no identifiers or secrets.

> **Warning:** `media serve` has no public route or public-ingress security
> contract. Bind it to loopback or a private container network only; do not
> expose it to the internet.

The Rust foundation now covers the full MVP delivery sequence: pure domain
types, versioned transport DTOs, explicit PostgreSQL migrations and
repositories, and the authenticated HTTP service and CLI sit alongside durable
job orchestration (leasing, heartbeat, checkpoints, retry, and the
notification outbox), the complete Rezka runner pipeline with real ffprobe and
ffmpeg/VAAPI adapters, Prowlarr/qBittorrent/Plex/Gluetun integrations, signed
`deliver_only` Hermes notifications, and five-result-paginated search
sessions. Tracking is notification-only by default and can explicitly enable
Rezka auto-download for a fixed result, translation, and season; the scheduler
then creates episode jobs without an LLM call. Hermes profile wiring and homelab deployment composition are tracked
in the `homelab/hermes` module and this repository. See the
[MVP roadmap](docs/superpowers/plans/2026-07-10-media-orchestrator-mvp-roadmap.md)
for the phase-by-phase delivery history and executable architecture checks
that enforce these boundaries.
