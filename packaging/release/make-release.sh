#!/usr/bin/env bash
# Assembles the files of a release in dist/: binary (built in an old-glibc container), support files, checksums, installer.
# It does not publish anything; the last line prints the command for that.
#   packaging/release/make-release.sh
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$here"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
[[ -f dist/wavexlr-x86_64-linux ]] || packaging/release/build.sh
stage="$(mktemp -d)"; trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/support"
cp packaging/70-wavexlr.rules packaging/wavexlr.desktop packaging/wavexlr-autostart.desktop "$stage/support/"
cp assets/wavexlr.svg "$stage/support/"
cp packaging/wireplumber/51-wavexlr.conf packaging/wireplumber/wavedevicefix.lua packaging/wireplumber/LICENSE-wavexlr-on-linux-cfg "$stage/support/"
tar -C "$stage" -czf dist/wavexlr-support.tar.gz support
cp packaging/release/install.sh dist/install.sh
(cd dist && sha256sum wavexlr-x86_64-linux wavexlr-support.tar.gz > SHA256SUMS && cat SHA256SUMS)
echo
echo "files in dist/: $(ls dist | tr '\n' ' ')"
echo "publish (only when told to): gh release create v$version dist/wavexlr-x86_64-linux dist/wavexlr-support.tar.gz dist/SHA256SUMS dist/install.sh --repo paulherter/wavexlr-linux --title $version --notes ''"
