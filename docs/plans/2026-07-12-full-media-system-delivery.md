# Full Media System Delivery Plan

**Status:** Active  
**Scope:** `media-orchestrator`, `hermes-home`, and `homelab`  
**Goal:** Fully implement, deploy, and live-verify the agreed personal media system. A capability is complete only when its implementation, deployment, and live evidence are all recorded.

This document is the canonical execution plan for the active Codex goal. The
goal must remain active until every item in the Completion Gate is satisfied;
partial implementation, passing unit tests, or a successful deployment alone
must not close it.

## Delivery Snapshot (2026-07-13)

### Live-Verified

- Real VPN session rotation with a sticky address within each job.
- Rezka episode download, VAAPI processing, publication, Plex identity, and initiator notification.
- Rezka movie download, VAAPI processing, subtitle sidecar, canonical year-qualified naming, and Plex identity.
- Prowlarr TV search, selection, qBittorrent category routing, restart recovery, seeding, and Plex discovery of all season episodes.
- TVmaze release lookup, personal tracking notification, and shared family tracking management.
- Owner isolation for jobs and search sessions across the Primary and Secondary profiles.
- Local status and verification commands and fail-closed deployment while a job is active.
- Owner-scoped explicit retry with idempotency and checkpoint preservation.
- Subtitle-only recovery without re-downloading or re-encoding the published video.
- Initiator-only and family Telegram notification routing with deduplicated delivery.
- Local deploy, migration, Hermes CLI refresh, health gating, rollback, and redeploy.
- Rezka full-catalog search with five-result pages and natural-language next-page
  navigation through Hermes, including isolated provider continuation state.
- Storage reserve enforcement against the real media volume: a Rezka episode job
  entered `blocked_storage`, published no files, released its lease, and remained
  parked without consuming additional attempts.
- A subtitle-present Rezka episode completed through VAAPI and Plex publication;
  a controlled subtitle-CDN failure then proved the full
  `partial -> owner retry -> completed` path without changing the published
  video's inode, size, or mtime.
- A real Rezka OVA stopped at `needs_action` before transfer, accepted an
  owner-confirmed `S00E01` mapping, resumed with a reset task ledger, and reused
  the persisted mapping in a later selection without downloading media.
- Queue status exposes durable runner availability and a safe blocking reason;
  a controlled `vpn_rotation_failed` state parked a diagnostic job without a
  lease until it was cancelled and lifecycle readiness was restored.

### Remaining Acceptance Work

1. Exercise primary Telegram media commands from both profiles and verify rejection of an unapproved external Telegram account when such an account is available.
2. Live-verify expired-stream recovery and Plex publication of a mapped
   Specials/OVA episode. Storage recovery, ambiguous numbering, and persistent
   canonical mapping are already live-verified.
3. Re-run the complete local deployment verification on the final revisions and confirm all containers, migrations, wrappers, and health gates.
4. Audit specs, architecture, runbook, plans, and evidence; then publish one final acceptance report that distinguishes implemented, deployed, and live-verified behavior.

### Immediate Execution Queue

1. Execute expired-stream recovery and Specials/OVA Plex publication with
   cleanup after each run.
2. Complete Secondary command coverage and the external unknown-user gate, or
   record the latter as an explicit external-account blocker rather than claiming
   it passed.
3. Re-run deployment verification on final revisions, reconcile every acceptance
   item with dated evidence, and close the goal only after no required work remains.

## Status Model

Every workstream must track three independent states:

- **Implemented:** the required code and configuration exist.
- **Deployed:** the current implementation is running on `host.example.invalid`.
- **Live-verified:** the real user flow passed against Rezka, Prowlarr, qBittorrent, Vaultwarden, Telegram, storage, and Plex as applicable.

Unit tests or a healthy container do not satisfy a live-verification gate.

## Fixed Product Decisions

