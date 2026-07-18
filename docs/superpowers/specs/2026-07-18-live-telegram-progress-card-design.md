# Live Telegram Progress Card Design

## Goal

Continuously update one Telegram status card for an active media download
without invoking Hermes' LLM and without creating a new message for each
progress sample. Terminal completion, partial, and failure notifications remain
separate messages so Telegram alerts the initiating user and raises the chat.

## User Experience

When a download enters an active transfer stage, the existing job status card
is updated approximately every five seconds with the trustworthy available
subset of:

- a ten-cell progress bar and percentage;
- downloaded and total bytes;
- current transfer speed;
- estimated remaining time;
- source state;
- torrent seeds and peers;
- current pipeline stage and job ID.

Direct MP4 and torrent downloads show percentage and ETA only when total size is
known. HLS downloads show downloaded bytes and speed without inventing a total,
percentage, or ETA. Missing values are omitted.

Updates do not create Telegram notifications and do not invoke an LLM. A final
`completed`, `partial`, or `failed` event is delivered as a new message using
the existing terminal notification path.

## Existing Foundation

The runner already persists normalized `StageCheckpoint` events at most every
five seconds. The existing signed Hermes `deliver_only` webhook bypasses the
LLM. Hermes already persists a mapping from `status_key` to Telegram
`message_id` and calls `edit_message` when the same status key is delivered
again.

This feature connects those existing paths. Hermes does not poll
`media-service`, and neither Hermes nor `media-service` receives qBittorrent or
Rezka transfer credentials.

```text
download source
      |
      v
download-runner -- StageCheckpoint (about every 5 seconds)
      |
      v
media-service transaction
      |
      +-- latest job stage checkpoint
      +-- one coalesced status-card outbox row
      |
      v
signed deliver_only webhook
      |
      v
Hermes edits the stored Telegram message_id
```

## Coalescing Model

There is exactly one mutable status-card outbox row for each active job and
recipient. All non-terminal job status events use one stable source dedupe key:

```text
media-job-status:<job-id>:<recipient>
```

The database constraint remains the authority for uniqueness. A new lifecycle
milestone or download checkpoint atomically upserts the row:

- replace `event_type` and `payload` with the newest card;
- increment an internal `generation`;
- make a previously delivered row pending immediately;
- clear previous retry/dead-letter state;
- preserve an active lease until its current delivery attempt acknowledges.

An upsert whose event type and payload are unchanged does nothing, so unchanged
measurements do not produce webhook calls.

Terminal events retain their event-specific dedupe keys and append a separate
notification row. They never overwrite the status-card row.

## Generation and Delivery Races

`notification_outbox` gains a positive `generation` column with default `1`.
The leased `NotificationDelivery` includes that generation, and delivery
acknowledgement operations provide it back to storage.

If the row has not changed, a successful acknowledgement marks it delivered as
today. If a checkpoint replaced the row while an older webhook request was in
flight, acknowledgement of the older generation only releases the lease and
keeps the latest generation pending for immediate delivery. The same rule
applies to retryable and terminal delivery failures: a failure belonging to an
obsolete generation must never back off or dead-letter the current card.

Because one row retains its active lease while being replaced, two generations
of the same card cannot be leased concurrently. Hermes therefore receives card
updates in order, and its existing `status_key` to `message_id` map is
sufficient; no LLM or Telegram API integration change is required.

## Progress Projection

`StageCheckpoint` produces a status-card update only for validated `download`
and `torrent_monitor` transfer checkpoints. The formatter uses the same bounded
field rules as the public job-detail contract:

- `kind` must be `direct`, `hls`, or `torrent`;
- percentage must be in `0..=100`;
- byte, speed, ETA, seed, and peer values must be unsigned;
- downloaded bytes cannot exceed a known total;
- source state and displayed metadata are bounded and sanitized;
- no source URL, torrent hash, path, credential, or raw provider payload is
  included.

Malformed checkpoints remain persisted for diagnostics but do not update the
Telegram card and do not fail the job.

Progress updates are routed to the initiator only, matching existing progress
milestone behavior even when the job's terminal notification scope is
`family`. Terminal notifications continue to honor the configured scope.

## Message Shape

The progress card is rendered deterministically by `media-service` in Russian,
using the existing job notification context for title, media kind, episode,
translation, and destination. It contains no generated prose.

Example:

```text
⬇️ **«Энола Холмс 3» скачивается**

`[#######---] 73%`
💾 4.1 GiB / 5.6 GiB
⚡ 18.4 MiB/s · осталось 1м 25с
🌱 12 сидов · 4 пира
🔄 Этап: загрузка и контроль торрента

🆔 `Job fcbc43b3-fbd3-4a54-9bf6-cf6c8c25063e`
```

The exact card omits absent metrics and remains within Telegram message limits.

## Hermes Behavior

Both profiles continue receiving the signed `media.notification`
`deliver_only` webhook. The webhook route sends or edits Telegram messages
directly and never enters the agent loop.

The shared media skill is updated to describe live direct-delivery progress and
must no longer claim that checkpoints are visible only after an explicit status
request. Explicit status questions still use `hermes-media jobs get` and the LLM
for an on-demand explanation; automatic card edits do not consume model tokens.

## Failure Handling

Progress delivery is best-effort and never fails or pauses a download.

- A webhook outage retains only the latest coalesced card and retries it with
  existing bounded backoff.
- A newer generation supersedes retry/dead-letter state from an older one.
- Invalid progress is skipped while lifecycle and terminal notifications remain
  operational.
- Service or Hermes restart preserves the outbox row and Telegram `message_id`
  mapping, so the next checkpoint resumes editing the same card.
- A Telegram edit failure keeps the existing Hermes fallback: remove the stale
  cached message ID, send one replacement card, and persist its new ID.

## Verification

Automated coverage must prove:

- repeated checkpoints keep exactly one status-card outbox row;
- a changed checkpoint increments generation and becomes pending;
- an unchanged checkpoint does not cause another delivery;
- acknowledgement or failure of an obsolete generation preserves and releases
  the newest card;
- malformed progress does not create a card or fail the job;
- initiator and family routing remain correct;
- terminal events remain separate notifications;
- the webhook payload keeps a stable `status_key` and uses `deliver_only`;
- both Hermes profiles retain the persistent edit-message behavior;
- the shared media skill distinguishes automatic zero-token edits from explicit
  LLM status answers.

Live verification uses one small active download and confirms that one Telegram
message changes at least twice, no progress-message spam appears, terminal
completion arrives separately, and the notification outbox has no pending or
dead rows afterward.

## Non-goals

- LLM-generated progress text.
- Telegram polling by `media-service`.
- Browser automation or Telegram bot tokens inside `media-service`.
- Historical progress graphs or long-term speed analytics.
- More frequent runner checkpoints than the existing approximate five-second
  cadence.
