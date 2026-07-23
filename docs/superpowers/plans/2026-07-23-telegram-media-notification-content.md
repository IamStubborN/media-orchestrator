# Telegram Media Notification Content Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Telegram media cards identify the exact movie, episode, or season, show only measured progress and final media facts, recover through direct inline actions, and keep routine updates quiet and model-free.

**Architecture:** `download-runner` probes the final published artifact and records sanitized measurements in the existing task checkpoint. `media-service` aggregates those persisted facts into a backward-compatible schema-v2 notification, while Hermes strictly parses and deterministically renders one mutable card plus one short terminal push. Telegram actions remain owner-scoped direct CLI/API calls; conversational choices use Hermes native `clarify`, and Telegram tool progress is disabled per platform.

**Tech Stack:** Rust 1.97, Tokio, SeaORM/PostgreSQL, Axum, ffprobe/ffmpeg VAAPI, Python 3, python-telegram-bot, Telegram Bot API, Docker Compose, mise.

## Global Constraints

- Automatic cards, progress edits, final pushes, and callback actions must never invoke an LLM.
- One tracked episode or manually requested episode owns one mutable card and uses an exact identity such as `S02E08`.
- One manually requested season owns one aggregate card; it must not create a full card per episode.
- Routine progress edits occur only when values change and no more frequently than once every five seconds; stage and terminal transitions remain immediate.
- Final video, audio, subtitle, size, duration, processing, and Plex fields come only from runner measurements or confirmed publication facts.
- Rezka reports `VAAPI upscale` only when that operation completed and the final probe confirms the artifact.
- Prowlarr and qBittorrent content never claims VAAPI processing and is reported as published without transcoding.
- A retry counter is labeled as a connection attempt, never as download progress or an active distribution.
- Missing subtitle tracks preserve usable video and produce a partial result.
- Primary cards never expose job IDs, paths, URLs, cookies, tokens, stack traces, raw commands, or internal error codes.
- Diagnostics may expose a sanitized job ID, attempt count, error code, and concise technical cause.
- Search keeps successful provider results when another provider fails.
- Telegram uses real inline keyboards; raw `<telegram-quick-replies>` markup is forbidden.
- Existing active downloads, notification revisions, lifecycle cycles, pending outbox rows, owner routing, and queue ordering remain unchanged.
- Deployment must not cancel, rewrite, or replace an active download.

## Repository And File Map

Implementation spans two repositories because the wire contract and its deterministic renderer must change together:

```text
/home/operator/Projects/personal/media-orchestrator
  crates/media-runner/src/media.rs
    ffprobe result and final artifact report
  crates/media-runner/src/adapters.rs
    ffprobe JSON decoding
  crates/media-runner/src/pipeline.rs
    measured processing and publication result
  crates/media/src/runner.rs
    sanitized checkpoint persistence
  crates/media-core/src/notification.rs
    validated notification domain values
  crates/media-contract/src/notification.rs
    schema-v2 JSON contract
  crates/media-storage/src/repository/lease.rs
    task aggregation and notification projection
  crates/media-storage/src/migration/m20260723_000027_detailed_notifications.rs
    backward-compatible payload validation
  crates/media-api/src/search.rs
  crates/media-api/src/route/search.rs
  crates/media/src/search.rs
  crates/media/src/client.rs
  crates/media/src/main.rs
  crates/media/src/render.rs
    owner-scoped alternative-provider search

/home/operator/Projects/personal/hermes-home
  scripts/hermes_media_notifications.py
    strict parser and Russian card renderer
  scripts/media-notifier
    five-second edit cadence and Telegram delivery
  shared/plugins/telegram-home/__init__.py
    owner-authorized inline callbacks
  shared/skills/media/SKILL.md
    native conversational choice rules
  profiles/primary/config/config.yaml
  profiles/secondary/config/config.yaml
    Telegram-only tool-progress suppression
```

---

### Task 1: Detailed Notification Domain, Wire Contract, And Migration

**Files:**
- Modify: `crates/media-core/src/notification.rs`
- Modify: `crates/media-core/src/lib.rs`
- Modify: `crates/media-contract/src/notification.rs`
- Modify: `crates/media-contract/src/lib.rs`
- Modify: `crates/media-contract/tests/tracking_notifications.rs`
- Create: `crates/media-storage/src/migration/m20260723_000027_detailed_notifications.rs`
- Modify: `crates/media-storage/src/migration/mod.rs`
- Modify: `crates/media-storage/tests/migrations.rs`

**Interfaces:**
- Produces: `MediaNotificationResult`, `MediaNotificationVideo`, `MediaNotificationAudio`, `MediaNotificationSubtitles`, `MediaNotificationProcessing`, and `MediaNotificationPublication`.
- Produces: matching `*Dto` types serialized under optional top-level `result`.
- Produces: optional `media.origin=tracked-episode` so Hermes can combine discovery and automatic download startup.
- Extends: `MediaNotificationProgress` with connection-attempt, VPN-rotation, and storage fields.
- Extends: `MediaNotificationAction` with `SearchAlternative`.
- Preserves: every schema-v2 payload accepted by migrations 24, 25, and 26.

- [ ] **Step 1: Write failing contract tests for final measured metadata**

Add an exact serialization assertion in `crates/media-contract/tests/tracking_notifications.rs`:

