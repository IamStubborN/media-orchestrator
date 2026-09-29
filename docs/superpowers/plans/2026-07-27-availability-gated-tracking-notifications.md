# Availability-Gated Tracking Notifications Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Emit a tracked-episode Telegram card only after Rezka or Prowlarr confirms that the exact episode is downloadable.

**Architecture:** Keep release metadata and source availability as separate ports. The tracking runtime obtains aired candidates, probes both providers, records only confirmed candidates, and derives the source-choice actions from confirmed providers. Reuse the existing discovery uniqueness constraint and leave unconfirmed candidates outside `known_episodes` so the calendar reproduces them on later checks.

**Tech Stack:** Rust 1.97, Tokio, Reqwest, SeaORM/PostgreSQL, Wiremock, Python Telegram notifier tests, Docker Compose.

## Global Constraints

- Calendar release metadata alone must never create a Telegram notification.
- Provider errors are `Unknown`, not `Unavailable`.
- Availability probes must not create a download job or invoke an LLM.
- `All` is present only when both Rezka and Prowlarr confirm the exact episode.
- Automatic Rezka download tracking retains its existing behavior and 15-minute cadence.
- Manual release-calendar candidates retry after 30 minutes while unconfirmed.
- Existing active downloads must not be interrupted during deployment.

---

### Task 1: Dynamic source-choice actions

**Files:**
- Modify: `crates/media-core/src/notification.rs`
- Modify: `crates/media-integrations/src/hermes.rs`
- Modify: `crates/media-storage/src/repository/tracking.rs`
- Test: `crates/media-core/src/notification.rs`
- Test: `crates/media-contract/tests/tracking_notifications.rs`
- Test: `crates/media-integrations/tests/hermes.rs`

**Interfaces:**
- Produces: `SourceChoiceNotification::new(..., actions: Vec<SourceChoiceAction>)`
- Guarantees: accepted action sets are `[Rezka]`, `[Prowlarr]`, and `[All, Rezka, Prowlarr]`

- [ ] Add failing tests for each valid action set and empty/invalid action sets.
- [ ] Change the fixed action array to a validated vector.
- [ ] Serialize only the actions stored in the notification.
- [ ] Run focused domain, contract, integration, and storage tests.
- [ ] Commit the dynamic-action contract.

### Task 2: Exact Prowlarr episode availability

**Files:**
- Modify: `crates/media-integrations/src/prowlarr.rs`
- Test: `crates/media-integrations/tests/prowlarr.rs`

**Interfaces:**
- Produces: `ProwlarrClient::episode_availability(query: EpisodeQuery) -> Result<bool, ProwlarrError>`
- Consumes: enabled indexer inventory and per-indexer Newznab TV search with `season` and `ep`

- [ ] Add Wiremock tests for available, unavailable, partially unavailable, and fully unavailable indexers.
- [ ] Add bounded XML response parsing for usable torrent items.
- [ ] Query distinct title variants and stop after the first usable result.
- [ ] Preserve typed transport, authorization, provider-response, and body-limit errors.
- [ ] Run focused Prowlarr tests.
- [ ] Commit exact episode probing.

### Task 3: Availability-gated tracking runtime

**Files:**
- Modify: `crates/media-core/src/tracking.rs`
- Modify: `crates/media/src/search.rs`
- Modify: `crates/media/src/composition.rs`
- Test: `crates/media-core/src/tracking.rs`
- Test: `crates/media/tests/search_flow.rs`
- Test: `crates/media/tests/tracking_download.rs`

**Interfaces:**
- Produces: `EpisodeAvailabilityPort`
- Produces: `EpisodeAvailabilityRequest`, `EpisodeAvailability`, and `ProviderAvailability`
- Consumes: matched release title/original title and all aired episode candidates

- [ ] Add failing runtime tests proving calendar-only candidates stay silent.
- [ ] Add tests for Rezka-only, Prowlarr-only, dual, unknown, and episode-gap behavior.
- [ ] Return release identity metadata with calendar candidates.
- [ ] Probe Rezka and Prowlarr concurrently.
- [ ] Record only candidates with at least one available provider.
- [ ] Use complete known-episode membership instead of a maximum baseline.
- [ ] Apply the 30-minute pending and 6-hour idle cadence.
- [ ] Keep automatic Rezka download tracking unchanged.
- [ ] Run focused tracking tests.
- [ ] Commit the availability gate.

### Task 4: Persistence and pending-row repair

**Files:**
- Modify: `crates/media-core/src/tracking.rs`
- Modify: `crates/media-storage/src/repository/tracking.rs`
- Create: `crates/media-storage/src/migration/m20260727_000029_availability_gated_tracking.rs`
- Modify: `crates/media-storage/src/migration/mod.rs`
- Test: `crates/media-storage/src/repository/tracking.rs`
- Test: `crates/media-storage/tests/migrations.rs`

**Interfaces:**
- Consumes: confirmed provider action set in `record_future_episode`
- Produces: atomic discovery, known-episode update, and notification payload

- [ ] Add failing persistence tests for dynamic actions and deduplication.
- [ ] Pass availability actions into `record_future_episode`.
- [ ] Add a migration that suppresses undelivered unverified cards and reopens their episodes for probing.
- [ ] Prove delivered historical rows remain unchanged.
- [ ] Run storage and migration tests.
- [ ] Commit persistence changes.

### Task 5: Hermes rendering compatibility

**Files:**
- Modify: `/Users/operator/Projects/personal/hermes-home/scripts/media_notifier.py`
- Modify: `/Users/operator/Projects/personal/hermes-home/tests/test_media_notifier.py`
- Modify: `/Users/operator/Projects/personal/hermes-home/tests/test_media_telegram_plugin.py`

**Interfaces:**
- Consumes: `media.source-choice` payload action list
- Produces: one Telegram inline row containing only requested actions

- [ ] Add tests for Rezka-only and Prowlarr-only cards.
- [ ] Remove assumptions that every card has all three actions.
- [ ] Preserve authorization, scope propagation, and existing three-button cards.
- [ ] Run the full Hermes test suite.
- [ ] Commit Hermes compatibility.

### Task 6: Verification and deployment

**Files:**
- Modify only deployment artifacts generated by the existing release workflow.

**Interfaces:**
- Produces: deployed `media-service`, notifier, and Telegram plugin behavior

- [ ] Run `cargo fmt --check`.
- [ ] Run focused Rust tests, then `cargo test --workspace`.
- [ ] Run Hermes Python and shell tests.
- [ ] Build release artifacts and Docker images using the repository scripts.
- [ ] Verify no active media job would be interrupted.
- [ ] Deploy media-service and notifier changes without restarting the download runner or VPN.
- [ ] Verify container health and Prowlarr exact episode probing.
- [ ] Run a silent unavailable-candidate QA and inspect the outbox.
- [ ] Run single-provider and dual-provider synthetic QA and verify inline buttons.
- [ ] Remove synthetic QA data and confirm no download job was created.
- [ ] Commit deployment metadata, push `main`, and report live evidence.
