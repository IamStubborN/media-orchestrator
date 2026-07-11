# Rezka Session Authentication Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the Phase 3 Rezka session foundation: independent transport, mirror handling, selected-origin-guarded cookies, Anubis proof-of-work, DLE login, internal cookie import/export, encrypted runner session persistence, and composition-time secret configuration.

**Architecture:** `rezka-client` is an independent provider-protocol crate with no dependency on any `media-*` crate. `media-runner` owns encrypted filesystem persistence for exported Rezka session state and does not connect to PostgreSQL. `media` remains the composition root that reads Docker-secret files, validates typed configuration, and constructs Rezka/session-store dependencies without adding catalog, playback, download, or job execution behavior.

**Tech Stack:** Rust 1.97.0, Cargo edition 2024, Reqwest 0.13.4 with rustls/form/compression, Tokio 1.52.3, cookie_store 0.22.0, scraper 0.24.0, SHA-2 0.11.0, AES-GCM 0.10.3, rand 0.8.6, secrecy 0.10.3, zeroize 1.9.0, base64 0.22.1, wiremock 0.6.5, tempfile 3.23.0, thiserror 2.0.18, tracing 0.1.44.

## Post-Review Amendment (2026-07-11)

This amendment is authoritative where it conflicts with the original task text below. It records
review remediation applied before Phase 3 deployment without rewriting the historical TDD steps or
their original evidence.

1. The exact `scraper` pin is `=0.27.0`, replacing `=0.24.0`. Its `selectors 0.38.0`
   dependency uses `rustc-hash` instead of the unmaintained `fxhash`. `tracing` is not a direct
   dependency of `rezka-client`, and `zeroize` is not a direct dependency of `media-runner`, because
   neither crate uses those direct APIs. MPL-2.0 is allowed for unmodified transitive Servo HTML/CSS
   parser crates; `RUSTSEC-2025-0057` is not ignored.
2. `SessionSnapshot` contains an exact scheme/host/effective-port origin binding around the internal
   `cookie_store` JSON. Restore rotates the matching configured origin to index zero or fails closed.
   Successful failover similarly promotes the selected origin by deterministic rotation. `select_next`
   remains non-wrapping, and every cross-origin failover replaces the jar with a new empty jar bound to
   the new origin.
3. Provider 2xx/3xx bodies are bounded to 2 MiB with Content-Length prechecks and checked chunk reads.
   Set-Cookie processing is atomic and bounded to 64 headers, 8 KiB per header, 64 accepted cookies,
   and a 128 KiB serialized opaque snapshot. Candidate stores are committed only after all limits pass.
   `media-runner` rejects snapshot plaintext above 128 KiB before AES allocation and retains the final
   256 KiB serialized-envelope check.
4. Async Anubis work uses one process-wide Tokio semaphore permit. The blocking solver receives a
   cooperative cancellation token, owns the permit until it exits, and is cancelled by an RAII guard
   when `ensure_authenticated` is dropped. Cancellation, semaphore closure, and join failure map to
   sanitized `ChallengeFailed`. The public synchronous solver remains deterministic.
5. DLE credentials may be submitted only over HTTPS or HTTP whose URL host is an exact IP loopback
   address in `127.0.0.0/8` or `::1` for local protocol tests. Production `RunnerConfig` and the
   credentialed live probe require HTTPS mirrors and probe URLs. Remote plaintext HTTP is rejected with
   a static configuration error before form construction or network submission.
6. `ProcessConfigSource` performs bounded regular-file reads. Unix opens use
   `O_NOFOLLOW | O_NONBLOCK`; non-Unix uses a documented best-effort pre-check. Database URLs, service
   tokens, Rezka credentials, and the cookie key all have pre-allocation bounds while preserving one
   accepted final line ending.
7. Valid UTF-8 provider bodies reuse their original allocation; lossy decoding occurs only on invalid
   UTF-8. Session orchestration uses one optional Anubis parse per body while the public detection and
   parsing APIs retain their original semantics.
8. `EncryptedRezkaSessionStore` Debug continues to print only the configured path parent, as required
   by the original plan, while hiding the key, file name, and plaintext size.

## Global Constraints

- Follow `docs/superpowers/specs/2026-07-10-media-orchestrator-mvp-design.md`, especially sections 6, 11, 20, 22, and 23.
- Follow `docs/ARCHITECTURE.md`; normal Cargo dependencies MUST remain acyclic.
- Keep Phase 3 scope limited to `rezka-client` transport/session/Anubis/DLE/mirrors/internal cookie import-export plus `media-runner` encrypted cookie store and `media` composition secret configuration.
- Do NOT add Rezka catalog search, title metadata parsing, translation parsing, season/episode parsing, playback stream resolution, subtitle discovery, media downloads, runner job loops, or PostgreSQL schema changes.
- `rezka-client` MUST NOT depend on `media-core`, `media-contract`, `media-api`, `media-storage`, `media-runner`, or `media`.
- `media-runner` MUST NOT depend on `media-storage`, `media-api`, SeaORM, PostgreSQL clients, or Docker libraries.
- Library crates use typed `thiserror` errors; only process entry points may use broader terminal context.
- Rezka account credentials, cookie snapshots, encrypted-store keys, signed provider URLs, raw provider bodies, and request cookies MUST NOT appear in `Display`, `Debug`, tracing fields, fixtures, snapshots, PostgreSQL, job payloads, or CLI arguments.
- Do NOT forward Rezka site cookies to CDN or any other cross-origin host. Before generating or attaching a Cookie header, `Transport` MUST verify that request scheme, host, and effective port exactly equal the selected Rezka origin; `cookie_store` domain/path matching alone is insufficient.
- Do NOT claim or bake in a proven generic Rezka session-validation endpoint. Validation is a caller-supplied harmless probe contract covered by fixtures and an ignored opt-in live probe.
- Internal cookie import/export exists only for encrypted runner persistence. Do NOT add a browser/manual copied-cookie fallback or a CLI that accepts raw cookies.
- Every behavior change follows RED-GREEN-REFACTOR and every task ends with focused tests plus `mise run check`, `mise run lint`, and `mise run test`.
- Opt-in live Rezka probes MUST be ignored by default and MUST NOT run in normal CI.

---

## Public Runtime Contract for This Phase

No new public HTTP route is added in this phase.

No new catalog, playback, or download CLI command is added in this phase.

The implementation produces these internal Rust contracts:

```rust
// rezka-client
pub struct RezkaClient;
pub struct RezkaClientConfig;
pub struct RezkaCredentials;
pub struct SessionSnapshot;
pub struct SessionValidationProbe;
pub struct ProbeResponse;
pub enum SessionValidation;
pub enum RezkaError;

// media-runner
pub struct EncryptedRezkaSessionStore;
pub struct RezkaSessionStoreConfig;
pub enum RezkaSessionStoreError;

// media composition
pub struct RunnerConfig;
pub struct RezkaCompositionConfig;
```

## Dependency Pin Recommendations

Add or move these exact pins into `[workspace.dependencies]` in `Cargo.toml`.
The versions listed below were chosen to stay below Rust 1.97.0 and align with
the current workspace pins.

```toml
aes-gcm = "=0.10.3"
base64 = "=0.22.1"
cookie_store = { version = "=0.22.0", default-features = false, features = ["public_suffix", "serde_json"] }
hex = "=0.4.3"
rand = "=0.8.6"
scraper = { version = "=0.24.0", default-features = false }
tempfile = "=3.23.0"
tracing = "=0.1.44"
url = "=2.5.8"
wiremock = "=0.6.5"
zeroize = "=1.9.0"
```

Keep the existing workspace pins for:

```toml
reqwest = { version = "=0.13.4", default-features = false, features = ["json", "rustls"] }
secrecy = "=0.10.3"
serde = { version = "=1.0.228", features = ["derive"] }
serde_json = "=1.0.150"
sha2 = "=0.11.0"
thiserror = "=2.0.18"
time = "=0.3.53"
tokio = "=1.52.3"
```

`crates/rezka-client/Cargo.toml` should request the additional Reqwest features
it needs without changing the shared version:

```toml
reqwest = { workspace = true, features = ["brotli", "deflate", "form", "gzip"] }
```

Feature verification commands (run before editing manifests):

```bash
cargo info aes-gcm@0.10.3
cargo info cookie_store@0.22.0
cargo info reqwest@0.13.4
```

Expected: all three versions support Rust 1.97.0; `cookie_store` lists
`serde_json`; Reqwest lists `brotli`, `deflate`, `form`, `gzip`, and `rustls`.
Keep `aes-gcm` featureless in this plan even if registry metadata exposes an
optional transitive zeroization feature: the store key remains in
`secrecy::SecretBox<[u8; 32]>`, and the separately pinned `zeroize` crate is
used only for owned temporary plaintext buffers that are actually mutable.

## File Map

All paths are relative to the repository root.

```text
Cargo.toml
Cargo.lock
crates/media/Cargo.toml
crates/media/src/composition.rs
crates/media/src/config.rs
crates/media/tests/architecture.rs
crates/media/tests/config.rs
crates/media/tests/rezka_composition.rs
crates/media-runner/Cargo.toml
crates/media-runner/src/lib.rs
crates/media-runner/src/rezka_session_store.rs
crates/media-runner/tests/rezka_session_store.rs
crates/rezka-client/Cargo.toml
crates/rezka-client/src/error.rs
crates/rezka-client/src/lib.rs
crates/rezka-client/src/mirror.rs
crates/rezka-client/src/redaction.rs
crates/rezka-client/src/session/anubis.rs
crates/rezka-client/src/session/cookie.rs
crates/rezka-client/src/session/dle.rs
crates/rezka-client/src/session/mod.rs
crates/rezka-client/src/session/validation.rs
crates/rezka-client/src/transport.rs
crates/rezka-client/tests/anubis.rs
crates/rezka-client/tests/fixtures/anubis_challenge.html
crates/rezka-client/tests/fixtures/anubis_malformed.html
crates/rezka-client/tests/fixtures/dle_login_failed.json
crates/rezka-client/tests/fixtures/dle_login_success.json
crates/rezka-client/tests/live_probe.rs
crates/rezka-client/tests/mirror_cookie_origin.rs
crates/rezka-client/tests/redaction.rs
crates/rezka-client/tests/session_flow.rs
crates/rezka-client/tests/support/mod.rs
```

Responsibilities:

```text
rezka-client/error.rs              public typed provider errors and stable codes
rezka-client/redaction.rs          URL/body/header/message sanitization helpers
rezka-client/mirror.rs             mirror validation and same-path/query origin rewrite
rezka-client/transport.rs          no-auto-redirect Reqwest wrapper, explicit bounded same-origin redirects, stable User-Agent, selected-origin cookie guard
rezka-client/session/cookie.rs     domain/path cookie jar plus opaque internal snapshot import/export, including session cookies
rezka-client/session/anubis.rs     challenge detection, parsing, bounded SHA-256 proof-of-work, pass submission
rezka-client/session/dle.rs        DLE login form request and response classification
rezka-client/session/validation.rs caller-supplied harmless probe contract
rezka-client/session/mod.rs        session orchestration over transport, Anubis, DLE, validation, and export/import
media-runner/rezka_session_store.rs AES-256-GCM encrypted file store for SessionSnapshot bytes
media/config.rs                    typed secret-file config for Rezka account, cookie key, mirrors, probe URL, and deployment-supplied validation markers
media/composition.rs               construction-only wiring for Rezka client and encrypted session store
media/tests/architecture.rs        dependency-boundary regression tests for new crates
```

## Security and Redaction Invariants

Every task below must preserve these invariants:

```text
1. Secret material is read only from secret files or encrypted session files.
2. Rezka username, password, cookie snapshot bytes, encrypted-store key, Cookie headers, Set-Cookie headers, Authorization values, signed URL query strings, and raw provider snippets never appear in Display, Debug, tracing fields, panic messages, committed fixtures, or test snapshots.
3. Provider text is reduced to a fixed redacted sentinel before being stored in errors. URL redaction removes credentials and inline queries; sanitizer tests cover IPv4/IPv6 literals, Cookie/Set-Cookie/Authorization header and assignment/JSON forms, password/token keys, inline URLs, and raw provider snippets.
4. SessionSnapshot Debug prints only "SessionSnapshot { bytes: [REDACTED] }".
5. RezkaCredentials Debug prints only "RezkaCredentials { username: [REDACTED], password: [REDACTED] }".
6. EncryptedRezkaSessionStore Debug prints only the path parent and never the key or plaintext size.
7. `cookie_store` applies domain/path rules, but `Transport` additionally requires exact selected-origin scheme/host/effective-port equality before asking the jar for a Cookie header. Same-site subdomains and alternate ports are cross-origin and receive no site cookies.
8. Mirror rewrite preserves path and query but replaces only scheme/host/port with a configured Rezka mirror origin.
9. DLE validation treats authenticated non-premium accounts as valid; premium CSS is not an authentication proof.
10. Live probes require both `#[ignore]` and `REZKA_LIVE_PROBE=1`; an explicitly invoked ignored test fails immediately when the opt-in variable is absent or not exactly `1`.
11. Probe URL, non-empty valid markers, and non-empty invalid markers are deployment supplied. A response containing both marker classes is never `Valid`.
12. Every Reqwest client owned by `Transport` uses `reqwest::redirect::Policy::none()`; only the explicit bounded same-origin follower processes redirects, storing `Set-Cookie` at every hop. DLE login and Anubis pass always inspect their first response.
```

## Task 1: Workspace Crates, Dependency Pins, and Architecture Gates

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/rezka-client/Cargo.toml`
- Create: `crates/rezka-client/src/lib.rs`
- Create: `crates/rezka-client/src/error.rs`
- Create: `crates/rezka-client/src/redaction.rs`
- Create: `crates/rezka-client/src/mirror.rs`
- Create: `crates/rezka-client/src/transport.rs`
- Create: `crates/rezka-client/src/session/mod.rs`
- Create: `crates/rezka-client/src/session/anubis.rs`
- Create: `crates/rezka-client/src/session/cookie.rs`
- Create: `crates/rezka-client/src/session/dle.rs`
- Create: `crates/rezka-client/src/session/validation.rs`
- Create: `crates/media-runner/Cargo.toml`
- Create: `crates/media-runner/src/lib.rs`
- Create: `crates/media-runner/src/rezka_session_store.rs`
- Modify: `crates/media/Cargo.toml`
- Modify: `crates/media/tests/architecture.rs`
- Generate: `Cargo.lock`

