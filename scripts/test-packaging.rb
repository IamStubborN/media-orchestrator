#!/usr/bin/env ruby
# frozen_string_literal: true

require "yaml"
require "open3"

ROOT = File.expand_path("..", __dir__)

def read(path)
  File.read(File.join(ROOT, path))
rescue Errno::ENOENT
  abort("missing packaging file: #{path}")
end

def assert(condition, message)
  abort(message) unless condition
end

dockerfile = read("Dockerfile")
compose_text = read("compose.yaml")
workflow = read(".github/workflows/release.yml")
tasks = read(".mise.toml")
smoke = read("scripts/docker-smoke.sh")
homelab = read("scripts/homelab.sh")
docker_build = read("scripts/docker-build.sh")
dockerignore = read(".dockerignore")
gitignore = read(".gitignore")
runbook = read("docs/RUNBOOK.md")

compose = YAML.safe_load(compose_text, aliases: true)
services = compose.fetch("services")

stages = []
dockerfile.scan(/^FROM\s+(\S+)(?:\s+AS\s+(\S+))?/i).each do |base, stage|
  internal = base == "scratch" || stages.include?(base)
  assert(internal || base.include?("${") || base.include?("@sha256:"),
         "external Dockerfile base image #{base} must be configurable or digest-pinned")
  stages << stage if stage
end
assert(dockerfile.include?("cargo chef cook") && dockerfile.include?("--locked"),
       "Dockerfile must use cargo-chef and locked Cargo builds")
assert(dockerfile.include?("media-target-${TARGETARCH}"),
       "Cargo target caches must be isolated by target architecture")
assert(dockerfile.include?("AS service") && dockerfile.include?("AS runner") && dockerfile.include?("AS cli-artifact"),
       "Dockerfile must expose service, runner, and cli-artifact targets")
assert(dockerfile.include?("USER 65532:65532"), "runtime targets must use the non-root media user")
assert(dockerfile.include?("HEALTHCHECK") && dockerfile.include?("media\", \"healthcheck"),
       "service image must use the media binary for health checks")

service_section = dockerfile.split(/^FROM .* AS service$/, 2).fetch(1).split(/^FROM .* AS runner-packages$/, 2).fetch(0)
assert(!service_section.match?(/ffmpeg|ffprobe|libva/i), "service target must not install runner media tooling")
assert(dockerfile.match?(/ffmpeg=.*ffprobe|ffmpeg=.*libva|ffmpeg/i), "runner target must install ffmpeg")
assert(dockerfile.match?(/amd64\).*intel-media-va-driver/) &&
       dockerfile.match?(/arm64\).*vaapi_driver=""/),
       "Intel VAAPI driver must be installed only for the amd64 runner")
%w[
  MEDIA_POSTGRES_PASSWORD
  MEDIA_DATABASE_URL
  MEDIA_PRIMARY_TOKEN
  MEDIA_SECONDARY_TOKEN
  MEDIA_RUNNER_TOKEN
  MEDIA_LIFECYCLE_TOKEN
].each do |name|
  assert(smoke.include?("#{name}=${#{name}:-"), "docker smoke must provide a disposable #{name}")
end

%w[postgres migrate service runner].each do |name|
  assert(services.key?(name), "compose stack is missing #{name}")
end

service = services.fetch("service")
runner = services.fetch("runner")
assert(service.fetch("environment").key?("MEDIA_LIFECYCLE_TOKEN"),
       "service must receive the lifecycle credential")
assert(service.fetch("networks").sort == ["backend"], "service must only join the backend network")
assert(!service.key?("network_mode"), "service must not use a VPN network namespace")
assert(runner.fetch("secrets", []).none? { |secret| secret.to_s.include?("database") },
       "runner must not receive the database secret")
assert(runner.fetch("devices").any? { |device| device.to_s.include?("/dev/dri") },
       "runner must receive /dev/dri")

%w[migrate service runner].each do |name|
  runtime = services.fetch(name)
  assert(runtime["read_only"] == true, "#{name} must use a read-only root filesystem")
  assert(runtime.dig("security_opt")&.include?("no-new-privileges:true"),
         "#{name} must set no-new-privileges")
  assert(runtime["cap_drop"]&.include?("ALL"), "#{name} must drop Linux capabilities")
end

