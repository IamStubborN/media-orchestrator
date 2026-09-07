#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
host=${MEDIA_HOMELAB_HOST:host.example.invalid}
remote_root=${MEDIA_HOMELAB_ROOT:-/srv/homelab}
# Included by $remote_root/compose.yml. Runtime operations use the root `homelab`
# project; do not `cd media` or Compose will create a second project.
compose_file=$remote_root/media/compose.media-orchestrator.yml
watcher_script_file=$remote_root/media/gluetun-rezka-watcher/watch.sh
environment_file=$remote_root/.env
compose_project=homelab
rollback_file=$remote_root/media/.media-orchestrator-images.previous
hermes_root=${HERMES_HOME_ROOT:-${HOMELAB_ROOT:-}/hermes}
hermes_remote_root=${HERMES_HOME_REMOTE_ROOT:-/srv/homelab/hermes}
remote_schema_file=$hermes_remote_root/shared/skills/media/MCP_SCHEMA.json
schema_hash_file=$remote_schema_file.expected-sha256
homelab_root=${HOMELAB_ROOT:-}

usage() {
    echo "usage: $0 status|verify|deploy|deploy-service|deploy-full|deploy-local-service|deploy-local-full|deploy-hermes|rollback|rollback-service|rollback-full" >&2
    exit 2
}

check_hermes_capabilities() {
    checker=${HERMES_CAPABILITY_CHECKER:-$hermes_root/scripts/check-media-capabilities}
    test -f "$checker" || {
        echo "Hermes capability checker not found: $checker" >&2
        exit 1
    }
    (cd "$hermes_root" && python3 "$checker")
}

require_homelab_root() {
    : "${HOMELAB_ROOT:?HOMELAB_ROOT is required}"
    homelab_root=$HOMELAB_ROOT
    hermes_root=${HERMES_HOME_ROOT:-$HOMELAB_ROOT/hermes}
}

with_release_snapshot() {
    operation=$1
    shift
    require_homelab_root
    : "${MEDIA_RELEASE_DIR:?MEDIA_RELEASE_DIR is required}"
    source=$MEDIA_RELEASE_DIR
    test -d "$source" || { echo "release directory is missing: $source" >&2; exit 1; }
    snapshot=$(mktemp -d "${TMPDIR:-/tmp}/media-release-snapshot.XXXXXX")
    trap 'rm -rf "$snapshot"' EXIT HUP INT TERM
    (cd "$source" && cp -R . "$snapshot/")
    MEDIA_RELEASE_DIR=$snapshot
    export MEDIA_RELEASE_DIR
    set +e
    (set -e; "$operation" "$@")
    result=$?
    set -e
    rm -rf "$snapshot"
    trap - EXIT HUP INT TERM
    return "$result"
}

release_value() {
    field=$1
    source=$MEDIA_RELEASE_DIR/release.json
    test -s "$source" || { echo "release manifest is missing: $source" >&2; exit 1; }
    case $field in
        service_image | runner_image | runner_build_digest | migration_version) ;;
        *) echo "unsupported release manifest field: $field" >&2; exit 2 ;;
    esac
    python3 - "$source" "$field" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as source:
    value = json.load(source)[sys.argv[2]]
if not isinstance(value, str) or not value:
    raise SystemExit(f"release manifest field is missing: {sys.argv[2]}")
print(value)
PY
}

pull_release_image() {
    image=$1
    remote "docker pull '$image' >/dev/null"
}

source_tree_digest() {
    "$root/scripts/docker-build.sh" --print-source-tree-digest
}

source_version() {
    "$root/scripts/docker-build.sh" --print-source-version
}

runner_build_digest() {
    "$root/scripts/docker-build.sh" --print-runner-build-digest
}

latest_migration_version() {
    (cd "$root" && cargo run --quiet --locked -p media-storage --bin latest-migration)
}

migration_predecessor() {
    target=$1
    (cd "$root" && cargo run --quiet --locked -p media-storage --bin latest-migration -- --predecessor-of "$target")
}

assert_deploy_migration_baseline() {
    target=$1
    validate_migration_version "$target"
    predecessor=$(migration_predecessor "$target")
    validate_migration_version "$predecessor"
    current=$(read_db_migration_version)
    case $current in
        "$target" | "$predecessor") return 0 ;;
        *)
            echo "current database migration is neither target nor its immediate predecessor: current=$current predecessor=$predecessor target=$target" >&2
            return 1
            ;;
    esac
}

checkpoint_images() {
    remote sh -s "$environment_file" "$rollback_file" "$remote_schema_file" "$compose_file" "$watcher_script_file" "$hermes_remote_root" "$schema_hash_file" <<'REMOTE'
set -eu
environment_file=$1
rollback_file=$2
schema_file=$3
compose_file=$4
watcher_script_file=$5
hermes_root=$6
schema_hash_file=$7
read_image() {
    key=$1
    count=$(grep -c "^${key}=" "$environment_file" || true)
    test "$count" = 1 || { echo "$key must occur exactly once in $environment_file" >&2; exit 1; }
    sed -n "s/^${key}=//p" "$environment_file"
}
service_image=$(read_image MEDIA_SERVICE_IMAGE)
runner_image=$(read_image DOWNLOAD_RUNNER_IMAGE)
test -n "$service_image" && test -n "$runner_image" || { echo "current image record is incomplete" >&2; exit 1; }
docker image inspect "$service_image" >/dev/null
docker image inspect "$runner_image" >/dev/null
test -s "$schema_file" || { echo "deployed Hermes MCP schema is missing: $schema_file" >&2; exit 1; }
test -s "$compose_file" || { echo "deployed media Compose file is missing: $compose_file" >&2; exit 1; }
test -s "$watcher_script_file" || { echo "deployed watcher script is missing: $watcher_script_file" >&2; exit 1; }
test -d "$hermes_root" || { echo "deployed Hermes source root is missing: $hermes_root" >&2; exit 1; }
python3 -c 'import json,sys; p=json.load(open(sys.argv[1])); assert p["schema_version"] == 1 and p["tools"]' "$schema_file"
umask 077
generation=$(mktemp -d "${rollback_file}.generation.XXXXXX")
trap 'rm -rf "$generation"' EXIT HUP INT TERM
service_revision=$(docker image inspect "$service_image" --format '{{index .Config.Labels "org.opencontainers.image.revision"}}')
runner_revision=$(docker image inspect "$runner_image" --format '{{index .Config.Labels "org.opencontainers.image.revision"}}')
service_image_id=$(docker image inspect "$service_image" --format '{{.Id}}')
runner_image_id=$(docker image inspect "$runner_image" --format '{{.Id}}')
running_service_image_id=$(docker inspect media-service --format '{{.Image}}')
running_runner_image_id=$(docker inspect download-runner --format '{{.Image}}')
test "$running_service_image_id" = "$service_image_id" || { echo "MEDIA_SERVICE_IMAGE does not match the running media-service image" >&2; exit 1; }
test "$running_runner_image_id" = "$runner_image_id" || { echo "DOWNLOAD_RUNNER_IMAGE does not match the running download-runner image" >&2; exit 1; }
service_source_digest=$(docker image inspect "$service_image" --format '{{index .Config.Labels "dev.iamstubborn.media.source-tree-digest"}}')
runner_source_digest=$(docker image inspect "$runner_image" --format '{{index .Config.Labels "dev.iamstubborn.media.source-tree-digest"}}')
service_runner_digest=$(docker image inspect "$service_image" --format '{{index .Config.Labels "dev.iamstubborn.media.runner-build-digest"}}')
runner_runner_digest=$(docker image inspect "$runner_image" --format '{{index .Config.Labels "dev.iamstubborn.media.runner-build-digest"}}')
for value in "$service_image_id" "$runner_image_id"; do
    printf '%s\n' "$value" | grep -Eq '^sha256:[0-9a-f]{64}$' || { echo "checkpoint image ID is invalid" >&2; exit 1; }
done
for value in "$service_source_digest" "$runner_source_digest" "$service_runner_digest" "$runner_runner_digest"; do
    printf '%s\n' "$value" | grep -Eq '^[0-9a-f]{64}$' || { echo "checkpoint image digest label is invalid" >&2; exit 1; }
done
schema_sha256=$(sha256sum "$schema_file" | awk '{print $1}')
schema_source_digest=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["source_digest"])' "$schema_file")
db_migration_version=$(docker exec media-postgres sh -lc 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc "select version from seaql_migrations order by version desc limit 1;"')
printf '%s\n' "$db_migration_version" | grep -Eq '^m[0-9]{8}_[0-9]{6}_[a-z0-9_]+$' || {
    echo "current database migration version is missing or invalid" >&2
    exit 1
}
printf 'MEDIA_SERVICE_IMAGE=%s\nDOWNLOAD_RUNNER_IMAGE=%s\nSERVICE_IMAGE_ID=%s\nRUNNER_IMAGE_ID=%s\nSERVICE_REVISION=%s\nRUNNER_REVISION=%s\nSERVICE_SOURCE_TREE_DIGEST=%s\nRUNNER_SOURCE_TREE_DIGEST=%s\nSERVICE_RUNNER_BUILD_DIGEST=%s\nRUNNER_RUNNER_BUILD_DIGEST=%s\nDB_MIGRATION_VERSION=%s\nMCP_SCHEMA_SHA256=%s\nMCP_SCHEMA_SOURCE_DIGEST=%s\n' \
    "$service_image" "$runner_image" "$service_image_id" "$runner_image_id" "$service_revision" "$runner_revision" \
    "$service_source_digest" "$runner_source_digest" "$service_runner_digest" "$runner_runner_digest" \
    "$db_migration_version" "$schema_sha256" "$schema_source_digest" >"$generation/images.env"
printf '%s\n' "$schema_sha256" >"$schema_hash_file.next"
mv -f "$schema_hash_file.next" "$schema_hash_file"
cp "$schema_file" "$generation/MCP_SCHEMA.json"
cp "$compose_file" "$generation/compose.media-orchestrator.yml"
cp "$watcher_script_file" "$generation/gluetun-rezka-watcher-watch.sh"
mkdir "$generation/hermes-source"
rsync -a --delete \
    --exclude .git \
    --exclude .worktrees/ \
    --exclude .env \
    --exclude artifacts/ \
    --exclude secrets/ \
    "$hermes_root/" "$generation/hermes-source/"
for name in hermes-primary hermes-secondary media-notifier-primary media-notifier-secondary; do
    image_id=$(docker inspect "$name" --format '{{.Image}}')
    image_ref=$(docker inspect "$name" --format '{{.Config.Image}}')
    printf '%s\n' "$image_id" | grep -Eq '^sha256:[0-9a-f]{64}$' || { echo "$name image ID is invalid" >&2; exit 1; }
    case $name in
        hermes-primary) key=HERMES_PRIMARY ;;
        hermes-secondary) key=HERMES_SECONDARY ;;
        media-notifier-primary) key=NOTIFIER_PRIMARY ;;
        media-notifier-secondary) key=NOTIFIER_SECONDARY ;;
    esac
    printf '%s_IMAGE_ID=%s\n%s_IMAGE_REF=%s\n' "$key" "$image_id" "$key" "$image_ref"
done >"$generation/hermes-images.env"
link=$(mktemp "${rollback_file}.link.XXXXXX")
rm "$link"
ln -s "$(basename "$generation")" "$link"
mv -Tf "$link" "$rollback_file"
trap - EXIT HUP INT TERM
REMOTE
}

protected_snapshot() {
    remote "for name in media-postgres gluetun gluetun-rezka qbittorrent; do docker inspect \"\$name\" --format '{{.Name}}|{{.Id}}|{{.State.StartedAt}}|{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}'; done; for name in download-runner gluetun-rezka-watcher; do docker inspect \"\$name\" --format '{{.Name}}|{{.Id}}|{{.Image}}'; done"
}

assert_protected_unchanged() {
    before=$1
    after=$(protected_snapshot)
    test "$before" = "$after" || { echo "a protected container changed during service deployment" >&2; exit 1; }
    stable=$(printf '%s\n' "$after" | grep -E '^/(media-postgres|gluetun|gluetun-rezka|qbittorrent)\|' || true)
    restarted=$(printf '%s\n' "$after" | grep -E '^/(download-runner|gluetun-rezka-watcher)\|' || true)
    test "$(printf '%s\n' "$stable" | grep -c .)" = 4 || {
        echo "service protected snapshot is missing a stable container" >&2
        exit 1
    }
    test "$(printf '%s\n' "$restarted" | grep -c .)" = 2 || {
        echo "service protected snapshot is missing a restarted container" >&2
        exit 1
    }
    if printf '%s\n' "$stable" | grep -Ev '\|healthy$' >/dev/null; then
        echo "a protected container is not healthy" >&2
        exit 1
    fi
    remote "set -eu; test \"\$(docker inspect gluetun-rezka-watcher --format '{{.State.Health.Status}}')\" = healthy; runner_state=\$(docker inspect download-runner --format '{{.State.Status}}'); test \"\$runner_state\" = running || test \"\$runner_state\" = exited"
}

full_protected_snapshot() {
    # qBittorrent may be recreate-allowlisted during full deploy; Id/health matter,
    # but StartedAt alone must not fail the protected snapshot compare.
    remote "for name in media-postgres gluetun; do docker inspect \"\$name\" --format '{{.Name}}|{{.Id}}|{{.State.StartedAt}}|{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}'; done; docker inspect qbittorrent --format '{{.Name}}|{{.Id}}|{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}'"
}

assert_full_protected_unchanged() {
    before=$1
    after=$(full_protected_snapshot)
    test "$before" = "$after" || { echo "PostgreSQL, Gluetun, or qBittorrent container identity changed during full operation" >&2; exit 1; }
    if printf '%s\n' "$after" | grep -Ev '\|healthy$' >/dev/null; then
        echo "PostgreSQL, VPN, or qBittorrent is not healthy" >&2
        exit 1
    fi
}

assert_service_only_rollout() {
    expected_runner_digest=${1:-$(runner_build_digest)}
    expected_runner_image=${2:-}
    live_runner_digest=$(remote "docker inspect download-runner --format '{{index .Config.Labels \"dev.iamstubborn.media.runner-build-digest\"}}'")
    test "$live_runner_digest" = "$expected_runner_digest" || {
        echo "live runner build inputs differ from the local source; use ./scripts/homelab.sh deploy-full" >&2
        exit 1
    }
    if test -n "$expected_runner_image"; then
        live_runner_image=$(remote "docker inspect download-runner --format '{{.Config.Image}}'")
        test "$live_runner_image" = "$expected_runner_image" || {
            echo "live runner image differs from the release bundle; use ./scripts/homelab.sh deploy-full" >&2
            exit 1
        }
    fi

    candidate_compose=$compose_file.service-candidate.$$
    candidate_watcher=$watcher_script_file.service-candidate.$$
    scp "$homelab_root/media/compose.media-orchestrator.yml" "$host:$candidate_compose" >/dev/null
    scp "$homelab_root/media/gluetun-rezka-watcher/watch.sh" "$host:$candidate_watcher" >/dev/null
    remote sh -s "$environment_file" "$compose_file" "$candidate_compose" "$watcher_script_file" "$candidate_watcher" <<'REMOTE'
set -eu
environment_file=$1
live_compose=$2
candidate_compose=$3
live_watcher=$4
candidate_watcher=$5
live_json=$(mktemp)
candidate_json=$(mktemp)
trap 'rm -f "$candidate_compose" "$candidate_watcher" "$live_json" "$candidate_json"' EXIT HUP INT TERM
test -s "$live_watcher" || { echo "live runner watcher script is missing" >&2; exit 1; }
test -s "$candidate_watcher" || { echo "candidate runner watcher script is missing" >&2; exit 1; }
live_watcher_sha256=$(sha256sum "$live_watcher" | awk '{print $1}')
candidate_watcher_sha256=$(sha256sum "$candidate_watcher" | awk '{print $1}')
test "$live_watcher_sha256" = "$candidate_watcher_sha256" || {
    echo "runner watcher script changed; use ./scripts/homelab.sh deploy-full" >&2
    exit 1
}
docker compose --project-name media-orchestrator --profile '*' --env-file "$environment_file" -f "$live_compose" config --format json >"$live_json"
docker compose --project-name media-orchestrator --profile '*' --env-file "$environment_file" -f "$candidate_compose" config --format json >"$candidate_json"
python3 - "$live_json" "$candidate_json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as source:
    live = json.load(source)
with open(sys.argv[2], encoding="utf-8") as source:
    candidate = json.load(source)

services = ("download-runner", "gluetun-rezka", "gluetun-rezka-watcher")
live_boundary = {
    "services": {name: live.get("services", {}).get(name) for name in services},
    "networks": live.get("networks"),
    "volumes": live.get("volumes"),
}
candidate_boundary = {
    "services": {name: candidate.get("services", {}).get(name) for name in services},
    "networks": candidate.get("networks"),
    "volumes": candidate.get("volumes"),
}
if any(value is None for value in live_boundary["services"].values()):
    raise SystemExit("live compose is missing the runner boundary")
if any(value is None for value in candidate_boundary["services"].values()):
    raise SystemExit("candidate compose is missing the runner boundary")
if live_boundary != candidate_boundary:
    raise SystemExit("runner compose contract changed; use ./scripts/homelab.sh deploy-full")
PY
REMOTE
}

sync_hermes_schema() {
    source=$MEDIA_RELEASE_DIR/MCP_SCHEMA.json
    test -s "$source" || { echo "release bundle MCP schema is missing: $source" >&2; exit 1; }
    scp "$source" "$host:$remote_schema_file.next" >/dev/null
    remote "install -m 0644 '$remote_schema_file.next' '$remote_schema_file'; rm '$remote_schema_file.next' '$schema_hash_file' 2>/dev/null || true"
}

verify_local_mcp_schema() {
    live_tools=$(mktemp "${TMPDIR:-/tmp}/media-live-tools.XXXXXX")
    trap 'rm -f "$live_tools"' EXIT HUP INT TERM
    remote sh -s >"$live_tools" <<'REMOTE'
set -eu
docker exec -i hermes-primary python3 - <<'PY'
import json, urllib.request
token=open('/run/secrets/media_api_token').read().strip()
headers={'Authorization':'Bearer '+token,'Content-Type':'application/json','Accept':'application/json, text/event-stream'}
def call(payload, protocol=None):
    request_headers=dict(headers)
    if protocol: request_headers['MCP-Protocol-Version']=protocol
    request=urllib.request.Request('http://media-service:8080/internal/mcp', data=json.dumps(payload).encode(), headers=request_headers, method='POST')
    with urllib.request.urlopen(request, timeout=15) as response: return json.loads(response.read())
call({'jsonrpc':'2.0','id':1,'method':'initialize','params':{'protocolVersion':'2025-03-26','capabilities':{},'clientInfo':{'name':'deploy-preflight','version':'1'}}})
print(json.dumps(call({'jsonrpc':'2.0','id':2,'method':'tools/list'}, '2025-03-26')['result']['tools'], sort_keys=True, separators=(',',':')))
PY
REMOTE
    "$hermes_root/scripts/deploy-preflight" --live-tools "$live_tools"
    rm -f "$live_tools"
    trap - EXIT HUP INT TERM
}

verify_local_backend_attestation() {
    live_attestation=$(mktemp "${TMPDIR:-/tmp}/media-live-attestation.XXXXXX")
    trap 'rm -f "$live_attestation"' EXIT HUP INT TERM
    remote "docker inspect media-service" >"$live_attestation"
    "$hermes_root/scripts/deploy-preflight" --live-attestation "$live_attestation"
    rm -f "$live_attestation"
    trap - EXIT HUP INT TERM
}