**Interfaces:**
- Produces workspace packages `rezka-client` and `media-runner`.
- Produces architecture tests that fail until the crates and dependency edges exist.
- Later tasks fill the modules without changing crate ownership.

- [ ] **Step 1: Write failing architecture tests**

Add these tests to `crates/media/tests/architecture.rs`:

```rust
#[test]
fn rezka_client_has_no_workspace_dependencies() {
    let metadata = workspace_metadata();
    let rezka = workspace_package_id(&metadata, "rezka-client");
    let workspace_dependencies: Vec<&str> = direct_dependency_package_ids(&metadata, rezka)
        .into_iter()
        .filter(|package_id| metadata.workspace_members.contains(package_id))
        .map(|package_id| metadata[package_id].name.as_str())
        .collect();

    assert!(
        workspace_dependencies.is_empty(),
        "rezka-client must stay independent of media workspace crates: {workspace_dependencies:?}",
    );
}

#[test]
fn media_runner_has_no_storage_api_or_database_dependencies() {
    let metadata = workspace_metadata();
    let runner = workspace_package_id(&metadata, "media-runner");

    for forbidden_name in ["media-api", "media-storage", "sea-orm", "sea-orm-migration"] {
        let forbidden = metadata
            .packages
            .iter()
            .find(|package| package.name.as_str() == forbidden_name)
            .map(|package| &package.id);
        if let Some(forbidden) = forbidden {
            assert!(
                !resolved_dependency_reachable(&metadata, runner, forbidden),
                "media-runner must not reach {forbidden_name}",
            );
        }
    }
}

#[test]
fn media_runner_depends_only_on_rezka_client_workspace_crate_in_phase_3() {
    let metadata = workspace_metadata();
    let runner = workspace_package_id(&metadata, "media-runner");
    let mut workspace_dependencies: Vec<&str> = direct_dependency_package_ids(&metadata, runner)
        .into_iter()
        .filter(|package_id| metadata.workspace_members.contains(package_id))
        .map(|package_id| metadata[package_id].name.as_str())
        .collect();
    workspace_dependencies.sort_unstable();

    assert_eq!(workspace_dependencies, ["rezka-client"]);
}
```

- [ ] **Step 2: Run the focused failing test**

Run:

```bash
cargo test -p media --test architecture rezka_client_has_no_workspace_dependencies --locked
```

Expected: FAIL because workspace package `rezka-client` was not found.

- [ ] **Step 3: Add workspace members and pinned dependencies**

Update the root `Cargo.toml`:

```toml
[workspace]
members = [
  "crates/media-core",
  "crates/media-contract",
  "crates/media-api",
  "crates/media-storage",
  "crates/rezka-client",
  "crates/media-runner",
  "crates/media",
]
resolver = "3"
```

Add the dependency pins from "Dependency Pin Recommendations" to
`[workspace.dependencies]`.

- [ ] **Step 4: Create the `rezka-client` manifest and module skeleton**

Use this manifest:

```toml
[package]
name = "rezka-client"
edition.workspace = true
rust-version.workspace = true
version.workspace = true
publish.workspace = true

[dependencies]
cookie_store.workspace = true
hex.workspace = true
reqwest = { workspace = true, features = ["brotli", "deflate", "form", "gzip"] }
scraper.workspace = true
secrecy.workspace = true
serde.workspace = true
serde_json.workspace = true
sha2.workspace = true
thiserror.workspace = true
time.workspace = true
tokio = { workspace = true, features = ["time"] }
tracing.workspace = true
url.workspace = true

[dev-dependencies]
wiremock.workspace = true
tokio.workspace = true

[lints]
workspace = true
```

Create `crates/rezka-client/src/lib.rs`:

```rust
#![forbid(unsafe_code)]

pub mod error;
pub mod mirror;
pub mod redaction;
pub mod session;
pub mod transport;
```

Each new module file contains only this compilable module body in Task 1; do
not re-export any type until the task that defines it:

```rust
// Module body intentionally starts minimal; behavior lands behind failing tests in later tasks.
```

- [ ] **Step 5: Create the `media-runner` manifest and module skeleton**

Use this manifest:

```toml
[package]
name = "media-runner"
edition.workspace = true
rust-version.workspace = true
version.workspace = true
publish.workspace = true

[dependencies]
aes-gcm.workspace = true
base64.workspace = true
rand.workspace = true
rezka-client = { path = "../rezka-client", version = "=0.1.0" }
secrecy.workspace = true
serde.workspace = true
serde_json.workspace = true
tempfile.workspace = true
thiserror.workspace = true
zeroize.workspace = true

[lints]
workspace = true
```

Create `crates/media-runner/src/lib.rs`:

```rust
#![forbid(unsafe_code)]

pub mod rezka_session_store;
```

- [ ] **Step 6: Add composition dependencies to `media`**

Add to `crates/media/Cargo.toml`:

```toml
media-runner = { path = "../media-runner", version = "=0.1.0" }
rezka-client = { path = "../rezka-client", version = "=0.1.0" }
base64.workspace = true
url.workspace = true
```

If `tracing = "=0.1.44"` already exists as a crate-local dependency, replace it
with `tracing.workspace = true` after the workspace pin is added.

- [ ] **Step 7: Update and inspect `Cargo.lock`, then prove Task 1 builds**

The manifest edits introduce new workspace packages and dependencies, so the
first post-edit Cargo command MUST run without `--locked`:

```bash
cargo check --workspace
```

Expected: PASS and `Cargo.lock` is created or updated. Inspect only the lockfile
change before using locked commands:

```bash
git diff -- Cargo.lock
cargo metadata --locked --no-deps --format-version 1 > /dev/null
```

Expected: the diff contains only dependency resolution required by the new
manifests, and metadata exits 0 with the refreshed lockfile. If unrelated
packages changed, correct the manifest pins and rerun `cargo check --workspace`;
do not hand-edit `Cargo.lock`.

- [ ] **Step 8: Run the focused architecture tests**

Run:

```bash
cargo test -p media --test architecture rezka_client_has_no_workspace_dependencies --locked
cargo test -p media --test architecture media_runner_has_no_storage_api_or_database_dependencies --locked
cargo test -p media --test architecture media_runner_depends_only_on_rezka_client_workspace_crate_in_phase_3 --locked
```

Expected: PASS.

- [ ] **Step 9: Run task verification**

Run:

```bash
mise run check
mise run lint
mise run test
```

Expected: PASS.

- [ ] **Step 10: Commit**

```bash
git add Cargo.toml Cargo.lock crates/rezka-client crates/media-runner crates/media/Cargo.toml crates/media/tests/architecture.rs
git commit -m "feat: add rezka session workspace crates"
```

## Task 2: Public Error Model and Redaction Helpers

**Files:**
- Modify: `crates/rezka-client/src/lib.rs`
- Modify: `crates/rezka-client/src/error.rs`
- Modify: `crates/rezka-client/src/redaction.rs`
- Create: `crates/rezka-client/tests/redaction.rs`

**Interfaces:**
- Produces `RezkaErrorCode`, `RezkaError`, `SanitizedSnippet`, `redact_url`, and `sanitize_provider_text`.
- Later transport/session tasks use these errors and helpers for every provider failure.

```rust
pub enum RezkaErrorCode {
    ChallengeRequired,
    ChallengeFailed,
    AuthenticationRequired,
    AuthenticationFailed,
    ProviderResponseInvalid,
    RateLimited,
    Transport,
    Configuration,
}

pub enum RezkaError {
    ChallengeFailed { context: SanitizedSnippet },
    AuthenticationRequired { context: SanitizedSnippet },
    AuthenticationFailed { context: SanitizedSnippet },
    ProviderResponseInvalid { context: SanitizedSnippet },
    RateLimited { retry_after_seconds: Option<u64> },
    Transport { context: SanitizedSnippet },
    Configuration { message: &'static str },
}
```

- [ ] **Step 1: Write failing redaction tests**

Create `crates/rezka-client/tests/redaction.rs`:

```rust
use rezka_client::{
    error::{RezkaError, RezkaErrorCode},
    redaction::{redact_url, sanitize_provider_text},
};

#[test]
fn redacts_url_credentials_queries_and_ip_literals() {
    let url = redact_url("https://cdn.example/video.mp4?md5=secret-token&expires=123");
    assert_eq!(url.as_ref(), "https://cdn.example/video.mp4?[REDACTED]");

    let credentialed = redact_url("https://alice:hunter2@cdn.example/file");
    assert!(!credentialed.as_ref().contains("alice"));
    assert!(!credentialed.as_ref().contains("hunter2"));

    for literal in ["https://203.0.113.9/a?token=x", "https://[2001:db8::7]/a?token=x"] {
        let rendered = redact_url(literal).to_string();
        assert!(!rendered.contains("203.0.113.9"));
        assert!(!rendered.contains("2001:db8::7"));
        assert!(!rendered.contains("token=x"));
    }
}

#[test]
fn sanitizes_headers_assignments_json_inline_urls_ips_and_raw_provider_text() {
    let samples = [
        "Cookie: PHPSESSID=header-secret",
        "Set-Cookie = dle_password=assignment-secret",
        r#"{\"Authorization\":\"Bearer json-secret\"}"#,
        r#"{\"password\":\"hunter2\",\"access_token\":\"token-secret\"}"#,
        "request failed at https://cdn.example/file?sig=query-secret",
        "upstream 203.0.113.9 and [2001:db8::7] refused",
        "unstructured raw provider body with unique-secret-fragment",
    ];

    for sample in samples {
        let rendered = sanitize_provider_text(sample).to_string();
        assert_eq!(rendered, "[REDACTED_PROVIDER_TEXT]");
        assert!(!rendered.contains(sample));
    }
}

#[test]
fn error_display_and_debug_never_include_provider_secret_material() {
    let context = sanitize_provider_text("raw provider snippet sig=abc login_password=hunter2");
    let error = RezkaError::ProviderResponseInvalid { context };
    let rendered = format!("{error:?}: {error}");

    assert_eq!(error.code(), RezkaErrorCode::ProviderResponseInvalid);
    assert!(rendered.contains("provider response invalid"));
    for forbidden in ["sig=abc", "hunter2", "PHPSESSID", "secret"] {
        assert!(!rendered.contains(forbidden), "leaked {forbidden}");
    }
}
```

- [ ] **Step 2: Run the focused failing test**

Run:

```bash
cargo test -p rezka-client --test redaction --locked
```

Expected: FAIL because the redaction API does not exist yet.

- [ ] **Step 3: Implement minimal redaction and typed errors**

Implement `crates/rezka-client/src/redaction.rs`:

```rust
use std::{fmt, net::IpAddr};

#[derive(Clone, Eq, PartialEq)]
pub struct SanitizedSnippet(String);

impl SanitizedSnippet {
    #[must_use]
    pub fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SanitizedSnippet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("SanitizedSnippet").field(&self.0).finish()
    }
}

impl fmt::Display for SanitizedSnippet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct RedactedUrl(String);

impl RedactedUrl {
    #[must_use]
    pub fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RedactedUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl fmt::Display for RedactedUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[must_use]
pub fn redact_url(value: &str) -> RedactedUrl {
    match url::Url::parse(value) {
        Ok(mut url) => {
            if url.query().is_some() {
                url.set_query(Some("[REDACTED]"));
            }
            if !url.username().is_empty() || url.password().is_some() {
                let _ = url.set_username("[REDACTED]");
                let _ = url.set_password(Some("[REDACTED]"));
            }
            if url.host_str().is_some_and(|host| host.parse::<IpAddr>().is_ok()) {
                let _ = url.set_host(Some("redacted.invalid"));
            }
            RedactedUrl(url.to_string())
        }
        Err(_) => RedactedUrl("[REDACTED_URL]".to_owned()),
    }
}

#[must_use]
pub fn sanitize_provider_text(input: &str) -> SanitizedSnippet {
    let _ = input;
    SanitizedSnippet("[REDACTED_PROVIDER_TEXT]".to_owned())
}
```

The fixed sentinel is intentional: Phase 3 has no need to retain arbitrary
provider text, and allow-listing a few known-safe status fields is safer than
trying to recognize every header, assignment, JSON, URL, IP, password, or token
shape. Build status context from static labels plus `redact_url`; never splice a
raw response snippet into an error or tracing field.

Implement `crates/rezka-client/src/error.rs` using the interface above and
`thiserror`. Every display string must be static or sanitized, for example:

```rust
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum RezkaErrorCode {
    ChallengeRequired,
    ChallengeFailed,
    AuthenticationRequired,
    AuthenticationFailed,
    ProviderResponseInvalid,
    RateLimited,
    Transport,
    Configuration,
}

#[derive(Debug, thiserror::Error)]
pub enum RezkaError {
    #[error("challenge failed: {context}")]
    ChallengeFailed { context: crate::redaction::SanitizedSnippet },
    #[error("authentication required: {context}")]
    AuthenticationRequired { context: crate::redaction::SanitizedSnippet },
    #[error("authentication failed: {context}")]
    AuthenticationFailed { context: crate::redaction::SanitizedSnippet },
    #[error("provider response invalid: {context}")]
    ProviderResponseInvalid { context: crate::redaction::SanitizedSnippet },
    #[error("rate limited")]
    RateLimited { retry_after_seconds: Option<u64> },
    #[error("transport failed: {context}")]
    Transport { context: crate::redaction::SanitizedSnippet },
    #[error("configuration invalid: {message}")]
    Configuration { message: &'static str },
}

impl RezkaError {
    #[must_use]
    pub const fn code(&self) -> RezkaErrorCode {
        match self {
            Self::ChallengeFailed { .. } => RezkaErrorCode::ChallengeFailed,
            Self::AuthenticationRequired { .. } => RezkaErrorCode::AuthenticationRequired,
            Self::AuthenticationFailed { .. } => RezkaErrorCode::AuthenticationFailed,
            Self::ProviderResponseInvalid { .. } => RezkaErrorCode::ProviderResponseInvalid,
            Self::RateLimited { .. } => RezkaErrorCode::RateLimited,
            Self::Transport { .. } => RezkaErrorCode::Transport,
            Self::Configuration { .. } => RezkaErrorCode::Configuration,
        }
    }
}
```

