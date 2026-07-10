# Rust Domain Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create a pinned, testable Rust workspace that establishes the media domain, transport-contract boundary, composition binary, architecture guardrails, and CI without introducing persistence or provider I/O.

**Architecture:** Start with three crates: pure `media-core`, serialization-only `media-contract`, and the `media` composition root. Domain identifiers and state transitions live in `media-core`; HTTP/CLI DTO shapes live in `media-contract`; only the binary knows both. PostgreSQL, Axum, Reqwest, Rezka, filesystem, and process execution are deliberately absent from this phase.

**Tech Stack:** Rust 1.97.0, Cargo edition 2024, uuid 1.23.4, thiserror 2.0.18, Serde 1.0.228, serde_json 1.0.150, Clap 4.6.1, cargo_metadata 0.23.1, assert_cmd 2.2.2, mise 2026.6+, cargo-nextest 0.9.140, cargo-deny 0.20.2, cargo-audit 0.22.2.

## Global Constraints

- Follow `docs/superpowers/specs/2026-07-10-media-orchestrator-mvp-design.md` and `docs/ARCHITECTURE.md`.
- `media-core` MUST have no dependency on Serde, Tokio, Axum, SeaORM, Reqwest, JSON, environment variables, filesystem, ffmpeg, or provider models.
- Domain IDs and transport IDs MUST remain distinct types.
- Domain types MUST NOT derive serialization for adapter convenience.
- No crate named `common`, `shared`, or `utils` may be created.
- Use `thiserror` in library crates; reserve `anyhow` for executable composition when contextual process errors exist.
- Commit `Cargo.lock` and pin the Rust/tooling versions listed above.
- Every behavior task starts with a failing test and ends with the narrowest test plus workspace checks.

---

## File Map

Create this phase's complete structure:

```text
.github/workflows/ci.yml
.gitignore
.mise.toml
Cargo.toml
Cargo.lock
deny.toml
crates/media-core/Cargo.toml
crates/media-core/src/lib.rs
crates/media-core/src/id.rs
crates/media-core/src/action.rs
crates/media-core/src/identity.rs
crates/media-core/src/job.rs
crates/media-contract/Cargo.toml
crates/media-contract/src/lib.rs
crates/media-contract/src/error.rs
crates/media-contract/src/id.rs
crates/media-contract/src/job.rs
crates/media/Cargo.toml
crates/media/src/main.rs
crates/media/tests/architecture.rs
crates/media/tests/cli.rs
```

Responsibilities:

```text
media-core/id.rs        strongly typed internal UUID identifiers
media-core/action.rs    reasons that require explicit user action
media-core/identity.rs  provider references, ordering, ambiguity policy
media-core/job.rs       job states, reasons, and legal transitions
media-contract/*        versioned JSON DTOs with no domain dependency
media/main.rs           composition root and initial CLI process
architecture.rs         executable dependency-boundary regression tests
cli.rs                  black-box binary smoke contract
```

### Task 1: Pin the Toolchain and Bootstrap the Workspace

**Files:**
- Create: `.mise.toml`
- Create: `.gitignore`
- Create: `Cargo.toml`
- Create: `crates/media-core/Cargo.toml`
- Create: `crates/media-core/src/lib.rs`
- Create: `crates/media-contract/Cargo.toml`
- Create: `crates/media-contract/src/lib.rs`
- Create: `crates/media/Cargo.toml`
- Create: `crates/media/src/main.rs`
- Generate: `Cargo.lock`

**Interfaces:**
- Produces workspace packages `media-core`, `media-contract`, and `media`.
- Produces `media --version` as the initial process smoke contract.
- Later tasks add modules without changing crate ownership.

- [ ] **Step 1: Create the pinned mise configuration**

Use this tool and task contract in `.mise.toml`:

```toml
[tools]
rust = "1.97.0"
"cargo:cargo-nextest" = "0.9.140"
"cargo:cargo-deny" = "0.20.2"
"cargo:cargo-audit" = "0.22.2"
"cargo:cargo-chef" = "0.1.77"
"cargo:sea-orm-cli" = "2.0.0-rc.42"

[env]
RUST_BACKTRACE = "1"

[tasks.format]
run = "cargo fmt --all --check"

[tasks.check]
run = "cargo check --workspace --all-targets --all-features --locked"

[tasks.lint]
run = "cargo clippy --workspace --all-targets --all-features --locked -- -D warnings"

[tasks.test]
run = "cargo nextest run --workspace --all-features"

[tasks.audit]
run = "cargo deny check && cargo audit"

[tasks.build]
run = "cargo build --workspace --all-targets --all-features --locked"
```

