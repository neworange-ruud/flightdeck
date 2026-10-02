#!/usr/bin/env bash
# Build a release FlightDeck.app (and a .zip of it) for the GPUI desktop app.
# See desktop/PACKAGING.md. Run from anywhere; output goes to target/desktop-dist/.
#
# Signing and notarization are opt-in through environment variables and are skipped
# when unset, so an unsigned local build needs no secrets:
#   CODESIGN_IDENTITY        e.g. "Developer ID Application: Name (TEAMID)"  -> codesign
#                            (same name as the TUI release's secret; the workflow
#                            imports the certificate, see desktop/PACKAGING.md)
# Notarization (needs a signed app), first match wins:
#   APPLE_API_KEY_PATH + APPLE_API_KEY_ID + APPLE_API_ISSUER_ID   App Store Connect API key
#   NOTARY_KEYCHAIN_PROFILE  a `xcrun notarytool store-credentials` profile
#   APPLE_ID + APPLE_TEAM_ID + APPLE_APP_PASSWORD                  Apple ID, app-specific password
# Other knobs:
#   CARGO_FEATURES_FLAGS     default "" (keeps the crate's default runtime-shaders feature).
#                            Set to "--no-default-features" on a machine with the Metal
#                            Toolchain to ship precompiled shaders.
#   SKIP_BUILD=1             reuse an existing target/release/flightdeck-desktop
#   BUNDLE_ID                default agency.neworange.flightdeck.desktop
#   MIN_MACOS                default 11.0
#   TARGET                   optional rust target triple (e.g. aarch64-apple-darwin)
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "macos-bundle.sh must run on macOS" >&2
  exit 1
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

BUNDLE_ID="${BUNDLE_ID:-agency.neworange.flightdeck.desktop}"
MIN_MACOS="${MIN_MACOS:-11.0}"
APP_NAME="FlightDeck"
EXE="flightdeck-desktop"
VERSION="$(sed -n '/^\[package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p;}' desktop/Cargo.toml | head -1)"
ICON_SRC="desktop/packaging/icons/flightdeck-1024.png"
OUT="$ROOT/target/desktop-dist"
APP="$OUT/$APP_NAME.app"

if [[ -z "${SKIP_BUILD:-}" ]]; then
  # shellcheck disable=SC2086
  cargo build -p flightdeck-desktop --release --locked ${CARGO_FEATURES_FLAGS:-} ${TARGET:+--target "$TARGET"}
fi
BIN="$ROOT/target/${TARGET:+$TARGET/}release/$EXE"
[[ -x "$BIN" ]] || { echo "missing $BIN" >&2; exit 1; }

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/$EXE"

# .icns from the 1024px master (iconutil wants the standard iconset names).
ICONSET="$(mktemp -d)/AppIcon.iconset"
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
  sips -z $s $s "$ICON_SRC" --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
  sips -z $((s * 2)) $((s * 2)) "$ICON_SRC" --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleExecutable</key><string>$EXE</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>$APP_NAME</string>
  <key>CFBundleDisplayName</key><string>$APP_NAME</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>$MIN_MACOS</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
  <key>NSPrincipalClass</key><string>NSApplication</string>
</dict>
</plist>
PLIST
plutil -lint "$APP/Contents/Info.plist" >/dev/null

# --- optional codesign ------------------------------------------------------
if [[ -n "${CODESIGN_IDENTITY:-}" ]]; then
  echo "codesign: $CODESIGN_IDENTITY"
  # No --deep: the bundle holds a single binary, so signing the .app signs it. Hardened
  # runtime needs no entitlements (Metal/GPUI does not require any).
  codesign --force --options runtime --timestamp \
    --sign "$CODESIGN_IDENTITY" "$APP"
  codesign --verify --strict --verbose=2 "$APP"
else
  echo "codesign: skipped (CODESIGN_IDENTITY unset)"
fi

# Asset name is a contract with the self-updater (desktop/src/selfupdate/release.rs):
#   FlightDeck-<version>-macos-<arch>.zip, FlightDeck.app at the zip root.
case "${TARGET:-$(uname -m)}" in
  aarch64*|arm64*) ARCH="aarch64" ;;
  x86_64*) ARCH="x86_64" ;;
  *) echo "unsupported architecture: ${TARGET:-$(uname -m)}" >&2; exit 1 ;;
esac
ZIP="$OUT/$APP_NAME-$VERSION-macos-$ARCH.zip"
make_zip() { rm -f "$ZIP"; ditto -c -k --keepParent "$APP" "$ZIP"; }
make_zip

# --- optional notarization (needs a signed app) -----------------------------
notary_args=()
if [[ -n "${APPLE_API_KEY_PATH:-}" && -n "${APPLE_API_KEY_ID:-}" && -n "${APPLE_API_ISSUER_ID:-}" ]]; then
  notary_args=(--key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER_ID")
elif [[ -n "${NOTARY_KEYCHAIN_PROFILE:-}" ]]; then
  notary_args=(--keychain-profile "$NOTARY_KEYCHAIN_PROFILE")
elif [[ -n "${APPLE_ID:-}" && -n "${APPLE_TEAM_ID:-}" && -n "${APPLE_APP_PASSWORD:-}" ]]; then
  notary_args=(--apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_PASSWORD")
fi
if [[ ${#notary_args[@]} -gt 0 && -n "${CODESIGN_IDENTITY:-}" ]]; then
  echo "notarize: submitting $ZIP"
  xcrun notarytool submit "$ZIP" "${notary_args[@]}" --wait
  xcrun stapler staple "$APP"
  make_zip # re-zip so the archive carries the stapled ticket
else
  echo "notarize: skipped (needs CODESIGN_IDENTITY plus APPLE_API_KEY_*, NOTARY_KEYCHAIN_PROFILE or APPLE_ID/APPLE_TEAM_ID/APPLE_APP_PASSWORD)"
fi

# <asset>.sha256 in `sha256sum` format ("<64 hex>  <name>"), made from inside $OUT so
# the file name carries no directory.
(cd "$OUT" && shasum -a 256 "$(basename "$ZIP")" > "$(basename "$ZIP").sha256")

echo "app:  $APP"
echo "zip:  $ZIP"
echo "sha256: $(cut -d' ' -f1 "$ZIP.sha256")"
