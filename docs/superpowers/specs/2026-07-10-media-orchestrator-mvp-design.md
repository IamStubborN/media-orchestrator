# Personal Media System Design

**Status:** Approved

**Date:** 2026-07-10

**Repositories:** `media-orchestrator`, `hermes-home`, existing `homelab`

## 1. Purpose

Build a private, Docker-first media system controlled through two Hermes agents. The system searches Rezka and Prowlarr, lets a user explicitly choose a source and result, downloads or delegates the selected media, tracks future episodes when requested, publishes compatible files to Plex, and sends status notifications to the initiating user.

The implementation is also a practical Rust project covering asynchronous networking, HTML parsing, PostgreSQL, queues, process supervision, Docker, and typed API design.

## 2. Goals

- Support two users: `primary` and `secondary`.
- Keep shared media capabilities identical while preserving separate Hermes profiles, Telegram identities, memories, browser state, and personal skills.
- Search Rezka and Prowlarr independently with explicit user choice.
- Download Rezka content through a dedicated rotating VPN session.
- Delegate torrent placement and seeding to the existing qBittorrent categories.
- Transcode Rezka content only, using HEVC VAAPI and the real probed resolution.
- Download every subtitle track available for the selected Rezka translation.
- Track ongoing series only after the user explicitly agrees.
- Notify the initiating user by default, with an optional family scope.
- Run as pinned Docker images and remain operable without browser automation.

## 3. Non-goals for MVP

- Automatic fallback from Rezka to Prowlarr or the reverse.
- Automatic torrent selection or submission without user confirmation.
- Browser, Playwright, or Obscura fallback for Rezka.
- Torrent transcoding, relocation, hardlinking, or seeding management.
- Multiple concurrent download jobs or multiple runners.
- Automatic database backups.
- An LLM wiki or knowledge-base subsystem.
- A custom Telegram UI or mandatory callback buttons.
- Exposing the media API outside the private Docker network.

## 4. Repository Boundaries

### 4.1 `media-orchestrator`

Local path:

```text
/home/operator/Projects/personal/media-orchestrator
```

Responsibilities:

- Rust workspace and the `media` multi-call binary.
- Rezka client library.
- Media domain model and HTTP API.
- PostgreSQL entities and migrations.
- Download runner and provider integrations.
- CLI used by Hermes.
- Docker images for the service and runner.
- API schemas, tests, and release artifacts.

It does not contain Hermes profiles, Telegram tokens, user memory, browser state, or homelab secrets.

### 4.2 `hermes-home`

Local path:

```text
/home/operator/Projects/personal/hermes-home
```

Responsibilities:

- Official Hermes Docker image with mounted local extensions.
- Shared media skills.
- `primary` and `secondary` profile configuration.
- Personal skill folders and safe helper wrappers.
- Installation of a pinned `media` CLI release.
- Browser and Vaultwarden Password Manager CLI support.

It does not contain the Rust media implementation, media database, downloaded files, runtime memory, browser profiles, or secrets.

### 4.3 Existing `homelab`

Responsibilities:

- Compose deployment and image versions.
- Docker networks and volumes.
- Docker secrets.
- Gluetun configuration and control credentials.
- Bind mounts for staging, downloads, and Plex libraries.
- Existing Prowlarr, qBittorrent, and Plex wiring.

No application source code is duplicated into `homelab`.

## 5. Runtime Topology

```text
hermes-primary ----\
                   +--> media-service --> PostgreSQL
hermes-secondary -/          |
                             | leases and events over HTTP
                             v
                      download-runner
                             |
                      gluetun-rezka
                             |
                     Rezka / media CDN

media-service --> Prowlarr --> existing qBittorrent --> category-managed folders
media-service --> Plex
```

MVP containers:

```text
hermes-primary
hermes-secondary
media-service
download-runner
gluetun-rezka
media-postgres
```

The two Hermes containers use the same image but separate configuration, Telegram tokens, persistent memory volumes, browser profiles, and Vaultwarden sessions.

## 6. Rust Architecture and Workspace

