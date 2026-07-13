#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
host=${MEDIA_HOMELAB_HOST:host.example.invalid}
remote_root=${MEDIA_HOMELAB_ROOT:-/srv/homelab}
compose_file=$remote_root/media/compose.media-orchestrator.yml
environment_file=$remote_root/.env
rollback_file=$remote_root/media/.media-orchestrator-images.previous
hermes_root=${HERMES_HOME_ROOT:-$root/../hermes-home}
hermes_remote_root=${HERMES_HOME_REMOTE_ROOT:-/home/operator/hermes-home}

usage() {
    echo "usage: $0 status|verify|deploy|rollback" >&2
    exit 2
}

remote() {
    # Arguments intentionally form the bounded command evaluated by the remote shell.
    # shellcheck disable=SC2029
    ssh "$host" "$@"
}

status() {
    remote "docker ps -a --format '{{.Names}} {{.Image}} {{.Status}}' | grep -E '^(media-service|download-runner|gluetun-rezka|gluetun-rezka-watcher|media-postgres|qbittorrent|prowlarr)'"
    remote "docker exec media-postgres sh -lc 'psql -U \"\$POSTGRES_USER\" -d \"\$POSTGRES_DB\" -c \"select state,previous_ip,current_ip,updated_at from runner_lifecycle; select id,provider,state,attempt_count,updated_at from jobs order by created_at desc limit 5;\"'"
}

verify() {
    remote sh -s <<'REMOTE'
set -eu
healthy() {
    attempts=0
    while :; do
        state=$(docker inspect "$1" --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}')
        test "$state" = healthy && return
        attempts=$((attempts + 1))
        test "$attempts" -lt 30 || {
            echo "$1 is not healthy: $state" >&2
            exit 1
        }
        sleep 5
    done
}
healthy media-postgres
healthy media-service
healthy gluetun-rezka
healthy gluetun-rezka-watcher
lifecycle=$(docker exec media-postgres sh -lc 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc "select state from runner_lifecycle;"')
test "$lifecycle" = ready || {
    echo "runner lifecycle is not ready: $lifecycle" >&2
    exit 1
}
runner_state=$(docker inspect download-runner --format '{{.State.Status}}')
case $runner_state in
    running | exited) ;;
    *) echo "download-runner has unexpected state: $runner_state" >&2; exit 1 ;;
esac
echo "homelab media verification passed"
REMOTE
}

assert_no_active_job() {
    states=$(remote "docker exec media-postgres sh -lc 'psql -U \"\$POSTGRES_USER\" -d \"\$POSTGRES_DB\" -Atc \"select state from jobs;\"'")
    active=$(printf '%s\n' "$states" | grep -Ec '^(leased|running|cancel_requested|publishing|plex_pending)$' || true)
    test "$active" = 0 || {
        echo "refusing runtime replacement while $active job is active" >&2
        exit 1
    }
}

replace_images() {
    service_image=$1
    runner_image=$2
    remote "set -eu; sed -i 's#^MEDIA_SERVICE_IMAGE=.*#MEDIA_SERVICE_IMAGE=$service_image#; s#^DOWNLOAD_RUNNER_IMAGE=.*#DOWNLOAD_RUNNER_IMAGE=$runner_image#' '$environment_file'; cd '$remote_root/media'; docker compose --env-file '$environment_file' -f '$compose_file' run --rm --no-deps media-service migrate; docker compose --env-file '$environment_file' -f '$compose_file' up -d --no-deps --force-recreate media-service; docker stop gluetun-rezka-watcher >/dev/null; docker compose --env-file '$environment_file' -f '$compose_file' up -d --no-deps --force-recreate download-runner; docker start gluetun-rezka-watcher >/dev/null"
    verify
}

