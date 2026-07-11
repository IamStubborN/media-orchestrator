# Rezka Catalog and Playback Resolution Design

**Date:** 2026-07-11
**Status:** Approved for detailed specification
**Parent design:** `2026-07-10-media-orchestrator-mvp-design.md`
**Delivery slice:** Rezka catalog, playback, quality normalization, and subtitles

## 1. Purpose

Extend the first-party Rust `rezka-client` from authenticated session handling
to safe, typed discovery and playback resolution. This slice must let a caller:

1. search the Rezka catalog;
2. load a selected title and its translations;
3. discover seasons and episodes for one translation;
4. resolve one movie or one episode into an in-memory playback manifest;
5. rank advertised qualities without treating the label as measured media data;
6. discover every subtitle track returned for the selected translation.

The output is sufficient for a later runner slice to download and probe media.
This slice does not transfer media bytes, execute `ffmpeg`, or publish to Plex.

## 2. Design Choice

Use a typed resolver pipeline:

```text
CatalogQuery
  -> CatalogPage<CatalogEntry>
  -> TitleLocator
  -> TitleDetails
  -> SelectedTranslation
  -> SeriesAvailability (series only)
  -> PlaybackRequest
  -> PlaybackManifest
```

Each transition validates provider data and returns an immutable value. Parsing,
HTTP transport, session state, and later download execution remain separate.

Rejected alternatives:

- A single large mutable `RezkaTitle` object would mix page parsing, AJAX calls,
  playback URLs, and episode state.
- A common Rezka/Prowlarr provider trait would force unlike direct-stream and
  torrent workflows into a premature abstraction. A later application port is
  owned by its consumer and maps each provider into source-neutral use cases.

## 3. Scope

### 3.1 Included

- Full catalog search result parsing.
- Opaque provider-page continuation for catalog traversal.
- Title metadata required for user selection and playback resolution.
- Movie and series classification.
- Translation parsing and stable translation identifiers.
- Per-translation season and episode discovery.
- Movie and episode playback AJAX requests.
- Plain and obfuscated stream-list decoding.
- Advertised quality-label normalization and deterministic ranking.
- HLS and direct MP4 alternative parsing.
- Subtitle discovery and language-label normalization.
- Typed public errors, strict resource budgets, redacted diagnostics, fixtures,
  mock HTTP tests, and opt-in live resolution probes.

### 3.2 Excluded

- Media-service HTTP endpoints and user search sessions.
- Five-item application pagination and its 24-hour persistence.
- PostgreSQL migrations or persistence of catalog/playback responses.
- Canonical TMDb/TVDB/IMDb mapping.
- Job creation, leases, runner loops, and notifications.
- Stream or subtitle download.
- Stream-size probing, `ffprobe`, VAAPI, naming, storage guard, or Plex.
- Prowlarr and qBittorrent.
- A browser fallback or manually imported browser cookies.

These are later vertical slices in the parent design.

## 4. Architectural Boundaries

The `rezka-client` crate remains independent and has no workspace-crate
dependencies. It adds these modules:

```text
catalog.rs             public catalog values and network operations
catalog/parser.rs      pure search-page and title-page parsers
playback.rs            public availability and resolution operations
playback/parser.rs     pure AJAX response parsers
quality.rs             stream decoding, label normalization, ranking
subtitles.rs           subtitle parsing and normalization
secret_url.rs          redacted ephemeral URL wrappers
```

Existing modules retain their ownership:

```text
session                Anubis, DLE login, cookies, session validation
transport              HTTP policy, bounds, retries, redirects, failover
mirror                 configured origins and origin rewriting
error                   stable public errors
redaction               sanitized diagnostics
```

`RezkaClient` is the stateful facade because the same transport, selected
mirror, User-Agent, and cookie jar must span all operations. Parsers are pure
functions and do not receive the client or transport.

The facade exposes one operation per protocol transition:

