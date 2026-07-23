#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

service_image=${MEDIA_SERVICE_IMAGE:-media-orchestrator-service:local}
runner_image=${MEDIA_RUNNER_IMAGE:-media-orchestrator-runner:local}
project=${COMPOSE_PROJECT_NAME:-media-orchestrator-smoke}

MEDIA_POSTGRES_PASSWORD=${MEDIA_POSTGRES_PASSWORD:-media-smoke-password}
MEDIA_DATABASE_URL=${MEDIA_DATABASE_URL:-postgres://media:${MEDIA_POSTGRES_PASSWORD}@postgres:5432/media_orchestrator}
MEDIA_PRIMARY_TOKEN=${MEDIA_PRIMARY_TOKEN:-media-smoke-primary-token}
MEDIA_SECONDARY_TOKEN=${MEDIA_SECONDARY_TOKEN:-media-smoke-secondary-token}
MEDIA_RUNNER_TOKEN=${MEDIA_RUNNER_TOKEN:-media-smoke-runner-token}
MEDIA_LIFECYCLE_TOKEN=${MEDIA_LIFECYCLE_TOKEN:-media-smoke-lifecycle-token}
export MEDIA_POSTGRES_PASSWORD MEDIA_DATABASE_URL
export MEDIA_PRIMARY_TOKEN MEDIA_SECONDARY_TOKEN MEDIA_RUNNER_TOKEN MEDIA_LIFECYCLE_TOKEN

cleanup() {
  docker compose --project-name "$project" down --volumes --remove-orphans >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

if [ "${MEDIA_SKIP_DOCKER_BUILD:-0}" != "1" ]; then
  "$root/scripts/docker-build.sh"
fi

docker compose --project-name "$project" config --quiet
docker compose --project-name "$project" up --detach --wait service

docker run --rm --read-only "$service_image" --version
docker run --rm --read-only --entrypoint /bin/sh "$service_image" -c '! command -v ffmpeg && ! command -v ffprobe'
docker run --rm --read-only "$runner_image" --version
docker run --rm --read-only --entrypoint ffprobe "$runner_image" -version >/dev/null
docker run --rm --read-only --entrypoint /bin/sh "$runner_image" -c \
  'ffmpeg -hide_banner -hwaccels 2>/dev/null | grep -qx vaapi'

test "$(docker image inspect "$service_image" --format '{{.Config.User}}')" = "65532:65532"
test "$(docker image inspect "$runner_image" --format '{{.Config.User}}')" = "65532:65532"
docker compose --project-name "$project" ps --status running --services | grep -qx service

echo "docker smoke checks passed"
