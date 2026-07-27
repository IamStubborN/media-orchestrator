# Prowlarr Episode Coverage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Confirm tracked-episode availability from Prowlarr when a release explicitly contains the requested episode, including exact, multi-episode, and bounded range titles.

**Architecture:** Keep the existing provider and tracking boundaries unchanged. Add a focused release-title parser inside `media-integrations`, combine its structured coverage with optional Newznab season/episode attributes, and keep ambiguous season packs unavailable until their contents can be verified.

**Tech Stack:** Rust 1.97, quick-xml, Tokio, Wiremock.

## Global Constraints

- Release-calendar entries remain internal candidates and never notify by themselves.
- Provider errors remain `Unknown`; successful empty or non-matching feeds remain `Unavailable`.
- The probe must not create a job, submit a torrent, or invoke an LLM.
- A release must identify the requested series and explicitly cover the requested season and episode.
- Bare season packs and absolute anime numbering without a mapping remain unconfirmed.
- Existing Rezka availability and active download behavior remain unchanged.

---

### Task 1: Structured release-title coverage

**Files:**
- Create: `crates/media-integrations/src/prowlarr_episode.rs`
- Modify: `crates/media-integrations/src/lib.rs`
- Test: `crates/media-integrations/src/prowlarr_episode.rs`

**Interfaces:**
- Produces: `EpisodeCoverage::parse(title: &str) -> Vec<EpisodeCoverage>`
- Produces: `EpisodeCoverage::contains(season: u32, episode: u32) -> bool`

- [x] Add failing unit tests for `S03E05`, `3x05`, `S03E05E06`, `S03E01-06`, `S03E01-E06`, `S03E01-S03E06`, and `S3E1-6 of 8`.
- [x] Add rejection tests for an out-of-range episode, another season, a bare season pack, embedded unrelated numbers, and absolute numbering.
- [x] Run the focused parser tests and verify the new cases fail.
- [x] Implement bounded ASCII coordinate parsing without adding a regex dependency.
- [x] Run the focused parser tests and verify they pass.

### Task 2: Hybrid Newznab feed matching

**Files:**
- Modify: `crates/media-integrations/src/prowlarr.rs`
- Test: `crates/media-integrations/tests/prowlarr.rs`

**Interfaces:**
- Consumes: `EpisodeCoverage`
- Produces: a positive Prowlarr availability result only for a usable download item with matching series identity and confirmed episode coverage

- [x] Add failing Wiremock tests for a matching range, an excluded range endpoint, multi-episode notation, matching Newznab attributes, conflicting attributes, and an unrelated title.
- [x] Run the focused integration tests and verify the new cases fail.
- [x] Parse `newznab:attr` and `torznab:attr` season/episode values within each RSS item.
- [x] Cross-check structured title coverage, provider attributes, query title identity, and usable download metadata.
- [x] Run the focused integration tests and verify they pass.

### Task 3: Verification

**Files:**
- Modify only files required by formatting.

**Interfaces:**
- Proves: existing exact-coordinate, empty-result, and provider-failure behavior remains intact

- [x] Run `cargo fmt --all --check`.
- [x] Run `cargo test -p media-integrations`.
- [x] Run `cargo test --workspace`.
- [x] Inspect the final diff for unrelated changes.
- [ ] Exercise the deployed Prowlarr probe against known range and excluded-range releases before reporting completion.
