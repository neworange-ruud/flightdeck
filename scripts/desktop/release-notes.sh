#!/usr/bin/env sh
# Print the release notes for one desktop version: the body of its
# `## [<version>] - <date>` section in desktop/CHANGELOG.md, without the
# "None yet." placeholder groups. Used by desktop.yml's publish job; see
# desktop/PACKAGING.md.
#
#   scripts/desktop/release-notes.sh 0.1.1 [desktop/CHANGELOG.md]
#
# Exits 1 (printing nothing) if the version has no section or the section is
# empty, so the caller can fall back to generated notes.
set -eu

if [ "${1:-}" = "" ]; then
  printf 'Usage: %s <version> [changelog]\n' "$0" >&2
  exit 2
fi
version="${1#desktop-v}"
version="${version#v}"
changelog="${2:-$(dirname "$0")/../../desktop/CHANGELOG.md}"

VERSION="$version" perl -0ne '
  my ($body) = /^## \[\Q$ENV{VERSION}\E\][^\n]*\n(.*?)(?=^## \[|\z)/sm or exit 1;
  # Drop groups whose only entry is the template placeholder.
  $body =~ s/^### [^\n]+\n\s*- None yet\.\n//mg;
  $body =~ s/\A\s+//;
  $body =~ s/\s+\z//;
  exit 1 unless length $body;
  print "$body\n";
' "$changelog"
