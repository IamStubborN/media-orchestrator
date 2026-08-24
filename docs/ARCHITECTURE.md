# Architecture

This document is the code map for `media-orchestrator`. It describes stable
boundaries, dependency direction, entry points, and deliberately absent
dependencies. Product behavior and rationale belong in
`docs/superpowers/specs/2026-07-10-media-orchestrator-mvp-design.md`.

## Architectural Style

`media-orchestrator` is a modular monolith with hexagonal boundaries.

- It is developed in one Cargo workspace and released as one `media` binary.
- Crates represent real API, heavy dependency, reuse, or runtime boundaries.
- Modules organize related implementation details inside each crate.
- Domain and application rules point inward; infrastructure points toward them.
- The binary crate is the composition root and is the only place that knows every concrete implementation.

The design intentionally avoids both extremes:

- It is not a single unrestricted crate where database, HTTP, and domain code can depend on each other freely.
- It is not strict Clean Architecture with a crate for every entity, port, adapter, and use case.

## Workspace Map

```text
crates/
  media-core/
  rezka-client/
  media-storage/
  media-integrations/
  media-contract/
  media-api/
  media-runner/
  media/
```

### `media-core`

Owns the media domain and service-side application rules:

- Media identity and provider references.
- Canonical season and episode numbering with persisted ambiguity resolution.
- Jobs, tasks, artifacts, and state transitions.
- Search sessions and pagination rules.
- Notification-only tracking and explicit Rezka auto-download subscriptions.
- Ranking, storage, retention, and ownership policies.
- Ports required by service-side use cases.

Architecture invariant: `media-core` performs no I/O and has no dependency on Axum, SeaORM, Reqwest, Docker, JSON serialization, environment variables, filesystem paths, ffmpeg, or provider response types.

### `rezka-client`

Owns the independent Rezka protocol implementation:

- Mirror selection.
- Cookie jar and anonymous session lifecycle.
- Import and export of session state without filesystem ownership.
- Anubis detection and proof-of-work.
- Catalog, translation, season, episode, stream, and subtitle parsing.
- Provider-specific retry and typed errors.

Architecture invariant: `rezka-client` does not depend on any `media-*` crate. It exposes Rezka-specific models and errors; adapters map them into the media domain.

The crate never reads Docker secrets or writes session files. A runner-side
adapter encrypts and persists the exported cookie state. Sessions are anonymous:
the client never posts account credentials.

#### Rezka Anonymous Session Contract

Session establishment is a bounded protocol flow, not browser automation:

1. Fetch the configured same-origin probe and classify it from explicit valid
   and invalid markers. A response containing both marker classes or neither is
   inconclusive and is rejected.
2. The transport detects Anubis markers before any Rezka parser, solves at most
   one supported `fast` challenge, submits the exact pass parameters, requires
   `techaro.lol-anubis-auth`, and retries the original request once. Unsupported
   algorithms and rejected native solutions may take one browser helper pass
   when `chrome-headless-shell` is present, still followed by a single
   original-request retry.
3. Accept either an explicit valid marker or an explicit invalid marker. Invalid
   is the expected anonymous state. No DLE login form, username, password,
   Vaultwarden broker, or Telegram session refresh exists in this application.
4. Export the encrypted cookie snapshot after the probe succeeds. The shared
   store lock covers load, challenge handling, validation, and atomic save.

The native fast solver is the default and the only mandatory session path. When
the runner image contains pinned amd64 Stable `chrome-headless-shell`, a
private helper (`media anubis-browser-challenge`) drives it over CDP for `preact`,
`metarefresh`, unknown algorithms, or a rejected native pass. There is no
operator toggle: the helper attaches whenever the binary is present and is
never launched on the SHA-256 path. Service images do not contain Chromium.
The adapter returns only `Set-Cookie` header strings into the in-memory jar.
Challenge HTML, DOM, localStorage, screenshots, and payloads stay inside the
helper process. Do not use FlareSolverr, user Chrome, or a persisted browser
profile. Callers must not manually inject or copy browser cookies.

Snapshots written before the anonymous-session contract are migrated once
under that lock: only the legacy DLE `PHPSESSID` is removed before any provider
request, while Anubis clearance and unrelated provider cookies are retained.
The new snapshot format marker prevents future anonymous `PHPSESSID` values
from being removed repeatedly.

Cookies are attached only to the exact selected Rezka origin and are never
exposed through the CLI, jobs, notifications, logs, or a manual browser-cookie
import path.

### `media-storage`

Owns PostgreSQL persistence:

- SeaORM entities.
- Explicit versioned migrations.
- Repository implementations for `media-core` ports.
- Transactional job leasing and notification outbox operations.
- Durable stage checkpoints, ownership, provider identity mappings, and manual
  numbering resolutions.
- Plex publication and reconciliation state.
- Mapping between persistence models and domain types.