`.gitignore` must contain:

```gitignore
/target/
.DS_Store
.env
*.env
!.env.example
```

- [ ] **Step 2: Create the workspace manifest**

Use one dependency catalog and forbid unsafe code:

```toml
[workspace]
members = [
  "crates/media-core",
  "crates/media-contract",
  "crates/media",
]
resolver = "3"

[workspace.package]
edition = "2024"
rust-version = "1.97.0"
version = "0.1.0"
publish = false

[workspace.dependencies]
assert_cmd = "=2.2.2"
cargo_metadata = "=0.23.1"
clap = { version = "=4.6.1", features = ["derive"] }
serde = { version = "=1.0.228", features = ["derive"] }
serde_json = "=1.0.150"
thiserror = "=2.0.18"
uuid = { version = "=1.23.4", features = ["v4", "serde"] }

[workspace.lints.rust]
unsafe_code = "forbid"
```

Each crate must inherit `edition`, `rust-version`, `version`, `publish`, and
workspace lints. `media-core` depends only on `thiserror` and `uuid` without
Uuid's `serde` feature; declare a crate-local dependency with `default-features
= false` and features `std`, `v4`. `media-contract` depends on Serde and Uuid.
`media` depends on Clap and both workspace crates.

- [ ] **Step 3: Create minimal compiling crate roots**

`media-core/src/lib.rs` and `media-contract/src/lib.rs` initially contain only
crate-level documentation and `#![forbid(unsafe_code)]`. The binary uses:

```rust
use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "media", version, about = "Personal media orchestration")]
struct Cli {}

fn main() {
    let _ = Cli::parse();
}
```

- [ ] **Step 4: Install tools and generate the lockfile**

Run:

```bash
mise install
mise exec -- cargo generate-lockfile
mise run format
mise run check
```

Expected: all tools install, `Cargo.lock` is created, formatting and workspace
compilation pass.

- [ ] **Step 5: Commit the workspace bootstrap**

```bash
git add .mise.toml .gitignore Cargo.toml Cargo.lock crates
git commit -m "build: bootstrap rust workspace"
```

### Task 2: Add Strong Internal Identifiers

**Files:**
- Create: `crates/media-core/src/id.rs`
- Modify: `crates/media-core/src/lib.rs`

**Interfaces:**
- Produces: `UserId`, `MediaId`, `SeasonId`, `EpisodeId`, `JobId`, and `TaskId`.
- Every ID exposes `new()`, `from_uuid(Uuid)`, `as_uuid() -> &Uuid`, and
  `into_uuid() -> Uuid`.
- Every ID implements `Copy`, `Clone`, `Eq`, `Ord`, `Hash`, `Display`, and
  `FromStr<Err = uuid::Error>`.

- [ ] **Step 1: Write failing ID round-trip tests**

Add these tests at the bottom of `id.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{EpisodeId, MediaId};

    #[test]
    fn media_id_round_trips_through_text() {
        let id = MediaId::new();
        assert_eq!(MediaId::from_str(&id.to_string()).unwrap(), id);
    }

    #[test]
    fn different_id_types_have_independent_values() {
        let media = MediaId::new();
        let episode = EpisodeId::new();
        assert_ne!(media.to_string(), episode.to_string());
    }
}
```

- [ ] **Step 2: Run the focused test and verify failure**

Run:

```bash
mise exec -- cargo test -p media-core id::tests -- --nocapture
```

Expected: compilation fails because the ID types do not exist.

- [ ] **Step 3: Implement the ID newtypes**

Use one private macro to generate the six concrete public newtypes. The macro
must expand to real nominal structs rather than aliases:

```rust
macro_rules! define_id {
    ($name:ident) => {
        #[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
        pub struct $name(uuid::Uuid);

        impl $name {
            #[must_use]
            pub fn new() -> Self {
                Self(uuid::Uuid::new_v4())
            }

            #[must_use]
            pub const fn from_uuid(value: uuid::Uuid) -> Self {
                Self(value)
            }

            #[must_use]
            pub const fn as_uuid(&self) -> &uuid::Uuid {
                &self.0
            }

            #[must_use]
            pub const fn into_uuid(self) -> uuid::Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                uuid::Uuid::parse_str(value).map(Self)
            }
        }
    };
}
```

