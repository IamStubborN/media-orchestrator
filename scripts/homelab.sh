#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
host=${MEDIA_HOMELAB_HOST:host.example.invalid}
remote_root=${MEDIA_HOMELAB_ROOT:-/srv/homelab}
compose_file=$remote_root/media/compose.media-orchestrator.yml
environment_file=$remote_root/.env
rollback_file=$remote_root/media/.media-orchestrator-images.previous
hermes_root=${HERMES_HOME_ROOT:-${HOMELAB_ROOT:-}/hermes}
hermes_remote_root=${HERMES_HOME_REMOTE_ROOT:-/srv/homelab/hermes}
remote_schema_file=$hermes_remote_root/shared/skills/media/MCP_SCHEMA.json
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
    remote sh -s "$environment_file" "$rollback_file" "$remote_schema_file" "$compose_file" "$hermes_remote_root" <<'REMOTE'
set -eu
environment_file=$1
rollback_file=$2
schema_file=$3
compose_file=$4
hermes_root=$5
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
cp "$schema_file" "$generation/MCP_SCHEMA.json"
cp "$compose_file" "$generation/compose.media-orchestrator.yml"
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
    remote "for name in media-postgres gluetun gluetun-rezka qbittorrent; do docker inspect \"\$name\" --format '{{.Name}}|{{.Id}}|{{.State.StartedAt}}|{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}'; done"
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
    scp "$homelab_root/media/compose.media-orchestrator.yml" "$host:$candidate_compose" >/dev/null
    remote sh -s "$environment_file" "$compose_file" "$candidate_compose" <<'REMOTE'
set -eu
environment_file=$1
live_compose=$2
candidate_compose=$3
live_json=$(mktemp)
candidate_json=$(mktemp)
trap 'rm -f "$candidate_compose" "$live_json" "$candidate_json"' EXIT HUP INT TERM
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
    remote "install -m 0644 '$remote_schema_file.next' '$remote_schema_file'; rm '$remote_schema_file.next'"
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
expected=$3
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
expected=$3
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

