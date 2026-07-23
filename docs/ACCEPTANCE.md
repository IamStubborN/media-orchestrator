# Acceptance Status

**Audited:** 2026-07-23
**Runtime revision:** `aa69027`
**Hermes revision:** `307ad63`
**Homelab revision:** `4e82866`

This is the authoritative completion matrix for the active media-system goal.
`Implemented`, `deployed`, and `live-verified` are independent claims. A partial
live result keeps the goal open even when its code and deployment are complete.

| Workstream | Implemented | Deployed | Live-verified | Evidence |
| --- | --- | --- | --- | --- |
| 1. VPN lifecycle | Yes | Yes | Yes | [session refresh and rotation](evidence/2026-07-12-session-refresh-vpn-lifecycle.md), [blocked lifecycle](evidence/2026-07-13-runner-availability.md) |
| 2. Rezka episode | Yes | Yes | Yes | [episode E2E](evidence/2026-07-13-rezka-episode-e2e.md), [final yt-dlp verification](evidence/2026-07-21-final-live-verification.md) |
| 3. Subtitle recovery | Yes | Yes | Yes | [subtitle-only retry](evidence/2026-07-13-subtitle-only-retry.md), [natural partial recovery](evidence/2026-07-13-natural-subtitle-partial-recovery.md) |
| 4. Prowlarr and qBittorrent | Yes | Yes | Yes | [TV E2E](evidence/2026-07-13-prowlarr-tv-e2e.md), [movie E2E](evidence/2026-07-13-prowlarr-movie-e2e.md) |
| 5. Notifications | Yes | Yes | Yes | [routing](evidence/2026-07-13-notification-routing.md), [rich delivery](evidence/2026-07-13-storage-resume-and-rich-notifications.md), [detailed mutable Telegram cards](evidence/2026-07-23-detailed-telegram-media-notifications.md) |
| 6. Tracking and release dates | Yes | Yes | Yes | [tracking E2E](evidence/2026-07-13-tracking-e2e.md), [automatic download without LLM](evidence/2026-07-21-final-live-verification.md), [release query](evidence/2026-07-13-release-query.md) |
| 7. Movie flow | Yes | Yes | Yes | [Rezka movie](evidence/2026-07-13-rezka-movie-e2e.md), [Prowlarr movie](evidence/2026-07-13-prowlarr-movie-e2e.md) |
| 8. Recovery, storage, mapping | Yes | Yes | Yes | [retry and deploy](evidence/2026-07-13-job-retry-and-local-deploy.md), [storage resume](evidence/2026-07-13-storage-resume-and-rich-notifications.md), [deterministic expired-stream recovery](evidence/2026-07-13-expired-stream-and-specials-deterministic.md), [live CDN retry and Specials publication](evidence/2026-07-21-final-live-verification.md) |
| 9. Multi-user Hermes | Yes | Yes | Yes | [API and ownership isolation](evidence/2026-07-13-multi-user-isolation.md), [notification routing](evidence/2026-07-13-notification-routing.md), [unknown-sender rejection](evidence/2026-07-13-telegram-unknown-sender.md) |
| 10. Local operations | Yes | Yes | Yes | [final deploy and rollback](evidence/2026-07-13-final-local-deployment.md) |
| 11. Documentation truthfulness | Yes | N/A | Yes | This matrix, [architecture](ARCHITECTURE.md), [runbook](RUNBOOK.md), and the [canonical plan](plans/2026-07-12-full-media-system-delivery.md) |

## Residual Live Exercises

### Deliberately Expired Stream

The explicit `stream_expired` path is covered by deterministic cross-boundary
tests. Live transfer recovered from real CDN HTTP 502 and DNS failures through
fresh resolution, and a mapped OVA completed under Plex `Specials`. Deliberately
holding a valid signed URL until provider expiry remains an optional production
fault-injection exercise; it is not required for normal operation or delivery.

### Deferred Secondary Conversation

Both containers have distinct allowlists, tokens, profiles, memories, and media
client credentials. API-level private ownership and family sharing are proven.
Primary's primary Telegram and direct notification paths are live-verified, and
Secondary received real personal and family tracking notifications. An
authenticated Primary Telegram Web session also sent `/start` to
`hermes-secondary`; the adapter rejected the absent-from-allowlist sender before
LLM or tool execution and produced no bot response.

The user moved Secondary's own natural-language Telegram conversation to a
separate follow-up. It is not used as evidence for the isolation claim and no
longer blocks this delivery.

## Completion Verdict

The system is implemented, deployed, and live-verified for the agreed MVP. The
remaining item above is an optional destructive-timing exercise rather than a
missing user workflow.
