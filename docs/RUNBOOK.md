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

## Rezka Session

Rezka sessions are anonymous cookie jars owned by `rezka-client`. The client
does not use Chromium, Playwright, Obscura, copied browser cookies, or a DLE
login. Search and download establish a session automatically:

1. The configured probe contains exactly one marker class: valid or invalid.
   Invalid (login form present) is the expected anonymous state.
2. Anubis, when present, is solved at most once before the next probe.
3. The encrypted cookie snapshot is saved after a conclusive probe. No account
   credentials are sent.

The deployed default probe is the Rezka root, with `logout` as the valid marker
and `login` as the invalid marker. If the provider changes either marker, expect
a sanitized `provider_response_invalid` result. Inspect the runner error code
and refresh the fixtures and parser together; do not add a manual browser-cookie
fallback or a login path.

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

### Export a release contract

From a clean private checkout, export the immutable release metadata after the
service image and runner image have been built. Extract the Linux CLI into the
repository's ignored `dist/` directory so the clean-worktree gate remains true:

```sh
mise run extract-linux-cli
python3 scripts/export-release-contract.py \
  --service-image 'registry.example/media-service@sha256:<64-lowercase-hex>' \
  --runner-image 'registry.example/media-runner@sha256:<64-lowercase-hex>' \
  --migration-version m20260810_000040_tracking_claims \
  --cli dist/media-linux-amd64 \
  --cli-checksum dist/media-linux-amd64.sha256 \
  --output /private/path/media-release
```

The exporter regenerates the MCP schema, checks it against
`config/media-capabilities.json`, verifies the CLI checksum, and writes
`release.json`, `MCP_SCHEMA.json`, `media-capabilities.json`, and
`media-linux-amd64.sha256` atomically. The destination must not exist unless
`--replace` is supplied. Export only creates this local bundle; it does not
publish, push, log in, or deploy.

## Safe Deployment

The normal rollout is service-only:

```sh
mise run homelab-deploy
```

This command consumes `$MEDIA_RELEASE_DIR/release.json`, pulls its immutable
service image reference, and preserves the running runner only when its exact
manifest reference and runner-build digest match. The explicit full rollout
pulls and deploys both manifest references. Neither path builds local images.
For an intentional source-checkout build instead, use
`./scripts/homelab.sh deploy-local-service` or
`./scripts/homelab.sh deploy-local-full`; these are separate operator commands.
Release commands copy the four validated candidate files into one private
snapshot after acquiring the host lock and use only that snapshot through the
operation, so a concurrent exporter replacement cannot mix bundle generations.
After migration, the database must report the manifest's exact registered
migration before the service is activated. Status, verification, and rollback
do not require a candidate release directory; rollback uses its remote
checkpointed schema and images.

It first runs the fail-closed `hermes-home/scripts/check-media-capabilities`
schema/capability check and compares the release manifest's runner build digest
with the live image's `dev.iamstubborn.media.runner-build-digest` label. The
guard also normalizes the live and candidate Compose files and compares the
runner, Gluetun, watcher, networks, and volumes. Any runner-impacting source or
runtime change fails closed with an explicit `deploy-full` instruction. An old
image without the runner digest therefore requires one full rollout before
service-only deployment is available.

The build stamps the commit, visibly dirty Git version, and a source-tree digest
of the exact Docker build context into the service image and its local tag.
The context digest applies `.dockerignore`, includes the Dockerfile and ignore
file themselves, and includes tracked or untracked configuration and toolchain
inputs that Docker can send. Deployment verifies those labels, resolves the
built tag to an immutable image ID, and later checks the running service. Before
synchronizing Compose, it atomically checkpoints the current image references,
immutable image IDs and digest labels together with the deployed MCP schema
artifact, OCI revisions, exact Compose, and applied database migration, then runs
migrations, and recreates only `media-service`. It refuses to run while a job is
active. Immediately before mutation it fences idleness again, stops the watcher
and runner, and keeps them quiesced through success or recovery. It does not
build or recreate `download-runner`; bounded resume must retain the same runner
and watcher container IDs and prove the existing runner can reach the new
service. PostgreSQL, qBittorrent, and both VPN containers remain untouched. Use
`mise run homelab-rollback` for the matching service-only rollback.

