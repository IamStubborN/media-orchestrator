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

### Remaining Acceptance Work

1. Force a subtitle failure and prove retry downloads only the missing track.
2. Verify Rezka subtitle-present episode handling and five-item pagination through Hermes.
3. Verify initiator and family Telegram delivery for both profiles, plus unknown-user rejection.
4. Exercise the ongoing-series Hermes conversation and release-date answers.
5. Live-verify storage blocking, ambiguous numbering, Specials/OVA mapping, and expired-stream recovery.
6. Audit all documentation and publish the final acceptance report with dated evidence.

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
**Current state:** Implemented and deployed. Two consecutive session-refresh jobs live-verified sticky per-job IPs and successful between-job rotation. The explicit durable VPN lifecycle blocking reason remains pending.

### Implementation

1. Add a narrow lifecycle controller outside the Gluetun network namespace.
2. Do not expose the Docker socket to `media-service`, Hermes, or `download-runner`.
3. Accept only a bounded internal command to recreate the dedicated `gluetun-rezka` and `download-runner` pair.
4. Trigger recreation only after the active job reaches a terminal state and releases its lease.
5. Wait for Gluetun and runner health checks before allowing the next job to lease.
6. Record the previous and current public IP without storing provider credentials.
7. Retry a bounded number of times when the public IP did not change.
8. Keep the queue blocked with an explicit VPN lifecycle reason if rotation cannot complete.

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
real episode. The authenticated non-premium session exposed advertised 1080p only
as a rejected 60-second preview; its highest complete stream measured 854x480.
Subtitle-present and pagination live gates remain pending.

### Implementation and Verification

1. Search a real multi-season series through Hermes.
2. Verify five-result pagination and isolated search state.
3. Show every available translation and require explicit selection.
4. Resolve one real episode and select the highest available stream.
5. Estimate download size and enforce the 20 GiB post-operation reserve.
6. Download with bounded resume/range behavior.
7. Probe the actual file and treat its measured dimensions as authoritative.
8. Download all valid subtitles for the selected translation.
9. Process one episode at a time through VAAPI HEVC.
10. Publish atomically only after media validation succeeds.
11. Scan Plex and verify the exact media-part path and canonical episode identity.

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
Hermes wrapper. The forced subtitle-failure live gate remains pending.

### Implementation and Verification

1. Confirm the current Rezka subtitle response shapes against live data.
2. Define deterministic Plex-compatible language and track suffixes.
3. Prevent duplicate sidecars when tracks share a language or label.
4. Treat missing subtitles as a valid no-subtitle result.
5. Treat failed or invalid subtitle downloads as `partial` after publishing a valid video.
6. Retry only missing or invalid subtitle tracks.
7. Preserve completed video and valid subtitle files during retry.
8. Allow only the job owner to requeue `partial` or `failed` jobs.
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

**Current state:** Job-kind-aware session-refresh notifications are implemented, deployed, and live-verified for Primary with outbox deduplication. Secondary and family routing live gates remain pending.

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
subscription remained private. The remaining product gate is the Hermes
conversation that offers tracking after explaining an ongoing series.

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

### Implementation and Verification

1. Prove lease expiry and restart recovery with a killed runner.
2. Resume only unfinished download or processing work.
3. Prevent duplicate publication and duplicate events.
4. Verify `blocked_storage` before violating the 20 GiB reserve.
5. Verify bounded retry for expired streams and transient provider failures.
6. Convert ambiguous absolute/season numbering to `needs_action`.
7. Persist a resolved canonical mapping and reuse it for later episodes.
8. Verify OVA and Specials mapping rather than treating them as duplicates.

### Live Gate

- A killed runner resumes from its durable checkpoint.
- A storage-blocked job performs no partial publication.
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
1. Notification semantics and VPN lifecycle foundation
2. Rezka episode E2E
3. Subtitle partial recovery and Plex verification
4. Prowlarr/qBittorrent TV E2E
5. Tracking and release-date flow
6. Rezka and Prowlarr movie E2E
7. Recovery, storage, and numbering scenarios
8. Multi-user Hermes verification
9. Local deployment tooling and rollback
10. Documentation audit and final acceptance report
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
