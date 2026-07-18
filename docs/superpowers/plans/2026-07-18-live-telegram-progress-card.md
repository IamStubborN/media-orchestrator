# Live Telegram Progress Card Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Update one Telegram job card from five-second transfer checkpoints without invoking an LLM while preserving separate terminal notifications.

**Architecture:** Reuse the existing transactional notification outbox and Hermes `deliver_only` status-key editor. All non-terminal job card events coalesce into one row per job and recipient; an outbox generation prevents an acknowledgement for an in-flight older payload from consuming a newer payload.

**Tech Stack:** Rust, SeaORM raw PostgreSQL statements, Axum service runtime, signed Hermes webhook, Python Hermes patch contract tests.

## Global Constraints

- Automatic progress updates must never invoke the LLM.
- Runner checkpoint cadence remains approximately five seconds.
- Progress remains best-effort and must never fail a media job.
- Only trustworthy fields are rendered; HLS never invents percentage or ETA.
- Progress is initiator-only; terminal notifications keep configured scope.
- Completion, partial, and failure remain separate Telegram messages.
- No provider URL, hash, path, credential, token, or raw checkpoint is exposed.

---

### Task 1: Generation-Safe Coalescing Outbox

**Files:**
- Create: `crates/media-storage/src/migration/m20260718_000021_notification_generation.rs`
- Modify: `crates/media-storage/src/migration/mod.rs`
- Modify: `crates/media-core/src/notification.rs`
- Modify: `crates/media-storage/src/repository/tracking.rs`
- Modify: `crates/media-storage/src/repository/lease.rs`
- Test: `crates/media-storage/tests/migrations.rs`
- Test: `crates/media-storage/tests/tracking_notification_repository.rs`
- Test: `crates/media-storage/tests/orchestration_repository.rs`
- Test: `crates/media-core/tests/tracking_runtime.rs`

**Interfaces:**
- Produces: `NotificationDelivery::generation() -> u64`.
- Produces: generation-aware `mark_delivered`, `mark_failed`, and `mark_dead` methods.
- Produces: one mutable non-terminal status row identified by `media-job-status:<job-id>` plus recipient uniqueness.

- [ ] **Step 1: Add failing migration and repository tests**

Assert that migration 21 adds `notification_outbox.generation`, that two non-terminal job events leave one row, changed payload increments generation, and unchanged payload does not reset delivery.

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```bash
cargo test -p media-storage --features integration-tests --test migrations notification_generation
cargo test -p media-storage --features integration-tests --test orchestration_repository coalesces
```

Expected: failures because the generation migration and coalescing upsert do not exist.

- [ ] **Step 3: Add generation migration and domain contract**

Migration SQL adds:

```sql
ALTER TABLE notification_outbox
ADD COLUMN generation bigint NOT NULL DEFAULT 1,
ADD CONSTRAINT notification_generation_positive CHECK (generation > 0)
```

Rehydrate a positive `u64` generation into `NotificationDelivery`. Extend acknowledgement methods with `generation: u64`.

- [ ] **Step 4: Implement one-row non-terminal upsert**

For status-card events use one dedupe key and:

```sql
ON CONFLICT (source_dedupe_key, recipient) DO UPDATE SET
  event_type = EXCLUDED.event_type,
  payload = EXCLUDED.payload,
  generation = notification_outbox.generation + 1,
  delivered_at = NULL,
  dead_at = NULL,
  next_attempt_at = now(),
  attempt_count = 0,
  last_error_code = NULL
WHERE notification_outbox.event_type IS DISTINCT FROM EXCLUDED.event_type
   OR notification_outbox.payload IS DISTINCT FROM EXCLUDED.payload
```

Do not clear an active lease during the upsert.

- [ ] **Step 5: Make acknowledgements generation-aware**

When the leased generation is current, apply normal delivered/retry/dead behavior. When it is obsolete, clear the lease, leave the latest row pending, and schedule it immediately without incrementing attempts or dead-lettering it.