Export all six types from `media-core/src/lib.rs` without exposing the macro.

- [ ] **Step 4: Verify IDs and workspace checks**

```bash
mise exec -- cargo test -p media-core id::tests
mise run format
mise run check
mise run lint
```

Expected: both ID tests and all workspace checks pass.

- [ ] **Step 5: Commit the ID model**

```bash
git add crates/media-core
git commit -m "feat(core): add strong domain identifiers"
```

### Task 3: Add Canonical Identity and Ambiguity Policy

**Files:**
- Create: `crates/media-core/src/action.rs`
- Create: `crates/media-core/src/identity.rs`
- Modify: `crates/media-core/src/lib.rs`

**Interfaces:**
- Produces: `NeedsActionReason`, `ExternalNamespace`, `MappingSource`,
  `ExternalReference`, `SeriesOrdering`, and `EpisodeResolution`.
- Produces: `resolve_episode_candidates(&[EpisodeId]) -> EpisodeResolution`.
- A unique candidate resolves; zero or multiple distinct candidates return
  `NeedsAction(IdentityAmbiguous)`.

- [ ] **Step 1: Write failing identity policy tests**

```rust
#[cfg(test)]
mod tests {
    use crate::EpisodeId;

    use super::{
        EpisodeResolution, NeedsActionReason, resolve_episode_candidates,
    };

    #[test]
    fn one_unique_candidate_resolves() {
        let episode = EpisodeId::new();
        assert_eq!(
            resolve_episode_candidates(&[episode, episode]),
            EpisodeResolution::Resolved(episode),
        );
    }

    #[test]
    fn no_candidate_needs_user_action() {
        assert_eq!(
            resolve_episode_candidates(&[]),
            EpisodeResolution::NeedsAction(NeedsActionReason::IdentityAmbiguous),
        );
    }

    #[test]
    fn conflicting_candidates_need_user_action() {
        assert_eq!(
            resolve_episode_candidates(&[EpisodeId::new(), EpisodeId::new()]),
            EpisodeResolution::NeedsAction(NeedsActionReason::IdentityAmbiguous),
        );
    }
}
```

- [ ] **Step 2: Run the focused test and verify failure**

```bash
mise exec -- cargo test -p media-core identity::tests
```

Expected: compilation fails because the identity module is absent.

- [ ] **Step 3: Implement the identity vocabulary and policy**

Define the cross-domain action reason in `action.rs`:

```rust
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum NeedsActionReason {
    IdentityAmbiguous,
    PlexMismatch,
}
```

Use these exact identity variants in `identity.rs`:

```rust
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum ExternalNamespace {
    Tmdb,
    Tvdb,
    Imdb,
    AniList,
    Rezka,
    Plex,
    ProwlarrResult,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum MappingSource {
    Discovered,
    ConfirmedByUser,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ExternalReference {
    pub namespace: ExternalNamespace,
    pub value: String,
    pub source: MappingSource,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum SeriesOrdering {
    TmdbAired,
    TvdbAired,
    TvdbDvd,
    TvdbAbsolute,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum EpisodeResolution {
    Resolved(crate::EpisodeId),
    NeedsAction(crate::NeedsActionReason),
}
```

Implement `resolve_episode_candidates` by deduplicating IDs with a
`BTreeSet`; resolve only when the set length is exactly one.

- [ ] **Step 4: Verify identity behavior and library boundaries**

```bash
mise exec -- cargo test -p media-core identity::tests
mise exec -- cargo tree -p media-core --edges normal
mise run lint
```

Expected: all three tests pass; the dependency tree contains only `uuid` and
its transitive dependencies plus `thiserror` once the job module is added.

- [ ] **Step 5: Commit the identity policy**

```bash
git add crates/media-core
git commit -m "feat(core): define canonical media identity"
```

### Task 4: Add the Job State Machine

**Files:**
- Create: `crates/media-core/src/job.rs`
- Modify: `crates/media-core/src/lib.rs`

**Interfaces:**
- Produces: `JobState`, `JobTransitionError`, and
  `JobState::transition(self, next) -> Result<JobState, JobTransitionError>`.
