#!/usr/bin/env sh
# Check the Desktop version about to be released (argument), or the versions
# already in both package manifests (no argument), against the embedded core.
set -eu

root=$(git rev-parse --show-toplevel)
cd "$root"

core_version=$(sed -n '/^\[package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p;}' Cargo.toml | head -1)
if [ "${1:-}" = "" ]; then
  desktop_version=$(sed -n '/^\[package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p;}' desktop/Cargo.toml | head -1)
else
  desktop_version=$1
fi

if [ "$core_version" = "" ] || [ "$desktop_version" = "" ]; then
  printf 'Could not read the FlightDeck core or Desktop package version.\n' >&2
  exit 1
fi

if [ "$core_version" != "$desktop_version" ]; then
  printf 'Desktop version %s would bundle FlightDeck core %s.\n' \
    "$desktop_version" "$core_version" >&2
  printf 'Release the CLI/core at version %s before releasing Desktop.\n' \
    "$desktop_version" >&2
  exit 1
fi

printf 'Desktop and FlightDeck core versions match: %s.\n' "$core_version"
