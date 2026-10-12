#!/bin/sh
# Download Firecracker (and its jailer) and a guest kernel into ~/.local/share/popcorn.
# Run once per machine. No root needed.
set -eu
out=${POPCORN_HOME:-$HOME/.local/share/popcorn}
mkdir -p "$out/bin"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

v=$(curl -fsSL https://api.github.com/repos/firecracker-microvm/firecracker/releases/latest | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p')
echo "Firecracker $v"
curl -fsSL "https://github.com/firecracker-microvm/firecracker/releases/download/$v/firecracker-$v-x86_64.tgz" | tar -xz -C "$tmp"
cp "$tmp/release-$v-x86_64/firecracker-$v-x86_64" "$out/bin/firecracker"
cp "$tmp/release-$v-x86_64/jailer-$v-x86_64" "$out/bin/jailer"

# The newest CI kernel published for a recent release line. Needs vsock, overlayfs and ext4.
kernel=v1.15/x86_64/vmlinux-6.1.155
curl -fsSL -o "$out/vmlinux" "https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/$kernel"
echo "Kernel $kernel"
"$out/bin/firecracker" --version | head -1