```rust
pub async fn search(&mut self, query: &CatalogQuery) -> Result<CatalogPage, RezkaError>;
pub async fn search_next(
    &mut self,
    continuation: &CatalogContinuation,
) -> Result<CatalogPage, RezkaError>;
pub async fn title(&mut self, locator: &TitleLocator) -> Result<TitleDetails, RezkaError>;
pub async fn series_availability(
    &mut self,
    selection: &SelectedTranslation,
) -> Result<SeriesAvailability, RezkaError>;
pub async fn resolve(
    &mut self,
    request: PlaybackRequest,
) -> Result<PlaybackManifest, RezkaError>;
```

The exact Rust signatures may use explicit lifetimes or owned values when that
improves ergonomics, but they may not weaken the validated state transitions
described below.

No provider trait is declared inside `rezka-client`. When the runner slice
needs substitution, the consumer defines the narrow async port it needs and an
adapter delegates to `RezkaClient`. This follows the dependency-inversion rule:
the consumer owns the interface, not the provider implementation.

## 5. Public Domain Values

Public values are validated on construction and expose read-only accessors.
They do not expose mutable collections.

### 5.1 Catalog values

```rust
pub struct CatalogQuery { /* normalized query */ }

pub struct CatalogPage {
    entries: Vec<CatalogEntry>,
    continuation: Option<CatalogContinuation>,
}

pub struct CatalogEntry {
    locator: TitleLocator,
    title: String,
    description: Option<String>,
    info: Option<String>,
    thumbnail: Option<PublicImageUrl>,
}

pub struct TitleLocator { /* mirror-neutral path */ }
pub struct CatalogContinuation { /* validated same-origin relative target */ }
```

`TitleLocator` stores only an absolute provider path such as
`/series/drama/123-title.html`. It never stores a mirror origin. It must:

- start with `/`;
- end in `.html`;
- contain no credentials, fragment, control character, dot segment, or
  backslash;
- contain no query string;
- fit within 2,048 bytes.

The selected mirror is applied only when a request is sent. This keeps a saved
selection usable after mirror failover or VPN rotation.

`CatalogContinuation` is an opaque same-origin relative path plus query. It is
created only by the parser from a validated provider navigation link. Callers
cannot construct arbitrary continuation targets. Its exact navigation contract
is defined in Section 7.

### 5.2 Title and translation values

```rust
pub enum RezkaMediaKind { Movie, Series }

pub struct RezkaTitleId(u64);
pub struct TranslationId(u64);

pub enum TranslationKey {
    Movie {
        id: TranslationId,
        is_camrip: bool,
        has_ads: bool,
        is_director: bool,
    },
    Series { id: TranslationId },
}

pub struct TitleDetails {
    id: RezkaTitleId,
    locator: TitleLocator,
    title: String,
    original_title: Option<String>,
    release_year: Option<u16>,
    kind: RezkaMediaKind,
    thumbnail: Option<PublicImageUrl>,
    translations: Vec<Translation>,
    default_translation: Option<TranslationKey>,
}

pub struct Translation {
    id: TranslationId,
    name: String,
    is_premium: bool,
    is_director: bool,
    is_camrip: bool,
    has_ads: bool,
}
```

Identifiers must be positive decimal integers within `u64`. Translation order
matches the provider page and is deterministic. For movies, the stable selection
identity is `TranslationKey::Movie` because the provider requires its three
flags during resolution and may reuse an ID across variants. Duplicate movie
keys are rejected, while the same ID with different flags remains two selectable
translations. For series, where the provider request has no flags, identity is
`TranslationKey::Series`; translation IDs must be unique and irrelevant flags
cannot affect equality or a persisted selection.

`default_translation` is optional. For a series, the player initialization ID
maps to the unique series key. For a movie, it maps only when exactly one movie
key has that ID; if multiple flag variants share the ID and the initialization
does not carry all three flags, the default is intentionally `None`. The parser
never marks multiple translations as default or guesses between them.

