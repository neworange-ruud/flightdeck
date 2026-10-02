#!/usr/bin/env bash
# Assert that release assets in a directory follow the naming contract of the desktop
# self-updater. Usage: check-asset-names.sh [--version X.Y.Z] [--os macos|linux|windows] [DIR]
# (DIR defaults to target/desktop-dist). Fails if a zip/AppImage that looks like an
# updater asset is misnamed, if no asset for --os is found, or if an asset has no valid
# .sha256.
#
# The contract lives in desktop/src/selfupdate/release.rs (`asset_name`, `checksum_name`),
# which builds the names with these format strings:
#   macOS    FlightDeck-{version}-macos-{arch}.zip
#   Linux    FlightDeck-{version}-linux-{arch}.AppImage
#   Windows  FlightDeck-{version}-windows-{arch}-portable.zip
#   checksum {asset}.sha256      containing `<64 hex>  <asset>`
#   {arch} in { x86_64, aarch64 }; {version} = plain major.minor.patch
# The same contract as a regex (keep in sync with release.rs):
#   ^FlightDeck-[0-9]+\.[0-9]+\.[0-9]+-(macos-(x86_64|aarch64)\.zip|linux-(x86_64|aarch64)\.AppImage|windows-(x86_64|aarch64)-portable\.zip)$
set -euo pipefail

VERSION="" OS_FILTER="" DIR=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) VERSION="$2"; shift 2 ;;
    --os) OS_FILTER="$2"; shift 2 ;;
    *) DIR="$1"; shift ;;
  esac
done
DIR="${DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/target/desktop-dist}"

VER_RE='[0-9]+\.[0-9]+\.[0-9]+'
ARCH_RE='(x86_64|aarch64)'
ASSET_RE="^FlightDeck-${VER_RE}-(macos-${ARCH_RE}\.zip|linux-${ARCH_RE}\.AppImage|windows-${ARCH_RE}-portable\.zip)\$"
case "$OS_FILTER" in
  macos) WANT_RE="^FlightDeck-${VER_RE}-macos-${ARCH_RE}\.zip\$" ;;
  linux) WANT_RE="^FlightDeck-${VER_RE}-linux-${ARCH_RE}\.AppImage\$" ;;
  windows) WANT_RE="^FlightDeck-${VER_RE}-windows-${ARCH_RE}-portable\.zip\$" ;;
  "") WANT_RE="$ASSET_RE" ;;
  *) echo "unknown --os: $OS_FILTER" >&2; exit 2 ;;
esac

fail=0 found=0
err() { echo "FAIL: $*" >&2; fail=1; }

for f in "$DIR"/FlightDeck-*; do
  [[ -e "$f" ]] || continue
  name="$(basename "$f")"
  case "$name" in
    *.sha256) [[ -e "${f%.sha256}" ]] || err "$name has no matching asset"; continue ;;
    *.zip|*.AppImage) ;;
    *) continue ;; # .msi/.deb/.rpm are for package managers, never fetched by the app
  esac
  if ! [[ "$name" =~ $ASSET_RE ]]; then
    err "$name does not match the updater's asset name regex"
    continue
  fi
  [[ "$name" =~ $WANT_RE ]] || continue # a valid asset of another OS
  found=$((found + 1))
  if [[ -n "$VERSION" && "$name" != FlightDeck-"$VERSION"-* ]]; then err "$name is not version $VERSION"; fi
  sums="$f.sha256"
  if [[ ! -f "$sums" ]]; then err "$name has no .sha256"; continue; fi
  if ! grep -Eq "^[0-9a-f]{64}  ${name//./\\.}\$" "$sums"; then
    err "$name.sha256 is not in sha256sum format for $name"
    continue
  fi
  expected="$(cut -d' ' -f1 "$sums")"
  if command -v sha256sum >/dev/null; then
    actual="$(sha256sum "$f" | cut -d' ' -f1)"
  else
    actual="$(shasum -a 256 "$f" | cut -d' ' -f1)"
  fi
  [[ "$expected" == "$actual" ]] || err "$name.sha256 does not match the file"
  echo "ok: $name"
done

if [[ $found -eq 0 ]]; then err "no ${OS_FILTER:-desktop} asset found in $DIR"; fi
exit $fail
