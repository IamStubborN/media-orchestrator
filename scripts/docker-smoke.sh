#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

service_image=${MEDIA_SERVICE_IMAGE:-media-orchestrator-service:local}
runner_image=${MEDIA_RUNNER_IMAGE:-media-orchestrator-runner:local}
project=${COMPOSE_PROJECT_NAME:-media-orchestrator-smoke}

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
