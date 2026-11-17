#!/usr/bin/env bash
# Contact sheets of a finished tutorial, to check a take without watching it:
#   tutorials/vm/sheet.sh NAME [EVERY_SECS]  -> target/tutorials/videos/NAME-sheet-N.png (4x4 each)
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
V="$ROOT/target/tutorials/videos/$1"
rm -f "$V"-sheet-*.png
ffmpeg -nostdin -loglevel error -y -i "$V.mp4" -vf "fps=1/${2:-3},scale=480:-1,tile=4x4" "$V-sheet-%d.png"
ls "$V"-sheet-*.png