The codebase is a modular monolith with hexagonal boundaries. It uses crates for real API, dependency, or process boundaries and modules for related implementation details inside those boundaries. It does not reproduce strict Clean Architecture as a separate crate for every layer and port.

Proposed layout:

```text
media-orchestrator/
  docs/
    ARCHITECTURE.md
    superpowers/
      specs/
        2026-07-10-media-orchestrator-mvp-design.md
  crates/
    media-core/
    rezka-client/
    media-storage/
    media-integrations/
    media-contract/
    media-api/
    media-runner/
    media/
  deploy/
  tests/
```

Crate responsibilities:

```text
media-core          domain types, state machines, use cases, and ports
rezka-client        independent Rezka protocol implementation
media-storage       SeaORM entities, migrations, and repositories
media-integrations  Prowlarr, qBittorrent, Plex, Gluetun, and webhooks
media-contract      versioned HTTP DTOs and public error codes
media-api           Axum routes, authentication, and idempotency
media-runner        leases, downloads, ffmpeg, VAAPI, and publication
media               composition root, configuration, and subcommands
```

Normal crate dependencies must always form a directed acyclic graph. If two crates require each other, the design must be corrected by merging strongly related code, moving a shared concept inward, or defining a consumer-owned port in `media-core`.

Architecture invariants:

- `media-core` has no Axum, SeaORM, Reqwest, Docker, JSON, or filesystem dependency.
- Ports are defined by the consuming use case, not by the adapter.
- Domain types are not serialized directly across HTTP; `media-contract` owns boundary DTOs.
- Adapters do not orchestrate each other; application use cases coordinate them.
- Only `media` constructs concrete adapters and connects the dependency graph.
- `thiserror` is used in libraries; `anyhow` is limited to process entry points and command composition.
- Crates named `common`, `shared`, or `utils` are prohibited. Shared code must have a concrete domain purpose.
- A new crate requires an independent API boundary, a heavy dependency boundary, meaningful reuse, or a separate runtime responsibility.
- `docs/ARCHITECTURE.md` is the maintained code map and records intentionally absent dependencies.

The `media` crate produces one multi-call binary:

```text
media serve
media runner
media migrate
media search ... --json
media jobs ... --json
media tracking ... --json
```

Hermes invokes the local CLI, not `curl`, `docker exec`, or `docker run`. The CLI calls `media-service` through HTTP/JSON. Hermes containers never receive the Docker socket.

## 7. Rust and Database Stack

Core dependencies:

```text
Tokio
Axum
SeaORM 2
SeaORM Migration
SeaQuery
PostgreSQL
Reqwest
Serde
Tracing
Thiserror
Clap
```

SeaORM is the normal persistence API for entities, relations, and CRUD. Explicit versioned migrations are mandatory. Production startup must not run entity schema synchronization.

Specialized PostgreSQL operations may use parameterized raw SQL through the same SeaORM connection and transaction. Job leasing is expected to use an atomic `UPDATE ... WHERE id = (SELECT ... FOR UPDATE SKIP LOCKED) RETURNING ...` operation.

Development starts from a pinned SeaORM 2 release candidate if no final 2.0 release exists. Before the first durable deployment, the dependency must be rechecked and upgraded to final SeaORM 2 when available.

Only `media-service` owns PostgreSQL credentials. The runner leases jobs and reports state through the private HTTP API and never connects directly to PostgreSQL.

## 8. Mise Tooling

The repositories use `mise` as the single developer entry point. `media-orchestrator/.mise.toml` pins an exact stable Rust version compatible with SeaORM 2 and installs:

```text
cargo-nextest
cargo-deny
cargo-audit
cargo-chef
sea-orm-cli
```

Expected tasks:

```text
mise run format
mise run check
mise run lint
mise run test
mise run test-integration
mise run migrate
mise run build
mise run docker-build
```

The repository must not depend on the machine's default nightly toolchain.

## 9. HTTP Contract

- HTTP/JSON under `/v1` is used for Hermes, service, and runner communication.
- gRPC is not part of MVP.
- Every write accepts an idempotency key.
- Every request carries a request ID and authenticated client identity.
- Each Hermes API token maps server-side to a fixed user; a model cannot spoof `requested_by`.
- Runner leases have an expiry, heartbeat, and idempotent event reporting.
- Provider-specific payload snapshots may be retained as JSONB for diagnostics.

