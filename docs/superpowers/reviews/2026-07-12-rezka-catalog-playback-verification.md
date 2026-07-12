# Rezka Catalog and Playback Phase 4 Verification

Date: 2026-07-12

## Scope

This artifact records verification of the Phase 4 Rezka catalog and playback
implementation (plan `docs/superpowers/plans/2026-07-12-rezka-catalog-playback.md`,
Tasks 1-8) and closes the Task 9 deliverables that the 2026-07-12 documentation
pass found missing:

- catalog/title/playback coverage in the opt-in live probe
  (`crates/rezka-client/tests/live_probe.rs`),
- an architecture assertion that domain playback types and the secret URL
  wrappers implement no Serde, and
- this verification review.

It supplements, and does not rewrite, the plan's dated verification notes. Every
pass count below is transcribed from a real run of the command shown; the two
Docker-dependent matrix targets (`test-integration`, `audit`, `build`) were not
re-run in this pass, and the live probe was not executed against the real
service because the opt-in environment was intentionally absent. Both omissions
are listed explicitly under "Not covered".

## Environment

- Toolchain invoked through `mise exec -- cargo ...` (the workspace pins a
  Rust version that the ambient `rustc 1.95.0-nightly` does not satisfy; direct
  `cargo` is rejected, `mise` selects the pinned toolchain).
- Other agents were building the same workspace concurrently; only the four
  files owned by this task were touched: `crates/rezka-client/tests/live_probe.rs`,
  `crates/media/tests/architecture.rs`, this review, and the plan's checkboxes.

## Deliverables added

### 1. Catalog/title/playback live probe

`crates/rezka-client/tests/live_probe.rs` previously contained only the Phase 3
session-authentication probe. Added `explicit_catalog_playback_live_probe`, an
`#[ignore]` + `#[tokio::test]` that:

- panics on the first line via
  `std::env::var("REZKA_LIVE_PROBE").expect(...)` when the opt-in is absent, so
  it fails before DNS or any network access;
- requires caller-supplied query (`REZKA_LIVE_SEARCH_QUERY`) and title
  (`REZKA_LIVE_TITLE_LOCATOR`), constructed through the same bounded
  `CatalogQuery`/`TitleLocator` value constructors normal callers use, plus
  secret-file credentials (`REZKA_LIVE_USERNAME_FILE`/`REZKA_LIVE_PASSWORD_FILE`)
  read by the existing bounded `read_live_secret` helper;
- authenticates, then exercises catalog search, title translation discovery,
  series season/episode availability (or movie selection, chosen by
  `RezkaMediaKind`), and playback manifest resolution against the live mirror;
- asserts only structural invariants — non-empty entries/translations/seasons/
  episodes/variants, absolute `.html` locators, strictly ascending season and
  episode numbers, in-bounds preferred variant index, at least one endpoint per
  variant, and `https` scheme on every stream and subtitle URL (read through the
  closure-only `with_url` accessor) — and never asserts specific title, URL, or
  provider content;
- emits a single redacted summary line of counts, the numeric title id, the
  media kind, the resolved target, and subtitle language codes only.

### 2. Serde-absence assertion for domain playback types

The plan (Task 9 Step 2) asks for an assertion that `PlaybackManifest`,
`SecretMediaUrl`, and `SecretSubtitleUrl` do not implement `serde::Serialize` or
`serde::Deserialize`. This was implemented as a direct, runtime-observable
type-level check, `playback_manifest_and_secret_url_types_have_no_serde_impls`,
in `crates/rezka-client/tests/live_probe.rs`, plus a complementary
crate-boundary guard, `media_core_and_contract_cannot_reach_rezka_client`, in
`crates/media/tests/architecture.rs`.

#### How the serde-absence check works, and why it is where it is

The honest test of "this type has no `Serialize`/`Deserialize` impl" is a
type-level property, not a crate-dependency property. It is implemented with the
inherent-vs-trait method-resolution trick (the mechanism behind the `impls`
crate):

```rust
struct Probe<T>(PhantomData<T>);
trait NotSerialize { fn is_serialize(&self) -> bool { false } }   // always present
impl<T> NotSerialize for Probe<T> {}
impl<T: serde::Serialize> Probe<T> { fn is_serialize(&self) -> bool { true } } // only when T: Serialize
```

