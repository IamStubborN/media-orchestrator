# yt-dlp Media Transfer Design

## Goal

Use `yt-dlp` as the single HTTP media transfer engine for Rezka and future web
providers such as VK Video. Keep provider discovery and selection outside the
transfer engine, and keep torrent downloads in qBittorrent.

## Boundaries

- Rezka client: authentication, search, translations, episodes, subtitles, and
  signed stream resolution.
- Future provider adapters: convert a provider selection into a downloadable
  URL or a provider page URL plus bounded request metadata.
- `yt-dlp` adapter: download HTTP media, resume partial files, retry transient
  failures, report progress, and support cancellation.
- qBittorrent adapter: torrent transfer and seeding.
- ffmpeg adapter: media probing and Rezka-only VAAPI upscale/transcode.

The domain pipeline depends on a `MediaTransferPort`; it does not depend on the
`yt-dlp` executable directly. No `vsd` dependency is introduced.

## Transfer Policy

The runner invokes a pinned `yt-dlp` executable with configuration loading
disabled, one item per invocation, continuation enabled, 20 request and fragment
retries, bounded exponential retry delays, a 30-second socket timeout, and four
concurrent HLS fragments. The output and temporary files remain in the existing
job staging directory so a retried job can resume safely.

Progress is read only from a dedicated machine-formatted stdout prefix. Raw
provider output is bounded and redacted before logging. Signed URLs, cookies,
headers, and credentials must never enter checkpoints, errors, or logs.

## Pipeline

Both direct MP4 and HLS inputs use the same transfer port. Short source probes
and subtitle sidecar downloads continue to use the bounded reqwest adapter.
After transfer, the existing ffprobe validation, Rezka VAAPI upscale, subtitle
recovery, atomic Plex publication, and Plex reconciliation remain unchanged.

Future VK support adds a provider adapter but reuses this transfer port. VK
media is preserved as delivered unless a separate compatibility policy requires
remuxing; Rezka continues to use the existing mandatory 1080p VAAPI path.

## Failure Handling

- Cancellation terminates the child process and returns `Cancelled`.
- Authentication or expired signed-stream failures return `SourceExpired` so
  the existing Rezka session/stream refresh path remains effective.
- Deterministic unsupported or unavailable media returns
  `SourceTransferRejected`.
- Network, CDN, fragment, and unknown process failures return
  `SourceTransferTransient` and use the existing job retry/VPN policy.
- A successful process without a non-empty output is treated as a transient
  transfer failure.

## Deployment

The runner image downloads the official standalone `yt-dlp` release pinned to
`2026.07.04` and verifies its SHA-256 checksum. The service image does not
contain `yt-dlp`. Deployment waits for the media queue to be idle before
replacing the runner.

