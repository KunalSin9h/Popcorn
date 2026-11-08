#!/bin/sh
# Download Firecracker (and its jailer) and a guest kernel into ~/.local/share/popcorn.
# Run once per machine. No root needed.
#
# The versions are pinned, and each download must match its SHA-256 before it is used.
# To update: read the Firecracker release notes and security advisories, then change the
# version and hash together. Get the hash of the release archive with:
#   curl -fsSL https://api.github.com/repos/firecracker-microvm/firecracker/releases/tags/<version> \
#     | jq -r '.assets[] | select(.name == "firecracker-<version>-x86_64.tgz") | .digest'
set -eu

FIRECRACKER=v1.17.0
FIRECRACKER_SHA256=06094a1108ae9e82aa4c23a775aa92758f53f1175d422270d9d6162cb9ade558
# The newest CI kernel published for a recent release line. Needs vsock, overlayfs and ext4.
KERNEL=v1.15/x86_64/vmlinux-6.1.155
KERNEL_SHA256=e20e46d0c36c55c0d1014eb20576171b3f3d922260d9f792017aeff53af3d4f2

out=${POPCORN_HOME:-$HOME/.local/share/popcorn}
mkdir -p "$out/bin"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
trap 'exit 130' INT TERM HUP

check() { # file, expected hash
  echo "$2  $1" | sha256sum -c --quiet - || { echo "SHA-256 of $1 does not match. Nothing installed." >&2; exit 1; }
}

# Copy next to the target, then rename over it. A rename is atomic, so a cut-short copy never
# replaces a good file, and it works while a corn runs the old binary.
place() { # source, target
  cp "$1" "$2.new"
  chmod 0755 "$2.new"
  mv -f "$2.new" "$2"
}

v=$FIRECRACKER
echo "Firecracker $v"
curl -fsSL --proto '=https' --retry 3 -o "$tmp/fc.tgz" "https://github.com/firecracker-microvm/firecracker/releases/download/$v/firecracker-$v-x86_64.tgz"
check "$tmp/fc.tgz" "$FIRECRACKER_SHA256"
tar -xzf "$tmp/fc.tgz" -C "$tmp"

curl -fsSL --proto '=https' --retry 3 -o "$tmp/vmlinux" "https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/$KERNEL"
check "$tmp/vmlinux" "$KERNEL_SHA256"

# Install only after every check passed.
place "$tmp/release-$v-x86_64/firecracker-$v-x86_64" "$out/bin/firecracker"
place "$tmp/release-$v-x86_64/jailer-$v-x86_64" "$out/bin/jailer"
place "$tmp/vmlinux" "$out/vmlinux"
chmod 0644 "$out/vmlinux"
echo "Kernel $KERNEL"
"$out/bin/firecracker" --version | head -1
