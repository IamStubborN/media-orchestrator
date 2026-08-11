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

%w[docker-build docker-smoke docker-lint extract-linux-cli].each do |task|
  assert(tasks.include?("[tasks.#{task}]"), "mise task #{task} is missing")
end

assert(docker_build.include?("MEDIA_BUILD_TARGETS") && docker_build.include?("service)"),
       "docker build must support a service-only target")
assert(tasks.include?('./scripts/homelab.sh deploy-service'),
       "the default homelab deploy task must use the service-only path")
assert(tasks.include?("[tasks.homelab-deploy-full]"),
       "full-stack deployment must remain an explicit task")

service_deploy = homelab.split("deploy_service() {", 2).fetch(1).split("deploy_full() {", 2).fetch(0)
assert(service_deploy.index("check_hermes_capabilities") < service_deploy.index("docker-build.sh"),
       "Hermes capability preflight must run before the service build")
assert(service_deploy.index("checkpoint_images") < service_deploy.index("replace_service_image"),
       "current images must be checkpointed before service replacement")
assert(service_deploy.include?("sync_homelab_compose"),
       "service deployment must synchronize the reviewed homelab compose file")
assert(!service_deploy.match?(/replace_images|docker stop|force-recreate download-runner/),
       "service-only deployment must not touch runner or VPN services")
assert(service_deploy.index("assert_no_active_job") < service_deploy.index("docker-build.sh"),
       "service deployment must fail closed while a job is active")
assert(service_deploy.include?("protected_snapshot") && service_deploy.include?("assert_protected_unchanged"),
       "service deployment must prove protected containers were unchanged")
assert(homelab.include?("verify_live_mcp_schema") && homelab.include?("MCP_SCHEMA_SHA256"),
       "deployment rollback must preserve and verify the exact MCP schema")
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
down_index = service_rollback.index("migrate_down_one_with_image")
old_image_index = service_rollback.index('replace_service_image "$service_image"')
old_schema_index = service_rollback.index('verify_live_mcp_schema')
assert(down_index && old_image_index && old_schema_index &&
       down_index < old_image_index && old_image_index < old_schema_index,
       "service rollback must migrate down with the forward image before starting and verifying the old image")
recovery_index = service_rollback.index('replace_service_image "$forward_image"')
recovery_migration_index = service_rollback.index('assert_db_migration_version "$forward_migration_version"')
assert(recovery_index && recovery_migration_index && recovery_index < recovery_migration_index,
       "failed rollback must migrate up with the forward image and verify the forward migration version")
assert(service_rollback.index("assert_no_active_job") < down_index &&
       service_rollback.index("protected_snapshot") < down_index &&
       service_rollback.rindex("assert_protected_unchanged") > recovery_migration_index,
       "migration rollback and recovery must remain inside the idle and protected-container guards")
assert(service_rollback.include?('if test "$forward_migration_version" != "$rollback_migration_version"') &&
       service_rollback.include?('assert_db_migration_version "$rollback_migration_version"'),
       "service rollback must skip migrate-down for equal versions and still verify the checkpointed version")

prepare_hermes = homelab.split("prepare_hermes_cli() {", 2).fetch(1).split("sync_homelab_compose() {", 2).fetch(0)
hermes_services = "media-notifier-primary media-notifier-secondary hermes-primary hermes-secondary"
assert(prepare_hermes.include?("docker compose --env-file .env pull #{hermes_services}"),
       "Hermes deployment must only pull the intended Hermes and notifier images")

replace_hermes = homelab.split("replace_hermes_agents() {", 2).fetch(1).split("image_suffix() {", 2).fetch(0)
assert(replace_hermes.include?("up -d --no-deps --force-recreate #{hermes_services}"),
       "Hermes replacement must only recreate the intended Hermes and notifier containers")
%w[agent-browser-updater vaultwarden-init-primary vaultwarden-broker-primary media-service].each do |service|
  assert(!replace_hermes.include?(service), "Hermes replacement must not touch #{service}")
end

hermes_deploy = homelab.split("deploy_hermes() {", 2).fetch(1).split("read_rollback_images() {", 2).fetch(0)
assert(!hermes_deploy.include?("sync_homelab_compose") &&
       !hermes_deploy.include?("force-recreate media-service"),
       "Hermes deployment must not sync or recreate media-service")

clean_env = { "PATH" => ENV.fetch("PATH") }
_stdout, stderr, status = Open3.capture3(clean_env, "./scripts/homelab.sh", "invalid-command", chdir: ROOT, unsetenv_others: true)
assert(status.exitstatus == 2 && stderr.include?("usage:"),
       "homelab command parsing must work in a clean environment without unbound variables")

puts "packaging architecture checks passed"