After the types compile, add their first root re-export to
`crates/rezka-client/src/lib.rs`:

```rust
pub use error::{RezkaError, RezkaErrorCode};
```

- [ ] **Step 4: Run the focused test**

Run:

```bash
cargo test -p rezka-client --test redaction --locked
```

Expected: PASS.

- [ ] **Step 5: Run task verification**

Run:

```bash
mise run check
mise run lint
mise run test
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/rezka-client/src/lib.rs crates/rezka-client/src/error.rs crates/rezka-client/src/redaction.rs crates/rezka-client/tests/redaction.rs
git commit -m "feat: add rezka redacted error model"
```

## Task 3: Mirrors, No-Auto-Redirect Transport, and Selected-Origin Cookie Guard

**Files:**
- Modify: `crates/rezka-client/src/lib.rs`
- Modify: `crates/rezka-client/src/mirror.rs`
- Modify: `crates/rezka-client/src/session/mod.rs`
- Modify: `crates/rezka-client/src/session/cookie.rs`
- Modify: `crates/rezka-client/src/transport.rs`
- Create: `crates/rezka-client/tests/mirror_cookie_origin.rs`
- Create: `crates/rezka-client/tests/support/mod.rs`

**Interfaces:**
- Produces `MirrorSet`, `SessionJar`, `SessionSnapshot`, and `Transport`.
- Later Anubis and DLE tasks consume one `Transport` instance and therefore one cookie jar and one stable User-Agent.

```rust
pub struct MirrorSet;
impl MirrorSet {
    pub fn new(origins: Vec<url::Url>) -> Result<Self, RezkaError>;
    pub fn primary_origin(&self) -> &url::Url;
    pub fn selected_origin(&self) -> &url::Url;
    pub fn contains_origin(&self, candidate: &url::Url) -> bool;
    pub fn rewrite_to_selected(&self, url: &url::Url) -> Result<url::Url, RezkaError>;
    pub fn select_next(&mut self) -> bool;
}

pub struct SessionSnapshot;
impl SessionSnapshot {
    pub fn from_secret_bytes(bytes: secrecy::SecretBox<Vec<u8>>) -> Self;
    pub fn with_secret_bytes<R>(&self, consumer: impl FnOnce(&[u8]) -> R) -> R;
    pub fn secret_eq(&self, other: &Self) -> bool;
}

pub struct SessionJar;
impl SessionJar {
    pub fn empty() -> Self;
    pub fn import(snapshot: &SessionSnapshot) -> Result<Self, RezkaError>;
    pub fn export(&self) -> Result<SessionSnapshot, RezkaError>;
    pub fn store_response_cookies<'a>(&mut self, headers: impl Iterator<Item = &'a str>, url: &url::Url);
    pub fn contains_cookie_for_url(&self, url: &url::Url, name: &str) -> bool;
}

pub struct TransportResponse {
    pub status: reqwest::StatusCode,
    pub url: url::Url,
    pub body: String,
    pub location: Option<url::Url>,
    stored_cookie_names: std::collections::BTreeSet<String>,
}
impl TransportResponse {
    pub fn stored_cookie_names(&self) -> &std::collections::BTreeSet<String>;
}

pub struct Transport;
impl Transport {
    pub fn new(mirrors: MirrorSet, jar: SessionJar, user_agent: String, request_timeout: time::Duration, max_retries: u8) -> Result<Self, RezkaError>;
    pub fn selected_origin(&self) -> &url::Url;
    pub async fn get_first(&mut self, url: url::Url, referer: Option<url::Url>) -> Result<TransportResponse, RezkaError>;
    pub async fn get_first_with_failover(&mut self, url: url::Url, referer: Option<url::Url>) -> Result<TransportResponse, RezkaError>;
    pub async fn get_following(&mut self, url: url::Url, referer: Option<url::Url>, max_redirects: u8) -> Result<TransportResponse, RezkaError>;
    pub async fn post_form_first(&mut self, url: url::Url, referer: Option<url::Url>, form: &[(&str, &str)]) -> Result<TransportResponse, RezkaError>;
    pub fn export_session(&self) -> Result<SessionSnapshot, RezkaError>;
}
```

- [ ] **Step 1: Write failing mirror and cookie-origin tests**

Create `crates/rezka-client/tests/mirror_cookie_origin.rs`:

```rust
use rezka_client::{
    mirror::MirrorSet,
    session::cookie::{SessionJar, SessionSnapshot},
};
use url::Url;

#[test]
fn mirror_rewrite_preserves_path_and_query_but_changes_only_origin() {
    let mirrors = MirrorSet::new(vec![Url::parse("https://rezka.test").unwrap()]).unwrap();
    let title = Url::parse("https://old.example/series/drama/42-title.html?season=1").unwrap();

    let rewritten = mirrors.rewrite_to_selected(&title).unwrap();

    assert_eq!(rewritten.as_str(), "https://rezka.test/series/drama/42-title.html?season=1");
}

#[test]
fn mirror_origins_reject_credentials_paths_queries_and_fragments() {
    for invalid in [
        "https://user:pass@rezka.test",
        "https://rezka.test/path",
        "https://rezka.test?query=1",
        "https://rezka.test/#fragment",
        "ftp://rezka.test",
    ] {
        let error = MirrorSet::new(vec![Url::parse(invalid).unwrap()]).unwrap_err();
        assert!(!format!("{error:?}: {error}").contains("user:pass"));
    }
}

#[test]
fn cookie_snapshot_round_trips_and_remains_redacted_in_debug() {
    let mut jar = SessionJar::empty();
    let origin = Url::parse("https://rezka.test/").unwrap();
    jar.store_response_cookies(
        [
            "session_cookie=opaque-a; Path=/; HttpOnly",
            "persistent_cookie=opaque-b; Path=/; Max-Age=3600",
            "expired_cookie=opaque-c; Path=/; Max-Age=0",
        ]
        .iter()
        .copied(),
        &origin,
    );

    let snapshot = jar.export().unwrap();
    let debug = format!("{snapshot:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("session_cookie"));
    assert!(!debug.contains("opaque-a"));

    let restored = SessionJar::import(&snapshot).unwrap();
    assert!(restored.contains_cookie_for_url(&origin, "session_cookie"));
    assert!(restored.contains_cookie_for_url(&origin, "persistent_cookie"));
    assert!(!restored.contains_cookie_for_url(&origin, "expired_cookie"));
}

#[tokio::test]
async fn transport_rejects_unrelated_and_same_site_cross_origin_cookie_targets() {
    use rezka_client::transport::Transport;
    use time::Duration;

    let mut jar = SessionJar::empty();
    let site = Url::parse("https://rezka.test/").unwrap();
    jar.store_response_cookies(
        ["site_session=opaque; Domain=rezka.test; Path=/; HttpOnly"].iter().copied(),
        &site,
    );
    let jar = SessionJar::import(&jar.export().unwrap()).unwrap();
    let mirrors = MirrorSet::new(vec![site.clone()]).unwrap();
    let mut transport = Transport::new(
        mirrors,
        jar,
        "media-orchestrator-test".to_owned(),
        Duration::seconds(10),
        0,
    )
    .unwrap();

    for forbidden in [
        "https://cdn.test/video.mp4?sig=x",
        "https://cdn.rezka.test/video.mp4?sig=x",
        "https://rezka.test:444/video.mp4?sig=x",
        "http://rezka.test/video.mp4?sig=x",
    ] {
        let error = transport.get_first(Url::parse(forbidden).unwrap(), None).await.unwrap_err();
        assert_eq!(error.code(), rezka_client::RezkaErrorCode::Configuration);
    }
}

#[test]
fn invalid_cookie_snapshot_does_not_expose_plaintext() {
    use secrecy::SecretBox;

    let snapshot = SessionSnapshot::from_secret_bytes(SecretBox::new(Box::new(
        vec![0xff, 0x00, 0x7f, 0x01],
    )));
    let error = SessionJar::import(&snapshot).unwrap_err();
    let rendered = format!("{error:?}: {error}");

    assert_eq!(error.code(), rezka_client::RezkaErrorCode::ProviderResponseInvalid);
    assert!(!rendered.contains("255"));
}
```

Add these tests to the same file:

```rust
#[tokio::test]
async fn explicit_bounded_redirects_store_cookies_from_every_hop() {
    use rezka_client::transport::Transport;
    use time::Duration;
    use wiremock::{matchers::{method, path}, Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET")).and(path("/start"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/middle")
            .insert_header("set-cookie", "hop_a=opaque-a; Path=/"))
        .expect(1).mount(&server).await;
    Mock::given(method("GET")).and(path("/middle"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/done")
            .insert_header("set-cookie", "hop_b=opaque-b; Path=/"))
        .expect(1).mount(&server).await;
    Mock::given(method("GET")).and(path("/done"))
        .respond_with(ResponseTemplate::new(200).set_body_string("done"))
        .expect(1).mount(&server).await;

    let mut transport = Transport::new(
        MirrorSet::new(vec![base.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        Duration::seconds(10),
        0,
    ).unwrap();
    let response = transport.get_following(base.join("/start").unwrap(), None, 2).await.unwrap();
    assert_eq!(response.url.path(), "/done");
    let restored = SessionJar::import(&transport.export_session().unwrap()).unwrap();
    assert!(restored.contains_cookie_for_url(&base, "hop_a"));
    assert!(restored.contains_cookie_for_url(&base, "hop_b"));
}

#[tokio::test]
async fn explicit_redirect_follower_stops_at_the_configured_bound() {
    use rezka_client::transport::Transport;
    use time::Duration;
    use wiremock::{matchers::{method, path}, Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET")).and(path("/start"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/middle"))
        .expect(1).mount(&server).await;
    Mock::given(method("GET")).and(path("/middle"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/must-not-follow"))
        .expect(1).mount(&server).await;
    Mock::given(method("GET")).and(path("/must-not-follow"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0).mount(&server).await;

    let mut transport = Transport::new(
        MirrorSet::new(vec![base.clone()]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        Duration::seconds(10),
        0,
    ).unwrap();
    let error = transport.get_following(base.join("/start").unwrap(), None, 1).await.unwrap_err();
    assert_eq!(error.code(), rezka_client::RezkaErrorCode::ProviderResponseInvalid);
    assert!(!format!("{error:?}: {error}").contains("must-not-follow"));
}

#[tokio::test]
async fn eligible_connect_failure_selects_next_mirror_within_retry_bound() {
    use rezka_client::transport::Transport;
    use std::net::TcpListener;
    use time::Duration;
    use wiremock::{matchers::{method, path, query_param}, Mock, MockServer, ResponseTemplate};

    let unavailable_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let unavailable = Url::parse(&format!("http://{}", unavailable_listener.local_addr().unwrap())).unwrap();
    drop(unavailable_listener);

    let server = MockServer::start().await;
    let available = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .and(query_param("source", "configured"))
        .respond_with(ResponseTemplate::new(200).set_body_string("valid-marker"))
        .expect(1)
        .mount(&server)
        .await;

    let mirrors = MirrorSet::new(vec![unavailable.clone(), available.clone()]).unwrap();
    let mut transport = Transport::new(
        mirrors,
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        Duration::seconds(2),
        1,
    )
    .unwrap();
    let configured_probe = unavailable.join("/account/probe?source=configured").unwrap();

    let response = transport.get_first_with_failover(configured_probe, None).await.unwrap();

    assert_eq!(response.url.as_str(), available.join("/account/probe?source=configured").unwrap().as_str());
    assert_eq!(transport.selected_origin(), &available);
}

#[tokio::test]
async fn zero_retry_budget_never_contacts_or_selects_second_mirror() {
    use rezka_client::transport::Transport;
    use std::net::TcpListener;
    use time::Duration;
    use wiremock::{matchers::{method, path}, Mock, MockServer, ResponseTemplate};

    let unavailable_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let unavailable = Url::parse(&format!("http://{}", unavailable_listener.local_addr().unwrap())).unwrap();
    drop(unavailable_listener);
    let second = MockServer::start().await;
    let available = Url::parse(&second.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&second)
        .await;

    let mut transport = Transport::new(
        MirrorSet::new(vec![unavailable.clone(), available]).unwrap(),
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        Duration::seconds(2),
        0,
    )
    .unwrap();

    let error = transport
        .get_first_with_failover(unavailable.join("/account/probe").unwrap(), None)
        .await
        .unwrap_err();

    assert_eq!(error.code(), rezka_client::RezkaErrorCode::Transport);
    assert_eq!(transport.selected_origin(), &unavailable);
}
```

- [ ] **Step 2: Run the focused failing test**

Run:

```bash
cargo test -p rezka-client --test mirror_cookie_origin --locked
```

Expected: FAIL because mirror and cookie APIs do not exist yet.

- [ ] **Step 3: Implement `MirrorSet`**

Implement the validation rules:

```text
1. At least one origin is required.
2. Each origin must use http or https.
3. Username and password must be empty.
4. Path must be "/" or empty.
5. Query and fragment must be absent.
6. Selection starts at index 0. `rewrite_to_selected` copies path/query from the input URL and uses only the selected mirror's scheme/host/port.
7. `select_next` advances exactly one configured mirror and returns false at the final mirror; it never wraps within one operation.
8. `contains_origin` compares normalized scheme, host, and effective port against every configured mirror, independent of the currently selected index.
```