- `media-service` coordinates state and remains outside the VPN.
- `download-runner` performs provider work inside the Gluetun network namespace.
- One active download job is allowed in the MVP.
- A job keeps one sticky VPN identity for its entire execution.
- Rezka and Prowlarr are separate sources; there is no automatic fallback.
- A user explicitly selects the source, result, and Rezka translation before download.
- Rezka uses the highest available stream, downloads every subtitle track available for the selected translation, and transcodes through VAAPI.
- Torrents are submitted to the existing qBittorrent categories and are never transcoded, moved, renamed, hardlinked, deleted, or otherwise managed by this system.
- Notifications go to the initiating profile by default; `family` notifies both profiles.
- Downloading and tracking are separate actions. For an ongoing series, Hermes reports that fact and asks whether tracking should be added as `personal` or `family`.
- Search returns at most five results per page and supports natural-language pagination independently for Rezka and Prowlarr.
- Browser automation is not part of the Rezka authentication or download path.
- PostgreSQL and SeaORM remain the durable state layer.
- Automatic database backup and Secondary Vaultwarden integration are outside the current delivery scope.

## Workstream 1: Real VPN Lifecycle

**Priority:** Critical  
**Current state:** Implemented, deployed, and live-verified. Two consecutive
session-refresh jobs proved sticky per-job IPs and successful between-job
rotation. A controlled durable `vpn_rotation_failed` state was exposed through
queue status, prevented a queued diagnostic job from leasing, and returned to
`ready` without provider activity.

### Implementation

1. Add a narrow lifecycle controller outside the Gluetun network namespace.
2. Do not expose the Docker socket to `media-service`, Hermes, or `download-runner`.
3. Accept only a bounded internal command to recreate the dedicated `gluetun-rezka` and `download-runner` pair.
4. Trigger recreation only after the active job reaches a terminal state and releases its lease.
5. Wait for Gluetun and runner health checks before allowing the next job to lease.
6. Record the previous and current public IP without storing provider credentials.
7. Retry a bounded number of times when the public IP did not change.
8. Keep the queue blocked with an explicit VPN lifecycle reason if rotation cannot complete.
   **Live-verified with `vpn_rotation_failed`.**

### Live Gate

- Job A records one public IP for its entire execution.
- The lifecycle controller recreates the VPN namespace after Job A.
- Job B starts only after readiness and uses a different public IP.
- DNS and Rezka connectivity remain healthy after rotation.
- No general Docker or host command execution is available through the controller.

## Workstream 2: Rezka Episode E2E

**Priority:** Critical  
**Current state:** Search, explicit translation selection, full download, VAAPI,
publication, Plex identity, and initiator notifications are live-verified for one
real episode. A subtitle-present `Food Wars` episode also completed with a
Plex-compatible Russian WebVTT sidecar. Its selected advertised `720p` stream
measured 854x480, so the measured probe remains authoritative. Full-catalog pagination is
implemented, deployed, and live-verified through a two-turn Hermes conversation:
the first and second pages each returned five distinct results while preserving
the Rezka search session.

### Implementation and Verification

1. Search a real multi-season series through Hermes.
2. Verify five-result pagination and isolated search state. **Live-verified.**
3. Show every available translation and require explicit selection.
4. Resolve one real episode and select the highest available stream.
5. Estimate peak source and VAAPI output usage before download. An optional
   runtime reserve is supported and configured to zero in the homelab. Streams
   without a discoverable byte length use a duration-aware conservative bitrate
   estimate rather than a fixed per-file allocation. Storage-blocked jobs are
   parked outside the active execution slot and can be safely retried with a
   fresh orchestration ledger.
6. Download with bounded resume/range behavior.
7. Probe the actual file and treat its measured dimensions as authoritative.
8. Download all valid subtitles for the selected translation.
9. Process one episode at a time through VAAPI HEVC.
10. Publish atomically only after media validation succeeds.
11. Scan Plex and verify the exact media-part path and canonical episode identity.

Rich initiator notifications are deployed and live-verified for every stage of
a real Rezka episode. They identify the media title, source, kind, episode,
translation, and job ID without exposing provider locators or credentials.
Successful download and transcode sub-stages now close explicitly; a forward
migration repaired legacy running stage rows attached to terminal jobs.

### Live Gate

- A real episode reaches `completed` from a Telegram request.
- The reported resolution matches `ffprobe`, not the provider label.
- The output is playable and VAAPI was used without CPU video encoding fallback.
- Every available valid subtitle appears as a correctly named Plex sidecar.
- Plex reports the expected show, season, episode, and exact published path.

