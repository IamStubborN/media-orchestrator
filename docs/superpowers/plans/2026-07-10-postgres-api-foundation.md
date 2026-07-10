# PostgreSQL and API Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add durable PostgreSQL persistence, fixed authenticated client identities, idempotent job creation, atomic single-runner leasing, a private Axum `/v1` API, and the first HTTP-backed `media` CLI commands without coupling transport handlers to SeaORM.

**Architecture:** `media-core` owns transport-neutral actors, job application services, and narrow async ports. `media-storage` owns SeaORM entities, explicit migrations, PostgreSQL transactions, and implementations of domain persistence ports. `media-api` owns Axum middleware, HTTP idempotency semantics, DTO conversion, and handlers while depending only on `media-core` and `media-contract`. The `media` composition root reads configuration and secret files, constructs concrete adapters, bridges the API-owned idempotency port to storage, and wires them into service and CLI processes.

**Tech Stack:** Rust 1.97.0, SeaORM/SeaORM Migration 2.0.0-rc.42, PostgreSQL 17, Axum 0.8.9, Tokio 1.52.3, Tower 0.5.3, Tower HTTP 0.7.0, Reqwest 0.13.4, async-trait 0.1.89, SHA-2 0.11.0, secrecy 0.10.3, time 0.3.53, testcontainers 0.27.3.

## Global Constraints

- Follow `docs/superpowers/specs/2026-07-10-media-orchestrator-mvp-design.md` and `docs/ARCHITECTURE.md`.
- `media-core` remains free of Axum, SeaORM, Reqwest, Serde, JSON, environment variables, filesystem paths, and provider response models.
- `media-api` MUST NOT depend on `media-storage`; `media-storage` MUST NOT depend on `media-api` or `media-contract`.
- SeaORM entities MUST never escape `media-storage`; every adapter maps them into domain types.
- All cross-crate behavior passes through narrow consumer-owned traits. No unrestricted database handle, HTTP client, or application context enters a domain use case.
- Only `media-service` receives PostgreSQL credentials. Runner-facing operations are private HTTP calls.
- User identity comes only from the authenticated API client record. Create-job requests MUST NOT contain `owner_id`, `requested_by`, or another impersonation field.
- Hermes clients map to exactly `primary` or `secondary`; runner clients have no user identity and cannot access user job details.
- Every write requires `Idempotency-Key`; duplicate same-body requests replay, different-body reuse conflicts, and in-progress reuse conflicts.
- One active lease is enforced transactionally in PostgreSQL. Concurrent lease requests can return at most one lease.
- Production startup MUST NOT run schema synchronization. Only `media migrate` applies explicit migrations.
- Tokens are read from secret files, hashed with SHA-256 before persistence, and never logged or returned.
- The PostgreSQL URL is read from `MEDIA_DATABASE_URL_FILE`; it is never accepted as a production command argument or emitted through debug output.
- JSON request bodies are limited to 64 KiB, bearer tokens to 512 bytes, request/idempotency IDs to 128 visible ASCII bytes, and buffered idempotent responses to 1 MiB.
- Idempotency reservations expire after 24 hours. Lease TTL is server-configured, defaults to 60 seconds, and is constrained to 30-300 seconds. Runner requests cannot choose or extend the configured TTL.
- Use PostgreSQL integration tests for constraints, migrations, idempotency, and leasing; mocks alone are insufficient.
- Every behavior change follows RED-GREEN-REFACTOR and every task ends with focused tests plus `mise run check`, `mise run lint`, and `mise run test`.

---

## Public Runtime Contract for This Phase

```text
media migrate
media serve
media jobs create --provider <rezka|prowlarr> --result-ref <value> --json
media jobs get <job-id> --json
media queue status --json
```

```text
GET  /v1/health
GET  /v1/ready
POST /v1/jobs
GET  /v1/jobs/{job_id}
GET  /v1/queue/status
POST /v1/runner/leases
POST /v1/runner/leases/{lease_id}/heartbeat
```

The API remains private and has no Traefik route in this phase.

## Fixed Identities

