#!/usr/bin/env ruby
# frozen_string_literal: true

require "yaml"
require "open3"
require "tmpdir"
require "fileutils"
require "digest"
require "shellwords"

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

remote_payloads = homelab.scan(/<<'REMOTE'\n(.*?)^REMOTE$/m).map(&:first)
assert(remote_payloads.length >= 30, "homelab must retain the expected remote shell payloads")
remote_payloads.each_with_index do |payload, index|
  _output, error, status = Open3.capture3("sh", "-n", stdin_data: payload)
  assert(status.success?, "remote heredoc #{index + 1} has invalid shell syntax: #{error}")
end

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
assert(dockerfile.include?("AS yt-dlp") && dockerfile.include?("AS chrome-headless-shell"),
       "Dockerfile must fetch yt-dlp and chrome-headless-shell in dedicated stages")
assert(dockerfile.include?("USER 65532:65532"), "runtime targets must use the non-root media user")
assert(dockerfile.include?("HEALTHCHECK") && dockerfile.include?("media\", \"healthcheck"),
       "service image must use the media binary for health checks")

service_section = dockerfile.split(/^FROM .* AS service$/, 2).fetch(1).split(/^FROM .* AS runner-packages$/, 2).fetch(0)
assert(!service_section.match?(/ffmpeg|ffprobe|libva|chrome|chromium|playwright/i),
       "service target must not install runner media tooling or a browser")
assert(dockerfile.match?(/ffmpeg=.*ffprobe|ffmpeg=.*libva|ffmpeg/i), "runner target must install ffmpeg")
assert(dockerfile.match?(/amd64\).*intel-media-va-driver/) &&
       dockerfile.match?(/arm64\).*vaapi_driver=""/),
       "Intel VAAPI driver must be installed only for the amd64 runner")
chrome_copy = "COPY --from=chrome-headless-shell --chown=65532:65532 /usr/local/lib/chrome-headless-shell /usr/local/lib/chrome-headless-shell"
runner_section = dockerfile.split(/^FROM .* AS runner$/, 2).fetch(1)
assert(!service_section.include?("chrome-headless-shell"),
       "service target must not contain chrome-headless-shell")
assert(runner_section.include?(chrome_copy),
       "runner target must copy checksum-pinned chrome-headless-shell")
assert(dockerfile.include?('echo "${checksum}  /tmp/${archive}" | sha256sum --check --strict'),
       "chrome-headless-shell zip must be sha256-verified like yt-dlp")
assert(dockerfile.include?("https://storage.googleapis.com/chrome-for-testing-public/${version}/${platform}/${archive}"),
       "chrome-headless-shell must be fetched from Chrome for Testing")
assert(dockerfile.include?("amd64") &&
       dockerfile.include?("version=152.0.7977.54") &&
       dockerfile.include?("platform=linux64") &&
       dockerfile.include?("11cedb5568cd374a76eb738e40bd434cd0c9956820fb406b8bd9edca53428d3e"),
       "amd64 runner must pin Chrome for Testing stable chrome-headless-shell 152.0.7977.54")
assert(!dockerfile.include?("153.0.8010.5"),
       "runner must not mix an arm64 Beta chrome-headless-shell pin")
assert(dockerfile.include?("chrome-headless-shell is amd64-only"),
       "arm64 runner must skip chrome-headless-shell instead of pinning a Beta build")
assert(dockerfile.match?(/FROM .* AS runner-packages[\s\S]*libnss3[\s\S]*FROM .* AS runner/),
       "runner-packages must install chrome-headless-shell shared libraries")
assert(!dockerfile.match?(/\bplaywright\b|\bnpx\b|\bnpm\b/i),
       "Dockerfile must not install browsers via npm, npx, or Playwright")
assert(!dockerfile.match?(/apt-get[\s\S]{0,400}\bchromium\b/i),
       "Dockerfile must not apt-get unpinned Debian Chromium as the browser binary")
assert(runner_section.include?("USER 65532:65532"),
       "runner must remain the non-root media user")
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
assert(!runner.fetch("environment").key?("MEDIA_REZKA_BROWSER_FALLBACK"),
       "compose must not expose an Anubis browser fallback toggle")
assert(!runner.fetch("environment").key?("MEDIA_REZKA_CHROMIUM_BIN"),
       "compose must not override the pinned chrome-headless-shell path")
assert(runner["shm_size"].to_s.match?(/256m/i) || runner["shm_size"] == 268_435_456,
       "compose runner must raise /dev/shm above Docker default 64m")

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
release_dispatch = homelab.split("case ${1:-} in", 2).fetch(1)
assert(homelab.include?("with_release_snapshot") &&
       homelab.include?('MEDIA_RELEASE_DIR=$snapshot') &&
       homelab.include?('(cd "$source" && cp -R . "$snapshot/")') &&
       release_dispatch.include?("with_host_lock deploy_release_service"),
       "release entry points must snapshot the candidate bundle once inside the host lock")
assert(!homelab.lines.first(20).join.include?("HOMELAB_ROOT is required") &&
       !homelab.lines.first(20).join.include?("MEDIA_RELEASE_DIR is required"),
       "status, verify, and rollback parsing must not require candidate paths")
assert(homelab.include?('expected_migration_version') &&
       homelab.include?('assert_db_migration_version "$expected_migration_version"'),
       "release migration must verify the exact manifest postcondition before activation")
migration_baseline = "assert_deploy_migration_baseline() {" + homelab.split("assert_deploy_migration_baseline() {", 2).fetch(1).split("checkpoint_images() {", 2).fetch(0)
assert(migration_baseline.include?("migration_predecessor") &&
       migration_baseline.include?("read_db_migration_version") &&
       migration_baseline.include?("current database migration is neither target nor its immediate predecessor"),
       "deploy baseline must allow only the target or its Migrator-derived immediate predecessor")
Dir.mktmpdir("media-migration-baseline") do |directory|
  marker = File.join(directory, "mutation")
  probe = <<~SH
    set -eu
    #{migration_baseline}
    validate_migration_version() { printf '%s\n' "$1" | grep -Eq '^m[0-9]{8}_[0-9]{6}_[a-z0-9_]+$'; }
    migration_predecessor() { test "$1" = m20260810_000040_tracking_claims; echo m20260810_000039_tracking_posters; }
    read_db_migration_version() { echo "$CURRENT_MIGRATION"; }
    assert_deploy_migration_baseline m20260810_000040_tracking_claims
    : >"$MUTATION_MARKER"
  SH
  environment = { "MUTATION_MARKER" => marker }
  %w[m20260810_000040_tracking_claims m20260810_000039_tracking_posters].each do |current|
    FileUtils.rm_f(marker)
    _out, error, status = Open3.capture3(environment.merge("CURRENT_MIGRATION" => current), "sh", "-c", probe)
    assert(status.success? && File.exist?(marker), "target and immediate predecessor baselines must be allowed: #{error}")
  end
  FileUtils.rm_f(marker)
  _out, gap_error, gap_status = Open3.capture3(
    environment.merge("CURRENT_MIGRATION" => "m20260809_000038_source_choice_sets"),
    "sh", "-c", probe
  )
  assert(!gap_status.success? && gap_error.include?("neither target nor its immediate predecessor") && !File.exist?(marker),
         "multi-step m38 to m40 deploy must fail before mutation")
end
assert(homelab.include?('release_value service_image') &&
       homelab.include?('release_value runner_image') &&
       homelab.include?('docker pull') &&
       homelab.include?('source=$MEDIA_RELEASE_DIR/release.json'),
       "release deployment must load and pull both immutable manifest image references")
release_service_deploy = homelab.split("deploy_service() {", 2).fetch(1).split("deploy_full() {", 2).fetch(0)
release_full_deploy = homelab.split("deploy_full() {", 2).fetch(1).split("deploy_hermes() {", 2).fetch(0)
release_service_attempt = homelab.split("perform_service_deploy() {", 2).fetch(1).split("deploy_service() {", 2).fetch(0)
assert(release_service_deploy.include?('if test "${MEDIA_DEPLOY_RELEASE:-0}" = 1') &&
       release_service_attempt.include?('replace_service_image "$service_image"') &&
       release_service_attempt.include?('verify_local_backend_attestation') &&
       release_full_deploy.include?('replace_full_runtime "$service_image" "$runner_image"') &&
       release_full_deploy.include?('verify_running_release_refs "$service_image" "$runner_image"'),
       "release deployment must deploy manifest refs without local builds and attest the deployed Config.Image")
assert(release_service_deploy.index('pull_release_image "$service_image"') < release_service_deploy.index("checkpoint_images") &&
       release_full_deploy.index('pull_release_image "$service_image"') < release_full_deploy.index("checkpoint_images") &&
       release_full_deploy.index('pull_release_image "$runner_image"') < release_full_deploy.index("checkpoint_images"),
       "release images must be pulled and attested before the guarded mutation checkpoint")
assert(homelab.include?('deploy-local-service') && homelab.include?('deploy-local-full'),
       "local builds must remain explicit separate commands")