### 9.1 Ownership and visibility

Every user-owned record stores an internal `owner_id`. The authenticated client
credential, not a request field supplied by Hermes or the LLM, determines that
owner:

```text
hermes-primary    -> primary
hermes-secondary -> secondary
```

MVP visibility and control rules are domain-specific instead of a generic ACL:

```text
search sessions   visible only to the initiating user
job queue          high-level availability visible to both users
job details        visible only to the initiating user
job control        cancel, resume, and restart belong to the initiating user
personal tracking visible and managed by its owner
family tracking   visible and managed by both users
published media   shared through the common Plex libraries
notifications     initiator by default, both users for explicit family scope
```

Administrative diagnostics remain a separate service capability and do not
change user ownership.

## 10. Search

Rezka and Prowlarr results are always presented separately. The user explicitly chooses a source and a concrete result before a job is created.

Rules:

- Five results per page for both providers.
- Natural-language pagination through Hermes, including `show more` and provider-specific variants.
- Search sessions are isolated by platform, chat, thread, and authenticated user.
- Search session TTL is 24 hours.
- A selected job remains permanently bound to its selected provider.
- There is no automatic cross-provider fallback.

Prowlarr ranking considers title and season match, quality, language, seeders, size, codec, and release group. Nothing is sent to qBittorrent until the user selects a result.

### 10.1 Canonical media identity and numbering

The system assigns stable internal `MediaId`, `SeasonId`, and `EpisodeId`
values. External identities are stored as mappings rather than used as primary
keys. Supported mapping namespaces include TMDb, TVDB, IMDb, AniList, Rezka,
Prowlarr result references, and Plex GUIDs.

Each series records the ordering expected by Plex:

```text
tmdb_aired
tvdb_aired
tvdb_dvd
tvdb_absolute
```

Provider episode numbers are mapped explicitly to canonical episodes. The
system never guesses when two mappings are plausible or no reliable mapping is
available. It changes the operation to `needs_action`, presents the ambiguity
to the initiating user, and persists the selected mapping so subsequent jobs
reuse the decision.

## 11. Rezka Client

`rezka-client` is a first-party Rust library with these internal modules:

```text
session      mirrors, cookies, DLE login, Anubis
catalog      search and title metadata
playback     translations, seasons, episodes, streams
subtitles    subtitle discovery and normalization
transport    timeouts, retries, response validation
error        typed public errors
```

### 11.1 Session flow

The client uses one cookie jar, stable User-Agent, and stable job IP for all title, AJAX, and CDN resolution requests.

Anubis and DLE authentication are separate layers:

1. Request the title page.
2. Detect an Anubis HTML response even when HTTP status is 200.
3. Parse the embedded challenge.
4. Solve `SHA-256(randomData + decimalNonce)` for the required number of leading zero hexadecimal nibbles.
5. Submit the challenge response using the same client identity and retain its cookie.
6. Perform DLE login or reuse a valid DLE session.
7. Continue with title, translation, episode, stream, and subtitle requests.

Rezka uses one shared service account that is independent from the two Hermes
users. Credentials enter the runner through Docker secrets. The client uses one
cookie jar for the complete job and persists it encrypted in a dedicated runner
volume. The encryption key is supplied through a separate Docker secret.

On runner startup and after a VPN IP change, the client validates the persisted
session. It re-runs Anubis and DLE login only when the session is invalid. Raw
cookies are never written to PostgreSQL, job payloads, notifications, or logs.
There is no browser or manually imported cookie fallback.

### 11.2 Errors

The library returns typed errors such as:

```text
challenge_required
challenge_failed
authentication_required
authentication_failed
title_not_found
translation_unavailable
episode_unavailable
quality_unavailable
stream_expired
provider_response_invalid
rate_limited
```

Provider messages and sanitized context are preserved for notifications and diagnostics.

## 12. Rezka Download Processing