all_packaging = [dockerfile, compose_text, workflow].join("\n")
assert(!all_packaging.match?(/:\s*latest(?:\s|$)/), "packaging must not use latest tags")
assert(!compose_text.include?("/var/run/docker.sock"), "compose stack must not mount the Docker socket")
assert(workflow.include?("provenance: mode=max") && workflow.include?("sbom: true"),
       "release workflow must publish provenance and SBOM attestations")
assert(workflow.include?("sha-${{ github.sha }}"), "release workflow must publish immutable SHA tags")
assert(!workflow.match?(/(?:^|\s)latest(?:\s|$)/), "release workflow must never publish latest")
assert(workflow.include?("--print-source-tree-digest") &&
       workflow.include?("--print-runner-build-digest") &&
       workflow.include?("OCI_SOURCE_TREE_DIGEST=${{") &&
       workflow.include?("OCI_RUNNER_BUILD_DIGEST=${{"),
       "release builds must stamp the same source and runner digest OCI args as local builds")

%w[docker-build docker-smoke docker-lint extract-linux-cli].each do |task|
  assert(tasks.include?("[tasks.#{task}]"), "mise task #{task} is missing")
end

assert(docker_build.include?("MEDIA_BUILD_TARGETS") && docker_build.include?("service)"),
       "docker build must support a service-only target")
assert(docker_build.include?("MEDIA_SOURCE_TREE_DIGEST") &&
       docker_build.include?('OCI_SOURCE_TREE_DIGEST=$source_tree_digest'),
       "docker builds must stamp the exact source-tree digest")
assert(docker_build.include?("--print-source-tree-digest") &&
       docker_build.include?(".dockerignore") &&
       docker_build.include?("os.walk") &&
       docker_build.include?("is_symlink") &&
       docker_build.include?("unsupported .dockerignore pattern") &&
       !docker_build.include?("any(fnmatch.fnmatchcase(part, pattern) for part in parts)"),
       "source digest must hash the exact Docker context including ignore rules and filesystem types")
assert(docker_build.include?("--print-source-version") &&
       docker_build.include?("--untracked-files=all") &&
       docker_build.include?("-dirty"),
       "OCI version must visibly identify tracked and untracked dirty source")
assert(docker_build.include?("MEDIA_RUNNER_BUILD_DIGEST") &&
       docker_build.include?('OCI_RUNNER_BUILD_DIGEST=$runner_build_digest'),
       "docker builds must stamp the deterministic runner build-impact digest")
assert(dockerfile.scan("dev.iamstubborn.media.source-tree-digest").length == 2,
       "service and runner images must expose the source-tree digest OCI extension label")
assert(dockerfile.scan("dev.iamstubborn.media.runner-build-digest").length == 2,
       "service and runner images must expose the runner build-impact digest")
assert(tasks.include?('./scripts/homelab.sh deploy-service'),
       "the default homelab deploy task must use the service-only path")
assert(tasks.include?("[tasks.homelab-deploy-full]"),
       "full-stack deployment must remain an explicit task")
assert(homelab.include?(': "${HOMELAB_ROOT:?HOMELAB_ROOT is required}"') &&
       homelab.include?(': "${MEDIA_RELEASE_DIR:?MEDIA_RELEASE_DIR is required}"') &&
       homelab.include?('hermes_root=${HERMES_HOME_ROOT:-$HOMELAB_ROOT/hermes}') &&
       !homelab.include?("../homelab"),
       "guarded deployment must require explicit Homelab and release paths without sibling discovery")
assert(homelab.include?('source=$MEDIA_RELEASE_DIR/MCP_SCHEMA.json'),
       "guarded deployment must source the MCP schema from the explicit release bundle")

service_deploy = homelab.split("deploy_service() {", 2).fetch(1).split("deploy_full() {", 2).fetch(0)
service_deploy_attempt = homelab.split("perform_service_deploy() {", 2).fetch(1).split("deploy_service() {", 2).fetch(0)
service_deploy_contract = service_deploy + service_deploy_attempt
assert(service_deploy.index("check_hermes_capabilities") < service_deploy.index("docker-build.sh"),
       "Hermes capability preflight must run before the service build")
assert(service_deploy.index("checkpoint_images") < service_deploy.index("replace_service_image"),
       "current images must be checkpointed before service replacement")
assert(service_deploy.index("checkpoint_images") < service_deploy.index("perform_service_deploy") &&
       service_deploy_attempt.index("sync_homelab_compose") < service_deploy_attempt.index("replace_service_image"),
       "service deployment must checkpoint the deployed Compose before synchronizing its replacement")
