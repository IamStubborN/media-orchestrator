#!/bin/sh
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

baseline=$("$root/scripts/docker-build.sh" --print-runner-build-digest)
printf '%s\n' "$baseline" | grep -Eq '^[0-9a-f]{64}$'

# Crate test-only edits must not change the runner digest.
probe=crates/media-api/tests/.runner-digest-probe
printf 'probe\n' >"$probe"
trap 'rm -f "$probe"' EXIT HUP INT TERM
after_test=$("$root/scripts/docker-build.sh" --print-runner-build-digest)
test "$baseline" = "$after_test" || {
  echo "FAIL: crate test file changed runner digest" >&2
  exit 1
}
rm -f "$probe"
trap - EXIT HUP INT TERM

# Source edits must change the runner digest (shared binary honesty).
probe_src=crates/media-api/src/.runner-digest-probe.rs
printf '// probe\n' >"$probe_src"
trap 'rm -f "$probe_src"' EXIT HUP INT TERM
after_src=$("$root/scripts/docker-build.sh" --print-runner-build-digest)
test "$baseline" != "$after_src" || {
  echo "FAIL: crate src file did not change runner digest" >&2
  exit 1
}
rm -f "$probe_src"
trap - EXIT HUP INT TERM

# deny.toml noise must not change the runner digest.
if test -f deny.toml; then
  cp deny.toml deny.toml.runner-digest.bak
  trap 'mv deny.toml.runner-digest.bak deny.toml' EXIT HUP INT TERM
  printf '\n# runner-digest-probe\n' >>deny.toml
  after_deny=$("$root/scripts/docker-build.sh" --print-runner-build-digest)
  test "$baseline" = "$after_deny" || {
    echo "FAIL: deny.toml changed runner digest" >&2
    exit 1
  }
  mv deny.toml.runner-digest.bak deny.toml
  trap - EXIT HUP INT TERM
fi

context=$("$root/scripts/docker-build.sh" --print-source-tree-digest)
test "$baseline" != "$context" || {
  echo "FAIL: narrowed runner digest still equals full context digest" >&2
  exit 1
}

echo "PASS: runner digest ignores tests/noise and tracks shared binary src"