```rust
#[test]
fn detailed_result_serializes_as_optional_schema_v2_content() {
    let value = serde_json::json!({
        "event_type": "media.notification",
        "schema_version": 2,
        "delivery_kind": "card",
        "card_key": "media-job:00000000-0000-0000-0000-000000000999",
        "revision": 8,
        "lifecycle_cycle": 1,
        "terminal": true,
        "state": "completed",
        "media": {
            "job_id": "00000000-0000-0000-0000-000000000999",
            "title": "Клинки Хранителей",
            "kind": "series",
            "provider": "rezka",
            "season": 2,
            "translation": "AniLibria"
        },
        "progress": {
            "completed_episodes": 1,
            "total_episodes": 1,
            "current_episode": 8
        },
        "stage": "publish",
        "next_step": "none",
        "result": {
            "video": {"codec": "hevc", "profile": "Main", "width": 1920, "height": 1080},
            "audio": {
                "language": "rus",
                "codec": "aac",
                "channels": 2,
                "channel_layout": "stereo",
                "title": "AniLibria"
            },
            "subtitles": {"downloaded": 2, "missing": 0},
            "file_size_bytes": 440401920,
            "duration_seconds": 1421,
            "processing": {"mode": "vaapi-upscale", "elapsed_seconds": 252},
            "publication": {
                "library": "tv-shows",
                "title": "Клинки Хранителей",
                "season": 2,
                "episode": 8
            }
        },
        "actions": ["details"]
    });
    let payload: HermesMediaNotificationWebhook =
        serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(payload).unwrap(), value);

    assert_eq!(value["progress"]["current_episode"], 8);
    assert_eq!(value["progress"]["total_episodes"], 1);
    assert_eq!(value["result"]["video"], serde_json::json!({
        "codec": "hevc",
        "profile": "Main",
        "width": 1920,
        "height": 1080
    }));
    assert_eq!(value["result"]["audio"], serde_json::json!({
        "language": "rus",
        "codec": "aac",
        "channels": 2,
        "channel_layout": "stereo",
        "title": "AniLibria"
    }));
    assert_eq!(value["result"]["subtitles"], serde_json::json!({
        "downloaded": 2,
        "missing": 0
    }));
    assert_eq!(value["result"]["processing"], serde_json::json!({
        "mode": "vaapi-upscale",
        "elapsed_seconds": 252
    }));
    assert_eq!(value["result"]["publication"]["episode"], 8);
}
```

Add a second test that deserializes an existing schema-v2 fixture with no `result` and no new progress fields.

- [ ] **Step 2: Run contract tests and verify RED**

Run:

```bash
cargo test -p media-contract --test tracking_notifications -- --nocapture
```

Expected: compilation fails because the detailed result types and progress fields do not exist.

- [ ] **Step 3: Add the wire types and stable enum names**

Add these shapes to `crates/media-contract/src/notification.rs` and add `result: Option<MediaNotificationResultDto>` to `HermesMediaNotificationWebhook`:

```rust
#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaNotificationResultDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video: Option<MediaNotificationVideoDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<MediaNotificationAudioDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitles: Option<MediaNotificationSubtitlesDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processing: Option<MediaNotificationProcessingDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publication: Option<MediaNotificationPublicationDto>,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaNotificationVideoDto {
    pub codec: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaNotificationAudioDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub codec: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_layout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaNotificationSubtitlesDto {
    pub downloaded: u32,
    pub missing: u32,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MediaNotificationProcessingModeDto {
    VaapiUpscale,
    Original,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaNotificationProcessingDto {
    pub mode: MediaNotificationProcessingModeDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_seconds: Option<u64>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MediaNotificationLibraryDto {
    Movies,
    TvShows,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaNotificationPublicationDto {
    pub library: MediaNotificationLibraryDto,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode: Option<u32>,
}
```

Add:

```rust
#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MediaNotificationOriginDto {
    TrackedEpisode,
}
```

and optional `origin: Option<MediaNotificationOriginDto>` to `MediaNotificationDto`. Absence means a manual request and preserves every existing payload.

Extend `MediaNotificationProgressDto` with:

```rust
pub connection_attempt: Option<u32>,
pub connection_attempt_limit: Option<u32>,
pub vpn_rotation_pending: Option<bool>,
pub storage_available_bytes: Option<u64>,
pub storage_required_bytes: Option<u64>,
```

All five fields use `skip_serializing_if = "Option::is_none"`. Add `SearchAlternative` to `MediaNotificationActionDto`.

- [ ] **Step 4: Add validated domain mirrors**

Mirror the DTOs in `crates/media-core/src/notification.rs`. Reuse `validate_display_field` for codec, profile, language, layout, title, and publication title. Preserve the current constructor signatures by adding `MediaNotification::with_result`, `MediaNotificationMedia::with_origin`, `MediaNotificationProgress::with_recovery`, and `MediaNotificationProgress::with_storage`. Enforce:

```rust
if width == 0 || height == 0 {
    return Err(NotificationValidationError::InvalidMediaResult);
}
if connection_attempt == Some(0)
    || connection_attempt_limit == Some(0)
    || connection_attempt.is_some_and(|attempt| {
        connection_attempt_limit.is_some_and(|limit| attempt > limit)
    })
{
    return Err(NotificationValidationError::InvalidAttemptProgress);
}
if storage_required_bytes.is_some() != storage_available_bytes.is_some() {
    return Err(NotificationValidationError::InvalidStorageProgress);
}
```

Use `u64` whole seconds for duration and processing elapsed time. Add getters with the same field names and re-export every new public type from both `lib.rs` files.

- [ ] **Step 5: Run contract tests and verify GREEN**

Run:

```bash
cargo test -p media-core notification
cargo test -p media-contract --test tracking_notifications
```

Expected: all focused tests pass, including the legacy fixture.

- [ ] **Step 6: Write failing migration acceptance tests**

In `crates/media-storage/tests/migrations.rs`, migrate through 26, insert:

1. an old schema-v2 payload;
2. a detailed completed payload;
3. a retry payload with `connection_attempt=5`, `connection_attempt_limit=20`, and `vpn_rotation_pending=true`;
4. a blocked-storage payload with both storage byte values.

Assert all four survive migration 27. Assert inserts fail for attempt `21/20`, one-sided storage values, zero video width, and an unknown `result` key.

- [ ] **Step 7: Add migration 27 without rewriting active jobs**

Create `notification_payload_v2_valid_detailed(candidate jsonb)`. It must:

```sql
-- Accept old payloads first.
notification_payload_v2_valid(candidate)
OR notification_payload_v2_valid_episode_number(candidate)
OR notification_payload_v2_valid_specials(candidate)
```