If the page has no translation list, the parser may construct exactly one
translation from the page's default player initialization only when both a
translation ID and a non-empty displayed translation name are present. Its movie
flags are false only when the page has no flag-bearing translation element and
the initialization identifies that sole translation; otherwise missing flags
are not invented.

### 5.3 Series availability

```rust
pub struct SeriesAvailability {
    selection: SelectedTranslation,
    seasons: Vec<SeasonAvailability>,
}

pub struct SeasonAvailability {
    number: u32,
    label: String,
    episodes: Vec<EpisodeAvailability>,
}

pub struct EpisodeAvailability {
    number: u32,
    label: String,
}
```

Season and episode numbers must be positive and fit the existing persistence
contract (`i32::MAX`). Results are sorted numerically. Duplicate seasons or
duplicate episodes within one season are invalid provider responses.

Provider numbering remains provider numbering. This slice does not claim that
it is the canonical Plex ordering.

### 5.4 Validated selections and playback request

```rust
pub struct SelectedTranslation {
    title: TitlePlaybackRef,
    translation: Translation,
}

pub struct SelectedEpisode {
    selection: SelectedTranslation,
    season: u32,
    episode: u32,
}

pub enum PlaybackRequest {
    Movie(SelectedTranslation),
    Episode(SelectedEpisode),
}
```

`TitlePlaybackRef` is derived from `TitleDetails` and contains the validated
title ID, locator, and media kind. `TitleDetails::select_translation(key)` returns
`SelectedTranslation` only for a `TranslationKey` present in that exact title
and carries the translation flags required by the provider wire contract.

`SelectedTranslation::movie_request()` succeeds only for a movie.
`RezkaClient::series_availability()` accepts only a selected series translation
and binds its result to that selection. `SeriesAvailability::select_episode()`
returns `SelectedEpisode` only for a season and episode present in that exact
availability snapshot. `SelectedEpisode::playback_request()` creates the series
request. Constructors for these capability values are not public.

Consequently a movie cannot accept an episode target, a series cannot accept a
movie target, and unchecked IDs from model output cannot directly construct a
playback request.

## 6. Playback Manifest and Secret URLs

```rust
pub struct PlaybackManifest {
    title: TitlePlaybackRef,
    translation: TranslationKey,
    target: ResolvedTarget,
    variants: Vec<StreamVariant>,
    preferred_variant: usize,
    subtitles: Vec<SubtitleTrack>,
}

pub enum ResolvedTarget {
    Movie,
    Episode { season: u32, episode: u32 },
}

pub struct StreamVariant {
    advertised_quality: AdvertisedQuality,
    endpoints: Vec<StreamEndpoint>,
}

pub struct AdvertisedQuality {
    label: String,
    vertical_hint: Option<u16>,
    tier: QualityTier,
}

pub enum QualityTier { Standard, Premium }
pub enum StreamKind { Hls, Mp4 }
pub struct StreamEndpoint { kind: StreamKind, url: SecretMediaUrl }

pub struct SubtitleTrack {
    id: SubtitleTrackId,
    language: Option<SubtitleLanguage>,
    label: String,
    alternatives: Vec<SecretSubtitleUrl>,
}

pub struct SubtitleTrackId {
    provider_label: String,
    ordinal: u16,
}

pub struct SubtitleLanguage(String);
```

`PlaybackManifest`, stream endpoints, and subtitle tracks are ephemeral runner
values. They deliberately do not implement `Serialize` or `Deserialize`.
Secret URL wrappers:

- store a validated HTTPS URL;
- implement exact redacted `Debug` and `Display`;
- do not implement `Clone` unless a later downloader API proves it necessary;
- reveal the URL only through a closure-based accessor;
- redact credentials, host, path, query, fragment, and literal IPs in every
  error path;
- reject credentials and fragments;
- reject localhost and non-global literal IP destinations;
- allow signed query parameters because CDN URLs commonly require them.

DNS-level private-address protection belongs to the later download transport,
which resolves the hostname before connection. Syntax validation here does not
claim to prevent DNS rebinding.