stage_release = homelab.split("stage_hermes_cli() {", 2).fetch(1).split("activate_hermes_stage() {", 2).fetch(0)
assert(stage_release.include?('media-linux-amd64.sha256') &&
       stage_release.include?('--staged-cli "$artifact"') &&
       stage_release.index('test "$artifact_sha256" = "$expected_cli_sha256"') < stage_release.index('--staged-cli "$artifact"'),
       "Hermes staging must verify the bundle CLI checksum before Homelab staged-CLI preflight")

stage_function = "stage_hermes_cli() {" + stage_release
dispatch_contract = "deploy_release_service() {" + homelab.split("deploy_release_service() {", 2).fetch(1)
Dir.mktmpdir("media-stage-contract") do |directory|
  root = File.join(directory, "hermes")
  bin = File.join(directory, "bin")
  release = File.join(directory, "release")
  FileUtils.mkdir_p([File.join(root, "artifacts"), bin, release])
  cli = File.join(directory, "media")
  File.binwrite(cli, "local extraction\n")
  digest = Digest::SHA256.file(cli).hexdigest
  File.write(File.join(release, "media-linux-amd64.sha256"), "#{digest}  media-linux-amd64\n")
  marker = File.join(directory, "preflight-called")
  File.write(File.join(root, "scripts-preflight"), <<~'SH')
    #!/bin/sh
    set -eu
    test "$1" = --staged-cli
    test -f "$2"
    : >"$PREFLIGHT_MARKER"
  SH
  File.chmod(0o755, File.join(root, "scripts-preflight"))
  File.write(File.join(bin, "docker"), <<~'SH')
    #!/bin/sh
    case $1 in
      create) echo fixture-container ;;
      cp) cp "$FAKE_CLI" "$3" ;;
      rm) ;;
      *) exit 2 ;;
    esac
  SH
  %w[rsync scp].each do |name|
    File.write(File.join(bin, name), "#!/bin/sh\nexit 0\n")
    File.chmod(0o755, File.join(bin, name))
  end
  File.chmod(0o755, File.join(bin, "docker"))
  probe = <<~SH
    set -eu
    #{stage_function}
    remote() { :; }
    hermes_root=#{root.shellescape}
    host=fixture-host
    remote_root=/remote
    hermes_remote_root=/remote/hermes
    compose_project=homelab
    environment_file=/dev/null
    MEDIA_RELEASE_DIR=#{release.shellescape}
    stage_hermes_cli image-ref docker-host
  SH
  environment = {
    "PATH" => "#{bin}:#{ENV.fetch("PATH")}",
    "FAKE_CLI" => cli,
    "PREFLIGHT_MARKER" => marker,
  }
  # Match the production path while keeping the test fixture compact.
  probe.gsub!("$hermes_root/scripts/deploy-preflight", "$hermes_root/scripts-preflight")
  _out, release_error, release_status = Open3.capture3(environment.merge("MEDIA_DEPLOY_RELEASE" => "1"), "sh", "-c", probe)
  assert(release_status.success? && File.exist?(marker), "release CLI staging must use the pinned bundle checksum and preflight")
  FileUtils.rm_f(marker)
  File.write(File.join(release, "media-linux-amd64.sha256"), "#{'0' * 64}  media-linux-amd64\n")
  _out, drift_error, drift_status = Open3.capture3(environment.merge("MEDIA_DEPLOY_RELEASE" => "1"), "sh", "-c", probe)
  assert(!drift_status.success? && drift_error.include?("staged CLI differs from the release bundle") && !File.exist?(marker),
         "release CLI staging must fail closed on pinned checksum drift")
  _out, local_error, local_status = Open3.capture3(environment.merge("MEDIA_DEPLOY_RELEASE" => "0"), "sh", "-c", probe)
  assert(local_status.success? && !File.exist?(marker),
         "local CLI staging must use its local extraction checksum without pinned-release identity: #{local_error}")

  dispatch_probe = <<~SH
    set -eu
    #{stage_function}
    remote() { :; }
    with_host_lock() { "$@"; }
    with_release_snapshot() { operation=$1; shift; "$operation" "$@"; }
    require_homelab_root() { :; }
    deploy_service() { stage_hermes_cli image-ref docker-host; }
    deploy_full() { stage_hermes_cli image-ref docker-host; }
    deploy_hermes() { stage_hermes_cli image-ref docker-host; }
    status() { :; }
    verify() { :; }
    rollback_service() { :; }
    rollback_full() { :; }
    usage() { exit 2; }
    hermes_root=#{root.shellescape}
    host=fixture-host
    remote_root=/remote
    hermes_remote_root=/remote/hermes
    compose_project=homelab
    environment_file=/dev/null
    MEDIA_RELEASE_DIR=#{release.shellescape}
    #{dispatch_contract}
  SH
  dispatch_probe.gsub!("$hermes_root/scripts/deploy-preflight", "$hermes_root/scripts-preflight")
  File.write(File.join(release, "media-linux-amd64.sha256"), "#{digest}  media-linux-amd64\n")
  _out, hermes_error, hermes_status = Open3.capture3(environment, "sh", "-c", dispatch_probe, "probe", "deploy-hermes")
  assert(hermes_status.success? && File.exist?(marker),
         "deploy-hermes dispatch must enforce release staged-CLI preflight: #{hermes_error}")
  FileUtils.rm_f(marker)
  File.write(File.join(release, "media-linux-amd64.sha256"), "#{'0' * 64}  media-linux-amd64\n")
  _out, hermes_drift_error, hermes_drift_status = Open3.capture3(environment, "sh", "-c", dispatch_probe, "probe", "deploy-hermes")
  assert(!hermes_drift_status.success? && hermes_drift_error.include?("staged CLI differs from the release bundle") && !File.exist?(marker),
         "deploy-hermes dispatch must fail closed on pinned CLI drift")
  _out, local_dispatch_error, local_dispatch_status = Open3.capture3(environment, "sh", "-c", dispatch_probe, "probe", "deploy-local-full")
  assert(local_dispatch_status.success? && !File.exist?(marker),
         "deploy-local-full dispatch must retain local extraction integrity without release identity: #{local_dispatch_error}")
end

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
assert(service_deploy.rindex("assert_no_active_job") < service_deploy.index("checkpoint_images") &&
       service_deploy.index('assert_deploy_migration_baseline "$expected_migration_version"') < service_deploy.index("checkpoint_images") &&
       service_deploy.index("checkpoint_images") < service_deploy.index("quiesce_runner"),
       "service checkpoint promotion must follow the final idle fence and immediately precede mutation")
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
       service_deploy.include?('service_image_id=$(immutable_image_id "$service_image")') &&
       service_deploy.include?('if test "${MEDIA_DEPLOY_RELEASE:-0}" != 1; then') &&
       service_deploy.include?('service_image=$service_image_id') &&
       service_deploy.index('service_image=$service_image_id') < service_deploy.index("checkpoint_images") &&
       service_deploy_contract.include?("verify_running_service_attestation") &&
       service_deploy_contract.include?("verify_mounted_hermes_sources"),
       "service deployment must attest the built and running service, persist local image IDs, and verify mounted Hermes sources")
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
assert(!runner_digest_contract.strip.start_with?("docker_context_digest") &&
       runner_digest_contract.include?("crates") &&
       runner_digest_contract.include?("Dockerfile") &&
       runner_digest_contract.include?("Cargo.lock") &&
       runner_digest_contract.include?(".cargo") &&
       !runner_digest_contract.include?("git ls-files") &&
       runner_digest_contract.include?("crates/*/tests"),
       "runner digest must hash runner-affecting inputs (Dockerfile/Cargo/crate src), not the full context or git ls-files")
assert(runner_digest_contract.include?("Does not invalidate runner") &&
       runner_digest_contract.include?("crates/*/tests"),
       "runner digest must document that crate tests do not invalidate the runner")
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
assert(service_only_guard.include?("candidate_watcher") &&
       service_only_guard.include?("sha256sum") &&
       service_only_guard.include?("runner watcher script changed"),
       "service-only guard must reject watcher script changes before mutation")
replace_images = homelab.split("replace_images() {", 2).fetch(1).split("replace_service_image() {", 2).fetch(0)
replace_full_runtime = homelab.split("replace_full_runtime() {", 2).fetch(1).split("replace_service_image() {", 2).fetch(0)
replace_service = homelab.split("replace_service_image() {", 2).fetch(1).split("migrate_down_one_with_image() {", 2).fetch(0)
replace_hermes = homelab.split("replace_hermes_agents() {", 2).fetch(1).split("verify_runner_service_compatibility() {", 2).fetch(0)
assert(replace_images.include?("--project-name '$compose_project'") &&
       replace_images.include?("cd '$remote_root'") &&
       !replace_images.include?("/media'; docker compose"),
       "runtime image replacement must use the root homelab Compose project")
