#!/usr/bin/env bash
# Start a fresh tutorial VM: a copy-on-write clone of the prepared base image
# (see tutorials/README.md), booted headless with the shared folder,
# with FlightDeck.app from target/desktop-dist installed in /Applications.
#
#   tutorials/vm/up.sh          throw away the previous clone, start a new one
#   KEEP=1 tutorials/vm/up.sh   reuse the existing clone if there is one
set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

APP="$TUT_ROOT/target/desktop-dist/FlightDeck.app"
[[ -d "$APP" ]] || { echo "no $APP: run scripts/desktop/macos-bundle.sh" >&2; exit 1; }

if tart list --quiet | grep -qx "$VM" && [[ "${KEEP:-0}" == "1" ]]; then
  echo "reusing $VM"
else
  tart stop "$VM" >/dev/null 2>&1 || true
  tart delete "$VM" >/dev/null 2>&1 || true
  tart clone "$VM_BASE" "$VM"
  tart set "$VM" --display "$VM_DISPLAY"
fi

if ! tart list | grep -E "^local +$VM +" | grep -q running; then
  nohup tart run "$VM" --no-graphics --no-audio --vnc-experimental \
    --dir "share:$VM_SHARE" < /dev/null > "$VM_RUN_LOG" 2>&1 &
  disown
fi
for _ in $(seq 1 60); do grep -q 'vnc://' "$VM_RUN_LOG" 2>/dev/null && break; sleep 1; done
vm_ip >/dev/null
for _ in $(seq 1 60); do vm_ssh true 2>/dev/null && break; sleep 2; done
# macOS keeps the mode it saved for the base image's display: switch it to
# VM_DISPLAY (HiDPI) before the VNC daemon connects, as the VNC framebuffer
# takes its size at connect time.
read -r PT_W PT_H <<< "$(sed 's/pt$//; s/x/ /' <<< "$VM_DISPLAY")"
vm_ssh 'cat > /tmp/display-mode.swift' < "$TUT_ROOT/tutorials/vm/guest/display-mode.swift"
vm_ssh "swift /tmp/display-mode.swift $PT_W $PT_H"
vm_vnc_daemon

# Install the app through the shared folder (ditto keeps the bundle intact).
rm -rf "$VM_SHARE/FlightDeck.app"
ditto "$APP" "$VM_SHARE/FlightDeck.app"
vm_ssh "rm -rf /Applications/FlightDeck.app && ditto '$VM_GUEST_SHARE/FlightDeck.app' /Applications/FlightDeck.app"
rm -rf "$VM_SHARE/FlightDeck.app"

# No OS notifications from FlightDeck: they cover the toolbar's right end.
vm_ssh "mkdir -p ~/.flightdeck && printf '[notifications]\nenabled = false\n' > ~/.flightdeck/config.toml"

echo "VM $VM up at $(vm_ip); $(vm_ssh 'defaults read /Applications/FlightDeck.app/Contents/Info CFBundleShortVersionString' 2>/dev/null | sed 's/^/FlightDeck /')"
echo "VNC: $(grep -o 'vnc://[^ ]*' "$VM_RUN_LOG" | tail -1)"