Return `RezkaError::Configuration { message: "invalid mirror origin" }` for
invalid mirrors.

- [ ] **Step 4: Implement `SessionJar` and `SessionSnapshot`**

Implementation requirements:

```text
1. Use cookie_store::CookieStore as the backing jar.
2. Export with `cookie_store::serde::json::save_incl_expired_and_nonpersistent`, so unexpired session cookies are persisted too. Import with `cookie_store::serde::json::load`, which restores persistent and non-persistent entries while filtering expired cookies.
3. Store snapshot bytes in secrecy::SecretBox<Vec<u8>>.
4. Implement Debug for SessionSnapshot manually and redact all bytes.
5. Do not expose a plaintext-returning accessor. `with_secret_bytes` scopes access to a callback for `media-runner` encryption, and `secret_eq` supports opaque round-trip assertions without returning bytes.
6. Keep the jar's request-value function private to `Transport`; do not add any helper that copies site cookies to another origin.
```

- [ ] **Step 5: Implement minimal `Transport`**

Implementation requirements:

```text
1. `Transport::new` builds and owns the only Reqwest client with `reqwest::redirect::Policy::none()`; callers cannot inject a client with a different policy.
2. Attach the stable User-Agent to every request.
3. Attach Referer only when the caller supplies it.
4. Before querying `cookie_store` or attaching Cookie, require exact equality of selected-origin scheme, host, and `port_or_known_default()` with the request URL. Reject unrelated hosts, same-site subdomains, scheme changes, and alternate ports before sending any request.
5. `get_first` and `post_form_first` always return the first response and store all of its Set-Cookie headers, including on 30x. While storing, collect only successfully parsed cookie names from that response into `TransportResponse.stored_cookie_names`; never retain or expose cookie values in `TransportResponse`, Debug, Display, or tracing. DLE and Anubis use only these methods.
6. `get_following` alone follows redirects, resolves relative Location against the current response URL, repeats the selected-origin guard at every hop, stores Set-Cookie before following, and stops after the caller's exact bound.
7. Classify HTTP 429 as RezkaError::RateLimited.
8. Return `TransportResponse` for 2xx and 30x so protocol layers can classify first responses. For other statuses, return ProviderResponseInvalid with sanitized status and redacted URL.
9. Read response bytes and decode as UTF-8 lossily for HTML/auth responses.
10. `get_first_with_failover` is reserved for harmless idempotent probe GETs. It attempts the selected mirror once, then at most `max_retries` additional mirrors and never more than `mirrors.len()` total attempts.
11. Eligible failover conditions are connection refusal/unreachable, request timeout before a response, and HTTP 502/503/504. HTTP 429, other 4xx, TLS/certificate validation failures, body/parse failures, Anubis responses, and authentication responses are terminal and do not rotate mirrors.
12. Before each eligible retry, call `select_next`, rewrite the original path/query onto the new selected origin, repeat the exact-origin cookie guard, and keep the new mirror selected after success. Return the final sanitized Transport error when the bound or mirror list is exhausted.
13. `get_first`, `get_following`, and `post_form_first` never fail over. Anubis pass and DLE login are tied to the currently selected origin and must not replay against another mirror.
14. A zero retry budget means exactly one attempt. On an eligible first-mirror failure, return the typed error without calling `select_next`; the second mirror receives zero requests and `selected_origin` remains unchanged.
```

Complete Task 3's module/root exports only after these types compile:

```rust
// crates/rezka-client/src/session/mod.rs
pub mod cookie;

// crates/rezka-client/src/lib.rs
pub use mirror::MirrorSet;
pub use session::cookie::SessionSnapshot;
```

Do not add catalog, playback, subtitle, or CDN-specific methods.

- [ ] **Step 6: Run the focused test**

Run:

```bash
cargo test -p rezka-client --test mirror_cookie_origin --locked
```

Expected: PASS.

- [ ] **Step 7: Run task verification**

Run:

```bash
mise run check
mise run lint
mise run test
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/rezka-client/src/lib.rs crates/rezka-client/src/mirror.rs crates/rezka-client/src/session/mod.rs crates/rezka-client/src/session/cookie.rs crates/rezka-client/src/transport.rs crates/rezka-client/tests/mirror_cookie_origin.rs crates/rezka-client/tests/support/mod.rs
git commit -m "feat: add rezka transport cookie origin handling"
```

## Task 4: Anubis Detection, Bounded Proof-of-Work, and Pass Submission

**Files:**
- Modify: `crates/rezka-client/src/session/anubis.rs`
- Modify: `crates/rezka-client/src/session/mod.rs`
- Create: `crates/rezka-client/tests/anubis.rs`
- Create: `crates/rezka-client/tests/fixtures/anubis_challenge.html`
- Create: `crates/rezka-client/tests/fixtures/anubis_malformed.html`

**Interfaces:**
- Produces Anubis challenge parsing and proof-of-work used by the session orchestrator.

```rust
pub struct AnubisChallenge {
    pub id: String,
    pub random_data: String,
    pub difficulty: u8,
}

pub struct AnubisProof {
    pub response_hex: String,
    pub nonce: u64,
}

pub fn detect_challenge(html: &str) -> bool;
pub fn parse_challenge(html: &str) -> Result<AnubisChallenge, RezkaError>;
pub fn solve_challenge(challenge: &AnubisChallenge, max_nonce: u64) -> Result<AnubisProof, RezkaError>;
pub async fn submit_challenge(transport: &mut Transport, challenge: &AnubisChallenge, proof: &AnubisProof, redir: url::Url, elapsed_ms: u128) -> Result<(), RezkaError>;
```

- [ ] **Step 1: Add Anubis fixtures**

Create `crates/rezka-client/tests/fixtures/anubis_challenge.html`:

```html
<!doctype html>
<html>
  <body>
    <script id="anubis_challenge" type="application/json">
      {"challenge":{"id":"challenge-123","randomData":"abc"},"rules":{"difficulty":3}}
    </script>
  </body>
</html>
```

Create `crates/rezka-client/tests/fixtures/anubis_malformed.html`:

```html
<!doctype html>
<html>
  <body>
    <script id="anubis_challenge" type="application/json">
      {"challenge":{"id":"","randomData":""},"rules":{"difficulty":3}}
    </script>
  </body>
</html>
```

- [ ] **Step 2: Write failing Anubis tests**

Create `crates/rezka-client/tests/anubis.rs`:

```rust
use rezka_client::session::anubis::{detect_challenge, parse_challenge, solve_challenge};

#[test]
fn detects_and_parses_anubis_challenge_from_html_200_body() {
    let html = include_str!("fixtures/anubis_challenge.html");

    let challenge = parse_challenge(html).unwrap();

    assert!(detect_challenge(html));
    assert_eq!(challenge.id, "challenge-123");
    assert_eq!(challenge.random_data, "abc");
    assert_eq!(challenge.difficulty, 3);
}

#[test]
fn solves_even_and_odd_leading_zero_nibble_difficulties() {
    let html = include_str!("fixtures/anubis_challenge.html");
    let mut challenge = parse_challenge(html).unwrap();

    challenge.difficulty = 2;
    let even = solve_challenge(&challenge, 100_000).unwrap();
    assert!(even.response_hex.starts_with("00"));

    challenge.difficulty = 3;
    let odd = solve_challenge(&challenge, 100_000).unwrap();
    assert!(odd.response_hex.starts_with("000"));
}

#[test]
fn malformed_challenge_and_excessive_work_are_sanitized_failures() {
    let malformed = include_str!("fixtures/anubis_malformed.html");
    let malformed_error = parse_challenge(malformed).unwrap_err();
    assert!(!format!("{malformed_error:?}: {malformed_error}").contains("randomData"));

    let html = include_str!("fixtures/anubis_challenge.html");
    let mut challenge = parse_challenge(html).unwrap();
    challenge.difficulty = 64;
    let bounded_error = solve_challenge(&challenge, 10).unwrap_err();
    assert!(format!("{bounded_error}").contains("challenge failed"));
}
```

- [ ] **Step 3: Run the focused failing test**

Run:

```bash
cargo test -p rezka-client --test anubis --locked
```

Expected: FAIL because Anubis parsing and proof-of-work do not exist yet.

- [ ] **Step 4: Implement challenge detection and parsing**

Implementation requirements:

```text
1. Detect Anubis by the presence of an element with id="anubis_challenge", not by HTTP status.
2. Parse the element text as JSON with fields challenge.id, challenge.randomData, and rules.difficulty.
3. Reject empty id, empty randomData, difficulty 0, and difficulty above 32.
4. Use sanitized ProviderResponseInvalid or ChallengeFailed errors.
```

- [ ] **Step 5: Implement bounded proof-of-work**

Implementation requirements:

```text
1. For nonce from 0 through max_nonce inclusive, compute SHA256(randomData + decimal_nonce).
2. Encode the digest as lowercase hex.
3. Accept only when the hex string has difficulty leading '0' characters.
4. Return ChallengeFailed when no nonce is found within max_nonce.
```

- [ ] **Step 6: Add pass-submission mock test**

Extend `crates/rezka-client/tests/anubis.rs` with a `wiremock` async test:

```rust
#[tokio::test]
async fn submits_pass_challenge_with_same_user_agent_and_retains_cookie() {
    use rezka_client::{
        mirror::MirrorSet,
        session::anubis::{parse_challenge, solve_challenge, submit_challenge},
        session::cookie::SessionJar,
        transport::Transport,
    };
    use url::Url;
    use wiremock::{
        matchers::{header, method, path, query_param},
        Mock, MockServer, ResponseTemplate,
    };

    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    let title = base.join("/series/1-title.html").unwrap();
    let challenge = parse_challenge(include_str!("fixtures/anubis_challenge.html")).unwrap();
    let proof = solve_challenge(&challenge, 100_000).unwrap();

    Mock::given(method("GET"))
        .and(path("/.within.website/x/cmd/anubis/api/pass-challenge"))
        .and(query_param("id", "challenge-123"))
        .and(query_param("nonce", proof.nonce.to_string()))
        .and(query_param("response", proof.response_hex.clone()))
        .and(query_param("redir", title.as_str()))
        .and(query_param("elapsedTime", "42"))
        .and(header("user-agent", "media-orchestrator-test"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "/must-not-be-followed")
                .insert_header("set-cookie", "anubis=opaque; Path=/"),
        )
        .mount(&server)
        .await;

    let mirrors = MirrorSet::new(vec![base.clone()]).unwrap();
    let mut transport = Transport::new(
        mirrors,
        SessionJar::empty(),
        "media-orchestrator-test".to_owned(),
        time::Duration::seconds(10),
        0,
    )
    .unwrap();

    submit_challenge(&mut transport, &challenge, &proof, title.clone(), 42).await.unwrap();

    let snapshot = transport.export_session().unwrap();
    let restored = SessionJar::import(&snapshot).unwrap();
    assert!(restored.contains_cookie_for_url(&title, "anubis"));
}
```

`submit_challenge` MUST call `Transport::get_first`, accept the observed first
30x response after storing its cookie, and never follow its Location. The mock's
unmounted `/must-not-be-followed` target makes accidental auto-follow fail the
test.

Expose the module only after its implementation compiles:

```rust
// crates/rezka-client/src/session/mod.rs
pub mod anubis;
```

- [ ] **Step 7: Run the focused tests**

Run:

```bash
cargo test -p rezka-client --test anubis --locked
```

Expected: PASS.

- [ ] **Step 8: Run task verification**

Run:

```bash
mise run check
mise run lint
mise run test
```

Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/rezka-client/src/session/anubis.rs crates/rezka-client/src/session/mod.rs crates/rezka-client/tests/anubis.rs crates/rezka-client/tests/fixtures/anubis_challenge.html crates/rezka-client/tests/fixtures/anubis_malformed.html
git commit -m "feat: solve rezka anubis challenge"
```

## Task 5: DLE Login, Caller-Supplied Session Validation, and Session Orchestration

**Files:**
- Modify: `crates/rezka-client/src/lib.rs`
- Modify: `crates/rezka-client/src/session/dle.rs`
- Modify: `crates/rezka-client/src/session/validation.rs`
- Modify: `crates/rezka-client/src/session/mod.rs`
- Modify: `crates/rezka-client/src/transport.rs`
- Create: `crates/rezka-client/tests/session_flow.rs`
- Create: `crates/rezka-client/tests/fixtures/dle_login_failed.json`
- Create: `crates/rezka-client/tests/fixtures/dle_login_success.json`

**Interfaces:**
- Produces the primary Phase 3 `RezkaClient` session API.
- Consumes `Transport`, `MirrorSet`, `SessionJar`, Anubis helpers, and redacted errors.

```rust
pub struct RezkaClientConfig {
    pub mirrors: MirrorSet,
    pub user_agent: String,
    pub request_timeout: time::Duration,
    pub max_retries: u8,
    pub anubis_max_nonce: u64,
}

pub struct RezkaCredentials {
    pub username: secrecy::SecretString,
    pub password: secrecy::SecretString,
}

pub struct SessionValidationProbe {
    url: url::Url,
    valid_markers: Vec<String>,
    invalid_markers: Vec<String>,
}
impl SessionValidationProbe {
    pub fn new(url: url::Url, valid_markers: Vec<String>, invalid_markers: Vec<String>) -> Result<Self, RezkaError>;
}

pub struct ProbeResponse {
    pub status: reqwest::StatusCode,
    pub url: url::Url,
    body: String,
}
impl ProbeResponse {
    pub fn body(&self) -> &str;
}

pub enum SessionValidation {
    Valid,
    Invalid,
    Inconclusive,
}