assert(replace_service.include?("--project-name homelab") &&
       replace_service.include?('cd "$remote_root"') &&
       !replace_service.include?('cd "$remote_root/media"'),
       "service image replacement must use the root homelab Compose project")
assert(replace_full_runtime.include?("gluetun-rezka") &&
       replace_full_runtime.include?("gluetun-rezka-watcher") &&
       replace_full_runtime.include?("--force-recreate gluetun-rezka") &&
       replace_full_runtime.include?("skipping force-recreate") &&
       replace_full_runtime.include?("gluetun_rezka_compose_digest") &&
       replace_full_runtime.include?("gluetun-watcher") &&
       replace_full_runtime.include?("--force-recreate --no-start download-runner gluetun-rezka-watcher") &&
       replace_full_runtime.index("gluetun-rezka") < replace_full_runtime.index("--no-start download-runner"),
       "full runtime replacement must optionally skip unchanged gluetun-rezka and wait for VPN before stopped runner/watcher")
assert(replace_hermes.include?("--project-name homelab") &&
       replace_hermes.include?('cd "$remote_root"') &&
       !replace_hermes.include?('cd "$hermes_root"'),
       "Hermes recreate must use the root homelab Compose project")
assert(homelab.include?("gluetun-watcher") &&
       homelab.include?("download_watcher_present") &&
       homelab.include?("restore_download_watcher"),
       "media quiescence must fence and restore download gluetun-watcher around gluetun-rezka mutation")
assert(homelab.include?("verify_live_mcp_schema") && homelab.include?("MCP_SCHEMA_SHA256"),
       "deployment rollback must preserve and verify the exact MCP schema")
schema_bootstrap = homelab.split("preflight_deployed_mcp_schema() {", 2).fetch(1).split("ensure_deployed_mcp_schema() {", 2).fetch(0)
assert(schema_bootstrap.include?("test -s \"$schema_file\"") &&
       schema_bootstrap.include?("sha256sum \"$schema_file\"") &&
       schema_bootstrap.include?("schema_version") &&
       schema_bootstrap.include?("schema-only") &&
       homelab.include?("refusing deployment without an exact Hermes MCP schema artifact") == false,
       "schema preflight must validate the stored artifact shape/hash without inventing source metadata")
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
assert(prepare_hermes.include?("docker compose --project-name '$compose_project' --env-file '$environment_file' pull #{hermes_services}"),
       "Hermes deployment must only pull the intended Hermes and notifier images")
assert(prepare_hermes.include?("hermes_consumers_unchanged") &&
       prepare_hermes.include?("skipping Hermes image pull") &&
       replace_hermes.include?("skipping Hermes consumer recreate"),
       "Hermes staging must skip pull/recreate when CLI, schema, and mount inputs are unchanged")

host_lock = homelab.split("acquire_host_lock() {", 2).fetch(1).split("release_host_lock() {", 2).fetch(0)
assert(host_lock.include?("flock -n") && host_lock.include?("media-orchestrator.deploy.lock"),
       "deploy and rollback commands must hold one host-wide flock")