The manifest is never written to PostgreSQL, job payloads, notifications, or
normal logs. A job stores only `TitleLocator`, title ID, `TranslationKey`, and
movie/episode selection. The runner resolves a fresh manifest immediately before
download because CDN URLs may expire.

This crate guarantees redaction in its own formatting, errors, and
instrumentation. The closure accessor cannot stop arbitrary external code from
logging a revealed URL. The later official runner adapter must preserve the same
no-log rule and is tested separately; behavior of unrelated consumer code is
outside this library's enforceable boundary.

## 7. Catalog Protocol

The initial search request uses:

```text
GET /search/?do=search&subaction=search&q=<percent-encoded query>
```

The query is trimmed once, must contain visible Unicode text, and is limited to
200 Unicode scalar values and 512 UTF-8 bytes. Empty or oversized queries fail
before network access.

Search pages are parsed from:

```text
div.b-content__inline_items > div.b-content__inline_item
```

Each accepted entry requires a non-empty title and valid title locator. Optional
description, info, and thumbnail fields may be absent. Relative thumbnails are
resolved against the selected mirror; user-facing image URLs are public HTTPS
URLs and must pass the same non-local destination validation as other public
media.

The next-page selector is `.b-navigation__next`. Its closest containing anchor
must provide `href`. Zero matching links means no continuation. Multiple links
are accepted only when they normalize to exactly the same target.

The target may be relative or absolute with the exact currently selected
provider origin. It is normalized to a relative path and query and must:

- have no scheme or authority after normalization;
- use one of two accepted page encodings: path `/search/` with exactly one
  `page=<decimal integer greater than one>` query value, or path
  `/search/page/<decimal integer greater than one>/` with no `page` query key;
- contain no fragment, credentials, control character, backslash, or dot
  segment;
- contain exactly one each of `do=search`, `subaction=search`, and `q`, plus only
  the optional `page` key required by the first path form;
- have `do=search`, `subaction=search`, and a `q` value byte-for-byte equal to
  the normalized original `CatalogQuery`;
- contain no other query keys;
- fit the continuation byte budget.

The parser emits that target as `CatalogContinuation`; it does not fetch
additional pages implicitly. One client call performs one bounded HTTP request.
The later search-session application decides how to group results into
five-item pages.

An empty valid result page is successful. A page containing result containers
but no valid entries is `provider_response_invalid`, not an empty search.

## 8. Title Protocol

Title loading performs a failover-capable GET to the locator rewritten onto the
selected mirror. Before title parsing:

1. `#anubis_challenge` means `challenge_required`, even on HTTP 200;
2. a trimmed `<title>` equal to `Sign In` means `authentication_required`;
3. a trimmed `<title>` equal to `Verify` means `challenge_required` with a
   static verification reason;
4. non-empty text in `.b-player__restricted__block_message`, excluding nested
   `.b-restricted__suggest`, maps to the allowlisted `Restricted` reason without
   retaining the text;
5. HTTP 404 or 410 means `title_not_found`;
6. HTTP 200 without the required title markers is
   `provider_response_invalid`, not guessed to be not-found;
7. every cross-origin redirect is rejected.

Transport adds a crate-private operation-specific method equivalent to:

```rust
async fn get_first_with_failover_accepting(
    &mut self,
    url: Url,
    referer: Option<Url>,
    accepted_terminal_statuses: &[StatusCode],
) -> Result<TransportResponse, RezkaError>;
```

The accepted-status slice is validated against a fixed internal allowlist and
is never caller/model input. Title loading passes only 404 and 410. Response
cookies and the existing body limit still apply before the bounded response is
returned. Rate limiting and eligible 502/503/504 failover retain their existing
precedence. Generic transport, DLE, Anubis, and probe methods continue treating
404/410 as sanitized failures. Endpoint-aware title code alone maps the returned
status to `title_not_found`; it never infers a title result from error text.