pub struct RezkaClient;
impl RezkaClient {
    pub fn new(config: RezkaClientConfig) -> Result<Self, RezkaError>;
    pub fn from_snapshot(config: RezkaClientConfig, snapshot: &SessionSnapshot) -> Result<Self, RezkaError>;
    pub async fn ensure_authenticated(&mut self, credentials: &RezkaCredentials, probe: &SessionValidationProbe) -> Result<SessionValidation, RezkaError>;
    pub async fn fetch_probe(&mut self, probe: &SessionValidationProbe) -> Result<ProbeResponse, RezkaError>;
    pub fn classify_probe(probe: &SessionValidationProbe, response: &ProbeResponse) -> SessionValidation;
    pub fn export_session(&self) -> Result<SessionSnapshot, RezkaError>;
}
```

- [ ] **Step 1: Add DLE fixtures**

Create `crates/rezka-client/tests/fixtures/dle_login_success.json`:

```json
{"success":true}
```

Create `crates/rezka-client/tests/fixtures/dle_login_failed.json`:

```json
{"success":false}
```

- [ ] **Step 2: Write failing session-flow tests**

Create `crates/rezka-client/tests/session_flow.rs` with mock-server tests for
the full auth state machine:

```rust
use rezka_client::{
    mirror::MirrorSet,
    session::{
        cookie::SessionJar, RezkaClient, RezkaClientConfig, RezkaCredentials, SessionValidation,
        SessionValidationProbe,
    },
};
use secrecy::SecretString;
use time::Duration;
use url::Url;
use wiremock::{
    matchers::{body_string_contains, header, method, path},
    Mock, MockServer, ResponseTemplate,
};

fn config(base: Url) -> RezkaClientConfig {
    RezkaClientConfig {
        mirrors: MirrorSet::new(vec![base]).unwrap(),
        user_agent: "media-orchestrator-test".to_owned(),
        request_timeout: Duration::seconds(10),
        max_retries: 0,
        anubis_max_nonce: 100_000,
    }
}

fn credentials() -> RezkaCredentials {
    RezkaCredentials {
        username: SecretString::from("rezka-user"),
        password: SecretString::from("rezka-password"),
    }
}

fn probe(base: &Url) -> SessionValidationProbe {
    SessionValidationProbe::new(
        base.join("/account/probe").unwrap(),
        vec!["data-authenticated=\"true\"".to_owned()],
        vec!["name=\"login_name\"".to_owned()],
    )
    .unwrap()
}

#[tokio::test]
async fn restored_valid_cookie_jar_skips_anubis_and_dle_login() {
    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();

    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .and(header("cookie", "PHPSESSID=valid"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html data-authenticated=\"true\"></html>"))
        .expect(1)
        .mount(&server)
        .await;

    let mut jar = SessionJar::empty();
    jar.store_response_cookies(["PHPSESSID=valid; Path=/; HttpOnly"].iter().copied(), &base);
    let snapshot = jar.export().unwrap();
    let mut restored = RezkaClient::from_snapshot(config(base.clone()), &snapshot).unwrap();

    let result = restored.ensure_authenticated(&credentials(), &probe(&base)).await.unwrap();

    assert_eq!(result, SessionValidation::Valid);
}

#[tokio::test]
async fn anubis_then_invalid_session_runs_dle_and_final_probe_using_three_total_fetches() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use wiremock::{Match, Request, Respond};

    #[derive(Clone)]
    struct ProbeSequence {
        calls: Arc<AtomicUsize>,
    }

    impl Respond for ProbeSequence {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let cookie = request
                .headers
                .get("cookie")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();

            match call {
                0 => ResponseTemplate::new(200)
                    .set_body_string(include_str!("fixtures/anubis_challenge.html")),
                1 => {
                    assert!(cookie.contains("anubis=opaque"));
                    ResponseTemplate::new(200)
                        .set_body_string("<input name=\"login_name\">")
                }
                2 => {
                    assert!(cookie.contains("anubis=opaque"));
                    assert!(cookie.contains("PHPSESSID=logged-in"));
                    ResponseTemplate::new(200)
                        .set_body_string("<html data-authenticated=\"true\"></html>")
                }
                _ => panic!("probe fetched more than three times"),
            }
        }
    }

    struct AnubisPassQuery {
        redir: String,
    }

    impl Match for AnubisPassQuery {
        fn matches(&self, request: &Request) -> bool {
            let query: std::collections::HashMap<_, _> = request.url.query_pairs().into_owned().collect();
            query.get("id").is_some_and(|value| value == "challenge-123")
                && query.get("nonce").is_some_and(|value| value == "1322")
                && query.get("response").is_some_and(|value| {
                    value == "000213955c51ad382c14a1634987938c793bb005b6106a3943a16795b65227cd"
                })
                && query.get("redir").is_some_and(|value| value == &self.redir)
                && query.get("elapsedTime").and_then(|value| value.parse::<u128>().ok()).is_some()
        }
    }

    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    let probe_calls = Arc::new(AtomicUsize::new(0));

    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .and(header("user-agent", "media-orchestrator-test"))
        .respond_with(ProbeSequence {
            calls: Arc::clone(&probe_calls),
        })
        .expect(3)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/.within.website/x/cmd/anubis/api/pass-challenge"))
        .and(header("user-agent", "media-orchestrator-test"))
        .and(AnubisPassQuery {
            redir: base.join("/account/probe").unwrap().to_string(),
        })
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "/must-not-follow")
                .insert_header("set-cookie", "anubis=opaque; Path=/; HttpOnly"),
        )
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/ajax/login/"))
        .and(header("x-requested-with", "XMLHttpRequest"))
        .and(header("user-agent", "media-orchestrator-test"))
        .and(body_string_contains("login_name=rezka-user"))
        .and(body_string_contains("login_password=rezka-password"))
        .and(body_string_contains("login_not_save=0"))
        .and(body_string_contains("login=submit"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "/")
                .insert_header("set-cookie", "PHPSESSID=logged-in; Path=/; HttpOnly"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut client = RezkaClient::new(config(base.clone())).unwrap();
    let result = client.ensure_authenticated(&credentials(), &probe(&base)).await.unwrap();

    assert_eq!(result, SessionValidation::Valid);
    assert_eq!(probe_calls.load(Ordering::SeqCst), 3);
    let restored = SessionJar::import(&client.export_session().unwrap()).unwrap();
    assert!(restored.contains_cookie_for_url(&base, "anubis"));
    assert!(restored.contains_cookie_for_url(&base, "PHPSESSID"));
}

#[tokio::test]
async fn failed_login_and_inconclusive_validation_are_sanitized() {
    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();

    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<input name=\"login_name\">"))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/ajax/login/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(include_str!("fixtures/dle_login_failed.json")))
        .mount(&server)
        .await;

    let mut client = RezkaClient::new(config(base.clone())).unwrap();
    let error = client.ensure_authenticated(&credentials(), &probe(&base)).await.unwrap_err();
    let rendered = format!("{error:?}: {error}");

    assert!(rendered.contains("authentication failed"));
    assert!(!rendered.contains("rezka-password"));
    assert!(!rendered.contains("rezka-user"));
}

#[tokio::test]
async fn dle_http_200_success_with_session_cookie_reaches_final_valid_probe() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use wiremock::{Request, Respond};

    #[derive(Clone)]
    struct InvalidThenValid {
        calls: Arc<AtomicUsize>,
    }

    impl Respond for InvalidThenValid {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            match self.calls.fetch_add(1, Ordering::SeqCst) {
                0 => ResponseTemplate::new(200).set_body_string("<input name=\"login_name\">"),
                1 => {
                    let cookie = request.headers.get("cookie").unwrap().to_str().unwrap();
                    assert!(cookie.contains("PHPSESSID=from-json-success"));
                    ResponseTemplate::new(200)
                        .set_body_string("<html data-authenticated=\"true\"></html>")
                }
                _ => panic!("probe fetched more than twice"),
            }
        }
    }

    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(InvalidThenValid { calls: Arc::clone(&calls) })
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ajax/login/"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("set-cookie", "PHPSESSID=from-json-success; Path=/; HttpOnly")
                .set_body_string(include_str!("fixtures/dle_login_success.json")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut client = RezkaClient::new(config(base.clone())).unwrap();
    let result = client.ensure_authenticated(&credentials(), &probe(&base)).await.unwrap();

    assert_eq!(result, SessionValidation::Valid);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn dle_http_200_success_without_session_cookie_is_rejected() {
    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<input name=\"login_name\">"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ajax/login/"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_string(include_str!("fixtures/dle_login_success.json")))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = RezkaClient::new(config(base.clone())).unwrap();
    let error = client.ensure_authenticated(&credentials(), &probe(&base)).await.unwrap_err();

    assert_eq!(error.code(), rezka_client::RezkaErrorCode::ProviderResponseInvalid);
}

#[tokio::test]
async fn dle_http_200_success_rejects_preexisting_stale_session_cookie() {
    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<input name=\"login_name\">"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ajax/login/"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_string(include_str!("fixtures/dle_login_success.json")))
        .expect(1)
        .mount(&server)
        .await;

    let mut jar = SessionJar::empty();
    jar.store_response_cookies(["PHPSESSID=opaque-stale; Path=/; HttpOnly"].iter().copied(), &base);
    let snapshot = jar.export().unwrap();
    let mut client = RezkaClient::from_snapshot(config(base.clone()), &snapshot).unwrap();
    let error = client.ensure_authenticated(&credentials(), &probe(&base)).await.unwrap_err();
    let rendered = format!("{error:?}: {error}");

    assert_eq!(error.code(), rezka_client::RezkaErrorCode::ProviderResponseInvalid);
    assert!(!rendered.contains("opaque-stale"));
}

#[tokio::test]
async fn dle_http_302_rejects_preexisting_stale_session_cookie() {
    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<input name=\"login_name\">"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ajax/login/"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/"))
        .expect(1)
        .mount(&server)
        .await;

    let mut jar = SessionJar::empty();
    jar.store_response_cookies(["PHPSESSID=opaque-stale; Path=/; HttpOnly"].iter().copied(), &base);
    let snapshot = jar.export().unwrap();
    let mut client = RezkaClient::from_snapshot(config(base.clone()), &snapshot).unwrap();
    let error = client.ensure_authenticated(&credentials(), &probe(&base)).await.unwrap_err();
    let rendered = format!("{error:?}: {error}");

    assert_eq!(error.code(), rezka_client::RezkaErrorCode::ProviderResponseInvalid);
    assert!(!rendered.contains("opaque-stale"));
}

#[test]
fn validation_probe_rejects_empty_marker_classes_and_empty_markers() {
    let url = Url::parse("https://rezka.test/account/probe").unwrap();
    for (valid, invalid) in [
        (vec![], vec!["logged-out".to_owned()]),
        (vec!["logged-in".to_owned()], vec![]),
        (vec![String::new()], vec!["logged-out".to_owned()]),
        (vec!["logged-in".to_owned()], vec!["   ".to_owned()]),
    ] {
        assert!(SessionValidationProbe::new(url.clone(), valid, invalid).is_err());
    }
}

#[tokio::test]
async fn direct_client_api_rejects_unconfigured_probe_origin_before_any_request() {
    let configured = MockServer::start().await;
    let unconfigured = MockServer::start().await;
    let configured_origin = Url::parse(&configured.uri()).unwrap();
    let unconfigured_origin = Url::parse(&unconfigured.uri()).unwrap();
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(500))
        .expect(0).mount(&configured).await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(500))
        .expect(0).mount(&unconfigured).await;

    let probe = SessionValidationProbe::new(
        unconfigured_origin.join("/account/probe").unwrap(),
        vec!["valid-marker".to_owned()],
        vec!["invalid-marker".to_owned()],
    )
    .unwrap();
    let mut client = RezkaClient::new(config(configured_origin)).unwrap();

    let error = client.fetch_probe(&probe).await.unwrap_err();

    assert_eq!(error.code(), rezka_client::RezkaErrorCode::Configuration);
    assert_eq!(error.to_string(), "configuration invalid: probe origin is not a configured Rezka mirror");
}