- `needs_action` reasons reuse `NeedsActionReason` from the action module.
- Completion is impossible directly from `running`; publication must pass
  through `publishing` and `plex_pending`.

- [ ] **Step 1: Write failing transition tests**

```rust
#[cfg(test)]
mod tests {
    use super::JobState;

    #[test]
    fn plex_verified_path_can_complete() {
        let state = JobState::Publishing
            .transition(JobState::PlexPending)
            .unwrap()
            .transition(JobState::Completed)
            .unwrap();
        assert_eq!(state, JobState::Completed);
    }

    #[test]
    fn running_job_cannot_skip_plex_verification() {
        let error = JobState::Running
            .transition(JobState::Completed)
            .unwrap_err();
        assert_eq!(error.from, JobState::Running);
        assert_eq!(error.to, JobState::Completed);
    }

    #[test]
    fn cancellation_is_cooperative() {
        let state = JobState::Running
            .transition(JobState::CancelRequested)
            .unwrap()
            .transition(JobState::Cancelled)
            .unwrap();
        assert_eq!(state, JobState::Cancelled);
    }

    #[test]
    fn expired_lease_can_return_to_queue() {
        assert_eq!(
            JobState::Leased.transition(JobState::Queued).unwrap(),
            JobState::Queued,
        );
    }
}
```

- [ ] **Step 2: Run the tests and verify failure**

```bash
mise exec -- cargo test -p media-core job::tests
```

Expected: compilation fails because `JobState` does not exist.

- [ ] **Step 3: Implement states and the explicit transition table**

Define these exact states:

```rust
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum JobState {
    Queued,
    Leased,
    Running,
    CancelRequested,
    BlockedStorage,
    Publishing,
    PlexPending,
    NeedsAction,
    Partial,
    Completed,
    Failed,
    Cancelled,
}
```

Define `JobTransitionError { pub from: JobState, pub to: JobState }` with
`thiserror::Error`. Allow only the transitions required by the approved spec:

```text
queued -> leased
leased -> running | queued | cancel_requested
running -> cancel_requested | blocked_storage | publishing | needs_action | failed
cancel_requested -> cancelled
blocked_storage -> queued | cancel_requested
publishing -> plex_pending | failed
plex_pending -> completed | partial | needs_action | failed
needs_action -> queued | cancel_requested
partial -> queued
failed -> queued
```

All other transitions return `JobTransitionError`. Terminal `completed` and
`cancelled` have no outgoing transitions in MVP.

- [ ] **Step 4: Run focused and workspace verification**

```bash
mise exec -- cargo test -p media-core job::tests
mise run format
mise run check
mise run lint
mise run test
```

Expected: all transition tests and workspace checks pass.

- [ ] **Step 5: Commit the state machine**

```bash
git add crates/media-core
git commit -m "feat(core): add durable job state policy"
```

### Task 5: Define Versioned Transport DTOs

**Files:**
- Create: `crates/media-contract/src/id.rs`
- Create: `crates/media-contract/src/error.rs`
- Create: `crates/media-contract/src/job.rs`
- Modify: `crates/media-contract/src/lib.rs`

**Interfaces:**
- Produces transport-only `PublicId`, `ApiErrorCode`, `ApiError`,
  `JobStateDto`, `NeedsActionReasonDto`, and `JobSummaryDto`.
- JSON enum names use `snake_case`.
- `media-contract` MUST NOT depend on `media-core`; conversion belongs to the
  future API boundary.

- [ ] **Step 1: Write failing serialization tests**

In the corresponding modules, add tests equivalent to:

```rust
#[test]
fn job_summary_has_stable_json_names() {
    let dto = JobSummaryDto {
        id: PublicId::parse("018f3f86-7b4c-7b4f-9b6a-6d62f45bb111").unwrap(),
        state: JobStateDto::NeedsAction,
        needs_action_reason: Some(NeedsActionReasonDto::PlexMismatch),
    };

    assert_eq!(
        serde_json::to_value(dto).unwrap(),
        serde_json::json!({
            "id": "018f3f86-7b4c-7b4f-9b6a-6d62f45bb111",
            "state": "needs_action",
            "needs_action_reason": "plex_mismatch"
        }),
    );
}

#[test]
fn api_error_has_a_stable_public_shape() {
    let error = ApiError {
        code: ApiErrorCode::IdentityAmbiguous,
        message: "Episode numbering needs confirmation".to_owned(),
        request_id: "req-123".to_owned(),
    };

    assert_eq!(
        serde_json::to_value(error).unwrap(),
        serde_json::json!({
            "code": "identity_ambiguous",
            "message": "Episode numbering needs confirmation",
            "request_id": "req-123"
        }),
    );
}
```