```text
primary user      00000000-0000-0000-0000-000000000001
secondary user   00000000-0000-0000-0000-000000000002
primary client    00000000-0000-0000-0001-000000000001
secondary client 00000000-0000-0000-0001-000000000002
runner client    00000000-0000-0000-0002-000000000001
```

The two user rows are seeded by migration. Client rows are upserted from these
secret-file settings at service startup:

```text
MEDIA_PRIMARY_TOKEN_FILE
MEDIA_SECONDARY_TOKEN_FILE
MEDIA_RUNNER_TOKEN_FILE
```

## File Map

```text
crates/media-core/src/actor.rs
crates/media-core/src/application.rs
crates/media-core/src/port.rs
crates/media-storage/Cargo.toml
crates/media-storage/src/lib.rs
crates/media-storage/src/entity/*.rs
crates/media-storage/src/migration/*.rs
crates/media-storage/src/repository/*.rs
crates/media-storage/tests/*.rs
crates/media-api/Cargo.toml
crates/media-api/src/lib.rs
crates/media-api/src/auth.rs
crates/media-api/src/error.rs
crates/media-api/src/idempotency.rs
crates/media-api/src/request_id.rs
crates/media-api/src/route/*.rs
crates/media-api/tests/*.rs
crates/media/src/client.rs
crates/media/src/config.rs
crates/media/src/main.rs
tests/postgres.rs
```

## Task 1: Domain Actors, Jobs, and Async Ports

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/media-core/Cargo.toml`
- Modify: `crates/media-core/src/id.rs`
- Modify: `crates/media-core/src/job.rs`
- Create: `crates/media-core/src/actor.rs`
- Create: `crates/media-core/src/port.rs`
- Create: `crates/media-core/src/application.rs`
- Modify: `crates/media-core/src/lib.rs`

**Interfaces:**

```rust
pub enum ClientRole { Hermes, Runner }
pub struct Actor { client_id: ClientId, user_id: Option<UserId>, role: ClientRole }
pub struct CredentialDigest([u8; 32]);
pub struct BootstrapClient { client_id: ClientId, name: String, role: ClientRole, user_id: Option<UserId>, digest: CredentialDigest }
pub enum Provider { Rezka, Prowlarr }
pub enum NotifyScope { Initiator, Family }
pub struct Job { /* private fields; validated rehydration and read-only accessors */ }
pub struct NewJob { /* private fields; validated constructor and read-only accessors */ }
pub struct QueueStatus { pub queued: u64, pub active: bool }
pub struct JobLease { /* private fields and read-only accessors */ }
```

```rust
#[async_trait::async_trait]
pub trait ClientStore: Send + Sync {
    async fn find_by_digest(&self, digest: CredentialDigest) -> Result<Option<Actor>, PortError>;
    async fn upsert_client(&self, client: BootstrapClient) -> Result<(), PortError>;
}

#[async_trait::async_trait]
pub trait JobStore: Send + Sync {
    async fn create(&self, job: NewJob) -> Result<Job, PortError>;
    async fn find_for_owner(&self, id: JobId, owner: UserId) -> Result<Option<Job>, PortError>;
    async fn queue_status(&self) -> Result<QueueStatus, PortError>;
}

#[async_trait::async_trait]
pub trait LeaseStore: Send + Sync {
    async fn lease_next(&self, runner: ClientId, ttl: time::Duration) -> Result<Option<JobLease>, PortError>;
    async fn heartbeat(&self, lease: LeaseId, runner: ClientId, ttl: time::Duration) -> Result<Option<JobLease>, PortError>;
}