mutating_dispatch = homelab.split("case ${1:-} in", 2).fetch(1)
assert(mutating_dispatch.include?("with_host_lock deploy_release_service") &&
       mutating_dispatch.include?("with_host_lock deploy_release_full") &&
       mutating_dispatch.include?("with_host_lock deploy_local_service") &&
       mutating_dispatch.include?("with_host_lock deploy_local_full") &&
       mutating_dispatch.include?("with_host_lock deploy_release_hermes") &&
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
assert(replace_hermes.include?('if test "$#" -ge 3; then') &&
       replace_hermes.include?("image_record=\$3"),
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
assert(full_deploy.rindex("assert_no_active_job") < full_deploy.index("checkpoint_images") &&
       full_deploy.index('assert_deploy_migration_baseline "$expected_migration_version"') < full_deploy.index("checkpoint_images") &&
       full_deploy.index("checkpoint_images") < full_deploy.index("quiesce_runner"),
       "full checkpoint promotion must follow the final idle fence and precede quiescence")
assert(full_deploy.scan("verify_image_attestation").length == 2 &&
       full_deploy.rindex("verify_image_attestation") < full_deploy.index("checkpoint_images"),
       "full deployment must attest both new images before publishing the rollback checkpoint")
assert(full_deploy.include?('service_image_id=$(immutable_image_id "$service_image")') &&
       full_deploy.include?('runner_image_id=$(immutable_image_id "$runner_image")'),
       "full deployment must resolve both refs to IDs for runtime compatibility attestation")
assert(full_deploy.include?('if test "${MEDIA_DEPLOY_RELEASE:-0}" != 1; then') &&
       full_deploy.include?('service_image=$service_image_id') &&
       full_deploy.include?('runner_image=$runner_image_id') &&
       full_deploy.index('service_image=$service_image_id') < full_deploy.index("checkpoint_images") &&
       full_deploy.index('runner_image=$runner_image_id') < full_deploy.index("checkpoint_images"),
       "local full deployment must persist exact service and runner image IDs before checkpointed mutation")
assert(full_deploy.include?("verify_live_mcp_schema") &&
       full_deploy.include?("verify_running_image_attestations") &&
       full_deploy.include?("verify_mounted_hermes_sources"),
       "full deployment must verify exact MCP, image attestations, and mounted Hermes sources")
assert(full_deploy.scan("assert_no_active_job").length >= 2 &&
       full_deploy.include?("quiesce_runner") &&
       full_deploy.include?("quiesce_runner full") &&
       full_deploy.index("quiesce_runner full") < full_deploy.index('replace_full_runtime "$service_image" "$runner_image"') &&
       full_deploy.include?("resume_runner_watcher") &&
       full_deploy.include?("verify_runner_service_compatibility"),
       "full deployment must quiesce the ready idle runner through replacement and bound compatibility")
assert(full_deploy.include?("stage_hermes_cli") &&
       full_deploy.index("checkpoint_images") < full_deploy.index("activate_hermes_stage") &&
       full_deploy.index("stage_hermes_cli") < full_deploy.index("activate_hermes_stage") &&
       full_deploy.include?("restore_checkpoint_deployment_sources"),
       "full deployment must stage Hermes off-live and recover checkpointed sources on activation failure")
assert(full_deploy.scan("resume_runner_watcher_and_wait_ready").length >= 1 &&
       full_deploy.index("resume_runner_watcher_and_wait_ready") < full_deploy.index("    ); then"),
       "full deployment success must keep bounded watcher readiness inside the transaction")
full_deploy_recovery = full_deploy.split("echo \"full deployment failed; restoring its exact checkpoint\"", 2).fetch(1)
assert(full_deploy_recovery.include?("restore_full_runtime_safe_hold \"$session_volume\"") &&
       full_deploy_recovery.include?("replace_full_runtime_safe_hold \"$rollback_service_image\" \"$rollback_runner_image\"") &&
       !full_deploy_recovery.include?("replace_full_runtime \"$rollback_service_image\" \"$rollback_runner_image\"") &&
       !full_deploy_recovery.include?("verify_live_mcp_schema") &&
       full_deploy_recovery.include?("runtime remains in safe hold") &&
       !full_deploy_recovery.include?("resume_runner_watcher_and_wait_ready") &&
       !full_deploy_recovery.include?("verify_resumed_runtime_or_requiesce"),
       "failed full deployment must restore sources/images and leave the runtime in an exact safe hold")
assert(full_deploy_recovery.scan('if test "$recovery_failed" = 0; then').length >= 7 &&
       full_deploy_recovery.index("restore_full_runtime_safe_hold") < full_deploy_recovery.index("restore_checkpoint_deployment_sources") &&
       full_deploy_recovery.index("restore_checkpoint_deployment_sources") < full_deploy_recovery.index("MCP_SCHEMA.json") &&
       full_deploy_recovery.index("MCP_SCHEMA.json") < full_deploy_recovery.index("replace_full_runtime_safe_hold"),
       "failed full deployment recovery must gate every restore step on a successful safe hold")
assert(full_deploy.scan("verify_resumed_runtime_or_requiesce").length >= 1 &&
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
assert(restore_checkpoint.include?("hermes-images.env") && restore_forward.include?("hermes-images.env") &&
       restore_checkpoint.include?("gluetun-rezka-watcher-watch.sh") &&
       restore_forward.include?("gluetun-rezka-watcher-watch.sh") &&
       restore_checkpoint.include?("MCP_SCHEMA_SHA256") &&
       restore_forward.include?("MCP_SCHEMA.sha256"),
       "full rollback source restoration must fail closed without exact Hermes/notifier image records")
checkpoint = homelab.split("checkpoint_images() {", 2).fetch(1).split("protected_snapshot() {", 2).fetch(0)
assert(checkpoint.include?("compose.media-orchestrator.yml") &&
       checkpoint.include?("gluetun-rezka-watcher-watch.sh") &&
       checkpoint.include?("hermes-source") && checkpoint.include?("hermes-images.env") &&
       checkpoint.include?("SERVICE_IMAGE_ID") && checkpoint.include?("RUNNER_IMAGE_ID") &&
       checkpoint.include?("key=HERMES_PRIMARY") && checkpoint.include?("key=NOTIFIER_PRIMARY") &&
       checkpoint.include?("rsync"),
       "full rollback checkpoint must capture exact Compose, sources, runtime IDs, and Hermes/notifier refs")
assert(checkpoint.include?('docker inspect media-service --format \'{{.Image}}\'') &&
       checkpoint.include?('docker inspect download-runner --format \'{{.Image}}\'') &&
       checkpoint.include?('test "$running_service_image_id" = "$service_image_id"') &&
       checkpoint.include?('test "$running_runner_image_id" = "$runner_image_id"') &&
       checkpoint.include?('schema_hash_file=$7') &&
       checkpoint.include?('mv -f "$schema_hash_file.next" "$schema_hash_file"'),
       "checkpoint publication must prove env image refs resolve to the exact running image IDs")
sync_compose = homelab.split("sync_homelab_compose() {", 2).fetch(1).split("replace_hermes_agents() {", 2).fetch(0)
assert(sync_compose.include?("watcher_source") &&
       sync_compose.include?("watch.sh") &&
       sync_compose.include?("watcher_next") &&
       sync_compose.include?("mv -f \"$watcher_file.next.ready\" \"$watcher_file\""),
       "Compose synchronization must atomically stage the candidate watcher script")
watcher_runtime = homelab.split("prepare_watcher_runtime() {", 2).fetch(1).split("replace_hermes_agents() {", 2).fetch(0)
assert(watcher_runtime.include?("stat -c '%g' /var/run/docker.sock") &&
       watcher_runtime.include?("DOCKER_SOCKET_GID=") &&
       watcher_runtime.include?("docker run --rm --user 0:0") &&
       watcher_runtime.include?("--cap-add CHOWN") &&
       watcher_runtime.include?("chown \"$uid:$gid\" /state"),
       "watcher preflight must persist the host socket GID and prepare its non-secret state volume")
replace_full_runtime_contract = homelab.split("replace_full_runtime() {", 2).fetch(1).split("replace_service_image() {", 2).fetch(0)
assert(replace_full_runtime_contract.include?("prepare_watcher_runtime"),
       "full runtime replacement must run watcher credential and state preflight before Compose")
safe_hold_recovery = homelab.split("restore_full_runtime_safe_hold() {", 2).fetch(1).split("verify_resumed_runtime_or_requiesce() {", 2).fetch(0)
assert(safe_hold_recovery.include?("docker stop gluetun-rezka-watcher") &&
       safe_hold_recovery.include?("docker stop download-runner") &&
       safe_hold_recovery.include?("watcher_restart_drain_checks=65") &&
       safe_hold_recovery.include?("watcher_fence_checks=3") &&
       safe_hold_recovery.include?("wait_watcher_fence() {") &&
       safe_hold_recovery.include?("wait_watcher_fence \"$watcher_fence_checks\"") &&
       safe_hold_recovery.include?("consume_watcher_fence() {") &&
       safe_hold_recovery.include?("consume_watcher_fence \"$watcher_restart_drain_checks\"") &&
       safe_hold_recovery.include?("max_resurrections=${2:-3}") &&
       safe_hold_recovery.include?("max_observations=$((checks * 2))") &&
       safe_hold_recovery.include?("observed_lifecycle_state=$(lifecycle_state)") &&
       safe_hold_recovery.include?("test \"$media_state\" = created") &&
       safe_hold_recovery.include?("write_lifecycle_rotating") &&
       safe_hold_recovery.index("docker stop gluetun-rezka-watcher") < safe_hold_recovery.index("write_lifecycle_rotating") &&
       safe_hold_recovery.include?('lifecycle_state)" = rotating') &&
       safe_hold_recovery.include?("--network container:media-service") &&
       safe_hold_recovery.include?("--user \"$watcher_uid:$watcher_gid\"") &&
       safe_hold_recovery.include?("--cap-drop ALL") &&
       safe_hold_recovery.include?('{"state":"rotating"}') &&
       safe_hold_recovery.include?("active_job_count") &&
       safe_hold_recovery.include?("expected_session_volume") &&
       safe_hold_recovery.include?("encrypted runner session volume changed") &&
       safe_hold_recovery.include?("safe_hold_snapshot") &&
       safe_hold_recovery.include?("exact_safe_hold") &&
       safe_hold_recovery.include?("stopped_state") &&
       safe_hold_recovery.include?("first=$(safe_hold_snapshot)") &&
       safe_hold_recovery.include?("second=$(safe_hold_snapshot)") &&
       safe_hold_recovery.include?("docker stop media-service") &&
       safe_hold_recovery.index("docker stop media-service") < safe_hold_recovery.index("write_lifecycle_direct_rotating\n", safe_hold_recovery.index("docker stop media-service")) &&
       safe_hold_recovery.index('test "$(active_job_count)" = 0 || { echo "a job became active before stopping the runner') < safe_hold_recovery.index("docker stop download-runner") &&
       safe_hold_recovery.include?("HostConfig.RestartPolicy.Name") &&
       !safe_hold_recovery.include?("docker update --restart=no gluetun-rezka-watcher >/dev/null 2>&1 || true") &&
       !safe_hold_recovery.include?("docker update --restart=no media-service >/dev/null 2>&1 || true") &&
       safe_hold_recovery.include?("media-service restarted during full recovery safe hold") &&
       safe_hold_recovery.include?("full recovery lifecycle changed after final watcher fence") &&
       safe_hold_recovery.include?("exact_safe_hold \"$watcher_fence_checks\" ||") &&
       safe_hold_recovery.include?("runner_restart_policy") &&
       safe_hold_recovery.include?("recovery_exact=1") &&
       safe_hold_recovery.include?("docker update --restart=no download-runner") &&
       safe_hold_recovery.index('test "$(active_job_count)" = 0 || {') <
         safe_hold_recovery.index("docker update --restart=no download-runner") &&
       safe_hold_recovery.include?("start_recovery_container") &&
       !safe_hold_recovery.include?("resume_runner_watcher_and_wait_ready"),
       "full recovery must stop the old boundary, fence rotating in isolation, and verify jobs and session volume without resuming")
recovery_exact_marker = safe_hold_recovery.index("recovery_exact=1")
recovery_reentry_watcher_update = safe_hold_recovery.index("docker update --restart=no gluetun-rezka-watcher", recovery_exact_marker || 0)
assert(recovery_exact_marker && recovery_reentry_watcher_update &&
       recovery_exact_marker < recovery_reentry_watcher_update,
       "full recovery must mark the first exact hold before any re-entry policy mutation")
recovery_stop = safe_hold_recovery.index("docker stop gluetun-rezka-watcher")
recovery_rotate = safe_hold_recovery.index("write_lifecycle_rotating\n", recovery_stop)
recovery_consuming_fence = safe_hold_recovery.index("consume_watcher_fence \"$watcher_restart_drain_checks\"", recovery_rotate)
assert(recovery_stop && recovery_rotate && recovery_consuming_fence &&
       recovery_stop < recovery_rotate && recovery_rotate < recovery_consuming_fence,
       "full recovery must reassert rotating before the consuming restart drain")
held_recovery = homelab.split("replace_full_runtime_safe_hold() {", 2).fetch(1).split("verify_resumed_runtime_or_requiesce() {", 2).fetch(0)
assert(held_recovery.include?("run --rm --no-deps media-service migrate") &&
       held_recovery.include?("up --no-start --no-deps --force-recreate media-service") &&
       held_recovery.include?("up --no-start --no-deps --force-recreate download-runner gluetun-rezka-watcher") &&
       !held_recovery.include?("create --no-start --no-deps --force-recreate") &&
       held_recovery.include?("up -d --no-deps --force-recreate gluetun-rezka") &&
       held_recovery.include?("docker update --restart=no media-service") &&
       held_recovery.include?("docker update --restart=no download-runner") &&
       held_recovery.include?("docker update --restart=no gluetun-rezka-watcher") &&
       held_recovery.include?("expected_session_volume") &&
       held_recovery.include?("lifecycle_state)\" = rotating") &&
       held_recovery.include?("active_job_count)\" = 0") &&
       held_recovery.index("up --no-start --no-deps --force-recreate media-service") <
         held_recovery.index("up -d --no-deps --force-recreate gluetun-rezka") &&
       held_recovery.index("up -d --no-deps --force-recreate gluetun-rezka") <
         held_recovery.index("up --no-start --no-deps --force-recreate download-runner gluetun-rezka-watcher") &&
       !held_recovery.include?("up -d --no-deps --force-recreate media-service") &&
       !held_recovery.include?("docker start media-service") &&
       !held_recovery.include?("verify_live_mcp_schema"),
       "held full recovery must recreate stopped containers, replace only Gluetun live, and never probe stopped media-service")
schema_preflight = homelab.split("preflight_deployed_mcp_schema() {", 2).fetch(1).split("\nREMOTE\n}", 2).fetch(0).split("<<'REMOTE'\n", 2).fetch(1)
schema_preflight_probe = <<~SH
  set -eu
  docker() {
    if test "$1" = inspect; then
      case "$2" in
        media-service) printf '%s\n' "$MEDIA_STATE" ;;
        gluetun-rezka-watcher) printf '%s\n' "$WATCHER_STATE" ;;
        download-runner) printf '%s\n' "$RUNNER_STATE" ;;
        *) exit 2 ;;
      esac
    elif test "$1" = exec; then
      case "$*" in
        *"select state from runner_lifecycle"*) printf '%s\n' "$LIFECYCLE_STATE" ;;
        *"select state from jobs"*) test "$ACTIVE_JOBS" = 0 || printf '%s\n' running ;;
        *"select count(*) from jobs"*) printf '%s\n' "$ACTIVE_JOBS" ;;
        *) exit 2 ;;
      esac
    else
      exit 2
    fi
  }
  #{schema_preflight}
