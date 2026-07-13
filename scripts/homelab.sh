#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
host=${MEDIA_HOMELAB_HOST:host.example.invalid}
remote_root=${MEDIA_HOMELAB_ROOT:-/srv/homelab}
compose_file=$remote_root/media/compose.media-orchestrator.yml
environment_file=$remote_root/.env
rollback_file=$remote_root/media/.media-orchestrator-images.previous

usage() {
    echo "usage: $0 status|verify|deploy|rollback" >&2
    exit 2
}

remote() {
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
    state=$(docker inspect "$1" --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}')
    test "$state" = healthy || {
        echo "$1 is not healthy: $state" >&2
        exit 1
    }
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
    active=$(printf '%s\n' "$states" | grep -Ec '^(leased|running|cancel_requested|blocked_storage|publishing|plex_pending)$' || true)
    test "$active" = 0 || {
        echo "refusing runtime replacement while $active job is active" >&2
        exit 1
    }
}

replace_images() {
    service_image=$1
    runner_image=$2
    remote "set -eu; sed -i 's#^MEDIA_SERVICE_IMAGE=.*#MEDIA_SERVICE_IMAGE=$service_image#; s#^DOWNLOAD_RUNNER_IMAGE=.*#DOWNLOAD_RUNNER_IMAGE=$runner_image#' '$environment_file'; cd '$remote_root/media'; docker compose --env-file '$environment_file' -f '$compose_file' up -d --no-deps --force-recreate media-service; docker stop gluetun-rezka-watcher >/dev/null; docker compose --env-file '$environment_file' -f '$compose_file' up -d --no-deps --force-recreate download-runner; docker start gluetun-rezka-watcher >/dev/null"
    verify
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
    remote "set -eu; umask 077; grep -E '^(MEDIA_SERVICE_IMAGE|DOWNLOAD_RUNNER_IMAGE)=' '$environment_file' >'$rollback_file'"
    replace_images "$service_image" "$runner_image"
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
