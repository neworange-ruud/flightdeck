# Shared helpers for the tutorial macOS VM. Source it: `. tutorials/vm/lib.sh`.
#
# The VM is a Tart clone of a Cirrus Labs "vanilla" macOS image: user
# admin/admin, auto-login, SSH on. Vanilla images have no Tart guest agent,
# so commands go over SSH (a key is installed on first contact) and the
# screen over VNC (Virtualization.framework's own server, so input arrives
# as from real hardware and needs no permission inside the guest).

TUT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TUT_STATE="$TUT_ROOT/target/tutorials"
VM_BASE="${VM_BASE:-fd-tutorial-base}"
VM="${VM:-fd-tutorial}"
VM_USER=admin
VM_DISPLAY="${VM_DISPLAY:-1920x1080pt}"
# Host folder shared into the guest at "/Volumes/My Shared Files/share":
# the app goes in, recordings and screenshots come out.
VM_SHARE="$TUT_STATE/share"
VM_GUEST_SHARE="/Volumes/My Shared Files/share"
VM_KEY="$TUT_STATE/ssh/id_ed25519"
VM_RUN_LOG="$TUT_STATE/$VM.run.log"
VENV="$TUT_ROOT/.tutorial-venv"
# vncdotool's VNC auth uses TripleDES, which `cryptography` warns about.
export PYTHONWARNINGS="ignore"

mkdir -p "$VM_SHARE" "$(dirname "$VM_KEY")"
[[ -f "$VM_KEY" ]] || ssh-keygen -q -t ed25519 -N '' -C fd-tutorial -f "$VM_KEY"

SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
          -o LogLevel=ERROR -o ConnectTimeout=5)

vm_ip() { tart ip "$VM" --wait "${1:-120}"; }

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

# vncdo against the running VM: `vm_vnc_do key cmd-space type hello`.
vm_vnc_do() {
  local addr pass
  read -r addr pass < <(vm_vnc)
  "$VENV/bin/vncdo" -s "$addr" ${pass:+-p "$pass"} "$@"
}
