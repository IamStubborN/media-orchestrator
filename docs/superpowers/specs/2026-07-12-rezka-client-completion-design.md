# Rezka Client Completion Design

## Goal

Complete the first-party Rezka integration for conversational discovery, authenticated premium playback, storage-aware downloads, HLS fallback, and simple single-host configuration.

## Configuration

The homelab deployment uses one Git-ignored `.env` file with mode `0600`. Runtime secrets may be supplied directly as environment variables. Existing `_FILE` variables remain supported and take precedence so file-backed deployments remain possible. The repository contains only `.env.example` with non-secret placeholders.

## Rezka Discovery

`RezkaClient` gains bounded operations for quick search, premium-session detection, catalog filtering, detailed title metadata, franchise navigation, trailer lookup, and stream-size probing. All provider text, URLs, result counts, response sizes, redirects, and timeouts remain bounded by the existing transport and redaction policies.

The conversational priority is: quick search, premium status, stream size, metadata, catalog filters, franchises, then trailers. Raw cookie import, provider-specific proxy configuration, analytics cookies, and a standalone downloader are excluded.

## HLS Downloads

MP4 remains preferred. When the selected quality has no MP4 endpoint, the runner accepts its HLS endpoint and invokes the existing pinned `ffmpeg` process adapter to ingest and remux the playlist into a local staging file. The normal probe, VAAPI transcode, subtitle recovery, publication, and Plex verification pipeline then continues unchanged.

The runner does not implement an HLS protocol stack. FFmpeg owns master/media playlists, encryption supported by FFmpeg, discontinuities, and segment retries. Cancellation and process timeouts continue through `ProcessPort`.

## Authentication E2E

The acceptance flow is Telegram command, unique native approval, one-time Vaultwarden credential resolution, same-origin Rezka DLE AJAX login, persisted browser restore state, browser restart, and a DOM check proving the anonymous login control is absent. Passwords never enter the model, Telegram, command arguments, logs, or audit records.

## Verification

Focused unit and integration tests cover direct secret precedence, bounded provider parsing, HLS endpoint selection and command construction, and redaction. The final deployment check verifies service health and the complete Telegram-to-Rezka login path.
