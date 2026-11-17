#!/usr/bin/env bash
# Publish a finished tutorial to the website: a web-sized MP4, a poster and
# the voiceover's subtitles, in web/public/videos/tutorials/.
#
#   tutorials/publish.sh first-agent           poster from 4 s in
#   POSTER=12 tutorials/publish.sh first-agent poster from 12 s in
#
# Screen recordings compress well: CRF 30 keeps text sharp at 1080p for about
# 1.3 MB a minute (web/public/videos/README.md asks for under ~1.5 MB).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NAME="${1:?publish.sh NAME}"
SRC="$ROOT/target/tutorials/videos/$NAME"
OUT="$ROOT/web/public/videos/tutorials"
mkdir -p "$OUT"

ffmpeg -nostdin -loglevel error -y -i "$SRC.mp4" \
  -c:v libx264 -preset slow -crf 30 -tune stillimage -pix_fmt yuv420p \
  -c:a aac -b:a 96k -ac 1 -movflags +faststart "$OUT/$NAME.mp4"
ffmpeg -nostdin -loglevel error -y -ss "${POSTER:-4}" -i "$SRC.mp4" \
  -frames:v 1 -vf scale=1280:-2 -q:v 4 "$OUT/$NAME.jpg"
if [[ -f "$SRC.vtt" ]]; then cp "$SRC.vtt" "$OUT/$NAME.vtt"; fi

dur="$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$OUT/$NAME.mp4")"
echo "$OUT/$NAME.mp4  $(du -h "$OUT/$NAME.mp4" | cut -f1)  $(printf '%.0f' "$dur") s"
