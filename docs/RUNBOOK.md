# Media Orchestrator Runbook

This runbook covers the live deployment at `host.example.invalid`. It
does not contain credentials, cookies, signed URLs, or Telegram tokens.

## Runtime Layout

```text
homelab root: /srv/homelab
compose file: /srv/homelab/media/compose.media-orchestrator.yml
environment: /srv/homelab/.env
project: media-orchestrator
```

`media-service` remains outside VPN namespaces. `download-runner` shares the
dedicated `gluetun-rezka` namespace and exits after one job. The
`gluetun-rezka-watcher` rotates the VPN before starting the next runner process.
qBittorrent and Prowlarr use their existing independent Gluetun namespace.

## Status

The repository exposes the operational commands through `mise`:

```sh
mise run homelab-status
mise run homelab-verify
mise run homelab-deploy
mise run homelab-rollback
```

Deploy and rollback refuse to replace runtime containers while a job is active.

```sh
ssh host.example.invalid \
  "docker ps -a --format '{{.Names}} {{.Image}} {{.Status}}' | \
   grep -E '^(media-service|download-runner|gluetun-rezka|gluetun-rezka-watcher|media-postgres|qbittorrent|prowlarr)'"
```

Inspect lifecycle and recent jobs without reading secrets:

```sh
ssh host.example.invalid \
  "docker exec media-postgres sh -lc 'psql -U \"\$POSTGRES_USER\" -d \"\$POSTGRES_DB\" -c \
  \"select state,previous_ip,current_ip,updated_at from runner_lifecycle; \
    select id,provider,state,attempt_count,updated_at from jobs order by created_at desc limit 10;\"'"
```

Hermes-facing queue availability is available without direct database access:

```sh
ssh host.example.invalid \
  'docker exec hermes-primary hermes-media queue status --json'
```

Interpret `runner_state` as follows:

- `ready`: queued work may lease immediately;
- `rotating`: the dedicated Rezka VPN is changing before the next job;
- `blocked`: queued work is parked. Inspect the sanitized `blocked_reason`;
  `vpn_rotation_failed` means bounded IP-rotation attempts were exhausted.

Do not infer availability from `active=false` alone. An idle ready runner and a
blocked runner both have no active lease, but require different operator action.

## Rezka Session Authentication

Rezka login is owned by `rezka-client` inside the runner and does not use
Chromium, Playwright, Obscura, or copied browser cookies. A session refresh must
follow this observable contract:

1. The configured probe contains exactly one marker class: valid or invalid.
2. Anubis, when present, is solved at most once before the next probe.
3. DLE login posts only to the selected HTTPS origin at `/ajax/login/`.
4. A current successful response may be HTTP 200 with body `Redirect`, HTTP 200
   with success JSON, or an HTTP redirect. Every accepted shape must also set a
   new `PHPSESSID`; a stale pre-existing cookie is insufficient.
5. The final probe must contain a valid marker and no invalid marker before the
   encrypted snapshot is saved.

The deployed default probe is the Rezka root, with `logout` as the valid marker
and `login` as the invalid marker. If the provider changes either marker or the
DLE response shape, expect a sanitized `provider_response_invalid` or
`authentication_required` result. Inspect the runner error code and refresh the
fixtures and parser together; do not add a manual browser-cookie fallback.

Refresh through the owner-scoped Hermes wrapper:

```sh
ssh host.example.invalid \
  'docker exec hermes-primary hermes-media rezka session refresh \
   --credential-request REQUEST_ID --json'
```

Credential-backed refresh requires the existing one-time Vaultwarden approval
flow. `REQUEST_ID` is the approved one-time broker request, not a credential;
the command returns a normal job whose terminal status is inspected through
`hermes-media jobs show JOB_ID --json`. Never place the username, password,
cookie, or resolved credential in a shell command, job payload, or evidence
file.

## Local Build

Manual deployment is the preferred and supported release path. The repository's
GitHub Actions workflows are intentionally disabled and must not deploy the
homelab. Run builds, migrations, guarded service replacement, and verification
from a trusted operator workstation.

Build immutable images directly on the Docker host without GitHub Actions:

```sh
revision=$(git rev-parse --short HEAD)
DOCKER_HOST=ssh://host.example.invalid \
  docker build --target service \
  -t "media-orchestrator-service:local-$revision" .
DOCKER_HOST=ssh://host.example.invalid \
  docker build --target runner \
  -t "media-orchestrator-runner:local-$revision" .
```

The runner package layer is independent of the application binary, so ffmpeg,
VAAPI packages, and the pinned checksum-verified `yt-dlp` executable remain
cached across normal Rust-only changes.

## Safe Deployment

The normal rollout is service-only:

```sh
mise run homelab-deploy
```

It first runs the fail-closed `hermes-home/scripts/check-media-capabilities`
schema/capability check, builds only the service target, verifies that both
currently configured image tags still exist, atomically checkpoints their exact
values together with the deployed MCP schema artifact, OCI revisions, and the
exact applied database migration version, runs
migrations, and recreates only `media-service`. It refuses to run while a job is
active. It does not build or
recreate `download-runner`, and it does not stop or restart the watcher,
qBittorrent, or either VPN container. Use `mise run homelab-rollback` for the
matching service-only rollback.

The service-only rollback is deliberately one schema step. While the queue is
idle, the still-current forward image verifies that the live migration is its
exact latest migration and that the checkpoint is its immediate predecessor,
then executes that migration's real SeaORM `down` implementation. Only after
the database is verified at the checkpoint does the old service image start and
the paired Hermes schema return. Missing, malformed, equal, unknown, or
multi-step version transitions fail closed. If schema rollback, old-service
startup, or MCP verification fails, the workflow restores the forward schema,
runs the forward image's migrations back to the captured forward version,
restarts that image, and verifies the protected container snapshot before
returning failure.

