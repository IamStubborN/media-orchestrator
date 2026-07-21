# yt-dlp Media Transfer Implementation Plan

**Status:** Implemented and live-verified on 2026-07-21.

**Goal:** Replace reqwest/ffmpeg video ingestion with one bounded `yt-dlp`
transfer adapter while preserving the existing Rezka processing pipeline.

1. Add transfer-port contract and red tests for arguments, progress, failure
   classification, output validation, and cancellation.
2. Implement `YtDlpTransferAdapter` and make focused adapter tests green.
3. Route direct and HLS EpisodePipeline media downloads through the transfer
   port; retain reqwest for probes and subtitles.
4. Pin and checksum the official `yt-dlp` runner binary in the Docker image.
5. Run formatting, focused tests, workspace tests, clippy, and image smoke
   checks.
6. Wait for an idle queue, deploy the runner image, and verify a real Rezka
   transfer through Gluetun without exposing signed URLs.