- The user selects one translation.
- The runner selects the highest available quality for that translation.
- `ffprobe` determines the real codec, dimensions, duration, and bitrate.
- The reported Rezka label is never trusted as the actual resolution.
- Rezka video is transcoded to HEVC with VAAPI.
- The output preserves the actual source dimensions and never upscales 720p-class content to 1080p.
- `/dev/dri` is passed only to the runner.
- Processing is one episode at a time.

The confirmed reference case advertised as `1080p` produced H.264 video at `1280x682`, demonstrating why probing and truthful metadata are mandatory.

### 12.1 Subtitles

- Download every subtitle track available for the selected translation.
- No subtitles upstream is a valid result.
- Track each expected subtitle independently.
- Require a successful HTTP response, non-empty content, and valid WEBVTT structure.
- Write to a temporary path and rename atomically after validation.
- Retry only missing or invalid tracks.
- If video succeeds while some expected tracks fail, publish the video and mark the job `partial`.
- Plex-compatible sidecar names are used.

## 13. Series Jobs and Tracking

A series or season is one parent job containing sequential episode tasks. Every completed episode is checkpointed. One failed episode does not discard successful episodes; processing continues and the parent becomes `partial` when appropriate.

Downloading and tracking are separate actions:

```text
download series  -> download episodes currently available
track series     -> monitor future episode availability
```

If a title is ongoing, Hermes explains that not all episodes are available and
asks whether to create `personal` or `family` tracking. Tracking is
notification-only by default. The user may explicitly enable automatic Rezka
downloads after choosing a fixed result, translation, and season. In that mode
the service scheduler creates one job per newly discovered episode without an
LLM call or source fallback.

The parent job, each episode task, and every artifact record retain the initiating
`owner_id`. Personal tracking belongs to one user; family tracking can be viewed
and changed by either user. Tracking scope does not change the default rule that
download notifications go to the user who created the job.

## 14. Prowlarr and qBittorrent

`media-service` searches Prowlarr and submits only the explicitly selected result to qBittorrent with an existing category.

qBittorrent remains the sole owner of:

- Torrent download execution.
- Category-based placement and relocation.
- Seeding and retention.
- Torrent data lifecycle.

The runner does not transcode, move, hardlink, rename, or delete torrent files.
The media system monitors qBittorrent state, records errors, and notifies the
initiating user. After qBittorrent reports completion, `media-service` obtains
the content paths from qBittorrent, requests a targeted Plex scan, and applies
the same exact path and canonical identity verification used for Rezka. The job
remains `plex_pending` until that verification succeeds, without changing the
torrent or its seeding lifecycle.

## 15. VPN and Networking

- `media-service` is outside every VPN network namespace.
- Rezka traffic from `download-runner` uses a dedicated `gluetun-rezka` instance.
- Existing Prowlarr and qBittorrent continue using their current shared Gluetun instance.
- The existing Gluetun instance is never rotated for a Rezka job.
- One top-level Rezka job keeps one sticky VPN IP for its full movie, season, or series execution.
- After a terminal job state, the runner may request a VPN rotation before leasing the next job.
- Gluetun's control API requires authentication and is reachable only from the private control network.
- MVP allows one active download job.

## 16. Storage and Plex

- Rezka staging is outside Plex library roots at
  `/mnt/internal/media-orchestrator/staging/rezka/{job_id}`.
- Finished Rezka TV and movie files are published to dedicated roots at
  `/mnt/internal/media/rezka/tv` and `/mnt/internal/media/rezka/movies`.
- The existing Plex TV and movie libraries include those Rezka roots in
  addition to the qBittorrent-managed roots.
- Staging and finished Rezka roots live on the same filesystem so publication
  uses an atomic rename. There is no second move after publication.
- The runner has write access to Rezka staging and finished roots. The service
  has no write access to media files.
- Before each episode, compute expected peak usage from download, transcode,
  and publication requirements.
- Preserve at least 20 GiB of free space after the expected operation.
- Insufficient space changes the job to `blocked_storage` and notifies the initiator.
- Nothing published to Plex is automatically deleted.
- Incomplete working files are eligible for cleanup after seven days only when
  no active lease references them.
- qBittorrent category targets remain authoritative for torrents.

Recommended Plex naming:

```text
Movies/Title (Year) {tmdb-ID}/Title (Year).ext
TV/Title (Year) {tmdb-ID}/Season 01/Title (Year) - S01E01 - Episode Title.ext
```

Provider, translation, and source-quality details belong in PostgreSQL rather than the primary Plex filename.

Publication is successful only after all of these steps complete:

1. The runner validates the encoded video and expected subtitle manifest.
2. The runner atomically publishes the sidecars and final video path.
3. `media-service` requests a targeted Plex library scan.
4. `media-service` polls the Plex HTTP API for a bounded period.
5. The returned Plex media part path, canonical identity, season, and episode
   match the expected publication.

Subtitle completeness is authoritative in the runner artifact manifest and
filesystem validation; Plex subtitle discovery is an additional observation.
Temporary Plex unavailability leaves the operation in `plex_pending` for later
reconciliation. An incorrect match becomes
`needs_action(reason=plex_mismatch)`; the system does not rename or delete the
media automatically.

## 17. Job States and Retention

Core terminal and operational states:

```text
queued
leased
running
cancel_requested
blocked_storage
publishing
plex_pending
needs_action
partial
completed
failed
cancelled
```

`needs_action` carries a machine-readable reason such as
`identity_ambiguous` or `plex_mismatch`; those reasons are not separate job
states.

The service leases work to a runner through PostgreSQL-backed atomic leasing.
The runner sends heartbeats through the API. An expired lease makes the job
eligible for recovery without allowing two runners to own it concurrently.

Every episode and processing stage has an idempotent checkpoint. Recovery
skips completed episodes and stages. An interrupted HTTP download resumes only
when the server supports a compatible range request; otherwise only the current
episode restarts. Subtitle recovery retries only missing or invalid tracks.
Each stage receives three automatic attempts before becoming `failed`.

User operations have distinct meanings:

```text
cancel   request cooperative process termination; keep published media
resume   continue from durable checkpoints
restart  discard unfinished temporary state and repeat the unfinished work
remove   a separate explicit operation for published media, outside automatic recovery
```

Cancellation never rolls back episodes already published to Plex. Temporary
artifacts remain eligible for the normal seven-day cleanup policy.

Retention defaults:

```text
search sessions       24 hours
job history           90 days
incomplete artifacts   7 days
published media        never automatically deleted
```

Automatic database backup is intentionally excluded from MVP. PostgreSQL uses a persistent volume; losing that volume loses queue, history, tracking, and notification state but not already published media.

## 18. Notifications

Every job stores:

```json
{
  "requested_by": "primary",
  "notify_scope": "initiator"
}
```

Default routing:

```text
primary initiated    -> hermes-primary
secondary initiated -> hermes-secondary
family scope        -> both
```

`media-service` writes notification events to a transactional outbox. A dispatcher sends a signed, structured `deliver_only` webhook to the correct Hermes instance. Hermes delivers the message to Telegram without invoking the LLM.

Events include job start, completion, encoding completion, Plex publication, partial completion, storage block, tracking discovery, and failure with a sanitized provider error.

## 19. Hermes Repository Design

Proposed structure:

```text
hermes-home/
  docker/
  shared/
    skills/
      media/
  profiles/
    primary/
      skills/
      config/
    secondary/
      skills/
      config/
  scripts/
```

Only `shared/skills` and the selected user's profile are mounted into each
runtime configuration. Personal skills are never mounted into the other user's
container.

The runtime uses:

- The official Hermes image, updated by the homelab container update policy.
- The pinned `media` CLI release.
- Mounted profile configuration, skills, browser tooling, and notification adapter.
- The Bitwarden Password Manager CLI (`bw`) for Vaultwarden, behind narrow allowlisted wrappers.

Vaultwarden does not support Bitwarden Secrets Manager (`bws`), so `bws` is not part of this design. Master passwords, session tokens, and secret values must not enter prompts or logs. Runtime memory, browser profiles, and Password Manager sessions live in separate persistent volumes for each user.

## 20. Security

- All repositories are private.
- No secrets, cookies, Telegram tokens, VPN credentials, or Password Manager sessions are committed.
- Docker secrets are used for service credentials.
- The shared Rezka account password and cookie-encryption key are separate
  Docker secrets.
