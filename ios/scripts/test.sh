#!/bin/bash
#
# ios/scripts/test.sh
#
# Regenerates the Xcode project from project.yml and runs the unit + UI
# test suites in the simulator. Keeps all build artifacts under ios/.derived
# (gitignored) instead of the shared Xcode DerivedData location.
#
# Usage:
#   ios/scripts/test.sh
#   FD_IOS_DESTINATION='platform=iOS Simulator,name=iPhone 16 Pro,OS=18.4' ios/scripts/test.sh
#
# The default destination pins no OS version, so it runs against whichever
# runtime is installed — a pinned version silently breaks the script whenever
# Xcode ships a new one and drops the old runtime (which is how this landed on
# an uninstallable iOS 18.4 pin). Override with FD_IOS_DESTINATION to target a
# specific device/runtime, e.g. in CI.
#
# Requires: xcodegen (brew install xcodegen), Xcode 26+ with an
# "iPhone 17 Pro" simulator available (or an override).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IOS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

DESTINATION="${FD_IOS_DESTINATION:-platform=iOS Simulator,name=iPhone 17 Pro}"
DERIVED_DATA_PATH="$IOS_DIR/.derived"

echo "==> Generating Xcode project (xcodegen)"
(cd "$IOS_DIR" && xcodegen generate)

echo "==> Running tests (xcodebuild test)"
xcodebuild test \
  -project "$IOS_DIR/FlightDeckRemote.xcodeproj" \
  -scheme "FlightDeckRemote" \
  -destination "$DESTINATION" \
  -derivedDataPath "$DERIVED_DATA_PATH" \
  -skipPackagePluginValidation \
  CODE_SIGNING_ALLOWED=NO
