#!/bin/sh
# Install the binary: fetch the release build for this version, or compile it.
#
# Run by herdr-plugin.toml's [[build]]. A fetch saves a C++ build and the
# toolchain it needs, and carries GPU support the source build would not have.
# Anything unexpected falls through to cargo, which always works.
set -eu

version=$(grep -m1 '^version = ' Cargo.toml | cut -d'"' -f2)
target=$(uname -m)-$(uname -s)
out=target/release/herdr-dictate

# Build with the GPU backend when this machine can, since that is roughly eight
# times faster; otherwise plain, which builds anywhere.
build() {
    if pkg-config --exists vulkan 2>/dev/null && command -v glslc >/dev/null 2>&1; then
        echo "building from source, with the vulkan backend"
        exec cargo build --release --locked --features vulkan
    fi
    echo "building from source"
    exec cargo build --release --locked
}

[ "$target" = "x86_64-Linux" ] || build

name="herdr-dictate-$version-x86_64-unknown-linux-gnu"
base="https://github.com/abhishekrana/herdr-dictate/releases/download/v$version"

command -v curl >/dev/null 2>&1 || build
command -v sha256sum >/dev/null 2>&1 || build

# The release build links libvulkan; without it the binary would not start,
# while a source build runs anywhere.
ldconfig -p 2>/dev/null | grep -q 'libvulkan\.so\.1' || build

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

curl -fsSL "$base/$name.tar.gz" -o "$tmp/archive.tar.gz" 2>/dev/null || build
curl -fsSL "$base/$name.tar.gz.sha256" -o "$tmp/archive.sha256" 2>/dev/null || build

# The published checksum names the file; check against what was downloaded.
expected=$(cut -d' ' -f1 <"$tmp/archive.sha256")
actual=$(sha256sum "$tmp/archive.tar.gz" | cut -d' ' -f1)
if [ "$expected" != "$actual" ]; then
    echo "checksum mismatch for $name.tar.gz: expected $expected, got $actual" >&2
    build
fi

tar -xzf "$tmp/archive.tar.gz" -C "$tmp" "$name/herdr-dictate" || build
mkdir -p "$(dirname "$out")"
cp "$tmp/$name/herdr-dictate" "$out"
chmod +x "$out"
echo "installed the v$version release build"
