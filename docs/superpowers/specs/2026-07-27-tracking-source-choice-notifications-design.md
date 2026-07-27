# Tracking Source Choice Notifications

## Goal

Replace the remaining legacy future-episode notification with a typed Telegram
card that offers three working inline actions:

- All
- Rezka
- Prowlarr

The actions start searches only. They never select a result, create a download,
or silently fall back between providers.

## Current Failure

`future-episode-found` stores only `{"message": "..."}`. The notification
integration therefore serializes it through the legacy webhook and the Telegram
notifier receives no action metadata. Existing media actions cannot be reused
because they require a download job ID, while a future-episode discovery has no
job yet.

The production inventory contains one active legacy producer:
`SeaOrmTrackingStore::record_future_episode`. Other job notification producers
already emit schema-v2 media cards. Legacy payloads in migrations and tests are
compatibility fixtures or historical rows.

## Envelope

Add a dedicated `media.source-choice` schema-v1 webhook and matching domain
content. It contains:

- a stable card key;
- tracking subscription ID;
- display title;
- season and episode;
- the fixed actions `all`, `rezka`, and `prowlarr`.

The notification outbox validates this shape separately from legacy payloads
and schema-v2 job notifications. Undelivered legacy
`future-episode-found` rows are migrated when their discovery metadata can be
resolved safely. Delivered historical messages remain unchanged.

## Telegram Behavior

The card is plain Telegram text without Markdown markers and has one row of
inline buttons:

`All | Rezka | Prowlarr`

Callback data is bounded and contains the action, tracking ID, season, and
episode. The Telegram plugin applies the existing sender allowlist before doing
any work.

For a selected provider, the plugin:

1. loads the visible tracking subscription through `hermes-media`;
2. validates that the callback still references a real subscription;
3. starts a series search for the stored title and season;
4. replies with up to five provider results and mentions the requested episode.

The `all` action runs the Rezka and Prowlarr searches concurrently and renders
their outcomes independently. A failure in one provider does not hide results
from the other.

## Scope Preservation

The callback passes the Telegram chat and thread into the media CLI environment.
The hardened `hermes-media` wrapper preserves only those explicit scope
variables in addition to its existing allowlist.

## Other Source Prompts

The shared media skill must always describe source choice as exactly:

- All sources
- Rezka
- Prowlarr

This affects conversational prompts only. Direct inline controls remain owned
by deterministic notification and Telegram plugin code; the model must not emit
XML-like quick-reply markup.

## Verification

- Rust domain, contract, storage, migration, and webhook tests cover the new
  envelope and prove that the producer no longer emits a legacy message.
- Python notifier tests cover parsing, rendering, and all three callbacks.
- Telegram plugin tests cover authorization, provider search, concurrent
  all-source search, partial provider failure, stale callbacks, and scope
  propagation.
- Repository-wide searches confirm there are no active
  `notification_outbox` producers that write message-only payloads.
- A deployed synthetic future-episode notification verifies the inline buttons
  in Telegram without creating a download job.
