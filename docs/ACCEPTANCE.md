# Acceptance Status

**Audited:** 2026-07-13  
**Runtime revision:** `27b3cfc`  
**Hermes revision:** `4e5a586`

This is the authoritative completion matrix for the active media-system goal.
`Implemented`, `deployed`, and `live-verified` are independent claims. A partial
live result keeps the goal open even when its code and deployment are complete.

| Workstream | Implemented | Deployed | Live-verified | Evidence |
| --- | --- | --- | --- | --- |
| 1. VPN lifecycle | Yes | Yes | Yes | [session refresh and rotation](evidence/2026-07-12-session-refresh-vpn-lifecycle.md), [blocked lifecycle](evidence/2026-07-13-runner-availability.md) |
| 2. Rezka episode | Yes | Yes | Yes | [episode E2E](evidence/2026-07-13-rezka-episode-e2e.md), [storage and rich notifications](evidence/2026-07-13-storage-resume-and-rich-notifications.md) |
| 3. Subtitle recovery | Yes | Yes | Yes | [subtitle-only retry](evidence/2026-07-13-subtitle-only-retry.md), [natural partial recovery](evidence/2026-07-13-natural-subtitle-partial-recovery.md) |
| 4. Prowlarr and qBittorrent | Yes | Yes | Yes | [TV E2E](evidence/2026-07-13-prowlarr-tv-e2e.md), [movie E2E](evidence/2026-07-13-prowlarr-movie-e2e.md) |
| 5. Notifications | Yes | Yes | Yes | [routing](evidence/2026-07-13-notification-routing.md), [rich delivery](evidence/2026-07-13-storage-resume-and-rich-notifications.md) |
| 6. Tracking and release dates | Yes | Yes | Yes | [tracking E2E](evidence/2026-07-13-tracking-e2e.md), [release query](evidence/2026-07-13-release-query.md), [Hermes conversation](evidence/2026-07-13-hermes-release-conversation.md) |
| 7. Movie flow | Yes | Yes | Yes | [Rezka movie](evidence/2026-07-13-rezka-movie-e2e.md), [Prowlarr movie](evidence/2026-07-13-prowlarr-movie-e2e.md) |
| 8. Recovery, storage, mapping | Yes | Yes | Partial | [retry and deploy](evidence/2026-07-13-job-retry-and-local-deploy.md), [storage resume](evidence/2026-07-13-storage-resume-and-rich-notifications.md), [OVA mapping](evidence/2026-07-13-episode-mapping-and-ova.md) |
| 9. Multi-user Hermes | Yes | Yes | Partial | [API and ownership isolation](evidence/2026-07-13-multi-user-isolation.md), [notification routing](evidence/2026-07-13-notification-routing.md) |
| 10. Local operations | Yes | Yes | Yes | [final deploy and rollback](evidence/2026-07-13-final-local-deployment.md) |
| 11. Documentation truthfulness | Yes | N/A | Yes | This matrix, [architecture](ARCHITECTURE.md), [runbook](RUNBOOK.md), and the [canonical plan](plans/2026-07-12-full-media-system-delivery.md) |

## Open Live Gates

### Expired Stream and Specials Publication

The bounded expired-stream code path and persistent `S00E01` mapping are
implemented and deployed. A real OVA reached `needs_action`, accepted the
mapping, and reused it later, but no mapped OVA has yet completed download,
VAAPI processing, publication under Plex `Specials`, and exact Plex identity
verification. A controlled real expired stream has also not completed a
refresh-and-resume cycle.

This gate requires an explicitly approved real Rezka download. It must record
the old and refreshed stream behavior, final `S00E..` path, ffprobe output,
subtitle result, Plex metadata identity, notifications, and cleanup state.

### Both-Profile Telegram and Unknown Sender

Both containers have distinct allowlists, tokens, profiles, memories, and media
client credentials. API-level private ownership and family sharing are proven.
Primary's primary Telegram and direct notification paths are live-verified, and
Secondary received real personal and family tracking notifications.

Still required:

- one primary natural-language media command sent by Secondary to
  `hermes-secondary`, with the resulting media API read recorded;
- one command from an identity absent from the target bot's allowlist, proving
  rejection before LLM or tool execution.

Opening the target bot chat without successfully sending the command is not
evidence and does not satisfy this gate.

## Completion Verdict

The system is implemented and deployed, but the overall goal is **not yet
complete**. Workstreams 8 and 9 retain the live gates above. No narrower test,
container health result, or static configuration check may be used to promote
either row to fully live-verified.
