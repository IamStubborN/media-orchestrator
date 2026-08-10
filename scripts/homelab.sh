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
remote_schema_file=$hermes_remote_root/shared/skills/media/MCP_SCHEMA.json
homelab_root=${HOMELAB_ROOT:-$root/../homelab}

usage() {
    echo "usage: $0 status|verify|deploy|deploy-service|deploy-full|deploy-hermes|rollback|rollback-service|rollback-full" >&2
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

checkpoint_images() {
    remote sh -s "$environment_file" "$rollback_file" "$remote_schema_file" <<'REMOTE'
set -eu
environment_file=$1
rollback_file=$2
schema_file=$3
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
python3 -c 'import json,sys; p=json.load(open(sys.argv[1])); assert p["schema_version"] == 1 and p["tools"]' "$schema_file"
umask 077
generation=$(mktemp -d "${rollback_file}.generation.XXXXXX")
trap 'rm -rf "$generation"' EXIT HUP INT TERM
service_revision=$(docker image inspect "$service_image" --format '{{index .Config.Labels "org.opencontainers.image.revision"}}')
runner_revision=$(docker image inspect "$runner_image" --format '{{index .Config.Labels "org.opencontainers.image.revision"}}')
schema_sha256=$(sha256sum "$schema_file" | awk '{print $1}')
schema_source_digest=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["source_digest"])' "$schema_file")
db_migration_version=$(docker exec media-postgres sh -lc 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc "select version from seaql_migrations order by version desc limit 1;"')
printf '%s\n' "$db_migration_version" | grep -Eq '^m[0-9]{8}_[0-9]{6}_[a-z0-9_]+$' || {
    echo "current database migration version is missing or invalid" >&2
    exit 1
}
printf 'MEDIA_SERVICE_IMAGE=%s\nDOWNLOAD_RUNNER_IMAGE=%s\nSERVICE_REVISION=%s\nRUNNER_REVISION=%s\nDB_MIGRATION_VERSION=%s\nMCP_SCHEMA_SHA256=%s\nMCP_SCHEMA_SOURCE_DIGEST=%s\n' "$service_image" "$runner_image" "$service_revision" "$runner_revision" "$db_migration_version" "$schema_sha256" "$schema_source_digest" >"$generation/images.env"
cp "$schema_file" "$generation/MCP_SCHEMA.json"
link=$(mktemp "${rollback_file}.link.XXXXXX")
rm "$link"
ln -s "$(basename "$generation")" "$link"
mv -Tf "$link" "$rollback_file"
trap - EXIT HUP INT TERM
REMOTE
}

protected_snapshot() {
    remote "for name in download-runner gluetun-rezka gluetun-rezka-watcher qbittorrent; do docker inspect \"\$name\" --format '{{.Name}}|{{.Id}}|{{.State.StartedAt}}|{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}'; done"
}

assert_protected_unchanged() {
    before=$1
    after=$(protected_snapshot)
    test "$before" = "$after" || { echo "a protected container changed during service deployment" >&2; exit 1; }
    if printf '%s\n' "$after" | grep -Ev '\|healthy$' >/dev/null; then
        echo "a protected container is not healthy" >&2
        exit 1
    fi
}

sync_hermes_schema() {
    source=$hermes_root/shared/skills/media/MCP_SCHEMA.json
    test -s "$source" || { echo "local Hermes MCP schema is missing: $source" >&2; exit 1; }
    scp "$source" "$host:$remote_schema_file.next" >/dev/null
    remote "install -m 0644 '$remote_schema_file.next' '$remote_schema_file'; rm '$remote_schema_file.next'"
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
    if ! remote "test -s '$remote_schema_file'"; then
        remote "mkdir -p '$(dirname "$remote_schema_file")'"
        remote "docker exec -i hermes-primary python3 - >'$remote_schema_file.next'" <<'PY'
import hashlib,json,urllib.request
token=open('/run/secrets/media_api_token').read().strip()
headers={'Authorization':'Bearer '+token,'Content-Type':'application/json','Accept':'application/json, text/event-stream'}
def call(payload, protocol=None):
    current=dict(headers)
    if protocol: current['MCP-Protocol-Version']=protocol
    request=urllib.request.Request('http://media-service:8080/internal/mcp',data=json.dumps(payload).encode(),headers=current,method='POST')
    with urllib.request.urlopen(request,timeout=15) as response: return json.loads(response.read())
call({'jsonrpc':'2.0','id':1,'method':'initialize','params':{'protocolVersion':'2025-03-26','capabilities':{},'clientInfo':{'name':'deploy-checkpoint','version':'1'}}})
tools=call({'jsonrpc':'2.0','id':2,'method':'tools/list'},'2025-03-26')['result']['tools']
canonical=json.dumps(sorted(tools,key=lambda tool:tool['name']),sort_keys=True,separators=(',',':'))
print(json.dumps({'schema_version':1,'source_digest':hashlib.sha256(canonical.encode()).hexdigest(),'tools':tools},sort_keys=True,separators=(',',':')))
PY
        remote "chmod 0644 '$remote_schema_file.next'; mv -f '$remote_schema_file.next' '$remote_schema_file'"
    fi
    verify_live_mcp_schema
}

remote() {
    # Arguments intentionally form the bounded command evaluated by the remote shell.
    # shellcheck disable=SC2029
    ssh "$host" "$@"
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

replace_images() {
    service_image=$1
    runner_image=$2
    remote "set -eu; sed -i 's#^MEDIA_SERVICE_IMAGE=.*#MEDIA_SERVICE_IMAGE=$service_image#; s#^DOWNLOAD_RUNNER_IMAGE=.*#DOWNLOAD_RUNNER_IMAGE=$runner_image#' '$environment_file'; cd '$remote_root/media'; docker compose --env-file '$environment_file' -f '$compose_file' run --rm --no-deps media-service migrate; docker compose --env-file '$environment_file' -f '$compose_file' up -d --no-deps --force-recreate media-service; docker stop gluetun-rezka-watcher >/dev/null; docker compose --env-file '$environment_file' -f '$compose_file' up -d --no-deps --force-recreate download-runner; docker start gluetun-rezka-watcher >/dev/null"
    verify
}

replace_service_image() {
    service_image=$1
    remote sh -s "$environment_file" "$remote_root" "$compose_file" "$service_image" <<'REMOTE'
set -eu
environment_file=$1
remote_root=$2
compose_file=$3
service_image=$4
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
    remote "set -eu; install -m 0755 '$hermes_remote_root/artifacts/media-$media_version-linux-amd64.next' '$hermes_remote_root/artifacts/media-$media_version-linux-amd64'; rm '$hermes_remote_root/artifacts/media-$media_version-linux-amd64.next'; sed -i '/^HERMES_HOME_IMAGE=/d; /^MEDIA_CLI_SHA256=/d' '$hermes_remote_root/.env'; cd '$hermes_remote_root'; attempts=0; until docker compose --env-file .env pull; do attempts=\$((attempts + 1)); test \"\$attempts\" -lt 5 || exit 1; sleep 5; done"
}

sync_homelab_compose() {
    source=$homelab_root/media/compose.media-orchestrator.yml
    scp "$source" "$host:$compose_file.next" >/dev/null
    remote "install -m 0644 '$compose_file.next' '$compose_file'; rm '$compose_file.next'"
}

replace_hermes_agents() {
    remote "set -eu; cd '$hermes_remote_root'; docker compose --env-file .env up -d --force-recreate agent-browser-updater vaultwarden-init-primary vaultwarden-broker-primary media-notifier-primary media-notifier-secondary hermes-primary hermes-secondary"
    remote sh -s <<'REMOTE'
set -eu
for name in agent-browser-updater vaultwarden-broker-primary media-notifier-primary media-notifier-secondary hermes-primary hermes-secondary; do
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

image_suffix() {
    revision=$(git -C "$root" rev-parse --short HEAD)
    worktree_fingerprint=$(
        {
            git -C "$root" diff --binary HEAD
            git -C "$root" ls-files --others --exclude-standard -z |
                sort -z |
                xargs -0 shasum -a 256 2>/dev/null || true
        } | shasum -a 256 | cut -c1-12
    )
    printf 'local-%s-%s\n' "$revision" "$worktree_fingerprint"
}

deploy_service() {
    check_hermes_capabilities
    assert_no_active_job
    suffix=$(image_suffix)
    service_image=media-orchestrator-service:$suffix
    docker_host=${MEDIA_DOCKER_HOST:-ssh://$host}
    (
        cd "$root"
        DOCKER_HOST=$docker_host MEDIA_BUILD_TARGETS=service MEDIA_SERVICE_IMAGE=$service_image ./scripts/docker-build.sh
    )
    remote "docker image inspect '$service_image' >/dev/null"
    sync_homelab_compose
    ensure_deployed_mcp_schema
    checkpoint_images
    protected_before=$(protected_snapshot)
    replace_service_image "$service_image"
    sync_hermes_schema
    replace_hermes_agents
    verify_live_mcp_schema
    assert_protected_unchanged "$protected_before"
}

deploy_full() {
    check_hermes_capabilities
    assert_no_active_job
    suffix=$(image_suffix)
    service_image=media-orchestrator-service:$suffix
    runner_image=media-orchestrator-runner:$suffix
    docker_host=${MEDIA_DOCKER_HOST:-ssh://$host}
    (
        cd "$root"
        DOCKER_HOST=$docker_host \
            MEDIA_SERVICE_IMAGE=$service_image \
            MEDIA_RUNNER_IMAGE=$runner_image \
            ./scripts/docker-build.sh
    )
    prepare_hermes_cli "$service_image" "$docker_host"
    sync_homelab_compose
    checkpoint_images
    replace_images "$service_image" "$runner_image"
    replace_hermes_agents
}

deploy_hermes() {
    check_hermes_capabilities
    assert_no_active_job
    service_image=$(remote "docker inspect media-service --format '{{.Config.Image}}'")
    docker_host=${MEDIA_DOCKER_HOST:-ssh://$host}
    prepare_hermes_cli "$service_image" "$docker_host"
    sync_homelab_compose
    replace_hermes_agents
    remote "set -eu; cd '$remote_root/media'; docker compose --env-file '$environment_file' -f '$compose_file' up -d --no-deps --force-recreate media-service"
    verify
}

read_rollback_images() {
    previous=$(remote "cat '$rollback_file/images.env'")
    service_image=$(printf '%s\n' "$previous" | sed -n 's/^MEDIA_SERVICE_IMAGE=//p')
    runner_image=$(printf '%s\n' "$previous" | sed -n 's/^DOWNLOAD_RUNNER_IMAGE=//p')
    rollback_migration_version=$(printf '%s\n' "$previous" | sed -n 's/^DB_MIGRATION_VERSION=//p')
    test -n "$service_image" && test -n "$runner_image" && test -n "$rollback_migration_version" || {
        echo "rollback image record is incomplete" >&2
        exit 1
    }
    validate_migration_version "$rollback_migration_version"
    remote "docker image inspect '$service_image' >/dev/null && docker image inspect '$runner_image' >/dev/null"
}

rollback_service() {
    check_hermes_capabilities
    assert_no_active_job
    read_rollback_images
    protected_before=$(protected_snapshot)
    forward_image=$(remote "sed -n 's/^MEDIA_SERVICE_IMAGE=//p' '$environment_file'")
    forward_migration_version=$(read_db_migration_version)
    forward_schema=$remote_root/media/.media-orchestrator-mcp-schema.forward.$$
    remote "cp '$remote_schema_file' '$forward_schema'"
    if ! (
        remote "set -eu; expected=\$(sed -n 's/^MCP_SCHEMA_SHA256=//p' '$rollback_file/images.env'); actual=\$(sha256sum '$rollback_file/MCP_SCHEMA.json' | awk '{print \$1}'); test \"\$expected\" = \"\$actual\"; cp '$rollback_file/MCP_SCHEMA.json' '$remote_schema_file.next'; chmod 0644 '$remote_schema_file.next'; mv -f '$remote_schema_file.next' '$remote_schema_file'"
        migrate_down_one_with_image "$forward_image" "$forward_migration_version" "$rollback_migration_version"
        replace_service_image "$service_image"
        assert_db_migration_version "$rollback_migration_version"
        replace_hermes_agents
        verify_live_mcp_schema
    ); then
        echo "rollback compatibility check failed; restoring the forward service and schema" >&2
        remote "cp '$forward_schema' '$remote_schema_file.next'; chmod 0644 '$remote_schema_file.next'; mv -f '$remote_schema_file.next' '$remote_schema_file'"
        replace_service_image "$forward_image"
        assert_db_migration_version "$forward_migration_version"
        replace_hermes_agents
        verify_live_mcp_schema
        remote "rm -f '$forward_schema'"
        assert_protected_unchanged "$protected_before"
        return 1
    fi
    remote "rm -f '$forward_schema'"
    assert_protected_unchanged "$protected_before"
}

rollback_full() {
    check_hermes_capabilities
    assert_no_active_job
    read_rollback_images
    replace_images "$service_image" "$runner_image"
}

case ${1:-} in
    status) status ;;
    verify) verify ;;
    deploy | deploy-service) deploy_service ;;
    deploy-full) deploy_full ;;
    deploy-hermes) deploy_hermes ;;
    rollback | rollback-service) rollback_service ;;
    rollback-full) rollback_full ;;
    *) usage ;;
esac