Architecture invariant: SeaORM models never escape this crate. Specialized PostgreSQL statements use the same SeaORM connection and transaction.

### `media-integrations`

Owns external service adapters other than Rezka:

- Prowlarr search.
- qBittorrent submission and monitoring.
- Plex refresh and publication integration.
- Plex item lookup and exact path/identity verification.
- Gluetun control.
- Hermes notification webhooks.

Each integration is an internal module with its own configuration, client, models, errors, and fixture tests. An integration becomes a separate crate only when it gains independent consumers or a significantly different dependency/runtime boundary.

Architecture invariant: integration modules do not call each other. Use cases coordinate multiple ports.

### `media-contract`

Owns the versioned transport contract:

- `/v1` request and response DTOs.
- Public IDs and pagination tokens.
- Runner lease and event DTOs.
- Stable public error codes.
- Notification payload schemas.

Architecture invariant: transport DTOs are not domain models. Conversion occurs at an API or runner boundary so changing an internal domain type does not silently change the wire format.

### `media-api`

Owns the HTTP server boundary:

- Axum routers and handlers.
- Authentication and fixed client identity.
- Idempotency and request IDs.
- DTO validation and domain conversion.
- HTTP status and public error mapping.
- Health and readiness endpoints.
- Unauthenticated Prometheus `/metrics` scrape endpoint (job-state and outbox
  gauges through a narrow `media-core` metrics port, plus HTTP counters and
  latency histograms labelled by matched route pattern).

Architecture invariant: handlers are thin. They validate transport input, call an application use case, and convert the result to a transport response.

### `media-client`

Owns the typed REST client used by the human CLI and deterministic integrations.
It depends only on `media-contract` among workspace crates, deserializes every
successful response into its declared DTO, and retains sanitized transport
errors. It cannot reach application, storage, provider, or MCP implementation
crates.

#### Internal media-admin MCP

`media-api` also exposes a protected Streamable HTTP MCP endpoint at
`/internal/mcp`. This is a second delivery boundary over the same application
services, not a second media implementation:

- MCP tools reuse the owner-scoped job, tracking, search, and media-admin
  applications.
- Conversational media workflows use MCP exclusively. The `media` CLI remains
  an independent human and operations adapter over the REST API; MCP never
  shells out to it and the CLI does not depend on MCP.
- The agent-facing surface covers provider search and continuation, exact
  result selection, jobs, release schedules, trends, the complete tracking
  lifecycle, recovery alternatives, and explicit episode mapping.
- Bearer authentication resolves the same fixed Hermes actor as the REST API.
- Hermes receives structured results and never receives provider credentials,
  database access, or the Docker socket.
- Plex and qBittorrent administration is mediated by `MediaAdminService`.
  Both Hermes profiles discover the complete published tool surface and may
  request mutations; user ownership and explicit destructive confirmation are
  enforced by the service.
- Filesystem reads are canonicalized and limited to configured media roots.
  A file mutation moves the target to quarantine instead of unlinking it.
- Destructive operations require a short-lived, owner-bound preview token and
  revalidate the exact Plex item, torrent, or filesystem fingerprint before
  execution.

The MCP facade stays a delivery adapter. Provider clients are composed behind
an application-level contract; MCP handlers do not receive URLs, credentials,
or unrestricted filesystem handles.

The endpoint accepts both legacy session negotiation used by current Hermes
clients and stateless MCP `2026-07-28` requests. Stateless requests carry their
protocol context independently; durable application state remains explicit in
PostgreSQL through search session, job, tracking, and confirmation identifiers.
Every tool publishes an output schema and safety annotations. Operational
process commands (`serve`, `runner`, `migrate`, and low-level diagnostics) are
intentionally CLI-only.

### `media-runner`

Owns runner-side execution:

- Job lease, heartbeat, and event client.
- Cooperative cancellation and expired-lease recovery.
- Sticky-job execution loop.
- Rezka download pipeline.
- Provider-neutral HTTP media transfer through a pinned `yt-dlp` adapter.
- Encrypted Rezka session persistence in a runner-owned volume.
- Storage preflight, dedicated Rezka staging, and atomic publication.
- ffprobe and ffmpeg/VAAPI process adapters.
- Subtitle validation and partial recovery.
- Plex-compatible publication.

Runner-specific ports are defined next to the use case that consumes them. The
media transfer port accepts provider-resolved URLs, so future providers such as
VK Video can reuse `yt-dlp` without coupling the pipeline to their discovery
protocol. Concrete process and filesystem adapters remain internal unless they
acquire another consumer.

Architecture invariant: `media-runner` never connects directly to PostgreSQL and never owns torrent file placement or transcoding.

### `media`

Owns process composition and user-facing commands:

```text
media serve
media runner
media migrate
media healthcheck
media search ... [--json]
media download ... [--json]      (alias: media select)
media jobs create ... [--json]
media jobs list [--json]
media jobs show JOB_ID [--json]  (aliases: get, status)
media jobs cancel JOB_ID [--json]
media queue status [--json]
media tracking add ... [--json]
media tracking enable-download TRACKING_ID ... [--json]
media tracking set-baseline TRACKING_ID --known-through SEASON:EPISODE [--json]
media tracking check-now TRACKING_ID [--json]
media tracking list [--json]
media tracking remove TRACKING_ID [--json]
```

It parses configuration once, creates concrete clients and repositories, wires use cases, starts the requested runtime, and maps terminal errors to exit codes.

Architecture invariant: `anyhow` is allowed here for final process context. Library crates expose typed errors with `thiserror`.

## Dependency Direction

```text
media-core             media-contract             rezka-client
    ^                    ^     ^                          ^
    |                    |     |                          |
media-storage        media-api                       media-runner
    ^                    ^     ^
    |                    |     |
    |              media-integrations
    |                    ^
    |                    |
    +--------------------+---------------------------------+
                          |
                        media
```

Direct workspace-crate edges, for precision the ASCII above cannot fully
express:

```text
media-storage       -> media-core
media-api            -> media-core, media-contract
media-client         -> media-contract
media-integrations   -> media-core, media-contract
media-runner         -> rezka-client
media                -> media-api, media-client, media-contract, media-core,
                         media-integrations, media-runner, media-storage,
                         rezka-client
```

Notably, `media-runner` depends on `rezka-client` only; it does not depend on
`media-storage`, `media-api`, or `media-integrations`. The diagram is
conceptual; the checked Cargo graph is authoritative. Every normal dependency
must follow a directed acyclic graph.

## Cycle Prevention

Normal Cargo dependencies between workspace crates must never be cyclic.

When a proposed change creates `A -> B -> A`, choose one resolution:

1. Merge `A` and `B` when they always change together and do not represent independent boundaries.
2. Move the shared concept into the innermost crate that semantically owns it.
3. Define a narrow port in the consuming inner crate and implement it in the outer adapter.
4. Move wire-only types to `media-contract` when the shared concept is an API contract rather than a domain concept.

Do not create a generic shared crate to break a cycle mechanically.

## Boundary Types

The same concept may have different representations at different boundaries:

```text
HTTP DTO <-> domain type <-> SeaORM model
Rezka model -> domain/provider result
runner lease DTO -> runner execution model
```

Conversions are explicit and located at the boundary that knows both sides. Domain types do not derive serialization solely for adapter convenience.

## Ports and Adapters

Port traits belong to the code that consumes them. For example, `media-core` defines the operations a job use case needs from a repository. `media-storage` implements that trait with SeaORM.

Prefer narrow capability traits over one application-wide context trait. Avoid passing an unrestricted database connection or HTTP client into domain use cases.

Dynamic dispatch is acceptable at runtime boundaries when it simplifies composition. Generics are preferred inside performance-sensitive or single-implementation code when they remain readable. Architecture must not be distorted solely to avoid `dyn` or `async_trait`.

## Error Boundaries

- Domain errors describe violated business rules.
- Provider errors describe external protocol failures.
- Storage errors preserve database context internally.
- API errors expose stable public codes and sanitized messages.
- Process entry points add operational context and determine exit status.

Secrets, cookies, signed media URLs, credentials, and raw provider bodies must not appear in error display strings or tracing fields.

## Configuration

Configuration is loaded and validated once by `media`. Library crates receive typed configuration or constructed clients. Library crates do not read environment variables directly.

Secrets are provided through Docker secret files and converted into redacted secret types at the composition boundary.

Storage roots are also validated by the composition root. The runner receives
separate typed paths for Rezka staging, Rezka TV publication, and Rezka movie
publication. `media-service` receives no writable media root.

The Rezka session-store adapter receives an encryption key for the anonymous
cookie jar. It persists only encrypted cookie state; plaintext session material
exists only in runner memory.

## Testing Boundaries

- `media-core`: pure unit and state-machine tests.
- `rezka-client`: fixture and mock-server protocol tests.
- `media-storage`: migration and PostgreSQL integration tests.
- `media-integrations`: provider fixture and mock-server tests.
- `media-contract`: serialization compatibility tests.
- `media-api`: router tests against mocked use cases.
- `media-runner`: pipeline tests with fake process, filesystem, and lease adapters.
- `media`: a small number of command and end-to-end composition tests.

Live Rezka access is opt-in and never required for normal CI.

## Change Rules

Before adding a crate, answer all of these questions:

1. Does it define an independent API boundary?
2. Does it isolate a heavy dependency or significantly improve incremental builds?
3. Does it have more than one real consumer?
4. Does it represent a distinct runtime responsibility?

If every answer is no, use a module in an existing crate.

Before adding a dependency to `media-core`, verify that the dependency does not perform I/O, encode an adapter concern, or force transport/persistence traits onto domain types.

Update this document whenever a crate is added, removed, renamed, or allowed to depend on a new workspace crate.
