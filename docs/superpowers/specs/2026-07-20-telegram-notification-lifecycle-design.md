# Telegram Notification Lifecycle Design

## Goal

Make media notifications understandable and quiet while preserving live progress, reliable completion alerts, and direct recovery actions. Automatic notification delivery and interaction must not invoke an LLM.

## User Experience

Each media job owns one Telegram status card for its complete lifecycle. A season download uses one card for the season rather than one message per episode. A tracked future episode owns its own card from discovery through Plex publication.

The card uses a balanced presentation:

- title, season or episode, provider, and selected translation;
- completed episode count for season jobs;
- provider-specific transfer measurements when available;
- one user-facing current stage and one next step;
- contextual inline actions.

The card updates at most once every ten seconds when progress changes and immediately when the lifecycle stage changes. Missing totals must not produce invented percentages or completion estimates.

On a terminal transition, the existing card is edited into the final result. Hermes also sends one short reply to that card so Telegram produces a completion notification. The reply contains only the outcome and title; details remain in the card.

## States And Copy

Active cards use user-facing states such as downloading, processing, adding to Plex, restoring the connection, and switching VPN. Retry counters, internal stage names, error codes, paths, and job identifiers are hidden from the primary card.

Rezka cards prioritize the current episode, downloaded bytes, speed, and VAAPI processing. Prowlarr cards prioritize torrent percentage, downloaded and total size, speed, seed count, and qBittorrent state. Both providers use the same card structure and status vocabulary.

A season with some published episodes is a partial success, not a generic failure. Its final card states how many episodes are available in Plex, lists missing episodes, and offers a direct action to retry only those episodes.

## Actions

Hermes renders contextual Telegram inline buttons from structured actions supplied by media-service:

- `cancel` while work is active;
- `retry` after a retryable terminal failure;
- `retry-missing` after a partial season result;
- `resume-storage` after storage has been freed;
- `details` for technical diagnostics.

Action callbacks are authorized against the Telegram user and job ownership before execution. Deterministic actions call media-service directly and do not pass through the conversational agent or consume model tokens. The details action may render sanitized diagnostics, including the job identifier, attempt count, and error code.

## Responsibilities

Media-service remains the source of truth. It emits a structured notification payload containing card identity, monotonically increasing revision, lifecycle state, terminal flag, media identity, progress, user-facing stage, next step, and allowed actions. It does not emit Telegram Markdown.

Hermes owns channel presentation. Its Telegram adapter renders deterministic Russian copy, emoji, progress bars, and inline keyboards. It persists the Telegram message ID, latest applied revision, and terminal state for each recipient and card key.

The media skill documents how conversational status requests should be answered, but automatic cards, final pushes, and inline callbacks bypass the LLM.

## Ordering And Delivery

Every card update has a monotonically increasing revision. Hermes ignores updates whose revision is not newer than the stored revision. Once a terminal update is applied, non-terminal updates can never change that card again.

Outbox delivery remains at-least-once. Card key and revision make card replay idempotent. A final card update and its short completion reply use separate delivery identities. Hermes records an acknowledged push receipt and suppresses later replays. Because Telegram does not accept an idempotency key for a new message, an ambiguous network timeout may produce one duplicate short push; it must never produce another full result card.

The current behavior that gives terminal events no status key must be removed: completed, partial, and failed events update the same status card. The separate completion reply is an explicit push event rather than a second full result card.

## Notification Routing

Progress cards and final pushes go to the job initiator by default. Family-scoped terminal and action-required outcomes go to both configured users, while intermediate progress remains initiator-only.

## Failure Handling

If editing a known Telegram message fails because it no longer exists, Hermes sends a replacement card and stores the new message ID. Transient Telegram failures remain retryable through the outbox. Invalid callbacks, stale actions, and ownership mismatches receive a short user-facing answer and make no media-service state change.

A failed notification must never alter the media job. A completed media job remains completed even when Telegram delivery is delayed.

## Acceptance Criteria

- A job produces one full status card, not one full message per stage.
- A season card reports episode progress and never labels retry count as download progress.
- Terminal state replaces the active card and cannot be overwritten by stale progress.
- Completion normally produces one short Telegram push reply and suppresses replay after acknowledged delivery.
- Partial season completion lists published and missing episodes and retries only missing work.
- Rezka and Prowlarr cards share a layout while exposing source-appropriate measurements.
- Primary cards contain no job ID, internal error code, filesystem path, or raw attempt counter.
- Inline actions execute without an LLM and enforce owner authorization.
- Tracking discovery flows through the same card until the episode is available or terminally failed.
- Existing active downloads continue independently during deployment; no migration rewrites or cancels their jobs.

## Verification

Contract and repository tests cover structured payload validation, revision ordering, terminal locking, deduplication, routing, season aggregation, partial results, and callback authorization. Hermes tests cover deterministic rendering, message replacement fallback, inline actions, and final-push replay suppression after acknowledged delivery.

Deployment verification uses synthetic notification payloads first. A real low-volume download is then observed from card creation through terminal replacement and final push, without restarting or cancelling unrelated active jobs.
