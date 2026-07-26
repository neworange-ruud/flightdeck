#!/bin/bash
#
# ios/scripts/archive.sh
#
# Builds a signed, uploadable FlightDeck Remote archive and (optionally) sends it
# to App Store Connect for TestFlight.
#
# Usage:
#   ios/scripts/archive.sh                 # archive + export a .ipa
#   ios/scripts/archive.sh --upload        # ...then upload to App Store Connect
#   FD_BUILD_NUMBER=7 ios/scripts/archive.sh --upload
#
# Requires:
#   - xcodegen (brew install xcodegen)
#   - An Apple Developer Program membership on team 7NKCS4AZS9, signed in to
#     Xcode (Settings -> Accounts). Automatic signing creates the Apple
#     Distribution certificate and App Store profile on first run, which needs
#     the Account Holder or Admin role.
#
# For --upload, an App Store Connect API key (App Store Connect -> Users and
# Access -> Integrations -> App Store Connect API). Export:
#   FD_ASC_KEY_ID=XXXXXXXXXX
#   FD_ASC_ISSUER_ID=xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx
#   FD_ASC_KEY_PATH=/path/to/AuthKey_XXXXXXXXXX.p8
# An API key is preferred over an app-specific password: it is scoped, revocable,
# and does not tie uploads to one person's Apple Account.
#
# The build number must strictly increase on EVERY upload — App Store Connect
# rejects a reused one. Override project.yml's value with FD_BUILD_NUMBER rather
# than editing the file for a throwaway build.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IOS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

SCHEME="FlightDeckRemote"
PROJECT="$IOS_DIR/FlightDeckRemote.xcodeproj"
BUILD_DIR="$IOS_DIR/.build-archive"
ARCHIVE_PATH="$BUILD_DIR/$SCHEME.xcarchive"
EXPORT_PATH="$BUILD_DIR/export"
EXPORT_OPTIONS="$BUILD_DIR/ExportOptions.plist"
TEAM_ID="7NKCS4AZS9"

UPLOAD=false
if [ "${1:-}" = "--upload" ]; then
  UPLOAD=true
fi

rm -rf "$BUILD_DIR"
mkdir -p "$BUILD_DIR"

echo "==> Generating Xcode project (xcodegen)"
(cd "$IOS_DIR" && xcodegen generate)

# An explicit build number override, for when the marketing version has not
# changed but a new build has to go up.
VERSION_ARGS=()
if [ -n "${FD_BUILD_NUMBER:-}" ]; then
  echo "==> Overriding build number: $FD_BUILD_NUMBER"
  VERSION_ARGS+=("CURRENT_PROJECT_VERSION=$FD_BUILD_NUMBER")
fi

echo "==> Archiving (Release, device)"
xcodebuild archive \
  -project "$PROJECT" \
  -scheme "$SCHEME" \
  -configuration Release \
  -destination 'generic/platform=iOS' \
  -archivePath "$ARCHIVE_PATH" \
  -skipPackagePluginValidation \
  -allowProvisioningUpdates \
  "${VERSION_ARGS[@]+"${VERSION_ARGS[@]}"}"

# `app-store-connect` is the current name for what used to be `app-store`.
# uploadSymbols keeps crash reports symbolicated in App Store Connect; the app
# has no bitcode (removed platform-wide) so nothing to configure there.
cat > "$EXPORT_OPTIONS" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>method</key>
	<string>app-store-connect</string>
	<key>teamID</key>
	<string>$TEAM_ID</string>
	<key>uploadSymbols</key>
	<true/>
	<key>destination</key>
	<string>export</string>
</dict>
</plist>
PLIST

echo "==> Exporting .ipa"
xcodebuild -exportArchive \
  -archivePath "$ARCHIVE_PATH" \
  -exportPath "$EXPORT_PATH" \
  -exportOptionsPlist "$EXPORT_OPTIONS" \
  -allowProvisioningUpdates

IPA="$(find "$EXPORT_PATH" -name '*.ipa' -maxdepth 1 | head -1)"
if [ -z "$IPA" ]; then
  echo "No .ipa produced in $EXPORT_PATH" >&2
  exit 1
fi
echo "==> Built: $IPA"

if [ "$UPLOAD" != true ]; then
  cat <<EOF

Archive exported but NOT uploaded. To upload:
  ios/scripts/archive.sh --upload

Or upload this .ipa by hand via Xcode's Organizer / Transporter:
  $IPA
EOF
  exit 0
fi

for var in FD_ASC_KEY_ID FD_ASC_ISSUER_ID FD_ASC_KEY_PATH; do
  if [ -z "${!var:-}" ]; then
    echo "--upload needs $var (see the header of this script)" >&2
    exit 1
  fi
done

if [ ! -f "$FD_ASC_KEY_PATH" ]; then
  echo "App Store Connect key not found: $FD_ASC_KEY_PATH" >&2
  exit 1
fi

# altool looks for the .p8 in a fixed set of directories rather than taking a
# path, so point it at the key's own directory via API_PRIVATE_KEYS_DIR.
export API_PRIVATE_KEYS_DIR
API_PRIVATE_KEYS_DIR="$(cd "$(dirname "$FD_ASC_KEY_PATH")" && pwd)"

echo "==> Validating with App Store Connect"
xcrun altool --validate-app \
  --type ios \
  --file "$IPA" \
  --apiKey "$FD_ASC_KEY_ID" \
  --apiIssuer "$FD_ASC_ISSUER_ID"

echo "==> Uploading to App Store Connect"
xcrun altool --upload-app \
  --type ios \
  --file "$IPA" \
  --apiKey "$FD_ASC_KEY_ID" \
  --apiIssuer "$FD_ASC_ISSUER_ID"

cat <<EOF

Uploaded. App Store Connect processes the build (usually a few minutes), then it
appears under TestFlight. Internal testers on the App Store Connect team get it
with no review; external testers need Beta App Review first.
EOF