assert(service_deploy.scan("assert_no_active_job").length >= 2 &&
       service_deploy.include?("quiesce_runner") &&
       service_deploy.index("quiesce_runner") < service_deploy.index("perform_service_deploy") &&
       service_deploy.scan("resume_runner_watcher_and_wait_ready").length >= 2 &&
       service_deploy.include?("verify_runner_service_compatibility") &&
       service_deploy.include?("same"),
       "service deployment must hold a final idle quiescence through mutation and resume the same runner generation")
assert(service_deploy.include?("perform_service_deploy") &&
       service_deploy.include?("restore_checkpoint_deployment_sources") &&
       service_deploy.include?("restoring its exact checkpoint") &&
       service_deploy.include?('replace_hermes_agents "$rollback_file/hermes-images.env"'),
       "service deployment must automatically restore checkpointed Compose, service, schema, and Hermes refs")
assert(service_deploy_attempt.include?("sync_homelab_compose"),
       "service deployment must synchronize the reviewed homelab compose file")
assert(!service_deploy.match?(/replace_images|force-recreate download-runner/),
       "service-only deployment may quiesce but must not recreate runner or VPN services")
assert(service_deploy.index("assert_no_active_job") < service_deploy.index("docker-build.sh"),
       "service deployment must fail closed while a job is active")
assert(service_deploy.index("assert_service_only_rollout") < service_deploy.index("docker-build.sh"),
       "service deployment must reject runner-affecting source and compose changes before building")
assert(service_deploy.include?("protected_snapshot") && service_deploy.include?("verify_resumed_runtime_or_requiesce"),
       "service deployment must prove protected containers were unchanged")
assert(service_deploy.index('verify_image_attestation "$service_image"') &&
       service_deploy.index('verify_image_attestation "$service_image"') < service_deploy.index("checkpoint_images") &&
       service_deploy.include?('service_image=$(immutable_image_id "$service_image")') &&
       service_deploy_contract.include?("verify_running_service_attestation") &&
       service_deploy_contract.include?("verify_mounted_hermes_sources"),
       "service deployment must attest the built and running service plus mounted Hermes sources")
assert(service_deploy_contract.scan("verify_mounted_hermes_sources").length >= 2 &&
       service_deploy.index("verify_mounted_hermes_sources") < service_deploy.index("docker-build.sh"),
       "service deployment must reject mounted Hermes source drift before building")
mounted_sources = homelab.split("verify_mounted_hermes_sources() {", 2).fetch(1).split("verify_live_mcp_schema() {", 2).fetch(0)
assert(!mounted_sources.include?("MCP_SCHEMA.json") && service_deploy_contract.include?("sync_hermes_schema"),
       "joint service/schema rollout must not create a cyclic mounted-schema preflight")
assert(mounted_sources.include?("scripts/media-notifier") &&
       mounted_sources.include?("scripts/hermes_media_notifications.py") &&
       mounted_sources.include?("shared/plugins/telegram-home/assets/media-menu.jpg") &&
       mounted_sources.include?("media-notifier-primary") &&
       mounted_sources.include?("media-notifier-secondary"),
       "mounted source verification must attest every notifier bind mount in both live containers")

service_only_guard = homelab.split("assert_service_only_rollout() {", 2).fetch(1).split("sync_hermes_schema() {", 2).fetch(0)
runner_digest_contract = docker_build.split("runner_build_digest() {", 2).fetch(1).split("case ${1:-}", 2).fetch(0)
assert(runner_digest_contract.include?("docker_context_digest") &&
       !runner_digest_contract.include?("git ls-files"),
       "runner digest must derive from the exact Docker context instead of a partial path list")
assert(dockerfile.include?("COPY . .") &&
       %w[Cargo.toml Cargo.lock .cargo config crates].none? { |path| dockerignore.lines.map(&:strip).include?(path) },
       "exact runner context must include Cargo, toolchain/config, and every runner/service source input")
assert(service_only_guard.include?("dev.iamstubborn.media.runner-build-digest") &&
       service_only_guard.include?("runner_build_digest"),
       "service-only guard must compare the exact local and deployed runner digests")
assert(!service_only_guard.include?("git diff --name-only") &&
       !service_only_guard.include?("git ls-files --others"),
       "digest-equal dirty runner baselines must not be rejected by revision-relative path diffs")