## Workstream 3: Subtitle and Partial Recovery

**Priority:** High

**Current state:** Subtitle response-shape compatibility, unknown-language
degradation, partial stage semantics, and checkpoint-preserving execution are
implemented. The owner-scoped, idempotent `POST /v1/jobs/{id}/retry` endpoint and
matching `media jobs retry` command are deployed and live-verified through the
Hermes wrapper. Missing-sidecar recovery is live-verified against a published
Rezka movie: the VTT was restored byte-for-byte while video inode, size, and
mtime remained unchanged. A controlled real-network rejection of only the
subtitle CDN produced `partial`; owner retry fetched a valid Russian sidecar and
moved the same job to `completed` while video inode, size, and mtime remained
unchanged.

### Implementation and Verification

1. Confirm the current Rezka subtitle response shapes against live data.
2. Define deterministic Plex-compatible language and track suffixes.
3. Prevent duplicate sidecars when tracks share a language or label.
4. Treat missing subtitles as a valid no-subtitle result.
5. Treat failed or invalid subtitle downloads as `partial` after publishing a valid video.
6. Retry only missing or invalid subtitle tracks.
7. Preserve completed video and valid subtitle files during retry.
8. Allow only the job owner to requeue `blocked_storage`, `partial`, or `failed`
   jobs, with a full orchestration-ledger reset only for storage-blocked work.
9. Make retry idempotent and reject retries for non-retryable states.

### Live Gate

- A forced subtitle failure produces `partial` without discarding video.
- Retry fetches only the missing track and moves the job to `completed`.
- Plex discovers all sidecars without duplicate or incorrectly attached tracks.

## Workstream 4: Prowlarr and qBittorrent E2E

**Priority:** Critical  
**Current state:** Real Prowlarr search, ranking, explicit selection, magnet
redirect verification, qBittorrent submission, TV category routing, managed save
path, restart recovery, completion, seeding, and exact Plex discovery of all nine
season episodes are live-verified. Support for slow provider
responses, partially usable result pages, qBittorrent 5.2 asynchronous adds, and
idempotent reuse of exact pending/existing torrents is deployed. A real Prowlarr
movie also completed on attempt 2 and was verified in Plex at its exact
qBittorrent-managed path.

### Implementation and Verification

1. Search a real title through Prowlarr and return five ranked results.
2. Support natural-language next-page requests without mixing Rezka state.
3. Require explicit result selection before submission.
4. Submit TV and movie results to their existing qBittorrent categories.
5. Monitor completion and discover the final category-managed path.
6. Verify Plex against that path without mutating torrent data.
7. Preserve qBittorrent seeding and retention behavior exactly as configured outside this system.

### Live Gate

- One selected TV result and one selected movie result reach the correct categories and Plex libraries.
- No transcoding or filesystem mutation is performed by media-orchestrator.
- A failed source remains failed; no Rezka/Prowlarr fallback occurs automatically.

## Workstream 5: Notification Semantics

**Priority:** High

**Current state:** Job-kind-aware session-refresh notifications are implemented,
deployed, and live-verified for Primary with outbox deduplication. A real personal
tracking discovery notified only Secondary. A real family discovery notified
Primary and Secondary with one shared source dedupe key. The remaining gate is a
message from an unapproved external Telegram account. Rich media-job messages
with safe title, source, media kind, season/episode, translation, phase, and Job
ID are deployed and live-verified on the real subtitle recovery job. Explicit
subtitle-partial guidance and a final fully-completed recovery message are
deployed; the latter awaits observation on the next real recovery because the
verified job completed before that event type was deployed.

### Implementation

1. Replace generic stage-derived messages with job-kind-aware notifications.
2. Give Rezka session refresh one concise success or failure notification.
3. Keep download, encoding, subtitle, Plex, partial, and failure notifications only for applicable jobs.
4. Include safe actionable error details and a stable job ID.
5. Preserve initiator routing and explicit family routing.
6. Ensure outbox retry cannot duplicate Telegram delivery.

### Live Gate

- Session refresh no longer emits download, encoding, or Plex messages.
- An Primary job notifies only `hermes-primary` by default.
- A Secondary job notifies only `hermes-secondary` by default.
- A family-scoped event reaches both exactly once.

