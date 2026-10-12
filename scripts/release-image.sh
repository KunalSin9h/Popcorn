#!/bin/sh
# Build the workbench image for a release: dist/golden-<version>.ext4.zst and its SHA-256.
# package.sh compiles that hash into the app, so the app accepts only this exact image.
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/Cargo.toml" | head -1)
dist="$here/dist"
mkdir -p "$dist"
POPCORN_HOME="$dist/build" "$here/scripts/build-image.sh"
zstd -q -19 -T0 -f "$dist/build/golden.ext4" -o "$dist/golden-$version.ext4.zst"
rm -rf "$dist/build"
(cd "$dist" && sha256sum "golden-$version.ext4.zst" > "golden-$version.ext4.zst.sha256")
cat "$dist/golden-$version.ext4.zst.sha256"
