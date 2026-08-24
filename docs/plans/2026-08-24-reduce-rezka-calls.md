# Reduce Rezka calls (items 1–7)

Date: 2026-08-24
Repos: `media-orchestrator` (application) and `homelab` (watcher + Hermes skill/runbook text)

## Goal

Cut background and duplicate Rezka HTTP without weakening Anubis handling, sticky VPN, or anonymous sessions. Do not log or persist cookies, JWTs, challenge HTML, or signed stream URLs. Native SHA-256 must still never launch Chromium.

Idle calendar checks must stay on TVMaze. Rezka is for: user search, choice-set refresh, auto-download enqueue, watcher probe when the VPN IP is new, and `ensure_session` when clearance is missing/stale or the IP changed.

## Item map

| # | Change | Default |
|---|---|---|
| 1 | Skip `ensure_session` probe when Anubis clearance exists, TTL is fresh, and VPN IP matches last validation | TTL 30 min |
| 4 | Auto-download interval | 15 min → 30 min |
| 7 | Notify-only interval | 1 h → 3 h |
| 2 | Notify-only tracking does not search Rezka | calendar notify only |
| 3 | Auto-download enqueue uses cached title by `provider_media_ref`, not catalog search | process TTL 30 min |
| 5 | One `ensure_session` per tracking `run_once`, not per inner search | warmup at pass start |
| 6 | Watcher skips Rezka probe when `public_ip` equals last successful ready `current_ip` | lifecycle GET |

## 1 — Session TTL bound to VPN IP

`RezkaClient::ensure_session` always `GET`s the probe URL today.

- Record inside the **encrypted** session snapshot (not logs): `validated_at` (unix seconds) and `validated_ip` (last public IP that passed probe). Redact both in Debug.
- `ensure_session` takes the current public IP (empty/unknown ⇒ always probe).
- Skip network probe only when all hold: clearance cookie present, `validated_ip` equals current IP, `now - validated_at < 30 minutes`.
- On skip, still classify as the last known valid/invalid anonymous state from cookies; if classification would be inconclusive, probe.
- IP mismatch, missing clearance, TTL expiry, rotating/blocked lifecycle, or probe failure ⇒ full probe (+ existing Anubis native then one browser fallback).
- Current IP for media-service: runner lifecycle `current_ip` when state is `ready`. If lifecycle is missing/rotating/blocked or IP unknown, probe.
- Runner already has Gluetun `public_ip`; pass that into ensure_session on runner paths.
- VPN rotation must not reuse a stale skip: watcher/lifecycle IP change is the signal (item 6 + 1 together).
- Tests in `rezka-client` session_flow and `media` search_flow: skip vs force-probe; never leak IP/cookies in errors.

## 2 — Notify-only tracking skips Rezka

In `TrackingRuntime::run_once`, when `tracking.download().is_none()`:

- Still discover episodes from the calendar port.
- **Do not** call `availability.probe` (that path catalog-searches Rezka and Prowlarr).
- Persist discovered episodes with empty actions / zero provider counts (or omit counts). Notification copy may say a new episode exists without `rezka_count`.
- `media_episode_choice_set_refresh` and explicit user search remain the Rezka/Prowlarr lookup.
- Auto-download subscriptions (`download().is_some()`) unchanged except items 3–5.
- Update `tracking_runtime` tests and Hermes `SKILL.md` / RUNBOOK: notify 3 h, no provider scrape until refresh.

## 3 — Cache title by `provider_media_ref`

`TrackedEpisodeDownloader::enqueue_episode` catalog-searches by title then matches `title_id`.

- Keep an in-process cache keyed by `provider_media_ref` (title id string) → last usable Rezka search result needed to build execution. TTL 30 min. Mutex, no disk, no cookies in the cache (result ids / title id / locator / translation metadata only).
- Cache hit: skip catalog `search`. Still `ensure_session` via item 5 warmup, then `verify_series_identity` + enqueue as today.
- Cache miss: one search (or `title()` if a stored locator exists without a new catalog query), then fill cache.
- Invalidate on identity mismatch / title fetch failure.
- Tests: second enqueue for the same ref does not call catalog search; different ref still searches.

## 4 and 7 — Intervals

In `crates/media-core/src/tracking.rs`:

- `DOWNLOAD_TRACKING_INTERVAL`: 30 minutes
- `NOTIFY_TRACKING_INTERVAL`: 3 hours
- Failure cooldown stays 15 minutes

Update `tracking_runtime` time assertions and RUNBOOK/SKILL.md.

## 5 — One session warmup per tracking pass

- Add a narrow port, e.g. `prepare_anonymous_session() -> Result<(), PortError>`, implemented by the existing Rezka search adapter (lock, reload, `ensure_session` once, save snapshot).
- `TrackingRuntime::run_once` calls it **once** before the due loop when any claimed row is auto-download **or** when notify is still not probing Rezka, skip warmup for notify-only batches that will not touch Rezka.
- If the pass includes auto-download rows, warmup once; inner `search_rezka` / enqueue reuse the jar and item 1 skip.
- Do not hold the Rezka mutex across TVMaze calls.
- Test: two auto-download enqueues in one `run_once` probe at most once when TTL/IP allow.

## 6 — Watcher skip probe on same IP

`homelab/media/gluetun-rezka-watcher/watch.sh`:

- `GET` lifecycle. If `state=ready` and `current_ip` is non-null and equals `public_ip`, skip `rezka_egress_healthy` and keep ready.
- Always probe when: no lifecycle, state is not ready, IP missing, IPs differ, or parent just rotated (`rotate_parent` path).
- After a successful probe, `put_lifecycle ready` still writes the current IP (unchanged).
- Contract test in `validate-media-orchestrator-compose.sh` / watcher script tests: same-IP skip, different-IP probe. Do not use a raw HTML health URL.

## Files (expected)

- `crates/rezka-client/src/session/{mod.rs,cookie.rs}` + `tests/session_flow.rs`
- `crates/media-core/src/tracking.rs` + `tests/tracking_runtime.rs`
- `crates/media/src/{search.rs,composition.rs}` + `tests/search_flow.rs`
- `docs/RUNBOOK.md`, `docs/ARCHITECTURE.md` (one short paragraph)
- `homelab/media/gluetun-rezka-watcher/watch.sh` + `media/tests/validate-media-orchestrator-compose.sh`
- `homelab/hermes/shared/skills/media/SKILL.md`

Do **not** touch unrelated Hermes Vaultwarden plugin diffs.

## Tests

- `cargo test -p rezka-client --test session_flow --test anubis`
- `cargo test -p media-core --test tracking_runtime`
- `cargo test -p media --test search_flow --test anubis_browser`
- `bash homelab/media/tests/validate-media-orchestrator-compose.sh`
- `cargo fmt` in media-orchestrator

No live Rezka, no VPN in CI. Mocks only.

## Out of scope

- Removing watcher probes on rotation
- Skipping ensure_session without IP
- Changing Chrome pin / Watchtower
- Deploy/commit (orchestrator does that after review)