verify_release_image_attestation() {
    image=$1
    role=$2
    attestation=$(mktemp "${TMPDIR:-/tmp}/media-release-image.XXXXXX")
    trap 'rm -f "$attestation"' EXIT HUP INT TERM
    remote "docker image inspect '$image'" >"$attestation"
    python3 - "$MEDIA_RELEASE_DIR/release.json" "$attestation" "$role" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as source:
    release = json.load(source)
with open(sys.argv[2], encoding="utf-8") as source:
    raw = json.load(source)
if len(raw) != 1 or not isinstance(raw[0].get("Config", {}).get("Labels"), dict):
    raise SystemExit("release image OCI attestation is invalid")
labels = raw[0]["Config"]["Labels"]
expected = {
    "revision": release["source_revision"],
    "version": release["application_version"],
    "source_tree_digest": release["source_tree_digest"],
    "runner_build_digest": release["runner_build_digest"],
}
actual = {
    "revision": labels.get("org.opencontainers.image.revision"),
    "version": labels.get("org.opencontainers.image.version"),
    "source_tree_digest": labels.get("dev.iamstubborn.media.source-tree-digest"),
    "runner_build_digest": labels.get("dev.iamstubborn.media.runner-build-digest"),
}
if actual != expected:
    raise SystemExit(f"{sys.argv[3]} release image OCI attestation differs")
PY
    rm -f "$attestation"
    trap - EXIT HUP INT TERM
}

verify_running_release_refs() {
    service_image=$1
    runner_image=$2
    remote sh -s "$service_image" "$runner_image" <<'REMOTE'
set -eu
test "$(docker inspect media-service --format '{{.Config.Image}}')" = "$1" || { echo "deployed service image ref differs from release manifest" >&2; exit 1; }
test "$(docker inspect download-runner --format '{{.Config.Image}}')" = "$2" || { echo "deployed runner image ref differs from release manifest" >&2; exit 1; }
REMOTE
}

verify_image_attestation() {
    image=$1
    expected_revision=$(git -C "$root" rev-parse HEAD)
    expected_version=$(source_version)
    expected_digest=$(source_tree_digest)
    expected_runner_digest=$(runner_build_digest)
    remote sh -s "$image" "$expected_revision" "$expected_version" "$expected_digest" "$expected_runner_digest" <<'REMOTE'
set -eu
image=$1
expected_revision=$2
expected_version=$3
expected_digest=$4
expected_runner_digest=$5
revision=$(docker image inspect "$image" --format '{{index .Config.Labels "org.opencontainers.image.revision"}}')
version=$(docker image inspect "$image" --format '{{index .Config.Labels "org.opencontainers.image.version"}}')
digest=$(docker image inspect "$image" --format '{{index .Config.Labels "dev.iamstubborn.media.source-tree-digest"}}')
runner_digest=$(docker image inspect "$image" --format '{{index .Config.Labels "dev.iamstubborn.media.runner-build-digest"}}')
test "$revision" = "$expected_revision" || { echo "$image revision attestation differs" >&2; exit 1; }
test "$version" = "$expected_version" || { echo "$image version attestation differs" >&2; exit 1; }
test "$digest" = "$expected_digest" || { echo "$image source-tree attestation differs" >&2; exit 1; }
test "$runner_digest" = "$expected_runner_digest" || { echo "$image runner build attestation differs" >&2; exit 1; }
REMOTE
}

immutable_image_id() {
    image_id=$(remote "docker image inspect '$1' --format '{{.Id}}'")
    printf '%s\n' "$image_id" | grep -Eq '^sha256:[0-9a-f]{64}$' || {
        echo "$1 did not resolve to an immutable Docker image ID" >&2
        exit 1
    }
    printf '%s\n' "$image_id"
}

running_image_id() {
    image_id=$(remote "docker inspect '$1' --format '{{.Image}}'")
    printf '%s\n' "$image_id" | grep -Eq '^sha256:[0-9a-f]{64}$' || {
        echo "$1 is not running from an immutable Docker image ID" >&2
        exit 1
    }
    printf '%s\n' "$image_id"
}

verify_running_service_attestation() {
    expected_revision=$(git -C "$root" rev-parse HEAD)
    expected_version=$(source_version)
    expected_digest=$(source_tree_digest)
    remote sh -s "$expected_revision" "$expected_version" "$expected_digest" <<'REMOTE'
set -eu
expected_revision=$1
expected_version=$2
expected_digest=$3
revision=$(docker inspect media-service --format '{{index .Config.Labels "org.opencontainers.image.revision"}}')
version=$(docker inspect media-service --format '{{index .Config.Labels "org.opencontainers.image.version"}}')
digest=$(docker inspect media-service --format '{{index .Config.Labels "dev.iamstubborn.media.source-tree-digest"}}')
test "$revision" = "$expected_revision" || { echo "media-service revision attestation differs" >&2; exit 1; }
test "$version" = "$expected_version" || { echo "media-service version attestation differs" >&2; exit 1; }
test "$digest" = "$expected_digest" || { echo "media-service source-tree attestation differs" >&2; exit 1; }
REMOTE
}

verify_running_image_attestations() {
    expected_revision=$(git -C "$root" rev-parse HEAD)
    expected_version=$(source_version)
    expected_digest=$(source_tree_digest)
    expected_runner_digest=$(runner_build_digest)
    remote sh -s "$expected_revision" "$expected_version" "$expected_digest" "$expected_runner_digest" <<'REMOTE'
set -eu
expected_revision=$1
expected_version=$2
expected_digest=$3
expected_runner_digest=$4
for container in media-service download-runner; do
    revision=$(docker inspect "$container" --format '{{index .Config.Labels "org.opencontainers.image.revision"}}')
    version=$(docker inspect "$container" --format '{{index .Config.Labels "org.opencontainers.image.version"}}')
    digest=$(docker inspect "$container" --format '{{index .Config.Labels "dev.iamstubborn.media.source-tree-digest"}}')
    runner_digest=$(docker inspect "$container" --format '{{index .Config.Labels "dev.iamstubborn.media.runner-build-digest"}}')
    test "$revision" = "$expected_revision" || { echo "$container revision attestation differs" >&2; exit 1; }
    test "$version" = "$expected_version" || { echo "$container version attestation differs" >&2; exit 1; }
    test "$digest" = "$expected_digest" || { echo "$container source-tree attestation differs" >&2; exit 1; }
    test "$runner_digest" = "$expected_runner_digest" || { echo "$container runner build attestation differs" >&2; exit 1; }
done
REMOTE
}

