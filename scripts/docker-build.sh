#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

service_image=${MEDIA_SERVICE_IMAGE:-media-orchestrator-service:local}
runner_image=${MEDIA_RUNNER_IMAGE:-media-orchestrator-runner:local}
revision=${OCI_REVISION:-$(git rev-parse HEAD)}
created=${OCI_CREATED:-$(git show -s --format=%cI "$revision")}
source=${OCI_SOURCE:-$(git config --get remote.origin.url || printf '%s' 'https://github.com/iamstubborn/media-orchestrator')}

docker_context_digest() {
  python3 - "$root" <<'PY'
import hashlib
import os
import pathlib
import posixpath
import re
import stat
import sys

root = pathlib.Path(sys.argv[1])
ignore_file = root / ".dockerignore"
rules = []
for number, raw in enumerate(ignore_file.read_text(encoding="utf-8-sig").splitlines(), 1):
    if raw.startswith("#"):
        continue
    pattern = raw.strip()
    if not pattern:
        continue
    if pattern.startswith("!") or "**" in pattern or any(char in pattern for char in "[]\\"):
        raise SystemExit(f"unsupported .dockerignore pattern on line {number}: {raw}")
    pattern = posixpath.normpath(pattern)
    if len(pattern) > 1:
        pattern = pattern.lstrip("/")
    if pattern != ".":
        expression = "".join(
            "[^/]*" if char == "*" else "[^/]" if char == "?" else re.escape(char)
            for char in pattern
        )
        rules.append(re.compile(f"^{expression}$"))

def candidates(relative: str):
    yield relative
    parts = relative.split("/")
    for length in range(1, len(parts)):
        yield "/".join(parts[:length])

def ignored(relative: str) -> bool:
    return any(pattern.fullmatch(candidate) for pattern in rules for candidate in candidates(relative))

entries = []
for current, directories, files in os.walk(root, topdown=True, followlinks=False):
    current_path = pathlib.Path(current)
    kept_directories = []
    for name in directories:
        path = current_path / name
        relative = path.relative_to(root).as_posix()
        if path.is_symlink():
            if not ignored(relative):
                entries.append((relative, path))
        elif not ignored(relative):
            entries.append((relative, path))
            kept_directories.append(name)
    directories[:] = kept_directories
    for name in files:
        path = current_path / name
        relative = path.relative_to(root).as_posix()
        if relative in {"Dockerfile", ".dockerignore"} or not ignored(relative):
            entries.append((relative, path))

digest = hashlib.sha256()
for relative, path in sorted(entries):
    metadata = path.lstat()
    mode = stat.S_IMODE(metadata.st_mode)
    if stat.S_ISREG(metadata.st_mode):
        kind = "file"
        payload = hashlib.sha256(path.read_bytes()).digest()
    elif stat.S_ISDIR(metadata.st_mode):
        kind = "dir"
        payload = b""
    elif stat.S_ISLNK(metadata.st_mode):
        kind = "symlink"
        payload = os.readlink(path).encode("utf-8")
    else:
        raise SystemExit(f"unsupported Docker context entry: {relative}")
    digest.update(f"{kind} {mode:o} {relative}\0".encode("utf-8"))
    digest.update(payload)
    digest.update(b"\0")
print(digest.hexdigest())
PY
}

source_version() {
  base=$(git describe --tags --always)
  if test -n "$(git status --porcelain --untracked-files=all)"; then
    printf '%s-dirty\n' "$base"
  else
    printf '%s\n' "$base"
  fi
}

runner_build_digest() {
  # Narrow digest: inputs that change the runner image or the shared `media`
  # binary it embeds. Service-facing crates still count because one binary feeds
  # both targets — changing media-api/storage src invalidates this digest
  # (honest). Crate tests, deny.toml, and other non-build noise do not.
  # Invalidates runner (forces deploy-full / blocks deploy-local-service):
  #   Dockerfile (runner stages, yt-dlp/chrome pins, runtime packages),
  #   .dockerignore, Cargo workspace/lock/.cargo, every crate Cargo.toml+src.
  # Does not invalidate runner (service-only OK if compose/watcher unchanged):
  #   crates/*/tests, deny.toml, .env.example, .gitignore, config/, docs/
  #   (docs already dockerignored; source-tree digest still covers full context).
  python3 - "$root" <<'PY'
import hashlib
import os
import pathlib
import stat
import sys

root = pathlib.Path(sys.argv[1])
digest = hashlib.sha256()

def add_file(relative: str, path: pathlib.Path) -> None:
    metadata = path.lstat()
    mode = stat.S_IMODE(metadata.st_mode)
    if stat.S_ISREG(metadata.st_mode):
        kind = "file"
        payload = hashlib.sha256(path.read_bytes()).digest()
    elif stat.S_ISLNK(metadata.st_mode):
        kind = "symlink"
        payload = os.readlink(path).encode("utf-8")
    else:
        raise SystemExit(f"unsupported runner digest entry: {relative}")
    digest.update(f"{kind} {mode:o} {relative}\0".encode("utf-8"))
    digest.update(payload)
    digest.update(b"\0")

def add_tree(relative_dir: str) -> None:
    base = root / relative_dir
    if not base.exists():
        return
    entries = []
    for current, directories, files in os.walk(base, topdown=True, followlinks=False):
        current_path = pathlib.Path(current)
        directories[:] = sorted(
            name for name in directories if not (current_path / name).is_symlink()
        )
        for name in directories:
            path = current_path / name
            relative = path.relative_to(root).as_posix()
            entries.append((relative, path, "dir"))
        for name in files:
            path = current_path / name
            relative = path.relative_to(root).as_posix()
            entries.append((relative, path, "file"))
    for relative, path, kind in sorted(entries):
        if kind == "dir":
            metadata = path.lstat()
            mode = stat.S_IMODE(metadata.st_mode)
            digest.update(f"dir {mode:o} {relative}\0".encode("utf-8"))
            digest.update(b"\0")
        else:
            add_file(relative, path)

for relative in (".dockerignore", "Dockerfile", "Cargo.toml", "Cargo.lock"):
    path = root / relative
    if not path.exists():
        raise SystemExit(f"runner digest input missing: {relative}")
    add_file(relative, path)

add_tree(".cargo")

crates_root = root / "crates"
if not crates_root.is_dir():
    raise SystemExit("runner digest input missing: crates")
for crate_dir in sorted(path for path in crates_root.iterdir() if path.is_dir()):
    cargo = crate_dir / "Cargo.toml"
    if cargo.is_file():
        add_file(cargo.relative_to(root).as_posix(), cargo)
    src = crate_dir / "src"
    if src.exists():
        add_tree(src.relative_to(root).as_posix())

print(digest.hexdigest())
PY
}


