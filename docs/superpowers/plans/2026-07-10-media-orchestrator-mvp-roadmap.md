# Media Orchestrator MVP Delivery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement each focused plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver the approved media-orchestrator MVP through a sequence of independently testable vertical plans across `media-orchestrator`, `hermes-home`, and the existing homelab deployment.

**Architecture:** The product remains a Rust modular monolith released as one `media` binary, with HTTP/JSON boundaries between Hermes, `media-service`, and `download-runner`. PostgreSQL owns durable state, Rezka execution is isolated behind `gluetun-rezka`, qBittorrent remains authoritative for torrents, and Plex completion requires exact API verification.

**Tech Stack:** Rust 1.97.0, Tokio 1.52.3, Axum 0.8.9, SeaORM 2.0.0-rc.42, PostgreSQL, Reqwest, Serde, Tracing, Clap, mise, Docker Compose, HEVC VAAPI.

## Global Constraints

- The approved specification is `docs/superpowers/specs/2026-07-10-media-orchestrator-mvp-design.md`.
- Normal Cargo dependencies MUST remain acyclic and respect `docs/ARCHITECTURE.md`.
- `media-core` MUST perform no I/O and MUST NOT depend on transport, persistence, provider, process, or filesystem libraries.
- `download-runner` MUST NOT connect directly to PostgreSQL.
- One Rust multi-call binary MUST provide service, runner, migration, and Hermes-facing CLI commands.
- Every functional change MUST begin with a failing focused test and end with the narrowest relevant verification plus `mise run check`, `mise run lint`, and `mise run test`.
- Rezka is the only source transcoded; output MUST use HEVC VAAPI and MUST preserve actual source dimensions.
- Provider fallback, automatic torrent selection, browser fallback, torrent file mutation, and automatic media deletion remain excluded.
- Secrets, cookies, signed provider URLs, and tokens MUST NOT enter logs, fixtures, commits, or test snapshots.

---

## Planning Model

This roadmap is the dependency and delivery index. It is not a substitute for
the focused task-level plans. A phase starts only after its plan is written and
reviewed against the approved spec. Each phase ends with a working, reviewable
increment and a clean repository.

## Phase Sequence

### Phase 1: Rust Foundation

**Focused plan:** `docs/superpowers/plans/2026-07-10-rust-domain-foundation.md`

**Produces:** Pinned mise toolchain, Cargo workspace, `media-core`,
`media-contract`, initial `media` binary, core identity and job state policy,
architecture regression tests, and CI.

**Exit gate:** All workspace checks pass and forbidden dependencies in
`media-core` fail an automated architecture test.

### Phase 2: PostgreSQL and API Foundation

**Focused plan name:** `2026-07-10-postgres-api-foundation.md`

**Produces:** `media-storage`, explicit SeaORM migrations, fixed users,
canonical media and provider mappings, durable jobs/tasks/stages, Axum server,
fixed client identity, request IDs, idempotency, and the first CLI HTTP client.

**Exit gate:** PostgreSQL integration tests prove migration round trips,
ownership isolation, idempotent writes, and atomic single-job leasing.

### Phase 3: Rezka Session and Authentication

**Focused plan name:** `2026-07-10-rezka-session-authentication.md`

**Produces:** Independent `rezka-client` transport, Anubis detection and
proof-of-work, DLE login, typed errors, cookie import/export, encrypted runner
session store, and mock-server fixtures.

**Exit gate:** A restored cookie jar is validated, an invalid session performs
automatic challenge and login, and no secret material appears in errors or
tracing output.

### Phase 4: Rezka Discovery and Playback

**Focused plan name:** `2026-07-10-rezka-catalog-playback.md`

**Produces:** Rezka search, title metadata, translations, seasons, episodes,
stream qualities, subtitle discovery, provider snapshots, five-result
pagination, and canonical episode mapping inputs.

**Exit gate:** Fixture and mock-server tests cover movies, multi-season series,
translations, malformed provider responses, highest-quality selection, and all
subtitle tracks.

