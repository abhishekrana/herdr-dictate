#!/bin/sh
# Make this working tree the plugin Herdr runs, then report what it sees.
#
# Refuses anything uncommitted or unpushed: what is live has to be what is on
# the branch, or a later bisect is reading a build nobody can reproduce.
set -eu

cd "$(dirname "$0")/.."

[ -z "$(git status --porcelain)" ] || {
    echo "working tree is dirty: commit before deploying" >&2
    exit 1
}
git fetch --quiet
[ "$(git rev-list --count '@{u}..HEAD')" = "0" ] || {
    echo "unpushed commits: push before deploying" >&2
    exit 1
}

# The GPU build wherever the toolchain allows it, matching what an install does.
if pkg-config --exists vulkan 2>/dev/null && command -v glslc >/dev/null 2>&1; then
    cargo build --release --locked --features vulkan
else
    cargo build --release --locked
fi

# Herdr reads the manifest when it registers a plugin, so re-register rather
# than reason about which change needs it.
id=$(grep -m1 '^id = ' herdr-plugin.toml | cut -d'"' -f2)
herdr plugin unlink "$id" >/dev/null 2>&1 || true
herdr plugin link . >/dev/null

# The resident server holds the old binary until it idles out.
./target/release/herdr-dictate serve-stop >/dev/null 2>&1 || true

echo
./target/release/herdr-dictate doctor
