# Telegram Notification Lifecycle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace noisy, stale media messages with one deterministic Telegram lifecycle card per job, a short terminal push, direct inline actions, and trustworthy season progress.

**Architecture:** `media-service` remains the source of truth and emits a signed structured notification envelope through its transactional outbox. Hermes validates and renders that envelope without an LLM, persists card revision/cycle state, edits one Telegram message, and executes owner-scoped actions through the existing `hermes-media` CLI. A lifecycle cycle distinguishes an explicit retry from stale events while the outbox generation remains both the delivery CAS and card revision.

**Tech Stack:** Rust 1.97, SeaORM/PostgreSQL, Axum, reqwest/HMAC, Python 3, python-telegram-bot, Docker Compose, Telegram Bot API.

## Global Constraints

- Automatic cards, final pushes, and inline actions must never invoke an LLM.
- One job owns one stable card key; a season job never creates one card per episode.
- Routine card updates occur at most every ten seconds, with immediate stage and terminal updates.
- A final card is edited in place; a separate push contains only outcome and title.
- Primary cards never expose job IDs, paths, raw attempt counters, internal stages, or error codes.
- Rezka and Prowlarr share one layout but render source-appropriate trustworthy metrics.
- A failed season with completed tasks is presented as partial success and retry resets only unfinished tasks.
- Existing active jobs and legacy pending notification rows must remain readable during deployment.
- Progress stays initiator-only; configured family terminal routing remains unchanged.
- No new secret, browser flow, or model call is introduced.

---

### Task 1: Structured Notification Domain And Backward-Compatible Storage

**Files:**
- Modify: `crates/media-core/src/notification.rs`
- Modify: `crates/media-contract/src/notification.rs`
- Modify: `crates/media-contract/tests/tracking_notifications.rs`
- Create: `crates/media-storage/src/migration/m20260720_000024_structured_notifications.rs`
- Modify: `crates/media-storage/src/migration/mod.rs`
- Modify: `crates/media-storage/tests/migrations.rs`

**Interfaces:**
- Produces: `NotificationContent::{LegacyMessage(String), Media(MediaNotification)}`.
- Produces: `MediaNotificationDeliveryKind::{Card, FinalPush}`.
- Produces: lifecycle, media identity, progress, issue, and action domain value types with bounded constructors.
- Produces: `HermesMediaNotificationWebhook` schema version `2` in `media-contract`.
- Produces: `jobs.notification_cycle bigint NOT NULL DEFAULT 1`.

- [ ] **Step 1: Write failing contract serialization tests**

Add an exact JSON assertion for this wire shape and retain a legacy webhook assertion:

```json
{
  "event_type": "media.notification",
  "schema_version": 2,
  "delivery_kind": "card",
  "card_key": "media-job:00000000-0000-0000-0000-000000000999",
  "revision": 7,
  "lifecycle_cycle": 1,
  "terminal": false,
  "state": "downloading",
  "media": {
    "job_id": "00000000-0000-0000-0000-000000000999",
    "title": "Example Show",
    "kind": "series",
    "provider": "rezka",
    "season": 1,
    "translation": "AniLibria"
  },
  "progress": {
    "completed_episodes": 7,
    "total_episodes": 12,
    "current_episode": 8,
    "downloaded_bytes": 195035136,
    "download_speed_bps": 5452595
  },
  "stage": "download",
  "next_step": "process",
  "actions": ["cancel", "details"]
}
```

- [ ] **Step 2: Run contract tests and verify RED**

Run:

```bash
cargo test -p media-contract tracking_notifications -- --nocapture
```

Expected: compile failure because the structured DTOs do not exist.

- [ ] **Step 3: Add bounded domain and wire types**

Keep `media-core` serialization-free. Add typed enums and structs to `media-core`; mirror them with `serde(rename_all = "kebab-case")` DTOs in `media-contract`. Validate non-empty bounded display fields, sane episode counts, percentage `0..=100`, and card keys using the existing safe character set. Keep `LegacyMessage` solely for rows created before migration 24.

- [ ] **Step 4: Write failing migration tests**

Assert migration 24:

```sql
ALTER TABLE jobs ADD COLUMN notification_cycle bigint NOT NULL DEFAULT 1;
```

and replaces `notification_payload_check` with a constraint accepting either the exact legacy `{message}` object or schema-version-2 structured payload. Reject arbitrary JSON objects and non-positive cycles.

