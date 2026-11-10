#!/usr/bin/env bash
# Builds the release binary in an old Ubuntu (glibc 2.35), so it runs on every current Linux desktop.
# Needs podman (or docker). Output: dist/wavexlr-x86_64-linux and its checksum.
#   packaging/release/build.sh
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
engine="$(command -v podman || command -v docker)" || { echo "podman or docker is needed" >&2; exit 1; }
mkdir -p "$here/dist"
"$engine" run --rm -v "$here:/src:ro,Z" -v "$here/dist:/out:Z" ubuntu:22.04 bash -euc '
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends build-essential pkg-config curl ca-certificates libasound2-dev >/dev/null
curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable >/dev/null
. "$HOME/.cargo/env"
mkdir -p /build && cd /build && tar -C /src --exclude=target --exclude=dist --exclude=.git -cf - . | tar -xf -
cargo build --release --locked
cp target/release/wavexlr /out/wavexlr-x86_64-linux
echo "highest glibc symbol:"; objdump -T target/release/wavexlr | grep -o "GLIBC_[0-9.]*" | sort -V | tail -1
echo "libraries:"; ldd target/release/wavexlr
'
cd "$here/dist" && sha256sum wavexlr-x86_64-linux > wavexlr-x86_64-linux.sha256
echo "built $(ls -la wavexlr-x86_64-linux | awk '{print $5}') bytes"
