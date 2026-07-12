# Rezka Catalog and Playback Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend `rezka-client` from authenticated sessions to typed catalog search, title/translation discovery, series availability, and ephemeral movie/episode playback manifests with every subtitle track.

**Architecture:** Keep `RezkaClient` as the stateful facade over one exact-origin transport and cookie jar. Implement provider parsing as pure bounded modules, represent user choices with non-forgeable capability values, and keep CDN URLs in non-serializable redacted wrappers. `rezka-client` remains independent of every other workspace crate.

**Tech Stack:** Rust 1.97, Tokio, Reqwest, Scraper 0.27, Serde/Serde JSON, URL, Secrecy, Base64, Wiremock, Cargo Nextest, Mise.

## Global Constraints

- Implement only `docs/superpowers/specs/2026-07-11-rezka-catalog-playback-design.md`.
- Do not add downloads, files, PostgreSQL, API routes, jobs, `ffmpeg`, Plex, Prowlarr, or qBittorrent.
- `rezka-client` must retain zero workspace dependencies.
- Normal CI must never contact Rezka; live probes require explicit environment opt-in.
- Provider response bodies remain capped at 2 MiB.
- Stream and subtitle URLs are HTTPS-only, non-serializable, redacted in formatting/errors, and never logged.
- Use test-driven development and observe each new test fail before implementing it.
- Run `git diff --check` and focused tests before every task commit.
- Do not revert or rewrite existing Phase 1-3 behavior.

---

## File Map

```text
crates/rezka-client/src/error.rs
    Stable catalog/playback error codes and enum variants.
crates/rezka-client/src/secret_url.rs
    Ephemeral validated/redacted media, subtitle, and public image URL values.
crates/rezka-client/src/catalog.rs
    Catalog query/page/title/translation values and RezkaClient catalog facade.
crates/rezka-client/src/catalog/parser.rs
    Pure bounded search and title HTML parsing.
crates/rezka-client/src/playback.rs
    Capability selections, availability values, manifest, and facade operations.
crates/rezka-client/src/playback/parser.rs
    Strict duplicate-aware AJAX JSON parsing.
crates/rezka-client/src/quality.rs
    Stream de-obfuscation, grammar, quality normalization, and ranking.
crates/rezka-client/src/subtitles.rs
    Subtitle listing/language parsing and stable track identity.
crates/rezka-client/src/transport.rs
    Title-only accepted statuses and idempotent form failover.
crates/rezka-client/tests/{secret_url,catalog,title,series_availability,quality,subtitles,playback}.rs
    Focused public-contract and mock HTTP tests.
crates/rezka-client/tests/fixtures/{catalog,title,episodes,playback}_*
    Sanitized provider fixtures with example.invalid URLs.
```

---

### Task 1: Stable Errors and Secret URL Values

**Files:**
- Modify: `crates/rezka-client/src/error.rs`
- Create: `crates/rezka-client/src/secret_url.rs`
- Modify: `crates/rezka-client/src/lib.rs`
- Create: `crates/rezka-client/tests/secret_url.rs`
- Modify: `crates/rezka-client/tests/redaction.rs`

**Interfaces:**
- Produces `ProviderFailureReason`, new `RezkaError` variants/codes, `SecretMediaUrl`, `SecretSubtitleUrl`, and `PublicImageUrl`.
- Secret wrappers expose `with_url<R>(&self, impl FnOnce(&Url) -> R) -> R` and exact redacted formatting.

- [x] **Step 1: Write failing error-code and redaction tests**

Cover `ChallengeRequired`, `TitleNotFound`, `TranslationUnavailable`, `EpisodeUnavailable`, `QualityUnavailable`, and `StreamExpired`. Construct every variant and assert `Debug` and `Display` contain no title, URL, host, IP, token, cookie, or provider message.

- [x] **Step 2: Write failing secret URL boundary tests**

Accept signed public HTTPS URLs. Reject HTTP, credentials, fragments, localhost, `.localhost`, and non-global literal IPv4/IPv6. Assert:

```rust
assert_eq!(format!("{url:?}"), "SecretMediaUrl([REDACTED])");
assert_eq!(format!("{url}"), "[REDACTED]");
```

- [x] **Step 3: Run RED**

```bash
cargo nextest run -p rezka-client --test secret_url --test redaction
```

Expected: compile failure because the new types do not exist.

- [x] **Step 4: Implement minimal values**