case ${1:-} in
  --print-source-tree-digest) docker_context_digest; exit ;;
  --print-source-version) source_version; exit ;;
  --print-runner-build-digest) runner_build_digest; exit ;;
  "") ;;
  *) echo "usage: $0 [--print-source-tree-digest|--print-source-version|--print-runner-build-digest]" >&2; exit 2 ;;
esac

version=${OCI_VERSION:-$(source_version)}
computed_source_tree_digest=$(docker_context_digest)
source_tree_digest=${MEDIA_SOURCE_TREE_DIGEST:-$computed_source_tree_digest}
printf '%s\n' "$source_tree_digest" | grep -Eq '^[0-9a-f]{64}$' || {
  echo "MEDIA_SOURCE_TREE_DIGEST must be a SHA-256 digest" >&2
  exit 2
}
test "$source_tree_digest" = "$computed_source_tree_digest" || {
  echo "MEDIA_SOURCE_TREE_DIGEST differs from the current backend build inputs" >&2
  exit 2
}
computed_runner_build_digest=$(runner_build_digest)
runner_build_digest=${MEDIA_RUNNER_BUILD_DIGEST:-$computed_runner_build_digest}
printf '%s\n' "$runner_build_digest" | grep -Eq '^[0-9a-f]{64}$' || {
  echo "MEDIA_RUNNER_BUILD_DIGEST must be a SHA-256 digest" >&2
  exit 2
}
test "$runner_build_digest" = "$computed_runner_build_digest" || {
  echo "MEDIA_RUNNER_BUILD_DIGEST differs from the current runner build inputs" >&2
  exit 2
}

case "$source" in
  git@github.com:*)
    source="https://github.com/${source#git@github.com:}"
    source=${source%.git}
    ;;
esac

# Optional local BuildKit cache (warm rebuilds). Enable with MEDIA_BUILDKIT_CACHE=1
# or set MEDIA_BUILD_CACHE_DIR to a directory (implies cache on). Default dir:
# <repo>/.cache/buildx
cache_dir=${MEDIA_BUILD_CACHE_DIR:-}
case ${MEDIA_BUILDKIT_CACHE:-0} in
  1|true|TRUE|yes|YES)
    if test -z "$cache_dir"; then
      cache_dir=$root/.cache/buildx
    fi
    ;;
esac
cache_args=
if test -n "$cache_dir"; then
  mkdir -p "$cache_dir"
  cache_args="--set=*.cache-from=type=local,src=$cache_dir --set=*.cache-to=type=local,dest=$cache_dir,mode=max"
fi

# Prefer a single buildx bake so service+runner share one planner/builder graph
# instead of two sequential buildx build invocations (double context walk).
bake() {
  targets=$1
  # shellcheck disable=SC2086
  SERVICE_IMAGE=$service_image \
  RUNNER_IMAGE=$runner_image \
  OCI_CREATED=$created \
  OCI_REVISION=$revision \
  OCI_RUNNER_BUILD_DIGEST=$runner_build_digest \
  OCI_SOURCE=$source \
  OCI_SOURCE_TREE_DIGEST=$source_tree_digest \
  OCI_VERSION=$version \
  docker buildx bake \
    --load \
    --file "$root/docker-bake.hcl" \
    $cache_args \
    $targets
}

case ${MEDIA_BUILD_TARGETS:-all} in
  all)
    bake default
    ;;
  service)
    bake service
    ;;
  runner)
    bake runner
    ;;
  *)
    echo "MEDIA_BUILD_TARGETS must be all, service, or runner" >&2
    exit 2
    ;;
esac