- [ ] **Step 6: Run focused tests and commit**

Run the migration, core dispatcher, tracking outbox, and orchestration repository tests. Commit as `feat: coalesce job status notifications`.

---

### Task 2: Deterministic Checkpoint Cards

**Files:**
- Modify: `crates/media-core/src/notification.rs`
- Modify: `crates/media-storage/src/repository/lease.rs`
- Modify: `crates/media-storage/src/migration/m20260718_000021_notification_generation.rs`
- Test: `crates/media-storage/tests/orchestration_repository.rs`
- Test: `crates/media-contract/tests/tracking_notifications.rs`

**Interfaces:**
- Produces: `NotificationEventType::DownloadProgress` with wire value `download-progress`.
- Consumes: normalized checkpoint keys already persisted by `download-runner`.

- [ ] **Step 1: Add failing checkpoint projection tests**

Report two `StageCheckpoint` events for `torrent_monitor` and assert one initiator row whose second message contains the updated percentage, byte totals, speed, ETA, seeds, peers, title, stage, and Job ID. Add HLS and malformed-checkpoint cases.

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```bash
cargo test -p media-storage --features integration-tests --test orchestration_repository live_progress
```

Expected: no progress notification is currently created for `StageCheckpoint`.

- [ ] **Step 3: Add bounded progress parsing and formatting**

Parse only `direct`, `hls`, and `torrent`. Validate percentage `0..=100`, non-negative integer metrics, downloaded not greater than total, and bounded safe source state. Render a ten-cell ASCII bar and binary byte units. Omit missing values.

- [ ] **Step 4: Route progress to the initiator and stable status key**

Treat `DownloadProgress` as a progress milestone. Use the same non-terminal status dedupe key and existing `media-job:<id>` status key as lifecycle milestones. Keep terminal rows separate.

- [ ] **Step 5: Run focused tests and commit**

Run contract, orchestration, and notification runtime tests. Commit as `feat: push live download progress cards`.

---

### Task 3: Hermes Skill Contract

**Files:**
- Modify in `hermes-home`: `shared/skills/media/SKILL.md`
- Modify in `hermes-home`: `tests/test_scaffold.py`

**Interfaces:**
- Consumes: direct `deliver_only` updates emitted by media-service.
- Preserves: explicit `hermes-media jobs get JOB_ID --json` status answers.

- [ ] **Step 1: Add a failing skill contract test**

Require the skill to state that automatic progress edits use direct delivery, consume no model tokens, update one card approximately every five seconds, and terminal notifications remain separate.

- [ ] **Step 2: Update the Jobs and diagnostics section**

Remove the claim that checkpoints appear only after explicit status requests. Keep the existing on-demand progress formatting rules for user questions.

- [ ] **Step 3: Run `./scripts/check` and commit**

Commit as `docs: explain automatic progress cards`.

---

### Task 4: Verification and Deployment

**Files:**
- Update evidence only if a real live test succeeds: `docs/evidence/2026-07-18-live-progress-card.md`

- [ ] **Step 1: Run complete local verification**

```bash
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Run `./scripts/check` in `hermes-home` and `git diff --check` in both repositories.

- [ ] **Step 2: Push both `main` branches**

Push reviewed worktree heads to `origin/main`, then fast-forward the primary local checkouts.

- [ ] **Step 3: Deploy only with an idle queue**

Confirm `active: false` and `queued: 0`, then run the existing `scripts/homelab.sh deploy` path with the updated Hermes source.

- [ ] **Step 4: Perform live verification**

Use one small explicitly selected download. Confirm one Telegram card receives at least two edits without LLM tool activity or new progress messages, then confirm a separate terminal notification. Check healthy containers and an empty pending/dead notification outbox.

- [ ] **Step 5: Clean worktrees and report**

Remove only clean temporary worktrees after primary checkouts match `origin/main`. Report exact deployed image tags and any live-test limitation.