For detailed payloads, validate the optional `result` object, optional `media.origin` value, and new progress keys. Remove `result`, the five new progress keys, and `media.origin` from a normalized copy, then pass the normalized payload to one of the three prior validators. `media.origin`, when present, must equal `tracked-episode`. Replace `notification_payload_check` with an OR across the four validators.

The down migration must remove `result`, `media.origin`, and the five new progress keys from pending schema-v2 outbox payloads before restoring the migration-26 constraint, then drop `notification_payload_v2_valid_detailed(jsonb)`. It must not update `jobs`, `job_tasks`, or active lease state.

- [ ] **Step 8: Run migration tests and commit**

Run:

```bash
cargo test -p media-storage --features integration-tests --test migrations detailed_notifications -- --nocapture
```

Expected: migration-up validation, compatibility, rejection, and migration-down normalization tests pass.

Commit in `media-orchestrator`:

```bash
git add crates/media-core crates/media-contract crates/media-storage/src/migration crates/media-storage/tests/migrations.rs
git commit -m "feat: extend media notification result contract"
```

---

### Task 2: Final Artifact Probe And Truthful Processing Report

**Files:**
- Modify: `crates/media-runner/src/media.rs`
- Modify: `crates/media-runner/src/adapters.rs`
- Modify: `crates/media-runner/src/pipeline.rs`
- Modify: `crates/media-runner/src/lib.rs`
- Test: `crates/media-runner/tests/adapters.rs`
- Test: `crates/media-runner/tests/episode_pipeline.rs`

**Interfaces:**
- Produces: extended `MediaProbe` with video profile and audio codec/channel facts.
- Produces: `EpisodeReport { outcome: EpisodeOutcome, artifact: Option<PublishedArtifact> }`.
- Produces: `PublishedArtifact` with final probe, file size, subtitle counts, and actual processing.
- Preserves: existing Plex reconciliation and subtitle recovery behavior.

- [ ] **Step 1: Write failing ffprobe decoding tests**

Extend the ffprobe fixture in `crates/media-runner/tests/adapters.rs`:

```json
{
  "codec_type": "video",
  "codec_name": "hevc",
  "profile": "Main",
  "width": 1920,
  "height": 1080,
  "bit_rate": "2100000"
},
{
  "codec_type": "audio",
  "codec_name": "aac",
  "channels": 2,
  "channel_layout": "stereo",
  "tags": {"language": "rus", "title": "AniLibria"}
}
```

Assert the decoded probe contains `video_profile=Some("Main")`, `audio_codec=Some("aac")`, `audio_channels=Some(2)`, and `audio_channel_layout=Some("stereo")`.

- [ ] **Step 2: Run adapter test and verify RED**

Run:

```bash
cargo test -p media-runner --test adapters ffprobe -- --nocapture
```

Expected: compilation fails on the new `MediaProbe` fields.

- [ ] **Step 3: Extend ffprobe decoding**

Add these optional fields to `MediaProbe`:

```rust
pub video_profile: Option<String>,
pub audio_codec: Option<String>,
pub audio_channels: Option<u32>,
pub audio_channel_layout: Option<String>,
```

Add `profile`, `channels`, and `channel_layout` to the private `ProbeStream` in `adapters.rs`. Map the first video stream and first audio stream without inventing absent values. Update all test probe constructors with explicit `None` values.

- [ ] **Step 4: Write failing pipeline report tests**

Add focused tests in `crates/media-runner/tests/episode_pipeline.rs`:

```rust
assert_eq!(report.outcome, EpisodeOutcome::Completed);
let artifact = report.artifact.unwrap();
assert_eq!(artifact.file_size_bytes, 440_401_920);
assert_eq!(artifact.subtitles_downloaded, 2);
assert_eq!(artifact.subtitles_missing, 0);
assert_eq!(artifact.processing.unwrap().mode, ProcessingMode::VaapiUpscale);
assert_eq!(artifact.probe.width, 1920);
assert_eq!(artifact.probe.height, 1080);
```

Cover four paths:

- newly transcoded Rezka file has `VaapiUpscale` and elapsed time;
- already published Rezka file has final metadata but no processing claim for the current run;
- missing subtitle returns `Partial` with downloaded/missing counts and keeps artifact metadata;
- blocked storage and cancelled work return no artifact.

- [ ] **Step 5: Run pipeline tests and verify RED**

Run:

```bash
cargo test -p media-runner --test episode_pipeline artifact_report -- --nocapture
```

Expected: compilation fails because `EpisodeReport` and `PublishedArtifact` do not exist.

- [ ] **Step 6: Add the report types and measure only real processing**

Add to `pipeline.rs` and re-export from `lib.rs`:

```rust
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ProcessingMode {
    VaapiUpscale,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MediaProcessing {
    pub mode: ProcessingMode,
    pub elapsed_seconds: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PublishedArtifact {
    pub probe: MediaProbe,
    pub file_size_bytes: u64,
    pub subtitles_downloaded: u32,
    pub subtitles_missing: u32,
    pub processing: Option<MediaProcessing>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EpisodeReport {
    pub outcome: EpisodeOutcome,
    pub artifact: Option<PublishedArtifact>,
}
```

Change `EpisodePipeline::run` to return `Result<EpisodeReport, RunnerPortError>`. Measure `tokio::time::Instant::now()` immediately before `process.run`; set `VaapiUpscale` only after ffmpeg succeeds and the encoded probe passes validation. After atomic publication, probe `final_video`, read its length through `FileSystemPort::file_len`, and build `PublishedArtifact`. Count downloaded subtitles from the expected tracks whose final sidecars exist.

- [ ] **Step 7: Keep torrent behavior truthful**

For `ProviderKind::Torrent`, retain direct Plex reconciliation. Return `EpisodeReport { outcome, artifact: None }`; no VAAPI mode is emitted. Media-service will render this as published without transcoding and omit unavailable file facts.

- [ ] **Step 8: Run runner tests and commit**

Run:

```bash
cargo test -p media-runner --test adapters
cargo test -p media-runner --test episode_pipeline
cargo test -p media-runner
```

Expected: all media-runner tests pass.

Commit in `media-orchestrator`:

```bash
git add crates/media-runner
git commit -m "feat: report final published media facts"
```

---

### Task 3: Persist Final Artifact Facts In The Existing Task Checkpoint

**Files:**
- Modify: `crates/media/src/runner.rs`
- Test: inline `#[cfg(test)]` module in `crates/media/src/runner.rs`
- Test: `crates/media/tests/runner_loop.rs`

**Interfaces:**
- Consumes: `EpisodeReport` from Task 2.
- Produces: sanitized `media_pipeline` stage checkpoint keys persisted by existing runner events.
- Preserves: storage checkpoint keys and `ExecutionOutcome` mapping.

- [ ] **Step 1: Write failing checkpoint conversion tests**

Add a unit-level assertion for `artifact_checkpoint(&PublishedArtifact)`:

```rust
assert_eq!(checkpoint["artifact_video_codec"], CheckpointValueDto::String("hevc".into()));
assert_eq!(checkpoint["artifact_video_profile"], CheckpointValueDto::String("Main".into()));
assert_eq!(checkpoint["artifact_width"], CheckpointValueDto::Unsigned(1920));
assert_eq!(checkpoint["artifact_height"], CheckpointValueDto::Unsigned(1080));
assert_eq!(checkpoint["artifact_duration_seconds"], CheckpointValueDto::Unsigned(1421));
assert_eq!(checkpoint["artifact_file_size_bytes"], CheckpointValueDto::Unsigned(440_401_920));
assert_eq!(checkpoint["artifact_audio_language"], CheckpointValueDto::String("rus".into()));
assert_eq!(checkpoint["artifact_audio_codec"], CheckpointValueDto::String("aac".into()));
assert_eq!(checkpoint["artifact_audio_channels"], CheckpointValueDto::Unsigned(2));
assert_eq!(checkpoint["artifact_audio_channel_layout"], CheckpointValueDto::String("stereo".into()));
assert_eq!(checkpoint["artifact_audio_title"], CheckpointValueDto::String("AniLibria".into()));
assert_eq!(checkpoint["artifact_subtitles_downloaded"], CheckpointValueDto::Unsigned(2));
assert_eq!(checkpoint["artifact_subtitles_missing"], CheckpointValueDto::Unsigned(0));
assert_eq!(checkpoint["artifact_processing_mode"], CheckpointValueDto::String("vaapi-upscale".into()));
assert_eq!(checkpoint["artifact_processing_seconds"], CheckpointValueDto::Unsigned(252));
```

Assert no key contains `path`, `url`, `cookie`, or `token`. Assert control characters are removed and strings are bounded to 64 UTF-8 bytes.

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```bash
cargo test -p media --test runner artifact_checkpoint -- --nocapture
```

Expected: failure because `artifact_checkpoint` does not exist.

- [ ] **Step 3: Add deterministic checkpoint conversion**

Implement:

```rust
fn artifact_checkpoint(
    artifact: &media_runner::PublishedArtifact,
) -> BTreeMap<String, media_contract::CheckpointValueDto>
```

Round finite positive duration down to whole seconds. Insert optional keys only when the probe supplied the value. Use `sanitize_checkpoint_text(value: &str) -> Option<String>` to remove controls, trim whitespace, and truncate on a UTF-8 boundary to 64 bytes.

- [ ] **Step 4: Store the report on `media_pipeline` completion**

Replace the current outcome-only branch with:

```rust
let report = pipeline.run(&work, &cancellation, &reporter).await?;
let mut checkpoint = match &report.outcome {
    EpisodeOutcome::BlockedStorage(blocked) => storage_checkpoint(blocked),
    _ => BTreeMap::new(),
};
if let Some(artifact) = &report.artifact {
    checkpoint.extend(artifact_checkpoint(artifact));
}
control
    .stage_completed_with_checkpoint(task_ordinal, "media_pipeline", 1, checkpoint)
    .await?;
let outcome = report.outcome;
```

Keep the existing mapping from `EpisodeOutcome` to job transition. A subtitle partial therefore persists artifact fields before transitioning to partial.

- [ ] **Step 5: Run runner integration tests and commit**

Run:

```bash
cargo test -p media --test runner
cargo test -p media runner
```

Expected: task completion contains artifact fields, blocked storage retains both storage fields, and existing retry behavior passes.

Commit in `media-orchestrator`:

```bash
git add crates/media/src/runner.rs crates/media/tests/runner_loop.rs
git commit -m "feat: persist published artifact checkpoints"
```

---

### Task 4: Aggregate Exact Episode, Retry, Storage, And Final Result Notifications

**Files:**
- Modify: `crates/media-storage/src/repository/lease.rs`
- Modify: `crates/media-storage/src/repository/tracking.rs`
- Test: `crates/media-storage/tests/orchestration_repository.rs`
- Test: `crates/media-storage/tests/tracking_notification_repository.rs`

**Interfaces:**
- Consumes: artifact checkpoint keys from Task 3 and detailed domain types from Task 1.
- Produces: optional `result` in card/final-push payloads.
- Produces: exact single-episode identity, aggregate season facts, curated retry status, storage status, and `SearchAlternative` action.
- Preserves: stable card key, revision ordering, lifecycle cycle, final-push deduplication, recipient routing, and terminal locking.

- [ ] **Step 1: Write failing exact-episode and season aggregation tests**

Create repository fixtures for:

```text
single selected episode: provider S02E08, total task count 1
season: 12 tasks, 11 completed, S01E07 failed
tracked episode: result_ref selection:tracking:00000000-0000-0000-0000-000000000777:2:8
```

Assert the first terminal payload has `progress.current_episode=8`, `progress.total_episodes=1`, and publication episode `8`. Assert the season payload has `completed_episodes=11`, `total_episodes=12`, `missing_episodes=[S01E07]`, and actions `retry-missing`, `search-alternative`, `details`.

