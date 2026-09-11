#!/bin/sh
# The gate. CI runs this too, so passing it locally means CI passes.
#
# Optional tools are reported as skipped rather than failing, so the script
# works on a fresh clone. scripts/install-dev-tools.sh installs them.
set -eu

skipped=""
skip() { skipped="$skipped  $1 - $2
"; }
step() { printf '\n\033[1m── %s\033[0m\n' "$1"; }

# shellcheck source=scripts/versions.env
. "$(dirname "$0")/versions.env"

msrv=$(grep -m1 '^rust-version = ' Cargo.toml | cut -d'"' -f2)

step "manifest versions"
cargo_version=$(grep -m1 '^version = ' Cargo.toml | cut -d'"' -f2)
plugin_version=$(grep -m1 '^version = ' herdr-plugin.toml | cut -d'"' -f2)
if [ "$cargo_version" != "$plugin_version" ]; then
    echo "Cargo.toml $cargo_version != herdr-plugin.toml $plugin_version" >&2
    exit 1
fi
echo "$cargo_version"

# Pinned so local and CI lint identically. Tests use the installed stable.
step "lint toolchain $RUST_VERSION"
rustup toolchain list 2>/dev/null | grep -q "^$RUST_VERSION" || {
    echo "not installed: scripts/install-dev-tools.sh" >&2
    exit 1
}
cargo "+$RUST_VERSION" --version

step "fmt"
cargo "+$RUST_VERSION" fmt --all --check

step "clippy"
cargo "+$RUST_VERSION" clippy --all-targets -- -D warnings

step "doc"
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --quiet

step "test"
cargo test --locked --quiet

if command -v cargo-deny >/dev/null 2>&1; then
    step "deny"
    cargo deny --all-features check
else
    skip "deny" "cargo install --locked cargo-deny"
fi

if command -v shellcheck >/dev/null 2>&1; then
    step "shellcheck"
    shellcheck -x scripts/*.sh
else
    skip "shellcheck" "apt install shellcheck"
fi

if command -v gitleaks >/dev/null 2>&1; then
    step "secrets"
    gitleaks dir . --config .gitleaks.toml --no-banner --redact
    gitleaks git . --config .gitleaks.toml --no-banner --redact
else
    skip "secrets" "https://github.com/gitleaks/gitleaks/releases"
fi

if rustup toolchain list 2>/dev/null | grep -q "^$msrv"; then
    step "msrv $msrv"
    cargo "+$msrv" check --locked --quiet
else
    skip "msrv $msrv" "rustup toolchain install $msrv"
fi

if [ "${SKIP_DOCKER:-}" != "1" ] && command -v docker >/dev/null 2>&1; then
    step "build from source"
    docker build -q -f test/Dockerfile -t herdr-dictate-src . >/dev/null
else
    skip "build from source" "docker, or SKIP_DOCKER=1"
fi

printf '\n\033[1mpassed\033[0m\n'
if [ -n "$skipped" ]; then
    printf 'skipped:\n%s' "$skipped"
fi
