# TMDB Trending Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a shared weekly TMDB trending command for both Hermes profiles.

**Architecture:** A typed client in `media-integrations` maps TMDB responses into DTOs from `media-contract`. `media-api` exposes the client through a read-only authenticated route, while the `media` binary provides the CLI used by the constrained Hermes wrapper.

**Tech Stack:** Rust, reqwest, axum, clap, serde, wiremock, Docker Compose, Hermes skills.

## Global Constraints

- Return at most five ordered TMDB results per request.
- Support `all`, `movie`, and `tv`; use the weekly window only.
- Keep TMDB credentials inside `media-service` and never expose them to Hermes.
- Do not persist trending requests and do not trigger downloads.
- Keep unrelated media operations available when TMDB is not configured.

---

### Task 1: Typed TMDB integration

**Files:**
- Create: `crates/media-contract/src/trending.rs`
- Modify: `crates/media-contract/src/lib.rs`
- Create: `crates/media-integrations/src/tmdb.rs`
- Modify: `crates/media-integrations/src/lib.rs`
- Create: `crates/media-integrations/tests/tmdb.rs`

**Interfaces:**
- Produces: `TrendingCategoryDto`, `TrendingItemDto`, `TrendingPageDto`.
- Produces: `TmdbConfig::new`, `TmdbClient::new`, and `TmdbClient::trending(category, page)`.

- [ ] Write wiremock tests for localized movie/TV mapping, five-item truncation, authorization failure, and malformed responses.
- [ ] Run `cargo test -p media-integrations --test tmdb` and verify the new module is missing.
- [ ] Implement DTOs and a bounded reqwest client for `GET /3/trending/{category}/week?api_key=...&language=...&page=...`.
- [ ] Re-run the TMDB integration and contract tests.
- [ ] Commit the typed integration.

### Task 2: Protected API and optional configuration

**Files:**
- Create: `crates/media-api/src/trending.rs`
- Create: `crates/media-api/src/route/trending.rs`
- Modify: `crates/media-api/src/lib.rs`
- Modify: `crates/media-api/src/route/mod.rs`
- Create: `crates/media-api/tests/trending.rs`
- Modify: `crates/media/src/config.rs`
- Modify: `crates/media/src/composition.rs`
- Modify: `crates/media/tests/config.rs`

**Interfaces:**
- Produces: async `TrendingService::trending(TrendingCategoryDto, u32) -> Result<TrendingPageDto, TrendingServiceError>`.
- Produces: authenticated `GET /v1/trending?category=all&page=1`.
- Produces: optional `ServerConfig::tmdb()` loaded from `MEDIA_TMDB_API_KEY` and `MEDIA_TMDB_LANGUAGE`.

- [ ] Write API tests for authorization, defaults, category/page validation, unavailable integration, and success.
- [ ] Write config tests proving optional startup and secret redaction.
- [ ] Run focused tests and confirm failures.
- [ ] Add the service trait, route, config, and composition adapter.
- [ ] Run `cargo test -p media-api --test trending` and focused config tests.
- [ ] Commit the API slice.

### Task 3: CLI command

**Files:**
- Modify: `crates/media/src/main.rs`
- Modify: `crates/media/src/client.rs`
- Modify: `crates/media/src/render.rs`
- Modify: `crates/media/tests/cli.rs`
- Modify: `crates/media/tests/http_cli.rs`
- Modify: `crates/media/tests/render_cli.rs`

**Interfaces:**
- Produces: `media trending [--category all|movie|tv] [--page N] [--json]`.
- Consumes: `GET /v1/trending` from Task 2.

- [ ] Add failing parsing, HTTP-forwarding, JSON, and human-render tests.
- [ ] Run the focused CLI tests and confirm failures.
- [ ] Add clap arguments, `HttpClient::trending`, and a concise list renderer.
- [ ] Re-run all focused CLI tests.
- [ ] Commit the CLI command.

### Task 4: Hermes, homelab, deployment, and live verification

**Files:**
- Modify: `/Users/operator/Projects/personal/hermes-home/scripts/hermes-media`
- Modify: `/Users/operator/Projects/personal/hermes-home/shared/skills/media/SKILL.md`
- Modify: `/Users/operator/Projects/personal/hermes-home/README.md`
- Modify: `/Users/operator/Projects/personal/hermes-home/tests/test_scaffold.py`
- Modify: `/Users/operator/Projects/personal/homelab/media/compose.media-orchestrator.yml`
- Modify: `/Users/operator/Projects/personal/homelab/.env.example`
- Modify: `/Users/operator/Projects/personal/homelab/media/tests/validate-media-orchestrator-compose.sh`

**Interfaces:**
- Produces: constrained `hermes-media trending` access for both profiles.
- Produces: `MEDIA_TMDB_API_KEY=${MT_TMDB_API_KEY}` and `MEDIA_TMDB_LANGUAGE=${MT_TMDB_LANGUAGE:-ru}` on `media-service` only.

- [ ] Add failing wrapper, skill-contract, and Compose validation assertions.
- [ ] Allowlist `trending`, document natural-language routing and pagination, and mount existing TMDB configuration into `media-service`.
- [ ] Run Hermes and homelab validation suites.
- [ ] Run `cargo fmt --check`, `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `git diff --check`.
- [ ] Commit and push the repositories, build updated images, deploy `media-service` and both Hermes profiles, and wait for healthy containers.
- [ ] Run page 1 and page 2 through `hermes-primary`, verify distinct results, and inspect logs for errors.