- The encrypted Rezka cookie jar is stored only in a runner-owned persistent
  volume with restrictive filesystem permissions.
- Hermes has no Docker socket.
- API tokens are scoped per client and mapped to server-side identities.
- Internal webhook payloads are signed and replay-protected.
- Provider URLs containing tokens are redacted from logs.
- Containers use private networks and expose only required internal ports.
- Images and release binaries are pinned by immutable version or digest.

## 21. Observability

- Structured JSON logs through `tracing`.
- Request, job, task, user, and provider identifiers on every relevant span.
- Health endpoints for service, runner, PostgreSQL reachability, Gluetun readiness, and VAAPI availability.
- Provider response bodies are sanitized before persistence.
- Job events form an append-only diagnostic timeline.

## 22. Testing Strategy

- Unit tests for domain transitions, ranking, naming, pagination, and retention.
- Dependency checks that reject forbidden inward dependencies and cyclic workspace dependencies.
- Fixture tests for Rezka pages, Anubis challenges, DLE responses, translations, episodes, qualities, and subtitles.
- Mock HTTP integration tests for retry, expiry, authentication, and malformed responses.
- PostgreSQL integration tests for migrations, idempotency, leases, outbox, and concurrent `SKIP LOCKED` behavior.
- CLI contract tests for stable JSON output consumed by Hermes skills.
- Docker integration tests for service-to-runner networking and secret mounts.
- Opt-in live Rezka probe tests that never run in normal CI.
- Media verification with `ffprobe` and a short VAAPI encode fixture.

## 23. MVP Acceptance Criteria

1. Each Hermes profile can search Rezka and Prowlarr and receive five isolated results per page.
2. Pagination works through natural-language requests interpreted by Hermes.
3. No download begins before explicit source, result, and translation selection.
4. The Rust Rezka client passes Anubis and DLE authentication without a browser.
5. A Rezka episode downloads, is probed, encoded through VAAPI, published with truthful dimensions, and includes all valid subtitle sidecars.
6. Subtitle failures produce `partial` without discarding a valid video.
7. A selected Prowlarr result is submitted with the requested qBittorrent category and is never transcoded or relocated by this system.
8. One active job retains one VPN IP and the next job can rotate to a new session.
9. Notifications reach the initiating Hermes profile; family scope reaches both.
10. An ongoing series can be downloaded without tracking, and tracking can be added separately.
11. Storage guard prevents an operation that would violate the 20 GiB reserve.
12. The complete stack starts through the homelab Docker deployment with no public media API exposure.
13. Ambiguous provider numbering becomes `needs_action` and a resolved mapping
    is reused by subsequent jobs.
14. A killed runner loses its lease and resumes from the last durable episode
    or stage checkpoint without duplicating published media.
15. A Rezka publication reaches `completed` only after Plex reports the exact
    expected media part path and canonical episode identity.
16. Search details and job control remain private to the initiating user while
    family tracking remains manageable by both users.
17. A persisted Rezka session survives runner restart, remains encrypted at
    rest, and is re-authenticated automatically when invalid after VPN rotation.

## 24. Delivery Order

The implementation should proceed in independently reviewable vertical slices:

1. Rust workspace, mise, CI, architecture checks, `media-core`, `media-contract`, PostgreSQL, and migrations.
2. Rezka session transport, Anubis, authentication, and fixture tests.
3. Rezka catalog, playback, quality normalization, and subtitles.
4. Media API, search sessions, identity, jobs, leases, and outbox.
5. Runner download, storage guard, ffprobe, VAAPI, publication, and partial recovery.
6. Prowlarr, qBittorrent category submission, monitoring, and Plex refresh.
7. Tracking and notification delivery.
8. Hermes image, shared skill, profile isolation, and CLI integration.
9. Homelab deployment and end-to-end verification.

This is an umbrella product specification spanning multiple independently
reviewable subsystems. Detailed implementation planning must produce a sequence
of focused plans following the delivery order above rather than one monolithic
execution plan. Planning begins only after this written design is reviewed and
approved.