### Phase 5: Durable Job Orchestration

**Focused plan name:** `2026-07-10-job-leasing-recovery.md`

**Produces:** Job creation after explicit selection, lease and heartbeat API,
stage checkpoints, three-attempt retry policy, cooperative cancellation,
resume/restart semantics, `needs_action` reasons, outbox records, and one-active
job enforcement.

**Exit gate:** Killing a fake runner expires its lease and resumes only
unfinished work without duplicate state transitions or events.

### Phase 6: Rezka Runner and Plex Publication

**Focused plan name:** `2026-07-10-rezka-runner-publication.md`

**Produces:** Dedicated staging, 20 GiB storage guard, one-episode processing,
range-aware download recovery, ffprobe validation, HEVC VAAPI encoding,
subtitle sidecars, atomic publication, Plex scan, exact path/identity
verification, and `plex_pending` reconciliation.

**Exit gate:** A media fixture completes the full pipeline, a subtitle failure
produces `partial`, a wrong Plex match produces
`needs_action(reason=plex_mismatch)`, and a runner restart does not duplicate
published files.

### Phase 7: Prowlarr and qBittorrent

**Focused plan name:** `2026-07-10-prowlarr-qbittorrent.md`

**Produces:** Prowlarr ranking and pagination, explicit result submission,
existing category selection, qBittorrent monitoring, path discovery, and Plex
verification without torrent mutation.

**Exit gate:** The selected result reaches the chosen category, while tests
prove the system never moves, renames, hardlinks, transcodes, deletes, or
changes the seeding lifecycle of torrent data.

### Phase 8: Tracking and Notifications

**Focused plan name:** `2026-07-10-tracking-notifications.md`

**Produces:** Personal/family tracking, ongoing-series prompts, future episode
discovery without auto-download, transactional outbox dispatch, signed
`deliver_only` Hermes webhooks, initiator routing, and family routing.

**Exit gate:** Personal state is isolated, family tracking is manageable by
both users, retries do not duplicate Telegram delivery, and no LLM invocation
is needed to deliver a prepared notification.

### Phase 9: Hermes Integration

**Focused plan name:** `2026-07-10-hermes-home-integration.md`

**Produces:** Pinned derived Hermes image, installed `media` CLI, shared media
skill, isolated Primary and Secondary profiles, Telegram credentials, separate
memory/browser/Vaultwarden volumes, and narrow Bitwarden wrappers.

**Exit gate:** Each Hermes container authenticates as its fixed media user,
cannot access the other profile, and completes search, pagination, selection,
job status, and tracking flows through the CLI without `curl` or Docker access.

### Phase 10: Homelab Deployment and E2E

**Focused plan name:** `2026-07-10-homelab-media-deployment.md`

**Produces:** `/srv/homelab/media/compose.yml` integration for
PostgreSQL, service, runner, dedicated Gluetun, immutable image versions,
secrets, private networks, `/dev/dri`, encrypted session volume, Rezka storage
roots, Plex library paths, health checks, and operational runbooks.

**Exit gate:** `docker compose config --quiet` passes, all health checks become
ready, a live Rezka episode and selected Prowlarr result reach the correct Plex
items, notifications reach the initiating profile, and rollback is documented.

## Dependency Order

```text
Phase 1 -> Phase 2 -> Phase 3 -> Phase 4 -> Phase 5 -> Phase 6 -> Phase 7
                                              |
                                              +---------> Phase 8

Phase 7 + Phase 8 -> Phase 9 -> Phase 10
```

Phase 7 follows Phase 6 because its completion path reuses the exact Plex
verifier. Phase 8 may begin after the outbox and job events from Phase 5 exist.
Phase 9 waits for both provider flows and notification behavior. Deployment
work stays last so homelab contains only versioned application releases, not
development source trees.

## MVP Completion Gate

The MVP is complete only when all 17 acceptance criteria in the approved spec
are evidenced by automated tests or a recorded homelab E2E check. Passing a
phase test suite does not waive a later cross-system acceptance criterion.