- [ ] **Step 2: Run contract tests and verify failure**

```bash
mise exec -- cargo test -p media-contract
```

Expected: compilation fails because the DTO modules are absent.

- [ ] **Step 3: Implement the transport types**

`PublicId` wraps `uuid::Uuid`, serializes transparently as a UUID string, and
provides only `parse(&str) -> Result<Self, uuid::Error>`, `as_uuid`, and
`Display`. Use these exact enum variants:

```rust
#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStateDto {
    Queued,
    Leased,
    Running,
    CancelRequested,
    BlockedStorage,
    Publishing,
    PlexPending,
    NeedsAction,
    Partial,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NeedsActionReasonDto {
    IdentityAmbiguous,
    PlexMismatch,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiErrorCode {
    InvalidRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    IdentityAmbiguous,
    PlexMismatch,
    Internal,
}
```

`JobSummaryDto` and `ApiError` derive `Serialize` and `Deserialize`; omit
`needs_action_reason` when it is `None`.

- [ ] **Step 4: Verify JSON and dependency boundaries**

```bash
mise exec -- cargo test -p media-contract
mise exec -- cargo tree -p media-contract --edges normal
mise run lint
```

Expected: JSON tests pass; `media-core` does not appear in the contract tree.

- [ ] **Step 5: Commit the transport contract**

```bash
git add crates/media-contract
git commit -m "feat(contract): define initial public dto shapes"
```

### Task 6: Add Binary and Architecture Contract Tests

**Files:**
- Create: `crates/media/tests/cli.rs`
- Create: `crates/media/tests/architecture.rs`
- Modify: `crates/media/Cargo.toml`

**Interfaces:**
- Consumes the workspace packages and package metadata.
- Produces executable tests that prevent `media-core` from acquiring forbidden
  dependencies and prevent `media-contract -> media-core` coupling.

- [ ] **Step 1: Write the failing CLI test**

```rust
#[test]
fn binary_reports_its_version() {
    assert_cmd::cargo::cargo_bin_cmd!("media")
        .arg("--version")
        .assert()
        .success()
        .stdout("media 0.1.0\n");
}
```

Run `mise exec -- cargo test -p media --test cli`. Expected: fail until
`assert_cmd` is added as a dev dependency and the binary package metadata is
correct.

- [ ] **Step 2: Write the architecture tests**

Use `cargo_metadata::MetadataCommand` to find workspace packages and assert:

```rust
const CORE_FORBIDDEN: &[&str] = &[
    "axum",
    "reqwest",
    "sea-orm",
    "serde",
    "serde_json",
    "tokio",
];

#[test]
fn media_core_has_no_forbidden_direct_dependencies() {
    let metadata = cargo_metadata::MetadataCommand::new()
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .exec()
        .unwrap();
    let core = metadata
        .packages
        .iter()
        .find(|p| p.name.as_str() == "media-core")
        .unwrap();
    let actual: Vec<&str> = core.dependencies.iter().map(|d| d.name.as_str()).collect();

    for forbidden in CORE_FORBIDDEN {
        assert!(!actual.contains(forbidden), "media-core depends on {forbidden}");
    }

    let workspace_names: std::collections::HashSet<&str> = metadata
        .workspace_packages()
        .iter()
        .map(|package| package.name.as_str())
        .collect();
    assert!(
        core.dependencies
            .iter()
            .all(|dependency| !workspace_names.contains(dependency.name.as_str())),
        "media-core must not depend on another workspace crate",
    );
}

#[test]
fn media_contract_does_not_depend_on_media_core() {
    let metadata = cargo_metadata::MetadataCommand::new()
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .exec()
        .unwrap();
    let contract = metadata
        .packages
        .iter()
        .find(|p| p.name.as_str() == "media-contract")
        .unwrap();

    assert!(contract.dependencies.iter().all(|d| d.name != "media-core"));
}
```

- [ ] **Step 3: Run the tests and verify expected failures**

```bash
mise exec -- cargo test -p media --test cli --test architecture
```