#[tokio::test]
async fn response_with_both_valid_and_invalid_markers_is_inconclusive() {
    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(ResponseTemplate::new(200).set_body_string("logged-in logged-out"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ajax/login/"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let probe = SessionValidationProbe::new(
        base.join("/account/probe").unwrap(),
        vec!["logged-in".to_owned()],
        vec!["logged-out".to_owned()],
    )
    .unwrap();
    let mut client = RezkaClient::new(config(base)).unwrap();
    let error = client.ensure_authenticated(&credentials(), &probe).await.unwrap_err();

    assert_eq!(error.code(), rezka_client::RezkaErrorCode::ProviderResponseInvalid);
}

#[tokio::test]
async fn response_with_no_markers_is_inconclusive_and_never_sends_credentials() {
    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(ResponseTemplate::new(200).set_body_string("neutral account page"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ajax/login/"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let mut client = RezkaClient::new(config(base.clone())).unwrap();
    let error = client.ensure_authenticated(&credentials(), &probe(&base)).await.unwrap_err();

    assert_eq!(error.code(), rezka_client::RezkaErrorCode::ProviderResponseInvalid);
}
```

Use the public `SessionJar` snapshot path for tests that need prepared cookies;
do not expose a public manual browser-cookie import API on `RezkaClient` or the
CLI.

- [ ] **Step 3: Run the focused failing test**

Run:

```bash
cargo test -p rezka-client --test session_flow --locked
```

Expected: FAIL because `RezkaClient`, DLE login, and validation APIs do not
exist yet.

- [ ] **Step 4: Implement `RezkaCredentials` and secret-safe debug**

Implementation requirements:

```text
1. Use secrecy::SecretString for username and password.
2. Implement Debug manually and redact both fields.
3. Never put username or password into errors.
4. Use ExposeSecret only at the request-building boundary.
```

- [ ] **Step 5: Implement caller-supplied validation**

Implementation requirements:

```text
1. `SessionValidationProbe::new` requires an http(s) URL with no username/password and requires both marker vectors to be non-empty with no empty/whitespace-only member. Its path/query form the deployment-supplied probe contract.
2. Before rewrite, failover, DNS, or any HTTP request, `RezkaClient::fetch_probe` checks the probe URL with `MirrorSet::contains_origin` against the client's complete configured mirror list, not merely the selected mirror. An unconfigured origin returns `RezkaError::Configuration { message: "probe origin is not a configured Rezka mirror" }` and makes zero requests. `ensure_authenticated` reaches probes only through this checked path.
3. After membership succeeds, `fetch_probe` calls `Transport::get_first_with_failover` exactly once as a logical operation. Each attempt rewrites the probe path/query onto the currently selected mirror, and the returned `ProbeResponse.url` must equal the successful selected origin plus that path/query.
4. `classify_probe` evaluates only that fetched response. If both valid and invalid marker classes appear, return `Inconclusive`; if only an invalid marker appears, return `Invalid`; if only a valid marker appears, return `Valid`; otherwise return `Inconclusive`.
5. Never classify `Valid` merely because a valid marker appears when an invalid marker also appears.
6. Do not hard-code a Rezka validation endpoint or either marker class.
7. Do not treat premium CSS or premium labels as authentication proof.
```

- [ ] **Step 6: Implement DLE login**

Implementation requirements:

```text
1. POST /ajax/login/ on the selected mirror origin.
2. Use application/x-www-form-urlencoded.
3. Send X-Requested-With: XMLHttpRequest.
4. Send Referer: <mirror-origin>/.
5. Send stable User-Agent through Transport.
6. Send fields login_name, login_password, login_not_save=0, and login=submit.
7. Call `Transport::post_form_first`; the client-wide no-redirect policy guarantees direct inspection of the first response.
8. Treat 30x as success only when that exact `TransportResponse.stored_cookie_names()` contains `PHPSESSID`.
9. Treat HTTP 200 JSON {"success":true} as success only when that exact `TransportResponse.stored_cookie_names()` contains `PHPSESSID`.
10. Treat HTTP 200 JSON {"success":false,...} as AuthenticationFailed with sanitized context.
11. Treat 30x or HTTP 200 `{"success":true}` without current-response PHPSESSID metadata as ProviderResponseInvalid, even if the jar already contained PHPSESSID before login.
12. DLE code must not query the jar to decide login success. It may inspect only status, sanitized body classification, and the current response's cookie-name set; cookie values remain confined to the secret jar.
```

- [ ] **Step 7: Implement `RezkaClient::ensure_authenticated`**

State machine (three probe fetches in the full Anubis + login path, with no
duplicate validation GET at any state):

```text
1. Call `fetch_probe(probe)` once and retain the returned `ProbeResponse`.
2. Detect Anubis and classify validation from that same first response; do not issue a separate validation GET.
3. If it is not Anubis and classification is Valid, return Valid. If it is Inconclusive, return ProviderResponseInvalid immediately and do not construct or send a DLE form.
4. If it is Anubis, parse and solve that body, submit the pass request with `Transport::get_first`, then call `fetch_probe(probe)` once more. If this second body is still Anubis, return ChallengeFailed without another retry.
5. Classify the current response. If Valid, return Valid. If Inconclusive, return ProviderResponseInvalid without sending credentials. Only Invalid may run DLE login with `Transport::post_form_first`.
6. Call `fetch_probe(probe)` exactly once after login. If this body is Anubis, return ChallengeFailed; do not loop.
7. Classify the final response. If Valid, return Valid; if Invalid, return AuthenticationRequired; if Inconclusive, return ProviderResponseInvalid with fixed sanitized context.
8. Never call both `fetch_probe` and another validation helper for the same state transition.
9. Build `Transport` with `RezkaClientConfig.max_retries`; this field is the exact additional-attempt budget for harmless probe failover and is not used by Anubis or DLE requests.
```

Complete the Task 5 module and root exports only after the final types compile:

```rust
// crates/rezka-client/src/session/mod.rs
pub mod dle;
pub mod validation;

// crates/rezka-client/src/lib.rs
pub use session::{
    ProbeResponse, RezkaClient, RezkaClientConfig, RezkaCredentials, SessionValidation,
    SessionValidationProbe,
};
```

This proves automatic re-authentication against the test's deployment-supplied
probe contract without treating `probe.url` as a universal provider endpoint.

- [ ] **Step 8: Run focused session tests**

Run:

```bash
cargo test -p rezka-client --test session_flow --locked
```

Expected: PASS.

- [ ] **Step 9: Run task verification**

Run:

```bash
mise run check
mise run lint
mise run test
```

Expected: PASS.

- [ ] **Step 10: Commit**

```bash
git add crates/rezka-client/src/lib.rs crates/rezka-client/src/session/dle.rs crates/rezka-client/src/session/validation.rs crates/rezka-client/src/session/mod.rs crates/rezka-client/src/transport.rs crates/rezka-client/tests/session_flow.rs crates/rezka-client/tests/fixtures/dle_login_failed.json crates/rezka-client/tests/fixtures/dle_login_success.json
git commit -m "feat: add rezka dle session authentication"
```

## Task 6: Encrypted Runner-Owned Rezka Session Store

**Files:**
- Modify: `crates/media-runner/src/lib.rs`
- Modify: `crates/media-runner/src/rezka_session_store.rs`
- Create: `crates/media-runner/tests/rezka_session_store.rs`

**Interfaces:**
- Consumes `rezka_client::SessionSnapshot`.
- Produces encrypted-at-rest persistence for runner-owned Rezka cookie state.

```rust
pub struct RezkaSessionStoreConfig {
    pub path: std::path::PathBuf,
    pub key: secrecy::SecretBox<[u8; 32]>,
}

pub struct EncryptedRezkaSessionStore;
impl EncryptedRezkaSessionStore {
    pub fn new(config: RezkaSessionStoreConfig) -> Result<Self, RezkaSessionStoreError>;
    pub fn load(&self) -> Result<Option<rezka_client::SessionSnapshot>, RezkaSessionStoreError>;
    pub fn save(&self, snapshot: &rezka_client::SessionSnapshot) -> Result<(), RezkaSessionStoreError>;
    pub fn delete(&self) -> Result<(), RezkaSessionStoreError>;
}
```

Encrypted file format:

```json
{
  "version": 1,
  "nonce_b64": "<12 random bytes>",
  "ciphertext_b64": "<AES-256-GCM ciphertext + tag>"
}
```

Use AAD:

```text
media-orchestrator:rezka-session:v1
```

- [ ] **Step 1: Write failing encrypted-store tests**

Create `crates/media-runner/tests/rezka_session_store.rs`:

```rust
use base64::Engine as _;
use media_runner::{EncryptedRezkaSessionStore, RezkaSessionStoreConfig};
use rezka_client::session::SessionSnapshot;
use secrecy::SecretBox;
use tempfile::TempDir;

fn key(byte: u8) -> SecretBox<[u8; 32]> {
    SecretBox::new(Box::new([byte; 32]))
}

fn snapshot(byte: u8) -> SessionSnapshot {
    SessionSnapshot::from_secret_bytes(SecretBox::new(Box::new(vec![byte; 64])))
}

#[test]
fn encrypted_store_round_trips_without_plaintext_on_disk() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("rezka-session.bin");
    let store = EncryptedRezkaSessionStore::new(RezkaSessionStoreConfig {
        path: path.clone(),
        key: key(7),
    })
    .unwrap();

    let original = snapshot(0x11);
    store.save(&original).unwrap();
    let disk = std::fs::read_to_string(&path).unwrap();

    assert!(disk.contains("\"version\":1"));
    assert!(!disk.contains(&base64::engine::general_purpose::STANDARD.encode(vec![0x11; 64])));

    let restored = store.load().unwrap().unwrap();
    assert!(restored.secret_eq(&original));
}

#[test]
fn repeated_saves_use_distinct_envelopes_and_overwrite_with_latest_snapshot() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("rezka-session.bin");
    let store = EncryptedRezkaSessionStore::new(RezkaSessionStoreConfig {
        path: path.clone(),
        key: key(7),
    })
    .unwrap();

    store.save(&snapshot(0x11)).unwrap();
    let first: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    store.save(&snapshot(0x22)).unwrap();
    let second: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();

    assert_ne!(first["nonce_b64"], second["nonce_b64"]);
    assert_ne!(first["ciphertext_b64"], second["ciphertext_b64"]);
    assert!(store.load().unwrap().unwrap().secret_eq(&snapshot(0x22)));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn wrong_key_and_corrupt_envelope_return_only_sanitized_errors() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("rezka-session.bin");
    EncryptedRezkaSessionStore::new(RezkaSessionStoreConfig {
        path: path.clone(),
        key: key(7),
    })
    .unwrap()
    .save(&snapshot(0x33))
    .unwrap();

    let wrong_key_store = EncryptedRezkaSessionStore::new(RezkaSessionStoreConfig {
        path: path.clone(),
        key: key(8),
    })
    .unwrap();
    let error = wrong_key_store.load().unwrap_err();
    let rendered = format!("{error:?}: {error}");
    assert_eq!(rendered, "DecryptionFailed: encrypted Rezka session could not be decrypted");

    std::fs::write(&path, b"{not-an-envelope").unwrap();
    let corrupt = wrong_key_store.load().unwrap_err();
    let rendered = format!("{corrupt:?}: {corrupt}");
    assert_eq!(rendered, "InvalidEnvelope: encrypted Rezka session envelope is invalid");
}

#[cfg(unix)]
#[test]
fn encrypted_store_uses_restrictive_unix_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("rezka-session.bin");
    let store = EncryptedRezkaSessionStore::new(RezkaSessionStoreConfig {
        path: path.clone(),
        key: key(7),
    })
    .unwrap();

    store.save(&snapshot(0x44)).unwrap();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}
```

- [ ] **Step 2: Run the focused failing test**

Run:

```bash
cargo test -p media-runner --test rezka_session_store --locked
```

Expected: FAIL because encrypted store APIs do not exist yet.

- [ ] **Step 3: Implement encrypted store**

Implementation requirements:

```text
1. Use aes_gcm::Aes256Gcm.
2. Generate a fresh 96-bit nonce for every save with rand::rngs::OsRng.
3. Use the AAD exactly as specified above.
4. Serialize only the encrypted envelope to disk.
5. load returns Ok(None) when the file is absent.
6. `save` creates `tempfile::NamedTempFile` in `path.parent()`, sets mode 0600 before writing on Unix, writes the complete envelope, calls `as_file_mut().sync_all()`, atomically replaces the destination with `persist(path)`, then opens and `sync_all()`s the parent directory before returning success. Propagate every failure as a static sanitized error.
7. delete removes the file and treats NotFound as success.
8. Debug and Display for all errors are sanitized.
9. Call `SessionSnapshot::with_secret_bytes` only around encryption. Copy into a mutable temporary buffer only if required by the AEAD API and call `zeroize::Zeroize::zeroize` immediately after encrypt/decrypt processing; never persist plaintext snapshot bytes outside process memory.
10. Repeated saves must generate distinct nonces/envelopes and load the newest snapshot after atomic overwrite. No successful save may leave a temporary file in the session directory.
```

Use this exact durability order in the implementation:

```rust
let parent = config.path.parent().ok_or(RezkaSessionStoreError::InvalidPath)?;
let mut temporary = tempfile::NamedTempFile::new_in(parent)
    .map_err(|_| RezkaSessionStoreError::WriteFailed)?;
#[cfg(unix)]
temporary
    .as_file()
    .set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))
    .map_err(|_| RezkaSessionStoreError::WriteFailed)?;
std::io::Write::write_all(temporary.as_file_mut(), &envelope_bytes)
    .map_err(|_| RezkaSessionStoreError::WriteFailed)?;
temporary
    .as_file_mut()
    .sync_all()
    .map_err(|_| RezkaSessionStoreError::WriteFailed)?;
temporary
    .persist(&config.path)
    .map_err(|_| RezkaSessionStoreError::WriteFailed)?;
#[cfg(unix)]
std::fs::File::open(parent)
    .and_then(|directory| directory.sync_all())
    .map_err(|_| RezkaSessionStoreError::WriteFailed)?;
```

After the store types compile, add their root re-export:

```rust
// crates/media-runner/src/lib.rs
pub use rezka_session_store::{
    EncryptedRezkaSessionStore, RezkaSessionStoreConfig, RezkaSessionStoreError,
};
```

- [ ] **Step 4: Run focused store tests**

Run:

```bash
cargo test -p media-runner --test rezka_session_store --locked
```

Expected: PASS.

- [ ] **Step 5: Run task verification**

Run:

```bash
mise run check
mise run lint
mise run test
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/media-runner/src/lib.rs crates/media-runner/src/rezka_session_store.rs crates/media-runner/tests/rezka_session_store.rs
git commit -m "feat: encrypt rezka runner session store"
```

## Task 7: Composition Secret Configuration and Construction Wiring

**Files:**
- Modify: `crates/media/src/config.rs`
- Modify: `crates/media/src/composition.rs`
- Modify: `crates/media/tests/config.rs`
- Create: `crates/media/tests/rezka_composition.rs`

**Interfaces:**
- Consumes `rezka-client` and `media-runner` constructors.
- Produces config-only wiring required for runner startup and VPN-rotation re-authentication in later phases.
- Does not add a runner loop, provider catalog call, playback call, download call, or database call.

```rust
pub struct RunnerConfig {
    service: ClientConfig,
    rezka: RezkaCompositionConfig,
}
impl RunnerConfig {
    pub fn service(&self) -> &ClientConfig;
    pub fn rezka(&self) -> &RezkaCompositionConfig;
}

pub struct RezkaCompositionConfig {
    mirrors: Vec<url::Url>,
    session_probe_url: url::Url,
    session_valid_markers: Vec<String>,
    session_invalid_markers: Vec<String>,
    username: secrecy::SecretString,
    password: secrecy::SecretString,
    cookie_key: secrecy::SecretBox<[u8; 32]>,
    session_store_path: std::path::PathBuf,
    user_agent: String,
}
impl RezkaCompositionConfig {
    pub fn mirrors(&self) -> &[url::Url];
    pub fn session_probe_url(&self) -> &url::Url;
    pub fn session_valid_markers(&self) -> &[String];
    pub fn session_invalid_markers(&self) -> &[String];
    pub fn username(&self) -> &secrecy::SecretString;
    pub fn password(&self) -> &secrecy::SecretString;
    pub fn cookie_key(&self) -> &secrecy::SecretBox<[u8; 32]>;
    pub fn session_store_path(&self) -> &std::path::Path;
    pub fn user_agent(&self) -> &str;
}

