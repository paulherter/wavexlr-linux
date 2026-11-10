#!/usr/bin/env bash
# Start test of dist/wavexlr-x86_64-linux in clean containers of other distributions.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
engine="$(command -v podman || command -v docker)"
run() { # image, install command
  echo "=== $1"
  "$engine" run --rm -v "$here/dist:/dist:ro,Z" "$1" bash -c "
    $2 >/dev/null 2>&1
    cp /dist/wavexlr-x86_64-linux /tmp/wavexlr && chmod +x /tmp/wavexlr
    echo -n 'missing libraries: '; ldd /tmp/wavexlr | grep -c 'not found' || true
    /tmp/wavexlr --version
    export XDG_RUNTIME_DIR=/tmp/run HOME=/tmp/home; mkdir -p \$XDG_RUNTIME_DIR \$HOME
    /tmp/wavexlr --headless & pid=\$!; sleep 3
    if kill -0 \$pid 2>/dev/null; then echo 'daemon runs (no device: waiting)'; kill \$pid; wait \$pid 2>/dev/null; echo \"exit code \$?\"; else echo 'DAEMON DID NOT STAY UP'; fi
  " 2>&1 | grep -v "^$"
}
[ -z "${ONLY:-}" ] || [ "$ONLY" = ubuntu22 ] && run ubuntu:22.04  "apt-get update -qq && apt-get install -y -qq libasound2"
run ubuntu:24.04  "apt-get update -qq && apt-get install -y -qq libasound2t64"
run debian:12     "apt-get update -qq && apt-get install -y -qq libasound2"
run fedora:latest "dnf install -y -q alsa-lib"
run archlinux:latest "pacman -Sy --noconfirm alsa-lib"
echo "=== opensuse/tumbleweed"; run opensuse/tumbleweed "zypper -q -n install alsa-lib"