- [ ] **Step 5: Implement and register migration 24**

Do not rewrite existing outbox rows or active jobs. The down migration restores the text-only constraint only after deleting schema-v2 rows, and removes `notification_cycle`.

- [ ] **Step 6: Run focused tests and commit**

Run:

```bash
cargo test -p media-contract tracking_notifications
cargo test -p media-storage --features integration-tests --test migrations structured_notifications
```

Commit: `feat: add structured media notification contract`

---

### Task 2: One Card Projection, Season Progress, And Retry Cycle

**Files:**
- Modify: `crates/media-storage/src/repository/lease.rs`
- Modify: `crates/media-storage/src/repository/tracking.rs`
- Modify: `crates/media-storage/src/repository/job.rs`
- Test: `crates/media-storage/tests/orchestration_repository.rs`
- Test: `crates/media-storage/tests/tracking_notification_repository.rs`
- Test: `crates/media-storage/tests/job_repository.rs`

**Interfaces:**
- Consumes: Task 1 domain notification types.
- Produces: one mutable `Card` row per `(job, recipient)` and one immutable `FinalPush` row per `(job, lifecycle_cycle, recipient, terminal_state)`.
- Produces: `NotificationDelivery::content()`, `card_key()`, `generation()`, and `lifecycle_cycle()`.
- Preserves: existing `POST /v1/jobs/{id}/retry`; no new API route is required.

- [ ] **Step 1: Add failing repository tests for the lifecycle card**

Cover all of these cases:

```text
started -> download-progress -> publishing -> completed
```

leaves one structured card row whose generation increases and whose final payload is terminal, plus one final-push row. Assert `completed`, `partial`, and `failed` retain the same `media-job:<job-id>` card key.

- [ ] **Step 2: Add failing season aggregation tests**

Create 12 selected episodes and persisted task states with 11 completed and one failed. Assert the card state is `partial`, progress is `11/12`, `missing_episodes` is `[12]`, actions are `retry-missing` and `details`, and no generic all-or-nothing failure copy is stored.

For an active task ordinal, assert the structured progress maps ordinal back to the selected episode coordinate. For legacy active jobs without task rows, assert optional episode progress is omitted rather than invented.

- [ ] **Step 3: Replace Markdown projection with structured projection**

Refactor `JobNotificationContext` into safe source data and remove `details()`, `message()`, `status_icon()`, progress-bar formatting, and Russian stage rendering from storage. Project:

```rust
struct ProjectedNotification {
    event_type: NotificationEventType,
    delivery_kind: MediaNotificationDeliveryKind,
    content: MediaNotification,
}
```

Query selected episode coordinates plus `job_tasks.ordinal/state` to derive completed, missing, and current episode counts. Subtitle-only partial keeps its own issue reason; missing-episode partial lists episode coordinates.

- [ ] **Step 4: Coalesce terminal events into the same card row**

Use the stable card dedupe key for every `Card` delivery, including terminal states. Produce `FinalPush` with a dedupe key containing job ID, lifecycle cycle, terminal state, and recipient. Insert card before push in one transaction.

Keep generation-aware acknowledgement semantics unchanged. `delivery_from_row()` must parse both legacy and schema-v2 payloads and must no longer clear the card key for terminal events.

- [ ] **Step 5: Increment lifecycle cycle on explicit retry**

In the existing owner-scoped retry transaction, execute:

```sql
UPDATE jobs
SET state = 'queued',
    notification_cycle = notification_cycle + 1,
    needs_action_reason = NULL,
    error_snapshot = NULL,
    attempt_count = 0,
    started_at = NULL,
    completed_at = NULL,
    updated_at = now()
WHERE id = $1
```

Retain the current behavior that resets only failed tasks/stages for failed or partial jobs. Therefore `retry-missing` in Telegram maps to this same endpoint and does not redownload completed episodes.

- [ ] **Step 6: Verify routing and legacy rows**

Assert initiator-only progress, family terminal delivery, owner-only mutating actions, and successful leasing of a pre-migration `{message}` row.

- [ ] **Step 7: Run focused tests and commit**

Run:

```bash
cargo test -p media-storage --features integration-tests --test orchestration_repository notification
cargo test -p media-storage --features integration-tests --test job_repository retry
cargo test -p media-storage --features integration-tests --test tracking_notification_repository
```

Commit: `feat: project one media lifecycle card`

---