#[async_trait::async_trait]
pub trait ReadinessPort: Send + Sync {
    async fn is_ready(&self) -> Result<bool, PortError>;
}
```

- [x] Write failing tests proving Hermes actors require a user, runner actors reject user job access, new jobs always use the authenticated actor's user, and empty result references are rejected.
- [x] Run `mise exec -- cargo test -p media-core actor application` and confirm failure from missing types/functions.
- [x] Add `ClientId` and `LeaseId` through the existing nominal-ID macro; define the five fixed identity constants verbatim; add `async-trait` and `time` without adding I/O dependencies.
- [x] Make `CredentialDigest` constructible only from `[u8; 32]`, expose bytes only by reference to adapters, and implement redacted `Debug`.
- [x] Implement `Actor::require_user`, `Actor::require_runner`, `NewJobCommand`, `JobApplication::create_job`, `get_job`, and `queue_status` against `Arc<dyn JobStore>`.
- [x] Implement `LeaseApplication` against `Arc<dyn LeaseStore>` and require a runner actor before leasing or heartbeat.
- [x] Use typed `ApplicationError` variants: `Forbidden`, `InvalidInput`, `NotFound`, `Conflict`, and `Infrastructure`.
- [x] Run focused tests, `mise run format`, `check`, `lint`, and `test`.
- [x] Commit: `feat(core): define application ports and actors`.

## Task 2: Expand the Versioned HTTP Contract

**Files:**
- Create: `crates/media-contract/src/actor.rs`
- Modify: `crates/media-contract/src/error.rs`
- Modify: `crates/media-contract/src/job.rs`
- Create: `crates/media-contract/src/lease.rs`
- Modify: `crates/media-contract/src/lib.rs`

**Interfaces:**

```rust
pub struct CreateJobRequest { pub provider: ProviderDto, pub result_ref: String, pub notify_scope: NotifyScopeDto }
pub struct JobDto { pub id: PublicId, pub provider: ProviderDto, pub result_ref: String, pub state: JobStateDto, pub needs_action_reason: Option<NeedsActionReasonDto>, pub notify_scope: NotifyScopeDto }
pub struct QueueStatusDto { pub queued: u64, pub active: bool }
pub struct LeaseDto { pub lease_id: PublicId, pub job: JobDto, pub expires_at: String }
```

- [x] Write failing JSON-shape tests for every request/response, including the invariant that `CreateJobRequest` has no owner field and rejects unknown fields.
- [x] Add `#[serde(deny_unknown_fields)]` to write requests and exact `snake_case` enums for provider and notification scope.
- [x] Add public error codes `missing_idempotency_key`, `idempotency_conflict`, `idempotency_in_progress`, `invalid_token`, and `lease_not_found`.
- [x] Add round-trip tests for all DTOs and golden JSON assertions for job creation and lease responses.
- [x] Confirm `cargo tree -p media-contract` contains no `media-core`.
- [x] Run workspace gates and commit: `feat(contract): add job and lease api contracts`.

