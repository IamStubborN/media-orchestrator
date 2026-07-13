#!/usr/bin/env ruby
# frozen_string_literal: true

require "yaml"

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

%w[postgres migrate service runner].each do |name|
  assert(services.key?(name), "compose stack is missing #{name}")
end

service = services.fetch("service")
runner = services.fetch("runner")
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

puts "packaging architecture checks passed"
