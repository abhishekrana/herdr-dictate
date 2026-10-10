#!/bin/sh
# Build dependencies for Debian and Ubuntu.
#
# libopenblas-dev is optional: a source build links it when present, and on
# dictation-length clips it made no measurable difference.
set -eu

SUDO=sudo
[ "$(id -u)" -eq 0 ] && SUDO=

$SUDO apt-get update
$SUDO apt-get install -y --no-install-recommends \
    build-essential \
    cmake \
    glslc \
    libasound2-dev \
    libvulkan-dev \
    pkg-config \
    spirv-headers
