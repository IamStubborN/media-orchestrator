#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

platform=${MEDIA_CLI_PLATFORM:-linux/amd64}
arch=${MEDIA_CLI_ARCH:-${platform#linux/}}
output_dir=${MEDIA_CLI_OUTPUT_DIR:-$root/dist}
temporary=$(mktemp -d "${TMPDIR:-/tmp}/media-cli.XXXXXX")
trap 'rm -rf "$temporary"' EXIT INT TERM

docker buildx build \
  --platform "$platform" \
  --target cli-artifact \
  --output "type=local,dest=$temporary" \
  "$root"

mkdir -p "$output_dir"
install -m 0755 "$temporary/media" "$output_dir/media-linux-$arch"
(
  cd "$output_dir"
  shasum -a 256 "media-linux-$arch" > "media-linux-$arch.sha256"
)

printf '%s\n' "$output_dir/media-linux-$arch"