## Task 3: Create Explicit PostgreSQL Migrations

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/media-storage/Cargo.toml`
- Create: `crates/media-storage/src/lib.rs`
- Create: `crates/media-storage/src/migration/mod.rs`
- Create: `crates/media-storage/src/migration/m20260710_000001_users_clients.rs`
- Create: `crates/media-storage/src/migration/m20260710_000002_media_identity.rs`
- Create: `crates/media-storage/src/migration/m20260710_000003_jobs.rs`
- Create: `crates/media-storage/src/migration/m20260710_000004_idempotency_leases.rs`
- Create: `crates/media-storage/tests/migrations.rs`
- Create: `crates/media-storage/tests/support/mod.rs`
- Modify: `.mise.toml`

**Schema:**

```text
users
api_clients
media
media_external_refs
seasons
episodes
episode_provider_mappings
jobs
job_tasks
job_stages
idempotency_records
job_leases
```

All IDs are UUID, timestamps are `timestamptz`, snapshots/checkpoints are
`jsonb`, and enum-like columns are text with explicit check constraints so
future values can be migrated transactionally without PostgreSQL enum DDL.

`job_leases` contains `slot smallint NOT NULL UNIQUE CHECK (slot = 1)` to make
the one-active-lease MVP invariant database-enforced.

- [x] Write a failing Testcontainers PostgreSQL test that runs `Migrator::up`, verifies every table and fixed user row, then runs `Migrator::down` and verifies removal.
- [x] Add pinned SeaORM features only: `macros`, `runtime-tokio-rustls`, `sqlx-postgres`, `with-json`, `with-time`, and `with-uuid`; disable default features where possible.
- [x] Implement migrations with foreign keys, unique constraints, and checks for media kind/ordering, actor role/user mapping, job state/reason, non-negative ordinals/attempts, idempotency status, and lease slot.
- [x] Seed only `primary` and `secondary`; never seed tokens.
- [x] Add tests that invalid series ordering, invalid job reason/state combinations, duplicate provider mappings, and a second lease slot fail at PostgreSQL level.
- [x] Verify down migrations in exact reverse order and no schema synchronization API is used.
- [x] Add `mise run test-integration` for the PostgreSQL/Testcontainers suite and fail clearly when Docker is unavailable; pin the PostgreSQL 17 Alpine image by digest in test support.
- [x] Commit: `feat(storage): add explicit postgres migrations`.

## Task 4: Add SeaORM Entities and Identity/Client Adapters

**Files:**
- Modify: `crates/media-core/src/identity.rs`
- Modify: `crates/media-core/src/port.rs`
- Modify: `crates/media-core/src/lib.rs`
- Create: `crates/media-storage/src/entity/mod.rs`
- Create: one focused entity module per migration table
- Create: `crates/media-storage/src/repository/client.rs`
- Create: `crates/media-storage/src/repository/identity.rs`
- Create: `crates/media-storage/src/repository/readiness.rs`
- Create: `crates/media-storage/src/repository/mod.rs`
- Create: `crates/media-storage/src/mapping.rs`
- Create: `crates/media-storage/tests/client_repository.rs`
- Create: `crates/media-storage/tests/identity_repository.rs`
- Create: `crates/media-storage/tests/readiness.rs`

- [x] Write failing integration tests for fixed-user lookup, API-client upsert, token rotation, disabled-client rejection, canonical media insertion, external-reference uniqueness, and persisted confirmed episode mapping.
- [x] Define SeaORM models with `#[sea_orm(primary_key, auto_increment = false)]`; do not generate schema from entities.
- [x] Implement explicit `TryFrom<Model>` and ActiveModel builders in `mapping.rs`; repository public methods return domain types only.
- [x] Implement SHA-256 token digest input as `CredentialDigest`; never accept or expose plaintext tokens in storage APIs.
- [x] Implement `ClientStore` for `SeaOrmClientStore` and identity repository ports for canonical media/mapping operations.
- [x] Implement `ReadinessPort` so it returns true only when PostgreSQL is reachable and `Migrator::get_pending_migrations` is empty.
- [x] Add tests confirming SeaORM model/debug output and repository errors do not include token bytes.
- [x] Run focused integration and workspace gates; commit: `feat(storage): implement client and identity adapters`.

## Task 5: Implement Jobs, Idempotency Records, and Atomic Leasing

**Files:**
- Create: `crates/media-storage/src/repository/job.rs`
- Create: `crates/media-storage/src/repository/idempotency.rs`
- Create: `crates/media-storage/src/repository/lease.rs`
- Create: `crates/media-storage/tests/job_repository.rs`
- Create: `crates/media-storage/tests/idempotency_repository.rs`
- Create: `crates/media-storage/tests/lease_repository.rs`

**Storage-facing idempotency records:**

```rust
pub enum ReservationRecord { Reserved, Replay(StoredResponseRecord), Conflict, InProgress }
pub struct StoredResponseRecord { pub status: u16, pub content_type: String, pub body: Vec<u8> }
```

These are repository records, not domain types. `media-storage` exposes narrow
concrete repository methods without depending on `media-api`. Task 6 defines
the consuming HTTP port, and Task 8 implements that port on a composition-root
newtype which maps to these records. This preserves both dependency direction
and Rust's orphan rules.

- [ ] Write failing PostgreSQL tests for owner-scoped job reads, queue counts, same-key replay, different-body conflict, in-progress conflict, abort/retry, and expiry replacement.
- [ ] Implement `SeaOrmJobStore`; enforce owner filtering in SQL rather than fetching then checking.
- [ ] Implement concrete idempotency repository methods with `INSERT ... ON CONFLICT` plus locked read in one transaction; do not add HTTP or Axum dependencies to `media-storage`.
- [ ] Write a concurrent lease test using two independent connections and a barrier; assert exactly one `Some(JobLease)` and one `None`.
- [ ] Implement lease acquisition in one transaction: lock or replace an expired slot, return the expired job from `leased` to `queued`, claim one queued job with `FOR UPDATE SKIP LOCKED`, insert slot `1`, and transition the claimed job to `leased`; no process-local mutex.
- [ ] Map a concurrent unique-slot conflict to `None` after confirming another non-expired lease exists; never surface the expected race as a 500.
- [ ] Implement heartbeat with exact lease ID and runner client ownership; another runner receives no lease.
- [ ] Confirm entities remain private and tests assert domain return values.
- [ ] Run integration/workspace gates; commit: `feat(storage): add durable jobs idempotency and leasing`.