Use private fields and constructors. `ProviderFailureReason::Display` emits static phrases only. Public image URLs expose a read-only URL accessor but use redacted `Debug`; stream/subtitle URLs use closure-only access and have no Serde implementation.

- [x] **Step 5: Run GREEN and regressions**

```bash
cargo nextest run -p rezka-client --test secret_url --test redaction
cargo nextest run -p rezka-client --test session_flow --test mirror_cookie_origin
git diff --check
```

- [x] **Step 6: Commit**

```bash
git add crates/rezka-client
git commit -m "feat: add rezka playback security values"
```

---

### Task 2: Catalog Values and Continuation Parser

**Files:**
- Create: `crates/rezka-client/src/catalog.rs`
- Create: `crates/rezka-client/src/catalog/parser.rs`
- Modify: `crates/rezka-client/src/lib.rs`
- Create: `crates/rezka-client/tests/catalog.rs`
- Create: `crates/rezka-client/tests/fixtures/catalog_results.html`
- Create: `crates/rezka-client/tests/fixtures/catalog_empty.html`
- Create: `crates/rezka-client/tests/fixtures/catalog_malformed.html`

**Interfaces:**
- Produces `CatalogQuery`, `TitleLocator`, `CatalogContinuation`, `CatalogEntry`, and `CatalogPage`.
- Produces pure `parse_catalog_page(html, query, selected_origin)`.

- [x] **Step 1: Write failing constructor tests**

Cover exact 200-scalar/512-byte query and 2,048-byte locator/continuation boundaries. Reject blank/control query text, locator query/fragment/backslash, encoded or decoded dot segments, non-`.html` paths, and foreign origins.

- [x] **Step 2: Write failing fixture tests**

Require the exact item selector, title, and locator. Prove 64 entries pass, 65 fail atomically, an empty valid page returns zero, and malformed containers fail.

- [x] **Step 3: Write failing continuation tests**

Accept exactly:

```text
/search/?do=search&subaction=search&q=query&page=2
/search/page/2/?do=search&subaction=search&q=query
```

Reject page 0/1, duplicate/unknown keys, query mismatch, foreign origins, dot segments, malformed page paths, and two different next links. Identical normalized links are accepted.

- [x] **Step 4: Run RED**

```bash
cargo nextest run -p rezka-client --test catalog
```

- [x] **Step 5: Implement bounded values and parser**

Keep fields private, expose slices/accessors, preserve provider order, collapse display whitespace, and return sanitized structural errors without query/title/URL content.

- [x] **Step 6: Run GREEN and commit**

```bash
cargo nextest run -p rezka-client --test catalog
git diff --check
git add crates/rezka-client
git commit -m "feat: parse rezka catalog pages"
```

---

### Task 3: Catalog HTTP and Title-Aware Status Policy

**Files:**
- Modify: `crates/rezka-client/src/transport.rs`
- Modify: `crates/rezka-client/src/session/mod.rs`
- Modify: `crates/rezka-client/src/catalog.rs`
- Modify: `crates/rezka-client/tests/catalog.rs`
- Modify: `crates/rezka-client/tests/mirror_cookie_origin.rs`

**Interfaces:**
- Adds crate-private `get_first_with_failover_accepting` with a fixed 404/410 allowlist.
- Adds `RezkaClient::search`, `search_next`, and internal bounded catalog fetch.

- [x] **Step 1: Write failing mock HTTP tests**

Assert exact initial GET path/query, one-request continuation, mirror rewriting, cookie retention, Anubis/login detection, and redacted failures.

- [x] **Step 2: Write failing status-policy tests**

Title policy receives bounded 404/410. Generic GET, probe, DLE, and Anubis still reject them. 429 and eligible 502/503/504 retain precedence.

- [x] **Step 3: Run RED**

```bash
cargo nextest run -p rezka-client --test catalog --test mirror_cookie_origin
```

- [x] **Step 4: Implement response policy and facade**

Thread an internal policy through `send_first`/`process_response`. Only title code passes `[404, 410]`; cookie capture, body cap, failover promotion, and origin checks stay unchanged. Add a narrow crate-private `RezkaClient::transport_mut()` accessor so sibling facade modules do not expose the transport or its field publicly.

- [x] **Step 5: Run GREEN and commit**

```bash
cargo nextest run -p rezka-client --test catalog --test mirror_cookie_origin --test session_flow
git diff --check
git add crates/rezka-client
git commit -m "feat: add rezka catalog transport operations"
```

---

### Task 4: Title, Translation Keys, and Capabilities