SH
Dir.mktmpdir("schema-preflight") do |directory|
  valid_schema = File.join(directory, "valid.json")
  malformed_schema = File.join(directory, "malformed.json")
  expected_hash = File.join(directory, "expected.sha256")
  File.write(valid_schema, '{"schema_version":1,"tools":[{"name":"media_search"}]}' + "\n")
  File.write(malformed_schema, '{"schema_version":1,"tools":[]}' + "\n")
  File.write(expected_hash, Digest::SHA256.file(valid_schema).hexdigest + "\n")
  schema_cases = {
    "exact stopped safe hold" => [{"MEDIA_STATE" => "exited", "LIFECYCLE_STATE" => "rotating", "WATCHER_STATE" => "exited", "RUNNER_STATE" => "exited", "ACTIVE_JOBS" => "0"}, valid_schema, expected_hash, true, "schema-only"],
    "created stopped safe hold" => [{"MEDIA_STATE" => "created", "LIFECYCLE_STATE" => "rotating", "WATCHER_STATE" => "created", "RUNNER_STATE" => "created", "ACTIVE_JOBS" => "0"}, valid_schema, expected_hash, true, "schema-only"],
    "running service uses live mode" => [{"MEDIA_STATE" => "running", "LIFECYCLE_STATE" => "ready", "WATCHER_STATE" => "running", "RUNNER_STATE" => "running", "ACTIVE_JOBS" => "0"}, valid_schema, expected_hash, true, "live"],
    "stopped runner mixed state" => [{"MEDIA_STATE" => "exited", "LIFECYCLE_STATE" => "rotating", "WATCHER_STATE" => "exited", "RUNNER_STATE" => "running", "ACTIVE_JOBS" => "0"}, valid_schema, expected_hash, false, nil],
    "stopped ready mixed state" => [{"MEDIA_STATE" => "exited", "LIFECYCLE_STATE" => "ready", "WATCHER_STATE" => "exited", "RUNNER_STATE" => "exited", "ACTIVE_JOBS" => "0"}, valid_schema, expected_hash, false, nil],
    "stopped malformed schema" => [{"MEDIA_STATE" => "exited", "LIFECYCLE_STATE" => "rotating", "WATCHER_STATE" => "exited", "RUNNER_STATE" => "exited", "ACTIVE_JOBS" => "0"}, malformed_schema, expected_hash, false, nil],
    "stopped hash mismatch" => [{"MEDIA_STATE" => "exited", "LIFECYCLE_STATE" => "rotating", "WATCHER_STATE" => "exited", "RUNNER_STATE" => "exited", "ACTIVE_JOBS" => "0"}, valid_schema, malformed_schema, false, nil]
  }
  schema_cases.each do |name, (environment, schema_file, hash_file, expected_success, expected_mode)|
    output, error, status = Open3.capture3(environment, "sh", "-c", schema_preflight_probe, "probe", schema_file, hash_file)
    assert(status.success? == expected_success, "schema preflight case #{name.inspect} returned an unexpected result: #{error}")
    assert(!expected_success || output.lines.map(&:strip).include?(expected_mode), "schema preflight case #{name.inspect} did not return #{expected_mode.inspect}")
  end
end
quiesce = homelab.split("quiesce_runner() {", 2).fetch(1).split("resume_runner_watcher_and_wait_ready() {", 2).fetch(0)
assert(quiesce.include?("full_safe_hold() {"),
       "full quiescence must expose an idempotent exact safe-hold gate")
safe_hold = quiesce.split("full_safe_hold() {", 2).fetch(1).split("if full_safe_hold", 2).fetch(0)
assert(safe_hold.include?("test \"$lifecycle_target\" = full") &&
       safe_hold.include?("= exited") &&
       safe_hold.include?("= created") &&
       safe_hold.include?("stopped_state") &&
       safe_hold.include?("active_job_count") &&
       safe_hold.include?("runner_restart_policy") &&
       safe_hold.scan("docker inspect").length >= 4,
       "safe-hold gate must require full mode, stopped containers, zero jobs, and a second verification")
safe_hold_probe = <<~SH
  set -eu
  lifecycle_state() { docker exec media-postgres sh -lc 'select state from runner_lifecycle'; }
  active_job_count() { docker exec media-postgres sh -lc 'select count(*) from jobs'; }
  #{"full_safe_hold() {" + safe_hold}
  docker() {
    if test "$1" = exec; then
      case "$*" in
        *"select state from runner_lifecycle"*) printf '%s\n' "$LIFECYCLE_STATE" ;;
        *"select count(*) from jobs"*) printf '%s\n' "$ACTIVE_JOBS" ;;
        *) exit 2 ;;
      esac
    elif test "$1" = inspect; then
      case "$2" in
        gluetun-rezka-watcher) printf '%s\n' "$WATCHER_STATE" ;;
        download-runner)
          case "$4" in
            *Mounts*) printf '%s\n' session-volume ;;
            *) printf '%s\n' "$RUNNER_STATE" ;;
          esac
          ;;
        media-service)
          case "$4" in
            *RestartPolicy.Name*) printf '%s\n' unless-stopped ;;
            *) printf '%s\n' running ;;
          esac
          ;;
        *) exit 2 ;;
      esac
    else
      exit 2
    fi
  }
  lifecycle_target=full
  full_safe_hold
SH
safe_hold_cases = {
  "exact safe hold" => {"LIFECYCLE_STATE" => "rotating", "WATCHER_STATE" => "exited", "RUNNER_STATE" => "exited", "ACTIVE_JOBS" => "0"},
  "created exact safe hold" => {"LIFECYCLE_STATE" => "rotating", "WATCHER_STATE" => "created", "RUNNER_STATE" => "created", "ACTIVE_JOBS" => "0"},
  "running runner" => {"LIFECYCLE_STATE" => "rotating", "WATCHER_STATE" => "exited", "RUNNER_STATE" => "running", "ACTIVE_JOBS" => "0"},
  "active job" => {"LIFECYCLE_STATE" => "rotating", "WATCHER_STATE" => "exited", "RUNNER_STATE" => "exited", "ACTIVE_JOBS" => "1"},
  "ready stopped" => {"LIFECYCLE_STATE" => "ready", "WATCHER_STATE" => "exited", "RUNNER_STATE" => "exited", "ACTIVE_JOBS" => "0"}
}
safe_hold_cases.each do |name, environment|
  _output, error, status = Open3.capture3(environment, "sh", "-c", safe_hold_probe)
  expected = name == "exact safe hold" || name == "created exact safe hold"
  assert(status.success? == expected, "safe-hold gate case #{name.inspect} returned an unexpected result: #{error}")
end
service_probe = safe_hold_probe.sub(/lifecycle_target=full\nfull_safe_hold/, "lifecycle_target=service\nif full_safe_hold; then exit 1; fi")
_output, service_error, service_status = Open3.capture3(safe_hold_cases.fetch("exact safe hold"), "sh", "-c", service_probe)
assert(service_status.success?, "service-only quiescence must reject a full safe hold: #{service_error}")
safe_hold_return = quiesce.split("if full_safe_hold", 2).fetch(1).split("watcher_restart_policy=", 2).fetch(0)
stop_media_contract = quiesce.split("stop_media_for_full_hold() {", 2).fetch(1).split("consume_watcher_fence() {", 2).fetch(0)
assert(stop_media_contract.include?("docker update --restart=no media-service") &&
       stop_media_contract.include?("docker stop media-service") &&
       stop_media_contract.include?("wait_media_stopped \"$watcher_restart_drain_checks\"") &&
       stop_media_contract.include?("write_lifecycle_direct_rotating") &&
       stop_media_contract.index("docker stop media-service") < stop_media_contract.index("write_lifecycle_direct_rotating"),
       "full quiescence must stop the HTTP lifecycle writer before the direct rotating fence")