## Task 6: Build Axum State, Authentication, and Request IDs

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/media-api/Cargo.toml`
- Create: `crates/media-api/src/lib.rs`
- Create: `crates/media-api/src/auth.rs`
- Create: `crates/media-api/src/error.rs`
- Create: `crates/media-api/src/idempotency.rs`
- Create: `crates/media-api/src/request_id.rs`
- Create: `crates/media-api/src/route/health.rs`
- Create: `crates/media-api/src/route/mod.rs`
- Create: `crates/media-api/tests/auth.rs`
- Create: `crates/media-api/tests/request_id.rs`

- [ ] Write failing `Router::oneshot` tests for missing bearer token, invalid token, disabled token, Hermes identity insertion, runner identity insertion, forbidden role, generated request ID, propagated valid request ID, and JSON error shape.
- [ ] Define an API-owned, object-safe `IdempotencyStore` port using HTTP response status/content-type/body, because exact replay is a transport concern rather than a domain concern.
- [ ] Define `ApiState` from `Arc<JobApplication>`, `Arc<LeaseApplication>`, `Arc<dyn ClientStore>`, `Arc<dyn IdempotencyStore>`, and `Arc<dyn ReadinessPort>`; no storage concrete type appears.
- [ ] Implement bearer hashing in middleware and lookup through `ClientStore`; insert `Actor` into request extensions.
- [ ] Layer request ID generation/propagation outside auth so authentication errors include the same `x-request-id` header and body field.
- [ ] Apply the global body/header limits before buffering or hashing request data; oversized input returns a stable 413/400 error without an idempotency reservation.
- [ ] Add `/v1/health` as process liveness and `/v1/ready` through a narrow readiness port; readiness failures return 503.
- [ ] Add compile-time architecture tests that reject `media-api -> media-storage` and any resolved path from `media-storage` to `media-api` or `media-contract`.
- [ ] Run router/workspace tests; commit: `feat(api): add authenticated axum foundation`.

## Task 7: Add Idempotent Job and Runner Routes

**Files:**
- Create: `crates/media-api/src/route/jobs.rs`
- Create: `crates/media-api/src/route/queue.rs`
- Create: `crates/media-api/src/route/runner.rs`
- Create: `crates/media-api/src/convert.rs`
- Create: `crates/media-api/tests/jobs.rs`
- Create: `crates/media-api/tests/idempotency.rs`
- Create: `crates/media-api/tests/runner.rs`

- [ ] Write failing handler tests proving Primary cannot read Secondary's job, runner cannot create/read user jobs, request owner spoofing is rejected as unknown JSON, and queue status exposes no private job details.
- [ ] Implement explicit DTO/domain conversions in `convert.rs`; no derive-based domain serialization.
- [ ] Require `Idempotency-Key` on POST routes, limit it to 1-128 visible ASCII characters, and fingerprint authenticated client ID + method + path + body bytes.
- [ ] Buffer handler responses: persist responses below 500; abort reservation on 500 so retry is possible; replay persisted status/content-type/body exactly.
- [ ] Test duplicate same-body replay creates one job, changed-body reuse returns 409, concurrent duplicate returns `idempotency_in_progress`, and a 500 can retry.
- [ ] Implement lease and heartbeat routes restricted to runner actors. Both operations use the server-configured TTL; neither request accepts a TTL override.
- [ ] Test lease response, empty queue 204, wrong runner heartbeat 404, and exact request ID propagation.
- [ ] Run router/workspace gates; commit: `feat(api): expose idempotent job and lease routes`.

## Task 8: Compose Service, Migrations, and HTTP CLI

**Files:**
- Modify: `crates/media/Cargo.toml`
- Create: `crates/media/src/config.rs`
- Create: `crates/media/src/client.rs`
- Modify: `crates/media/src/main.rs`
- Create: `crates/media/tests/config.rs`
- Create: `crates/media/tests/http_cli.rs`

**Configuration:**

```text
MEDIA_DATABASE_URL_FILE
MEDIA_LISTEN_ADDR (default 0.0.0.0:8080)
MEDIA_SERVICE_URL (CLI only)
MEDIA_TOKEN_FILE (CLI only)
MEDIA_PRIMARY_TOKEN_FILE
MEDIA_SECONDARY_TOKEN_FILE
MEDIA_RUNNER_TOKEN_FILE
MEDIA_LEASE_TTL_SECONDS (default 60; valid 30-300)
```

- [ ] Write failing tests for a missing database URL file, unreadable secret file, empty token, redacted config/debug output, and default listen address.
- [ ] Implement typed configuration loaded only in `media`; libraries receive constructed clients and values.
- [ ] Add `media migrate` using `media_storage::Migrator::up`; `media serve` MUST fail fast before binding the listener when migrations are pending and MUST NOT apply them automatically.
- [ ] At service startup, read three token files with `secrecy::SecretString`, hash them, and upsert fixed client mappings without logging secret content.
- [ ] Add a local `StorageIdempotencyAdapter` newtype which implements the API-owned idempotency port by mapping to `media-storage` repository records; compose all stores, core applications, and the Axum router without adding an API dependency to storage.
- [ ] Compose SeaORM stores, core applications, and the Axum router; install structured tracing without logging headers/bodies.
- [ ] Implement Reqwest CLI client with bearer token file, generated request ID, generated idempotency key for create, and stable JSON output.
- [ ] Add black-box tests against an in-process Axum listener for `jobs create/get` and `queue status`.
- [ ] Run workspace gates; commit: `feat(media): compose service migrations and http cli`.

## Task 9: End-to-End PostgreSQL Gate and CI

**Files:**
- Modify: `.mise.toml`
- Modify: `.github/workflows/ci.yml`
- Create: `tests/postgres.rs`
- Modify: `README.md`
- Modify: `docs/ARCHITECTURE.md` only if implemented dependencies differ from its current graph

- [ ] Add `mise run test-integration` that runs the opt-in PostgreSQL/Testcontainers suite and fails clearly when Docker is unavailable.
- [ ] Write one real-DB end-to-end test: migrate, bootstrap clients, authenticated idempotent create job, owner read, cross-owner 404, concurrent runner lease, heartbeat, and queue status.
- [ ] Add a CI integration job on Ubuntu with Docker, after the normal verify job; keep provider/live-network tests excluded.
- [ ] Update README with PostgreSQL prerequisites, migration commands, secret-file setup using dummy local values, service/CLI examples, and explicit no-public-route warning.
- [ ] Run `mise run format`, `check`, `lint`, `test`, `test-integration`, `audit`, and `build`.
- [ ] Run `cargo metadata` architecture tests and `git diff --check`.
- [ ] Commit: `ci: verify postgres api foundation`.

## Phase Completion Checklist

- [ ] Explicit migrations create and reverse all schema; fixed users are seeded and no token is seeded.
- [ ] SeaORM entities remain private to `media-storage` and adapters return domain types.
- [ ] Domain use cases depend only on async ports; API handlers contain no SQL or SeaORM imports.
- [ ] Hermes tokens resolve to fixed users and request bodies cannot spoof ownership.
- [ ] Runner credentials cannot access user job details.
- [ ] Same idempotency key/body replays exactly; different body and in-progress reuse conflict.
- [ ] Concurrent lease calls produce at most one active lease, enforced by PostgreSQL.
- [ ] `media migrate`, `serve`, `jobs create/get`, and `queue status` work through the multi-call binary.
- [ ] Unit, router, migration, repository, concurrency, CLI, and real-DB E2E tests pass.
- [ ] CI verify and PostgreSQL integration jobs pass on GitHub.
- [ ] No provider, download, ffmpeg, Plex, Gluetun, Telegram, or public ingress behavior was added in this phase.

The next focused plan is `2026-07-10-rezka-session-authentication.md`.