**Files:**
- Modify: `crates/rezka-client/src/catalog.rs`
- Modify: `crates/rezka-client/src/catalog/parser.rs`
- Create: `crates/rezka-client/src/playback.rs`
- Modify: `crates/rezka-client/src/lib.rs`
- Create: `crates/rezka-client/tests/title.rs`
- Create: `crates/rezka-client/tests/fixtures/title_movie.html`
- Create: `crates/rezka-client/tests/fixtures/title_series.html`
- Create: `crates/rezka-client/tests/fixtures/title_single_translation.html`
- Create: `crates/rezka-client/tests/fixtures/title_conflicting_ids.html`

**Interfaces:**
- Produces `RezkaMediaKind`, IDs, `TranslationKey`, `Translation`, `TitleDetails`, `TitlePlaybackRef`, and `SelectedTranslation`.
- Adds `RezkaClient::title` and `TitleDetails::select_translation`.

- [x] **Step 1: Write failing title-state tests**

Cover exact Anubis, `Sign In`, `Verify`, restricted block, 404, 410, malformed HTTP 200, and cross-origin redirects.

- [x] **Step 2: Write failing ID-source tests**

Exercise all six specified ID sources. Equal candidates pass; conflict, zero, overflow, or absence fails.

- [x] **Step 3: Write failing translation identity tests**

Movie translations with one ID/different flags remain distinct; an exact duplicate movie key fails; duplicate series IDs fail. Ambiguous movie default is `None`; unique defaults resolve. Test 128/129 translation boundary.

- [x] **Step 4: Write failing capability tests**

Only a key present in the same `TitleDetails` can produce `SelectedTranslation`. A foreign key returns `TranslationUnavailable`. Wrong media-kind capability methods fail before network.

- [x] **Step 5: Run RED**

```bash
cargo nextest run -p rezka-client --test title
```

- [x] **Step 6: Implement title parsing/capabilities**

Parse every known ID source before selecting. Use media-kind-specific `TranslationKey`; define capability values in `playback.rs`, keep their constructors private, and let `TitleDetails` invoke a crate-private constructor only after membership validation.

- [x] **Step 7: Run GREEN and commit**

```bash
cargo nextest run -p rezka-client --test title --test catalog
git diff --check
git add crates/rezka-client
git commit -m "feat: parse rezka titles and translations"
```

---

### Task 5: Series Availability

**Files:**
- Modify: `crates/rezka-client/src/playback.rs`
- Create: `crates/rezka-client/src/playback/parser.rs`
- Modify: `crates/rezka-client/src/lib.rs`
- Create: `crates/rezka-client/tests/series_availability.rs`
- Create: `crates/rezka-client/tests/fixtures/episodes_success.json`
- Create: `crates/rezka-client/tests/fixtures/episodes_empty.json`
- Create: `crates/rezka-client/tests/fixtures/episodes_malformed.json`

**Interfaces:**
- Produces `SeriesAvailability`, `SeasonAvailability`, `EpisodeAvailability`, `SelectedEpisode`, and `ResolvedTarget`.
- Adds `RezkaClient::series_availability`, `SeriesAvailability::select_episode`, and `SelectedEpisode::playback_request`.

- [x] **Step 1: Write failing parser tests**

Parse `success`, `seasons`, and `episodes`; sort numeric values; reject duplicates, orphan episodes, zero/overflow, missing/wrong fields. Test 256 seasons, 4,096 per season, and 16,384 total boundaries.

- [x] **Step 2: Write failing AJAX tests**

Assert exact `id`, `translator_id`, `action=get_episodes`, title referer, XHR header, cookies, and rejection of movie selections.

- [x] **Step 3: Write failing selection tests**

Absent season/episode returns `EpisodeUnavailable`; valid selection binds original title/translation and cannot be publicly forged.

- [x] **Step 4: Run RED**

```bash
cargo nextest run -p rezka-client --test series_availability
```

- [x] **Step 5: Implement and run GREEN**

```bash
cargo nextest run -p rezka-client --test series_availability --test title
git diff --check
git add crates/rezka-client
git commit -m "feat: resolve rezka series availability"
```

---

