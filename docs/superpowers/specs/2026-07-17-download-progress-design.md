# Download Progress Design

## Goal

Expose trustworthy, source-aware download progress through `media jobs get` so
Hermes can answer status questions with useful details for both Rezka and
Prowlarr jobs. Progress reporting must not create additional Telegram messages
or grant `media-service` or Hermes direct access to qBittorrent credentials.

## User Experience

For an active download, Hermes should report the available subset of:

- progress percentage;
- downloaded and total bytes;
- current download speed;
- estimated remaining time;
- provider state;
- connected seeds and peers for torrents;
- checkpoint age.

Missing or unreliable values are omitted. Hermes must not infer a percentage or
ETA from the job state alone. Existing status-card notifications continue to
change only at phase boundaries; progress checkpoints are visible on explicit
status requests and do not emit Telegram notifications.

## Architecture

The download runner remains the only component that talks to download sources.
It already polls qBittorrent and owns the Rezka transfer loop, so it converts
source observations into a bounded `TransferProgress` checkpoint and reports
that checkpoint through the existing runner event API.

```text
Rezka HTTP/HLS or qBittorrent
             |
             v
       download-runner
             |
       StageCheckpoint
             |
             v
       media-service DB
             |
        GET /v1/jobs/:id
             |
             v
       media CLI / Hermes
```

`media-service` does not receive qBittorrent credentials and does not poll an
external provider while serving a job-status request. A status response is a
read of the latest persisted checkpoint and can therefore be several seconds
old.

## Progress Contract

`JobDetailDto` gains an optional `progress` object:

```json
{
  "kind": "torrent",
  "state": "downloading",
  "progress_percent": 73,
  "downloaded_bytes": 4402341478,
  "total_bytes": 6012954214,
  "download_speed_bps": 19293798,
  "eta_seconds": 85,
  "seeds": 12,
  "peers": 4,
  "updated_at": "2026-07-17T18:23:05Z"
}
```

The object is optional for queued jobs, non-download stages, legacy jobs, and
sources that have not produced a checkpoint. Individual fields are optional.
The service validates persisted checkpoint values before exposing them:

- percentage is in `0..=100`;
- byte and rate fields are non-negative integers;
- downloaded bytes do not exceed total bytes when both are present;
- provider-specific fields are bounded and unknown provider values are omitted;
- timestamps come from service persistence time, not a runner-controlled string.

No source locator, torrent hash, filesystem path, credential, signed URL, or raw
provider payload is included.

## Prowlarr and qBittorrent

The existing qBittorrent `torrents/info` response is extended to deserialize:

- `downloaded` and `size`;
- `dlspeed` and `eta`;
- `num_seeds` and `num_leechs`;
- the existing `progress`, `amount_left`, and `state` fields.

The runner maps each two-second monitor snapshot to the common progress model.
It persists at most one checkpoint every five seconds, plus the first snapshot,
meaningful state changes, and the final 100% snapshot. qBittorrent sentinel or
unknown integer values are omitted instead of being converted into misleading
numbers.

## Rezka

For direct MP4 transfers, the HTTP adapter reports byte milestones from the
existing streaming loop. Total bytes come from the source-size probe when that
probe succeeds. Speed is calculated from monotonic elapsed time over a bounded
window, percentage is derived only when total bytes are known, and ETA is
derived only when both remaining bytes and a positive measured speed exist.
Resumed downloads include the already persisted partial bytes.

For HLS ingestion, the runner reports the reliably observable subset. Downloaded
bytes and speed may be sampled from the growing staging file. Percentage and ETA
are exposed only when ffmpeg provides trustworthy media progress and the expected
duration is known; otherwise those fields remain absent. The initial
implementation must prefer omission over an estimate based solely on a title's
nominal duration.

The progress reporter is a runner port rather than a callback tied to HTTP, so
MP4, HLS, and future source implementations share rate limiting and checkpoint
serialization.

## Persistence and Reads

The existing `StageCheckpoint` event and `job_stages.checkpoint` JSONB column are
reused. No new progress table is required. The running `download` or
`torrent_monitor` stage stores a normalized checkpoint. `find_detail_for_owner`
reads the latest running stage and its checkpoint in one query and converts it
to the domain model. Terminal jobs may expose the final checkpoint only when it
is useful and valid; `current_stage` remains absent after completion.

Checkpoint events remain internal progress events. They are persisted in the
event/outbox history for diagnostics but do not match notification rules and do
not update the Telegram status card.

## CLI and Hermes

`media jobs get JOB_ID --json` returns the new object without changing the
command shape. The human renderer prints only present values using binary byte
units and a compact duration.

The shared Hermes media skill instructs both profiles to:

- use the returned progress instead of describing only the stage;
- format a ten-cell text progress bar when a percentage is present;
- show downloaded/total size, speed, ETA, torrent peers, and checkpoint age;
- say that an exact percentage is unavailable when it is absent;
- never expose raw JSON or invent missing values.

## Failure Handling

Progress is best-effort and must never fail a download. A transient checkpoint
delivery failure is logged and retried by the next rate-limited observation.
Malformed or stale checkpoint data is ignored by the read path while job state
and stage remain available. Provider polling failures retain the current runner
failure behavior and do not return stale provider errors as progress.

## Verification

Coverage includes:

- qBittorrent response parsing, sentinel handling, and mapping;
- MP4 progress calculation, resume behavior, rate limiting, and overflow bounds;
- HLS omission rules for unknown percentage and ETA;
- runner checkpoint event delivery without job failure;
- storage parsing, owner isolation, and malformed-checkpoint rejection;
- API and CLI JSON/human rendering;
- Hermes skill contract and wrapper compatibility;
- a live Prowlarr download smoke test showing changing percentage and speed;
- a live Rezka download smoke test showing the reliable available fields;
- confirmation that no extra Telegram messages are emitted by checkpoints.

## Non-goals

- Continuous progress pushes to Telegram.
- Direct qBittorrent access from `media-service` or Hermes.
- Historical speed graphs or long-term transfer analytics.
- Exact progress for a source that cannot provide trustworthy total work.