assert(!safe_hold_return.include?("docker start") &&
       safe_hold_return.include?("docker update --restart=no gluetun-rezka-watcher") &&
       safe_hold_return.include?("consume_watcher_fence rotating \"$watcher_restart_drain_checks\"") &&
       safe_hold_return.include?("normalize_full_runner") &&
       safe_hold_return.include?("trap cleanup_full_safe_hold_exit EXIT") &&
       safe_hold_return.include?("trap cleanup_full_safe_hold_signal HUP INT TERM") &&
       !safe_hold_return.include?("write_lifecycle ready") &&
       safe_hold_return.index("stop_media_for_full_hold") < safe_hold_return.index("consume_watcher_fence rotating \"$watcher_restart_drain_checks\"") &&
       safe_hold_return.include?("full_safe_hold 1 ||") &&
       safe_hold_return.index("normalize_full_runner") < safe_hold_return.index("full_safe_hold 1 ||") &&
       safe_hold_return.include?("exit 0"),
       "accepted safe hold must stop the legacy media service and establish the direct lifecycle fence before draining restarts")

# If the post-rotation idle fence fails, recovery must leave the runner
# available and stop before any replacement/VPN work. Exercise that ordering
# with an active-job fixture rather than relying only on source-text order.
restore_remote = safe_hold_recovery.split("<<'REMOTE'\n", 2).fetch(1).split("\nREMOTE\n}", 2).fetch(0)
# Keep the fixture fast while retaining the same bounded-fence code path.
restore_remote = restore_remote.gsub("watcher_restart_drain_checks=65", "watcher_restart_drain_checks=3")
restore_ordering_probe = <<~SH
  set -eu
  lifecycle_state_value=ready
  active_jobs_value=1
  watcher_state_value=running
  runner_state_value=running
  media_state_value=running
  media_restart_policy_value=unless-stopped
  watcher_restart_policy_value=unless-stopped
  session_volume_value=session-volume
  events_file=$3
  docker() {
    case $1 in
      inspect)
        case $2 in
          media-service)
            case $4 in
              *State.Status*) printf '%s\\n' "$media_state_value" ;;
              *RestartPolicy.Name*) printf '%s\\n' "$media_restart_policy_value" ;;
              *) exit 2 ;;
            esac
            ;;
          gluetun-rezka-watcher)
            case $4 in
              *State.Status*) printf '%s\\n' "$watcher_state_value" ;;
              *RestartPolicy.Name*) printf '%s\\n' "$watcher_restart_policy_value" ;;
              *.Config.Image*) printf '%s\\n' watcher-image ;;
              *.Mounts*) printf '%s\\n' /tmp/lifecycle-token ;;
              *) exit 2 ;;
            esac
            ;;
          download-runner)
            case $4 in
              *State.Status*) printf '%s\\n' "$runner_state_value" ;;
              *RestartPolicy.Name*) printf '%s\\n' no ;;
              *Destination*) printf '%s\\n' "$session_volume_value" ;;
              *) exit 2 ;;
            esac
            ;;
          *) exit 2 ;;
        esac
        ;;
      exec)
        case "$*" in
          *"select state from runner_lifecycle"*) printf '%s\\n' "$lifecycle_state_value" ;;
          *"select state from jobs"*) test "$active_jobs_value" = 0 || printf '%s\\n' running ;;
          *"select count(*) from jobs"*) printf '%s\\n' "$active_jobs_value" ;;
          *) exit 2 ;;
        esac
        ;;
      update)
        case "$*" in
          *gluetun-rezka-watcher*) watcher_restart_policy_value=no ;;
          *media-service*) media_restart_policy_value=no ;;
          *download-runner*) ;;
          *) exit 2 ;;
        esac
        ;;
      stop)
        case "$2" in
          gluetun-rezka-watcher) watcher_state_value=exited; echo watcher-stopped >>"$events_file" ;;
          download-runner) runner_state_value=exited; echo runner-stopped >>"$events_file" ;;
          media-service) media_state_value=exited; echo media-stopped >>"$events_file" ;;
          *) exit 2 ;;
        esac
        ;;
      run) lifecycle_state_value=rotating ;;
      *) exit 2 ;;
    esac
  }
  #{restore_remote}
SH
Dir.mktmpdir("restore-ordering") do |directory|
  environment_file = File.join(directory, "environment")
  events_file = File.join(directory, "events")
  File.write(environment_file, "PUID=1000\nPGID=1000\n")
  _output, error, status = Open3.capture3(
    "sh", "-c", restore_ordering_probe, "probe", environment_file, "session-volume", events_file
  )
  events = File.exist?(events_file) ? File.read(events_file).lines.map(&:strip) : []
  assert(!status.success?, "active-job safe-hold fixture must fail closed")
  assert(events.empty?, "active-job safe-hold fixture must fail before mutating the running boundary: #{error}")

  created_safe_hold_probe = restore_ordering_probe
    .sub("lifecycle_state_value=ready", "lifecycle_state_value=rotating")
    .sub("active_jobs_value=1", "active_jobs_value=0")
    .sub("watcher_state_value=running", "watcher_state_value=created")
    .sub("runner_state_value=running", "runner_state_value=created")
    .sub("media_state_value=running", "media_state_value=created")
    .sub("media_restart_policy_value=unless-stopped", "media_restart_policy_value=no")
  FileUtils.rm_f(events_file)
  _output, created_error, created_status = Open3.capture3(
    "sh", "-c", created_safe_hold_probe, "probe", environment_file, "session-volume", events_file
  )
  created_events = File.exist?(events_file) ? File.read(events_file).lines.map(&:strip) : []
  assert(created_status.success?, "created-state safe-hold fixture must be accepted: #{created_error}")
  assert(created_events.empty?, "created-state safe-hold fixture must not stop containers")
end
runtime_start = quiesce.index('test "$initial_watcher_state" = running')
runtime_watcher_stop = quiesce.index("docker stop gluetun-rezka-watcher", runtime_start)
runtime_rotation = quiesce.index("rotation_marked=1", runtime_start)
runtime_policy_update = quiesce.index("docker update --restart=no gluetun-rezka-watcher", runtime_start)
assert(quiesce.include?("lifecycle_target") &&
       quiesce.include?("write_lifecycle rotating") &&
       quiesce.include?("docker stop gluetun-rezka-watcher") &&
       runtime_rotation && runtime_watcher_stop && runtime_rotation < runtime_watcher_stop,
       "full quiescence must authenticate lifecycle rotating before stopping the watcher")
assert(quiesce.include?("docker update --restart=no gluetun-rezka-watcher") &&
       runtime_policy_update && runtime_rotation && runtime_policy_update < runtime_rotation,
       "quiescence must disable the watcher restart policy before lifecycle fencing")
runner_policy_update = quiesce.index("runner_restart_policy_disabled=1", runtime_start)
assert(quiesce.include?("runner_restart_policy_disabled=1") &&
       quiesce.include?("could not disable download-runner restart policy") &&
       runner_policy_update && runtime_rotation && runtime_policy_update &&
       runner_policy_update < runtime_policy_update && runner_policy_update < runtime_rotation,
       "full quiescence must disable the download-runner restart policy before lifecycle fencing")
assert(quiesce.include?("docker run --rm") &&
       quiesce.include?("--network container:media-service") &&
       quiesce.include?("lifecycle_token_source") &&
       !quiesce.include?("docker exec gluetun-rezka-watcher") &&
       quiesce.index("write_lifecycle rotating") < quiesce.index("docker stop gluetun-rezka-watcher") &&
       quiesce.rindex("docker stop gluetun-rezka-watcher") < quiesce.rindex("write_lifecycle rotating"),
       "quiescence must re-assert rotating from an isolated helper after the watcher is fully stopped")
watcher_stop = runtime_watcher_stop
post_stop_rotate = quiesce.index("write_lifecycle rotating", watcher_stop)
post_stop_long_fence = quiesce.index("consume_watcher_fence \"$fence_lifecycle\" \"$watcher_restart_drain_checks\"", watcher_stop)
full_order = quiesce.split("if test \"$lifecycle_target\" = full; then\n    # Stop the only HTTP lifecycle writer", 2).fetch(1, "")
stop_media_index = full_order.index("stop_media_for_full_hold")
consume_index = full_order.index("consume_watcher_fence")
normalize_index = full_order.index("normalize_full_runner")
normalizer = quiesce.split("normalize_full_runner() {", 2).fetch(1, "").split("full_safe_hold() {", 2).first
assert(quiesce.include?("wait_watcher_fence() {") &&
       quiesce.include?("normalize_full_runner() {") &&
       quiesce.include?("current_runner_state=$(docker inspect download-runner") &&
       quiesce.include?("watcher_restart_drain_checks=65") &&
       quiesce.include?("watcher_fence_checks=3") &&
       quiesce.include?("runner watcher resurrected during quiescence") &&
       quiesce.include?("observed_lifecycle_state=$(lifecycle_state)") &&
       quiesce.include?("consume_watcher_fence() {") &&
       quiesce.include?("consume_watcher_fence \"$fence_lifecycle\" \"$watcher_restart_drain_checks\"") &&
       quiesce.include?("max_resurrections=${3:-3}") &&
       quiesce.include?("max_observations=$((checks * 2))") &&
       quiesce.include?("wait_watcher_fence \"$fence_lifecycle\" \"$watcher_fence_checks\"") &&
       watcher_stop && post_stop_rotate && post_stop_long_fence &&
       watcher_stop < post_stop_rotate && post_stop_rotate < post_stop_long_fence &&
       stop_media_index && consume_index && normalize_index &&
       stop_media_index < consume_index && consume_index < normalize_index &&
       normalizer.include?("docker stop download-runner") &&
       normalizer.include?("write_lifecycle rotating"),
       "full quiescence must stop media before draining the watcher and normalize the current runner under the rotating fence")
