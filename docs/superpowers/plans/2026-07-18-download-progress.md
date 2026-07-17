# Download Progress Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Expose trustworthy live download details for Rezka and Prowlarr jobs through `media jobs get` and both Hermes profiles.

**Architecture:** The runner converts source observations into rate-limited `StageCheckpoint` events. PostgreSQL retains the latest normalized checkpoint, the owner-scoped job-detail read validates and exposes it, and Hermes formats only fields returned by the API. `media-service` and Hermes never receive qBittorrent credentials.

**Tech Stack:** Rust 1.97, Tokio, Axum, SeaORM/PostgreSQL, reqwest, qBittorrent Web API, Python unittest for Hermes skill contracts, Docker Compose.

## Global Constraints

- Progress reporting is best-effort and must never fail a download.
- Checkpoints do not emit Telegram notifications or create new status cards.
- Never expose source URLs, torrent hashes, paths, credentials, or raw provider payloads.
- Omit percentage and ETA when the source does not provide enough trustworthy data.
- Persist routine progress no more than once every five seconds, except first, state-change, and final observations.

---

### Task 1: Public progress read model

**Files:**
- Modify: `crates/media-core/src/job.rs`
- Modify: `crates/media-contract/src/job.rs`
- Modify: `crates/media-api/src/convert.rs`
- Modify: `crates/media-storage/src/repository/job.rs`
- Test: `crates/media-contract/src/job.rs`
- Test: `crates/media-storage/tests/orchestration_repository.rs`

**Interfaces:**
- Produces: `TransferProgress`, `TransferKind`, and `TransferProgressDto`.
- Produces: `JobDetail.progress: Option<TransferProgress>` and `JobDetailDto.progress: Option<TransferProgressDto>`.
- Consumes: normalized keys from a running `download` or `torrent_monitor` stage checkpoint.

- [ ] **Step 1: Add failing contract tests**

Assert that a job detail serializes an optional `progress` object with `kind`, `state`, percentage, byte counts, speed, ETA, peers, seeds, and RFC3339 `updated_at`, while legacy detail JSON omits it.

- [ ] **Step 2: Run the contract test and verify RED**

Run: `cargo test -p media-contract job_detail -- --nocapture`

Expected: compile failure because `TransferProgressDto` and `JobDetailDto.progress` do not exist.

- [ ] **Step 3: Add failing storage tests**

Report a `StageCheckpoint` containing normalized progress keys, assert owner-scoped `find_detail_for_owner` returns them, then write malformed percentage/byte values and assert progress is omitted while stage and job remain readable.

- [ ] **Step 4: Run the storage tests and verify RED**

Run: `cargo test -p media-storage --test orchestration_repository job_detail_reports_progress -- --nocapture`

Expected: compile failure because `JobDetail.progress` does not exist.

- [ ] **Step 5: Implement domain, DTO, conversion, and validated storage parsing**

Use integer `progress_percent` in `0..=100`, optional non-negative metrics, and the database stage `updated_at` timestamp. Query stage name, checkpoint, and timestamp together; only parse `download` and `torrent_monitor` checkpoints.

- [ ] **Step 6: Run focused tests and commit**

Run: `cargo test -p media-contract job_detail && cargo test -p media-storage --test orchestration_repository job_detail`

Commit: `feat: expose job download progress`

### Task 2: qBittorrent progress checkpoints

**Files:**
- Modify: `crates/media-integrations/src/qbittorrent.rs`
- Modify: `crates/media/src/runner.rs`
- Test: `crates/media-integrations/tests/qbittorrent.rs`
- Test: `crates/media/tests/runner_loop.rs`

**Interfaces:**
- Produces: optional `downloaded_bytes`, `total_bytes`, `download_speed_bps`, `eta_seconds`, `seeds`, and `peers` on `TorrentSnapshot`.
- Consumes: `RunnerControl::stage_checkpoint(...)` from Task 3's runner-event helper, or introduces that helper here before use.

- [ ] **Step 1: Add a failing qBittorrent monitor test**

Return `downloaded`, `size`, `dlspeed`, `eta`, `num_seeds`, and `num_leechs` from the wiremock endpoint and assert the typed snapshot. Add a sentinel test where `-1` becomes `None`.

- [ ] **Step 2: Run the integration test and verify RED**

Run: `cargo test -p media-integrations --test qbittorrent monitor -- --nocapture`

Expected: compile failure because the snapshot fields do not exist.

- [ ] **Step 3: Extend typed qBittorrent parsing**

Deserialize provider integers as signed values, convert only non-negative values, preserve existing identity validation, and never expose hash/category/path fields in checkpoints.

- [ ] **Step 4: Add failing runner checkpoint tests**

Use a scripted snapshot sequence to assert the first observation is sent, routine observations inside five seconds are suppressed, a state change is sent, and checkpoint delivery failure does not fail torrent execution.

- [ ] **Step 5: Implement rate-limited torrent checkpoint reporting**

Map snapshots to normalized checkpoint keys and send the first, five-second, state-change, and final observations through the existing runner event API. Keep the two-second qBittorrent monitoring cadence.

- [ ] **Step 6: Run focused tests and commit**