Expected: tests fail until `assert_cmd` and `cargo_metadata` are declared under
`[dev-dependencies]` and the test target can locate the workspace.

- [ ] **Step 4: Wire dev dependencies and pass both contracts**

Add:

```toml
[dev-dependencies]
assert_cmd.workspace = true
cargo_metadata.workspace = true
```

Run:

```bash
mise exec -- cargo test -p media --test cli --test architecture
mise run check
mise run lint
mise run test
```

Expected: CLI, architecture, and full workspace suites pass.

- [ ] **Step 5: Commit executable guardrails**

```bash
git add crates/media
git commit -m "test: enforce cli and architecture contracts"
```

### Task 7: Add Dependency Policy and CI

**Files:**
- Create: `deny.toml`
- Create: `.github/workflows/ci.yml`
- Modify: `README.md`

**Interfaces:**
- Produces the required CI gates: format, check, lint, test, deny, and audit.
- Documents mise as the only supported developer entry point.

- [ ] **Step 1: Add the dependency policy**

Use this initial `deny.toml`, based on the cargo-deny 0.20.2 schema:

```toml
[graph]
all-features = true

[advisories]
ignore = []

[licenses]
allow = [
  "Apache-2.0",
  "MIT",
  "Unicode-3.0",
  "Zlib",
]
confidence-threshold = 0.8
exceptions = []

[licenses.private]
ignore = true
registries = []

[bans]
multiple-versions = "warn"
wildcards = "deny"
highlight = "all"
workspace-default-features = "allow"
external-default-features = "allow"
allow = []
allow-workspace = true
deny = []
skip = []
skip-tree = []

[sources]
unknown-registry = "deny"
unknown-git = "deny"
allow-registry = ["https://github.com/rust-lang/crates.io-index"]
allow-git = []

[sources.allow-org]
github = []
gitlab = []
bitbucket = []
```

Do not add a license or advisory exception unless `cargo deny` identifies a
specific resolved crate that requires it. Any such exception must name that
crate and version and include a reason.

- [ ] **Step 2: Verify the policy locally**

```bash
mise run audit
```

Expected: `cargo deny check` and `cargo audit` both exit successfully. Any
advisory exception must include advisory ID, dependency, reason, and expiry in
`deny.toml`; do not use wildcard ignores.

- [ ] **Step 3: Add the CI workflow**

Create `.github/workflows/ci.yml` with:

```yaml
name: CI

on:
  pull_request:
  push:
    branches: [main]

permissions:
  contents: read

jobs:
  verify:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: jdx/mise-action@v3
        with:
          install: true
          cache: true
      - run: mise run format
      - run: mise run check
      - run: mise run lint
      - run: mise run test
      - run: mise run audit
```

Do not add PostgreSQL, Docker, code coverage, release publishing, or live
provider access in this phase.

- [ ] **Step 4: Document exact local setup**

Update `README.md` with this quick start:

```bash
mise trust
mise install
mise run format
mise run check
mise run lint
mise run test
mise run audit
```

State that this phase intentionally contains no network provider, database,
filesystem, or ffmpeg implementation and link the MVP roadmap.

- [ ] **Step 5: Run the final phase gate**

```bash
mise run format
mise run check
mise run lint
mise run test
mise run audit
mise run build
git diff --check
git status --short
```

Expected: every command succeeds; `git status --short` lists only the intended
CI, policy, and README changes before commit.

- [ ] **Step 6: Commit the phase gate**

```bash
git add .github/workflows/ci.yml deny.toml README.md
git commit -m "ci: verify rust foundation"
```

## Phase Completion Checklist

- [ ] `mise current rust` reports `1.97.0`.
- [ ] `cargo metadata --no-deps` lists exactly `media-core`, `media-contract`, and `media` as workspace packages.
- [ ] `media-core` has no forbidden direct dependency and no workspace dependency.
- [ ] `media-contract` does not depend on `media-core`.
- [ ] Domain ID, identity ambiguity, and job transition tests pass.
- [ ] Contract JSON tests pass with exact `snake_case` values.
- [ ] `media --version` reports `media 0.1.0`.
- [ ] All six mise verification tasks pass.
- [ ] The worktree is clean after the final commit.

The next plan is `2026-07-10-postgres-api-foundation.md`; write and review it
only after this phase is complete and its architecture tests pass.