pub struct PreparedRunnerSession {
    pub client: rezka_client::RezkaClient,
    pub credentials: rezka_client::RezkaCredentials,
    pub probe: rezka_client::SessionValidationProbe,
    pub store: media_runner::EncryptedRezkaSessionStore,
}

pub fn prepare_runner_session(config: &RunnerConfig) -> Result<PreparedRunnerSession, RunnerCompositionError>;
```

Required environment and secret-file settings:

```text
MEDIA_SERVICE_URL
MEDIA_TOKEN_FILE
MEDIA_REZKA_MIRRORS
MEDIA_REZKA_SESSION_PROBE_URL
MEDIA_REZKA_SESSION_VALID_MARKERS_JSON
MEDIA_REZKA_SESSION_INVALID_MARKERS_JSON
MEDIA_REZKA_USERNAME_FILE
MEDIA_REZKA_PASSWORD_FILE
MEDIA_REZKA_COOKIE_KEY_FILE
MEDIA_REZKA_SESSION_STORE_FILE
MEDIA_REZKA_USER_AGENT optional; default "media-orchestrator/0.1 rezka-session"
```

Secret file validation:

```text
MEDIA_REZKA_USERNAME_FILE: non-empty UTF-8 after one final newline trim, max 256 visible ASCII bytes.
MEDIA_REZKA_PASSWORD_FILE: non-empty UTF-8 after one final newline trim, max 1024 bytes, no NUL.
MEDIA_REZKA_COOKIE_KEY_FILE: base64 for exactly 32 decoded bytes after one final newline trim.
```

- [ ] **Step 1: Write failing config tests**

Extend `crates/media/tests/config.rs`:

```rust
use base64::{engine::general_purpose::STANDARD, Engine as _};

