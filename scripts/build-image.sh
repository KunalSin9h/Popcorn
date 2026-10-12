#!/bin/sh
# Build the golden image: guest/Dockerfile -> a read-only ext4 root filesystem for the microVMs.
# Runs on the host with Docker; the final mkfs runs as root inside a throwaway container so
# file ownership and setuid bits (sudo, su) survive.
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
out=${POPCORN_HOME:-$HOME/.local/share/popcorn}
mkdir -p "$out"
docker build -t popcorn-golden "$here/guest"
id=$(docker create popcorn-golden)
trap 'docker rm -f "$id" >/dev/null 2>&1 || true' EXIT
docker export "$id" | docker run --rm -i -v "$out:/out" debian:bookworm-slim sh -euc '
  apt-get update -qq >/dev/null && apt-get install -y -qq e2fsprogs >/dev/null
  mkdir /r && tar -x -C /r --numeric-owner
  rm -f /r/.dockerenv
  size=$(( $(du -sm /r | cut -f1) * 13 / 10 + 256 ))
  rm -f /out/golden.ext4.new
  mkfs.ext4 -q -L popcorn -d /r /out/golden.ext4.new ${size}M
  chown '"$(id -u):$(id -g)"' /out/golden.ext4.new
'
mv "$out/golden.ext4.new" "$out/golden.ext4"
echo "golden image: $out/golden.ext4 ($(du -h "$out/golden.ext4" | cut -f1))"