assert(service_only_guard.include?("docker compose") &&
       service_only_guard.include?("--profile '*'") &&
       service_only_guard.include?("config --format json") &&
       service_only_guard.include?("download-runner") &&
       service_only_guard.include?("gluetun-rezka") &&
       service_only_guard.include?('"networks"') &&
       service_only_guard.include?('"volumes"') &&
       !service_only_guard.include?("YAML.safe_load") &&
       service_only_guard.include?("deploy-full"),
       "service-only guard must safely compare the full effective runner boundary and name the full rollout command")
assert(homelab.include?("verify_live_mcp_schema") && homelab.include?("MCP_SCHEMA_SHA256"),
       "deployment rollback must preserve and verify the exact MCP schema")
schema_bootstrap = homelab.split("ensure_deployed_mcp_schema() {", 2).fetch(1).split("remote() {", 2).fetch(0)
assert(!schema_bootstrap.include?("hashlib") &&
       !schema_bootstrap.include?("source_digest") &&
       schema_bootstrap.include?("refusing deployment without an exact Hermes MCP schema artifact"),
       "missing deployed schema must fail closed without inventing source metadata")
assert(homelab.include?("DB_MIGRATION_VERSION") &&
       homelab.include?("migrate-down-one --expected-current") &&
       homelab.include?("assert_db_migration_version"),
       "service rollback must preserve and verify the exact database migration version")
migration_down_helper = homelab.split("migrate_down_one_with_image() {", 2).fetch(1).split("prepare_hermes_cli() {", 2).fetch(0)
assert(migration_down_helper.include?('migration_image=$1') &&
       !migration_down_helper.include?('service_image=$1'),
       "migration rollback helper must not overwrite the checkpointed old service image")
assert(homelab.include?('docker image inspect "$service_image"') &&
       homelab.include?('docker image inspect "$runner_image"') &&
       homelab.include?('mktemp -d "${rollback_file}.generation.XXXXXX"') &&
       homelab.include?('mv -Tf "$link" "$rollback_file"'),
       "rollback checkpoint must validate both images and publish atomically")

service_rollback = homelab.split("rollback_service() {", 2).fetch(1).split("rollback_full() {", 2).fetch(0)
service_attempt = homelab.split("perform_service_rollback() {", 2).fetch(1).split("rollback_service() {", 2).fetch(0)
down_index = service_attempt.index("migrate_down_one_with_image")
old_image_index = service_attempt.index('replace_service_image "$service_image"')
old_schema_index = service_attempt.index('verify_live_mcp_schema')
assert(down_index && old_image_index && old_schema_index &&
       down_index < old_image_index && old_image_index < old_schema_index,
       "service rollback must migrate down with the forward image before starting and verifying the old image")
recovery_index = service_rollback.index('replace_service_image "$forward_image"')
recovery_migration_index = service_rollback.index('assert_db_migration_version "$forward_migration_version"')
assert(recovery_index && recovery_migration_index && recovery_index < recovery_migration_index,
       "failed rollback must migrate up with the forward image and verify the forward migration version")
attempt_index = service_rollback.index("perform_service_rollback")
assert(service_rollback.index("assert_no_active_job") < attempt_index &&
       service_rollback.index("protected_snapshot") < attempt_index &&
       service_rollback.rindex("verify_resumed_runtime_or_requiesce") > recovery_migration_index,
       "migration rollback and recovery must remain inside the idle and protected-container guards")
assert(service_rollback.include?('forward_image=$(running_image_id media-service)'),
       "service rollback forward recovery must preserve the immutable running image ID")
assert(service_rollback.scan("assert_no_active_job").length >= 2 &&
       service_rollback.include?("quiesce_runner") &&
       service_rollback.scan("resume_runner_watcher_and_wait_ready").length >= 2 &&
       service_rollback.scan("verify_resumed_runtime_or_requiesce").length >= 2,
       "service rollback must hold final idle quiescence through rollback and guarded recovery")
assert(service_attempt.include?("restore_checkpoint_deployment_sources") &&
       service_attempt.include?('replace_hermes_agents "$rollback_file/hermes-images.env"') &&
       service_rollback.include?("checkpoint_forward_deployment_sources") &&
       service_rollback.include?("restore_forward_deployment_sources") &&
       service_rollback.include?('replace_hermes_agents "$forward_sources/hermes-images.env"'),
       "service rollback must restore exact checkpoint and forward Compose/Hermes pairs")
assert(service_attempt.include?('if test "$forward_migration_version" != "$rollback_migration_version"') &&
       service_attempt.include?('assert_db_migration_version "$rollback_migration_version"'),
       "service rollback must skip migrate-down for equal versions and still verify the checkpointed version")
