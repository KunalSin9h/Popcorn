#!/usr/bin/env sh
# Builds a release binary and installs it plus the desktop entry and icon for the current user.
set -e
cd "$(dirname "$0")"
cargo build --release
mkdir -p ~/.local/bin ~/.local/share/applications ~/.local/share/icons/hicolor/512x512/apps
cp target/release/popcorn ~/.local/bin/popcorn
cp assets/popcorn.desktop ~/.local/share/applications/popcorn.desktop
cp assets/popcorn.png ~/.local/share/icons/hicolor/512x512/apps/popcorn.png
gtk-update-icon-cache -q ~/.local/share/icons/hicolor 2>/dev/null || true
update-desktop-database -q ~/.local/share/applications 2>/dev/null || true
echo "installed: ~/.local/bin/popcorn"