Run: `cargo test -p media-integrations --test qbittorrent && cargo test -p media --test runner_loop`

Commit: `feat: report torrent download progress`

### Task 3: Rezka MP4 and HLS progress checkpoints

**Files:**
- Modify: `crates/media-runner/src/ports.rs`
- Modify: `crates/media-runner/src/adapters.rs`
- Modify: `crates/media-runner/src/pipeline.rs`
- Modify: `crates/media/src/runner.rs`
- Test: `crates/media-runner/tests/adapters.rs`
- Test: `crates/media-runner/tests/episode_pipeline.rs`
- Test: `crates/media/tests/runner_loop.rs`

**Interfaces:**
- Produces: source-neutral `TransferObservation` delivered through `StageReporter::stage_progress`.
- Consumes: `RunnerControl::stage_checkpoint` and the five-second throttle.

- [ ] **Step 1: Add failing MP4 progress tests**

Assert streamed chunks include resumed bytes, exact probed total, measured speed, percentage, and ETA. Assert unknown totals omit percentage and ETA.

- [ ] **Step 2: Run the adapter tests and verify RED**

Run: `cargo test -p media-runner --test adapters progress -- --nocapture`

Expected: compile failure because `TransferObservation` and `stage_progress` do not exist.

- [ ] **Step 3: Implement source-neutral observations and MP4 reporting**

Extend `StageReporter` with a default no-op progress method. Pass a trustworthy optional total into the HTTP streaming loop, include resumed bytes, calculate speed from monotonic elapsed time, and emit a final observation.

- [ ] **Step 4: Add failing HLS omission tests**

Assert HLS can report growing output bytes and speed but does not report percentage or ETA when total work is unknown.

- [ ] **Step 5: Implement HLS observable progress**

Add a default-compatible `ProcessPort::run_with_progress` path. The Tokio adapter samples the growing staging file while ffmpeg runs and reports bytes/s; existing test fakes retain the default implementation.

- [ ] **Step 6: Connect Rezka observations to normalized checkpoints**

Map MP4/HLS observations in `ControlStageReporter`, enforce the shared five-second throttle, and swallow/log checkpoint delivery failures.

- [ ] **Step 7: Run focused tests and commit**

Run: `cargo test -p media-runner && cargo test -p media --test runner_loop`

Commit: `feat: report rezka download progress`

### Task 4: CLI rendering and Hermes behavior

**Files:**
- Modify: `crates/media/src/render.rs`
- Test: `crates/media/tests/render_cli.rs`
- Modify in sibling repository: `shared/skills/media/SKILL.md`
- Test in sibling repository: `tests/test_scaffold.py`

**Interfaces:**
- Consumes: `JobDetailDto.progress` JSON from Task 1.
- Produces: compact human output and Telegram response rules for both profiles.

- [ ] **Step 1: Add a failing CLI rendering test**

Assert a 73% torrent renders a ten-cell bar, downloaded/total binary sizes, speed, compact ETA, seeds/peers, source state, and updated timestamp. Assert absent fields are not invented.

- [ ] **Step 2: Run the renderer test and verify RED**

Run: `cargo test -p media --test render_cli job_renders_download_progress -- --nocapture`

Expected: assertion failure because progress is not rendered.

- [ ] **Step 3: Implement compact rendering**

Reuse binary size formatting, add bytes-per-second and duration helpers, and render only present validated fields.

- [ ] **Step 4: Add a failing Hermes skill-contract test**

Assert the media skill requires returned progress fields, a ten-cell bar, honest omission, torrent peers, checkpoint age, and no extra status-card pushes.

- [ ] **Step 5: Update the shared media skill and verify**

Run: `./scripts/check`

Expected: all Hermes tests pass for the shared skill used by both profiles.

- [ ] **Step 6: Commit each repository**

Media commit: `feat: render download progress`

Hermes commit: `feat: explain detailed download progress`

### Task 5: Full verification, push, deploy, and live smoke tests

**Files:**
- Verify only; no planned production edits.

- [ ] **Step 1: Run complete local verification**

Run: `cargo fmt --all --check`

Run: `cargo test --workspace`

Run: `cargo clippy --workspace --all-targets -- -D warnings`

Run in Hermes: `./scripts/check`

- [ ] **Step 2: Push media-orchestrator and hermes-home commits to main**

Fast-forward only after all local checks pass.

- [ ] **Step 3: Deploy with the repository homelab script**

Confirm no active job before replacing runtime containers. Build versioned media and Hermes images, run migrations, recreate services, and wait for healthy status.

- [ ] **Step 4: Run live Prowlarr smoke test**

During a selected torrent job, call `hermes-media jobs get JOB_ID --json` twice and verify percentage/bytes/speed change, peer fields are present when qBittorrent supplies them, and no checkpoint-created Telegram messages appear.

- [ ] **Step 5: Run live Rezka smoke test**

During one small episode download, verify the available direct/HLS metrics change and absent totals do not produce invented percentage or ETA.

- [ ] **Step 6: Verify logs and repository state**

Confirm `media-service`, `download-runner`, `hermes-primary`, and `hermes-secondary` are healthy, recent logs have no errors, queue state is ready, and local main branches match origin.
