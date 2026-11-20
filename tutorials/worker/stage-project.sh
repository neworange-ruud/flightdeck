#!/usr/bin/env bash
# Put an extra mock project on the tutorial worker, off camera: a project from
# tutorials/stage/NAME, which (unlike tutorials/projects/) is not baked into
# the image, so it shows in no other scene's project list.
#
#   tutorials/worker/stage-project.sh bakery
#
# Like the image's projects it becomes ~/flightdeck/NAME, a git repo with one
# commit and an `origin` in /srv/git. Extra setup for the developer's own
# checkout (e.g. `npm install`) is the scene's business.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
NAME="${WORKER_NAME:-fd-tutorial-worker}"
project="$1"
src="$ROOT/tutorials/stage/$project"
[[ -d "$src" ]] || { echo "no such project: $src" >&2; exit 1; }

COPYFILE_DISABLE=1 tar --no-xattrs -C "$src" -cf - . | podman exec -i -u dev "$NAME" sh -c "
  set -e
  mkdir -p ~/flightdeck/$project && tar -C ~/flightdeck/$project -xf -
  cd ~/flightdeck/$project
  git init -q && git add -A && git commit -q -m 'Initial commit'
  git clone -q --bare . /srv/git/$project.git
  git remote add origin /srv/git/$project.git
  git fetch -q origin && git branch -q -u origin/main main
"
echo "staged ~/flightdeck/$project"