The checker is discovered through sibling `../hermes-home` by default. Set
`HERMES_HOME_ROOT` for a different checkout root, or
`HERMES_CAPABILITY_CHECKER` for an explicit checker path. A missing checker or
schema mismatch aborts before any image build or container operation.

`mise run homelab-deploy-full` and `mise run homelab-rollback-full` are explicit
operator-only workflows for changes that genuinely require the runner and
Hermes artifacts to move together. They retain the idle-job guard and the full
health sequence below.

1. Confirm no job is active before replacing the runner. `queued` is safe;
   `leased`, `running`, `publishing`, `plex_pending`, or `cancel_requested` is not.
2. Build both immutable image tags.
3. Extract the Linux `media` binary from the service image and verify its
   SHA-256. Hermes uses the official image; `hermes-home` synchronizes and
   mounts the CLI, skills, profile configuration, and notification adapter.
4. Record the current `MEDIA_SERVICE_IMAGE` and `DOWNLOAD_RUNNER_IMAGE` values.
5. Update only those two lines in the private root `.env`.
6. Run the new service image as a one-shot `media migrate` container. Stop the
   rollout if migration fails.
7. Recreate `media-service` and wait for health.
8. Stop `gluetun-rezka-watcher` only while lifecycle is `ready`.
9. Recreate `download-runner`, wait for health, then start the watcher again.
10. Pull the official Hermes image, synchronize mounted extensions, recreate
    both profiles, and wait for both health checks.
11. Verify lifecycle `ready`, watcher health, image tags, queue state, and logs.

Never replace the runner merely to deploy documentation or service-only changes.
For an active torrent job, qBittorrent can continue independently, but runner
replacement must be an explicit recovery test rather than the normal deploy path.

## Interrupted VPN Rotation

If the watcher was stopped while lifecycle is `rotating`, the API correctly
refuses to lease queued jobs. Recovery:

1. Ensure `gluetun-rezka` becomes healthy.
2. Start `gluetun-rezka-watcher`.
3. Wait until `runner_lifecycle.state = ready` and `download-runner` is running.
4. Do not manually edit lifecycle rows.

```sh
ssh host.example.invalid \
  'docker start gluetun-rezka-watcher >/dev/null'
```

The watcher records the current public IP and completes the lifecycle transition
through the narrow internal API.

The service permits up to three attempts for the same job on one VPN session.
It then returns `vpn_rotation_required` before issuing another lease. A different
queued job requires rotation immediately. The watcher reads this durable decision,
rotates Gluetun only when required, and otherwise restarts the one-attempt runner
on the current session. The provider-specific stage limit remains 20 attempts for
Rezka.

## Automatic Episode Downloads

Tracking remains notification-only unless download parameters are explicitly
present. Inspect subscriptions with `hermes-media tracking list --json`. An
automatic subscription must contain `provider_media_ref`, `translation_id`,
and `season`. When a new provider episode is discovered, `media-service`
creates an episode job directly from the scheduler; Hermes and the LLM are not
involved. Use `tracking enable-download` to configure an existing subscription.
Use `tracking set-baseline` to correct the known-through episode without
recreating a subscription, and `tracking check-now` to make it due for the next
scheduler pass. Notification-only subscriptions run hourly; automatic-download
subscriptions run every 15 minutes. `tracking list --json` reports the last
check result and the next scheduled check.
Never infer or switch the source or translation automatically.

## Stuck Runner Recovery

If a runner dies during a job, do not alter the job row. qBittorrent downloads
continue independently, and Rezka staging remains durable. Start the intended
runner image and allow the lease to expire. The next lease attempt increments
`attempt_count` and resumes from the durable provider/staging state.

An exact Prowlarr info hash is reused idempotently. Rezka HTTP transfers resume
through `yt-dlp` staging files, completed video is not re-encoded during
subtitle-only recovery, and published files are never automatically deleted.

## Plex Pending

Check that the target path belongs to the configured Plex section before
retrying. Current required roots include:

```text
TV: /data/internal/media/rezka/tv
Movies: /data/internal/media/rezka/movies
Torrents TV: /data/internal/torrents/tv
Torrents Movies: /data/internal/torrents/movies
```

Refresh only the affected section. A completed job requires an exact Plex Part
path match, not merely a title match.

## Rollback

The default rollback restores the service image and its exactly paired Hermes
MCP schema, recreates Hermes/notifier consumers to invalidate cached tool
schemas, and compares the live `tools/list` response with the restored artifact.
On any mismatch it automatically restores the forward service and schema. The
runner, watcher, qBittorrent, and VPN containers must retain identical container
IDs, start times, and healthy states throughout. Full rollback is explicit and
restores both images. Do not
roll back PostgreSQL migrations
by deleting data. If the old service cannot read the current schema, stop and
roll forward with a compatible image instead.

## Logs

```sh
ssh host.example.invalid 'docker logs --since 10m media-service'
ssh host.example.invalid 'docker logs --since 10m download-runner'
ssh host.example.invalid 'docker logs --since 10m gluetun-rezka-watcher'
```

Logs may contain safe job IDs and static error codes. They must never contain
cookies, credentials, signed media URLs, magnet URIs, API keys, or raw provider
response bodies.
Jobs parked in `blocked_storage` do not hold a runner lease and do not prevent a
deployment. The deployment guard still refuses replacement while any job is
`leased`, `running`, `cancel_requested`, `publishing`, or `plex_pending`.
