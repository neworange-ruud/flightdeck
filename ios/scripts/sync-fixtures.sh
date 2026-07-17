#!/bin/bash
#
# ios/scripts/sync-fixtures.sh
#
# Regenerates ios/FlightDeckRemoteTests/ProtocolTests/FixturesGenerated.swift
# from the golden wire-protocol fixtures in remote/protocol/tests/fixtures/.
# Those JSON files are the cross-language contract (spec §12: "those files
# are the contract"); the Swift fixture-conformance test decodes every one,
# re-encodes it, and compares semantically.
#
# The fixtures are embedded as base64 string constants in a generated Swift
# file (rather than copied as bundle resources) so they ride the test
# target's existing `sources:` glob in ios/project.yml — no project.yml edit
# and no dependence on xcodegen's resource-phase inference. Base64 keeps the
# embedding escape-proof (fixtures contain quotes and \u escapes).
#
# The e2e_crypto/vectors.json file is NOT a message fixture (it drives the
# E2EChannel crypto tests) and is skipped.
#
# Usage:
#   ios/scripts/sync-fixtures.sh
#
# Run it whenever remote/protocol/tests/fixtures/ changes, then commit the
# regenerated FixturesGenerated.swift.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
FIXTURES_DIR="$REPO_ROOT/remote/protocol/tests/fixtures"
OUT_DIR="$REPO_ROOT/ios/FlightDeckRemoteTests/ProtocolTests"
OUT_FILE="$OUT_DIR/FixturesGenerated.swift"

if [[ ! -d "$FIXTURES_DIR" ]]; then
  echo "error: fixtures directory not found: $FIXTURES_DIR" >&2
  exit 1
fi

mkdir -p "$OUT_DIR"

{
  cat <<'HEADER'
//
//  FixturesGenerated.swift
//  FlightDeckRemoteTests
//
//  GENERATED FILE — DO NOT EDIT BY HAND.
//  Regenerate with ios/scripts/sync-fixtures.sh whenever
//  remote/protocol/tests/fixtures/ changes.
//
//  Each entry is one golden wire-protocol fixture, base64-encoded verbatim.
//  Categories map to the top-level Swift wire types:
//    relay            -> Wire.RelayFrame
//    desktop_to_phone -> Wire.DesktopToPhone
//    phone_to_desktop -> Wire.PhoneCommand
//

import Foundation

/// One golden JSON fixture from remote/protocol/tests/fixtures/.
struct ProtocolFixture {
    /// Fixture directory: `relay`, `desktop_to_phone`, or `phone_to_desktop`.
    let category: String
    /// File name without extension, e.g. `pairing_claimed`.
    let name: String
    /// Base64 of the fixture file's exact bytes.
    let base64: String

    /// The fixture's raw JSON bytes.
    var data: Data { Data(base64Encoded: base64)! }
}

enum ProtocolFixtures {
    static let all: [ProtocolFixture] = [
HEADER

  count=0
  for category in relay desktop_to_phone phone_to_desktop; do
    dir="$FIXTURES_DIR/$category"
    if [[ ! -d "$dir" ]]; then
      echo "error: expected fixture category directory: $dir" >&2
      exit 1
    fi
    for file in "$dir"/*.json; do
      name="$(basename "$file" .json)"
      b64="$(base64 < "$file" | tr -d '\n')"
      printf '        ProtocolFixture(\n'
      printf '            category: "%s",\n' "$category"
      printf '            name: "%s",\n' "$name"
      printf '            base64: "%s"),\n' "$b64"
      count=$((count + 1))
    done
  done

  cat <<FOOTER
    ]

    /// Number of embedded fixtures, asserted by the conformance test so a
    /// stale FixturesGenerated.swift is caught when new fixtures land.
    static let expectedCount = $count
}
FOOTER
} > "$OUT_FILE"

echo "wrote $OUT_FILE ($count fixtures)"