assert(service_rollback.include?("perform_service_rollback") &&
       service_attempt.scan("|| return 1").length >= 5,
       "service rollback must explicitly propagate every failed attempt step")

prepare_hermes = homelab.split("prepare_hermes_cli() {", 2).fetch(1).split("sync_homelab_compose() {", 2).fetch(0)
hermes_services = "media-notifier-primary media-notifier-secondary hermes-primary hermes-secondary"
assert(prepare_hermes.include?("docker compose --env-file .env pull #{hermes_services}"),
       "Hermes deployment must only pull the intended Hermes and notifier images")

host_lock = homelab.split("acquire_host_lock() {", 2).fetch(1).split("release_host_lock() {", 2).fetch(0)
assert(host_lock.include?("flock -n") && host_lock.include?("media-orchestrator.deploy.lock"),
       "deploy and rollback commands must hold one host-wide flock")
mutating_dispatch = homelab.split("case ${1:-} in", 2).fetch(1)
assert(mutating_dispatch.include?("with_host_lock deploy_service") &&
       mutating_dispatch.include?("with_host_lock deploy_full") &&
       mutating_dispatch.include?("with_host_lock deploy_hermes") &&
       mutating_dispatch.include?("with_host_lock rollback_service") &&
       mutating_dispatch.include?("with_host_lock rollback_full"),
       "every deploy and rollback entry point must use the host-wide flock")

lock_probe = <<~'SH'
  set -eu
  marker=$(mktemp)
  rm -f "$marker"
  export marker
  if sh -c '
      set -eu
      acquire_host_lock() { :; }
      release_host_lock() { :; }
      with_host_lock() {
          acquire_host_lock
          set +e
          (set -e; "$@")
          result=$?
          set -e
          release_host_lock
          return "$result"
      }
      failing_gate() {
          false
          printf marker >"$marker"
      }
      with_host_lock failing_gate
  '; then
      exit 1
  fi
  test ! -e "$marker"
  rm -f "$marker"
SH
_, _, lock_status = Open3.capture3("sh", "-c", lock_probe)
assert(lock_status.success?, "host lock must preserve errexit and stop after a failing gate")

replace_hermes = homelab.split("replace_hermes_agents() {", 2).fetch(1).split("verify_runner_service_compatibility() {", 2).fetch(0)
assert(replace_hermes.include?("up -d --no-deps --force-recreate #{hermes_services}"),
       "Hermes replacement must only recreate the intended Hermes and notifier containers")
assert(replace_hermes.include?("image_record=${2:-}"),
       "Hermes replacement must accept an empty forward image record and a non-empty rollback record")
image_record_probe = <<~'SH'
  set -eu
  remote_replace() {
      image_record=${2:-}
      if test -n "$image_record"; then
          test "$image_record" = rollback.env
      else
          test -z "$image_record"
      fi
  }
  remote_replace /hermes
  remote_replace /hermes rollback.env
SH
_, _, image_record_status = Open3.capture3("sh", "-c", image_record_probe)
assert(image_record_status.success?, "Hermes replacement image record forwarding must cover empty and rollback values")
%w[agent-browser-updater vaultwarden-init-primary vaultwarden-broker-primary media-service].each do |service|
  assert(!replace_hermes.include?(service), "Hermes replacement must not touch #{service}")
end

hermes_deploy = homelab.split("deploy_hermes() {", 2).fetch(1).split("read_rollback_images() {", 2).fetch(0)
assert(!hermes_deploy.include?("sync_homelab_compose") &&
       !hermes_deploy.include?("force-recreate media-service"),
       "Hermes deployment must not sync or recreate media-service")
first_live_check = hermes_deploy.index("verify_local_mcp_schema")
prepare_index = hermes_deploy.index("stage_hermes_cli")
replace_index = hermes_deploy.index("replace_hermes_agents")
health_index = hermes_deploy.index("verify || exit 1")
last_live_check = hermes_deploy.rindex("verify_local_mcp_schema")
assert(first_live_check && prepare_index && replace_index && health_index && last_live_check &&
       first_live_check < prepare_index && replace_index < health_index && health_index < last_live_check,
       "Hermes deployment must compare local schema with live tools before replacement and after health")
assert(hermes_deploy.scan("verify_local_backend_attestation").length >= 2,
       "Hermes deployment must attest backend source and version before replacement and after health")