Assert the tracked job payload has `media.origin=tracked-episode`, the exact `S02E08` coordinate, and the same `media-job:00000000-0000-0000-0000-000000000999` card key for queued, downloading, processing, and terminal events. No separate discovery outbox row is expected when automatic download is enabled.

- [ ] **Step 2: Write failing final metadata aggregation tests**

Persist `media_pipeline` checkpoints for two completed tasks. Assert:

- `file_size_bytes`, `duration_seconds`, subtitle counts, and processing elapsed seconds are summed;
- video and audio are present only when all completed artifacts have the same measured values;
- differing audio layouts omit aggregate audio instead of choosing one;
- a one-episode payload exposes the complete measured video/audio result;
- Prowlarr emits `processing.mode=original` but no unmeasured probe fields.

- [ ] **Step 3: Write failing recovery and storage tests**

For retryable Rezka `StageFailed`, assert the nonterminal card contains:

```json
{
  "state": "downloading",
  "progress": {
    "connection_attempt": 5,
    "connection_attempt_limit": 20,
    "vpn_rotation_pending": true
  },
  "issue": {
    "code": "source_recovering",
    "message": "source transfer is being recovered"
  }
}
```

For terminal attempt 20, assert state `failed`, attempt `20/20`, and actions `retry`, `search-alternative`, `details`. For blocked storage, assert both available and required bytes are projected from the persisted stage checkpoint.

For `PlexPending`, assert the card remains in publish recovery, retains the measured artifact result, and offers a retry that re-enters the existing pipeline with the already published file so transfer and VAAPI are not repeated.

- [ ] **Step 4: Run repository tests and verify RED**

Run:

```bash
cargo test -p media-storage --features integration-tests --test orchestration_repository detailed_notification -- --nocapture
```

Expected: assertions fail because result aggregation and curated recovery fields are absent.

- [ ] **Step 5: Load task artifacts independently of the current event**

Add a private query that reads each selected task's canonical coordinate, task state, and merged `media_pipeline` checkpoint from `job_stages`. Convert only the fixed artifact keys into:

```rust
struct TaskArtifactProjection {
    season: u32,
    episode: u32,
    state: String,
    video: Option<MediaNotificationVideo>,
    audio: Option<MediaNotificationAudio>,
    subtitles: Option<MediaNotificationSubtitles>,
    file_size_bytes: Option<u64>,
    duration_seconds: Option<u64>,
    processing: Option<MediaNotificationProcessing>,
}
```

Reject malformed or overlarge checkpoint strings by omitting that field. Never copy arbitrary checkpoint keys into the notification.

- [ ] **Step 6: Aggregate only trustworthy facts**

Implement:

```rust
fn aggregate_result(
    context: &JobNotificationContext,
    tasks: &[TaskArtifactProjection],
) -> Option<MediaNotificationResult>
```

For one completed task, retain its measured fields. For multiple completed tasks, sum numeric totals using checked addition and retain video/audio only when every completed task has an equal value. Build publication from canonical title, media kind, season, and exact episode for a one-task selection. Use `Original` only for Prowlarr; use `VaapiUpscale` only from `artifact_processing_mode`.

- [ ] **Step 7: Project exact identity and curated recovery**

When `selected.len() == 1`, always set `current_episode` from the selected coordinate for active and terminal events. Do not compare the absolute episode number with `total_episodes`.

For retryable `StageFailed`, use the persisted stage attempt and provider limit (`20` for Rezka, `3` for Prowlarr). Set `vpn_rotation_pending=true` only for a retryable Rezka transfer failure that will cross the existing sticky-VPN attempt boundary. Never render the raw provider error as the primary issue message.

When `job.result_ref()` begins with `selection:tracking:`, set `media.origin=TrackedEpisode`. Do not infer tracked origin from title text or notification recipient.

- [ ] **Step 8: Add detailed result to payload conversion**

Extend `ProjectedNotification` with `result: Option<MediaNotificationResult>`. Extend `projected_payload` to serialize it through the Task 1 DTOs. Add `SearchAlternative` only to partial/failed states where choosing another provider is meaningful.

- [ ] **Step 9: Parse detailed persisted payloads for delivery**

Extend `delivery_from_row` in `repository/tracking.rs` to parse optional origin, result, recovery/storage progress, and `search-alternative`. Build the domain value through the Task 1 builder methods. Add a lease test proving both an old schema-v2 row and a detailed schema-v2 row become `NotificationContent::Media`.

- [ ] **Step 10: Run storage tests and commit**

Run:

```bash
cargo test -p media-storage --features integration-tests --test orchestration_repository
cargo test -p media-storage --features integration-tests --test tracking_notification_repository
cargo test -p media-storage --features integration-tests
```

Expected: exact episode, aggregation, retry, storage, lifecycle, routing, and stale-event tests pass.

Commit in `media-orchestrator`:

```bash
git add crates/media-storage/src/repository/lease.rs crates/media-storage/src/repository/tracking.rs crates/media-storage/tests
git commit -m "feat: project detailed media notification state"
```

---

### Task 5: Owner-Scoped Alternative Provider Search

**Files:**
- Modify: `crates/media-contract/src/search.rs`
- Modify: `crates/media-contract/src/lib.rs`
- Modify: `crates/media-api/src/search.rs`
- Modify: `crates/media-api/src/route/search.rs`
- Modify: `crates/media/src/search.rs`
- Modify: `crates/media/src/client.rs`
- Modify: `crates/media/src/main.rs`
- Modify: `crates/media/src/render.rs`
- Test: `crates/media-api/tests/search.rs`
- Test: `crates/media/tests/search_flow.rs`
- Test: `crates/media/tests/cli.rs`

**Interfaces:**
- Produces: `POST /v1/jobs/{job_id}/alternative-search`.
- Produces: `hermes-media jobs alternatives {job_id} [--json]`.
- Returns: normal `SearchPageDto` with up to five results and existing continuation/session behavior.
- Authorizes: job owner before reading the saved execution selection.