## Workstream 6: Tracking and Release Dates

**Priority:** High

**Current state:** TVmaze release queries and durable Rezka tracking are
implemented, deployed, and live-verified. A personal subscription discovered
exactly one future episode and notified only Primary once. A family subscription
created by Primary was visible and removable by Secondary, while Primary's personal
subscription remained private. The Hermes Telegram conversation for `One Piece
(1999)` used structured TVmaze data, reported the next episode and date, offered
`personal` or `family`, and created neither tracking nor a download without
confirmation.

### Implementation and Verification

1. Detect whether a selected series is complete or ongoing.
2. Report released and expected episode counts when reliable metadata is available.
3. Ask whether to add tracking only when the series is ongoing.
4. Support `personal` and `family` tracking scopes.
5. Discover future episode dates from the best available structured source.
6. Answer natural-language questions about the next episode and full release schedule.
7. Notify the tracking owner about new availability without automatic download.
8. Keep downloading and tracking as independent commands.

### Live Gate

- An ongoing series can be downloaded without creating tracking.
- Hermes offers tracking after explaining that not all episodes are released.
- Personal tracking is isolated; family tracking is manageable by both profiles.
- Hermes answers a real next-episode date question with source and uncertainty handling.

## Workstream 7: Movie Flow

**Priority:** High

**Current state:** Rezka movie search, explicit translation selection, real
movie playback resolution, subtitle parsing, full download, VAAPI transcode,
year-qualified naming, movie-root publication, and exact Plex movie/subtitle
discovery are live-verified. Prowlarr movie search, selection, transient-failure
recovery, qBittorrent completion, and exact Plex discovery are also live-verified.

### Implementation and Verification

1. Verify Rezka movie discovery, translations, stream selection, and subtitles.
2. Use movie-specific canonical naming without season or episode identifiers.
3. Apply VAAPI only to Rezka media.
4. Publish to the Plex Movies root and verify the exact Plex item.
5. Verify the equivalent Prowlarr movie flow and category.

### Live Gate

- One Rezka movie and one Prowlarr movie complete end to end.
- Both appear as the intended Plex movie with the exact expected path.

## Workstream 8: Recovery, Storage, and Mapping

**Priority:** High

**Current state:** The formerly configured 20 GiB storage reserve gate is
live-verified. A
subtitle-present Rezka episode job entered `blocked_storage` before download or
publication. The lease was released, the attempt count remained stable, and the
job did not requeue after lease expiry. Explicit storage recovery after capacity
became available is live-verified. A real OVA also entered `needs_action` before
transfer, accepted an owner-confirmed `S00E01` mapping, resumed with a reset task
ledger, and reused that mapping on a later selection. Expired-stream recovery and
actual Plex publication under `Specials` remain pending.

### Implementation and Verification

1. Prove lease expiry and restart recovery with a killed runner.
2. Resume only unfinished download or processing work.
3. Prevent duplicate publication and duplicate events.
4. Verify `blocked_storage` before violating estimated operation space plus the
   configured reserve. **Live-verified with the former 20 GiB configuration.**
5. Provide an explicit owner-scoped resume operation that returns a storage-blocked
   job to the queue only after re-running storage preflight and unfinished stages.
6. Verify bounded retry for expired streams and transient provider failures.
7. Convert ambiguous absolute/season numbering to `needs_action`.
   **Live-verified with a real Rezka OVA.**
8. Persist a resolved canonical mapping and reuse it for later episodes.
   **Live-verified with `S01E01 -> S00E01`.**
9. Verify OVA and Specials mapping rather than treating them as duplicates.
   **Mapping live-verified; Plex publication remains pending.**

### Live Gate

- A killed runner resumes from its durable checkpoint.
- A storage-blocked job performs no partial publication.
- A storage-blocked job can be resumed explicitly after capacity is restored,
  without bypassing preflight or repeating completed work.
- A resolved season/episode mapping is reused by a subsequent job.
- Published media is never automatically deleted.

## Workstream 9: Multi-User Hermes Verification

**Priority:** High

