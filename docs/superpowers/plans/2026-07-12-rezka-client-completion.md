# Rezka Client Completion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete configuration, discovery, HLS fallback, and authenticated browser integration for the Rezka media workflow.

**Architecture:** Keep `rezka-client` as the independent bounded provider client, `media-runner` as the download/process owner, and `media` as the composition root. Direct environment secrets and `_FILE` sources converge in configuration, while HLS is ingested by FFmpeg before the existing VAAPI pipeline.

**Tech Stack:** Rust 1.97, Tokio, Reqwest, Scraper, Axum, FFmpeg/VAAPI, Docker Compose, Hermes, agent-browser, Vaultwarden.

## Global Constraints

- `_FILE` values take precedence over direct environment secrets.
- Provider responses, URLs, counts, and text remain bounded and redacted.
- MP4 remains preferred; HLS is fallback only.
- Credentials never appear in command arguments, logs, Telegram, or model context.

---

### Task 1: Single-host `.env` configuration

**Files:** `compose.yaml`, `.env.example`, `.gitignore`, `crates/media/src/config.rs`, `crates/media/tests/config.rs`, `README.md`

**Interfaces:** Consumes existing `ConfigSource`; produces direct secret variables with `_FILE` precedence.

- [ ] Add failing tests for direct values and `_FILE` precedence.
- [ ] Implement bounded direct-secret reads without adding secret values to error output.
- [ ] Replace example secret mounts in Compose with `.env` variables and retain optional file variables.
- [ ] Document `.env` permissions and migrate the deployed host.
- [ ] Run `mise run test` and configuration tests.

### Task 2: Rezka discovery and account capabilities

**Files:** `crates/rezka-client/src/catalog.rs`, `crates/rezka-client/src/catalog/parser.rs`, `crates/rezka-client/src/session/mod.rs`, new focused modules under `crates/rezka-client/src/`, and tests/fixtures under `crates/rezka-client/tests/`.

**Interfaces:** Produces bounded quick-search, premium status, metadata, filter, franchise, trailer, and size APIs on `RezkaClient`.

- [ ] Add fixtures and failing parser/transport tests for every operation.
- [ ] Implement quick search and premium status first.
- [ ] Implement stream-size probing and detailed metadata.
- [ ] Implement catalog filters, franchise navigation, and trailer lookup.
- [ ] Run `mise exec -- cargo nextest run -p rezka-client` and Clippy.

### Task 3: HLS fallback in the runner

**Files:** `crates/media/src/runner.rs`, `crates/media-runner/src/media.rs`, `crates/media-runner/src/pipeline.rs`, `crates/media-runner/src/ports.rs`, and focused runner tests.

**Interfaces:** Consumes `StreamKind::Hls`; produces an FFmpeg HLS ingest command and a local staging source for the existing pipeline.

- [ ] Add failing tests proving MP4 preference and HLS fallback selection.
- [ ] Add an HLS ingest process command with no credential-bearing URL in Debug output.
- [ ] Route HLS through ProcessPort with cancellation before normal probe/transcode.
- [ ] Verify MP4 resume behavior remains unchanged.
- [ ] Run runner and composition tests.

### Task 4: Deployment and authentication E2E

**Files:** deployed `.env`, `hermes-home` Vaultwarden browser integration, and homelab Compose state.

**Interfaces:** Produces a persisted authenticated Rezka browser session after one Telegram approval.

- [ ] Deploy updated images and `.env` with mode `0600`.
- [ ] Start a fresh Telegram login request and approve it once.
- [ ] Verify credential consumption and redacted audit output.
- [ ] Restart the browser session and prove the anonymous login control is absent.
- [ ] Run repository checks, verify container health, commit, and push.