assert(hermes_deploy.index("stage_hermes_cli") < hermes_deploy.index("checkpoint_images") &&
       hermes_deploy.index("checkpoint_images") < hermes_deploy.index("activate_hermes_stage") &&
       hermes_deploy.include?("restoring its exact checkpoint") &&
       hermes_deploy.include?("restore_checkpoint_deployment_sources") &&
       hermes_deploy.include?('replace_hermes_agents "$rollback_file/hermes-images.env"'),
       "Hermes-only deployment must stage off-live and recover its exact checkpoint transactionally")

full_deploy = homelab.split("deploy_full() {", 2).fetch(1).split("deploy_hermes() {", 2).fetch(0)
assert(full_deploy.index("stage_hermes_cli") < full_deploy.index("checkpoint_images") &&
       full_deploy.index("checkpoint_images") < full_deploy.index("activate_hermes_stage"),
       "full deployment must stage Hermes off-live and checkpoint before activation")
assert(full_deploy.scan("verify_image_attestation").length == 2 &&
       full_deploy.rindex("verify_image_attestation") < full_deploy.index("checkpoint_images"),
       "full deployment must attest both new images before publishing the rollback checkpoint")
assert(full_deploy.include?('service_image=$(immutable_image_id "$service_image")') &&
       full_deploy.include?('runner_image=$(immutable_image_id "$runner_image")'),
       "full deployment must replace both images by their attested immutable IDs")
assert(full_deploy.include?("verify_live_mcp_schema") &&
       full_deploy.include?("verify_running_image_attestations") &&
       full_deploy.include?("verify_mounted_hermes_sources"),
       "full deployment must verify exact MCP, image attestations, and mounted Hermes sources")
assert(full_deploy.scan("assert_no_active_job").length >= 2 &&
       full_deploy.include?("quiesce_runner") &&
       full_deploy.index("quiesce_runner") < full_deploy.index('replace_images "$service_image" "$runner_image"') &&
       full_deploy.include?("resume_runner_watcher") &&
       full_deploy.include?("verify_runner_service_compatibility"),
       "full deployment must quiesce the ready idle runner through replacement and bound compatibility")
assert(full_deploy.include?("stage_hermes_cli") &&
       full_deploy.index("checkpoint_images") < full_deploy.index("activate_hermes_stage") &&
       full_deploy.index("stage_hermes_cli") < full_deploy.index("activate_hermes_stage") &&
       full_deploy.include?("restore_checkpoint_deployment_sources"),
       "full deployment must stage Hermes off-live and recover checkpointed sources on activation failure")
assert(full_deploy.scan("resume_runner_watcher_and_wait_ready").length >= 2 &&
       full_deploy.index("resume_runner_watcher_and_wait_ready") < full_deploy.index("    ); then"),
       "full deployment must keep bounded watcher readiness inside the rollback transaction")
assert(full_deploy.scan("verify_resumed_runtime_or_requiesce").length >= 2 &&
       full_deploy.index("verify_resumed_runtime_or_requiesce") < full_deploy.index("    ); then"),
       "full deployment must restore quiescence when post-resume verification fails")
running_attestations = homelab.split("verify_running_image_attestations() {", 2).fetch(1).split("verify_mounted_hermes_sources() {", 2).fetch(0)
assert(running_attestations.include?("dev.iamstubborn.media.source-tree-digest") &&
       running_attestations.include?("dev.iamstubborn.media.runner-build-digest"),
       "running service and runner must expose both exact build attestations")

image_suffix = homelab.split("image_suffix() {", 2).fetch(1).split("deploy_service() {", 2).fetch(0)
assert(image_suffix.include?("source_tree_digest") && !image_suffix.include?("git diff --binary"),
       "image suffix must use the same deterministic source-tree digest as OCI labels")

full_rollback = homelab.split("rollback_full() {", 2).fetch(1).split("case ${1:-}", 2).fetch(0)
full_attempt = homelab.split("perform_full_rollback() {", 2).fetch(1).split("rollback_full() {", 2).fetch(0)
full_contract = full_rollback + full_attempt
%w[
  forward_service_image
  forward_runner_image
  forward_migration_version
  forward_schema
  migrate_down_one_with_image
  assert_db_migration_version
  replace_hermes_agents
  verify_live_mcp_schema
  verify_mounted_hermes_sources
  restore_checkpoint_deployment_sources
  restore_forward_deployment_sources
].each do |contract|
  assert(full_contract.include?(contract), "full rollback must include transactional contract #{contract}")
