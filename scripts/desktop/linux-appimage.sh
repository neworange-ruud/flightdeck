#!/usr/bin/env bash
# Build an unsigned AppImage of flightdeck-desktop with linuxdeploy. Linux only.
# NOT run in the authoring environment (no Linux machine); exercised by the
# `package` job in .github/workflows/desktop.yml. See desktop/PACKAGING.md.
#
# Needs on PATH (or in $LINUXDEPLOY): linuxdeploy-x86_64.AppImage. On a runner without
# FUSE set APPIMAGE_EXTRACT_AND_RUN=1 (the CI job does).
#
# GPU note: the Vulkan loader and driver (libvulkan.so.1, mesa ICDs) are deliberately NOT
# bundled; they must match the host GPU driver, so the AppImage uses the system's.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

[[ "$(uname -s)" == "Linux" ]] || { echo "linux-appimage.sh must run on Linux" >&2; exit 1; }

VERSION="$(sed -n '/^\[package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p;}' desktop/Cargo.toml | head -1)"
ARCH="${ARCH:-$(uname -m)}"
LINUXDEPLOY="${LINUXDEPLOY:-linuxdeploy-$ARCH.AppImage}"
OUT="$ROOT/target/desktop-dist"
APPDIR="$OUT/AppDir"

[[ -n "${SKIP_BUILD:-}" ]] || cargo build -p flightdeck-desktop --release --locked
BIN="$ROOT/target/release/flightdeck-desktop"

rm -rf "$APPDIR"
mkdir -p "$OUT"

# Every hicolor size we ship; linuxdeploy reads the icon name from the .desktop file.
ICON_ARGS=()
for s in 16 32 48 64 128 256 512; do
  mkdir -p "$APPDIR/usr/share/icons/hicolor/${s}x${s}/apps"
  cp "desktop/packaging/icons/flightdeck-$s.png" \
    "$APPDIR/usr/share/icons/hicolor/${s}x${s}/apps/flightdeck-desktop.png"
done
ICON_ARGS+=(--icon-file "desktop/packaging/icons/flightdeck-256.png" --icon-filename flightdeck-desktop)

VERSION="$VERSION" "$LINUXDEPLOY" \
  --appdir "$APPDIR" \
  --executable "$BIN" \
  --desktop-file desktop/packaging/linux/flightdeck-desktop.desktop \
  "${ICON_ARGS[@]}" \
  --exclude-library 'libvulkan*' \
  --output appimage

mv FlightDeck*-"$ARCH".AppImage "$OUT/FlightDeck-$VERSION-linux-$ARCH.AppImage" 2>/dev/null \
  || mv ./*.AppImage "$OUT/FlightDeck-$VERSION-linux-$ARCH.AppImage"
echo "appimage: $OUT/FlightDeck-$VERSION-linux-$ARCH.AppImage"