If any service-only step after the checkpoint fails, the same invocation
restores the checkpointed Compose, service image ID, migration, MCP schema,
Hermes sources, and Hermes/notifier image IDs before returning failure.

The service-only rollback is deliberately at most one schema step. While the
queue is idle, the still-current forward image verifies the live migration. If
the checkpoint is its immediate predecessor, the workflow executes that
migration's real SeaORM `down` implementation; if both versions are equal, it
skips migration down and still verifies the checkpointed version. Only after
the database is verified at the checkpoint does the old service image start and
the paired Hermes schema return. Missing, malformed, unknown, or multi-step
version transitions fail closed. If schema rollback, old-service
startup, or MCP verification fails, the workflow restores the forward schema,
runs the forward image's migrations back to the captured forward version,
restarts that image, and verifies the protected container snapshot before
returning failure.

Every invocation requires explicit `HOMELAB_ROOT` and `MEDIA_RELEASE_DIR`
values. `HERMES_HOME_ROOT` defaults only to `$HOMELAB_ROOT/hermes`; set it for a
different checkout root, or set `HERMES_CAPABILITY_CHECKER` for an explicit
checker path. The MCP schema synchronized by the guarded deploy comes from
`$MEDIA_RELEASE_DIR/MCP_SCHEMA.json`. A missing checker, bundle schema, or
schema mismatch aborts before any image build or container operation.

The Hermes-only rollout stages the complete source tree and extracted CLI
off-live, then checkpoints images, Compose, schema, and mounted sources before
activating mounted sources. If activation, container health, MCP comparison, or
mount attestation fails, it restores the exact checkpointed Hermes/notifier
image IDs and sources without recreating media-service or download-runner.

`mise run homelab-deploy-full` and `mise run homelab-rollback-full` are explicit
operator-only workflows for changes that genuinely require the runner and
Hermes artifacts to move together. They retain the idle-job guard and the full
health sequence below. Every deploy and rollback command holds a host-wide
deployment lock for its complete operation.

1. Confirm no job is active before replacing the runner. `queued` is safe;
   `leased`, `running`, `publishing`, `plex_pending`, or `cancel_requested` is not.
2. Build and attest both tags, then resolve them to immutable image IDs.
3. Extract the Linux `media` binary, verify its SHA-256, and stage the complete
   Hermes tree and CLI off-live. Pull the staged Compose images without changing
   mounted live sources or containers.
4. Checkpoint the old image IDs and digest labels, migration, exact deployed MCP
   schema, Compose and Hermes source snapshot, plus Hermes and notifier image
   references and IDs.
5. Apply a final idle fence, stop the watcher, require lifecycle `ready` with no
   active job, and stop the runner. Both remain quiesced until the rollout has
   succeeded or the exact checkpoint has been restored.
6. Activate the staged Hermes tree and synchronize Compose inside this protected
   transaction. Update only the two image lines in the private root `.env`.
7. Run the new service image as a one-shot `media migrate` container. Stop the
   rollout if migration fails.
8. Recreate `media-service` and wait for health.
9. Recreate `download-runner` and require a new healthy generation or clean exit.
10. Recreate
    both profiles, and wait for both health checks.
11. Verify lifecycle `ready`, watcher health, exact running image attestations,
    live MCP `tools/list`, mounted Hermes and notifier source hashes, queue state,
    and a bounded runner iteration window with no `Service` compatibility error.

The bounded watcher-readiness gate remains inside the transaction. A timeout or
post-resume verification failure re-quiesces watcher and runner before restoring
the checkpoint or forward snapshot. The protected container set includes
PostgreSQL, qBittorrent, and both `gluetun` and `gluetun-rezka`; their identities,
start times, and health must remain unchanged.

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
IDs, start times, and healthy states throughout.

Full rollback is explicit and transactional. Before mutation it captures the
forward service and runner image IDs, migration version, MCP schema, Compose,
Hermes sources, and exact Hermes/notifier image references. It migrates down by
at most one version, restores both
checkpointed images plus the exact Compose and Hermes source snapshot, then
verifies live `tools/list`, container mounts, and runner compatibility. On
failure, it automatically restores the forward full stack, migrates back to the
captured forward version, recreates Hermes consumers, and re-verifies MCP.
PostgreSQL, both Gluetun containers, and qBittorrent
container identities and health must remain unchanged through either path. Do
not roll back PostgreSQL migrations by deleting data.

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
