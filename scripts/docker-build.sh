#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

service_image=${MEDIA_SERVICE_IMAGE:-media-orchestrator-service:local}
runner_image=${MEDIA_RUNNER_IMAGE:-media-orchestrator-runner:local}
revision=${OCI_REVISION:-$(git rev-parse HEAD)}
created=${OCI_CREATED:-$(git show -s --format=%cI "$revision")}
source=${OCI_SOURCE:-$(git config --get remote.origin.url || printf '%s' 'https://github.com/iamstubborn/media-orchestrator')}
version=${OCI_VERSION:-$(git describe --tags --always --dirty)}

case "$source" in
  git@github.com:*)
    source="https://github.com/${source#git@github.com:}"
    source=${source%.git}
    ;;
esac

build() {
  target=$1
  image=$2
  docker buildx build \
    --load \
    --target "$target" \
    --tag "$image" \
    --build-arg "OCI_CREATED=$created" \
    --build-arg "OCI_REVISION=$revision" \
    --build-arg "OCI_SOURCE=$source" \
    --build-arg "OCI_VERSION=$version" \
    "$root"
}

build service "$service_image"
build runner "$runner_image"