- [ ] **Step 1: Write failing route and service tests**

Add:

```rust
#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlternativeSearchRequest {
    pub scope: SearchScopeDto,
}
```

The route test posts this body to `/v1/jobs/00000000-0000-0000-0000-000000000999/alternative-search` and expects a normal `SearchPageDto`. Add rejection tests for another owner's job, an unknown job, and a `RezkaSessionRefresh` execution.

- [ ] **Step 2: Run API tests and verify RED**

Run:

```bash
cargo test -p media-api --test search alternative_search -- --nocapture
```

Expected: 404 or compilation failure because the route and service method are absent.

- [ ] **Step 3: Add the owner-scoped service operation**

Extend `SearchService`:

```rust
async fn start_alternative(
    &self,
    owner: UserId,
    job_id: JobId,
    request: AlternativeSearchRequest,
) -> Result<SearchPageDto, SearchError>;
```

In `DurableSearchService`, call `get_job_for_owner`, load `execution_for(job.result_ref())`, and derive:

```rust
let request = StartSearchRequest {
    scope: request.scope,
    source: opposite_provider,
    query: execution_title,
    media_kind: Some(execution_media_kind),
    season: execution_season,
    preferred_qualities: Vec::new(),
    preferred_languages: Vec::new(),
    preferred_codecs: Vec::new(),
    preferred_release_groups: Vec::new(),
};
self.start(owner, request).await
```

Map Rezka to Prowlarr and Prowlarr to Rezka. Preserve the selected season. Do not create a download job and do not fall back automatically.

- [ ] **Step 4: Wire the API route**

Add:

```rust
.route(
    "/v1/jobs/{job_id}/alternative-search",
    post(start_alternative),
)
```

Use the same authenticated actor extraction and error mapping as existing search routes. This endpoint needs no idempotency receipt because it creates an expiring search session, not a download.

- [ ] **Step 5: Add CLI and rendering tests**

Add `JobsCommand::Alternatives { job_id, json }`. Assert the human renderer lists source, up to five results, and the existing continuation hint without printing a standalone UUID. Assert `--json` returns the untouched `SearchPageDto`.

- [ ] **Step 6: Implement CLI call**

Add to `HttpClient`:

```rust
pub async fn alternative_search(
    &self,
    job_id: &str,
    scope: media_contract::SearchScopeDto,
) -> Result<String, ClientError> {
    let path = format!("v1/jobs/{job_id}/alternative-search");
    self.execute(
        self.request(reqwest::Method::POST, &path)?
            .timeout(Duration::from_secs(150))
            .json(&media_contract::AlternativeSearchRequest {
                scope,
            }),
    )
    .await
}
```

Pass the scope from `run_jobs` rather than calling the private function from `client.rs`. Render with the existing search page renderer.

- [ ] **Step 7: Run search and CLI tests and commit**

Run:

```bash
cargo test -p media-api --test search alternative_search
cargo test -p media --test search_flow alternative_search
cargo test -p media --test cli alternatives
```

Expected: owner isolation, opposite-provider selection, up-to-five results, pagination continuation, and non-download behavior pass.

Commit in `media-orchestrator`:

```bash
git add crates/media-contract crates/media-api crates/media/src crates/media/tests
git commit -m "feat: add alternative provider search action"
```

---

### Task 6: Deterministic Detailed Telegram Renderer And Five-Second Cards

**Working directory:** `/home/operator/Projects/personal/hermes-home`

**Files:**
- Modify: `scripts/hermes_media_notifications.py`
- Modify: `scripts/media-notifier`
- Test: `tests/test_media_notifications.py`
- Test: `tests/test_media_notifier.py`

**Interfaces:**
- Consumes: Task 1 schema-v2 payload including optional `result` and new progress fields.
- Produces: one deterministic Russian card, exact episode identity, direct actions, and one short final push.
- Preserves: revision/cycle ordering, terminal lock, replacement of deleted Telegram cards, and outbox retry semantics.

- [ ] **Step 1: Write failing strict parser tests**

Add fixtures for:

- detailed Rezka completed episode;
- Prowlarr completed season with `processing.mode=original`;
- tracked episode with `media.origin=tracked-episode`;
- retry attempt `5/20` with pending VPN rotation;
- blocked storage;
- old schema-v2 payload without `result`.

Assert unknown result fields, attempt `21/20`, malformed video dimensions, and one-sided storage values raise `NotificationParseError`.

- [ ] **Step 2: Write failing rendering tests**

Assert exact text fragments:

```python
assert "Клинки Хранителей · S02E08" in card.text
assert "Видео: 1920x1080 · HEVC Main" in card.text
assert "Аудио: русский · AAC Stereo" in card.text
assert "Субтитры: 2 дорожки" in card.text
assert "Обработка: VAAPI upscale · 4 мин 12 сек" in card.text
assert "Plex → Сериалы → Клинки Хранителей → Сезон 2" in card.text
assert render_push(notification) == "✅ Клинки Хранителей · S02E08 уже в Plex"
```

For one episode with `total_episodes=1`, assert the title is never only `Сезон 2`. For Prowlarr, assert `Без перекодирования` is present and `VAAPI` is absent. For absent metadata, assert the entire unavailable line is omitted.

For a tracked episode in queued/downloading state, assert the same card begins with `🆕 Найдена новая серия` and contains `⬇️ Автоматическое скачивание началось`. For `PlexPending`, assert the card says the prepared file is safe and only Plex publication will be retried.

- [ ] **Step 3: Run renderer tests and verify RED**

Run:

```bash
python3 -m unittest tests.test_media_notifications -v
```

Expected: parser or rendering assertions fail because detailed result support is absent.

- [ ] **Step 4: Add strict Python data classes and parser**

Add immutable data classes mirroring the exact Rust names, including `origin: str | None` on `MediaIdentity`:

```python
@dataclass(frozen=True)
class MediaResult:
    video: VideoResult | None
    audio: AudioResult | None
    subtitles: SubtitleResult | None
    file_size_bytes: int | None
    duration_seconds: int | None
    processing: ProcessingResult | None
    publication: PublicationResult | None
```

Extend `_TOP_LEVEL_FIELDS` with `result`. Add the five progress keys and `search-alternative` action. Keep unknown-field rejection and integer/bool separation.

- [ ] **Step 5: Render exact identity and truthful details**

Use:

```python
def media_identity(notification: Notification) -> str:
    progress = notification.progress
    if (
        notification.media.kind == "series"
        and progress is not None
        and progress.current_episode is not None
        and progress.total_episodes == 1
    ):
        season = notification.media.season or 0
        return f"{notification.media.title} · S{season:02}E{progress.current_episode:02}"
    if notification.media.kind == "series" and notification.media.season is not None:
        return f"{notification.media.title} · Сезон {notification.media.season}"
    return notification.media.title
```

Render only present result fields. Map `rus` to `русский`, `aac` to `AAC`, `hevc` to `HEVC`, and `stereo` to `Stereo`; unknown safe values remain visible as supplied. Format durations and byte sizes with existing helpers.

- [ ] **Step 6: Render recovery, storage, and partial season copy**

For `source_recovering`, render `Попытка соединения: N из M` and the VPN line only when `vpn_rotation_pending` is true. Never use the words `раздача активна` for an attempt counter. For storage, show required and available sizes. For a partial season, show published and missing episode coordinates and attach `Повторить`, `Выбрать другой источник`, and `Диагностика`.

- [ ] **Step 7: Change routine card throttle to five seconds**

Set:

```python
PROGRESS_THROTTLE_SECONDS = 5.0
```

Keep stage, terminal, action-set, lifecycle-cycle, and revision transitions immediate. Extend `CardState` with a SHA-256 fingerprint of rendered text plus callback data, bump the state file to version 3, and migrate version-2 cards with an empty fingerprint. If a newer revision renders the same text and actions, acknowledge it without calling `editMessageText`. Add notifier tests proving unchanged content is skipped, two changed progress updates inside five seconds coalesce, and a processing transition edits immediately.

- [ ] **Step 8: Run Hermes rendering tests and commit**

Run:

```bash
python3 -m unittest tests.test_media_notifications tests.test_media_notifier -v
```

Expected: detailed cards, legacy cards, terminal push deduplication, stale revision rejection, and five-second throttling pass.

Commit in `hermes-home`:

```bash
git add scripts/hermes_media_notifications.py scripts/media-notifier tests/test_media_notifications.py tests/test_media_notifier.py
git commit -m "feat: render detailed media lifecycle cards"
```

---

### Task 7: Inline Alternative Search, Native Choices, And Quiet Telegram

**Working directory:** `/home/operator/Projects/personal/hermes-home`

**Files:**
- Modify: `shared/plugins/telegram-home/__init__.py`
- Modify: `shared/skills/media/SKILL.md`
- Modify: `profiles/primary/config/config.yaml`
- Modify: `profiles/secondary/config/config.yaml`
- Create: `tests/test_telegram_home_plugin.py`
- Create: `tests/test_media_skill.py`
- Test: `tests/test_scaffold.py`

**Interfaces:**
- Consumes: `ma:search-alternative:{job_id}` callback from Task 6 and `hermes-media jobs alternatives` from Task 5.
- Produces: direct owner-authorized alternative search reply without invoking the model.
- Uses: Hermes native `clarify` for conversational choices.
- Produces: Telegram-only `tool_progress: "off"` while retaining current progress behavior on other platforms.

- [ ] **Step 1: Write failing callback tests**

Extend `_CALLBACK_RE` with `search-alternative`. Assert:

- an authorized owner invokes `hermes-media jobs alternatives {job_id} --json`;
- the returned search page is rendered as a concise reply with up to five choices;
- another Telegram user receives `Действие недоступно`;
- stale/malformed callback data makes no subprocess call;
- service failure receives `Поиск другого источника временно недоступен`;
- no response contains a raw UUID except explicit `details`.

- [ ] **Step 2: Run plugin tests and verify RED**

Run:

```bash
python3 -m unittest tests.test_telegram_home_plugin -v
```

Expected: the new callback is rejected by the current regular expression.

- [ ] **Step 3: Implement the direct callback**

Extend:

```python
_CALLBACK_RE = re.compile(
    r"ma:(cancel|retry|retry-missing|resume-storage|details|search-alternative):"
    r"([0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12})\Z"
)
```

Map `search-alternative` to:

```python
("jobs", "alternatives", job_id, "--json")
```

Parse the JSON through a bounded renderer that accepts a top-level search page, emits at most five results, and includes the normal “show more” wording only when a continuation exists. Keep the existing callback user authorization before subprocess execution.

- [ ] **Step 4: Lock the media skill to native Telegram controls**

In `shared/skills/media/SKILL.md`, state:

```text
For conversational choices, call Hermes native `clarify`; Telegram renders its options
as an inline keyboard. Never print XML-like quick-reply tags. Job card actions are owned
by media-notifier and must not be duplicated in conversational text.
```

Retain the existing rule that successful results from one provider are shown when the other fails. Add a test that rejects `<telegram-quick-replies>` and requires `clarify`.

- [ ] **Step 5: Disable only Telegram tool progress**

In both profile configs, keep the existing global value and add:

```yaml
display:
  tool_progress: new
  platforms:
    telegram:
      tool_progress: "off"
```

This follows the official Hermes per-platform configuration contract. The native `clarify` Telegram behavior is documented by Hermes at:

- https://github.com/NousResearch/hermes-agent/blob/main/website/docs/user-guide/messaging/telegram.md
- https://github.com/NousResearch/hermes-agent/blob/main/website/docs/user-guide/configuration.md

- [ ] **Step 6: Run Hermes tests and commit**

Run:

```bash
python3 -m unittest tests.test_telegram_home_plugin tests.test_media_skill tests.test_scaffold -v
python3 -m unittest discover -s tests -v
```