### Task 6: Stream Decoder and Quality Ranking

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/rezka-client/Cargo.toml`
- Modify: `Cargo.lock`
- Create: `crates/rezka-client/src/quality.rs`
- Modify: `crates/rezka-client/src/lib.rs`
- Create: `crates/rezka-client/tests/quality.rs`
- Create: `crates/rezka-client/tests/fixtures/stream_plain.txt`
- Create: `crates/rezka-client/tests/fixtures/stream_obfuscated.txt`

**Interfaces:**
- Produces `AdvertisedQuality`, `QualityTier`, `StreamKind`, `StreamEndpoint`, `StreamVariant`.
- Produces pure `parse_stream_variants`.

- [x] **Step 1: Add `base64.workspace = true` and write failing decoder tests**

Test plain/obfuscated equivalence, all known salts, fixed-16 fallback, 60/61 markers, invalid base64/UTF-8, one-MiB limit, trailing garbage, and panic-free malformed inputs.

- [x] **Step 2: Write failing endpoint tests**

Cover modern marked HLS, ordinary M3U8, MP4, legacy two-MP4 positional semantics, malformed/HTTP/local URLs, de-duplication, and four/five endpoint limit.

- [x] **Step 3: Write failing quality tests**

Normalize HTML premium labels. Merge by `(label, tier)`, keep standard/premium separate, and sort by vertical hint, tier, then label independent of input order.

- [x] **Step 4: Run RED**

```bash
cargo nextest run -p rezka-client --test quality
```

- [x] **Step 5: Implement strict decoder/ranking**

Use Base64 STANDARD after bounded salt removal. Parse the complete listing, validate every endpoint through `SecretMediaUrl`, then compute preferred index zero.

- [x] **Step 6: Run GREEN, audit, and commit**

```bash
cargo nextest run -p rezka-client --test quality --test secret_url
mise run audit
git diff --check
git add Cargo.toml Cargo.lock crates/rezka-client
git commit -m "feat: normalize rezka stream qualities"
```

---

### Task 7: Subtitle Tracks

**Files:**
- Create: `crates/rezka-client/src/subtitles.rs`
- Modify: `crates/rezka-client/src/lib.rs`
- Create: `crates/rezka-client/tests/subtitles.rs`

**Interfaces:**
- Produces `SubtitleTrackId`, `SubtitleLanguage`, and `SubtitleTrack`.
- Produces duplicate-aware `parse_subtitle_fields(wrapper_json: &str)` and internal parsed field values reusable by playback JSON.

- [x] **Step 1: Write failing empty-form tests**

Accept `false`, `null`, and `""` subtitle data; accept `false`, `null`, `""`, and `{}` maps. Reject other non-empty wrong types.

- [x] **Step 2: Write failing identity/alternative tests**

Preserve duplicate labels/languages as separate ordinal IDs. Preserve alternatives in order, de-duplicate exact URLs within a track, and test four/five plus 64/65 limits.

- [x] **Step 3: Write failing language tests**

Test ASCII trim, underscore-to-hyphen, lowercase, exact 1-35 byte grammar, opaque `ua`, invalid values, and ignored unmatched map key.

- [x] **Step 4: Run RED**

```bash
cargo nextest run -p rezka-client --test subtitles
```

- [x] **Step 5: Implement with duplicate-aware JSON map visitor**

Use a custom wrapper/object visitor before conversion to `serde_json::Value` so duplicate language-map keys remain observable and fail. Reject any malformed non-empty alternative atomically. Do not fetch WEBVTT.

- [x] **Step 6: Run GREEN and commit**

```bash
cargo nextest run -p rezka-client --test subtitles --test secret_url
git diff --check
git add crates/rezka-client
git commit -m "feat: parse rezka subtitle tracks"
```

---

### Task 8: Playback Manifest and Idempotent POST Failover

**Files:**
- Modify: `crates/rezka-client/src/playback.rs`
- Modify: `crates/rezka-client/src/playback/parser.rs`
- Modify: `crates/rezka-client/src/transport.rs`
- Modify: `crates/rezka-client/src/session/mod.rs`
- Create: `crates/rezka-client/tests/playback.rs`
- Create: `crates/rezka-client/tests/fixtures/playback_movie.json`
- Create: `crates/rezka-client/tests/fixtures/playback_episode.json`
- Create: `crates/rezka-client/tests/fixtures/playback_failed.json`

**Interfaces:**
- Produces non-serializable `PlaybackManifest` and `PlaybackRequest`.
- Adds `RezkaClient::resolve` and crate-private idempotent form failover.

- [x] **Step 1: Write failing form tests**

Movie form includes `id`, `translator_id`, all three `0|1` flags, and `action=get_movie`. Episode form includes IDs/numbers and `action=get_stream` with no movie flags. Both use exact AJAX path, rewritten title referer, XHR, and cookies.

- [x] **Step 2: Write failing manifest tests**

Require boolean `success` and non-empty string `url`. Parse every variant/track, preserve key/target, map allowlisted failure reasons, and prove formatting/serialization cannot leak URLs.

- [x] **Step 3: Write failing POST failover tests**

Prove bounded non-wrapping A/B/C behavior, promotion, endpoint/referer rewriting, cookie reset/isolation, terminal 429, and next-operation recovery.

- [x] **Step 4: Run RED**

```bash
cargo nextest run -p rezka-client --test playback --test mirror_cookie_origin
```

- [x] **Step 5: Implement facade and shared failover loop**

Share internal GET/idempotent-form retry logic without changing public non-idempotent POST. Parse JSON once, then delegate stream/subtitle parsing.

- [x] **Step 6: Run GREEN and commit**

```bash
cargo nextest run -p rezka-client
git diff --check
git add crates/rezka-client
git commit -m "feat: resolve rezka playback manifests"
```

---

### Task 9: Live Guardrails and Full Verification

**Files:**
- Modify: `crates/rezka-client/tests/live_probe.rs`
- Modify: `crates/media/tests/architecture.rs`
- Create: `docs/superpowers/reviews/2026-07-12-rezka-catalog-playback-verification.md`

**Interfaces:**
- Adds ignored exact-opt-in search/title/playback live tests.
- Preserves zero workspace dependencies and records reproducible evidence.

**Verification note (2026-07-12 documentation pass):** Tasks 1-8 are
implemented and their tests pass, but this task's own deliverables were not
found in the repository. `crates/rezka-client/tests/live_probe.rs` only
contains the Phase 3 session-authentication live probe; it has no
`explicit_catalog_playback_live_probe` or other search/title/playback live
guard. `crates/media/tests/architecture.rs` still only has the Phase 3
dependency-boundary tests (`rezka_client_has_no_workspace_dependencies`,
`media_runner_depends_only_on_rezka_client_workspace_crate_in_phase_3`, etc.);
no test asserts the absence of Serde on `PlaybackManifest`/secret URL types.
`docs/superpowers/reviews/2026-07-12-rezka-catalog-playback-verification.md`
does not exist. The steps below are left unticked accordingly.

**Closure note (2026-07-12 verification pass):** Steps 1-3 are now done; see
`docs/superpowers/reviews/2026-07-12-rezka-catalog-playback-verification.md`.
`explicit_catalog_playback_live_probe` and
`playback_manifest_and_secret_url_types_have_no_serde_impls` were added to
`live_probe.rs`, and `media_core_and_contract_cannot_reach_rezka_client` to
`architecture.rs`. The direct type-level serde-absence assertion lives in the
`rezka-client` test binary rather than `architecture.rs` because `media` does
not depend on `serde` directly (a crate-metadata "no serde dep" check would be
false — rezka-client legitimately uses serde); `architecture.rs` instead guards
the complementary crate boundary. Steps 4-5 stay unticked: the Docker-dependent
matrix targets were not re-run and no commit was made (per task constraints).

- [x] **Step 1: Write live negative guard first**

Without `REZKA_LIVE_PROBE=1`, explicit ignored probe fails before DNS/network. Require caller query/title/target and secret-file credentials. Output only counts, IDs, labels, kinds, language codes, and redacted values.

Done: `explicit_catalog_playback_live_probe` panics on the opt-in `expect`
before any network access (verified with `env -u REZKA_LIVE_PROBE ... --ignored
--exact`), takes caller query/title from env, reads secret-file credentials,
and asserts only structural invariants with a redacted counts/ids/kinds/
language-codes summary.

- [x] **Step 2: Extend architecture tests**

Prove no workspace dependency, no provider types in core/contract, and no Serde implementation on manifest/secret URL source definitions.

Done: no-workspace-dependency remains covered by
`rezka_client_has_no_workspace_dependencies`; the new
`media_core_and_contract_cannot_reach_rezka_client` proves provider types cannot
reach the serializable domain/contract crates; and
`playback_manifest_and_secret_url_types_have_no_serde_impls` (in `live_probe.rs`,
where serde is nameable) asserts `PlaybackManifest`, `SecretMediaUrl`, and
`SecretSubtitleUrl` implement neither `Serialize` nor `Deserialize`.

- [x] **Step 3: Run focused guardrails**

```bash
cargo nextest run -p rezka-client --test live_probe
cargo nextest run -p media --test architecture
env -u REZKA_LIVE_PROBE cargo test -p rezka-client --test live_probe -- --ignored --exact explicit_catalog_playback_live_probe
```

Expected: normal tests pass; explicit probe exits non-zero with opt-in message before network.

Done: `mise exec -- cargo nextest run -p rezka-client` reports 167 passed, 2
skipped (both `#[ignore]` live probes); `... -p media -E 'binary(architecture)'`
reports 11 passed; and the `env -u REZKA_LIVE_PROBE ... --ignored --exact
explicit_catalog_playback_live_probe` command exits non-zero, panicking at the
opt-in `expect` (`live_probe.rs:77`) before network.