watcher_fence = quiesce.split("wait_watcher_fence() {", 2).fetch(1).split("full_safe_hold() {", 2).fetch(0)
watcher_resurrection_probe = <<~SH
  set -eu
  resurrected_marker=$(mktemp)
  rm -f "$resurrected_marker"
  trap 'rm -f "$resurrected_marker"' EXIT HUP INT TERM
  docker() {
    if test "$1" = inspect; then
      if test ! -e "$resurrected_marker"; then
        : >"$resurrected_marker"
        printf '%s\n' exited
      else
        printf '%s\n' running
      fi
    elif test "$1" = exec; then
      printf '%s\n' rotating
    else
      exit 2
    fi
  }
  #{"wait_watcher_fence() {" + watcher_fence}
  wait_watcher_fence rotating 3
SH
_output, fence_error, fence_status = Open3.capture3("sh", "-c", watcher_resurrection_probe)
assert(!fence_status.success?, "watcher stop fence must fail closed when a pending restart resurrects the watcher: #{fence_error}")
lifecycle_race_probe = watcher_resurrection_probe
  .sub("if test ! -e \"$resurrected_marker\"; then\n        : >\"$resurrected_marker\"\n        printf '%s\\n' exited\n      else\n        printf '%s\\n' running\n      fi", "printf '%s\\n' exited")
  .sub("printf '%s\\n' rotating", "if test ! -e \"$resurrected_marker\"; then\n        : >\"$resurrected_marker\"\n        printf '%s\\n' rotating\n      else\n        printf '%s\\n' ready\n      fi")
_output, lifecycle_error, lifecycle_status = Open3.capture3("sh", "-c", lifecycle_race_probe)
assert(!lifecycle_status.success?, "watcher stop fence must fail closed when lifecycle briefly returns ready: #{lifecycle_error}")
consuming_fence = quiesce.split("consume_watcher_fence() {", 2).fetch(1).split("full_safe_hold() {", 2).fetch(0)
consuming_fence_probe = <<~SH
  set -eu
  lifecycle_target=full
  lifecycle_state_value=ready
  watcher_state_value=exited
  runner_state_value=running
  watcher_restart_marker=$(mktemp)
  rm -f "$watcher_restart_marker"
  trap 'rm -f "$watcher_restart_marker"' EXIT HUP INT TERM
  restart_policy_value=unless-stopped
  lifecycle_writes=0
  active_job_count() { printf '%s\\n' 0; }
  lifecycle_state() {
    printf '%s\n' "$lifecycle_state_value"
  }
  write_lifecycle() {
    test "$1" = rotating
    lifecycle_state_value=rotating
    lifecycle_writes=$((lifecycle_writes + 1))
  }
  docker() {
    case "$1" in
      inspect)
        case "$4" in
          *State.Status*)
            if test "$2" = download-runner; then
              printf '%s\\n' "$runner_state_value"
            elif test "${perpetual_resurrection:-0}" = 1; then
              printf '%s\\n' running
            elif test ! -e "$watcher_restart_marker"; then
              : >"$watcher_restart_marker"
              printf '%s\\n' running
            else
              printf '%s\\n' "$watcher_state_value"
            fi
            ;;
          *RestartPolicy.Name*) printf '%s\\n' "$restart_policy_value" ;;
          *) exit 2 ;;
        esac
        ;;
      exec) printf '%s\\n' "$lifecycle_state_value" ;;
      update) restart_policy_value=no ;;
      stop)
        test "$restart_policy_value" = no
        if test "$2" = download-runner; then
          runner_state_value=exited
        else
          watcher_state_value=exited
        fi
        ;;
      *) exit 2 ;;
    esac
  }
  #{"consume_watcher_fence() {" + consuming_fence}
  consume_watcher_fence rotating 3 3
  wait_watcher_fence() { :; }
  #{"normalize_full_runner() {" + quiesce.split("normalize_full_runner() {", 2).fetch(1).split("full_safe_hold() {", 2).fetch(0)}
  normalize_full_runner
  test "$restart_policy_value" = no
  test "$lifecycle_state_value" = rotating
  test "$watcher_state_value" = exited
  test "$runner_state_value" = exited
  test "$lifecycle_writes" -ge 1
SH
_output, consuming_error, consuming_status = Open3.capture3("sh", "-c", consuming_fence_probe)
assert(consuming_status.success?, "consuming watcher fence must absorb a queued restart and reach stability: #{consuming_error}")
perpetual_resurrection_probe = consuming_fence_probe
  .sub("set -eu", "set -eu\n  perpetual_resurrection=1")
  .sub("consume_watcher_fence rotating 3 3", "if consume_watcher_fence rotating 3 2; then exit 1; fi\n  test \"$restart_policy_value\" = no")
_output, perpetual_error, perpetual_status = Open3.capture3("sh", "-c", perpetual_resurrection_probe)
assert(perpetual_status.success?, "consuming watcher fence must bound perpetual resurrection while retaining restart policy no: #{perpetual_error}")
restore_runtime = quiesce.split("restore_runtime() {", 2).fetch(1).split("trap restore_runtime", 2).fetch(0)
assert(restore_runtime.index("restore_media_runtime") < restore_runtime.index("start_container download-runner") &&
       restore_runtime.index("start_container download-runner") < restore_runtime.index("start_container gluetun-rezka-watcher") &&
       restore_runtime.index("start_container gluetun-rezka-watcher") < restore_runtime.index("write_lifecycle ready"),
       "quiescence recovery must restore media, runner, and watcher before opening the ready lifecycle gate")
assert(quiesce.include?("initial_watcher_state") &&
       !restore_runtime.include?("test \"$watcher_state\" = running") &&
       quiesce.include?("restore_on_exit()") && quiesce.include?("restore_on_signal()") &&
       quiesce.include?("trap restore_on_exit EXIT") &&
       quiesce.include?("trap restore_on_signal HUP INT TERM") &&
       !quiesce.include?("trap restore_runtime EXIT HUP INT TERM"),
       "quiescence recovery must preserve captured container state and exit after signal recovery")
restore_runtime_function = quiesce.split("restore_runtime() {", 2).fetch(1).split("restore_on_exit() {", 2).fetch(0)
clobber_probe = <<~SH
  set -eu
  initial_runner_state=exited
  initial_watcher_state=running
  runtime_restored=0
  lifecycle_target=service
  rotation_marked=0
  restart_policy_disabled=0
  start_container() { printf '%s\\n' "started:$1"; }
  restore_watcher_restart_policy() { :; }
  docker() {
    test "$1" = inspect || exit 2
    printf '%s\\n' exited
  }
  #{"restore_runtime() {" + restore_runtime_function}
  restore_runtime
SH
clobber_output, clobber_error, clobber_status = Open3.capture3("sh", "-c", clobber_probe)
assert(clobber_status.success? && clobber_output.lines.map(&:strip) == ["started:gluetun-rezka-watcher"],
       "quiescence recovery must restart a captured running watcher even after stop-fence polling: #{clobber_error}")
recovery_fence = safe_hold_recovery.split("wait_watcher_fence() {", 2).fetch(1).split("session_volume() {", 2).fetch(0)
watcher_fence_function = "wait_watcher_fence() {" + watcher_fence
recovery_fence_function = "wait_watcher_fence() {" + recovery_fence
recovery_fence_probe = watcher_resurrection_probe
  .sub(watcher_fence_function, recovery_fence_function)
  .sub("wait_watcher_fence rotating 3", "wait_watcher_fence 3")
_output, recovery_fence_error, recovery_fence_status = Open3.capture3("sh", "-c", recovery_fence_probe)
assert(!recovery_fence_status.success?, "full recovery stop fence must reject watcher resurrection: #{recovery_fence_error}")
signal_handlers = quiesce.split("restore_on_exit() {", 2).fetch(1).split("test \"$initial_watcher_state\" = running", 2).fetch(0)
signal_probe = <<~SH
  set -eu
  restore_runtime() { printf '%s\\n' restored >&2; }
  #{"restore_on_exit() {" + signal_handlers}
  trap restore_on_exit EXIT
  trap restore_on_signal HUP INT TERM
  kill -TERM $$
  printf '%s\\n' continued >&2