`Probe::<T>(PhantomData).is_serialize()` resolves to the inherent method (which
returns `true`) only when `T: Serialize` is satisfied; otherwise the
always-present trait fallback (returning `false`) is selected. The same shape
with a `serde::de::DeserializeOwned` bound covers the deserialize direction.
The result is a plain `bool` asserted at run time, needs no value of `T`, and a
`String` sanity anchor confirms the detector reports `true` for a type that
genuinely implements Serde (so the negatives are not vacuous).

This trick must name `serde::Serialize`/`serde::de::DeserializeOwned`, which is
only possible in a crate that lists `serde` as a direct dependency. `serde` is a
direct dependency of `rezka-client`, so the assertion lives in that crate's
integration-test binary. It cannot live in `crates/media/tests/architecture.rs`:
`media` does not depend on `serde` directly (only `serde_json`), which was
verified empirically — `fn _f<T: serde::Serialize>() {}` in that file fails to
compile with `E0433: cannot find module or crate serde`. Adding `serde` to
`media`'s manifest was outside this task's permitted file set.

A cargo-metadata check of the form "rezka-client does not depend on serde" would
be **false and dishonest**: rezka-client legitimately uses `serde` for its
custom JSON deserializers (`playback/parser.rs`, `subtitles.rs`) and for
session-cookie persistence (`session/cookie.rs`, which even derives
`Serialize`/`Deserialize` on cookie envelopes). Crate-level metadata cannot
distinguish "a domain type implements Serialize" from "the crate uses serde
internally". Therefore the metadata infrastructure in `architecture.rs` was used
for the invariant it *can* express honestly: `media-core` and `media-contract`
— the serializable domain and wire-contract crates — cannot reach `rezka-client`
in the resolved dependency graph, so no provider type (Serde or not) can ever be
embedded in a serialized domain or wire payload. The two checks are
complementary: the type-level trick proves the specific types carry no Serde
impl; the boundary check proves provider types never enter the layer where Serde
is derived.

## Exit-gate verification

Each Phase 4 exit-gate claim below is verified against the actual test files in
`crates/rezka-client/tests/`. Counts are from
`mise exec -- cargo nextest run -p rezka-client -E 'binary(<name>)'`.

### Movies

`playback.rs` (6 passed):

- `parser_builds_complete_redacted_movie_and_episode_manifests` — parses the
  movie fixture, asserts `target() == ResolvedTarget::Movie`, two stream
  variants, two subtitle tracks, and that Debug leaks no CDN/subtitle URL.
- `resolve_sends_exact_movie_and_episode_forms_with_ajax_headers` — asserts the
  exact `action=get_movie` form body, rewritten title referer, and XHR header
  for a movie request.

### Multi-season series

`series_availability.rs` (8 passed) and `playback.rs`:

- `parser_sorts_and_binds_all_seasons_and_episodes` — parses two seasons,
  asserts numeric sort of seasons (1, 2) and episodes (1, 2, 3 across seasons),
  and that a selected episode binds the original title/translation.
- `parser_enforces_series_resource_budgets_atomically` — 256-season /
  4,096-per-season / 16,384-total boundaries.
- `parser_accepts_explicit_empty_but_rejects_missing_wrong_or_orphan_fields`,
  `parser_rejects_duplicate_zero_overflow_and_duplicate_json_fields`.
- `series_availability_sends_exact_ajax_multimap_referer_xhr_and_cookie`,
  `series_availability_fails_over_to_the_next_mirror`,
  `movie_selection_is_rejected_without_network`,
  `absent_episode_is_typed_and_snapshot_bound`.
- `playback.rs::parser_builds_complete_redacted_movie_and_episode_manifests`
  asserts `ResolvedTarget::Episode { season: 1, episode: 2 }`.

### Translations

`title.rs` (26 passed), representative:

- `all_six_title_id_sources_are_accepted`, `equal_title_id_candidates_pass_but_conflicts_fail`,
  `invalid_or_absent_title_ids_fail_atomically`.
- `movie_fixture_preserves_flag_specific_identity_and_ambiguous_default`,
  `movie_default_resolves_only_for_one_flag_variant`,
  `series_fixture_has_series_keys_and_unique_default`,
  `duplicate_translation_identity_is_rejected_by_media_kind`,
  `translation_budget_accepts_128_and_rejects_129`.
- `selection_validates_membership_and_media_kind`,
  `title_capability_debug_matrix_is_exact_and_redacted`.

### Malformed provider responses

Rejection is covered across the parser suites:

- `playback.rs::parser_requires_strict_success_and_non_empty_stream_payload` —
  six malformed manifest bodies (missing/duplicate `success`/`url`, empty url).
- `catalog.rs::result_containers_without_valid_entries_fail`,
  `continuation_rejects_unsafe_or_noncanonical_targets`,
  `continuation_rejects_malformed_percent_encodings`.
- `title.rs::malformed_javascript_is_handled_without_panicking_or_forging_initialization`.
- `series_availability.rs::parser_accepts_explicit_empty_but_rejects_missing_wrong_or_orphan_fields`
  and `..._rejects_duplicate_zero_overflow_and_duplicate_json_fields`.
- `subtitles.rs::malformed_alternatives_languages_and_duplicate_json_keys_fail_atomically`.
- `quality.rs::multibyte_salt_boundary_is_rejected_without_panicking`,
  `decoder_enforces_marker_and_decoded_size_budgets`.

### Highest-quality selection

`quality.rs` (7 passed) and `playback.rs`:

- `quality_normalization_merges_identity_and_ranks_highest_first` — normalizes
  premium labels, merges by `(label, tier)`, and ranks highest first.
- `parser_enforces_variant_budget_and_redacts_debug`,
  `endpoint_classification_is_strict_ordered_and_duplicate_aware`,
  `insecure_variant_is_skipped_while_valid_variants_survive`,
  `plain_and_known_or_fixed_salt_obfuscation_are_equivalent`.
- `playback.rs::parser_builds_complete_redacted_movie_and_episode_manifests`
  asserts `preferred_variant_index() == 0`, i.e. the highest-ranked variant.

### Subtitle tracks

`subtitles.rs` (6 passed):

- `duplicate_labels_and_languages_keep_distinct_ordinal_identity`,
  `subtitle_track_and_alternative_budgets_are_exact`,
  `empty_provider_forms_produce_no_tracks`,
  `track_with_an_insecure_alternative_is_skipped_while_valid_tracks_survive`,
  `debug_is_redacted_and_values_are_read_only`.

### Redaction and secret URL wrappers (supporting security gate)

- `secret_url.rs` (7 passed): global/redacted IPv4+IPv6 acceptance, non-public
  rejection, exact `[REDACTED]` Debug/Display for media and subtitle URLs.
- `redaction.rs` (6 passed): error Display/Debug never leak provider material;
  `constructed_security_types_never_leak_debug_material`.

## Gates run in this pass

- `mise run check` (`cargo check --workspace --all-targets --all-features
  --locked`): PASS. Only the pre-existing `proc-macro-error2 v2.0.1`
  future-incompatibility warning.
- `mise run lint` (`cargo clippy --workspace --all-targets --all-features
  --locked -- -D warnings`): PASS. Same pre-existing warning; no clippy errors.
- `mise exec -- cargo nextest run -p rezka-client`: 167 passed, 2 skipped. The
  two skipped are the `#[ignore]` live probes — the Phase 3
  `live_probe_rezka_session_authentication_contract` and the new
  `explicit_catalog_playback_live_probe`.
- `mise exec -- cargo nextest run -p media -E 'binary(architecture)'`: 11 passed,
  0 skipped, including the new `media_core_and_contract_cannot_reach_rezka_client`.
- Live probe negative guard:
  `env -u REZKA_LIVE_PROBE cargo test -p rezka-client --test live_probe --
  --ignored --exact explicit_catalog_playback_live_probe` exits non-zero,
  panicking at `live_probe.rs:77` on the opt-in `expect` before any network
  access, as intended.

## Not covered

- The live probe was **not** executed against the real Rezka service: the opt-in
  environment (`REZKA_LIVE_PROBE=1`, mirror, credentials, query/title) was
  intentionally absent. Only compilation and default-skip/negative-guard behavior
  were verified.
- `mise run test-integration`, `mise run audit`, and `mise run build` (the
  Docker-dependent matrix targets) were not re-run in this pass; other agents
  were modifying the same workspace concurrently.
- The direct type-level serde-absence assertion lives in the `rezka-client`
  test binary, not in `crates/media/tests/architecture.rs`, for the toolchain
  reason documented above; `architecture.rs` carries only the complementary
  crate-boundary guard.
- Task 10 remains open: there is still no pushed `feat/rezka-catalog-playback`
  branch, GitHub PR, or CI run, and no recorded four-review cycle for this
  vertical slice. This verification does not create any of those.