Expected: callback ownership, direct alternative search, native-choice rules, no pseudo markup, and both profile configs pass.

Commit in `hermes-home`:

```bash
git add shared/plugins/telegram-home shared/skills/media profiles/primary/config/config.yaml profiles/secondary/config/config.yaml tests
git commit -m "feat: improve Telegram media actions"
```

---

### Task 8: Cross-Repository Verification, Safe Deployment, UI Evidence, And Cleanup

**Working directories:**
- `/home/operator/Projects/personal/media-orchestrator`
- `/home/operator/Projects/personal/hermes-home`

**Files:**
- Modify: `docs/ACCEPTANCE.md`
- Create: `docs/evidence/2026-07-23-detailed-telegram-media-notifications.md`

**Interfaces:**
- Consumes: compatible commits from Tasks 1-7.
- Produces: deployed service/runner/Hermes versions and live evidence.
- Preserves: active downloads and all non-test media.

- [x] **Step 1: Run complete local verification**

In `media-orchestrator`:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
mise run check
git diff --check
```

In `hermes-home`:

```bash
python3 -m unittest discover -s tests -v
git diff --check
```

Expected: every command exits zero.

- [x] **Step 2: Verify synthetic cards before touching live jobs**

Send signed synthetic payloads for:

```text
tracked S02E08 discovered/downloading
single S02E08 processing
season 5/12 downloading
Rezka completed with measured VAAPI result
Prowlarr completed without transcoding
retry 5/20 with VPN rotation
blocked storage
partial 11/12 with S01E07 missing
terminal failed 20/20
```

Assert each lifecycle edits one Telegram message. Assert the completed payload creates exactly one short reply. Assert delayed progress cannot overwrite a terminal card.

- [x] **Step 3: Inspect live queue before deployment**

Run:

```bash
ssh host.example.invalid \
  'docker exec hermes-primary hermes-media queue status --json'
```

If `active` is true, build images but defer replacement of `download-runner` until the current task reaches a safe terminal or stage boundary. Do not cancel the job.

- [x] **Step 4: Push both repositories and deploy through the existing guarded path**

Push `main` in each repository, then from `media-orchestrator` run:

```bash
mise run homelab-deploy
```

Expected: migrations complete, health gates pass, and the deployment guard refuses an unsafe runner replacement rather than interrupting work.

- [x] **Step 5: Verify deployed health and configuration**

Run:

```bash
ssh host.example.invalid \
  "docker ps --format '{{.Names}} {{.Status}}' | \
   grep -E '^(media-service|download-runner|gluetun-rezka|hermes-primary|hermes-secondary)'"
ssh host.example.invalid \
  'docker exec hermes-primary hermes-media queue status --json'
ssh host.example.invalid \
  'docker logs --since 10m media-service'
ssh host.example.invalid \
  'docker logs --since 10m download-runner'
```

Expected: containers are healthy, queue state is coherent, and logs contain no migration, payload validation, callback, or Telegram delivery errors.

- [x] **Step 6: Verify Telegram mobile and Web Telegram**

Use Chrome/Computer Use against the existing authenticated sessions. Confirm:

- no shell/terminal progress blocks appear;
- a native conversational choice appears as inline buttons;
- job card buttons are attached under the card;
- one button callback does not create a model tool trace;
- exact episode identity and detailed final layout fit without truncation;
- no raw quick-reply tag, standalone UUID, or `execution_failed` appears.

- [x] **Step 7: Run one real low-volume Rezka episode**

Choose one explicitly requested episode that is not currently active. Confirm in the card:

1. exact `SxxExx`;
2. transfer progress edits the same message every 5-10 seconds when values change;
3. processing says VAAPI only after runner reports it;
4. final card is edited in place;
5. exactly one short completion reply is sent.

Immediately before creating the job, record:

```bash
TEST_STARTED_AT="$(date -u '+%Y-%m-%d %H:%M:%S UTC')"
```

Find the only media file created after the recorded test start timestamp and compare the final card with:

```bash
TEST_FILE="$(
  ssh host.example.invalid \
    "find /mnt/internal/torrents/tv -type f -newermt '${TEST_STARTED_AT}' \
     \( -name '*.mkv' -o -name '*.mp4' \) -print" |
  head -n 1
)"
ssh host.example.invalid \
  "ffprobe -v error -show_streams -show_format -of json '$TEST_FILE'"
```

and with the published subtitle sidecars and Plex canonical season/episode placement. Record only sanitized measurements in evidence; do not record the provider URL or local secret material.

- [x] **Step 8: Exercise controlled recovery**

Use a synthetic retry event or a naturally occurring transient transfer failure. Confirm the same card shows `Попытка соединения: N из 20`, VPN rotation wording only when scheduled, and no per-attempt message spam. Use a synthetic terminal failure to verify `Выбрать другой источник`; confirm it opens an opposite-provider search with up to five results and does not create a download.

- [x] **Step 9: Remove test artifacts**

Delete only the test episode and its subtitle sidecars from the test Plex location, remove its staging directory, trigger the appropriate Plex library scan, and verify the item disappears. Do not remove pre-existing media, completed user jobs, or current tracking subscriptions.

- [x] **Step 10: Record evidence and final acceptance**

Write `docs/evidence/2026-07-23-detailed-telegram-media-notifications.md` with:

- exact local commits and deployed image IDs;
- test command results;
- synthetic lifecycle results;
- Telegram mobile/Web observations;
- sanitized ffprobe comparison;
- real card/push count;
- recovery and alternative-search observations;
- cleanup confirmation.

Update the notification row in `docs/ACCEPTANCE.md` to `Implemented`, `Deployed`, and `Live verified` only when all three have direct evidence.

- [x] **Step 11: Commit evidence**

Run:

```bash
git add docs/ACCEPTANCE.md docs/evidence/2026-07-23-detailed-telegram-media-notifications.md
git commit -m "docs: record detailed notification verification"
git push origin main
```

Expected: `main` is clean and matches `origin/main` in both repositories.
