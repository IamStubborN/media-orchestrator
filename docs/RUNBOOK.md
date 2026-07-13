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

## Local Build

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

The runner package layer is independent of the application binary, so ffmpeg
and VAAPI packages remain cached across normal Rust-only changes.

## Safe Deployment

1. Confirm no job is active before replacing the runner. `queued` is safe;
   `leased`, `running`, `publishing`, `plex_pending`, or `cancel_requested` is not.
2. Build both immutable image tags.
3. Record the current `MEDIA_SERVICE_IMAGE` and `DOWNLOAD_RUNNER_IMAGE` values.
4. Update only those two lines in the private root `.env`.
5. Recreate `media-service` and wait for health.
6. Stop `gluetun-rezka-watcher` only while lifecycle is `ready`.
7. Recreate `download-runner`, wait for health, then start the watcher again.
8. Verify lifecycle `ready`, watcher health, image tags, queue state, and logs.

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

## Stuck Runner Recovery

If a runner dies during a job, do not alter the job row. qBittorrent downloads
continue independently, and Rezka staging remains durable. Start the intended
runner image and allow the lease to expire. The next lease attempt increments
`attempt_count` and resumes from the durable provider/staging state.

An exact Prowlarr info hash is reused idempotently. Rezka range downloads reuse a
valid partial, completed video is not re-encoded during subtitle-only recovery,
and published files are never automatically deleted.

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

Rollback means restoring the two previously recorded immutable image tags and
repeating the safe deployment sequence. Do not roll back PostgreSQL migrations
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
