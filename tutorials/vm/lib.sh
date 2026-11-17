# Shared helpers for the tutorial macOS VM. Source it: `. tutorials/vm/lib.sh`.
#
# The VM is a Tart clone of a Cirrus Labs "vanilla" macOS image: user
# admin/admin, auto-login, SSH on. Vanilla images have no Tart guest agent,
# so commands go over SSH (a key is installed on first contact), input over
# Tart's --vnc-experimental server, and screenshots/recordings with
# screencapture over SSH.
#
# That VNC server is Virtualization.framework's private _VZVNCServer: no
# "screen is being controlled" banner or menu-bar indicator in the guest (the
# guest's own Screen Sharing shows both, on every connection), and input
# arrives as from real hardware. Its one known crash: a client connecting
# while the guest is recording its screen can take the whole VM down
# (assertion in -[_VZVNCServer _setupVirtualMachineAccessor]). So one daemon
# (vnc_drive.py serve) connects once per boot, before any recording, and
# everything else sends it scripts over a unix socket; nothing else connects.

TUT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TUT_STATE="$TUT_ROOT/target/tutorials"
VM_BASE="${VM_BASE:-fd-tutorial-base}"
VM="${VM:-fd-tutorial}"
VM_USER=admin
# The guest's screen in points (Retina: twice that in pixels). Smaller than
# the 1920x1080 video on purpose: edit.py scales it up, so everything on
# screen reads bigger, like a zoomed-in view.
VM_DISPLAY="${VM_DISPLAY:-1440x810pt}"
# Host folder shared into the guest at "/Volumes/My Shared Files/share":
# the app goes in, recordings and screenshots come out.
VM_SHARE="$TUT_STATE/share"
VM_GUEST_SHARE="/Volumes/My Shared Files/share"
VM_KEY="$TUT_STATE/ssh/id_ed25519"
VM_RUN_LOG="$TUT_STATE/$VM.run.log"
# Editing marks of the recording in progress (see vnc_drive.py, edit.py).
export TUT_TIMELINE="$TUT_STATE/timeline.log"
VENV="$TUT_ROOT/.tutorial-venv"
# vncdotool's VNC auth uses TripleDES, which `cryptography` warns about.
export PYTHONWARNINGS="ignore"

mkdir -p "$VM_SHARE" "$(dirname "$VM_KEY")"
[[ -f "$VM_KEY" ]] || ssh-keygen -q -t ed25519 -N '' -C fd-tutorial -f "$VM_KEY"

# One shared connection per guest (ControlMaster): a call then takes
# milliseconds instead of a second of handshake, which also keeps the
# recording's stop mark in step with the moment screencapture is stopped.
SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
          -o LogLevel=ERROR -o ConnectTimeout=5
          -o ControlMaster=auto -o "ControlPath=${TMPDIR%/}/fdt-%h" -o ControlPersist=15m)

vm_ip() { tart ip "$VM" --wait "${1:-120}"; }

# Long-lived processes start through `detach` (a session of their own), so
# stopping whatever started them does not stop them too.
DETACH="$TUT_ROOT/tutorials/vm/detach"

# ssh into the guest with the tutorial key.
vm_ssh() { ssh "${SSH_OPTS[@]}" -i "$VM_KEY" "$VM_USER@$(vm_ip)" "$@"; }

# First contact: install the key using the image's default password.
vm_install_key() {
  local askpass="$TUT_STATE/ssh/askpass.sh"
  printf '#!/bin/sh\necho admin\n' > "$askpass" && chmod +x "$askpass"
  local ip; ip="$(vm_ip)"
  for _ in $(seq 1 60); do
    if SSH_ASKPASS="$askpass" SSH_ASKPASS_REQUIRE=force \
        ssh "${SSH_OPTS[@]}" -o PubkeyAuthentication=no "$VM_USER@$ip" \
        "mkdir -p ~/.ssh && chmod 700 ~/.ssh && cat >> ~/.ssh/authorized_keys" \
        < "$VM_KEY.pub" 2>/dev/null; then
      return 0
    fi
    sleep 2
  done
  echo "could not reach $VM over SSH" >&2
  return 1
}

# The VNC address `tart run --vnc-experimental` printed, as vncdotool's
# "host::port" plus the password.
vm_vnc() {
  local url
  url="$(grep -o 'vnc://[^ ]*' "$VM_RUN_LOG" | tail -1)"
  [[ -n "$url" ]] || { echo "no VNC URL in $VM_RUN_LOG" >&2; return 1; }
  python3 - "$url" <<'PY'
import sys, urllib.parse
u = urllib.parse.urlparse(sys.argv[1])
print(f"{u.hostname}::{u.port} {u.password or ''}")
PY
}

VNC_SOCK="${TMPDIR%/}/fdt-vnc.sock"
VNC_LOG="$TUT_STATE/vncd.log"

# (Re)start the VNC daemon for the running VM: only right after boot, never
# while recording (connecting then can crash the VM).
vm_vnc_daemon() {
  local addr pass
  read -r addr pass < <(vm_vnc)
  pkill -f "vnc_drive.py serve" 2>/dev/null || true
  rm -f "$VNC_SOCK"
  VM_DISPLAY="$VM_DISPLAY" nohup "$DETACH" "$VENV/bin/python" "$TUT_ROOT/tutorials/vm/vnc_drive.py" serve "$addr" "$pass" "$VNC_SOCK" \
    < /dev/null > "$VNC_LOG" 2>&1 &
  disown
  for _ in $(seq 1 100); do grep -q "daemon ready" "$VNC_LOG" 2>/dev/null && return 0; sleep 0.2; done
  echo "VNC daemon did not start; see $VNC_LOG" >&2
  return 1
}

# Run a scene script through the daemon: `vm_vnc_run file|-`.
vm_vnc_run() {
  [[ -S "$VNC_SOCK" ]] || { echo "no VNC daemon ($VNC_SOCK); run tutorials/vm/up.sh" >&2; return 1; }
  "$VENV/bin/python" "$TUT_ROOT/tutorials/vm/vnc_drive.py" send "$VNC_SOCK" "${1:--}"
}