end
restore_checkpoint = homelab.split("restore_checkpoint_deployment_sources() {", 2).fetch(1).split("restore_forward_deployment_sources() {", 2).fetch(0)
restore_forward = homelab.split("restore_forward_deployment_sources() {", 2).fetch(1).split("perform_service_rollback() {", 2).fetch(0)
assert(restore_checkpoint.include?("hermes-images.env") && restore_forward.include?("hermes-images.env"),
       "full rollback source restoration must fail closed without exact Hermes/notifier image records")
checkpoint = homelab.split("checkpoint_images() {", 2).fetch(1).split("protected_snapshot() {", 2).fetch(0)
assert(checkpoint.include?("compose.media-orchestrator.yml") &&
       checkpoint.include?("hermes-source") && checkpoint.include?("hermes-images.env") &&
       checkpoint.include?("SERVICE_IMAGE_ID") && checkpoint.include?("RUNNER_IMAGE_ID") &&
       checkpoint.include?("key=HERMES_PRIMARY") && checkpoint.include?("key=NOTIFIER_PRIMARY") &&
       checkpoint.include?("rsync"),
       "full rollback checkpoint must capture exact Compose, sources, runtime IDs, and Hermes/notifier refs")
assert(checkpoint.include?('docker inspect media-service --format \'{{.Image}}\'') &&
       checkpoint.include?('docker inspect download-runner --format \'{{.Image}}\'') &&
       checkpoint.include?('test "$running_service_image_id" = "$service_image_id"') &&
       checkpoint.include?('test "$running_runner_image_id" = "$runner_image_id"'),
       "checkpoint publication must prove env image refs resolve to the exact running image IDs")
compatibility = homelab.split("verify_runner_service_compatibility() {", 2).fetch(1).split("image_suffix() {", 2).fetch(0)
assert(compatibility.include?("attempts") && compatibility.include?("runner iteration failed") &&
       compatibility.include?("Service") && compatibility.include?("previous_runner_id") &&
       compatibility.include?("ExitCode") && compatibility.include?('test "$exit_code" = 0') &&
       compatibility.include?(".State.Health.Status") && compatibility.include?("healthy"),
       "runner/service compatibility must require a new generation and reject Service failures or nonzero exit")
assert(compatibility.include?("expected_service_image") &&
       compatibility.include?("expected_runner_image") &&
       compatibility.include?("docker exec download-runner media healthcheck") &&
       compatibility.include?("http://media-service:8080/v1/ready"),
       "runner compatibility must attest exact images and prove current-generation service interaction")
replace_images = homelab.split("replace_images() {", 2).fetch(1).split("replace_service_image() {", 2).fetch(0)
resume_runner = homelab.split("resume_runner_watcher_and_wait_ready() {", 2).fetch(1).split("hold_runner_quiescence() {", 2).fetch(0)
assert(replace_images.include?("docker compose") && replace_images.include?("create --force-recreate download-runner") &&
       !replace_images.include?("up -d --no-deps --force-recreate download-runner") &&
       resume_runner.include?("docker start download-runner"),
       "full replacement must create the runner stopped and start it only during guarded resume")
assert(full_rollback.include?('replace_images "$forward_service_image" "$forward_runner_image"') &&
       full_rollback.include?("restoring the forward full stack"),
       "failed full rollback must automatically restore the exact forward stack")
assert(full_rollback.include?("quiesce_runner") &&
       full_contract.scan("resume_runner_watcher_and_wait_ready").length >= 2 &&
       full_contract.scan("verify_resumed_runtime_or_requiesce").length >= 2 &&
       full_rollback.include?("recovery_failed"),
       "full rollback must hold quiescence through either result and attempt watcher recovery on every failure")
assert(full_rollback.include?("checkpoint_forward_deployment_sources") &&
       full_rollback.scan("cleanup_forward_deployment_sources").length >= 2 &&
       full_rollback.index("checkpoint_forward_deployment_sources") < full_rollback.index("quiesce_runner") &&
       full_rollback.rindex("cleanup_forward_deployment_sources") > full_rollback.index("perform_full_rollback"),
       "full rollback forward snapshot must live through rollback, recovery, and guarded verification")
assert(full_rollback.include?("perform_full_rollback") &&
       full_attempt.scan("|| return 1").length >= 5,
       "full rollback must explicitly propagate every failed attempt step")
assert(full_attempt.include?("verify_mounted_hermes_sources remote") &&
       full_rollback.include?("verify_mounted_hermes_sources remote"),
       "full rollback and recovery must verify restored remote sources rather than the forward checkout")
