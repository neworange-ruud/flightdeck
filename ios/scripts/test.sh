#!/bin/bash
#
# ios/scripts/test.sh
#
# Regenerates the Xcode project from project.yml and runs the unit + UI
# test suites against the iPhone 16 Pro / iOS 18.4 simulator. Keeps all
# build artifacts under ios/.derived (gitignored) instead of the shared
# Xcode DerivedData location.
#
# Usage:
#   ios/scripts/test.sh
#
# Requires: xcodegen (brew install xcodegen), Xcode 26+ with an
# "iPhone 16 Pro" (iOS 18.4) simulator available.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IOS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

DESTINATION="platform=iOS Simulator,name=iPhone 16 Pro,OS=18.4"
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