The parser gathers title-ID candidates from these exact sources:

1. `#post_id[value]`;
2. `#send-video-issue[data-id]`;
3. `#user-favorites-holder[data-post_id]`;
4. `.b-userset__fav_holder[data-post_id]`;
5. the title ID argument of `initCDNSeriesEvents` or `initCDNMoviesEvents`;
6. as a final fallback only, the positive decimal prefix before the first `-`
   in the locator's `.html` filename.

Every discovered candidate must parse as the same positive `u64`. A conflict is
invalid, and at least one source must exist. The path fallback cannot override
an explicit DOM or player value. The parser then reads:

- title;
- optional original title;
- movie or series kind from structured page metadata/player initialization;
- optional release year;
- optional thumbnail;
- translations and their flags;
- optional default translation from player initialization using the unambiguous
  rules in Section 5.2.

The title path's category is only a hint and never the sole kind discriminator.
Missing optional presentation metadata is valid. Missing title ID, title, kind,
or all translations is invalid.

## 9. Series Availability Protocol

For one validated series title and translation:

```text
POST /ajax/get_cdn_series/
id=<title_id>
translator_id=<translation_id>
action=get_episodes
```

The request uses `X-Requested-With: XMLHttpRequest` and the title URL as the
referer. The JSON response must have a boolean `success` field. On success,
`seasons` and `episodes` must be strings containing bounded HTML fragments.

The parser joins season elements and episode elements by numeric season ID.
An episode referencing an unknown season is invalid. A valid translation may
return no seasons only when the response explicitly represents an empty
catalog; missing fields are not equivalent to empty.

## 10. Playback Resolution Protocol

Movie resolution uses:

```text
POST /ajax/get_cdn_series/
id=<title_id>
translator_id=<translation_id>
is_camrip=<0-or-1>
is_ads=<0-or-1>
is_director=<0-or-1>
action=get_movie
```

The three translation flags are mandatory for every movie resolution and come
from `SelectedTranslation`; callers cannot supply them independently. This
matches the provider app contract and prevents resolving a different version
that happens to share a translation identifier.

Episode resolution additionally sends:

```text
season=<provider season>
episode=<provider episode>
action=get_stream
```

These POST operations are read-only provider resolutions and are safe to retry
within the configured mirror limit. Transport adds an idempotent-form failover
operation with the same bounded, non-wrapping behavior as GET failover. On a
mirror change it:

- rewrites the endpoint and referer to the newly selected origin;
- discards the previous origin's cookie jar;
- never sends provider cookies cross-origin;
- preserves deterministic promotion for the next logical operation.

The AJAX JSON response requires `success: true` and a non-empty string `url`
stream payload. Optional `subtitle` and `subtitle_lns` fields follow Section 12
and may be `false`, `null`, or an empty value. Failure responses map to the most
specific typed error and may retain only the allowlisted semantic provider
failure reason defined in Section 14.

## 11. Stream Decoding and Quality Normalization

The stream payload supports two known forms:

1. a plain quality-tagged listing beginning with `[quality]`;
2. a `#h` base64 payload containing known or fixed-width salt fragments after
   `//_//` markers.

Decoding is strict:

- cap encoded and decoded sizes;
- cap salt-marker count;
- remove known salts exactly;
- apply the documented fixed-width fallback only when enough bytes remain;
- require valid base64 for an encoded payload;
- require valid UTF-8;
- parse the entire decoded listing without ignored trailing garbage;
- reject unknown URL schemes or malformed alternatives.

Unlike legacy clients, decoding failure never returns the undecoded payload as
if it were a usable URL.

The decoded stream grammar is a comma-separated sequence of entries:

```text
[quality-label]<alternative> or <alternative> ...
```

The parser recognizes commas only between complete quality entries. Each entry
must have one non-empty label and at least one non-empty alternative. Every
alternative must validate; one malformed non-empty alternative rejects the
whole payload instead of returning a partial manifest.