### Task 3: Generation-Aware Signed Hermes Webhook

**Files:**
- Modify: `crates/media-integrations/src/hermes.rs`
- Modify: `crates/media-integrations/tests/hermes_webhook.rs`
- Modify: `crates/media-contract/tests/tracking_notifications.rs`

**Interfaces:**
- Consumes: structured `NotificationDelivery` from Task 2.
- Produces: schema-v2 signed webhook body and delivery identity `<notification-id>-<generation>`.
- Preserves: legacy signed webhook body for `NotificationContent::LegacyMessage`.

- [ ] **Step 1: Add failing exact-body tests**

Assert card and push envelopes serialize exactly, contain no provider URL/path/token, and use:

```text
X-Request-ID: <notification-uuid>-<generation>
```

This is required because a coalesced outbox row keeps its UUID while generation changes; using UUID alone causes Hermes to discard new progress as a duplicate.

- [ ] **Step 2: Implement domain-to-contract conversion**

Map enums exhaustively. Use `generation` as webhook `revision`, preserve `notification_cycle`, and include the same `card_key` in both card and push envelopes so Hermes can reply to the card.

- [ ] **Step 3: Preserve failure taxonomy**

Keep 408/429/5xx and transport failures retryable, deterministic 4xx terminal, and redaction of endpoints and secrets. Do not parse Telegram response content.

- [ ] **Step 4: Run tests and commit**

Run:

```bash
cargo test -p media-integrations --test hermes_webhook
cargo test -p media-contract tracking_notifications
```

Commit: `feat: deliver structured media notifications`

---

### Task 4: Deterministic Hermes Renderer And Card State

**Files in `/Users/operator/Projects/personal/hermes-home`:**
- Create: `scripts/hermes_media_notifications.py`
- Modify: `scripts/patch_hermes_telegram.py`
- Modify: `Dockerfile`
- Create: `tests/test_media_notifications.py`
- Modify: `tests/test_scaffold.py`

**Interfaces:**
- Produces: `parse_notification(payload) -> MediaNotification`.
- Produces: `render_card(notification) -> RenderedCard` and `render_push(notification) -> str`.
- Produces: `decide_update(stored, incoming) -> send | edit | ignore | retry`.
- Produces: callback protocol `ma:<action-code>:<job-uuid>`, always below Telegram's 64-byte limit.
- Produces: persisted schema `{"version": 2, "cards": {...}, "push_receipts": [...]}`.

- [ ] **Step 1: Write pure failing renderer tests**

Cover balanced Rezka and Prowlarr cards, absent HLS totals, `7/12` season progress, `11/12` partial result, storage blocking, user-facing VPN recovery, terminal results, hidden IDs/error codes, and action labels.

Expected active shape:

```text
⬇️ Магия и мускулы · Сезон 1

📺 Готово: 7 из 12 серий
🎙 AniLibria · Rezka
📦 Серия 8: 186 МБ · 5,2 МБ/с

🔄 Скачиваю исходное видео
➡️ Далее: обработка и добавление в Plex
```

- [ ] **Step 2: Write state-machine tests**

Assert stale/equal revisions are ignored, routine progress inside ten seconds is deferred, a stage change bypasses the throttle, a terminal update blocks later non-terminal updates in the same lifecycle cycle, a larger lifecycle cycle reopens the same card after explicit retry, a missing Telegram message causes replacement send, and an acknowledged push receipt suppresses replay.

Migrate legacy persisted values such as `{"chat:key": "123"}` to revision `0`, cycle `0`, and non-terminal state.

- [ ] **Step 3: Implement the pure notification module**

Use standard-library dataclasses, enums, JSON, and atomic file replacement. Keep validation fail-closed and bound card count and push receipt count to 1000. Rendering is deterministic Russian text and never imports Hermes agent/model code.

- [ ] **Step 4: Wire structured direct delivery into the webhook patch**

For schema version 2:

- render `Card` and send/edit using stored message ID;
- pass inline actions in Telegram metadata;
- render `FinalPush` as a new message with `telegram_reply_to_message_id` set to the stored card message ID;
- return a retryable failure when a push arrives before its required terminal card revision;
- store card state or push receipt only after acknowledged Telegram success.

Fix deliver-only idempotency so a failed direct delivery removes its pre-recorded delivery ID; otherwise the next outbox retry is incorrectly answered as a duplicate.

- [ ] **Step 5: Wire direct media callbacks before `qr:` callbacks**