- [ ] **Step 4: Run full matrix**

```bash
mise run format
mise run check
mise run lint
mise run test
mise run test-integration
mise run audit
mise run build
git diff --check
```

Partially done (left unticked): `mise run check` and `mise run lint` PASS in the
2026-07-12 verification pass (only the pre-existing `proc-macro-error2 v2.0.1`
warning); focused `rezka-client` and `media` architecture suites PASS. Not
re-run: `mise run format`, `mise run test` (full), and the Docker-dependent
`test-integration`, `audit`, and `build` targets — other agents were modifying
the workspace concurrently.

- [ ] **Step 5: Record exact evidence and commit**

```bash
git add crates/rezka-client crates/media/tests/architecture.rs docs/superpowers/reviews
git commit -m "test: verify rezka catalog and playback"
```

Partially done (left unticked): the review document
`docs/superpowers/reviews/2026-07-12-rezka-catalog-playback-verification.md` now
exists and records exact evidence, but no commit was made — committing is outside
this verification pass's task constraints, so `git log` still has no commit
matching this message.

---

### Task 10: Independent Reviews, Remediation, and PR

**Files:**
- Modify only files required by validated findings.
- Update: `docs/superpowers/reviews/2026-07-12-rezka-catalog-playback-verification.md`

**Verification note (2026-07-12 documentation pass):** `git log` shows real
fix commits consistent with review remediation (e.g. `93d55c2 fix: close
rezka catalog validation gaps`, `c9b23ec fix: harden rezka catalog
validation`, `ece882a fix: harden rezka title parsing`, `e7a5eb1 fix:
recognize exact rezka player calls`, `623af31 fix: parse rezka player
initialization ast`, `12e15d7 fix: reject non-global rezka IPv6 URLs`), but
there is no dedicated review document (unlike Phase 3's
`docs/superpowers/reviews/2026-07-11-rezka-session-phase-3-verification.md`),
so the formal four-review process cannot be confirmed. The branch was merged
to `main` locally as commit `d2a2d96` (a two-parent merge commit with no
GitHub PR reference), and `feat/rezka-catalog-playback` was never pushed to
`origin` (only `feat/rezka-session-authentication` and
`feat/postgres-api-foundation` exist there) — there was no GitHub PR or CI run
for this branch.