Endpoint classification is deterministic and order-sensitive:

1. if any alternative ends in `:hls:manifest.m3u8`, the entry uses the modern
   form: marked alternatives are HLS and the remaining validated alternatives
   are MP4;
2. otherwise, exactly two alternatives use the legacy positional form: the
   first is HLS and the second is MP4 even when both parsed paths end in `.mp4`;
3. otherwise, a URL whose parsed path ends in `.m3u8` is HLS and one whose path
   ends in `.mp4` is MP4;
4. any remaining unclassified alternative is invalid.

Signed queries do not affect path-suffix classification. Variant merge identity
is `(normalized display label, QualityTier)`. Repeated entries with the same
identity are merged in provider order. Standard and premium entries remain
separate even when HTML stripping yields the same display label. Repeated
identical `(kind, URL)` endpoints are de-duplicated; distinct endpoints are
preserved up to the endpoint budget. Two labels that normalize to different
display text remain separate even when their numeric hint matches.

Quality labels may contain provider HTML. Normalization parses the fragment as
HTML, extracts text, collapses whitespace, and records:

- a safe display label;
- the first plausible vertical-resolution hint (`360`, `480`, `720`, `1080`,
  `2160`, or another bounded positive value);
- premium tier when explicit premium/ultra markup or text is present.

The label is advertised metadata, not measured resolution. Names such as
`1080p`, `1080p Ultra`, or HTML-bearing premium labels remain visible for user
choice and diagnostics, but no code claims the media is Full HD.

Variants are ordered by:

1. vertical hint descending;
2. premium tier before standard at the same hint;
3. normalized label ascending as a stable tie-breaker.

The first variant is `preferred_variant`. Every variant retains all valid
alternatives. HLS is ordered before MP4 within equal quality because it is the
preferred later download path; MP4 remains an explicit fallback. The later
runner may fall back only within the selected quality, never silently downgrade
to another quality without recording that decision.

No codec, width, height, duration, bitrate, or file size is inferred here.
Those facts come only from `ffprobe` after transfer.

## 12. Subtitle Normalization

Subtitle parsing consumes the selected translation's playback response. Every
valid track is retained; there is no user selection in this slice.

The playback response represents `subtitle` as either `false`, `null`, an empty
string, or a comma-separated listing:

```text
[provider label]<URL> or <URL>,[provider label]<URL>
```

`subtitle_lns` is either `false`, `null`, the empty string `""`, an empty object,
or an object mapping provider labels to normalized language codes. Any other
non-empty JSON type is invalid. A missing map entry does not discard a track;
its language is `None`.

`SubtitleLanguage` is an opaque normalized provider code, not a claim of full
BCP 47 or ISO 639 conformance. Normalization trims ASCII whitespace, replaces
`_` with `-`, and lowercases ASCII. The result must be 1-35 ASCII bytes and
match `[a-z][a-z0-9]{0,7}(-[a-z0-9]{1,8}){0,3}`. Values such as provider-specific
`ua` remain valid opaque codes. Mapping to Plex language tags is a later
publisher concern; this slice never guesses a different language.

Each listing entry is one track and requires:

- a non-empty provider label;
- at least one valid HTTPS alternative;
- a stable `SubtitleTrackId` made from the normalized provider label and the
  entry's zero-based provider-order ordinal;
- an optional normalized language attribute derived from `subtitle_lns`.

All non-empty alternatives must validate and are retained in provider order;
exact duplicate URLs within one track are de-duplicated. Duplicate language
codes and duplicate labels are valid because one language may have full,
forced, signs, or other distinct tracks. The ordinal keeps their identities
different and lets the later publisher produce collision-free sidecar names.

An object key in `subtitle_lns` that has no matching listing label is ignored as
unused provider metadata. Duplicate JSON object keys, invalid language values,
or a non-empty malformed listing reject the response atomically.

No subtitle data is a valid empty result. Missing video streams is always an
error even if subtitle data is present. WEBVTT content validation belongs to
the later download slice.