full_protected = homelab.split("full_protected_snapshot() {", 2).fetch(1).split("assert_service_only_rollout() {", 2).fetch(0)
%w[media-postgres gluetun gluetun-rezka qbittorrent].each do |container|
  assert(full_protected.include?(container), "full rollback must preserve #{container} container identity")
end
service_protected = homelab.split("protected_snapshot() {", 2).fetch(1).split("full_protected_snapshot() {", 2).fetch(0)
service_protected_assertion = homelab.split("assert_protected_unchanged() {", 2).fetch(1).split("full_protected_snapshot() {", 2).fetch(0)
%w[download-runner gluetun gluetun-rezka gluetun-rezka-watcher media-postgres qbittorrent].each do |container|
  assert(service_protected.include?(container), "service deployment must preserve #{container} container identity")
end
assert(service_protected.include?("{{.Image}}") && service_protected.include?("assert_protected_unchanged") &&
       %w[media-postgres gluetun gluetun-rezka qbittorrent].all? { |name| service_protected_assertion.include?(name) },
       "service protected assertion must compare matching stable and restarted container fields")
assert(service_deploy.scan('verify_resumed_runtime_or_requiesce "$protected_before" assert_protected_unchanged').length >= 2 &&
       service_rollback.scan('verify_resumed_runtime_or_requiesce "$protected_before" assert_protected_unchanged').length >= 2 &&
       full_contract.scan('verify_resumed_runtime_or_requiesce "$protected_before" assert_full_protected_unchanged').length >= 2,
       "post-resume verification must use the matching service or full protected snapshot contract")
normalized_runbook = runbook.gsub(/\s+/, " ")
assert(normalized_runbook.include?("runner-build-digest") && normalized_runbook.include?("source-tree digest") &&
       normalized_runbook.include?("automatically restores the forward full stack"),
       "runbook must document build attestations and transactional full rollback")
assert(normalized_runbook.include?("exact Docker build context") &&
       normalized_runbook.include?("final idle fence") &&
       normalized_runbook.include?("Compose and Hermes source snapshot") &&
       normalized_runbook.include?("runner iteration"),
       "runbook must document exact context, quiescence, rollback sources, and compatibility checks")
assert(normalized_runbook.include?("host-wide deployment lock") &&
       normalized_runbook.include?("off-live") &&
       normalized_runbook.include?("immutable image IDs") &&
       normalized_runbook.include?("quiesced until") &&
       normalized_runbook.include?("notifier image references"),
       "runbook must document the locked transactional activation and exact recovery boundary")
assert(normalized_runbook.include?("both `gluetun` and `gluetun-rezka`") &&
       normalized_runbook.include?("bounded watcher-readiness") &&
       normalized_runbook.include?("re-quiesces"),
       "runbook must name the protected Gluetun set and bounded watcher rollback behavior")
assert(!normalized_runbook.include?("does not stop or restart the watcher") &&
       normalized_runbook.include?("same runner and watcher container IDs") &&
       normalized_runbook.include?("Hermes-only rollout stages") &&
       normalized_runbook.include?("before activating mounted sources"),
       "runbook must describe service quiescence and transactional Hermes-only staging")
assert(normalized_runbook.include?("export-release-contract.py") &&
       normalized_runbook.include?("--cli dist/media-linux-amd64") &&
       normalized_runbook.include?("--cli-checksum dist/media-linux-amd64.sha256") &&
       normalized_runbook.include?("HOMELAB_ROOT") &&
       normalized_runbook.include?("MEDIA_RELEASE_DIR") &&
       normalized_runbook.include?("does not publish, push, log in, or deploy") &&
       !normalized_runbook.include?("discovered through sibling"),
       "runbook must document the private bundle export and explicit deployment roots")
assert(gitignore.lines.map(&:strip).include?("/dist/"),
       "documented release artifacts must remain outside the clean-worktree gate")

clean_env = {
  "PATH" => ENV.fetch("PATH"),
  "HOMELAB_ROOT" => File.join(ROOT, "test-homelab"),
  "MEDIA_RELEASE_DIR" => File.join(ROOT, "test-release"),
}
_stdout, stderr, status = Open3.capture3(clean_env, "./scripts/homelab.sh", "invalid-command", chdir: ROOT, unsetenv_others: true)
assert(status.exitstatus == 2 && stderr.include?("usage:"),
       "homelab command parsing must work in a clean environment without unbound variables")

puts "packaging architecture checks passed"
