#!/bin/sh
# Build dependencies for Debian and Ubuntu.
set -eu

SUDO=sudo
[ "$(id -u)" -eq 0 ] && SUDO=

$SUDO apt-get update
$SUDO apt-get install -y --no-install-recommends \
    build-essential \
    cmake \
    glslc \
    libasound2-dev \
    libclang-dev \
    libvulkan-dev \
    pkg-config
