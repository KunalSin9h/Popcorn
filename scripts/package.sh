#!/bin/sh
# Build release packages into dist/: Popcorn-<version>-x86_64.AppImage and popcorn_<version>_amd64.deb.
# Needs dist/golden-<version>.ext4.zst.sha256 (scripts/release-image.sh) and Firecracker plus the
# kernel (scripts/fetch-firecracker.sh). Usage: scripts/package.sh [appimage|deb|all]
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
what=${1:-all}
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/Cargo.toml" | head -1)
dist="$here/dist"
vm=${POPCORN_VM_DIR:-$HOME/.local/share/popcorn}
sum="$dist/golden-$version.ext4.zst.sha256"
[ -f "$sum" ] || { echo "missing $sum: run scripts/release-image.sh first" >&2; exit 1; }
for f in bin/firecracker bin/jailer vmlinux; do
  [ -f "$vm/$f" ] || { echo "missing $vm/$f: run scripts/fetch-firecracker.sh first" >&2; exit 1; }
done

# Build into its own folder, so a release build never replaces a running development binary.
export CARGO_TARGET_DIR="$here/target/package"
POPCORN_IMAGE_SHA256=$(cut -d' ' -f1 "$sum") cargo build --release --manifest-path "$here/Cargo.toml"
bin="$CARGO_TARGET_DIR/release/popcorn"

# Lay out a Linux file tree: /usr/bin, /usr/lib/popcorn, the desktop entry and the icon.
tree() {
  root=$1
  install -Dm755 "$bin" "$root/usr/bin/popcorn"
  install -Dm755 "$vm/bin/firecracker" "$root/usr/lib/popcorn/firecracker"
  install -Dm755 "$vm/bin/jailer" "$root/usr/lib/popcorn/jailer"
  install -Dm644 "$vm/vmlinux" "$root/usr/lib/popcorn/vmlinux"
  install -Dm644 "$here/assets/popcorn.desktop" "$root/usr/share/applications/popcorn.desktop"
  install -Dm644 "$here/assets/popcorn.png" "$root/usr/share/icons/hicolor/512x512/apps/popcorn.png"
}

if [ "$what" = deb ] || [ "$what" = all ]; then
  d="$dist/deb"
  rm -rf "$d"
  tree "$d"
  mkdir -p "$d/DEBIAN"
  cat > "$d/DEBIAN/control" <<CTL
Package: popcorn
Version: $version
Architecture: amd64
Maintainer: Popcorn developers
Depends: libc6, libvulkan1, libxkbcommon0, libxkbcommon-x11-0, libwayland-client0, libfontconfig1, libfreetype6, libx11-6, libxcb1, e2fsprogs, tar, diffutils, curl
Section: utils
Priority: optional
Description: Security workbench of disposable microVMs
 Run risky code in a corn: a Firecracker microVM with its own kernel, no network
 card, and approval for every connection. Needs KVM.
CTL
  dpkg-deb --root-owner-group -Zzstd --build "$d" "$dist/popcorn_${version}_amd64.deb" >/dev/null
  rm -rf "$d"
  echo "$dist/popcorn_${version}_amd64.deb"
fi

if [ "$what" = appimage ] || [ "$what" = all ]; then
  tool="$dist/appimagetool"
  if [ ! -x "$tool" ]; then
    curl -fsSL -o "$tool" https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
    chmod +x "$tool"
  fi
  a="$dist/Popcorn.AppDir"
  rm -rf "$a"
  tree "$a"
  cp "$here/assets/popcorn.desktop" "$a/popcorn.desktop"
  cp "$here/assets/popcorn.png" "$a/popcorn.png"
  cat > "$a/AppRun" <<'RUN'
#!/bin/sh
here=$(dirname "$(readlink -f "$0")")
export APPDIR="${APPDIR:-$here}"
exec "$here/usr/bin/popcorn" "$@"
RUN
  chmod +x "$a/AppRun"
  out="$dist/Popcorn-$version-x86_64.AppImage"
  # Extract-and-run: works without FUSE, for example in CI.
  APPIMAGE_EXTRACT_AND_RUN=1 ARCH=x86_64 "$tool" -n "$a" "$out" >"$dist/appimagetool.log" 2>&1 || { cat "$dist/appimagetool.log" >&2; exit 1; }
  rm -rf "$a"
  echo "$out"
fi