#[test]
fn runner_config_loads_rezka_secret_files_and_redacts_debug() {
    let mut source = FakeSource::default();
    source.set_env("MEDIA_SERVICE_URL", "https://media.internal.example");
    source.set_secret("MEDIA_TOKEN_FILE", b"runner-token\n");
    source.set_env("MEDIA_REZKA_MIRRORS", "https://rezka.test,https://rezka-alt.test");
    source.set_env("MEDIA_REZKA_SESSION_PROBE_URL", "https://rezka.test/account/probe");
    source.set_env("MEDIA_REZKA_SESSION_VALID_MARKERS_JSON", r#"["account-menu","logout-link"]"#);
    source.set_env("MEDIA_REZKA_SESSION_INVALID_MARKERS_JSON", r#"["login-form","login_name"]"#);
    source.set_env("MEDIA_REZKA_SESSION_STORE_FILE", "/runner/rezka/session.bin");
    source.set_secret("MEDIA_REZKA_USERNAME_FILE", b"rezka-user\n");
    source.set_secret("MEDIA_REZKA_PASSWORD_FILE", b"rezka-password\n");
    source.set_secret("MEDIA_REZKA_COOKIE_KEY_FILE", STANDARD.encode([7_u8; 32]).as_bytes());

    let config = media::config::RunnerConfig::load_from(&source).unwrap();
    let debug = format!("{config:?}");
    let encoded_key = STANDARD.encode([7_u8; 32]);

    assert_eq!(config.rezka().mirrors().len(), 2);
    assert_eq!(config.rezka().session_probe_url().as_str(), "https://rezka.test/account/probe");
    assert_eq!(config.rezka().session_valid_markers(), ["account-menu", "logout-link"]);
    assert_eq!(config.rezka().session_invalid_markers(), ["login-form", "login_name"]);
    for forbidden in ["runner-token", "rezka-user", "rezka-password", encoded_key.as_str()] {
        assert!(!debug.contains(forbidden), "debug output exposed {forbidden}");
    }
}

#[test]
fn runner_config_rejects_credentialed_mirror_in_isolation() {
    let mut source = valid_runner_source();
    source.set_env("MEDIA_REZKA_MIRRORS", "https://user:pass@rezka.test");

    let error = media::config::RunnerConfig::load_from(&source).unwrap_err();
    let rendered = format!("{error:?}: {error}");
    assert_eq!(error.to_string(), "configuration invalid: invalid Rezka mirror origin");
    assert!(!rendered.contains("user:pass"));
}

#[test]
fn runner_config_rejects_queried_probe_in_isolation() {
    let mut source = valid_runner_source();
    source.set_env("MEDIA_REZKA_SESSION_PROBE_URL", "https://rezka.test/account/probe?token=secret");

    let error = media::config::RunnerConfig::load_from(&source).unwrap_err();
    let rendered = format!("{error:?}: {error}");
    assert_eq!(error.to_string(), "configuration invalid: invalid Rezka session probe URL");
    assert!(!rendered.contains("token=secret"));
}

#[test]
fn runner_config_rejects_malformed_cookie_key_base64_in_isolation() {
    let mut source = valid_runner_source();
    source.set_secret("MEDIA_REZKA_COOKIE_KEY_FILE", b"not-base64-secret");

    let error = media::config::RunnerConfig::load_from(&source).unwrap_err();
    let rendered = format!("{error:?}: {error}");
    assert_eq!(error.to_string(), "configuration invalid: Rezka cookie key must be base64");
    assert!(!rendered.contains("not-base64-secret"));
}

#[test]
fn runner_config_rejects_wrong_decoded_cookie_key_length_in_isolation() {
    let mut source = valid_runner_source();
    let short_key = STANDARD.encode([8_u8; 31]);
    source.set_secret("MEDIA_REZKA_COOKIE_KEY_FILE", short_key.as_bytes());

    let error = media::config::RunnerConfig::load_from(&source).unwrap_err();
    let rendered = format!("{error:?}: {error}");
    assert_eq!(error.to_string(), "configuration invalid: Rezka cookie key must decode to 32 bytes");
    assert!(!rendered.contains(&short_key));
}

#[test]
fn runner_config_rejects_missing_empty_or_blank_validation_marker_classes() {
    for (valid, invalid) in [
        ("[]", r#"["login-form"]"#),
        (r#"["account-menu"]"#, "[]"),
        (r#"[""]"#, r#"["login-form"]"#),
        (r#"["account-menu"]"#, r#"["   "]"#),
    ] {
        let mut source = valid_runner_source();
        source.set_env("MEDIA_REZKA_SESSION_VALID_MARKERS_JSON", valid);
        source.set_env("MEDIA_REZKA_SESSION_INVALID_MARKERS_JSON", invalid);

        let error = media::config::RunnerConfig::load_from(&source).unwrap_err();
        assert_eq!(error.to_string(), "configuration invalid: Rezka validation markers must be non-empty");
    }
}
```

Add this helper beside the existing `FakeSource` helpers:

```rust
fn valid_runner_source() -> FakeSource {
    let mut source = FakeSource::default();
    let encoded_key = STANDARD.encode([7_u8; 32]);
    source.set_env("MEDIA_SERVICE_URL", "https://media.internal.example");
    source.set_secret("MEDIA_TOKEN_FILE", b"runner-token");
    source.set_env("MEDIA_REZKA_MIRRORS", "https://rezka.test");
    source.set_env("MEDIA_REZKA_SESSION_PROBE_URL", "https://rezka.test/account/probe");
    source.set_env("MEDIA_REZKA_SESSION_VALID_MARKERS_JSON", r#"["account-menu"]"#);
    source.set_env("MEDIA_REZKA_SESSION_INVALID_MARKERS_JSON", r#"["login-form"]"#);
    source.set_env("MEDIA_REZKA_SESSION_STORE_FILE", "/runner/rezka/session.bin");
    source.set_secret("MEDIA_REZKA_USERNAME_FILE", b"rezka-user");
    source.set_secret("MEDIA_REZKA_PASSWORD_FILE", b"rezka-password");
    source.set_secret("MEDIA_REZKA_COOKIE_KEY_FILE", encoded_key.as_bytes());
    source
}
```

- [ ] **Step 2: Run the focused failing config tests**

Run:

```bash
cargo test -p media --test config runner_config_ --locked
```

Expected: FAIL because `RunnerConfig` and Rezka config do not exist yet.

- [ ] **Step 3: Implement config loading**

Implementation requirements:

```text
1. Reuse the existing ConfigSource pattern.
2. Reuse existing one-final-newline trimming behavior.
3. Do not accept credentials or cookie keys from command-line args.
4. Require `MEDIA_REZKA_SESSION_PROBE_URL`, `MEDIA_REZKA_SESSION_VALID_MARKERS_JSON`, and `MEDIA_REZKA_SESSION_INVALID_MARKERS_JSON`; document that all three are deployment supplied and do not describe a proven generic provider endpoint.
5. Reject probe URLs with credentials, query, or fragment.
6. Require probe origin to match one configured mirror origin.
7. Reject mirror origins with credentials, paths, queries, or fragments.
8. Parse each marker setting as a JSON array of strings. Require both arrays to be non-empty and reject empty/whitespace-only members; do not supply defaults.
9. Decode MEDIA_REZKA_COOKIE_KEY_FILE with base64 and require exactly 32 bytes.
10. Implement Debug manually and redact service URL, tokens, username, password, cookie key, session store path, probe URL, and marker contents.
11. Implement every getter declared in the Task 7 interface with the exact return type shown there; tests and `prepare_runner_session` use getters exclusively and never access private fields directly.
```

- [ ] **Step 4: Write failing composition construction test**

Create `crates/media/tests/rezka_composition.rs`:

```rust
use base64::{engine::general_purpose::STANDARD, Engine as _};
use media::config::{ConfigSource, RunnerConfig};
use std::{
    collections::HashMap,
    ffi::OsString,
    io,
    path::{Path, PathBuf},
};

#[derive(Default)]
struct FakeSource {
    env: HashMap<&'static str, OsString>,
    files: HashMap<PathBuf, Vec<u8>>,
}

impl FakeSource {
    fn set_env(&mut self, name: &'static str, value: impl Into<OsString>) {
        self.env.insert(name, value.into());
    }

    fn set_secret(&mut self, name: &'static str, value: &[u8]) {
        let path = PathBuf::from(format!("/{name}.secret"));
        self.set_env(name, path.as_os_str());
        self.files.insert(path, value.to_vec());
    }
}

impl ConfigSource for FakeSource {
    fn var_os(&self, name: &'static str) -> Option<OsString> {
        self.env.get(name).cloned()
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing"))
    }
}

#[test]
fn composition_constructs_rezka_session_dependencies_without_running_provider_calls() {
    let mut source = FakeSource::default();
    source.set_env("MEDIA_SERVICE_URL", "https://media.internal.example");
    source.set_secret("MEDIA_TOKEN_FILE", b"runner-token\n");
    source.set_env("MEDIA_REZKA_MIRRORS", "https://rezka.test");
    source.set_env("MEDIA_REZKA_SESSION_PROBE_URL", "https://rezka.test/account/probe");
    source.set_env("MEDIA_REZKA_SESSION_VALID_MARKERS_JSON", r#"["account-menu"]"#);
    source.set_env("MEDIA_REZKA_SESSION_INVALID_MARKERS_JSON", r#"["login-form"]"#);
    source.set_env("MEDIA_REZKA_SESSION_STORE_FILE", "/tmp/rezka-session.bin");
    source.set_secret("MEDIA_REZKA_USERNAME_FILE", b"rezka-user");
    source.set_secret("MEDIA_REZKA_PASSWORD_FILE", b"rezka-password");
    source.set_secret("MEDIA_REZKA_COOKIE_KEY_FILE", STANDARD.encode([9_u8; 32]).as_bytes());

    let config = RunnerConfig::load_from(&source).unwrap();
    let prepared = media::composition::prepare_runner_session(&config).unwrap();
    let debug = format!("{prepared:?}");

    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("rezka-user"));
    assert!(!debug.contains("rezka-password"));
}
```

- [ ] **Step 5: Run the focused failing composition test**

Run:

```bash
cargo test -p media --test rezka_composition --locked
```

Expected: FAIL because `prepare_runner_session` does not exist yet.

- [ ] **Step 6: Implement construction-only composition**

Implementation requirements:

```text
1. Construct RezkaClientConfig from mirrors, user-agent, timeout 30s, max_retries 2, and anubis_max_nonce 5_000_000.
2. Construct RezkaCredentials from username/password secrets.
3. Construct `SessionValidationProbe::new` from the deployment-supplied probe URL, valid markers, and invalid markers without adding or replacing any marker.
4. Construct EncryptedRezkaSessionStore from session-store path and cookie key.
5. Do not call `fetch_probe`, `ensure_authenticated`, DLE login, Anubis pass, catalog, playback, download, API, or database code.
6. Implement Debug for PreparedRunnerSession manually and redact all fields.
```

- [ ] **Step 7: Run focused config and composition tests**

Run:

```bash
cargo test -p media --test config runner_config_ --locked
cargo test -p media --test rezka_composition --locked
```

Expected: PASS.

- [ ] **Step 8: Run task verification**

Run:

```bash
mise run check
mise run lint
mise run test
```

Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/media/src/config.rs crates/media/src/composition.rs crates/media/tests/config.rs crates/media/tests/rezka_composition.rs
git commit -m "feat: wire rezka session secrets in composition"
```

## Task 8: Ignored Live Probe and Final Security Regression Pass

**Files:**
- Create: `crates/rezka-client/tests/live_probe.rs`
- Modify: `crates/rezka-client/tests/session_flow.rs`
- Modify: `crates/rezka-client/tests/redaction.rs`
- Modify: `crates/media-runner/tests/rezka_session_store.rs`

**Interfaces:**
- Produces an opt-in live probe that is never part of normal CI.
- Strengthens fixture/mock security coverage before Phase 3 closes.

- [ ] **Step 1: Add the ignored live probe**

Create `crates/rezka-client/tests/live_probe.rs`:

```rust
#[tokio::test]
#[ignore = "requires REZKA_LIVE_PROBE=1 and real Rezka secret files; never run in normal CI"]
async fn live_probe_rezka_session_authentication_contract() {
    let opt_in = std::env::var("REZKA_LIVE_PROBE")
        .expect("explicit live probe requires REZKA_LIVE_PROBE=1");
    assert_eq!(opt_in, "1", "explicit live probe requires REZKA_LIVE_PROBE=1");

    let mirror = std::env::var("REZKA_LIVE_MIRROR").expect("REZKA_LIVE_MIRROR is required");
    let probe_url = std::env::var("REZKA_LIVE_SESSION_PROBE_URL")
        .expect("REZKA_LIVE_SESSION_PROBE_URL is required");
    let valid_markers: Vec<String> = serde_json::from_str(
        &std::env::var("REZKA_LIVE_SESSION_VALID_MARKERS_JSON")
            .expect("REZKA_LIVE_SESSION_VALID_MARKERS_JSON is required"),
    )
    .expect("REZKA_LIVE_SESSION_VALID_MARKERS_JSON must be a JSON string array");
    let invalid_markers: Vec<String> = serde_json::from_str(
        &std::env::var("REZKA_LIVE_SESSION_INVALID_MARKERS_JSON")
            .expect("REZKA_LIVE_SESSION_INVALID_MARKERS_JSON is required"),
    )
    .expect("REZKA_LIVE_SESSION_INVALID_MARKERS_JSON must be a JSON string array");
    let username_file = std::env::var("REZKA_LIVE_USERNAME_FILE")
        .expect("REZKA_LIVE_USERNAME_FILE is required");
    let password_file = std::env::var("REZKA_LIVE_PASSWORD_FILE")
        .expect("REZKA_LIVE_PASSWORD_FILE is required");

    let username = std::fs::read_to_string(username_file).expect("username file is readable");
    let password = std::fs::read_to_string(password_file).expect("password file is readable");

    let config = rezka_client::session::RezkaClientConfig {
        mirrors: rezka_client::MirrorSet::new(vec![url::Url::parse(&mirror).unwrap()]).unwrap(),
        user_agent: "media-orchestrator-live-probe".to_owned(),
        request_timeout: time::Duration::seconds(30),
        max_retries: 1,
        anubis_max_nonce: 5_000_000,
    };
    let probe = rezka_client::session::SessionValidationProbe::new(
        url::Url::parse(&probe_url).unwrap(),
        valid_markers,
        invalid_markers,
    )
    .unwrap();
    let credentials = rezka_client::session::RezkaCredentials {
        username: secrecy::SecretString::from(username.trim_end_matches(&['\r', '\n'][..]).to_owned()),
        password: secrecy::SecretString::from(password.trim_end_matches(&['\r', '\n'][..]).to_owned()),
    };

    let mut client = rezka_client::session::RezkaClient::new(config).unwrap();
    let validation = client.ensure_authenticated(&credentials, &probe).await.unwrap();

    assert_eq!(validation, rezka_client::session::SessionValidation::Valid);
}

#[tokio::test]
async fn live_equivalent_client_path_rejects_probe_outside_configured_mirrors() {
    let configured_mirror = url::Url::parse("https://configured-rezka.invalid").unwrap();
    let unconfigured_probe = url::Url::parse("https://unconfigured-rezka.invalid/account/probe").unwrap();
    let config = rezka_client::session::RezkaClientConfig {
        mirrors: rezka_client::MirrorSet::new(vec![configured_mirror]).unwrap(),
        user_agent: "media-orchestrator-live-probe-test".to_owned(),
        request_timeout: time::Duration::seconds(1),
        max_retries: 0,
        anubis_max_nonce: 1,
    };
    let probe = rezka_client::session::SessionValidationProbe::new(
        unconfigured_probe,
        vec!["deployment-valid-marker".to_owned()],
        vec!["deployment-invalid-marker".to_owned()],
    )
    .unwrap();
    let mut client = rezka_client::session::RezkaClient::new(config).unwrap();

    let error = client.fetch_probe(&probe).await.unwrap_err();

    assert_eq!(error.code(), rezka_client::RezkaErrorCode::Configuration);
    assert_eq!(error.to_string(), "configuration invalid: probe origin is not a configured Rezka mirror");
}
```

Do not add the credentialed ignored test to CI. The existing
`cargo nextest run --workspace --all-features` path does not run ignored tests.
The non-ignored `live_equivalent_client_path_rejects_probe_outside_configured_mirrors`
test remains in normal CI because it performs no network request; only the
credentialed live test is ignored.

Manual opt-in command:

```bash
REZKA_LIVE_PROBE=1 \
REZKA_LIVE_MIRROR=https://example-rezka-mirror.invalid \
REZKA_LIVE_SESSION_PROBE_URL=https://example-rezka-mirror.invalid/account/probe \
REZKA_LIVE_SESSION_VALID_MARKERS_JSON='["deployment-valid-marker"]' \
REZKA_LIVE_SESSION_INVALID_MARKERS_JSON='["deployment-invalid-marker"]' \
REZKA_LIVE_USERNAME_FILE=/run/secrets/rezka_username \
REZKA_LIVE_PASSWORD_FILE=/run/secrets/rezka_password \
cargo test -p rezka-client --test live_probe -- --ignored --nocapture
```

- [ ] **Step 2: Add bounded-flow and encrypted-store regressions**

Add these exact tests to `crates/rezka-client/tests/session_flow.rs` using the
`config`, `credentials`, and `probe` helpers from Task 5:

```rust
#[tokio::test]
async fn repeated_anubis_after_pass_returns_challenge_failed_without_login_or_loop() {
    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_string(include_str!("fixtures/anubis_challenge.html")))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/.within.website/x/cmd/anubis/api/pass-challenge"))
        .respond_with(ResponseTemplate::new(302)
            .insert_header("set-cookie", "anubis=opaque; Path=/"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ajax/login/"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let mut client = RezkaClient::new(config(base.clone())).unwrap();
    let error = client.ensure_authenticated(&credentials(), &probe(&base)).await.unwrap_err();
    assert_eq!(error.code(), rezka_client::RezkaErrorCode::ChallengeFailed);
}

#[tokio::test]
async fn dle_redirect_without_session_cookie_is_provider_response_invalid() {
    let server = MockServer::start().await;
    let base = Url::parse(&server.uri()).unwrap();
    Mock::given(method("GET"))
        .and(path("/account/probe"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<input name=\"login_name\">"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ajax/login/"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/"))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = RezkaClient::new(config(base.clone())).unwrap();
    let error = client.ensure_authenticated(&credentials(), &probe(&base)).await.unwrap_err();
    assert_eq!(error.code(), rezka_client::RezkaErrorCode::ProviderResponseInvalid);
}
```

Add this exact test to `crates/media-runner/tests/rezka_session_store.rs`:

```rust
#[test]
fn encrypted_store_delete_removes_file_and_missing_delete_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("rezka-session.bin");
    let store = EncryptedRezkaSessionStore::new(RezkaSessionStoreConfig {
        path: path.clone(),
        key: key(7),
    })
    .unwrap();
    store.save(&snapshot(0x55)).unwrap();
    assert!(path.exists());

    store.delete().unwrap();
    assert!(!path.exists());
    store.delete().unwrap();
}
```

The remaining security behavior is already exercised by exact earlier tests:
`response_with_both_valid_and_invalid_markers_is_inconclusive`,
`transport_rejects_unrelated_and_same_site_cross_origin_cookie_targets`,
`sanitizes_headers_assignments_json_inline_urls_ips_and_raw_provider_text`,
`runner_config_loads_rezka_secret_files_and_redacts_debug`, and
`composition_constructs_rezka_session_dependencies_without_running_provider_calls`.
In each constructor test, render every constructed type that implements Debug
and use this assertion body:

```rust
for forbidden in ["rezka-user", "rezka-password", "opaque-a", "opaque-b"] {
    assert!(!debug.contains(forbidden), "Debug leaked {forbidden}");
}
```

- [ ] **Step 3: Run focused security tests**

Run:

```bash
cargo test -p rezka-client --test redaction --locked
cargo test -p rezka-client --test mirror_cookie_origin --locked
cargo test -p rezka-client --test anubis --locked
cargo test -p rezka-client --test session_flow --locked
cargo test -p media-runner --test rezka_session_store --locked
cargo test -p media --test config runner_config_ --locked
cargo test -p media --test rezka_composition --locked
```

Expected: PASS.

- [ ] **Step 4: Prove the live probe remains excluded from normal test runs**

Run:

```bash
cargo test -p rezka-client --test live_probe --locked
```

Expected: PASS with the ignored test reported as ignored, not executed. Also
run the explicit negative guard once:

```bash
env -u REZKA_LIVE_PROBE cargo test -p rezka-client --test live_probe -- --ignored --nocapture
```

Expected: FAIL with `explicit live probe requires REZKA_LIVE_PROBE=1`. This
negative command is a local contract check and MUST NOT be added to normal CI.

- [ ] **Step 5: Run final workspace verification**

Run:

```bash
mise run check
mise run lint
mise run test
```

Expected: PASS.

- [ ] **Step 6: Confirm Phase 3 scope did not expand**

Run:

```bash
rg -n "catalog|playback|download|subtitle|episode|translation|get_cdn|ffmpeg|plex|prowlarr|qbittorrent" crates/rezka-client crates/media-runner crates/media/src crates/media/tests
```

Expected: no new Phase 4+ behavior in `rezka-client`, `media-runner`, or
`media` beyond existing pre-Phase-3 words already present in unrelated CLI/API
code. If a match points to a new Phase 3 file, remove the behavior and replace
it with a narrower session/auth-only interface.

- [ ] **Step 7: Commit**

```bash
git add crates/rezka-client/tests/live_probe.rs crates/rezka-client/tests/session_flow.rs crates/rezka-client/tests/redaction.rs crates/media-runner/tests/rezka_session_store.rs
git commit -m "test: add rezka session live probe guardrails"
```

## Phase 3 Exit Gate

Phase 3 is complete only when all statements below are true:

```text
1. A restored SessionSnapshot is classified from one fetched caller-supplied ProbeResponse and skips Anubis/DLE when that response is valid; no state performs a duplicate validation GET.
2. Task 1 builds with module-only stubs, refreshes and inspects Cargo.lock before any post-manifest `--locked` command, and every later root re-export is added only in the task that defines the exported type.
3. The full mock flow serves Anubis HTML, verifies pass-challenge id/nonce/response/redir/elapsedTime and the returned Anubis cookie, then performs DLE login and exactly one final probe.
4. Inconclusive validation, including both-marker and no-marker responses, returns ProviderResponseInvalid with `/ajax/login/` called zero times; only Invalid may send DLE credentials.
5. DLE login and Anubis pass inspect their first response directly through a client-wide `Policy::none()` transport; only the bounded explicit follower follows redirects and stores Set-Cookie at every hop.
6. Harmless probe GETs fail over from an unreachable selected mirror to the next configured mirror within `max_retries`; path/query are rebound to the successful selected origin, while Anubis and DLE never fail over. With `max_retries=0`, the second mirror receives zero requests, selection does not change, and the typed Transport error is returned.
7. Every public client probe path rejects an origin absent from the complete configured MirrorSet before rewrite or network I/O; direct-client and live-equivalent tests cover the rejection.
8. DLE success requires `PHPSESSID` in the current login response's cookie-name metadata. HTTP 200 and 302 responses without current Set-Cookie are rejected even when the jar contains a stale PHPSESSID, and no cookie value is exposed outside the jar.
9. Both DLE HTTP 200 success branches are tested: `success:true` plus current-response PHPSESSID reaches the final valid probe, while the same JSON without it is rejected.
10. RunnerConfig and RezkaCompositionConfig tests use the exact declared getters; credentialed mirror, queried probe, malformed base64, and wrong decoded key length each start from a valid base and assert their own static redacted error.
11. Mirror rewrite preserves path/query, and Transport rejects cookie-bearing requests unless scheme/host/effective port exactly match the selected mirror origin.
12. Site cookies are not forwarded to unrelated CDN hosts, same-site subdomain CDN hosts, alternate ports, or scheme-changed URLs, even when a Domain cookie would match.
13. There is no hard-coded generic Rezka validation endpoint.
14. Cookie import/export exists only as an internal SessionSnapshot API and is not exposed as browser/manual fallback.
15. Snapshot export includes unexpired persistent and non-persistent cookies; restore filters expired cookies and exposes no direct plaintext-returning accessor.
16. The runner store persists only AES-GCM encrypted session bytes and completes same-directory 0600 temp write, file sync, atomic replacement, and parent-directory sync; repeated saves use distinct nonces and load the newest envelope.
17. Rezka username, password, cookie key, cookies, Authorization values, signed/inline URL queries, IPv4/IPv6 literals, and raw provider snippets are absent from Display, Debug, tracing, committed fixtures, and test snapshots.
18. Probe URL plus non-empty valid and invalid marker classes are deployment supplied; both classes present yields Inconclusive, never Valid.
19. The ignored live probe is excluded from CI, succeeds only with `REZKA_LIVE_PROBE=1`, and fails when explicitly invoked without that exact opt-in.
20. No catalog, playback, subtitle, episode, translation, download, Plex, Prowlarr, qBittorrent, or PostgreSQL behavior was added.
21. `mise run check`, `mise run lint`, and `mise run test` pass.
```

## Implementation Notes

- Use mock-server and fixture tests as the source of truth for this phase.
- Treat `/ajax/login/` and the Anubis pass path as locally observed behavior, not a public protocol guarantee.
- Treat session validation as a deployment-supplied harmless probe contract. The plan intentionally does not name a universal Rezka validation endpoint.
- Keep live-provider experiments out of normal CI and out of committed fixtures.
- Do not update deployment files in this phase unless a later focused deployment plan explicitly asks for Docker secret wiring.