ensure_deployed_mcp_schema() {
    remote "test -s '$remote_schema_file'" || {
        echo "refusing deployment without an exact Hermes MCP schema artifact: $remote_schema_file" >&2
        exit 1
    }
    verify_live_mcp_schema
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
    remote sh -s <<'REMOTE'
set -eu
watcher_state=$(docker inspect gluetun-rezka-watcher --format '{{.State.Status}}')
runner_state=$(docker inspect download-runner --format '{{.State.Status}}')
restore_runtime() {
    test "$runner_state" != running || docker start download-runner >/dev/null
    test "$watcher_state" != running || docker start gluetun-rezka-watcher >/dev/null
}
trap restore_runtime EXIT HUP INT TERM
test "$watcher_state" = running || { echo "runner watcher is not running before quiescence" >&2; exit 1; }
docker stop gluetun-rezka-watcher >/dev/null
lifecycle=$(docker exec media-postgres sh -lc 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc "select state from runner_lifecycle;"')
test "$lifecycle" = ready || { echo "runner lifecycle is not ready for quiescence: $lifecycle" >&2; exit 1; }
active=$(docker exec media-postgres sh -lc 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc "select count(*) from jobs where state in ('"'"'leased'"'"','"'"'running'"'"','"'"'cancel_requested'"'"','"'"'publishing'"'"','"'"'plex_pending'"'"');"')
test "$active" = 0 || { echo "a job became active while quiescing the runner" >&2; exit 1; }
test "$runner_state" != running || docker stop download-runner >/dev/null
lifecycle=$(docker exec media-postgres sh -lc 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc "select state from runner_lifecycle;"')
test "$lifecycle" = ready || { echo "runner lifecycle changed during quiescence: $lifecycle" >&2; exit 1; }
active=$(docker exec media-postgres sh -lc 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc "select count(*) from jobs where state in ('"'"'leased'"'"','"'"'running'"'"','"'"'cancel_requested'"'"','"'"'publishing'"'"','"'"'plex_pending'"'"');"')
test "$active" = 0 || { echo "a job became active before runner replacement" >&2; exit 1; }
trap - EXIT HUP INT TERM
REMOTE
}

resume_runner_watcher_and_wait_ready() {
    remote sh -s <<'REMOTE'
set -eu
hold_quiescence() {
    docker stop gluetun-rezka-watcher >/dev/null 2>&1 || true
    docker stop download-runner >/dev/null 2>&1 || true
}
trap hold_quiescence EXIT HUP INT TERM
docker start download-runner >/dev/null
docker start gluetun-rezka-watcher >/dev/null
attempts=0
while test "$attempts" -lt 30; do
    watcher_health=$(docker inspect gluetun-rezka-watcher --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}')
    lifecycle=$(docker exec media-postgres sh -lc 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc "select state from runner_lifecycle;"')
    if test "$watcher_health" = healthy && test "$lifecycle" = ready; then
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
    remote "docker stop gluetun-rezka-watcher download-runner >/dev/null 2>&1 || true"
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
    remote "set -eu; test \"\$(docker inspect gluetun-rezka-watcher --format '{{.State.Status}}')\" != running; test \"\$(docker inspect download-runner --format '{{.State.Status}}')\" != running; sed -i 's#^MEDIA_SERVICE_IMAGE=.*#MEDIA_SERVICE_IMAGE=$service_image#; s#^DOWNLOAD_RUNNER_IMAGE=.*#DOWNLOAD_RUNNER_IMAGE=$runner_image#' '$environment_file'; cd '$remote_root/media'; docker compose --env-file '$environment_file' -f '$compose_file' run --rm --no-deps media-service migrate; if test -n '$expected_migration_version'; then actual=\$(docker exec media-postgres sh -lc 'psql -U \"\$POSTGRES_USER\" -d \"\$POSTGRES_DB\" -Atc \"select version from seaql_migrations order by version desc limit 1;\"'); test \"\$actual\" = '$expected_migration_version' || { echo \"database migration differs from release manifest\" >&2; exit 1; }; fi; docker compose --env-file '$environment_file' -f '$compose_file' up -d --no-deps --force-recreate media-service; docker compose --env-file '$environment_file' -f '$compose_file' create --force-recreate download-runner"
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
cd "$remote_root/media"
docker compose --env-file "$next" -f "$compose_file" run --rm --no-deps media-service migrate
if test -n "$expected_migration_version"; then
    actual=$(docker exec media-postgres sh -lc 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc "select version from seaql_migrations order by version desc limit 1;"')
    test "$actual" = "$expected_migration_version" || { echo "database migration differs from release manifest" >&2; exit 1; }
fi
mv -f "$next" "$environment_file"
trap - EXIT HUP INT TERM
docker compose --env-file "$environment_file" -f "$compose_file" up -d --no-deps --force-recreate media-service
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
cd "$remote_root/media"
docker compose --env-file "$rollback_env" -f "$compose_file" run --rm --no-deps media-service \
    migrate-down-one --expected-current "$expected_current" --expected-target "$expected_target"
REMOTE
    assert_db_migration_version "$expected_target"
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
    rsync -az --delete \
        --exclude .git \
        --exclude .worktrees/ \
        --exclude .env \
        --exclude artifacts/ \
        --exclude secrets/ \
        "$hermes_root/" "$host:$hermes_remote_root/"
    scp "$artifact" "$host:$hermes_remote_root/artifacts/media-$media_version-linux-amd64.next" >/dev/null
    remote "set -eu; install -m 0755 '$hermes_remote_root/artifacts/media-$media_version-linux-amd64.next' '$hermes_remote_root/artifacts/media-$media_version-linux-amd64'; rm '$hermes_remote_root/artifacts/media-$media_version-linux-amd64.next'; sed -i '/^HERMES_HOME_IMAGE=/d; /^MEDIA_CLI_SHA256=/d' '$hermes_remote_root/.env'; cd '$hermes_remote_root'; attempts=0; until docker compose --env-file .env pull media-notifier-primary media-notifier-secondary hermes-primary hermes-secondary; do attempts=\$((attempts + 1)); test \"\$attempts\" -lt 5 || exit 1; sleep 5; done"
}

stage_hermes_cli() {
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
    remote "set -eu; test \"\$(sha256sum '$hermes_stage/artifacts/media-$media_version-linux-amd64' | awk '{print \$1}')\" = '$artifact_sha256'; cd '$hermes_stage/source'; attempts=0; until docker compose --env-file '$hermes_remote_root/.env' -f compose.yaml pull media-notifier-primary media-notifier-secondary hermes-primary hermes-secondary; do attempts=\$((attempts + 1)); test \"\$attempts\" -lt 5 || exit 1; sleep 5; done"
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
    scp "$source" "$host:$compose_file.next" >/dev/null
    remote "install -m 0644 '$compose_file.next' '$compose_file'; rm '$compose_file.next'"
}

replace_hermes_agents() {
    image_record=${1:-}
    remote sh -s "$hermes_remote_root" "$image_record" <<'REMOTE'
set -eu
hermes_root=$1
image_record=${2:-}
cd "$hermes_root"
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
    docker compose --env-file .env -f compose.yaml -f "$override" up -d --no-deps --force-recreate media-notifier-primary media-notifier-secondary hermes-primary hermes-secondary
else
    docker compose --env-file .env up -d --no-deps --force-recreate media-notifier-primary media-notifier-secondary hermes-primary hermes-secondary
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
    ensure_deployed_mcp_schema
    protected_before=$(protected_snapshot)
    assert_no_active_job
    runner_container_id=$(remote "docker inspect download-runner --format '{{.Id}}'")
    runner_image_id=$(running_image_id download-runner)
    assert_deploy_migration_baseline "$expected_migration_version"
    checkpoint_images
    quiesce_runner
    if ! (
        perform_service_deploy || exit 1
        resume_runner_watcher_and_wait_ready || exit 1
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
        resume_runner_watcher_and_wait_ready || recovery_failed=1
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
    ensure_deployed_mcp_schema
    stage_hermes_cli "$service_image" "$docker_host"
    protected_before=$(full_protected_snapshot)
    assert_no_active_job
    previous_runner_id=$(remote "docker inspect download-runner --format '{{.Id}}'")
    assert_deploy_migration_baseline "$expected_migration_version"
    checkpoint_images
    quiesce_runner
    if ! (
        activate_hermes_stage || exit 1
        sync_homelab_compose || exit 1
        replace_images "$service_image" "$runner_image" "$expected_migration_version" || exit 1
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
        verify_resumed_runtime_or_requiesce "$protected_before" assert_full_protected_unchanged || exit 1
    ); then
        echo "full deployment failed; restoring its exact checkpoint" >&2
        forward_service_image=$service_image
        read_rollback_images
        rollback_service_image=$service_image
        rollback_runner_image=$runner_image
        recovery_failed=0
        restore_checkpoint_deployment_sources || recovery_failed=1
        remote "cp '$rollback_file/MCP_SCHEMA.json' '$remote_schema_file.next'; chmod 0644 '$remote_schema_file.next'; mv -f '$remote_schema_file.next' '$remote_schema_file'" || recovery_failed=1
        current_migration_version=$(read_db_migration_version) || recovery_failed=1
        if test "$recovery_failed" = 0 && test "$current_migration_version" != "$rollback_migration_version"; then
            migrate_down_one_with_image "$forward_service_image" "$current_migration_version" "$rollback_migration_version" || recovery_failed=1
        fi
        recovery_runner_id=$(remote "docker inspect download-runner --format '{{.Id}}'") || recovery_failed=1
        replace_images "$rollback_service_image" "$rollback_runner_image" || recovery_failed=1
        replace_hermes_agents "$rollback_file/hermes-images.env" || recovery_failed=1
        verify_live_mcp_schema || recovery_failed=1
        verify_mounted_hermes_sources remote || recovery_failed=1
        resume_runner_watcher_and_wait_ready || recovery_failed=1
        if test "$recovery_failed" = 0 && ! verify_runner_service_compatibility "$recovery_runner_id" "$rollback_service_image" "$rollback_runner_image"; then
            hold_runner_quiescence
            recovery_failed=1
        fi
        test "$recovery_failed" != 0 || verify_resumed_runtime_or_requiesce "$protected_before" assert_full_protected_unchanged || recovery_failed=1
        cleanup_hermes_stage || recovery_failed=1
        test "$recovery_failed" = 0 || echo "full deployment recovery also failed" >&2
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
    remote sh -s "$destination" "$remote_schema_file" "$compose_file" "$hermes_remote_root" <<'REMOTE'
set -eu
destination=$1
schema_file=$2
compose_file=$3
hermes_root=$4
mkdir "$destination"
trap 'rm -rf "$destination"' EXIT HUP INT TERM
cp "$schema_file" "$destination/MCP_SCHEMA.json"
cp "$compose_file" "$destination/compose.media-orchestrator.yml"
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
    remote sh -s "$rollback_file" "$compose_file" "$hermes_remote_root" <<'REMOTE'
set -eu
rollback_file=$1
compose_file=$2
hermes_root=$3
test -s "$rollback_file/compose.media-orchestrator.yml"
test -d "$rollback_file/hermes-source"
test -s "$rollback_file/hermes-images.env"
install -m 0644 "$rollback_file/compose.media-orchestrator.yml" "$compose_file.next"
mv -f "$compose_file.next" "$compose_file"
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
    remote sh -s "$forward_sources" "$compose_file" "$hermes_remote_root" <<'REMOTE'
set -eu
forward_sources=$1
compose_file=$2
hermes_root=$3
test -s "$forward_sources/compose.media-orchestrator.yml"
test -d "$forward_sources/hermes-source"
test -s "$forward_sources/hermes-images.env"
install -m 0644 "$forward_sources/compose.media-orchestrator.yml" "$compose_file.next"
mv -f "$compose_file.next" "$compose_file"
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
    quiesce_runner
    if ! (
        perform_service_rollback || exit 1
        resume_runner_watcher_and_wait_ready || exit 1
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
        resume_runner_watcher_and_wait_ready || recovery_failed=1
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
    replace_images "$service_image" "$runner_image" || return 1
    replace_hermes_agents "$rollback_file/hermes-images.env" || return 1
    verify_live_mcp_schema || return 1
    verify_mounted_hermes_sources remote || return 1
    resume_runner_watcher_and_wait_ready || return 1
    if ! verify_runner_service_compatibility "$previous_runner_id" "$service_image" "$runner_image"; then
        hold_runner_quiescence
        return 1
    fi
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
    quiesce_runner
    if ! perform_full_rollback; then
        echo "full rollback failed; restoring the forward full stack" >&2
        recovery_failed=0
        restore_forward_deployment_sources || recovery_failed=1
        remote "cp '$forward_schema' '$remote_schema_file.next'; chmod 0644 '$remote_schema_file.next'; mv -f '$remote_schema_file.next' '$remote_schema_file'" || recovery_failed=1
        recovery_runner_id=$(remote "docker inspect download-runner --format '{{.Id}}'") || recovery_failed=1
        replace_images "$forward_service_image" "$forward_runner_image" || recovery_failed=1
        assert_db_migration_version "$forward_migration_version" || recovery_failed=1
        replace_hermes_agents "$forward_sources/hermes-images.env" || recovery_failed=1
        verify_live_mcp_schema || recovery_failed=1
        verify_mounted_hermes_sources remote || recovery_failed=1
        resume_runner_watcher_and_wait_ready || recovery_failed=1
        if test "$recovery_failed" = 0 && ! verify_runner_service_compatibility "$recovery_runner_id" "$forward_service_image" "$forward_runner_image"; then
            hold_runner_quiescence
            recovery_failed=1
        fi
        test "$recovery_failed" != 0 || verify_resumed_runtime_or_requiesce "$protected_before" assert_full_protected_unchanged || recovery_failed=1
        cleanup_forward_deployment_sources "$forward_sources" || recovery_failed=1
        test "$recovery_failed" = 0 || echo "forward full-stack recovery also failed" >&2
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