SH
_output, signal_error, signal_status = Open3.capture3("sh", "-c", signal_probe)
assert(!signal_status.success? && signal_error.lines.map(&:strip) == ["restored"],
       "quiescence signal recovery must restore once and terminate without continuation: #{signal_error}")
assert(quiesce.include?("cleanup_full_safe_hold_body() {") &&
       quiesce.include?("cleanup_full_safe_hold_exit() {") &&
       quiesce.include?("cleanup_full_safe_hold_signal() {") &&
       quiesce.include?("trap cleanup_full_safe_hold_exit EXIT") &&
       quiesce.include?("trap cleanup_full_safe_hold_signal HUP INT TERM"),
       "full safe-hold cleanup must split EXIT status preservation from signal termination")
fast_cleanup_body = quiesce.split("cleanup_full_safe_hold_body() {", 2).fetch(1).split("cleanup_full_safe_hold_exit() {", 2).fetch(0)
fast_cleanup_exit = quiesce.split("cleanup_full_safe_hold_exit() {", 2).fetch(1).split("cleanup_full_safe_hold_signal() {", 2).fetch(0)
fast_cleanup_signal = quiesce.split("cleanup_full_safe_hold_signal() {", 2).fetch(1).split("if full_safe_hold", 2).fetch(0)
fast_signal_probe = <<~SH
  set -eu
  active_job_count() { printf '%s\\n' 0; }
  docker() {
    case "$1" in
      inspect) printf '%s\\n' running ;;
      stop) : >"${FAST_CLEANUP_MARKER:?}" ;;
      *) exit 2 ;;
    esac
  }
  #{"cleanup_full_safe_hold_body() {" + fast_cleanup_body}
  #{"cleanup_full_safe_hold_exit() {" + fast_cleanup_exit}
  #{"cleanup_full_safe_hold_signal() {" + fast_cleanup_signal}
  trap cleanup_full_safe_hold_signal HUP INT TERM
  kill -TERM $$
  printf '%s\\n' continued >&2
SH
Dir.mktmpdir("fast-safe-hold-signal") do |directory|
  marker = File.join(directory, "stopped")
  _output, fast_signal_error, fast_signal_status = Open3.capture3(
    {"FAST_CLEANUP_MARKER" => marker}, "sh", "-c", fast_signal_probe
  )
  assert(!fast_signal_status.success? && File.file?(marker) && !fast_signal_error.include?("continued"),
         "full safe-hold signal cleanup must exit nonzero without continuation: #{fast_signal_error}")
end
assert(fast_cleanup_body.include?("fast_safe_hold_reached") &&
       fast_cleanup_body.include?("initial_media_restart_policy") &&
       fast_cleanup_body.include?("initial_watcher_restart_policy") &&
       fast_cleanup_body.include?("initial_runner_restart_policy") &&
       fast_cleanup_body.include?("write_lifecycle_direct_ready") &&
       fast_cleanup_body.include?("cleanup_session_volume") &&
       !fast_cleanup_body.include?("docker start download-runner"),
       "full safe-hold cleanup must restore policies and lifecycle without launching the runner")
resume_contract = homelab.split("resume_runner_watcher_and_wait_ready() {", 2).fetch(1).split("hold_runner_quiescence() {", 2).fetch(0)
assert(resume_contract.include?("watcher_restart_policy=$1") &&
       resume_contract.include?("docker update --restart=\"$watcher_restart_policy\" gluetun-rezka-watcher") &&
       resume_contract.index("docker update --restart=\"$watcher_restart_policy\" gluetun-rezka-watcher") < resume_contract.index("start_container gluetun-rezka-watcher") &&
       resume_contract.include?("state=$(docker inspect \"$container\" --format '{{.State.Status}}')") &&
       resume_contract.include?("test \"$state\" = running || return 1"),
       "runner resume must restore the watcher restart policy and tolerate an idempotent start")
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
full_compatibility = homelab.split("verify_full_runtime_compatibility() {", 2).fetch(1).split("image_suffix() {", 2).fetch(0)
assert(full_compatibility.include?("previous_rezka_id") &&
       full_compatibility.include?("previous_watcher_id") &&
       full_compatibility.include?("REZKA_PROBE_IMAGE") &&
       full_compatibility.include?("DOWNLOAD_RUNNER_IMAGE") &&
       full_compatibility.include?("session volume changed") &&
       full_compatibility.include?("State.Health.Status") &&
       full_compatibility.include?("rezka_mode") &&
       full_compatibility.include?("unchanged contract"),
       "full runtime compatibility must prove replacement IDs (or intentional gluetun-rezka skip), watcher probe image, health, and session volume")
replace_images = homelab.split("replace_images() {", 2).fetch(1).split("replace_service_image() {", 2).fetch(0)
resume_runner = homelab.split("resume_runner_watcher_and_wait_ready() {", 2).fetch(1).split("hold_runner_quiescence() {", 2).fetch(0)
assert(replace_images.include?("docker compose") &&
       replace_images.include?("up --no-deps --force-recreate --no-start download-runner") &&
       !replace_images.include?("up -d --no-deps --force-recreate download-runner") &&
       resume_runner.include?("start_container download-runner"),
       "full replacement must create the runner stopped and start it only during guarded resume")
assert(full_rollback.include?('replace_full_runtime_safe_hold "$forward_service_image" "$forward_runner_image"') &&
       full_rollback.include?("restoring the forward full stack"),
       "failed full rollback must automatically restore the exact forward stack in a held replacement")
assert(full_rollback.include?("quiesce_runner") &&
       full_contract.scan("resume_runner_watcher_and_wait_ready").length >= 1 &&
       full_contract.scan("verify_resumed_runtime_or_requiesce").length >= 1 &&
       full_rollback.include?("recovery_failed"),
       "full rollback must hold quiescence through the successful result")
full_rollback_recovery = full_rollback.split("echo \"full rollback failed; restoring the forward full stack\"", 2).fetch(1)
assert(full_rollback_recovery.include?("restore_full_runtime_safe_hold \"$session_volume\"") &&
       full_rollback_recovery.include?("replace_full_runtime_safe_hold \"$forward_service_image\" \"$forward_runner_image\"") &&
       full_rollback_recovery.include?("runtime remains in safe hold") &&
       !full_rollback_recovery.include?("replace_full_runtime \"$forward_service_image\" \"$forward_runner_image\"") &&
       !full_rollback_recovery.include?("verify_live_mcp_schema") &&
       !full_rollback_recovery.include?("resume_runner_watcher_and_wait_ready") &&
       !full_rollback_recovery.include?("verify_resumed_runtime_or_requiesce"),
       "failed full rollback must restore the forward sources/images and leave the runtime in an exact safe hold")
assert(full_rollback_recovery.scan('if test "$recovery_failed" = 0; then').length >= 7 &&
       full_rollback_recovery.index("restore_full_runtime_safe_hold") < full_rollback_recovery.index("restore_forward_deployment_sources") &&
       full_rollback_recovery.index("restore_forward_deployment_sources") < full_rollback_recovery.index("forward_schema") &&
       full_rollback_recovery.index("forward_schema") < full_rollback_recovery.index("replace_full_runtime_safe_hold"),
       "failed full rollback recovery must gate every restore step on a successful safe hold")
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
%w[media-postgres gluetun qbittorrent].each do |container|
  assert(full_protected.include?(container), "full rollback must preserve #{container} container identity")
end
assert(full_rollback.include?("previous_rezka_id") &&
       full_rollback.include?("previous_watcher_id") &&
       full_contract.include?("verify_full_runtime_compatibility"),
       "full rollback must recreate and verify the dedicated VPN and watcher generation")
assert(!full_protected.include?("gluetun-rezka"),
       "full protected snapshot must exclude the intentionally replaced dedicated VPN")
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
       full_contract.scan('verify_resumed_runtime_or_requiesce "$protected_before" assert_full_protected_unchanged').length >= 1,
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
assert(normalized_runbook.include?("main `gluetun`") &&
       normalized_runbook.include?("dedicated `gluetun-rezka`, runner, and watcher") &&
       normalized_runbook.include?("bounded watcher-readiness") &&
       normalized_runbook.include?("re-quiesces"),
       "runbook must name the protected Gluetun set and bounded watcher rollback behavior")
assert(normalized_runbook.include?("project: homelab") &&
       normalized_runbook.include?("--project-name homelab") &&
       normalized_runbook.include?("Do not invoke Compose from `media/` or `hermes/`"),
       "runbook must document the root homelab Compose project")
assert(normalized_runbook.include?("sha256:<image-id>") &&
       normalized_runbook.include?("not registry pull references") &&
       normalized_runbook.include?("portable `registry@sha256` references"),
       "runbook must distinguish host-local image IDs from portable release references")
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
}
_stdout, stderr, status = Open3.capture3(clean_env, "./scripts/homelab.sh", "invalid-command", chdir: ROOT, unsetenv_others: true)
assert(status.exitstatus == 2 && stderr.include?("usage:"),
       "homelab command parsing must work in a clean environment without unbound variables")

puts "packaging architecture checks passed"