verify_mounted_hermes_sources() {
    verification_source=${1:-local}
    for source in \
        shared/skills/media/SKILL.md \
        shared/plugins/telegram-home/__init__.py \
        shared/plugins/telegram-home/media_action_store.py \
        shared/plugins/telegram-home/media_callbacks.py \
        shared/plugins/telegram-home/media_panel.py \
        shared/plugins/telegram-home/media_search.py \
        shared/plugins/telegram-home/media_trending.py; do
        expected=
        if test "$verification_source" = local; then
            expected=$(shasum -a 256 "$hermes_root/$source" | awk '{print $1}')
        fi
        case $source in
            shared/skills/*) mounted=/etc/hermes-home/skills/${source#shared/skills/} ;;
            shared/plugins/*) mounted=/etc/hermes-home/shared-plugins/${source#shared/plugins/} ;;
        esac
        remote sh -s "$hermes_remote_root/$source" "$mounted" "$expected" <<'REMOTE'
set -eu
host_source=$1
mounted_source=$2
expected=${3-}
digest() {
    python3 -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())' "$1"
}
host_digest=$(digest "$host_source")
test -z "$expected" || test "$host_digest" = "$expected" || { echo "remote Hermes source differs: $host_source" >&2; exit 1; }
expected=${expected:-$host_digest}
for container in hermes-primary hermes-secondary; do
    actual=$(docker exec "$container" python3 -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())' "$mounted_source")
    test "$actual" = "$expected" || { echo "$container mounted Hermes source differs: $mounted_source" >&2; exit 1; }
done
REMOTE
    done
    for notifier_mount in \
        'scripts/media-notifier|/usr/local/bin/media-notifier' \
        'scripts/hermes_media_notifications.py|/usr/local/lib/hermes-home/hermes_media_notifications.py' \
        'shared/plugins/telegram-home/assets/media-menu.jpg|/usr/local/share/hermes-home/media-menu.jpg'; do
        notifier_source=${notifier_mount%%|*}
        mounted_source=${notifier_mount#*|}
        expected=
        if test "$verification_source" = local; then
            expected=$(shasum -a 256 "$hermes_root/$notifier_source" | awk '{print $1}')
        fi
        remote sh -s "$hermes_remote_root/$notifier_source" "$mounted_source" "$expected" <<'REMOTE'
set -eu
host_source=$1
mounted_source=$2
expected=${3-}
digest() {
    python3 -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())' "$1"
}
host_digest=$(digest "$host_source")
test -z "$expected" || test "$host_digest" = "$expected" || { echo "remote Hermes notifier source differs" >&2; exit 1; }
expected=${expected:-$host_digest}
for container in media-notifier-primary media-notifier-secondary; do
    actual=$(docker exec "$container" python3 -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())' "$mounted_source")
    test "$actual" = "$expected" || { echo "$container mounted notifier source differs: $mounted_source" >&2; exit 1; }
done
REMOTE
    done
}

verify_live_mcp_schema() {
    schema_file=${1:-$remote_schema_file}
    remote sh -s "$schema_file" <<'REMOTE'
set -eu
schema_file=$1
live=$(mktemp)
trap 'rm -f "$live"' EXIT HUP INT TERM
docker exec -i hermes-primary python3 - >"$live" <<'PY'
import json, urllib.request
token=open('/run/secrets/media_api_token').read().strip()
headers={'Authorization':'Bearer '+token,'Content-Type':'application/json','Accept':'application/json, text/event-stream'}
def call(payload, protocol=None):
    request_headers=dict(headers)
    if protocol: request_headers['MCP-Protocol-Version']=protocol
    request=urllib.request.Request('http://media-service:8080/internal/mcp', data=json.dumps(payload).encode(), headers=request_headers, method='POST')
    with urllib.request.urlopen(request, timeout=15) as response: return json.loads(response.read())
call({'jsonrpc':'2.0','id':1,'method':'initialize','params':{'protocolVersion':'2025-03-26','capabilities':{},'clientInfo':{'name':'deploy-verifier','version':'1'}}})
print(json.dumps(call({'jsonrpc':'2.0','id':2,'method':'tools/list'}, '2025-03-26')['result']['tools'], sort_keys=True, separators=(',',':')))
PY
python3 - "$schema_file" "$live" <<'PY'
import json,sys
expected=json.load(open(sys.argv[1]))['tools']
actual=json.load(open(sys.argv[2]))
canonical=lambda tools: json.dumps(sorted(tools,key=lambda tool:tool['name']),sort_keys=True,separators=(',',':'))
assert canonical(expected) == canonical(actual), 'live MCP tools/list differs from restored artifact'
PY
REMOTE
}

preflight_deployed_mcp_schema() {
    remote sh -s "$remote_schema_file" "$schema_hash_file" <<'REMOTE'
set -eu
schema_file=$1
expected_hash_file=$2
snapshot() {
    media_state=$(docker inspect media-service --format '{{.State.Status}}')
    lifecycle=$(docker exec media-postgres sh -lc 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc "select state from runner_lifecycle;"')
    watcher_state=$(docker inspect gluetun-rezka-watcher --format '{{.State.Status}}')
    runner_state=$(docker inspect download-runner --format '{{.State.Status}}')
    states=$(docker exec media-postgres sh -lc 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc "select state from jobs;"')
    active=$(printf '%s\n' "$states" | grep -Ec '^(leased|running|cancel_requested|publishing|plex_pending)$' || true)
    printf '%s|%s|%s|%s|%s\n' "$media_state" "$lifecycle" "$watcher_state" "$runner_state" "$active"
}
media_state=$(docker inspect media-service --format '{{.State.Status}}')
if test "$media_state" = running; then
    printf '%s\n' live
    exit 0
fi
first=$(snapshot)
second=$(snapshot)
test "$first" = "$second" || { echo "runner safe-hold state changed during schema preflight" >&2; exit 1; }
IFS='|' read -r media_state lifecycle watcher_state runner_state active <<EOF
$second
EOF
{ test "$media_state" = exited || test "$media_state" = created; } \
    && test "$lifecycle" = rotating \
    && { test "$watcher_state" = exited || test "$watcher_state" = created; } \
    && { test "$runner_state" = exited || test "$runner_state" = created; } \
    && test "$active" = 0 || {
    echo "media-service is stopped outside the exact full recovery safe hold" >&2
    exit 1
}
test -s "$schema_file" || { echo "deployed Hermes MCP schema is missing: $schema_file" >&2; exit 1; }
schema_sha256=$(sha256sum "$schema_file" | awk '{print $1}')
printf '%s\n' "$schema_sha256" | grep -Eq '^[0-9a-f]{64}$' || {
    echo "deployed Hermes MCP schema hash is invalid" >&2
    exit 1
}
test -s "$expected_hash_file" || {
    echo "safe-hold schema checkpoint hash is missing: $expected_hash_file" >&2
    exit 1
}
expected_sha256=$(tr -d '\n' < "$expected_hash_file")
printf '%s\n' "$expected_sha256" | grep -Eq '^[0-9a-f]{64}$' || {
    echo "safe-hold schema checkpoint hash is invalid" >&2
    exit 1
}
test "$schema_sha256" = "$expected_sha256" || {
    echo "safe-hold schema differs from its checkpoint hash" >&2
    exit 1
}
python3 - "$schema_file" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as source:
    schema = json.load(source)
if schema.get("schema_version") != 1 or not isinstance(schema.get("tools"), list) or not schema["tools"]:
    raise SystemExit("deployed Hermes MCP schema has an unexpected shape")
if any(not isinstance(tool, dict) or not isinstance(tool.get("name"), str) or not tool["name"] for tool in schema["tools"]):
    raise SystemExit("deployed Hermes MCP schema contains an invalid tool")
PY
printf '%s\n' schema-only
REMOTE
}

ensure_deployed_mcp_schema() {
    schema_mode=$(preflight_deployed_mcp_schema) || exit 1
    case $schema_mode in
        live) verify_live_mcp_schema ;;
        schema-only) return 0 ;;
        *) echo "unexpected deployed MCP schema preflight result" >&2; exit 1 ;;
    esac
}

remote() {
    # Arguments intentionally form the bounded command evaluated by the remote shell.
    # shellcheck disable=SC2029
    ssh "$host" "$@"
}

acquire_host_lock() {
    host_lock_state=$(mktemp -d "${TMPDIR:-/tmp}/media-homelab-lock.XXXXXX")
    host_lock_fifo=$host_lock_state/hold
    host_lock_status=$host_lock_state/status
    host_lock_stderr=$host_lock_state/stderr
    mkfifo "$host_lock_fifo"
    # The SSH process owns the remote flock until this process closes fd 9.
    # shellcheck disable=SC2029
    ssh "$host" "exec 8>'$remote_root/media/.media-orchestrator.deploy.lock'; flock -n 8 || { echo busy >&2; exit 75; }; echo locked; cat >/dev/null" \
        <"$host_lock_fifo" >"$host_lock_status" 2>"$host_lock_stderr" &
    host_lock_pid=$!
    exec 9>"$host_lock_fifo"
    attempts=0
    while ! grep -qx locked "$host_lock_status" 2>/dev/null; do
        if ! kill -0 "$host_lock_pid" 2>/dev/null; then
            wait "$host_lock_pid" || true
            cat "$host_lock_stderr" >&2
            rm -rf "$host_lock_state"
            echo "another media deploy or rollback holds the host lock" >&2
            return 1
        fi
        attempts=$((attempts + 1))
        test "$attempts" -lt 50 || {
            exec 9>&-
            wait "$host_lock_pid" || true
            rm -rf "$host_lock_state"
            echo "timed out acquiring the media deploy host lock" >&2
            return 1
        }
        sleep 0.1
    done
}

release_host_lock() {
    exec 9>&-
    wait "$host_lock_pid" || true
    test ! -s "$host_lock_stderr" || cat "$host_lock_stderr" >&2
    rm -rf "$host_lock_state"
}

with_host_lock() {
    acquire_host_lock
    trap 'release_host_lock; exit 130' HUP INT TERM
    # Keep the deploy function out of a conditional context so its errexit
    # behavior remains active, while still releasing the host lock on failure.
    set +e
    (set -e; "$@")
    result=$?
    set -e
    release_host_lock
    trap - HUP INT TERM
    return "$result"
}

status() {
    remote "docker ps -a --format '{{.Names}} {{.Image}} {{.Status}}' | grep -E '^(media-service|download-runner|gluetun-rezka|gluetun-rezka-watcher|media-postgres|qbittorrent|prowlarr)'"
    remote "docker exec media-postgres sh -lc 'psql -U \"\$POSTGRES_USER\" -d \"\$POSTGRES_DB\" -c \"select state,previous_ip,current_ip,sticky_job_id,sticky_attempt_count,updated_at from runner_lifecycle; select id,provider,state,attempt_count,updated_at from jobs order by created_at desc limit 5;\"'"
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

verify_service() {
    remote sh -s <<'REMOTE'
set -eu
attempts=0
while :; do
    state=$(docker inspect media-service --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}')
    test "$state" = healthy && break
    attempts=$((attempts + 1))
    test "$attempts" -lt 30 || { echo "media-service is not healthy: $state" >&2; exit 1; }
    sleep 5
done
for protected in download-runner gluetun-rezka gluetun-rezka-watcher; do
    docker inspect "$protected" >/dev/null
done
echo "homelab media-service verification passed"
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

validate_migration_version() {
    printf '%s\n' "$1" | grep -Eq '^m[0-9]{8}_[0-9]{6}_[a-z0-9_]+$' || {
        echo "database migration version is missing or invalid" >&2
        exit 1
    }
}

read_db_migration_version() {
    version=$(remote "docker exec media-postgres sh -lc 'psql -U \"\$POSTGRES_USER\" -d \"\$POSTGRES_DB\" -Atc \"select version from seaql_migrations order by version desc limit 1;\"'")
    validate_migration_version "$version"
    printf '%s\n' "$version"
}

assert_db_migration_version() {
    actual=$(read_db_migration_version)
    test "$actual" = "$1" || {
        echo "database migration version mismatch: expected $1, found $actual" >&2
        exit 1
    }
}

quiesce_runner() {
    lifecycle_target=${1:-service}
    remote sh -s "$lifecycle_target" "$environment_file" <<'REMOTE'
set -eu
lifecycle_target=$1
environment_file=$2
rotation_marked=0
watcher_restart_drain_checks=65
watcher_fence_checks=3
media_restart_policy_disabled=0
media_stopped_for_quiescence=0
fast_safe_hold_reached=0
initial_watcher_state=$(docker inspect gluetun-rezka-watcher --format '{{.State.Status}}')
initial_watcher_restart_policy=$(docker inspect gluetun-rezka-watcher --format '{{.HostConfig.RestartPolicy.Name}}')
download_watcher_present=0
initial_download_watcher_state=absent
initial_download_watcher_restart_policy=no
if docker inspect gluetun-watcher >/dev/null 2>&1; then
    download_watcher_present=1
    initial_download_watcher_state=$(docker inspect gluetun-watcher --format '{{.State.Status}}')
    initial_download_watcher_restart_policy=$(docker inspect gluetun-watcher --format '{{.HostConfig.RestartPolicy.Name}}')
fi
initial_runner_state=$(docker inspect download-runner --format '{{.State.Status}}')
initial_runner_restart_policy=$(docker inspect download-runner --format '{{.HostConfig.RestartPolicy.Name}}')
initial_media_state=$(docker inspect media-service --format '{{.State.Status}}')
initial_media_restart_policy=$(docker inspect media-service --format '{{.HostConfig.RestartPolicy.Name}}')
initial_session_volume=$(docker inspect download-runner --format '{{range .Mounts}}{{if eq .Destination "/var/lib/media-orchestrator/session"}}{{.Name}}{{end}}{{end}}')
lifecycle_state() {
    docker exec media-postgres sh -lc 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc "select state from runner_lifecycle;"'
}
active_job_count() {
    states=$(docker exec media-postgres sh -lc 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc "select state from jobs;"')
    printf '%s\n' "$states" | grep -Ec '^(leased|running|cancel_requested|publishing|plex_pending)$' || true
}
read_env() {
    key=$1
    value=$(sed -n "s/^${key}=//p" "$environment_file")
    test "$(printf '%s\n' "$value" | wc -l | tr -d ' ')" -le 1 || {
        echo "$key must occur at most once in $environment_file" >&2
        exit 1
    }
    printf '%s' "$value"
}
watcher_uid=$(read_env PUID)
watcher_gid=$(read_env PGID)
watcher_uid=${watcher_uid:-1000}
watcher_gid=${watcher_gid:-1000}
printf '%s\n' "$watcher_uid" | grep -Eq '^[0-9]+$' || { echo "PUID must be numeric" >&2; exit 1; }
printf '%s\n' "$watcher_gid" | grep -Eq '^[0-9]+$' || { echo "PGID must be numeric" >&2; exit 1; }
watcher_image=$(docker inspect gluetun-rezka-watcher --format '{{.Config.Image}}')
lifecycle_token_source=$(docker inspect gluetun-rezka-watcher --format '{{range .Mounts}}{{if eq .Destination "/run/secrets/media_lifecycle_token"}}{{.Source}}{{end}}{{end}}')
test -n "$watcher_image" && test -n "$lifecycle_token_source" || {
    echo "runner watcher image or lifecycle secret mount is missing" >&2
    exit 1
}
write_lifecycle_direct_rotating() {
    docker exec media-postgres sh -lc '
psql -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" -c "
UPDATE runner_lifecycle
SET state = '\''rotating'\'', reason = NULL, previous_ip = NULL, current_ip = NULL,
    sticky_job_id = NULL, sticky_attempt_count = 0, updated_at = now()
WHERE singleton = true;"
'
}
write_lifecycle_direct_ready() {
    docker exec media-postgres sh -lc '
psql -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" -c "
UPDATE runner_lifecycle
SET state = '\''ready'\'', reason = NULL, previous_ip = NULL, current_ip = NULL,
    sticky_job_id = NULL, sticky_attempt_count = 0, updated_at = now()
WHERE singleton = true;"
'
}
write_lifecycle() {
    state=$1
    if test "$state" = rotating && test "$media_stopped_for_quiescence" = 1; then
        write_lifecycle_direct_rotating
        return
    fi
    docker run --rm \
        --user "$watcher_uid:$watcher_gid" \
        --read-only \
        --cap-drop ALL \
        --security-opt no-new-privileges:true \
        --network container:media-service \
        --mount "type=bind,source=$lifecycle_token_source,target=/run/secrets/media_lifecycle_token,readonly" \
        --env MEDIA_LIFECYCLE_TOKEN_FILE=/run/secrets/media_lifecycle_token \
        --env MEDIA_SERVICE_URL=http://127.0.0.1:8080 \
        --entrypoint /bin/sh "$watcher_image" -s - "$state" <<'WATCHER'
set -eu
state=$1
test -n "${MEDIA_LIFECYCLE_TOKEN_FILE:-}"
token=$(tr -d '\n' < "$MEDIA_LIFECYCLE_TOKEN_FILE")
test -n "$token"
wget -q -T "${LIFECYCLE_HTTP_TIMEOUT:-10}" -O /dev/null \
    --header "Authorization: Bearer $token" \
    --header 'Content-Type: application/json' \
    --post-data "{\"state\":\"$state\"}" \
    "${MEDIA_SERVICE_URL:-http://media-service:8080}/v1/runner/lifecycle"
WATCHER
}
wait_watcher_fence() {
    expected_lifecycle=$1
    checks=${2:-1}
    attempts=0
    while test "$attempts" -lt "$checks"; do
        observed_watcher_state=$(docker inspect gluetun-rezka-watcher --format '{{.State.Status}}')
        observed_lifecycle_state=$(lifecycle_state)
        case $observed_watcher_state in
            exited | created) ;;
            *)
                echo "runner watcher resurrected during quiescence: $observed_watcher_state" >&2
                return 1
                ;;
        esac
        test "$observed_lifecycle_state" = "$expected_lifecycle" || {
            echo "runner lifecycle changed while watcher was fenced: $observed_lifecycle_state" >&2
            return 1
        }
        attempts=$((attempts + 1))
        test "$attempts" -ge "$checks" || sleep 1
    done
}
wait_media_stopped() {
    checks=${1:-1}
    attempts=0
    while test "$attempts" -lt "$checks"; do
        observed_media_state=$(docker inspect media-service --format '{{.State.Status}}')
        case $observed_media_state in
            exited | created) ;;
            *)
                echo "media-service restarted during full safe hold: $observed_media_state" >&2
                return 1
                ;;
        esac
        attempts=$((attempts + 1))
        test "$attempts" -ge "$checks" || sleep 1
    done
}
stop_media_for_full_hold() {
    media_restart_policy_disabled=1
    docker update --restart=no media-service >/dev/null
    test "$(docker inspect media-service --format '{{.HostConfig.RestartPolicy.Name}}')" = no || {
        echo "could not disable media-service restart policy for full safe hold" >&2
        return 1
    }
    media_state=$(docker inspect media-service --format '{{.State.Status}}')
    test "$media_state" = running && docker stop media-service >/dev/null
    wait_media_stopped "$watcher_restart_drain_checks" || return 1
    media_stopped_for_quiescence=1
    write_lifecycle_direct_rotating
}
consume_watcher_fence() {
    expected_lifecycle=$1
    checks=${2:-1}
    max_resurrections=${3:-3}
    stable_checks=0
    resurrections=0
    observations=0
    max_observations=$((checks * 2))
    while test "$stable_checks" -lt "$checks"; do
        observations=$((observations + 1))
        observed_watcher_state=$(docker inspect gluetun-rezka-watcher --format '{{.State.Status}}')
        observed_lifecycle_state=$(lifecycle_state)
        case $observed_watcher_state in
            exited | created)
                case $observed_lifecycle_state in
                    "$expected_lifecycle")
                        stable_checks=$((stable_checks + 1))
                        ;;
                    ready)
                        test "$lifecycle_target" = full || {
                            echo "runner lifecycle changed while watcher was fenced: ready" >&2
                            return 1
                        }
                        resurrections=$((resurrections + 1))
                        write_lifecycle rotating
                        test "$resurrections" -le "$max_resurrections" || {
                            echo "runner watcher exceeded lifecycle repair limit during quiescence" >&2
                            return 1
                        }
                        stable_checks=0
                        ;;
                    *)
                        echo "runner lifecycle changed while watcher was fenced: $observed_lifecycle_state" >&2
                        return 1
                        ;;
                esac
                ;;
            running | restarting)
                test "$lifecycle_target" = full || {
                    echo "runner watcher resurrected during service quiescence: $observed_watcher_state" >&2
                    return 1
                }
                resurrections=$((resurrections + 1))
                restart_policy_disabled=1
                docker update --restart=no gluetun-rezka-watcher >/dev/null
                docker stop gluetun-rezka-watcher >/dev/null
                write_lifecycle rotating
                test "$resurrections" -le "$max_resurrections" || {
                    echo "runner watcher exceeded resurrection limit during quiescence" >&2
                    return 1
                }
                stable_checks=0
                ;;
            *)
                echo "runner watcher entered unexpected state during quiescence: $observed_watcher_state" >&2
                return 1
                ;;
        esac
        test "$observations" -le "$max_observations" || {
            echo "runner watcher fence exceeded its bounded observation window during quiescence" >&2
            return 1
        }
        test "$stable_checks" -ge "$checks" || sleep 1
    done
}
normalize_full_runner() {
    current_active=$(active_job_count)
    test "$current_active" = 0 || {
        echo "a job became active while normalizing the full runner hold" >&2
        return 1
    }
    current_runner_state=$(docker inspect download-runner --format '{{.State.Status}}')
    case $current_runner_state in
        running)
            docker stop download-runner >/dev/null
            write_lifecycle rotating
            wait_watcher_fence rotating "$watcher_fence_checks" || return 1
            ;;
        exited | created) ;;
        *)
            echo "download runner entered unexpected state during full quiescence: $current_runner_state" >&2
            return 1
            ;;
    esac
    current_runner_state=$(docker inspect download-runner --format '{{.State.Status}}')
    test "$current_runner_state" = exited || test "$current_runner_state" = created || {
        echo "download runner did not remain stopped during full quiescence" >&2
        return 1
    }
    current_active=$(active_job_count)
    test "$current_active" = 0 || {
        echo "a job became active after normalizing the full runner hold" >&2
        return 1
    }
}
full_safe_hold() {
    test "$lifecycle_target" = full || return 1
    require_media_hold=${1:-0}
    stopped_state() {
        test "$1" = exited || test "$1" = created
    }
    lifecycle=$(lifecycle_state) || return 1
    watcher_state=$(docker inspect gluetun-rezka-watcher --format '{{.State.Status}}') || return 1
    runner_state=$(docker inspect download-runner --format '{{.State.Status}}') || return 1
    runner_restart_policy=$(docker inspect download-runner --format '{{.HostConfig.RestartPolicy.Name}}') || return 1
    active=$(active_job_count) || return 1
    media_state=$(docker inspect media-service --format '{{.State.Status}}') || return 1
    media_restart_policy=$(docker inspect media-service --format '{{.HostConfig.RestartPolicy.Name}}') || return 1
    session_volume=$(docker inspect download-runner --format '{{range .Mounts}}{{if eq .Destination "/var/lib/media-orchestrator/session"}}{{.Name}}{{end}}{{end}}') || return 1
    test "$lifecycle" = rotating && stopped_state "$watcher_state" \
        && stopped_state "$runner_state" && test "$active" = 0 || return 1
    if test "$require_media_hold" = 1; then
        stopped_state "$media_state" && test "$media_restart_policy" = no \
            && test "$runner_restart_policy" = no \
            && test "$session_volume" = "$initial_session_volume" || return 1
    fi

    lifecycle_again=$(lifecycle_state) || return 1
    watcher_state_again=$(docker inspect gluetun-rezka-watcher --format '{{.State.Status}}') || return 1
    runner_state_again=$(docker inspect download-runner --format '{{.State.Status}}') || return 1
    runner_restart_policy_again=$(docker inspect download-runner --format '{{.HostConfig.RestartPolicy.Name}}') || return 1
    active_again=$(active_job_count) || return 1
    media_state_again=$(docker inspect media-service --format '{{.State.Status}}') || return 1
    media_restart_policy_again=$(docker inspect media-service --format '{{.HostConfig.RestartPolicy.Name}}') || return 1
    session_volume_again=$(docker inspect download-runner --format '{{range .Mounts}}{{if eq .Destination "/var/lib/media-orchestrator/session"}}{{.Name}}{{end}}{{end}}') || return 1
    test "$lifecycle_again" = rotating && stopped_state "$watcher_state_again" \
        && stopped_state "$runner_state_again" && test "$active_again" = 0 \
        && test "$lifecycle_again" = "$lifecycle" \
        && test "$watcher_state_again" = "$watcher_state" \
        && test "$runner_state_again" = "$runner_state" \
        && test "$active_again" = "$active" \
        && { test "$require_media_hold" != 1 || { stopped_state "$media_state_again" \
            && test "$media_restart_policy_again" = no \
            && test "$runner_restart_policy_again" = no \
            && test "$session_volume_again" = "$initial_session_volume"; }; }
}
cleanup_full_safe_hold_body() {
    # Once the final two-snapshot hold has passed, leave every stopped
    # container and the rotating gate intact for recovery. Before that point,
    # roll back only the bounded fast-path mutations. In particular, never
    # start download-runner from a signal/failure handler.
    test "${fast_safe_hold_reached:-0}" = 1 && return 0
    cleanup_failed=0
    cleanup_active=$(active_job_count 2>/dev/null || printf 1)

    cleanup_runner_state=$(docker inspect download-runner --format '{{.State.Status}}' 2>/dev/null || printf unknown)
    if test "$cleanup_runner_state" = running || test "$cleanup_runner_state" = restarting; then
        if test "$cleanup_active" = 0; then
            docker stop download-runner >/dev/null 2>&1 || cleanup_failed=1
        fi
    fi

    # Stop the lifecycle writer before restoring the gate. Disabling its
    # restart policy first makes this a bounded, fail-closed fence.
    cleanup_watcher_state=$(docker inspect gluetun-rezka-watcher --format '{{.State.Status}}' 2>/dev/null || printf unknown)
    if test "$cleanup_watcher_state" = running || test "$cleanup_watcher_state" = restarting; then
        docker update --restart=no gluetun-rezka-watcher >/dev/null 2>&1 || cleanup_failed=1
        docker stop gluetun-rezka-watcher >/dev/null 2>&1 || cleanup_failed=1
    fi

    cleanup_media_state=$(docker inspect media-service --format '{{.State.Status}}' 2>/dev/null || printf unknown)
    if test "$initial_media_state" = running; then
        if test "$cleanup_media_state" != running; then
            docker start media-service >/dev/null 2>&1 || cleanup_failed=1
        fi
    elif test "$cleanup_media_state" = running || test "$cleanup_media_state" = restarting; then
        docker stop media-service >/dev/null 2>&1 || cleanup_failed=1
    fi

    # The direct DB write is intentional: media-service may be the container
    # that was stopped while entering the hold, so an HTTP lifecycle request
    # is not a safe cleanup dependency.
    if test "$cleanup_active" = 0; then
        write_lifecycle_direct_ready >/dev/null 2>&1 || cleanup_failed=1
    else
        cleanup_failed=1
    fi

    case $initial_media_restart_policy in
        '' | no) docker update --restart=no media-service >/dev/null 2>&1 || cleanup_failed=1 ;;
        *) docker update --restart="$initial_media_restart_policy" media-service >/dev/null 2>&1 || cleanup_failed=1 ;;
    esac
    case $initial_watcher_restart_policy in
        '' | no) docker update --restart=no gluetun-rezka-watcher >/dev/null 2>&1 || cleanup_failed=1 ;;
        *) docker update --restart="$initial_watcher_restart_policy" gluetun-rezka-watcher >/dev/null 2>&1 || cleanup_failed=1 ;;
    esac
    case $initial_runner_restart_policy in
        '' | no) docker update --restart=no download-runner >/dev/null 2>&1 || cleanup_failed=1 ;;
        *) docker update --restart="$initial_runner_restart_policy" download-runner >/dev/null 2>&1 || cleanup_failed=1 ;;
    esac

    # The fast path only ever restores a watcher that was running at entry;
    # the runner remains stopped so cleanup cannot launch a job.
    if test "$initial_watcher_state" = running; then
        cleanup_watcher_state=$(docker inspect gluetun-rezka-watcher --format '{{.State.Status}}' 2>/dev/null || printf unknown)
        if test "$cleanup_watcher_state" != running; then
            docker start gluetun-rezka-watcher >/dev/null 2>&1 || cleanup_failed=1
        fi
    fi

    cleanup_session_volume=$(docker inspect download-runner --format '{{range .Mounts}}{{if eq .Destination "/var/lib/media-orchestrator/session"}}{{.Name}}{{end}}{{end}}' 2>/dev/null || printf unknown)
    test "$cleanup_session_volume" = "$initial_session_volume" || cleanup_failed=1
    test "$cleanup_failed" = 0 || {
        echo "full safe-hold cleanup could not restore every bounded runtime invariant" >&2
        return 1
    }
}
cleanup_full_safe_hold_exit() {
    cleanup_status=$?
    trap - EXIT HUP INT TERM
    cleanup_full_safe_hold_body || true
    exit "$cleanup_status"
}
cleanup_full_safe_hold_signal() {
    trap - EXIT HUP INT TERM
    cleanup_full_safe_hold_body || true
    exit 1
}
if full_safe_hold 1; then
    # An already complete hold is the recovery checkpoint. Do not touch its
    # policies, containers, or lifecycle gate while taking the fast path.
    fast_safe_hold_reached=1
    exit 0
fi
if full_safe_hold; then
    # A previous interrupted full operation may already have left the exact
    # safe hold. Continue directly to replacement without opening the gate,
    # starting either container, or asking the watcher to rotate the VPN. The
    # old service is stopped here as the final fail-closed fence; schema
    # preflight already validated it while it was still reachable.
    trap cleanup_full_safe_hold_exit EXIT
    trap cleanup_full_safe_hold_signal HUP INT TERM
    runner_restart_policy_disabled=1
    docker update --restart=no download-runner >/dev/null
    test "$(docker inspect download-runner --format '{{.HostConfig.RestartPolicy.Name}}')" = no || {
        echo "could not disable download-runner restart policy for safe hold" >&2
        exit 1
    }
    docker update --restart=no gluetun-rezka-watcher >/dev/null
    test "$(docker inspect gluetun-rezka-watcher --format '{{.HostConfig.RestartPolicy.Name}}')" = no || {
        echo "could not disable runner watcher restart policy for safe hold" >&2
        exit 1
    }
    stop_media_for_full_hold || {
        echo "media-service did not enter the full safe hold" >&2
        exit 1
    }
    consume_watcher_fence rotating "$watcher_restart_drain_checks" || {
        echo "runner watcher did not remain stopped after the direct lifecycle fence" >&2
        exit 1
    }
    normalize_full_runner || {
        echo "runner did not remain in the full safe hold after watcher drain" >&2
        exit 1
    }
    full_safe_hold 1 || {
        echo "runner safe-hold state changed while stopping media-service" >&2
        exit 1
    }
    fast_safe_hold_reached=1
    trap - EXIT HUP INT TERM
    exit 0
fi
watcher_restart_policy=$(docker inspect gluetun-rezka-watcher --format '{{.HostConfig.RestartPolicy.Name}}')
restart_policy_disabled=1
start_container() {
    container=$1
    state=$(docker inspect "$container" --format '{{.State.Status}}')
    test "$state" = running && return 0
    docker start "$container" >/dev/null 2>&1 || {
        state=$(docker inspect "$container" --format '{{.State.Status}}')
        test "$state" = running || return 1
    }
}
restore_watcher_restart_policy() {
    test "$restart_policy_disabled" = 1 || return 0
    case $watcher_restart_policy in
        '' | no) docker update --restart=no gluetun-rezka-watcher >/dev/null ;;
        *) docker update --restart="$watcher_restart_policy" gluetun-rezka-watcher >/dev/null ;;
    esac
    restart_policy_disabled=0
}
restore_runner_restart_policy() {
    test "${runner_restart_policy_disabled:-0}" = 1 || return 0
    case $initial_runner_restart_policy in
        '' | no) docker update --restart=no download-runner >/dev/null ;;
        *) docker update --restart="$initial_runner_restart_policy" download-runner >/dev/null ;;
    esac
    runner_restart_policy_disabled=0
}
restore_media_runtime() {
    test "$media_restart_policy_disabled" = 1 || return 0
    current_media_state=$(docker inspect media-service --format '{{.State.Status}}')
    if test "$initial_media_state" = running; then
        test "$current_media_state" = running || start_container media-service
    else
        test "$current_media_state" != running || docker stop media-service >/dev/null
    fi
    case $initial_media_restart_policy in
        '' | no) docker update --restart=no media-service >/dev/null ;;
        *) docker update --restart="$initial_media_restart_policy" media-service >/dev/null ;;
    esac
    media_restart_policy_disabled=0
    media_stopped_for_quiescence=0
}
runtime_restored=0
restore_download_watcher() {
    test "$download_watcher_present" = 1 || return 0
    case $initial_download_watcher_restart_policy in
        '' | no) docker update --restart=no gluetun-watcher >/dev/null ;;
        *) docker update --restart="$initial_download_watcher_restart_policy" gluetun-watcher >/dev/null ;;
    esac
    if test "$initial_download_watcher_state" = running; then
        current=$(docker inspect gluetun-watcher --format '{{.State.Status}}')
        test "$current" = running || start_container gluetun-watcher
    fi
}
restore_runtime() {
    test "$runtime_restored" = 0 || return 0
    runtime_restored=1
    if test "$lifecycle_target" = full && test "$rotation_marked" = 1; then
        restore_media_runtime
        restore_runner_restart_policy
        # Keep the lifecycle gate closed until the runner and watcher are
        # available again; otherwise a restored media API could admit work
        # while the runner is still stopped.
        test "$initial_runner_state" != running || start_container download-runner
        restore_watcher_restart_policy
        if test "$initial_watcher_state" = running; then
            current_watcher_state=$(docker inspect gluetun-rezka-watcher --format '{{.State.Status}}')
            test "$current_watcher_state" = running || start_container gluetun-rezka-watcher
        fi
        restore_download_watcher
        current_lifecycle_state=$(lifecycle_state)
        test "$current_lifecycle_state" = ready || write_lifecycle ready
    else
        test "$initial_runner_state" != running || start_container download-runner
        if test "$initial_watcher_state" = running; then
            restore_watcher_restart_policy
            current_watcher_state=$(docker inspect gluetun-rezka-watcher --format '{{.State.Status}}')
            test "$current_watcher_state" = running || start_container gluetun-rezka-watcher
        fi
        restore_download_watcher
    fi
}
restore_on_exit() {
    exit_status=$?
    trap - EXIT HUP INT TERM
    restore_runtime
    restore_status=$?
    test "$exit_status" = 0 || exit "$exit_status"
    exit "$restore_status"
}
restore_on_signal() {
    trap - EXIT HUP INT TERM
    restore_runtime
    exit 1
}
trap restore_on_exit EXIT
trap restore_on_signal HUP INT TERM
test "$initial_watcher_state" = running || { echo "runner watcher is not running before quiescence" >&2; exit 1; }
lifecycle=$(docker exec media-postgres sh -lc 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc "select state from runner_lifecycle;"')
test "$lifecycle" = ready || { echo "runner lifecycle is not ready for quiescence: $lifecycle" >&2; exit 1; }
fence_lifecycle=ready
if test "$lifecycle_target" = full; then
    runner_restart_policy_disabled=1
    docker update --restart=no download-runner >/dev/null
    test "$(docker inspect download-runner --format '{{.HostConfig.RestartPolicy.Name}}')" = no || {
        echo "could not disable download-runner restart policy for full safe hold" >&2
        exit 1
    }
fi
restart_policy_disabled=1
docker update --restart=no gluetun-rezka-watcher >/dev/null
if test "$lifecycle_target" = full; then
    rotation_marked=1
    write_lifecycle rotating
    fence_lifecycle=rotating
fi
docker stop gluetun-rezka-watcher >/dev/null
if test "$download_watcher_present" = 1; then
    docker update --restart=no gluetun-watcher >/dev/null
    download_watcher_state=$(docker inspect gluetun-watcher --format '{{.State.Status}}')
    test "$download_watcher_state" = running && docker stop gluetun-watcher >/dev/null
fi
if test "$lifecycle_target" = full; then
    # The watcher may finish an in-flight lifecycle reconciliation while
    # docker stop is waiting. Re-assert rotating only after the process is
    # fully stopped, then hold that gate through the restart-manager window.
    write_lifecycle rotating
fi
active=$(active_job_count)
test "$active" = 0 || { echo "a job became active while quiescing the runner" >&2; exit 1; }
if test "$lifecycle_target" = full; then
    # Stop the only HTTP lifecycle writer before draining queued watcher
    # restarts. The direct database fence after media-service is fully stopped
    # wins over every request that the old process could have accepted.
    stop_media_for_full_hold || { echo "media-service did not enter the full safe hold" >&2; exit 1; }
    consume_watcher_fence "$fence_lifecycle" "$watcher_restart_drain_checks" || {
        echo "runner watcher did not remain stopped after the direct lifecycle fence" >&2
        exit 1
    }
    normalize_full_runner || { echo "runner did not remain in the full safe hold" >&2; exit 1; }
else
    # Service-only replacement keeps media-service available and preserves
    # the existing ready lifecycle while the stopped watcher is drained.
    consume_watcher_fence "$fence_lifecycle" "$watcher_restart_drain_checks"
    test "$initial_runner_state" != running || docker stop download-runner >/dev/null
fi
if test "$lifecycle_target" = full; then
    wait_watcher_fence "$fence_lifecycle" "$watcher_fence_checks"
fi
lifecycle=$(docker exec media-postgres sh -lc 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc "select state from runner_lifecycle;"')
if test "$lifecycle_target" = full; then
    test "$lifecycle" = rotating || { echo "runner lifecycle changed during quiescence: $lifecycle" >&2; exit 1; }
else
    test "$lifecycle" = ready || { echo "runner lifecycle changed during quiescence: $lifecycle" >&2; exit 1; }
fi
active=$(active_job_count)
test "$active" = 0 || { echo "a job became active before runner replacement" >&2; exit 1; }
wait_watcher_fence "$fence_lifecycle" "$watcher_fence_checks"
test "$(lifecycle_state)" = "$fence_lifecycle" || {
    echo "runner lifecycle changed after the final watcher fence" >&2
    exit 1
}
test "$(active_job_count)" = 0 || {
    echo "a job became active after the final watcher fence" >&2
    exit 1
}
if test "$lifecycle_target" = full; then
    test "$(docker inspect media-service --format '{{.State.Status}}')" = exited || \
        test "$(docker inspect media-service --format '{{.State.Status}}')" = created || {
        echo "media-service restarted during the final full safe hold" >&2
        exit 1
    }
    test "$(docker inspect media-service --format '{{.HostConfig.RestartPolicy.Name}}')" = no || {
        echo "media-service restart policy changed during the final full safe hold" >&2
        exit 1
    }
    test "$(docker inspect download-runner --format '{{range .Mounts}}{{if eq .Destination "/var/lib/media-orchestrator/session"}}{{.Name}}{{end}}{{end}}')" = "$initial_session_volume" || {
        echo "encrypted runner session volume changed during the final full safe hold" >&2
        exit 1
    }
fi
trap - EXIT HUP INT TERM
REMOTE
}

resume_runner_watcher_and_wait_ready() {
    watcher_restart_policy=${1:-unless-stopped}
    remote sh -s "$watcher_restart_policy" <<'REMOTE'
set -eu
watcher_restart_policy=$1
case "$watcher_restart_policy" in
    no | always | unless-stopped | on-failure | on-failure:*) ;;
    *) echo "invalid watcher restart policy: $watcher_restart_policy" >&2; exit 1 ;;
esac
hold_quiescence() {
    docker stop gluetun-rezka-watcher >/dev/null 2>&1 || true
    docker stop download-runner >/dev/null 2>&1 || true
    docker stop gluetun-watcher >/dev/null 2>&1 || true
}
trap hold_quiescence EXIT HUP INT TERM
start_container() {
    container=$1
    state=$(docker inspect "$container" --format '{{.State.Status}}')
    test "$state" = running && return 0
    docker start "$container" >/dev/null 2>&1 || {
        state=$(docker inspect "$container" --format '{{.State.Status}}')
        test "$state" = running || return 1
    }
}
start_container download-runner
docker update --restart="$watcher_restart_policy" gluetun-rezka-watcher >/dev/null
start_container gluetun-rezka-watcher
attempts=0
while test "$attempts" -lt 30; do
    watcher_health=$(docker inspect gluetun-rezka-watcher --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}')
    lifecycle=$(docker exec media-postgres sh -lc 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc "select state from runner_lifecycle;"')
    if test "$watcher_health" = healthy && test "$lifecycle" = ready; then
        if docker inspect gluetun-watcher >/dev/null 2>&1; then
            download_state=$(docker inspect gluetun-watcher --format '{{.State.Status}}')
            if test "$download_state" != running; then
                # Restore unless-stopped (compose default) when we only know it was fenced.
                docker update --restart=unless-stopped gluetun-watcher >/dev/null 2>&1 || true
                start_container gluetun-watcher || true
            fi
        fi
        trap - EXIT HUP INT TERM
        exit 0
    fi
    test "$watcher_health" != unhealthy || { echo "runner watcher became unhealthy" >&2; exit 1; }
    attempts=$((attempts + 1))
    test "$attempts" -ge 30 || sleep 5
done
echo "runner watcher did not restore lifecycle ready within the bounded window" >&2
exit 1
REMOTE
}

hold_runner_quiescence() {
    remote "docker stop gluetun-rezka-watcher download-runner gluetun-watcher >/dev/null 2>&1 || true"
}

restore_full_runtime_safe_hold() {
    session_volume=$1
    remote sh -s "$environment_file" "$session_volume" <<'REMOTE'
set -eu
environment_file=$1
expected_session_volume=$2
watcher_restart_drain_checks=65
watcher_fence_checks=3
read_env() {
    key=$1
    value=$(sed -n "s/^${key}=//p" "$environment_file")
    test "$(printf '%s\n' "$value" | wc -l | tr -d ' ')" -le 1 || {
        echo "$key must occur at most once in $environment_file" >&2
        exit 1
    }
    printf '%s' "$value"
}
lifecycle_state() {
    docker exec media-postgres sh -lc 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc "select state from runner_lifecycle;"'
}
active_job_count() {
    states=$(docker exec media-postgres sh -lc 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc "select state from jobs;"')
    printf '%s\n' "$states" | grep -Ec '^(leased|running|cancel_requested|publishing|plex_pending)$' || true
}
wait_watcher_fence() {
    checks=${1:-1}
    attempts=0
    while test "$attempts" -lt "$checks"; do
        observed_watcher_state=$(docker inspect gluetun-rezka-watcher --format '{{.State.Status}}')
        observed_lifecycle_state=$(lifecycle_state)
        case $observed_watcher_state in
            exited | created) ;;
            *)
                echo "runner watcher resurrected during full recovery safe hold: $observed_watcher_state" >&2
                return 1
                ;;
        esac
        test "$observed_lifecycle_state" = rotating || {
            echo "full recovery lifecycle changed while watcher was fenced: $observed_lifecycle_state" >&2
            return 1
        }
        attempts=$((attempts + 1))
        test "$attempts" -ge "$checks" || sleep 1
    done
}
wait_media_stopped() {
    checks=${1:-1}
    attempts=0
    while test "$attempts" -lt "$checks"; do
        observed_media_state=$(docker inspect media-service --format '{{.State.Status}}')
        case $observed_media_state in
            exited | created) ;;
            *)
                echo "media-service restarted during full recovery safe hold: $observed_media_state" >&2
                return 1
                ;;
        esac
        attempts=$((attempts + 1))
        test "$attempts" -ge "$checks" || sleep 1
    done
}
session_volume() {
    docker inspect download-runner --format '{{range .Mounts}}{{if eq .Destination "/var/lib/media-orchestrator/session"}}{{.Name}}{{end}}{{end}}'
}
test -n "$expected_session_volume" || {
    echo "encrypted runner session volume is required for full recovery" >&2
    exit 1
}
watcher_uid=$(read_env PUID)
watcher_gid=$(read_env PGID)
watcher_uid=${watcher_uid:-1000}
watcher_gid=${watcher_gid:-1000}
printf '%s\n' "$watcher_uid" | grep -Eq '^[0-9]+$' || { echo "PUID must be numeric" >&2; exit 1; }
printf '%s\n' "$watcher_gid" | grep -Eq '^[0-9]+$' || { echo "PGID must be numeric" >&2; exit 1; }

safe_hold_snapshot() {
    media_state=$(docker inspect media-service --format '{{.State.Status}}')
    media_restart_policy=$(docker inspect media-service --format '{{.HostConfig.RestartPolicy.Name}}')
    lifecycle=$(lifecycle_state)
    watcher_state=$(docker inspect gluetun-rezka-watcher --format '{{.State.Status}}')
    runner_state=$(docker inspect download-runner --format '{{.State.Status}}')
    runner_restart_policy=$(docker inspect download-runner --format '{{.HostConfig.RestartPolicy.Name}}')
    active=$(active_job_count)
    held_session_volume=$(session_volume)
    printf '%s|%s|%s|%s|%s|%s|%s|%s\n' \
        "$media_state" "$media_restart_policy" "$lifecycle" "$watcher_state" \
        "$runner_state" "$runner_restart_policy" "$active" "$held_session_volume"
}
exact_safe_hold() {
    fence_checks=${1:-$watcher_fence_checks}
    stopped_state() {
        test "$1" = exited || test "$1" = created
    }
    wait_media_stopped "$fence_checks" || return 1
    wait_watcher_fence "$fence_checks" || return 1
    first=$(safe_hold_snapshot) || return 1
    second=$(safe_hold_snapshot) || return 1
    test "$first" = "$second" || return 1
    IFS='|' read -r media_state media_restart_policy lifecycle watcher_state runner_state runner_restart_policy active held_session_volume <<EOF
$second
EOF
    { test "$media_state" = exited || test "$media_state" = created; } \
        && test "$media_restart_policy" = no \
        && test "$runner_restart_policy" = no \
        && test "$lifecycle" = rotating && stopped_state "$watcher_state" \
        && stopped_state "$runner_state" && test "$active" = 0 \
        && test "$held_session_volume" = "$expected_session_volume"
}

write_lifecycle_direct_ready() {
    docker exec media-postgres sh -lc '
psql -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" -c "
UPDATE runner_lifecycle
SET state = '\''ready'\'', reason = NULL, previous_ip = NULL, current_ip = NULL,
    sticky_job_id = NULL, sticky_attempt_count = 0, updated_at = now()
WHERE singleton = true;"
'
}

# Capture the pre-recovery runtime before changing the watcher policy. If a
# recovery fence fails after stopping media-service, the EXIT/signal cleanup
# restores this bounded snapshot or leaves a verified exact hold.
initial_watcher_state=$(docker inspect gluetun-rezka-watcher --format '{{.State.Status}}')
initial_watcher_restart_policy=$(docker inspect gluetun-rezka-watcher --format '{{.HostConfig.RestartPolicy.Name}}')
initial_runner_state=$(docker inspect download-runner --format '{{.State.Status}}')
initial_runner_restart_policy=$(docker inspect download-runner --format '{{.HostConfig.RestartPolicy.Name}}')
initial_media_state=$(docker inspect media-service --format '{{.State.Status}}')
initial_media_restart_policy=$(docker inspect media-service --format '{{.HostConfig.RestartPolicy.Name}}')
recovery_exact=0
recovery_cleanup_running=0
restore_recovery_policy() {
    container=$1
    policy=$2
    case $policy in
        '' | no) docker update --restart=no "$container" >/dev/null 2>&1 ;;
        *) docker update --restart="$policy" "$container" >/dev/null 2>&1 ;;
    esac
}
start_recovery_container() {
    container=$1
    state=$(docker inspect "$container" --format '{{.State.Status}}' 2>/dev/null || printf unknown)
    test "$state" = running && return 0
    docker start "$container" >/dev/null 2>&1 || {
        state=$(docker inspect "$container" --format '{{.State.Status}}' 2>/dev/null || printf unknown)
        test "$state" = running
    }
}
recovery_cleanup_body() {
    test "${recovery_exact:-0}" = 1 && return 0
    test "${recovery_cleanup_running:-0}" = 0 || return 0
    recovery_cleanup_running=1
    cleanup_failed=0
    cleanup_active=$(active_job_count 2>/dev/null || printf 1)
    if test "$cleanup_active" != 0; then
        echo "recovery cleanup found an active job; retaining the rotating gate" >&2
        return 1
    fi

    cleanup_watcher_state=$(docker inspect gluetun-rezka-watcher --format '{{.State.Status}}' 2>/dev/null || printf unknown)
    if test "$cleanup_watcher_state" = running || test "$cleanup_watcher_state" = restarting; then
        docker update --restart=no gluetun-rezka-watcher >/dev/null 2>&1 || cleanup_failed=1
        docker stop gluetun-rezka-watcher >/dev/null 2>&1 || cleanup_failed=1
    fi
    cleanup_media_state=$(docker inspect media-service --format '{{.State.Status}}' 2>/dev/null || printf unknown)
    if test "$initial_media_state" = running; then
        if test "$cleanup_media_state" != running; then
            start_recovery_container media-service || cleanup_failed=1
        fi
    elif test "$cleanup_media_state" = running || test "$cleanup_media_state" = restarting; then
        docker stop media-service >/dev/null 2>&1 || cleanup_failed=1
    fi

    # Restore the gate while the watcher is stopped, then make the runner
    # available before reopening lifecycle. The watcher is last so it cannot
    # observe ready while the runner is still unavailable.
    restore_recovery_policy media-service "$initial_media_restart_policy" || cleanup_failed=1
    restore_recovery_policy download-runner "$initial_runner_restart_policy" || cleanup_failed=1
    cleanup_runner_available=1
    if test "$initial_runner_state" = running; then
        start_recovery_container download-runner || {
            cleanup_runner_available=0
            cleanup_failed=1
        }
    fi
    if test "$cleanup_runner_available" = 1; then
        write_lifecycle_direct_ready >/dev/null 2>&1 || cleanup_failed=1
    else
        echo "recovery cleanup kept lifecycle rotating because the runner did not recover" >&2
        return 1
    fi
    restore_recovery_policy gluetun-rezka-watcher "$initial_watcher_restart_policy" || cleanup_failed=1
    if test "$initial_watcher_state" = running; then
        start_recovery_container gluetun-rezka-watcher || cleanup_failed=1
    fi
    cleanup_session_volume=$(session_volume 2>/dev/null || printf unknown)
    test "$cleanup_session_volume" = "$expected_session_volume" || cleanup_failed=1
    test "$cleanup_failed" = 0 || {
        echo "full recovery cleanup could not restore the pre-recovery runtime" >&2
        return 1
    }
}
recovery_cleanup_exit() {
    cleanup_status=$?
    trap - EXIT HUP INT TERM
    recovery_cleanup_body || true
    exit "$cleanup_status"
}
recovery_cleanup_signal() {
    trap - EXIT HUP INT TERM
    recovery_cleanup_body || true
    exit 1
}
trap recovery_cleanup_exit EXIT
trap recovery_cleanup_signal HUP INT TERM

# Recovery can be interrupted after the old runtime has been fully fenced but
# before replacement starts. Accept that exact state idempotently, while
# double-checking every boundary before changing the watcher policy.
if exact_safe_hold "$watcher_restart_drain_checks"; then
    # The first exact snapshot is the durable re-entry checkpoint. Mark it
    # before any policy mutation so a signal during the second confirmation
    # preserves rotating+stopped state instead of reopening lifecycle ready.
    recovery_exact=1
    docker update --restart=no gluetun-rezka-watcher >/dev/null
    test "$(docker inspect gluetun-rezka-watcher --format '{{.HostConfig.RestartPolicy.Name}}')" = no || {
        echo "could not disable runner watcher restart policy for safe hold" >&2
        exit 1
    }
    exact_safe_hold "$watcher_fence_checks" || {
        echo "exact full recovery safe hold changed while re-entering it" >&2
        exit 1
    }
    trap - EXIT HUP INT TERM
    echo "recovery already held the exact full safe hold; continuing" >&2
    exit 0
fi

# Full recovery must never resume a checkpointed watcher. Its compose/script
# may predate the 0600 lifecycle-secret contract and may also encode different
# VPN/session behavior. Stop the watcher first, then fence lifecycle from an
# isolated helper while no watcher can reconcile it. The runner is stopped
# only after the rotating fence is verified, so it cannot race the transition.
test "$(active_job_count)" = 0 || {
    echo "a job is active; refusing to mutate the recovery boundary" >&2
    exit 1
}
docker update --restart=no download-runner >/dev/null
test "$(docker inspect download-runner --format '{{.HostConfig.RestartPolicy.Name}}')" = no || {
    echo "could not disable download-runner restart policy for full recovery safe hold" >&2
    exit 1
}
docker update --restart=no gluetun-rezka-watcher >/dev/null
test "$(docker inspect gluetun-rezka-watcher --format '{{.HostConfig.RestartPolicy.Name}}')" = no || {
    echo "could not disable runner watcher restart policy for safe hold" >&2
    exit 1
}
initial_watcher_state=$(docker inspect gluetun-rezka-watcher --format '{{.State.Status}}')
test "$initial_watcher_state" = running && docker stop gluetun-rezka-watcher >/dev/null
watcher_image=$(docker inspect gluetun-rezka-watcher --format '{{.Config.Image}}')
lifecycle_token_source=$(docker inspect gluetun-rezka-watcher --format '{{range .Mounts}}{{if eq .Destination "/run/secrets/media_lifecycle_token"}}{{.Source}}{{end}}{{end}}')
media_state=$(docker inspect media-service --format '{{.State.Status}}')
test "$media_state" = running || {
    echo "current media-service is unavailable; cannot fence full recovery" >&2
    exit 1
}
test -n "$watcher_image" && test -n "$lifecycle_token_source" || {
    echo "runner watcher image or lifecycle secret mount is missing; cannot verify safe hold" >&2
    exit 1
}
write_lifecycle_direct_rotating() {
    docker exec media-postgres sh -lc '
psql -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" -c "
UPDATE runner_lifecycle
SET state = '\''rotating'\'', reason = NULL, previous_ip = NULL, current_ip = NULL,
    sticky_job_id = NULL, sticky_attempt_count = 0, updated_at = now()
WHERE singleton = true;"
'
}
write_lifecycle_rotating_api() {
    docker run --rm \
        --user "$watcher_uid:$watcher_gid" \
        --read-only \
        --cap-drop ALL \
        --security-opt no-new-privileges:true \
        --network container:media-service \
        --mount "type=bind,source=$lifecycle_token_source,target=/run/secrets/media_lifecycle_token,readonly" \
        --env MEDIA_LIFECYCLE_TOKEN_FILE=/run/secrets/media_lifecycle_token \
        --env MEDIA_SERVICE_URL=http://127.0.0.1:8080 \
        --entrypoint /bin/sh "$watcher_image" -s - <<'WATCHER'
set -eu
token=$(tr -d '\n' < "$MEDIA_LIFECYCLE_TOKEN_FILE")
test -n "$token"
wget -q -T "${LIFECYCLE_HTTP_TIMEOUT:-10}" -O /dev/null \
    --header "Authorization: Bearer $token" \
    --header 'Content-Type: application/json' \
    --post-data '{"state":"rotating"}' \
    "$MEDIA_SERVICE_URL/v1/runner/lifecycle"
WATCHER
}
# set -u: must be defined before the first write_lifecycle_rotating call.
media_stopped_for_quiescence=0
write_lifecycle_rotating() {
    if test "$media_stopped_for_quiescence" = 1; then
        write_lifecycle_direct_rotating
    else
        write_lifecycle_rotating_api
    fi
}
consume_watcher_fence() {
    checks=${1:-1}
    max_resurrections=${2:-3}
    stable_checks=0
    resurrections=0
    observations=0
    max_observations=$((checks * 2))
    while test "$stable_checks" -lt "$checks"; do
        observations=$((observations + 1))
        observed_watcher_state=$(docker inspect gluetun-rezka-watcher --format '{{.State.Status}}')
        observed_lifecycle_state=$(lifecycle_state)
        case $observed_watcher_state in
            exited | created)
                case $observed_lifecycle_state in
                    rotating)
                        stable_checks=$((stable_checks + 1))
                        ;;
                    ready)
                        resurrections=$((resurrections + 1))
                        write_lifecycle_rotating
                        test "$resurrections" -le "$max_resurrections" || {
                            echo "runner watcher exceeded lifecycle repair limit during full recovery safe hold" >&2
                            return 1
                        }
                        stable_checks=0
                        ;;
                    *)
                        echo "full recovery lifecycle changed while watcher was fenced: $observed_lifecycle_state" >&2
                        return 1
                        ;;
                esac
                ;;
            running | restarting)
                resurrections=$((resurrections + 1))
                docker update --restart=no gluetun-rezka-watcher >/dev/null
                docker stop gluetun-rezka-watcher >/dev/null
                write_lifecycle_rotating
                test "$resurrections" -le "$max_resurrections" || {
                    echo "runner watcher exceeded resurrection limit during full recovery safe hold" >&2
                    return 1
                }
                stable_checks=0
                ;;
            *)
                echo "runner watcher entered unexpected state during full recovery safe hold: $observed_watcher_state" >&2
                return 1
                ;;
        esac
        test "$observations" -le "$max_observations" || {
            echo "runner watcher fence exceeded its bounded observation window during full recovery safe hold" >&2
            return 1
        }
        test "$stable_checks" -ge "$checks" || sleep 1
    done
}
write_lifecycle_rotating
test "$(active_job_count)" = 0 || { echo "a job became active before stopping the runner for full recovery" >&2; exit 1; }
media_restart_policy_disabled=1
docker update --restart=no media-service >/dev/null
test "$(docker inspect media-service --format '{{.HostConfig.RestartPolicy.Name}}')" = no || {
    echo "could not disable media-service restart policy for safe hold" >&2
    exit 1
}
media_state=$(docker inspect media-service --format '{{.State.Status}}')
test "$media_state" = running && docker stop media-service >/dev/null
wait_media_stopped "$watcher_restart_drain_checks" || {
    echo "media-service did not remain stopped during full recovery" >&2
    exit 1
}
media_stopped_for_quiescence=1
write_lifecycle_direct_rotating
consume_watcher_fence "$watcher_restart_drain_checks" || exit 1
# Do not stop a running runner until the post-rotation idle fence is proven.
# If a job appeared, fail while leaving the runner available to finish it.
test "$(active_job_count)" = 0 || { echo "a job became active before stopping the runner for full recovery" >&2; exit 1; }
runner_state=$(docker inspect download-runner --format '{{.State.Status}}')
test "$runner_state" = running && docker stop download-runner >/dev/null
wait_watcher_fence "$watcher_fence_checks" || exit 1
write_lifecycle_rotating
wait_watcher_fence "$watcher_fence_checks" || exit 1
test "$(docker inspect download-runner --format '{{.State.Status}}')" != running || {
    echo "download runner restarted during full recovery safe hold" >&2
    exit 1
}
test "$(active_job_count)" = 0 || { echo "a job became active during full recovery safe hold" >&2; exit 1; }
wait_watcher_fence "$watcher_fence_checks" || exit 1
test "$(lifecycle_state)" = rotating || { echo "full recovery lifecycle changed after final watcher fence" >&2; exit 1; }
test "$(active_job_count)" = 0 || { echo "a job became active after final full recovery fence" >&2; exit 1; }
test "$(session_volume)" = "$expected_session_volume" || {
    echo "encrypted runner session volume changed during recovery" >&2
    exit 1
}
exact_safe_hold "$watcher_fence_checks" || {
    echo "full recovery safe hold changed after stopping media-service" >&2
    exit 1
}
recovery_exact=1
echo "recovery restored checkpoint in safe hold; rerun guarded deploy-full" >&2
REMOTE
}

# Recovery-only replacement. The old service is already fenced by
# restore_full_runtime_safe_hold before sources, schema, or migrations are
# restored. Keep that service stopped while changing immutable image refs,
# recreating the containers, and replacing the dedicated VPN generation.
replace_full_runtime_safe_hold() {
    service_image=$1
    runner_image=$2
    expected_migration_version=$3
    expected_session_volume=$4
    test -n "$expected_migration_version" || { echo "held recovery migration version is required" >&2; return 1; }
    test -n "$expected_session_volume" || { echo "held recovery session volume is required" >&2; return 1; }
    prepare_watcher_runtime
    remote sh -s "$environment_file" "$remote_root" "$compose_project" "$service_image" "$runner_image" "$expected_migration_version" "$expected_session_volume" <<'REMOTE'
set -eu
environment_file=$1
remote_root=$2
compose_project=$3
service_image=$4
runner_image=$5
expected_migration_version=$6
expected_session_volume=$7
lifecycle_state() {
    docker exec media-postgres sh -lc 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc "select state from runner_lifecycle;"'
}
active_job_count() {
    states=$(docker exec media-postgres sh -lc 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc "select state from jobs;"')
    printf '%s\n' "$states" | grep -Ec '^(leased|running|cancel_requested|publishing|plex_pending)$' || true
}
session_volume() {
    docker inspect download-runner --format '{{range .Mounts}}{{if eq .Destination "/var/lib/media-orchestrator/session"}}{{.Name}}{{end}}{{end}}'
}
test "$(docker inspect media-service --format '{{.State.Status}}')" != running || {
    echo "held recovery would replace a running media-service" >&2
    exit 1
}
test "$(docker inspect gluetun-rezka-watcher --format '{{.State.Status}}')" != running || {
    echo "held recovery watcher is running" >&2
    exit 1
}
test "$(docker inspect download-runner --format '{{.State.Status}}')" != running || {
    echo "held recovery runner is running" >&2
    exit 1
}
test "$(lifecycle_state)" = rotating || { echo "held recovery lifecycle is not rotating" >&2; exit 1; }
test "$(active_job_count)" = 0 || { echo "held recovery has an active job" >&2; exit 1; }
test "$(docker inspect media-service --format '{{.HostConfig.RestartPolicy.Name}}')" = no || {
    echo "held recovery media-service restart policy is not disabled" >&2
    exit 1
}
test "$(docker inspect gluetun-rezka-watcher --format '{{.HostConfig.RestartPolicy.Name}}')" = no || {
    echo "held recovery watcher restart policy is not disabled" >&2
    exit 1
}
test "$(session_volume)" = "$expected_session_volume" || {
    echo "encrypted runner session volume changed before held recovery replacement" >&2
    exit 1
}
docker image inspect "$service_image" >/dev/null
docker image inspect "$runner_image" >/dev/null
umask 077
next=$(mktemp "${environment_file}.full-held-next.XXXXXX")
trap 'rm -f "$next"' EXIT HUP INT TERM
sed "s#^MEDIA_SERVICE_IMAGE=.*#MEDIA_SERVICE_IMAGE=$service_image#; s#^DOWNLOAD_RUNNER_IMAGE=.*#DOWNLOAD_RUNNER_IMAGE=$runner_image#" \
    "$environment_file" >"$next"
cd "$remote_root"
# Run migration as a disposable stopped service container. This never starts
# the restored media-service and keeps the DB postcondition explicit.
docker compose --project-name "$compose_project" --env-file "$next" \
    run --rm --no-deps media-service migrate
actual=$(docker exec media-postgres sh -lc 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc "select version from seaql_migrations order by version desc limit 1;"')
test "$actual" = "$expected_migration_version" || {
    echo "database migration differs from held recovery manifest" >&2
    exit 1
}
mv -f "$next" "$environment_file"
trap - EXIT HUP INT TERM

# media-service is created stopped. Recreate and health-check only the VPN
# namespace before creating its stopped dependents, because runner network_mode
# points at the dedicated Gluetun container.
docker compose --project-name "$compose_project" --env-file "$environment_file" \
    up --no-start --no-deps --force-recreate media-service
docker compose --project-name "$compose_project" --env-file "$environment_file" \
    up -d --no-deps --force-recreate gluetun-rezka
attempts=0
while :; do
    state=$(docker inspect gluetun-rezka --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}')
    test "$state" = healthy && break
    attempts=$((attempts + 1))
    test "$attempts" -lt 30 || { echo "gluetun-rezka is not healthy during held recovery: $state" >&2; exit 1; }
    sleep 5
done
docker compose --project-name "$compose_project" --env-file "$environment_file" \
    up --no-start --no-deps --force-recreate download-runner gluetun-rezka-watcher

docker update --restart=no media-service >/dev/null
docker update --restart=no download-runner >/dev/null
docker update --restart=no gluetun-rezka-watcher >/dev/null
for container in media-service download-runner gluetun-rezka-watcher; do
    test "$(docker inspect "$container" --format '{{.State.Status}}')" != running || {
        echo "$container started during held recovery" >&2
        exit 1
    }
    test "$(docker inspect "$container" --format '{{.HostConfig.RestartPolicy.Name}}')" = no || {
        echo "$container restart policy is not disabled during held recovery" >&2
        exit 1
    }
done
test "$(lifecycle_state)" = rotating || { echo "held recovery lifecycle changed after replacement" >&2; exit 1; }
test "$(active_job_count)" = 0 || { echo "a job became active during held recovery" >&2; exit 1; }
test "$(session_volume)" = "$expected_session_volume" || {
    echo "encrypted runner session volume changed during held recovery" >&2
    exit 1
}
test "$(docker inspect gluetun-rezka --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}')" = healthy || {
    echo "gluetun-rezka lost health during held recovery" >&2
    exit 1
}
echo "recovery restored checkpoint in exact safe hold; media-service remains stopped" >&2
REMOTE
}

verify_resumed_runtime_or_requiesce() {
    protected_before=$1
    protected_verifier=$2
    case $protected_verifier in
        assert_protected_unchanged | assert_full_protected_unchanged) ;;
        *) echo "invalid protected snapshot verifier" >&2; return 1 ;;
    esac
    if verify && "$protected_verifier" "$protected_before"; then
        return 0
    fi
    hold_runner_quiescence
    return 1
}

replace_images() {
    service_image=$1
    runner_image=$2
    expected_migration_version=${3:-}
    remote "set -eu; test \"\$(docker inspect gluetun-rezka-watcher --format '{{.State.Status}}')\" != running; test \"\$(docker inspect download-runner --format '{{.State.Status}}')\" != running; sed -i 's#^MEDIA_SERVICE_IMAGE=.*#MEDIA_SERVICE_IMAGE=$service_image#; s#^DOWNLOAD_RUNNER_IMAGE=.*#DOWNLOAD_RUNNER_IMAGE=$runner_image#' '$environment_file'; cd '$remote_root'; docker compose --project-name '$compose_project' --env-file '$environment_file' run --rm --no-deps media-service migrate; if test -n '$expected_migration_version'; then actual=\$(docker exec media-postgres sh -lc 'psql -U \"\$POSTGRES_USER\" -d \"\$POSTGRES_DB\" -Atc \"select version from seaql_migrations order by version desc limit 1;\"'); test \"\$actual\" = '$expected_migration_version' || { echo \"database migration differs from release manifest\" >&2; exit 1; }; fi; docker compose --project-name '$compose_project' --env-file '$environment_file' up -d --no-deps --force-recreate media-service; docker compose --project-name '$compose_project' --env-file '$environment_file' up --no-deps --force-recreate --no-start download-runner"
    verify_service
}

replace_full_runtime() {
    service_image=$1
    runner_image=$2
    expected_migration_version=${3:-}
    prepare_watcher_runtime
    gluetun_rezka_recreate_mode=$(remote sh -s "$environment_file" "$remote_root" "$compose_project" "$service_image" "$runner_image" "$expected_migration_version" <<'REMOTE'
set -eu
environment_file=$1
remote_root=$2
compose_project=$3
service_image=$4
runner_image=$5
expected_migration_version=$6
test "$(docker inspect gluetun-rezka-watcher --format '{{.State.Status}}')" != running
test "$(docker inspect download-runner --format '{{.State.Status}}')" != running
if docker inspect gluetun-watcher >/dev/null 2>&1; then
    test "$(docker inspect gluetun-watcher --format '{{.State.Status}}')" != running || {
        echo "download gluetun-watcher is still running during full runtime replace" >&2
        exit 1
    }
fi
docker image inspect "$service_image" >/dev/null
docker image inspect "$runner_image" >/dev/null
umask 077
next=$(mktemp "${environment_file}.full-next.XXXXXX")
trap 'rm -f "$next"' EXIT HUP INT TERM
sed "s#^MEDIA_SERVICE_IMAGE=.*#MEDIA_SERVICE_IMAGE=$service_image#; s#^DOWNLOAD_RUNNER_IMAGE=.*#DOWNLOAD_RUNNER_IMAGE=$runner_image#" \
    "$environment_file" >"$next"
cd "$remote_root"

gluetun_rezka_compose_digest() {
    env_file=$1
    json_file=$2
    docker compose --project-name "$compose_project" --env-file "$env_file" \
        config --format json >"$json_file"
    python3 - "$json_file" <<'PY'
import hashlib, json, pathlib, sys
config = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
service = config["services"]["gluetun-rezka"]
secrets = config.get("secrets") or {}
digest = hashlib.sha256()
payload = {
    "image": service.get("image"),
    "environment": service.get("environment"),
    "secrets": service.get("secrets"),
    "cap_add": service.get("cap_add"),
    "devices": service.get("devices"),
    "command": service.get("command"),
    "entrypoint": service.get("entrypoint"),
}
digest.update(json.dumps(payload, sort_keys=True, separators=(",", ":")).encode())
digest.update(b"\0")
def secret_name(value):
    if isinstance(value, str):
        return value
    if isinstance(value, dict):
        name = value.get("source") or value.get("target")
        if isinstance(name, str) and name:
            return name
    raise SystemExit(f"gluetun-rezka secret entry is invalid: {value!r}")

names = sorted(secret_name(value) for value in (service.get("secrets") or []))
for name in names:
    entry = secrets.get(name) or {}
    path = entry.get("file")
    digest.update(name.encode()); digest.update(b"\0")
    if path:
        digest.update(hashlib.sha256(pathlib.Path(path).read_bytes()).digest())
    digest.update(b"\0")
print(digest.hexdigest())
PY
}

contract_json=$(mktemp)
trap 'rm -f "$next" "$contract_json"' EXIT HUP INT TERM
before=$(gluetun_rezka_compose_digest "$environment_file" "$contract_json")
after=$(gluetun_rezka_compose_digest "$next" "$contract_json")
rm -f "$contract_json"
trap 'rm -f "$next"' EXIT HUP INT TERM

docker compose --project-name "$compose_project" --env-file "$next" run --rm --no-deps media-service migrate >&2
if test -n "$expected_migration_version"; then
    actual=$(docker exec media-postgres sh -lc 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc "select version from seaql_migrations order by version desc limit 1;"')
    test "$actual" = "$expected_migration_version" || { echo "database migration differs from release manifest" >&2; exit 1; }
fi
mv -f "$next" "$environment_file"
trap - EXIT HUP INT TERM
docker compose --project-name "$compose_project" --env-file "$environment_file" \
    up -d --no-deps --force-recreate media-service
attempts=0
while :; do
    state=$(docker inspect media-service --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}')
    test "$state" = healthy && break
    attempts=$((attempts + 1))
    test "$attempts" -lt 30 || { echo "media-service is not healthy: $state" >&2; exit 1; }
    sleep 5
done
rezka_mode=new
if test "$before" = "$after"; then
    echo "gluetun-rezka image/env/secrets unchanged; skipping force-recreate" >&2
    rezka_mode=same
else
    docker compose --project-name "$compose_project" --env-file "$environment_file" \
        up -d --no-deps --force-recreate gluetun-rezka
fi
attempts=0
while :; do
    state=$(docker inspect gluetun-rezka --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}')
    test "$state" = healthy && break
    attempts=$((attempts + 1))
    test "$attempts" -lt 30 || { echo "gluetun-rezka is not healthy: $state" >&2; exit 1; }
    sleep 5
done
docker compose --project-name "$compose_project" --env-file "$environment_file" \
    up --no-deps --force-recreate --no-start download-runner gluetun-rezka-watcher
printf '%s\n' "$rezka_mode"
REMOTE
) || return 1
    gluetun_rezka_recreate_mode=$(printf '%s
' "$gluetun_rezka_recreate_mode" | awk 'NF { line=$0 } END { print line }')
    case $gluetun_rezka_recreate_mode in
        new | same) ;;
        *)
            echo "full runtime replace returned invalid gluetun-rezka mode: $gluetun_rezka_recreate_mode" >&2
            return 1
            ;;
    esac
    verify_service
}


replace_service_image() {
    service_image=$1
    expected_migration_version=${2:-}
    remote sh -s "$environment_file" "$remote_root" "$compose_file" "$service_image" "$expected_migration_version" <<'REMOTE'
set -eu
environment_file=$1
remote_root=$2
compose_file=$3
service_image=$4
expected_migration_version=$5
test "$(grep -c '^MEDIA_SERVICE_IMAGE=' "$environment_file" || true)" = 1 || {
    echo "MEDIA_SERVICE_IMAGE must occur exactly once in $environment_file" >&2
    exit 1
}

docker image inspect "$service_image" >/dev/null
umask 077
next=$(mktemp "${environment_file}.next.XXXXXX")
trap 'rm -f "$next"' EXIT HUP INT TERM
sed "s#^MEDIA_SERVICE_IMAGE=.*#MEDIA_SERVICE_IMAGE=$service_image#" "$environment_file" >"$next"
cd "$remote_root"
docker compose --project-name homelab --env-file "$next" run --rm --no-deps media-service migrate >&2
if test -n "$expected_migration_version"; then
    actual=$(docker exec media-postgres sh -lc 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc "select version from seaql_migrations order by version desc limit 1;"')
    test "$actual" = "$expected_migration_version" || { echo "database migration differs from release manifest" >&2; exit 1; }
fi
mv -f "$next" "$environment_file"
trap - EXIT HUP INT TERM
docker compose --project-name homelab --env-file "$environment_file" up -d --no-deps --force-recreate media-service
REMOTE
    verify_service
}

migrate_down_one_with_image() {
    migration_image=$1
    expected_current=$2
    expected_target=$3
    validate_migration_version "$expected_current"
    validate_migration_version "$expected_target"
    test "$expected_current" != "$expected_target" || {
        echo "rollback migration versions must differ" >&2
        exit 1
    }
    remote sh -s "$environment_file" "$remote_root" "$compose_file" "$migration_image" "$expected_current" "$expected_target" <<'REMOTE'
set -eu
environment_file=$1
remote_root=$2
compose_file=$3
service_image=$4
expected_current=$5
expected_target=$6
docker image inspect "$service_image" >/dev/null
umask 077
rollback_env=$(mktemp "${environment_file}.rollback.XXXXXX")
trap 'rm -f "$rollback_env"' EXIT HUP INT TERM
sed "s#^MEDIA_SERVICE_IMAGE=.*#MEDIA_SERVICE_IMAGE=$service_image#" "$environment_file" >"$rollback_env"
cd "$remote_root"
docker compose --project-name homelab --env-file "$rollback_env" run --rm --no-deps media-service \
    migrate-down-one --expected-current "$expected_current" --expected-target "$expected_target"
REMOTE
    assert_db_migration_version "$expected_target"
}

prepare_hermes_cli() {
    service_image=$1
    docker_host=$2
    media_version=0.1.0
    artifact=$hermes_root/artifacts/media-$media_version-linux-amd64
    mkdir -p "$(dirname "$artifact")"
    container=$(DOCKER_HOST=$docker_host docker create "$service_image")
    trap 'DOCKER_HOST=$docker_host docker rm -f "$container" >/dev/null 2>&1 || true' EXIT HUP INT TERM
    DOCKER_HOST=$docker_host docker cp "$container:/usr/local/bin/media" "$artifact"
    DOCKER_HOST=$docker_host docker rm "$container" >/dev/null
    trap - EXIT HUP INT TERM
    chmod 0755 "$artifact"
    rsync -az --delete \
        --exclude .git \
        --exclude .worktrees/ \
        --exclude .env \
        --exclude artifacts/ \
        --exclude secrets/ \
        "$hermes_root/" "$host:$hermes_remote_root/"
    scp "$artifact" "$host:$hermes_remote_root/artifacts/media-$media_version-linux-amd64.next" >/dev/null
    remote "set -eu; mkdir -p '$hermes_remote_root/artifacts'; install -m 0755 '$hermes_remote_root/artifacts/media-$media_version-linux-amd64.next' '$hermes_remote_root/artifacts/media-$media_version-linux-amd64'; rm '$hermes_remote_root/artifacts/media-$media_version-linux-amd64.next'; sed -i '/^HERMES_HOME_IMAGE=/d; /^MEDIA_CLI_SHA256=/d' '$hermes_remote_root/.env'; cd '$remote_root'; attempts=0; until docker compose --project-name '$compose_project' --env-file '$environment_file' pull media-notifier-primary media-notifier-secondary hermes-primary hermes-secondary; do attempts=\$((attempts + 1)); test \"\$attempts\" -lt 5 || exit 1; sleep 5; done"
}

hermes_mount_inputs_digest() {
    python3 - "$hermes_root" <<'PY'
import hashlib
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
paths = [
    "shared/skills/media/SKILL.md",
    "shared/skills/media/MCP_SCHEMA.json",
    "shared/plugins/telegram-home/__init__.py",
    "shared/plugins/telegram-home/media_action_store.py",
    "shared/plugins/telegram-home/media_callbacks.py",
    "shared/plugins/telegram-home/media_panel.py",
    "shared/plugins/telegram-home/media_search.py",
    "shared/plugins/telegram-home/media_trending.py",
    "shared/plugins/telegram-home/assets/media-menu.jpg",
    "scripts/media-notifier",
    "scripts/hermes_media_notifications.py",
]
digest = hashlib.sha256()
for relative in paths:
    path = root / relative
    if not path.is_file():
        raise SystemExit(f"Hermes mount input missing: {relative}")
    digest.update(relative.encode("utf-8"))
    digest.update(b"\0")
    digest.update(hashlib.sha256(path.read_bytes()).digest())
    digest.update(b"\0")
print(digest.hexdigest())
PY
}

hermes_consumers_unchanged() {
    cli_sha=$1
    schema_sha=$2
    mounts_digest=$3
    remote sh -s "$hermes_remote_root" "$remote_schema_file" "$cli_sha" "$schema_sha" "$mounts_digest" <<'REMOTE'
set -eu
hermes_root=$1
schema_file=$2
cli_sha=$3
schema_sha=$4
mounts_digest=$5
media_version=0.1.0
artifact=$hermes_root/artifacts/media-$media_version-linux-amd64
test -x "$artifact" || exit 10
live_cli=$(sha256sum "$artifact" | awk '{print $1}')
test "$live_cli" = "$cli_sha" || exit 11
test -s "$schema_file" || exit 12
live_schema=$(sha256sum "$schema_file" | awk '{print $1}')
test "$live_schema" = "$schema_sha" || exit 13
live_mounts=$(python3 - "$hermes_root" <<'PY'
import hashlib
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
paths = [
    "shared/skills/media/SKILL.md",
    "shared/skills/media/MCP_SCHEMA.json",
    "shared/plugins/telegram-home/__init__.py",
    "shared/plugins/telegram-home/media_action_store.py",
    "shared/plugins/telegram-home/media_callbacks.py",
    "shared/plugins/telegram-home/media_panel.py",
    "shared/plugins/telegram-home/media_search.py",
    "shared/plugins/telegram-home/media_trending.py",
    "shared/plugins/telegram-home/assets/media-menu.jpg",
    "scripts/media-notifier",
    "scripts/hermes_media_notifications.py",
]
digest = hashlib.sha256()
for relative in paths:
    path = root / relative
    if not path.is_file():
        raise SystemExit(f"missing:{relative}")
    digest.update(relative.encode("utf-8"))
    digest.update(b"\0")
    digest.update(hashlib.sha256(path.read_bytes()).digest())
    digest.update(b"\0")
print(digest.hexdigest())
PY
) || exit 14
test "$live_mounts" = "$mounts_digest" || exit 15
exit 0
REMOTE
}

# Set by stage_hermes_cli: skip pull/recreate when CLI+schema+mounts unchanged.
hermes_skip_recreate=0

stage_hermes_cli() {
    service_image=$1
    docker_host=$2
    media_version=0.1.0
    artifact=$hermes_root/artifacts/media-$media_version-linux-amd64
    mkdir -p "$(dirname "$artifact")"
    container=$(DOCKER_HOST=$docker_host docker create "$service_image")
    trap 'DOCKER_HOST=$docker_host docker rm -f "$container" >/dev/null 2>&1 || true' EXIT HUP INT TERM
    DOCKER_HOST=$docker_host docker cp "$container:/usr/local/bin/media" "$artifact"
    DOCKER_HOST=$docker_host docker rm "$container" >/dev/null
    trap - EXIT HUP INT TERM
    chmod 0755 "$artifact"
    artifact_sha256=$(shasum -a 256 "$artifact" | awk '{print $1}')
    printf '%s\n' "$artifact_sha256" | grep -Eq '^[0-9a-f]{64}$' || { echo "local extracted CLI checksum is invalid" >&2; exit 1; }
    if test "${MEDIA_DEPLOY_RELEASE:-0}" = 1; then
        expected_cli_sha256=$(awk 'NF == 2 && $2 == "media-linux-amd64" { print $1 }' "$MEDIA_RELEASE_DIR/media-linux-amd64.sha256")
        printf '%s\n' "$expected_cli_sha256" | grep -Eq '^[0-9a-f]{64}$' || { echo "release bundle CLI checksum is invalid" >&2; exit 1; }
        test "$artifact_sha256" = "$expected_cli_sha256" || { echo "staged CLI differs from the release bundle" >&2; exit 1; }
        "$hermes_root/scripts/deploy-preflight" --staged-cli "$artifact"
    fi
    hermes_stage=$remote_root/media/.hermes-stage.$$
    remote "rm -rf '$hermes_stage'; mkdir -p '$hermes_stage/source' '$hermes_stage/artifacts'"
    rsync -az --delete \
        --exclude .git \
        --exclude .worktrees/ \
        --exclude .env \
        --exclude artifacts/ \
        --exclude secrets/ \
        "$hermes_root/" "$host:$hermes_stage/source/"
    scp "$artifact" "$host:$hermes_stage/artifacts/media-$media_version-linux-amd64" >/dev/null
    remote "set -eu; test \"\$(sha256sum '$hermes_stage/artifacts/media-$media_version-linux-amd64' | awk '{print \$1}')\" = '$artifact_sha256'"
    schema_source=${MEDIA_RELEASE_DIR:+$MEDIA_RELEASE_DIR/MCP_SCHEMA.json}
    if test -z "${MEDIA_RELEASE_DIR:-}" || ! test -s "${schema_source:-}"; then
        schema_source=$hermes_root/shared/skills/media/MCP_SCHEMA.json
    fi
    test -s "$schema_source" || { echo "Hermes MCP schema missing for consumer skip check: $schema_source" >&2; exit 1; }
    staged_schema_sha256=$(shasum -a 256 "$schema_source" | awk '{print $1}')
    staged_mounts_digest=$(hermes_mount_inputs_digest)
    hermes_skip_recreate=0
    if hermes_consumers_unchanged "$artifact_sha256" "$staged_schema_sha256" "$staged_mounts_digest"; then
        hermes_skip_recreate=1
        echo "Hermes CLI/schema/mounts unchanged; skipping Hermes image pull" >&2
    else
        echo "Hermes CLI/schema/mounts changed; pulling Hermes consumer images" >&2
        remote "set -eu; cd '$remote_root'; attempts=0; until docker compose --project-name '$compose_project' --env-file '$environment_file' pull media-notifier-primary media-notifier-secondary hermes-primary hermes-secondary; do attempts=\$((attempts + 1)); test \"\$attempts\" -lt 5 || exit 1; sleep 5; done"
    fi
}


activate_hermes_stage() {
    remote sh -s "$hermes_stage" "$hermes_remote_root" <<'REMOTE'
set -eu
stage=$1
hermes_root=$2
test -d "$stage/source"
test -x "$stage/artifacts/media-0.1.0-linux-amd64"
rsync -a --delete \
    --exclude .git \
    --exclude .worktrees/ \
    --exclude .env \
    --exclude artifacts/ \
    --exclude secrets/ \
    "$stage/source/" "$hermes_root/"
mkdir -p "$hermes_root/artifacts"
install -m 0755 "$stage/artifacts/media-0.1.0-linux-amd64" "$hermes_root/artifacts/media-0.1.0-linux-amd64.next"
mv -f "$hermes_root/artifacts/media-0.1.0-linux-amd64.next" "$hermes_root/artifacts/media-0.1.0-linux-amd64"
sed -i '/^HERMES_HOME_IMAGE=/d; /^MEDIA_CLI_SHA256=/d' "$hermes_root/.env"
REMOTE
}

cleanup_hermes_stage() {
    test -z "${hermes_stage:-}" || remote "rm -rf '$hermes_stage'"
}

sync_homelab_compose() {
    source=$homelab_root/media/compose.media-orchestrator.yml
    watcher_source=$homelab_root/media/gluetun-rezka-watcher/watch.sh
    test -s "$source" || { echo "homelab media Compose file is missing: $source" >&2; return 1; }
    test -s "$watcher_source" || { echo "homelab runner watcher script is missing: $watcher_source" >&2; return 1; }
    scp "$source" "$host:$compose_file.next" >/dev/null
    scp "$watcher_source" "$host:$watcher_script_file.next" >/dev/null
    remote sh -s "$compose_file.next" "$compose_file" "$watcher_script_file.next" "$watcher_script_file" <<'REMOTE'
set -eu
compose_next=$1
compose_file=$2
watcher_next=$3
watcher_file=$4
test -s "$compose_next"
test -s "$watcher_next"
install -m 0644 "$compose_next" "$compose_file.next.ready"
install -m 0755 "$watcher_next" "$watcher_file.next.ready"
mv -f "$compose_file.next.ready" "$compose_file"
mv -f "$watcher_file.next.ready" "$watcher_file"
rm -f "$compose_next" "$watcher_next"
REMOTE
}

prepare_watcher_runtime() {
    remote sh -s "$environment_file" <<'REMOTE'
set -eu
environment_file=$1
read_env() {
    key=$1
    count=$(grep -c "^${key}=" "$environment_file" || true)
    test "$count" -le 1 || { echo "$key must occur at most once in $environment_file" >&2; exit 1; }
    sed -n "s/^${key}=//p" "$environment_file"
}
uid=$(read_env PUID)
gid=$(read_env PGID)
uid=${uid:-1000}
gid=${gid:-1000}
printf '%s\n' "$uid" | grep -Eq '^[0-9]+$' || { echo "PUID must be numeric" >&2; exit 1; }
printf '%s\n' "$gid" | grep -Eq '^[0-9]+$' || { echo "PGID must be numeric" >&2; exit 1; }
socket_gid=$(stat -c '%g' /var/run/docker.sock)
printf '%s\n' "$socket_gid" | grep -Eq '^[0-9]+$' || { echo "Docker socket group ID is invalid" >&2; exit 1; }
next=$(mktemp "${environment_file}.socket-gid.XXXXXX")
trap 'rm -f "$next"' EXIT HUP INT TERM
awk -v gid="$socket_gid" '
    BEGIN { updated = 0 }
    /^DOCKER_SOCKET_GID=/ {
        if (!updated) {
            print "DOCKER_SOCKET_GID=" gid
            updated = 1
        }
        next
    }
    { print }
    END {
        if (!updated) print "DOCKER_SOCKET_GID=" gid
    }
' "$environment_file" >"$next"
mv -f "$next" "$environment_file"
trap - EXIT HUP INT TERM

state_volume=$(docker inspect gluetun-rezka-watcher --format '{{range .Mounts}}{{if eq .Destination "/state"}}{{.Name}}{{end}}{{end}}')
test -n "$state_volume" || { echo "runner watcher lifecycle volume is missing" >&2; exit 1; }
watcher_image=$(docker inspect gluetun-rezka-watcher --format '{{.Config.Image}}')
test -n "$watcher_image" || { echo "runner watcher image is missing" >&2; exit 1; }
docker image inspect "$watcher_image" >/dev/null
# The watcher runs as the application UID so Docker secrets with mode 0600 remain
# readable without granting DAC_OVERRIDE. Normalize its non-secret audit volume
# before recreating the watcher; this does not touch the encrypted session volume.
docker run --rm --user 0:0 --cap-drop ALL --cap-add CHOWN --security-opt no-new-privileges:true \
    --mount "type=volume,source=$state_volume,target=/state" \
    --entrypoint /bin/sh "$watcher_image" -s -c '
        uid=$1
        gid=$2
        chown "$uid:$gid" /state
        test ! -e /state/rotations.tsv || chown "$uid:$gid" /state/rotations.tsv
    ' sh "$uid" "$gid"
REMOTE
}

replace_hermes_agents() {
    image_record=${1:-}
    if test -z "$image_record" && test "${hermes_skip_recreate:-0}" = 1; then
        echo "Hermes CLI/schema/mounts unchanged; skipping Hermes consumer recreate" >&2
        remote sh -s <<'REMOTE'
set -eu
for name in media-notifier-primary media-notifier-secondary hermes-primary hermes-secondary; do
    state=$(docker inspect "$name" --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}')
    test "$state" = healthy || {
        echo "$name is not healthy while skipping recreate: $state" >&2
        exit 1
    }
done
REMOTE
        return 0
    fi
    remote sh -s "$remote_root" "$environment_file" "$image_record" <<'REMOTE'
set -eu
remote_root=$1
environment_file=$2
image_record=
if test "$#" -ge 3; then
    image_record=$3
fi
cd "$remote_root"
compose_files="-f compose.yml"
test -f compose.override.yml && compose_files="$compose_files -f compose.override.yml"
if test -n "$image_record"; then
    test -s "$image_record"
    override=$(mktemp)
    trap 'rm -f "$override"' EXIT HUP INT TERM
    python3 - "$image_record" "$override" <<'PY'
import json
import re
import sys

values = {}
with open(sys.argv[1], encoding="utf-8") as source:
    for line in source:
        key, separator, value = line.rstrip("\n").partition("=")
        if separator:
            values[key] = value
services = {
    "hermes-primary": values.get("HERMES_PRIMARY_IMAGE_ID"),
    "hermes-secondary": values.get("HERMES_SECONDARY_IMAGE_ID"),
    "media-notifier-primary": values.get("NOTIFIER_PRIMARY_IMAGE_ID"),
    "media-notifier-secondary": values.get("NOTIFIER_SECONDARY_IMAGE_ID"),
}
if any(not re.fullmatch(r"sha256:[0-9a-f]{64}", value or "") for value in services.values()):
    raise SystemExit("Hermes rollback image record is incomplete or invalid")
with open(sys.argv[2], "w", encoding="utf-8") as destination:
    json.dump({"services": {name: {"image": image} for name, image in services.items()}}, destination)
PY
    # shellcheck disable=SC2086
    docker compose --project-name homelab --env-file "$environment_file" $compose_files -f "$override" \
        up -d --no-deps --force-recreate media-notifier-primary media-notifier-secondary hermes-primary hermes-secondary
else
    docker compose --project-name homelab --env-file "$environment_file" \
        up -d --no-deps --force-recreate media-notifier-primary media-notifier-secondary hermes-primary hermes-secondary
fi
REMOTE
    remote sh -s <<'REMOTE'
set -eu
for name in media-notifier-primary media-notifier-secondary hermes-primary hermes-secondary; do
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

verify_runner_service_compatibility() {
    previous_runner_id=$1
    expected_service_image=$2
    expected_runner_image=$3
    generation_mode=${4:-new}
    remote sh -s "$previous_runner_id" "$expected_service_image" "$expected_runner_image" "$generation_mode" <<'REMOTE'
set -eu
previous_runner_id=$1
expected_service_image=$2
expected_runner_image=$3
generation_mode=$4
for image in "$expected_service_image" "$expected_runner_image"; do
    printf '%s\n' "$image" | grep -Eq '^sha256:[0-9a-f]{64}$' || { echo "compatibility image ID is invalid" >&2; exit 1; }
done
service_image=$(docker inspect media-service --format '{{.Image}}')
runner_image=$(docker inspect download-runner --format '{{.Image}}')
test "$service_image" = "$expected_service_image" || { echo "media-service is not running the attested image" >&2; exit 1; }
test "$runner_image" = "$expected_runner_image" || { echo "download-runner is not running the attested image" >&2; exit 1; }
runner_id=$(docker inspect download-runner --format '{{.Id}}')
case $generation_mode in
    new) test "$runner_id" != "$previous_runner_id" || { echo "download-runner was not replaced with a new generation" >&2; exit 1; } ;;
    same) test "$runner_id" = "$previous_runner_id" || { echo "service-only deployment recreated download-runner" >&2; exit 1; } ;;
    *) echo "invalid runner generation compatibility mode" >&2; exit 1 ;;
esac
since=$(docker inspect download-runner --format '{{.State.StartedAt}}')
attempts=0
while test "$attempts" -lt 30; do
    state=$(docker inspect download-runner --format '{{.State.Status}}')
    logs=$(docker logs --since "$since" download-runner 2>&1 || true)
    if printf '%s\n' "$logs" | grep -E 'runner iteration failed.*Service|runner service request failed' >/dev/null; then
        echo "download-runner iteration reported Service incompatibility" >&2
        exit 1
    fi
    case $state in
        running)
            health=$(docker inspect download-runner --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}missing{{end}}')
            case $health in
                healthy)
                    docker exec download-runner media healthcheck --url http://media-service:8080/v1/ready >/dev/null
                    exit 0
                    ;;
                starting) ;;
                *) echo "download-runner compatibility health is $health" >&2; exit 1 ;;
            esac
            ;;
        exited)
            exit_code=$(docker inspect download-runner --format '{{.State.ExitCode}}')
            test "$exit_code" = 0 || { echo "download-runner exited with status $exit_code" >&2; exit 1; }
            docker run --rm --network container:gluetun-rezka "$expected_runner_image" healthcheck --url http://media-service:8080/v1/ready >/dev/null
            exit 0
            ;;
        *) echo "download-runner has unexpected compatibility state: $state" >&2; exit 1 ;;
    esac
    attempts=$((attempts + 1))
    test "$attempts" -ge 30 || sleep 5
done
echo "download-runner did not reach a successful new-generation state" >&2
exit 1
REMOTE
}

verify_full_runtime_compatibility() {
    previous_runner_id=$1
    previous_rezka_id=$2
    previous_watcher_id=$3
    expected_service_image=$4
    expected_runner_image=$5
    expected_service_ref=$6
    expected_runner_ref=$7
    expected_session_volume=$8
    rezka_mode=${9:-new}
    case $rezka_mode in
        new | same) ;;
        *) echo "invalid gluetun-rezka compatibility mode: $rezka_mode" >&2; return 1 ;;
    esac
    remote sh -s "$previous_runner_id" "$previous_rezka_id" "$previous_watcher_id" \
        "$expected_service_image" "$expected_runner_image" "$expected_service_ref" \
        "$expected_runner_ref" "$expected_session_volume" "$rezka_mode" <<'REMOTE'
set -eu
previous_runner_id=$1
previous_rezka_id=$2
previous_watcher_id=$3
expected_service_image=$4
expected_runner_image=$5
expected_service_ref=$6
expected_runner_ref=$7
expected_session_volume=$8
rezka_mode=$9
for image in "$expected_service_image" "$expected_runner_image"; do
    printf '%s\n' "$image" | grep -Eq '^sha256:[0-9a-f]{64}$' || { echo "full runtime image ID is invalid" >&2; exit 1; }
done
test -n "$expected_service_ref" && test -n "$expected_runner_ref" && test -n "$expected_session_volume"
service_image=$(docker inspect media-service --format '{{.Image}}')
runner_image=$(docker inspect download-runner --format '{{.Image}}')
test "$service_image" = "$expected_service_image" || { echo "media-service is not running the attested image" >&2; exit 1; }
test "$runner_image" = "$expected_runner_image" || { echo "download-runner is not running the attested image" >&2; exit 1; }
service_ref=$(docker inspect media-service --format '{{.Config.Image}}')
runner_ref=$(docker inspect download-runner --format '{{.Config.Image}}')
watcher_probe_image=$(docker inspect gluetun-rezka-watcher --format '{{range .Config.Env}}{{println .}}{{end}}' | sed -n 's/^REZKA_PROBE_IMAGE=//p')
test "$service_ref" = "$expected_service_ref" || { echo "media-service image ref differs from the candidate .env" >&2; exit 1; }
test "$runner_ref" = "$expected_runner_ref" || { echo "download-runner image ref differs from the candidate .env" >&2; exit 1; }
test "$watcher_probe_image" = "$expected_runner_ref" || { echo "watcher REZKA_PROBE_IMAGE does not match DOWNLOAD_RUNNER_IMAGE" >&2; exit 1; }
runner_id=$(docker inspect download-runner --format '{{.Id}}')
rezka_id=$(docker inspect gluetun-rezka --format '{{.Id}}')
watcher_id=$(docker inspect gluetun-rezka-watcher --format '{{.Id}}')
test "$runner_id" != "$previous_runner_id" || { echo "download-runner was not recreated" >&2; exit 1; }
case $rezka_mode in
    new) test "$rezka_id" != "$previous_rezka_id" || { echo "gluetun-rezka was not recreated" >&2; exit 1; } ;;
    same) test "$rezka_id" = "$previous_rezka_id" || { echo "gluetun-rezka was recreated despite unchanged contract" >&2; exit 1; } ;;
    *) echo "invalid gluetun-rezka compatibility mode: $rezka_mode" >&2; exit 1 ;;
esac
test "$watcher_id" != "$previous_watcher_id" || { echo "gluetun-rezka-watcher was not recreated" >&2; exit 1; }
test "$(docker inspect gluetun-rezka --format '{{.State.Health.Status}}')" = healthy || { echo "gluetun-rezka is not healthy" >&2; exit 1; }
test "$(docker inspect gluetun-rezka-watcher --format '{{.State.Health.Status}}')" = healthy || { echo "gluetun-rezka-watcher is not healthy" >&2; exit 1; }
session_volume=$(docker inspect download-runner --format '{{range .Mounts}}{{if eq .Destination "/var/lib/media-orchestrator/session"}}{{.Name}}{{end}}{{end}}')
test "$session_volume" = "$expected_session_volume" || { echo "runner session volume changed" >&2; exit 1; }
REMOTE
}

image_suffix() {
    revision=$(git -C "$root" rev-parse --short HEAD)
    digest=$(source_tree_digest)
    printf 'local-%s-%s\n' "$revision" "$(printf '%s' "$digest" | cut -c1-12)"
}

perform_service_deploy() {
    sync_homelab_compose || return 1
    replace_service_image "$service_image" "$expected_migration_version" || return 1
    assert_db_migration_version "$expected_migration_version" || return 1
    sync_hermes_schema || return 1
    # Service-only does not stage a new CLI artifact; decide recreate from live CLI + local schema/mounts.
    if test -n "${hermes_root:-}" && test -d "$hermes_root"; then
        live_cli_sha=$(remote "sha256sum '$hermes_remote_root/artifacts/media-0.1.0-linux-amd64' 2>/dev/null | awk '{print \$1}'" || true)
        schema_source=$hermes_root/shared/skills/media/MCP_SCHEMA.json
        if test -n "${MEDIA_RELEASE_DIR:-}" && test -s "$MEDIA_RELEASE_DIR/MCP_SCHEMA.json"; then
            schema_source=$MEDIA_RELEASE_DIR/MCP_SCHEMA.json
        fi
        if test -n "$live_cli_sha" && test -s "$schema_source"; then
            staged_schema_sha256=$(shasum -a 256 "$schema_source" | awk '{print $1}')
            staged_mounts_digest=$(hermes_mount_inputs_digest)
            if hermes_consumers_unchanged "$live_cli_sha" "$staged_schema_sha256" "$staged_mounts_digest"; then
                hermes_skip_recreate=1
            else
                hermes_skip_recreate=0
            fi
        fi
    fi
    replace_hermes_agents || return 1
    verify_live_mcp_schema || return 1
    if test "${MEDIA_DEPLOY_RELEASE:-0}" = 1; then
        verify_local_backend_attestation || return 1
    else
        verify_running_service_attestation || return 1
    fi
    verify_mounted_hermes_sources || return 1
}

deploy_service() {
    check_hermes_capabilities
    assert_no_active_job
    verify_mounted_hermes_sources
    docker_host=${MEDIA_DOCKER_HOST:-ssh://$host}
    if test "${MEDIA_DEPLOY_RELEASE:-0}" = 1; then
        runner_image=$(release_value runner_image)
        assert_service_only_rollout "$(release_value runner_build_digest)" "$runner_image"
        service_image=$(release_value service_image)
        expected_migration_version=$(release_value migration_version)
        pull_release_image "$service_image"
        verify_release_image_attestation "$service_image" service
    else
        assert_service_only_rollout
        suffix=$(image_suffix)
        build_source_digest=$(source_tree_digest)
        build_runner_digest=$(runner_build_digest)
        service_image=media-orchestrator-service:$suffix
        (
            cd "$root"
            DOCKER_HOST=$docker_host MEDIA_SOURCE_TREE_DIGEST=$build_source_digest MEDIA_RUNNER_BUILD_DIGEST=$build_runner_digest MEDIA_BUILD_TARGETS=service MEDIA_SERVICE_IMAGE=$service_image ./scripts/docker-build.sh
        )
        remote "docker image inspect '$service_image' >/dev/null"
        verify_image_attestation "$service_image"
        expected_migration_version=$(latest_migration_version)
    fi
    service_image_id=$(immutable_image_id "$service_image")
    if test "${MEDIA_DEPLOY_RELEASE:-0}" != 1; then
        # Local builds are present only on this Docker host. Persist the exact
        # image ID in .env so a mutable local tag cannot be retargeted between
        # deployment and rollback. Release bundles retain their portable
        # registry@sha256 references.
        service_image=$service_image_id
    fi
    ensure_deployed_mcp_schema
    protected_before=$(protected_snapshot)
    assert_no_active_job
    runner_container_id=$(remote "docker inspect download-runner --format '{{.Id}}'")
    runner_image_id=$(running_image_id download-runner)
    watcher_restart_policy=$(remote "docker inspect gluetun-rezka-watcher --format '{{.HostConfig.RestartPolicy.Name}}'")
    assert_deploy_migration_baseline "$expected_migration_version"
    checkpoint_images
    quiesce_runner
    if ! (
        perform_service_deploy || exit 1
        resume_runner_watcher_and_wait_ready "$watcher_restart_policy" || exit 1
        if ! verify_runner_service_compatibility "$runner_container_id" "$service_image_id" "$runner_image_id" same; then
            hold_runner_quiescence
            exit 1
        fi
        verify_resumed_runtime_or_requiesce "$protected_before" assert_protected_unchanged || exit 1
    ); then
        echo "service deployment failed; restoring its exact checkpoint" >&2
        forward_service_image=$service_image
        read_rollback_images
        rollback_service_image=$service_image
        rollback_runner_image=$runner_image
        recovery_failed=0
        restore_checkpoint_deployment_sources || recovery_failed=1
        remote "set -eu; expected=\$(sed -n 's/^MCP_SCHEMA_SHA256=//p' '$rollback_file/images.env'); actual=\$(sha256sum '$rollback_file/MCP_SCHEMA.json' | awk '{print \$1}'); test \"\$expected\" = \"\$actual\"; cp '$rollback_file/MCP_SCHEMA.json' '$remote_schema_file.next'; chmod 0644 '$remote_schema_file.next'; mv -f '$remote_schema_file.next' '$remote_schema_file'" || recovery_failed=1
        current_migration_version=$(read_db_migration_version) || recovery_failed=1
        if test "$recovery_failed" = 0 && test "$current_migration_version" != "$rollback_migration_version"; then
            migrate_down_one_with_image "$forward_service_image" "$current_migration_version" "$rollback_migration_version" || recovery_failed=1
        fi
        replace_service_image "$rollback_service_image" || recovery_failed=1
        replace_hermes_agents "$rollback_file/hermes-images.env" || recovery_failed=1
        verify_live_mcp_schema || recovery_failed=1
        verify_mounted_hermes_sources remote || recovery_failed=1
        resume_runner_watcher_and_wait_ready "$watcher_restart_policy" || recovery_failed=1
        if test "$recovery_failed" = 0 && ! verify_runner_service_compatibility "$runner_container_id" "$rollback_service_image" "$rollback_runner_image" same; then
            hold_runner_quiescence
            recovery_failed=1
        fi
        test "$recovery_failed" != 0 || verify_resumed_runtime_or_requiesce "$protected_before" assert_protected_unchanged || recovery_failed=1
        test "$recovery_failed" = 0 || echo "service deployment recovery also failed" >&2
        return 1
    fi
}

deploy_full() {
    check_hermes_capabilities
    assert_no_active_job
    docker_host=${MEDIA_DOCKER_HOST:-ssh://$host}
    if test "${MEDIA_DEPLOY_RELEASE:-0}" = 1; then
        service_image=$(release_value service_image)
        runner_image=$(release_value runner_image)
        pull_release_image "$service_image"
        pull_release_image "$runner_image"
        verify_release_image_attestation "$service_image" service
        verify_release_image_attestation "$runner_image" runner
        expected_migration_version=$(release_value migration_version)
    else
        suffix=$(image_suffix)
        build_source_digest=$(source_tree_digest)
        build_runner_digest=$(runner_build_digest)
        service_image=media-orchestrator-service:$suffix
        runner_image=media-orchestrator-runner:$suffix
        (
            cd "$root"
            DOCKER_HOST=$docker_host \
                MEDIA_SOURCE_TREE_DIGEST=$build_source_digest \
                MEDIA_RUNNER_BUILD_DIGEST=$build_runner_digest \
                MEDIA_SERVICE_IMAGE=$service_image \
                MEDIA_RUNNER_IMAGE=$runner_image \
                ./scripts/docker-build.sh
        )
        verify_image_attestation "$service_image"
        verify_image_attestation "$runner_image"
        expected_migration_version=$(latest_migration_version)
    fi
    service_image_id=$(immutable_image_id "$service_image")
    runner_image_id=$(immutable_image_id "$runner_image")
    target_service_ref=$service_image
    target_runner_ref=$runner_image
    if test "${MEDIA_DEPLOY_RELEASE:-0}" != 1; then
        # Local builds are present only on this Docker host. Persist the exact
        # image IDs in .env so mutable local tags cannot be retargeted between
        # deployment and rollback. Release bundles retain their portable
        # registry@sha256 references.
        service_image=$service_image_id
        runner_image=$runner_image_id
        target_service_ref=$service_image
        target_runner_ref=$runner_image
    fi
    ensure_deployed_mcp_schema
    stage_hermes_cli "$service_image" "$docker_host"
    protected_before=$(full_protected_snapshot)
    assert_no_active_job
    previous_runner_id=$(remote "docker inspect download-runner --format '{{.Id}}'")
    previous_rezka_id=$(remote "docker inspect gluetun-rezka --format '{{.Id}}'")
    previous_watcher_id=$(remote "docker inspect gluetun-rezka-watcher --format '{{.Id}}'")
    session_volume=$(remote "docker inspect download-runner --format '{{range .Mounts}}{{if eq .Destination \"/var/lib/media-orchestrator/session\"}}{{.Name}}{{end}}{{end}}'")
    test -n "$session_volume" || { echo "download-runner session volume is missing" >&2; exit 1; }
    assert_deploy_migration_baseline "$expected_migration_version"
    checkpoint_images
    quiesce_runner full
    if ! (
        activate_hermes_stage || exit 1
        sync_homelab_compose || exit 1
        replace_full_runtime "$service_image" "$runner_image" "$expected_migration_version" || exit 1
        assert_db_migration_version "$expected_migration_version" || exit 1
        replace_hermes_agents || exit 1
        sync_hermes_schema || exit 1
        verify_live_mcp_schema || exit 1
        if test "${MEDIA_DEPLOY_RELEASE:-0}" = 1; then
            verify_local_backend_attestation || exit 1
            verify_running_release_refs "$service_image" "$runner_image" || exit 1
        else
            verify_running_image_attestations || exit 1
        fi
        verify_mounted_hermes_sources || exit 1
        resume_runner_watcher_and_wait_ready || exit 1
        if ! verify_runner_service_compatibility "$previous_runner_id" "$service_image_id" "$runner_image_id"; then
            hold_runner_quiescence
            exit 1
        fi
        verify_full_runtime_compatibility "$previous_runner_id" "$previous_rezka_id" "$previous_watcher_id" \
            "$service_image_id" "$runner_image_id" "$target_service_ref" "$target_runner_ref" "$session_volume" \
            "${gluetun_rezka_recreate_mode:-new}" || exit 1
        verify_resumed_runtime_or_requiesce "$protected_before" assert_full_protected_unchanged || exit 1
    ); then
        echo "full deployment failed; restoring its exact checkpoint" >&2
        forward_service_image=$service_image
        read_rollback_images
        rollback_service_image=$service_image
        rollback_runner_image=$runner_image
        recovery_failed=0
        restore_full_runtime_safe_hold "$session_volume" || recovery_failed=1
        if test "$recovery_failed" = 0; then
            restore_checkpoint_deployment_sources || recovery_failed=1
        fi
        if test "$recovery_failed" = 0; then
            remote "cp '$rollback_file/MCP_SCHEMA.json' '$remote_schema_file.next'; chmod 0644 '$remote_schema_file.next'; mv -f '$remote_schema_file.next' '$remote_schema_file'" || recovery_failed=1
        fi
        if test "$recovery_failed" = 0; then
            current_migration_version=$(read_db_migration_version) || recovery_failed=1
        fi
        if test "$recovery_failed" = 0 && test "$current_migration_version" != "$rollback_migration_version"; then
            migrate_down_one_with_image "$forward_service_image" "$current_migration_version" "$rollback_migration_version" || recovery_failed=1
        fi
        if test "$recovery_failed" = 0; then
            replace_full_runtime_safe_hold "$rollback_service_image" "$rollback_runner_image" "$rollback_migration_version" "$session_volume" || recovery_failed=1
        fi
        if test "$recovery_failed" = 0; then
            replace_hermes_agents "$rollback_file/hermes-images.env" || recovery_failed=1
        fi
        if test "$recovery_failed" = 0; then
            verify_mounted_hermes_sources remote || recovery_failed=1
        fi
        if test "$recovery_failed" = 0; then
            cleanup_hermes_stage || recovery_failed=1
        fi
        test "$recovery_failed" = 0 || echo "full deployment recovery also failed; runtime remains in safe hold" >&2
        return 1
    fi
    cleanup_hermes_stage
}

deploy_hermes() {
    check_hermes_capabilities
    assert_no_active_job
    verify_local_mcp_schema
    verify_local_backend_attestation
    service_image=$(running_image_id media-service)
    docker_host=${MEDIA_DOCKER_HOST:-ssh://$host}
    stage_hermes_cli "$service_image" "$docker_host"
    checkpoint_images
    protected_before=$(protected_snapshot)
    if ! (
        activate_hermes_stage || exit 1
        replace_hermes_agents || exit 1
        verify || exit 1
        verify_local_mcp_schema || exit 1
        verify_local_backend_attestation || exit 1
        verify_mounted_hermes_sources || exit 1
    ); then
        echo "Hermes deployment failed; restoring its exact checkpoint" >&2
        recovery_failed=0
        restore_checkpoint_deployment_sources || recovery_failed=1
        remote "set -eu; expected=\$(sed -n 's/^MCP_SCHEMA_SHA256=//p' '$rollback_file/images.env'); actual=\$(sha256sum '$rollback_file/MCP_SCHEMA.json' | awk '{print \$1}'); test \"\$expected\" = \"\$actual\"; cp '$rollback_file/MCP_SCHEMA.json' '$remote_schema_file.next'; chmod 0644 '$remote_schema_file.next'; mv -f '$remote_schema_file.next' '$remote_schema_file'" || recovery_failed=1
        replace_hermes_agents "$rollback_file/hermes-images.env" || recovery_failed=1
        verify_live_mcp_schema "$rollback_file/MCP_SCHEMA.json" || recovery_failed=1
        verify_local_backend_attestation || recovery_failed=1
        verify_mounted_hermes_sources remote || recovery_failed=1
        assert_protected_unchanged "$protected_before" || recovery_failed=1
        cleanup_hermes_stage || recovery_failed=1
        test "$recovery_failed" = 0 || echo "Hermes deployment recovery also failed" >&2
        return 1
    fi
    assert_protected_unchanged "$protected_before"
    cleanup_hermes_stage
}

read_rollback_images() {
    previous=$(remote "cat '$rollback_file/images.env'")
    service_image_ref=$(printf '%s\n' "$previous" | sed -n 's/^MEDIA_SERVICE_IMAGE=//p')
    runner_image_ref=$(printf '%s\n' "$previous" | sed -n 's/^DOWNLOAD_RUNNER_IMAGE=//p')
    service_image=$(printf '%s\n' "$previous" | sed -n 's/^SERVICE_IMAGE_ID=//p')
    runner_image=$(printf '%s\n' "$previous" | sed -n 's/^RUNNER_IMAGE_ID=//p')
    rollback_migration_version=$(printf '%s\n' "$previous" | sed -n 's/^DB_MIGRATION_VERSION=//p')
    test -n "$service_image_ref" && test -n "$runner_image_ref" &&
        test -n "$service_image" && test -n "$runner_image" && test -n "$rollback_migration_version" || {
        echo "rollback image record is incomplete" >&2
        exit 1
    }
    for image_id in "$service_image" "$runner_image"; do
        printf '%s\n' "$image_id" | grep -Eq '^sha256:[0-9a-f]{64}$' || {
            echo "rollback image ID is invalid" >&2
            exit 1
        }
    done
    validate_migration_version "$rollback_migration_version"
    remote sh -s "$rollback_file/images.env" "$service_image" "$runner_image" <<'REMOTE'
set -eu
record=$1
service_image=$2
runner_image=$3
verify_label() {
    image=$1
    label=$2
    key=$3
    expected=$(sed -n "s/^${key}=//p" "$record")
    actual=$(docker image inspect "$image" --format "{{index .Config.Labels \"$label\"}}")
    test -n "$expected" && test "$actual" = "$expected" || { echo "$image checkpoint digest differs: $label" >&2; exit 1; }
}
test "$(docker image inspect "$service_image" --format '{{.Id}}')" = "$service_image"
test "$(docker image inspect "$runner_image" --format '{{.Id}}')" = "$runner_image"
verify_label "$service_image" dev.iamstubborn.media.source-tree-digest SERVICE_SOURCE_TREE_DIGEST
verify_label "$runner_image" dev.iamstubborn.media.source-tree-digest RUNNER_SOURCE_TREE_DIGEST
verify_label "$service_image" dev.iamstubborn.media.runner-build-digest SERVICE_RUNNER_BUILD_DIGEST
verify_label "$runner_image" dev.iamstubborn.media.runner-build-digest RUNNER_RUNNER_BUILD_DIGEST
REMOTE
}

checkpoint_forward_deployment_sources() {
    destination=$1
    remote sh -s "$destination" "$remote_schema_file" "$compose_file" "$watcher_script_file" "$hermes_remote_root" <<'REMOTE'
set -eu
destination=$1
schema_file=$2
compose_file=$3
watcher_script_file=$4
hermes_root=$5
mkdir "$destination"
trap 'rm -rf "$destination"' EXIT HUP INT TERM
cp "$schema_file" "$destination/MCP_SCHEMA.json"
sha256sum "$schema_file" | awk '{print $1}' >"$destination/MCP_SCHEMA.sha256"
cp "$compose_file" "$destination/compose.media-orchestrator.yml"
cp "$watcher_script_file" "$destination/gluetun-rezka-watcher-watch.sh"
mkdir "$destination/hermes-source"
rsync -a --delete \
    --exclude .git \
    --exclude .worktrees/ \
    --exclude .env \
    --exclude artifacts/ \
    --exclude secrets/ \
    "$hermes_root/" "$destination/hermes-source/"
for name in hermes-primary hermes-secondary media-notifier-primary media-notifier-secondary; do
    image_id=$(docker inspect "$name" --format '{{.Image}}')
    image_ref=$(docker inspect "$name" --format '{{.Config.Image}}')
    printf '%s\n' "$image_id" | grep -Eq '^sha256:[0-9a-f]{64}$' || { echo "$name image ID is invalid" >&2; exit 1; }
    case $name in
        hermes-primary) key=HERMES_PRIMARY ;;
        hermes-secondary) key=HERMES_SECONDARY ;;
        media-notifier-primary) key=NOTIFIER_PRIMARY ;;
        media-notifier-secondary) key=NOTIFIER_SECONDARY ;;
    esac
    printf '%s_IMAGE_ID=%s\n%s_IMAGE_REF=%s\n' "$key" "$image_id" "$key" "$image_ref"
done >"$destination/hermes-images.env"
trap - EXIT HUP INT TERM
REMOTE
}

cleanup_forward_deployment_sources() {
    remote "rm -rf '$1'"
}

restore_checkpoint_deployment_sources() {
    remote sh -s "$rollback_file" "$compose_file" "$watcher_script_file" "$hermes_remote_root" "$schema_hash_file" <<'REMOTE'
set -eu
rollback_file=$1
compose_file=$2
watcher_script_file=$3
hermes_root=$4
schema_hash_file=$5
test -s "$rollback_file/compose.media-orchestrator.yml"
test -s "$rollback_file/gluetun-rezka-watcher-watch.sh"
test -s "$rollback_file/images.env"
test -d "$rollback_file/hermes-source"
test -s "$rollback_file/hermes-images.env"
schema_sha256=$(sed -n 's/^MCP_SCHEMA_SHA256=//p' "$rollback_file/images.env")
printf '%s\n' "$schema_sha256" | grep -Eq '^[0-9a-f]{64}$'
install -m 0644 "$rollback_file/compose.media-orchestrator.yml" "$compose_file.next"
mv -f "$compose_file.next" "$compose_file"
install -m 0755 "$rollback_file/gluetun-rezka-watcher-watch.sh" "$watcher_script_file.next"
mv -f "$watcher_script_file.next" "$watcher_script_file"
printf '%s\n' "$schema_sha256" >"$schema_hash_file.next"
mv -f "$schema_hash_file.next" "$schema_hash_file"
rsync -a --delete \
    --exclude .git \
    --exclude .worktrees/ \
    --exclude .env \
    --exclude artifacts/ \
    --exclude secrets/ \
    "$rollback_file/hermes-source/" "$hermes_root/"
REMOTE
}

restore_forward_deployment_sources() {
    remote sh -s "$forward_sources" "$compose_file" "$watcher_script_file" "$hermes_remote_root" "$schema_hash_file" <<'REMOTE'
set -eu
forward_sources=$1
compose_file=$2
watcher_script_file=$3
hermes_root=$4
schema_hash_file=$5
test -s "$forward_sources/compose.media-orchestrator.yml"
test -s "$forward_sources/gluetun-rezka-watcher-watch.sh"
test -s "$forward_sources/MCP_SCHEMA.sha256"
test -d "$forward_sources/hermes-source"
test -s "$forward_sources/hermes-images.env"
schema_sha256=$(tr -d '\n' < "$forward_sources/MCP_SCHEMA.sha256")
printf '%s\n' "$schema_sha256" | grep -Eq '^[0-9a-f]{64}$'
install -m 0644 "$forward_sources/compose.media-orchestrator.yml" "$compose_file.next"
mv -f "$compose_file.next" "$compose_file"
install -m 0755 "$forward_sources/gluetun-rezka-watcher-watch.sh" "$watcher_script_file.next"
mv -f "$watcher_script_file.next" "$watcher_script_file"
printf '%s\n' "$schema_sha256" >"$schema_hash_file.next"
mv -f "$schema_hash_file.next" "$schema_hash_file"
rsync -a --delete \
    --exclude .git \
    --exclude .worktrees/ \
    --exclude .env \
    --exclude artifacts/ \
    --exclude secrets/ \
    "$forward_sources/hermes-source/" "$hermes_root/"
REMOTE
}

perform_service_rollback() {
    restore_checkpoint_deployment_sources || return 1
    remote "set -eu; expected=\$(sed -n 's/^MCP_SCHEMA_SHA256=//p' '$rollback_file/images.env'); actual=\$(sha256sum '$rollback_file/MCP_SCHEMA.json' | awk '{print \$1}'); test \"\$expected\" = \"\$actual\"; cp '$rollback_file/MCP_SCHEMA.json' '$remote_schema_file.next'; chmod 0644 '$remote_schema_file.next'; mv -f '$remote_schema_file.next' '$remote_schema_file'" || return 1
    if test "$forward_migration_version" != "$rollback_migration_version"; then
        migrate_down_one_with_image "$forward_image" "$forward_migration_version" "$rollback_migration_version" || return 1
    fi
    assert_db_migration_version "$rollback_migration_version" || return 1
    replace_service_image "$service_image" || return 1
    replace_hermes_agents "$rollback_file/hermes-images.env" || return 1
    verify_live_mcp_schema || return 1
    verify_mounted_hermes_sources remote || return 1
}

rollback_service() {
    assert_no_active_job
    read_rollback_images
    protected_before=$(protected_snapshot)
    forward_image=$(running_image_id media-service)
    forward_runner_image=$(running_image_id download-runner)
    forward_migration_version=$(read_db_migration_version)
    forward_sources=$remote_root/media/.media-orchestrator-forward-service.$$
    forward_schema=$forward_sources/MCP_SCHEMA.json
    checkpoint_forward_deployment_sources "$forward_sources"
    assert_no_active_job
    runner_container_id=$(remote "docker inspect download-runner --format '{{.Id}}'")
    watcher_restart_policy=$(remote "docker inspect gluetun-rezka-watcher --format '{{.HostConfig.RestartPolicy.Name}}'")
    quiesce_runner
    if ! (
        perform_service_rollback || exit 1
        resume_runner_watcher_and_wait_ready "$watcher_restart_policy" || exit 1
        if ! verify_runner_service_compatibility "$runner_container_id" "$service_image" "$runner_image" same; then
            hold_runner_quiescence
            exit 1
        fi
        verify_resumed_runtime_or_requiesce "$protected_before" assert_protected_unchanged || exit 1
    ); then
        echo "service rollback failed; restoring the exact forward service and Hermes pair" >&2
        recovery_failed=0
        restore_forward_deployment_sources || recovery_failed=1
        remote "cp '$forward_schema' '$remote_schema_file.next'; chmod 0644 '$remote_schema_file.next'; mv -f '$remote_schema_file.next' '$remote_schema_file'" || recovery_failed=1
        replace_service_image "$forward_image" || recovery_failed=1
        assert_db_migration_version "$forward_migration_version" || recovery_failed=1
        replace_hermes_agents "$forward_sources/hermes-images.env" || recovery_failed=1
        verify_live_mcp_schema "$forward_schema" || recovery_failed=1
        verify_mounted_hermes_sources remote || recovery_failed=1
        resume_runner_watcher_and_wait_ready "$watcher_restart_policy" || recovery_failed=1
        if test "$recovery_failed" = 0 && ! verify_runner_service_compatibility "$runner_container_id" "$forward_image" "$forward_runner_image" same; then
            hold_runner_quiescence
            recovery_failed=1
        fi
        test "$recovery_failed" != 0 || verify_resumed_runtime_or_requiesce "$protected_before" assert_protected_unchanged || recovery_failed=1
        cleanup_forward_deployment_sources "$forward_sources" || recovery_failed=1
        test "$recovery_failed" = 0 || echo "forward service rollback recovery also failed" >&2
        return 1
    fi
    cleanup_forward_deployment_sources "$forward_sources"
}

perform_full_rollback() {
    restore_checkpoint_deployment_sources || return 1
    remote "set -eu; expected=\$(sed -n 's/^MCP_SCHEMA_SHA256=//p' '$rollback_file/images.env'); actual=\$(sha256sum '$rollback_file/MCP_SCHEMA.json' | awk '{print \$1}'); test \"\$expected\" = \"\$actual\"; cp '$rollback_file/MCP_SCHEMA.json' '$remote_schema_file.next'; chmod 0644 '$remote_schema_file.next'; mv -f '$remote_schema_file.next' '$remote_schema_file'" || return 1
    if test "$forward_migration_version" != "$rollback_migration_version"; then
        migrate_down_one_with_image "$forward_service_image" "$forward_migration_version" "$rollback_migration_version" || return 1
    fi
    assert_db_migration_version "$rollback_migration_version" || return 1
    replace_full_runtime "$service_image" "$runner_image" || return 1
    replace_hermes_agents "$rollback_file/hermes-images.env" || return 1
    verify_live_mcp_schema || return 1
    verify_mounted_hermes_sources remote || return 1
    resume_runner_watcher_and_wait_ready || return 1
    if ! verify_runner_service_compatibility "$previous_runner_id" "$service_image" "$runner_image"; then
        hold_runner_quiescence
        return 1
    fi
    verify_full_runtime_compatibility "$previous_runner_id" "$previous_rezka_id" "$previous_watcher_id" \
        "$service_image" "$runner_image" "$service_image" "$runner_image" "$session_volume" || return 1
    verify_resumed_runtime_or_requiesce "$protected_before" assert_full_protected_unchanged || return 1
}

rollback_full() {
    assert_no_active_job
    read_rollback_images
    protected_before=$(full_protected_snapshot)
    forward_service_image=$(running_image_id media-service)
    forward_runner_image=$(running_image_id download-runner)
    test -n "$forward_service_image" && test -n "$forward_runner_image" || {
        echo "forward image record is incomplete" >&2
        exit 1
    }
    remote "docker image inspect '$forward_service_image' >/dev/null && docker image inspect '$forward_runner_image' >/dev/null"
    forward_migration_version=$(read_db_migration_version)
    forward_sources=$remote_root/media/.media-orchestrator-forward-full.$$
    forward_schema=$forward_sources/MCP_SCHEMA.json
    checkpoint_forward_deployment_sources "$forward_sources"
    assert_no_active_job
    previous_runner_id=$(remote "docker inspect download-runner --format '{{.Id}}'")
    previous_rezka_id=$(remote "docker inspect gluetun-rezka --format '{{.Id}}'")
    previous_watcher_id=$(remote "docker inspect gluetun-rezka-watcher --format '{{.Id}}'")
    session_volume=$(remote "docker inspect download-runner --format '{{range .Mounts}}{{if eq .Destination \"/var/lib/media-orchestrator/session\"}}{{.Name}}{{end}}{{end}}'")
    test -n "$session_volume" || { echo "download-runner session volume is missing" >&2; exit 1; }
    quiesce_runner full
    if ! perform_full_rollback; then
        echo "full rollback failed; restoring the forward full stack" >&2
        recovery_failed=0
        restore_full_runtime_safe_hold "$session_volume" || recovery_failed=1
        if test "$recovery_failed" = 0; then
            restore_forward_deployment_sources || recovery_failed=1
        fi
        if test "$recovery_failed" = 0; then
            remote "cp '$forward_schema' '$remote_schema_file.next'; chmod 0644 '$remote_schema_file.next'; mv -f '$remote_schema_file.next' '$remote_schema_file'" || recovery_failed=1
        fi
        if test "$recovery_failed" = 0; then
            replace_full_runtime_safe_hold "$forward_service_image" "$forward_runner_image" "$forward_migration_version" "$session_volume" || recovery_failed=1
        fi
        if test "$recovery_failed" = 0; then
            assert_db_migration_version "$forward_migration_version" || recovery_failed=1
        fi
        if test "$recovery_failed" = 0; then
            replace_hermes_agents "$forward_sources/hermes-images.env" || recovery_failed=1
        fi
        if test "$recovery_failed" = 0; then
            verify_mounted_hermes_sources remote || recovery_failed=1
        fi
        if test "$recovery_failed" = 0; then
            cleanup_forward_deployment_sources "$forward_sources" || recovery_failed=1
        fi
        test "$recovery_failed" = 0 || echo "forward full-stack recovery also failed; runtime remains in safe hold" >&2
        return 1
    fi
    cleanup_forward_deployment_sources "$forward_sources"
}

deploy_release_service() {
    MEDIA_DEPLOY_RELEASE=1 with_release_snapshot deploy_service
}

deploy_release_full() {
    MEDIA_DEPLOY_RELEASE=1 with_release_snapshot deploy_full
}

deploy_release_hermes() {
    MEDIA_DEPLOY_RELEASE=1 with_release_snapshot deploy_hermes
}

deploy_local_service() {
    require_homelab_root
    MEDIA_DEPLOY_RELEASE=0 deploy_service
}

deploy_local_full() {
    require_homelab_root
    MEDIA_DEPLOY_RELEASE=0 deploy_full
}

case ${1:-} in
    status) status ;;
    verify) verify ;;
    deploy | deploy-service) with_host_lock deploy_release_service ;;
    deploy-full) with_host_lock deploy_release_full ;;
    deploy-local-service) with_host_lock deploy_local_service ;;
    deploy-local-full) with_host_lock deploy_local_full ;;
    deploy-hermes) with_host_lock deploy_release_hermes ;;
    rollback | rollback-service) with_host_lock rollback_service ;;
    rollback-full) with_host_lock rollback_full ;;
    *) usage ;;
esac
