#!/bin/sh
# Tools scripts/check.sh and scripts/release.sh need, beyond a Rust toolchain.
# Everything here installs under ~/.cargo and ~/.rustup; no sudo, no system
# packages. Build dependencies are separate: scripts/install-deps.sh.
set -eu

# shellcheck source=scripts/versions.env
. "$(dirname "$0")/versions.env"

command -v rustup >/dev/null 2>&1 || {
    echo "rustup is required: https://rustup.rs" >&2
    exit 1
}

# The toolchain lints run under, pinned so its clippy matches CI's.
rustup toolchain install "$RUST_VERSION" --profile minimal --no-self-update
rustup component add --toolchain "$RUST_VERSION" rustfmt clippy
rustup component add rustfmt clippy

# The MSRV toolchain, so the minimum this crate claims can be checked locally.
msrv=$(grep -m1 '^rust-version = ' Cargo.toml | cut -d'"' -f2)
rustup toolchain install "$msrv" --profile minimal --no-self-update

cargo install --locked cargo-deny --version "$CARGO_DENY_VERSION"
cargo install --locked git-cliff --version "$GIT_CLIFF_VERSION"

echo
echo "Installed. Optional, checked by scripts/check.sh when present:"
echo "  shellcheck  (apt install shellcheck)"
echo "  gitleaks $GITLEAKS_VERSION (https://github.com/gitleaks/gitleaks/releases)"
echo "  docker      (for the build-from-source check)"