**Current state:** Both Hermes containers are healthy with one distinct numeric
Telegram allowlist identity each. Live API checks prove private job/search
isolation and shared family tracking management. Full Telegram command coverage
from Secondary and an unknown-sender rejection check remain pending.

### Implementation and Verification

1. Verify fixed Telegram chat and user allowlists for both bots.
2. Verify profile, memory, search-session, browser, and token isolation.
3. Exercise search, pagination, selection, job status, cancellation, and tracking from both profiles.
4. Confirm neither profile can query the other user's private jobs or searches.
5. Keep the shared media skill identical while retaining personal configuration and memory.
6. Leave Secondary Vaultwarden unconfigured.

### Live Gate

- Cross-profile private reads and writes are rejected.
- Shared family tracking works from both profiles.
- Unknown Telegram users cannot operate either bot.

## Workstream 10: Local Delivery and Operations

**Priority:** Medium

**Current state:** Local immutable Docker builds, cached runner packages, a
credential-free operational runbook, and `mise` status/verify/deploy/rollback
tasks are implemented. A complete deploy, migration, backend health check,
Hermes CLI refresh, Hermes health check, rollback to the previous image pair,
and redeploy to the current pair are live-verified. Deploy was also verified to
fail closed before build while a movie job was active.

### Implementation

1. Add `mise` tasks for check, build, package, deploy, verify, rollback, and status.
2. Build immutable local image tags without depending on GitHub Actions.
3. Update pinned image references and the Hermes media CLI checksum automatically.
4. Run Compose validation before any rollout.
5. Perform controlled health waits and post-deploy smoke checks.
6. Add a runbook for session refresh, VPN recreation, queue inspection, job errors, rollback, and stuck-runner recovery.
7. Keep `.env` mode `0600` and retain file-based Gluetun secrets where required.
8. Document the current manual backup posture without adding automatic database backup.

### Live Gate

- One command builds and deploys all changed components to the Docker host.
- A failed health gate stops deployment and leaves a documented rollback path.
- Rollback restores the previous images and healthy services.

## Workstream 11: Documentation Truthfulness

**Priority:** Medium

1. Audit existing specs, plans, reviews, and architecture documentation.
2. Replace broad `implemented` claims with the three-state status model.
3. Link every live-verification claim to a dated evidence record.
4. Remove stale browser-based Rezka login instructions.
5. Document the current DLE `Redirect` response and authenticated marker contract.
6. Keep deferred ideas separate: LLM Wiki, Secondary Vaultwarden, gRPC, browser fallback, and automatic provider fallback.

## Evidence Records

Create dated records under `docs/evidence/` for each live gate. Every record must include:

- exact component image or commit;
- date and environment;
- job/search/tracking identifiers where safe;
- commands or UI flow used;
- observed state transitions;
- Plex path and media identity where applicable;
- redacted errors and remediation for failed attempts;
- cleanup state.

Secrets, cookies, Telegram tokens, Vaultwarden values, and signed URLs must never be recorded.

## Execution Order

```text
1. Deploy the zero-reserve runtime configuration and storage resume
2. Complete one subtitle-present Rezka episode E2E and Plex verification
3. Prove natural subtitle failure and selective recovery
4. Prove expired-stream, numbering, mapping, and Specials/OVA behavior
5. Complete both-profile Telegram coverage and unknown-user rejection
6. Re-run final local deployment, rollback, and health verification
7. Audit documentation and publish the final acceptance report
```

Notification cleanup is first so later E2E runs produce truthful user-visible evidence. VPN lifecycle precedes downloads because the next-job IP contract affects every Rezka live test.

## Completion Gate

The goal is complete only when:

1. All eleven workstreams are implemented and deployed.
2. Every live gate has a dated evidence record.
3. Both Hermes profiles pass their isolation and primary workflow checks.
4. A Rezka episode, Rezka movie, Prowlarr TV result, and Prowlarr movie result are verified in Plex.
5. VPN identity changes between consecutive Rezka jobs while remaining sticky within each job.
6. Subtitle partial recovery, runner restart recovery, storage blocking, and ambiguous numbering are demonstrated.
7. The local deployment and rollback commands are verified on the Docker host.
8. Existing documentation accurately distinguishes implemented, deployed, and live-verified behavior.