prepare_hermes_cli() {
    service_image=$1
    docker_host=$2
    media_version=0.1.0
    artifact=$hermes_root/artifacts/media-$media_version-linux-amd64
    container=$(DOCKER_HOST=$docker_host docker create "$service_image")
    trap 'DOCKER_HOST=$docker_host docker rm -f "$container" >/dev/null 2>&1 || true' EXIT HUP INT TERM
    DOCKER_HOST=$docker_host docker cp "$container:/usr/local/bin/media" "$artifact"
    DOCKER_HOST=$docker_host docker rm "$container" >/dev/null
    trap - EXIT HUP INT TERM
    chmod 0755 "$artifact"
    checksum=$(shasum -a 256 "$artifact" | awk '{print $1}')

    scp "$hermes_root/Dockerfile" "$host:$hermes_remote_root/Dockerfile.next" >/dev/null
    scp "$hermes_root/scripts/hermes-media" "$host:$hermes_remote_root/scripts/hermes-media.next" >/dev/null
    scp "$hermes_root/shared/skills/media/SKILL.md" "$host:$hermes_remote_root/shared/skills/media/SKILL.md.next" >/dev/null
    scp "$artifact" "$host:$hermes_remote_root/artifacts/media-$media_version-linux-amd64.next" >/dev/null
    remote "set -eu; install -m 0644 '$hermes_remote_root/Dockerfile.next' '$hermes_remote_root/Dockerfile'; rm '$hermes_remote_root/Dockerfile.next'; install -m 0755 '$hermes_remote_root/scripts/hermes-media.next' '$hermes_remote_root/scripts/hermes-media'; rm '$hermes_remote_root/scripts/hermes-media.next'; install -m 0644 '$hermes_remote_root/shared/skills/media/SKILL.md.next' '$hermes_remote_root/shared/skills/media/SKILL.md'; rm '$hermes_remote_root/shared/skills/media/SKILL.md.next'; install -m 0755 '$hermes_remote_root/artifacts/media-$media_version-linux-amd64.next' '$hermes_remote_root/artifacts/media-$media_version-linux-amd64'; rm '$hermes_remote_root/artifacts/media-$media_version-linux-amd64.next'; sed -i 's#^MEDIA_CLI_SHA256=.*#MEDIA_CLI_SHA256=$checksum#' '$hermes_remote_root/.env'; cd '$hermes_remote_root'; docker compose --env-file .env build hermes-primary"
}

replace_hermes_agents() {
    remote "set -eu; cd '$hermes_remote_root'; docker compose --env-file .env up -d --no-deps --force-recreate hermes-primary hermes-secondary"
    remote sh -s <<'REMOTE'
set -eu
for name in hermes-primary hermes-secondary; do
    attempts=0
    while :; do
        state=$(docker inspect "$name" --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}')
        test "$state" = healthy && break
        attempts=$((attempts + 1))
        test "$attempts" -lt 30 || {
            echo "$name is not healthy: $state" >&2
            exit 1
        }
        sleep 5
    done
done
REMOTE
}

deploy() {
    assert_no_active_job
    revision=$(git -C "$root" rev-parse --short HEAD)
    service_image=media-orchestrator-service:local-$revision
    runner_image=media-orchestrator-runner:local-$revision
    docker_host=${MEDIA_DOCKER_HOST:-ssh://$host}
    (
        cd "$root"
        DOCKER_HOST=$docker_host \
            MEDIA_SERVICE_IMAGE=$service_image \
            MEDIA_RUNNER_IMAGE=$runner_image \
            ./scripts/docker-build.sh
    )
    prepare_hermes_cli "$service_image" "$docker_host"
    remote "set -eu; umask 077; grep -E '^(MEDIA_SERVICE_IMAGE|DOWNLOAD_RUNNER_IMAGE)=' '$environment_file' >'$rollback_file'"
    replace_images "$service_image" "$runner_image"
    replace_hermes_agents
}

rollback() {
    assert_no_active_job
    previous=$(remote "cat '$rollback_file'")
    service_image=$(printf '%s\n' "$previous" | sed -n 's/^MEDIA_SERVICE_IMAGE=//p')
    runner_image=$(printf '%s\n' "$previous" | sed -n 's/^DOWNLOAD_RUNNER_IMAGE=//p')
    test -n "$service_image" && test -n "$runner_image" || {
        echo "rollback image record is incomplete" >&2
        exit 1
    }
    replace_images "$service_image" "$runner_image"
}

case ${1:-} in
    status) status ;;
    verify) verify ;;
    deploy) deploy ;;
    rollback) rollback ;;
    *) usage ;;
esac