- [ ] **Step 1: Run four reviews**

Review exact spec/plan coverage, security/redaction/SSRF/budgets, Rust architecture/API invariants, and simplification/maintainability.

Not verified: no review document exists to confirm this happened formally.

- [ ] **Step 2: Remediate each actionable finding with a failing test**

Do not accept suggestions that weaken approved security or scope boundaries. Commit named remediation changes.

Partially evidenced by fix commits (see note above), but not tied to a
recorded review, so left unticked.

- [ ] **Step 3: Re-run the full matrix**

```bash
mise run format
mise run check
mise run lint
mise run test
mise run test-integration
mise run audit
mise run build
git diff --check
```

Not verified in this documentation pass (see Task 9 Step 4 note).

- [ ] **Step 4: Push, open PR, watch CI, and merge**

```bash
git push -u origin feat/rezka-catalog-playback
gh pr create --base main --head feat/rezka-catalog-playback --title "feat: resolve Rezka catalog and playback" --body "Implements the approved Phase 4 Rezka catalog and playback specification with typed capability selections, strict bounded parsing, ephemeral redacted playback URLs, complete subtitle discovery, and fixture/mock/live-guard verification."
gh pr checks --watch --interval 10
```

Not done: the branch merged locally (commit `d2a2d96`) rather than through a
pushed branch, GitHub PR, and CI run.

Merge only after every GitHub check and final reviewer passes. Fetch `origin/main` and verify the merge commit before starting the next vertical slice.