## 13. Resource Budgets

Existing two-MiB response-body limits remain mandatory. Parsers additionally
enforce:

| Resource | Maximum |
|---|---:|
| Catalog entries per provider page | 64 |
| Catalog continuation bytes | 2,048 |
| Title locator bytes | 2,048 |
| Normalized text field bytes | 4,096 |
| Translations per title | 128 |
| Seasons per translation | 256 |
| Episodes per season | 4,096 |
| Total episodes per translation | 16,384 |
| Stream payload bytes after decoding | 1 MiB |
| Stream variants | 32 |
| Endpoints per variant | 4 |
| Subtitle tracks | 64 |
| Subtitle alternatives per track | 4 |
| Salt markers | 60 |

Crossing a budget fails the whole operation atomically with
`provider_response_invalid`. Parsers do not return a silently truncated model.

## 14. Errors

Extend the stable public code set with:

```text
title_not_found
translation_unavailable
episode_unavailable
quality_unavailable
stream_expired
```

Existing codes remain stable. Errors contain static structural context and may
contain only an allowlisted semantic diagnostic:

```rust
pub enum ProviderFailureReason {
    AuthenticationRequired,
    PremiumRequired,
    Restricted,
    TranslationUnavailable,
    EpisodeUnavailable,
    RateLimited,
    Unknown,
}
```

The parser may map an explicit provider machine field, HTTP state, or an exact
known normalized message to this enum. The allowlist lives as static code and
tests; it never copies any provider-supplied substring into the result. Unknown
or changed messages become `Unknown`. `Display` emits a static phrase for the
enum value. This refines the parent requirement to preserve useful provider
diagnostics: the semantic reason is retained, but arbitrary provider text is
not. Raw free-form provider messages never enter an error, event, or trace.

Add the missing concrete variant for the already-public code:

```rust
RezkaError::ChallengeRequired { context: SanitizedSnippet }
```

It maps to `RezkaErrorCode::ChallengeRequired`, uses a redacted `Display`, and is
emitted when a catalog/title/AJAX response is positively identified as Anubis.
The five new codes above likewise receive concrete redacted error variants,
even though `stream_expired` is not emitted until the later downloader.

Errors never contain:

- raw HTML or JSON;
- query text;
- title, continuation, stream, subtitle, or thumbnail URLs;
- hostnames or IP addresses;
- provider cookies or headers;
- stream payloads or obfuscation fragments.

`stream_expired` is defined now for contract stability but is first emitted by
the later downloader after an expired CDN response. Catalog/playback parsing
does not guess expiration from URL shape.

## 15. Session and Challenge Behavior

Catalog and playback methods use the existing authenticated `RezkaClient` and
never accept credentials. The caller remains responsible for loading a stored
session and calling `ensure_authenticated` before provider operations.

If Anubis or an authentication page appears during catalog or playback:

- do not parse it as an empty result or missing title;
- return `challenge_required` or `authentication_required`;
- let the runner re-enter the existing bounded authentication flow;
- retry the logical operation at most once after successful re-authentication.

That one-shot retry belongs to the later consumer adapter because credentials
and encrypted session persistence are runner concerns. The provider parser
does not invoke login recursively.

## 16. Testing Strategy

### 16.1 Pure fixture tests

Commit sanitized fixtures for:

- non-empty and empty catalog pages;
- valid, duplicate, cross-origin, mismatched-query, and malformed next-page
  navigation;
- movie and series title pages;
- a title with one implicit/default translation;
- duplicate and over-budget translation data;
- season/episode AJAX fragments;
- movie and episode stream JSON;
- plain and obfuscated stream listings;
- HTML-bearing premium quality labels;
- modern marked HLS/MP4 alternatives and a legacy two-`.mp4` positional pair;
- no subtitles, multiple tracks sharing one language, absent language maps, and
  `subtitle_lns: ""` plus multiple subtitle alternatives;