Authorize the Telegram caller with `_is_callback_user_authorized`. Map action codes to explicit argv only:

```text
cancel          -> hermes-media jobs cancel <job-id>
retry           -> hermes-media jobs retry <job-id>
retry-missing   -> hermes-media jobs retry <job-id>
resume-storage  -> hermes-media jobs retry <job-id>
details         -> hermes-media jobs get <job-id>
```

Use `asyncio.create_subprocess_exec`, a bounded timeout, no shell, and the existing profile-scoped token wrapper. Never call `handle_message()`. Mutating callbacks answer briefly and wait for the next service card update; details send a sanitized reply to the card.

- [ ] **Step 6: Add image wiring and patch-contract tests**

Copy the new module into `/opt/hermes/gateway/platforms/media_notifications.py` in both patch and final stages. Keep patch anchors fail-closed against the pinned Hermes commit. Assert `ma:` is separate from conversational `qr:` and no media callback reaches the LLM.

- [ ] **Step 7: Run Hermes checks and commit**

Run:

```bash
./scripts/check
python3 -m unittest tests.test_media_notifications -v
```

Commit: `feat: render media lifecycle cards in Telegram`

---

### Task 5: Skill Contract And Cross-Repository Verification

**Files in `/Users/operator/Projects/personal/hermes-home`:**
- Modify: `shared/skills/media/SKILL.md`
- Modify: `tests/test_scaffold.py`

**Files in media-orchestrator:**
- Update after successful live test: `docs/evidence/2026-07-20-telegram-notification-lifecycle.md`

**Interfaces:**
- Documents: automatic ten-second updates, one lifecycle card, terminal replacement, short push, partial seasons, and direct actions without LLM.
- Removes: the obsolete rule that terminal notifications remain separate full messages.

- [ ] **Step 1: Add failing skill assertions**

Require all approved behavior and forbid claims that `completed` alone proves transcoding/upscale. Keep explicit user status requests owner-scoped through `hermes-media jobs get`.

- [ ] **Step 2: Update the shared media skill**

State that direct webhook cards must not be answered or summarized by the agent. Explain that technical IDs are shown only after `Подробнее` and that `retry-missing` preserves completed episodes.

- [ ] **Step 3: Run complete local verification**

Run in media-orchestrator:

```bash
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```

Run in hermes-home:

```bash
./scripts/check
git diff --check
```

- [ ] **Step 4: Commit the skill update**

Commit: `docs: explain media lifecycle notifications`

---

### Task 6: Safe Deployment And Telegram E2E

**Files:**
- Verify deployment scripts and evidence only; no planned runtime configuration changes.

**Interfaces:**
- Consumes: compatible schema-v2 Hermes image and media-service image.
- Produces: live evidence for card editing, final push, callback action, and no LLM tool activity.

- [ ] **Step 1: Inspect live work before deployment**

Read queue and container state. Do not cancel active jobs. Build versioned images while downloads continue; recreate media/Hermes containers only at a safe stage boundary using the repository deployment path.

- [ ] **Step 2: Deploy Hermes-compatible receiver first**

Deploy Hermes before media-service starts emitting schema v2. Confirm both profiles are healthy and the legacy notification path still works.

- [ ] **Step 3: Deploy media-service and runner**

Run migration 24, recreate services, and confirm queue ownership, Gluetun routing, PostgreSQL readiness, and notification outbox health.

- [ ] **Step 4: Run synthetic delivery tests**

Send signed card revisions `1`, `2`, stale `1`, terminal `3`, and stale active `2`. Confirm one Telegram message is created, only newer revisions edit it, and terminal state remains. Send final push twice with the same acknowledged delivery identity and confirm one push.

- [ ] **Step 5: Run direct callback tests**

Use an owned safe fixture job to verify `Подробнее` and one non-destructive retry/status action. Confirm Telegram allowlist enforcement and logs show no model invocation.

- [ ] **Step 6: Run one real low-volume download**

Use one explicitly selected small episode. Through Chrome or Computer Use, observe a single card from queued through download, processing, Plex publication, terminal replacement, and short reply. Confirm user-visible language/audio/episode data and verify no unrelated active job was cancelled.

- [ ] **Step 7: Record evidence, push main branches, and report**

Write exact image tags, commits, Telegram observations, queue state, and any limitation to the evidence file. Push only verified `main` commits in both repositories and confirm local heads match origin.