- malformed, duplicate, truncated, and over-budget payloads;
- Anubis/login/not-found pages that resemble valid HTTP 200 responses.

Fixtures replace real origins, signed tokens, account information, and cookies.
No live CDN URL is committed.

### 16.2 Property and boundary tests

Use deterministic table/property-style tests for:

- every constructor boundary;
- capability constructors rejecting a foreign translation or absent episode;
- quality ordering independent of provider input order;
- movie translations with one ID and different flag tuples remaining distinct;
- exact duplicate movie keys and duplicate series IDs being rejected;
- allowed duplicate subtitle languages and labels retaining distinct track IDs;
- plain/encoded decoding equivalence;
- malformed salt/base64 handling without panics;
- parser atomicity at every resource limit;
- exact redaction of every public `Debug`, `Display`, and error variant.

### 16.3 Mock HTTP integration tests

Wiremock tests prove:

- exact search/title/AJAX methods, paths, forms, referers, and headers;
- exact movie `is_camrip`, `is_ads`, and `is_director` flags copied from the
  selected translation capability;
- same-session cookie retention;
- GET and idempotent POST failover bounds;
- selected-origin rewrite and cookie isolation;
- challenge/login HTML cannot become catalog models;
- typed status and provider-failure mapping;
- title-only 404/410 acceptance without changing DLE, Anubis, or probe status
  handling;
- a manifest contains all variants and subtitle tracks but leaks no URLs in
  formatted output.

### 16.4 Opt-in live probes

Normal CI never contacts Rezka. Explicit live probes require the existing
credential and opt-in gates and may:

1. search for a caller-supplied query;
2. load a caller-supplied selected title;
3. resolve one caller-supplied movie or episode;
4. print only counts, IDs, normalized quality labels, stream kinds, and subtitle
   language keys.

They must never print or download stream/subtitle URLs.

## 17. Acceptance Criteria

This slice is complete only when:

1. Search returns typed entries and a validated optional continuation from
   sanitized fixtures and mock HTTP.
2. A selected title returns stable title/translation metadata without storing
   a mirror origin in its locator.
3. A foreign translation key and an unavailable episode cannot construct a
   playback request; valid selections use non-public capability constructors.
4. Movie variants sharing an ID but differing in flags remain distinct; an
   exact duplicate movie key and a duplicate series ID are rejected.
5. Each translation of a series can return sorted, validated season/episode
   availability bound to the selected title and translation.
6. A movie and one series episode each resolve into an in-memory manifest, and
   movie resolution sends the selected translation's three required flags.
7. Plain and known obfuscated stream payloads produce the same normalized
   variants.
8. The highest advertised quality is selected deterministically, including a
   usable premium tier, while remaining explicitly unverified.
9. Every valid subtitle for the chosen translation is present, including tracks
   sharing one language; no subtitles is successful.
10. Stream/subtitle URLs cannot appear through serialization or through
   `rezka-client` and the official adapter's `Debug`, `Display`, errors, or
   tracing instrumentation.
11. GET and idempotent AJAX failover remain bounded and exact-origin cookies do
   not cross mirrors.
12. Malformed, duplicate, and over-budget provider data fails atomically with a
    typed sanitized error.
13. Anubis encountered outside the authentication probe maps to the concrete
    redacted `ChallengeRequired` error.
14. Existing Phase 1-3 tests remain green and architecture checks still prove
    that `rezka-client` has no workspace dependencies.
15. Format, check, Clippy, unit tests, integration tests, dependency audit, and
    build all pass.

## 18. Delivery Boundary

The next detailed plan implements only this document. Its final public artifact
is a safe `PlaybackManifest` and catalog/title/availability values in
`rezka-client`.

The following slice consumes these values through a consumer-owned runner port,
adds API search sessions and persisted user choices where required, and later
performs download, `ffprobe`, VAAPI encoding, subtitle validation, and Plex
publication. No temporary implementation in this slice may bypass that boundary
by writing media files or persisting secret URLs.
