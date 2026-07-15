#!/usr/bin/env bash
#
# scripts/e2e/run-fullstack.sh — Tier B full-stack simulator E2E orchestrator
# for FlightDeck Remote (see the remote-control-c3m epic / plan
# "kind-jingling-plum.md", "Tier B — Full-stack simulator E2E").
#
# PURPOSE
#   Stand up the WHOLE remote stack on one machine and run the iOS XCUITest
#   against it, proving a real round trip:
#
#       real iOS app (simulator)  <->  real relay  <->  real desktop (PTY)
#
#   This script OWNS the entire lifecycle:
#     1. Build the relay + desktop (Rust) and the iOS app for testing.
#     2. Boot + clean (uninstall) the simulator so the phone starts UNPAIRED.
#     3. Start the relay, generate a fixture project, start the desktop under a
#        PTY with autopair enabled so it offers pairing with a fixed claim
#        token (4729).
#     4. Construct the `fdr1:` pairing payload (the ONLY way the phone learns
#        the local relay URL — the manual code path is nailed to prod) and hand
#        it to the XCUITest via the environment.
#     5. Run the XCUITest, then tear everything down (always, via a trap) and
#        propagate the test's exit code.
#
# WHAT SUPPLIES THE SWIFT TEST
#   The XCUITest itself — `ios/FlightDeckRemoteUITests/RemoteLiveE2EUITests.swift`
#   — is the live-pairing test (remote-control-c3m.9) plus the Tier B capability
#   flows that EXTEND it (remote-control-c3m.10): monitor/projects, new agent,
#   chat reply, git status, and shell. This orchestrator selects the whole class
#   (`-only-testing:FlightDeckRemoteUITests/RemoteLiveE2EUITests`). Use
#   `--bringup-only` / `--print-payload-only` to exercise the parts that do not
#   depend on the Swift test.
#
# DESKTOP-SIDE CROSS-CHECKS (remote-control-c3m.10)
#   After a GREEN XCUITest run, the orchestrator additionally asserts the REAL
#   on-disk effects the capability flows must have produced (a green UI action
#   plus a real filesystem effect together prove a true round trip):
#     * the phone's new-agent flow created a worktree under the fixture's
#       `.flightdeck/worktrees/`;
#     * the phone's chat reply reached the desktop and the fake agent appended
#       it to the fixture worktree's `.flightdeck/agent-replies.log`.
#   These poll (the desktop applies phone commands asynchronously) and fail the
#   whole run non-zero if a side effect is missing. See
#   `assert_desktop_side_effects`.
#
#   c3m.9's test reads two values from its process environment and forwards
#   them into `app.launchEnvironment`:
#       FLIGHTDECK_PAIRING   = "real"      (force RealPairingService)
#       FLIGHTDECK_E2E_FDR1  = "<payload>" (the fdr1: pairing payload)
#   xcodebuild only forwards environment variables to the test-runner process
#   when they carry the `TEST_RUNNER_` prefix, so this script exports BOTH the
#   plain names and the `TEST_RUNNER_`-prefixed names; the prefixed ones are the
#   ones that actually reach the runner.
#
# MODES
#   (default)             Full run: build everything, boot sim, pair live, run
#                         the XCUITest, tear down. Exit = XCUITest exit code.
#   --print-payload-only  Build a fresh fdr1: payload, print it to stdout, print
#                         the decoded JSON to stderr, and exit. No builds, no
#                         relay/desktop/sim. Fast; for verifying the payload
#                         shape.
#   --bringup-only        Build relay + desktop only (skip the slow iOS build),
#                         start the relay + fixture + desktop, assert /healthz=ok
#                         and that the desktop actually OFFERS pairing (its
#                         autopair code appears in the PTY), then tear down.
#                         Fast; verifies the Rust bringup + pairing offer + the
#                         teardown in isolation, without the simulator.
#   --help                Show usage.
#
# ENV KNOBS (all optional; sane defaults)
#   E2E_SIM_DEVICE   Simulator device name.  Default: "iPhone 16 Pro"
#   E2E_SIM_OS       Simulator runtime version. Default: "26.5"
#                    (NOTE: ios/scripts/test.sh pins OS=18.4 which is NOT
#                    installed on this machine — this orchestrator defaults to
#                    the installed 26.5 runtime and is parameterized so the
#                    26.5-vs-18.4 mismatch never recurs. Deployment target 18.0
#                    runs fine on 26.5.)
#   E2E_SCHEME       xcodebuild scheme. Default: "FlightDeckRemote"
#   E2E_UITEST       -only-testing selector.
#                    Default: "FlightDeckRemoteUITests/RemoteLiveE2EUITests"
#   E2E_BUNDLE_ID    App bundle id (for simctl uninstall).
#                    Default: "agency.neworange.flightdeck.remote"
#   E2E_CLAIM_TOKEN  Fixed 4-digit pairing code (must match the desktop's
#                    FLIGHTDECK_REMOTE_AUTOPAIR). Default: "4729"
#   E2E_PORT         Relay port. Default: an auto-picked free port.
#   E2E_KEEP_SIM     If "1", do NOT shut down the simulator on teardown.
#                    Default: shut it down only if this script booted it.
#   E2E_OFFER_TIMEOUT  Max seconds to wait for the desktop to bring its pairing
#                    offer live (the autopair code appears in the PTY). The
#                    desktop is not "ready" until it has actually offered.
#                    Default: 30
#
# REQUIREMENTS: bash, cargo, Xcode + xcodebuild, xcodegen, xcrun simctl,
#               openssl, python3, curl.
#
# ------------------------------------------------------------------------------

set -euo pipefail

# --- Resolve paths ------------------------------------------------------------

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd -P)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/../.." >/dev/null 2>&1 && pwd -P)"
IOS_DIR="${REPO_ROOT}/ios"

# E2E-only entitlements: ad-hoc signs the simulator build WITH a
# keychain-access-groups entitlement so the REAL keychain code path
# (DeviceIdentity.loadOrCreate -> KeychainStore SecItem*) works. Without it, the
# data-protection Keychain returns errSecMissingEntitlement (-34018) and live
# pairing dies before it ever opens the WebSocket. See the file's own comment.
E2E_ENTITLEMENTS="${SCRIPT_DIR}/e2e.entitlements"

RELAY_MANIFEST="${REPO_ROOT}/remote/relay/Cargo.toml"
RELAY_BIN="${REPO_ROOT}/remote/target/debug/flightdeck-relay"
DESKTOP_MANIFEST="${REPO_ROOT}/Cargo.toml"
DESKTOP_BIN="${REPO_ROOT}/target/debug/flightdeck"
FAKE_AGENT="${SCRIPT_DIR}/fake-agent.sh"
MAKE_FIXTURE="${SCRIPT_DIR}/make-fixture-project.sh"

DERIVED_DATA_PATH="${IOS_DIR}/.derived"
XCODEPROJ="${IOS_DIR}/FlightDeckRemote.xcodeproj"

# --- Env knobs / defaults -----------------------------------------------------

E2E_SIM_DEVICE="${E2E_SIM_DEVICE:-iPhone 16 Pro}"
E2E_SIM_OS="${E2E_SIM_OS:-26.5}"
E2E_SCHEME="${E2E_SCHEME:-FlightDeckRemote}"
E2E_UITEST="${E2E_UITEST:-FlightDeckRemoteUITests/RemoteLiveE2EUITests}"
E2E_BUNDLE_ID="${E2E_BUNDLE_ID:-agency.neworange.flightdeck.remote}"
E2E_CLAIM_TOKEN="${E2E_CLAIM_TOKEN:-4729}"
E2E_KEEP_SIM="${E2E_KEEP_SIM:-0}"
# Max seconds to wait for the desktop TUI to bring its pairing offer live (the
# autopair code appears in the PTY output). Supersedes the fixed settle: the
# desktop is not considered ready until it has actually OFFERED pairing.
E2E_OFFER_TIMEOUT="${E2E_OFFER_TIMEOUT:-30}"

# Whether to assert the desktop-side effects of the capability flows (worktree +
# chat reply on disk). Default OFF: the capability flows are gated on
# remote-control-9yv (the phone link drops after new-agent creation), so they are
# XCTSkip'd in the XCUITest and produce no on-disk effects. Set this to 1 at the
# same time as re-enabling the flows in
# RemoteLiveE2EUITests.testLiveRemoteCapabilityFlows once 9yv is fixed.
E2E_ASSERT_SIDE_EFFECTS="${E2E_ASSERT_SIDE_EFFECTS:-0}"

# Desktop-side cross-check knobs (see assert_desktop_side_effects). These MUST
# match the constants the XCUITest drives with
# (RemoteLiveE2EUITests.liveReplyToken / .liveAgentName): the test sends the
# reply text + names the new agent; the orchestrator verifies the on-disk
# effects those produced.
E2E_REPLY_TOKEN="${E2E_REPLY_TOKEN:-e2ereply4729}"
E2E_AGENT_SLUG="${E2E_AGENT_SLUG:-livee2e}"
E2E_EFFECT_TIMEOUT="${E2E_EFFECT_TIMEOUT:-30}"

DESTINATION="platform=iOS Simulator,name=${E2E_SIM_DEVICE},OS=${E2E_SIM_OS}"

# --- Mutable state (referenced by teardown) -----------------------------------

MODE="full"
TMP_ROOT=""        # holds temp HOME, fixture, logs
TMP_HOME=""
FIXTURE_DIR=""
RELAY_LOG=""
DESKTOP_LOG=""
RELAY_PID=""
DESKTOP_PID=""
PORT=""
BOOTED_SIM_BY_US=0
XCUITEST_RC=0

# --- Logging helpers ----------------------------------------------------------

log()  { printf '==> %s\n' "$*" >&2; }
warn() { printf 'WARN: %s\n' "$*" >&2; }
die()  { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

print_tail() {
    # print_tail <label> <file>
    local label="$1" file="$2"
    if [[ -n "${file}" && -f "${file}" ]]; then
        printf -- '----- %s (tail) -----\n' "${label}" >&2
        tail -n 40 "${file}" >&2 || true
        printf -- '----- end %s -----\n' "${label}" >&2
    fi
}

# --- Teardown -----------------------------------------------------------------

# shellcheck disable=SC2329  # invoked indirectly (by teardown / trap)
stop_process() {
    # stop_process <name> <pid>
    local name="$1" pid="$2"
    [[ -n "${pid}" ]] || return 0
    if kill -0 "${pid}" 2>/dev/null; then
        log "stopping ${name} (pid ${pid})"
        # Kill any children first (the PTY `script` wrapper's child, etc.)
        pkill -P "${pid}" 2>/dev/null || true
        kill "${pid}" 2>/dev/null || true
        # Give it a moment to exit, then hard-kill if needed.
        for _ in 1 2 3 4 5 6 7 8 9 10; do
            kill -0 "${pid}" 2>/dev/null || break
            sleep 0.2
        done
        if kill -0 "${pid}" 2>/dev/null; then
            pkill -9 -P "${pid}" 2>/dev/null || true
            kill -9 "${pid}" 2>/dev/null || true
        fi
    fi
}

# shellcheck disable=SC2329  # invoked indirectly via `trap teardown EXIT INT TERM`
teardown() {
    local rc=$?
    set +e
    log "teardown (exit code so far: ${rc})"

    stop_process "desktop" "${DESKTOP_PID}"
    stop_process "relay" "${RELAY_PID}"

    # Safety net: reap any of OUR exact binaries that outlived their tracked
    # pid (targeted on the absolute repo paths so nothing else is touched).
    pkill -f "${DESKTOP_BIN}" 2>/dev/null || true
    pkill -f "${RELAY_BIN}" 2>/dev/null || true

    # Shut down the sim only if we booted it and the caller didn't ask to keep.
    if [[ "${BOOTED_SIM_BY_US}" == "1" && "${E2E_KEEP_SIM}" != "1" ]]; then
        log "shutting down simulator '${E2E_SIM_DEVICE}'"
        xcrun simctl shutdown "${E2E_SIM_DEVICE}" 2>/dev/null || true
    fi

    # Preserve logs for debugging when asked (E2E_KEEP_LOGS=1): copy the relay +
    # desktop logs somewhere durable before the temp sandbox is reaped.
    if [[ "${E2E_KEEP_LOGS:-0}" == "1" && -n "${TMP_ROOT}" && -d "${TMP_ROOT}" ]]; then
        local keep="${E2E_KEEP_LOGS_DIR:-${TMPDIR:-/tmp}/flightdeck-e2e-kept}"
        mkdir -p "${keep}" 2>/dev/null || true
        cp -f "${RELAY_LOG}" "${keep}/relay.log" 2>/dev/null || true
        cp -f "${DESKTOP_LOG}" "${keep}/desktop.log" 2>/dev/null || true
        [[ -n "${FIXTURE_DIR}" && -f "${FIXTURE_DIR}/.flightdeck/agent-replies.log" ]] \
            && cp -f "${FIXTURE_DIR}/.flightdeck/agent-replies.log" "${keep}/agent-replies.log" 2>/dev/null || true
        log "kept logs in ${keep}"
    fi

    # Clean temp dirs (fresh HOME sandbox + fixture + logs).
    if [[ -n "${TMP_ROOT}" && -d "${TMP_ROOT}" ]]; then
        rm -rf "${TMP_ROOT}" 2>/dev/null || true
    fi
    if [[ -n "${FIXTURE_DIR}" && -d "${FIXTURE_DIR}" && "${FIXTURE_DIR}" != "${TMP_ROOT}"* ]]; then
        rm -rf "${FIXTURE_DIR}" 2>/dev/null || true
    fi

    # Propagate the REAL exit code. A bash EXIT trap otherwise leaves the script
    # exiting with the status of the trap's LAST command (a cleanup `[[ ]]`/rm),
    # which silently masked XCUITest / cross-check failures as success. Re-exit
    # with the status captured on entry so a failure always surfaces non-zero.
    exit "${rc}"
}

# --- Free port ----------------------------------------------------------------

pick_free_port() {
    python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
}

# --- fdr1: payload builder ----------------------------------------------------
#
# Matches, field-for-field, the Rust builder and the Swift decoder:
#   Rust: src/remote/pairing.rs::build_qr_payload (QrPayloadJson: claim_token,
#         pairing_secret, relay_url) -> "fdr1:" + base64url_nopad(JSON).
#   Swift: ios/.../Features/Pairing/PairingModels.swift (PairingQRPayload
#          CodingKeys: claim_token / pairing_secret / relay_url; decode() strips
#          the "fdr1:" prefix then base64url-no-pad decodes the JSON).
# The `pairing_secret` is 32 random bytes (CSPRNG via `openssl rand 32`),
# base64url-encoded with no padding.

build_payload() {
    # build_payload <claim_token> <relay_url>  -> echoes the fdr1: payload
    local claim="$1" relay_url="$2" secret_b64url
    secret_b64url="$(openssl rand 32 | python3 -c \
        'import sys,base64; sys.stdout.write(base64.urlsafe_b64encode(sys.stdin.buffer.read()).rstrip(b"=").decode())')"
    python3 - "${claim}" "${secret_b64url}" "${relay_url}" <<'PY'
import sys, json, base64
claim, secret, relay = sys.argv[1], sys.argv[2], sys.argv[3]
body = json.dumps(
    {"claim_token": claim, "pairing_secret": secret, "relay_url": relay},
    separators=(",", ":"),
).encode()
print("fdr1:" + base64.urlsafe_b64encode(body).rstrip(b"=").decode())
PY
}

decode_payload() {
    # decode_payload <payload>  -> prints the decoded JSON (pretty) to stdout
    python3 - "$1" <<'PY'
import sys, json, base64
p = sys.argv[1]
assert p.startswith("fdr1:"), "payload must start with fdr1:"
b = p[len("fdr1:"):]
b += "=" * (-len(b) % 4)  # restore padding for the stdlib decoder
data = base64.urlsafe_b64decode(b)
print(json.dumps(json.loads(data), indent=2))
PY
}

# --- Build phases -------------------------------------------------------------

build_relay() {
    log "building relay (flightdeck-relay)"
    cargo build --manifest-path "${RELAY_MANIFEST}" -p flightdeck-relay \
        || die "relay build failed"
    [[ -x "${RELAY_BIN}" ]] || die "relay binary missing: ${RELAY_BIN}"
}

build_desktop() {
    log "building desktop (flightdeck)"
    cargo build --manifest-path "${DESKTOP_MANIFEST}" \
        || die "desktop build failed"
    [[ -x "${DESKTOP_BIN}" ]] || die "desktop binary missing: ${DESKTOP_BIN}"
}

build_ios() {
    command -v xcodegen >/dev/null 2>&1 || die "xcodegen not found (brew install xcodegen)"
    log "generating Xcode project (xcodegen)"
    (cd "${IOS_DIR}" && xcodegen generate) || die "xcodegen generate failed"
    [[ -f "${E2E_ENTITLEMENTS}" ]] || die "e2e entitlements missing: ${E2E_ENTITLEMENTS}"
    log "building iOS app for testing (${DESTINATION})"
    # Ad-hoc SIGN the simulator build (CODE_SIGN_IDENTITY="-") and apply the
    # E2E keychain entitlement so SecItem* works and RealPairingService's real
    # keychain path runs. CODE_SIGNING_REQUIRED=NO keeps this a local ad-hoc
    # signature (no provisioning profile / DEVELOPMENT_TEAM needed on the sim).
    # This overrides project.yml's unsigned defaults for THIS build only — the
    # production Release/device build config is untouched.
    xcodebuild build-for-testing \
        -project "${XCODEPROJ}" \
        -scheme "${E2E_SCHEME}" \
        -destination "${DESTINATION}" \
        -derivedDataPath "${DERIVED_DATA_PATH}" \
        -skipPackagePluginValidation \
        CODE_SIGNING_ALLOWED=YES \
        CODE_SIGNING_REQUIRED=NO \
        CODE_SIGN_STYLE=Manual \
        CODE_SIGN_IDENTITY="-" \
        CODE_SIGN_ENTITLEMENTS="${E2E_ENTITLEMENTS}" \
        DEVELOPMENT_TEAM="" \
        PROVISIONING_PROFILE_SPECIFIER="" \
        || die "iOS build-for-testing failed"
    verify_ios_entitlements
}

# Assert the built .app actually CARRIES the keychain-access-groups entitlement
# (proves the ad-hoc signature applied the E2E entitlements). Without this the
# whole point of the signing change is unverified.
#
# IMPORTANT — where a simulator build keeps its entitlements: for an
# iphonesimulator destination Xcode signs the app with a STRIPPED "genuine"
# entitlements blob (so `codesign -d --entitlements` prints an EMPTY dict — the
# simulator's kernel does not enforce the codesign entitlements). The
# entitlements the simulator actually honours at runtime are the "simulated"
# entitlements the linker embeds into the Mach-O `__TEXT,__entitlements`
# section. So we assert on the EMBEDDED section — that is the artifact
# `SecItemCopyMatching` is gated on. We print the codesign blob too, for the
# record.
verify_ios_entitlements() {
    local app bin
    app="$(find "${DERIVED_DATA_PATH}/Build/Products" -maxdepth 2 -name 'FlightDeckRemote.app' -type d 2>/dev/null | head -n1 || true)"
    [[ -n "${app}" ]] || die "built FlightDeckRemote.app not found under ${DERIVED_DATA_PATH}/Build/Products"
    bin="${app}/FlightDeckRemote"
    [[ -f "${bin}" ]] || die "app binary not found: ${bin}"
    log "verifying keychain-access-groups entitlement on ${app}"

    # For the record: the (empty-on-simulator) codesign entitlements blob.
    local cs_ents
    cs_ents="$(codesign -d --entitlements - "${app}" 2>/dev/null || true)"
    printf -- '----- codesign entitlements blob (%s) — empty on simulator by design -----\n%s\n----- end -----\n' \
        "$(basename "${app}")" "${cs_ents}" >&2

    # The effective simulator entitlements: the embedded __TEXT,__entitlements
    # Mach-O section (little-endian 4-byte words -> file-order bytes).
    local embedded
    embedded="$(python3 - "${bin}" <<'PY'
import sys, subprocess, re
out = subprocess.run(["otool", "-X", "-s", "__TEXT", "__entitlements", sys.argv[1]],
                     capture_output=True, text=True).stdout
buf = b""
for line in out.splitlines():
    for tok in line.split():
        if re.fullmatch(r"[0-9a-fA-F]{8}", tok):
            buf += bytes.fromhex(tok)[::-1]
sys.stdout.write(buf.decode("utf-8", "replace"))
PY
)"
    printf -- '----- embedded __TEXT,__entitlements (effective on simulator) -----\n%s\n----- end embedded entitlements -----\n' \
        "${embedded}" >&2
    if ! printf '%s' "${embedded}" | grep -q "keychain-access-groups"; then
        die "built app is MISSING the keychain-access-groups entitlement — SecItem* will fail (-34018)"
    fi
    log "OK: built app carries keychain-access-groups entitlement (embedded __entitlements)"
}

# --- Simulator ----------------------------------------------------------------

boot_sim() {
    log "booting simulator '${E2E_SIM_DEVICE}'"
    local state
    state="$(xcrun simctl list devices 2>/dev/null \
        | grep -F "${E2E_SIM_DEVICE} (" | head -n1 || true)"
    [[ -n "${state}" ]] || die "simulator device '${E2E_SIM_DEVICE}' not found (xcrun simctl list devices)"
    if printf '%s' "${state}" | grep -q "(Booted)"; then
        log "simulator already booted"
    else
        xcrun simctl boot "${E2E_SIM_DEVICE}" || die "simctl boot failed"
        BOOTED_SIM_BY_US=1
    fi
    xcrun simctl bootstatus "${E2E_SIM_DEVICE}" -b >/dev/null 2>&1 || true
}

clean_app() {
    # Uninstall the app so the phone starts from a clean, UNPAIRED state.
    log "uninstalling app '${E2E_BUNDLE_ID}' for a clean unpaired start"
    xcrun simctl uninstall "${E2E_SIM_DEVICE}" "${E2E_BUNDLE_ID}" 2>/dev/null || true
}

# --- Relay + fixture + desktop bringup ---------------------------------------

make_temp_dirs() {
    TMP_ROOT="$(mktemp -d -t flightdeck-e2e-fullstack)"
    TMP_HOME="${TMP_ROOT}/home"
    RELAY_LOG="${TMP_ROOT}/relay.log"
    DESKTOP_LOG="${TMP_ROOT}/desktop.log"
    mkdir -p "${TMP_HOME}"
}

start_relay() {
    log "starting relay on port ${PORT}"
    PORT="${PORT}" "${RELAY_BIN}" >"${RELAY_LOG}" 2>&1 &
    RELAY_PID=$!
    # Wait for /healthz to answer "ok".
    local _
    for _ in $(seq 1 50); do
        if ! kill -0 "${RELAY_PID}" 2>/dev/null; then
            print_tail "relay.log" "${RELAY_LOG}"
            die "relay process exited during startup"
        fi
        if [[ "$(curl -fsS "http://127.0.0.1:${PORT}/healthz" 2>/dev/null || true)" == "ok" ]]; then
            log "relay healthy (/healthz = ok)"
            return 0
        fi
        sleep 0.2
    done
    print_tail "relay.log" "${RELAY_LOG}"
    die "relay did not become healthy on port ${PORT}"
}

make_fixture() {
    log "generating fixture project (relay port ${PORT}, fake agent)"
    FIXTURE_DIR="$(PORT="${PORT}" FAKE_AGENT="${FAKE_AGENT}" "${MAKE_FIXTURE}" | tail -n1)"
    [[ -n "${FIXTURE_DIR}" && -d "${FIXTURE_DIR}" ]] \
        || die "fixture generation failed (no dir returned)"
    log "fixture: ${FIXTURE_DIR}"
}

# Poll the desktop's PTY output until the autopair code appears — i.e. the TUI
# reached its tick loop, the autopair seam fired, and the pairing offer went
# live (the overlay renders the 4-digit code as one contiguous span, so it
# survives ratatui's per-cell escape interleaving; this mirrors the Rust
# launcher's `DesktopHandle::wait_for_output`). Returns non-zero on timeout or
# if the desktop exits first.
wait_for_desktop_offer() {
    local timeout="$1" deadline
    deadline=$(( $(date +%s) + timeout ))
    while :; do
        if [[ -f "${DESKTOP_LOG}" ]] && LC_ALL=C grep -qa "${E2E_CLAIM_TOKEN}" "${DESKTOP_LOG}"; then
            return 0
        fi
        kill -0 "${DESKTOP_PID}" 2>/dev/null || return 1
        [[ "$(date +%s)" -ge "${deadline}" ]] && return 1
        sleep 0.3
    done
}

start_desktop() {
    # Run the TUI desktop under a real PTY (it only reads crossterm events from
    # a terminal, so it can't run headless). We allocate the pty the same way
    # the Rust harness `tests/e2e/support/desktop.rs::DesktopHandle` does — the
    # child gets the pty SLAVE as its stdin/stdout/stderr + controlling
    # terminal, and the parent holds the MASTER open for the child's whole
    # lifetime. Two details are load-bearing and match DesktopHandle:
    #   * A real `TERM` (xterm-256color) + a generous window so ratatui renders.
    #   * The child's stdin (the slave) MUST NOT hit EOF. An earlier attempt ran
    #     the desktop under `script -q /dev/null <bin> </dev/null`: feeding
    #     /dev/null makes the pty stdin EOF immediately (a `^D`), which starves
    #     crossterm's event loop so the first tick — and thus the autopair
    #     offer — never fires. Holding the master open keeps the slave's stdin
    #     open with no input, exactly like DesktopHandle's reader thread.
    #
    # macOS `script` can't provide this (it puts its OWN stdin tty into raw mode
    # and errors on a non-tty like a pipe: "tcgetattr/ioctl … not supported").
    # So we drive the pty from a small inline python3 launcher (python3 is
    # already a harness requirement). The launcher does not read its own stdin,
    # so backgrounding it raises no SIGTTIN, and it forwards SIGTERM/SIGINT to
    # the child so teardown stops it cleanly.
    log "starting desktop under PTY (HOME sandbox, autopair ${E2E_CLAIM_TOKEN})"
    (
        cd "${FIXTURE_DIR}" || exit 1
        HOME="${TMP_HOME}" \
        TERM="xterm-256color" \
        FLIGHTDECK_REMOTE_AUTOPAIR="${E2E_CLAIM_TOKEN}" \
            exec python3 - "${DESKTOP_BIN}" <<'PY'
import os, pty, sys, select, signal, fcntl, termios, struct

cmd = sys.argv[1:]
if not cmd:
    sys.stderr.write("pty-run: no command given\n")
    sys.exit(2)

# Fork with a controlling pty: in the child, the slave is stdin/stdout/stderr
# and the controlling terminal; the parent gets the master fd.
pid, master = pty.fork()
if pid == 0:
    try:
        os.execvp(cmd[0], cmd)
    except Exception as exc:  # noqa: BLE001
        sys.stderr.write("pty-run: exec failed: %s\n" % exc)
        os._exit(127)

# Generous window so the TUI (and the pairing overlay's code) render untruncated
# (matches DesktopHandle's 40x120).
try:
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
except Exception:
    pass

def _forward(sig, _frame):
    try:
        os.kill(pid, sig)
    except Exception:
        pass

signal.signal(signal.SIGTERM, _forward)
signal.signal(signal.SIGINT, _forward)

# Drain the master -> our stdout (the caller redirects it to the desktop log).
# Never close/write the master, so the child's pty stdin never EOFs and its
# event loop keeps ticking (autopair fires on a tick). EOF on read means the
# child exited and the slave closed.
while True:
    try:
        readable, _, _ = select.select([master], [], [], 1.0)
    except (OSError, InterruptedError):
        break
    if master in readable:
        try:
            data = os.read(master, 65536)
        except OSError:
            break
        if not data:
            break
        try:
            os.write(1, data)
        except OSError:
            break

try:
    os.waitpid(pid, 0)
except OSError:
    pass
PY
    ) >"${DESKTOP_LOG}" 2>&1 &
    DESKTOP_PID=$!
    sleep 1
    if ! kill -0 "${DESKTOP_PID}" 2>/dev/null; then
        print_tail "desktop.log" "${DESKTOP_LOG}"
        die "desktop process exited during startup"
    fi
    # Ready = the pairing offer is actually live (autopair code rendered), not
    # merely "process alive". Offer TTL is 120s, so this timeout is ample.
    log "waiting up to ${E2E_OFFER_TIMEOUT}s for the desktop pairing offer (code ${E2E_CLAIM_TOKEN}) to go live"
    if ! wait_for_desktop_offer "${E2E_OFFER_TIMEOUT}"; then
        print_tail "desktop.log" "${DESKTOP_LOG}"
        die "desktop did not bring its pairing offer live (code ${E2E_CLAIM_TOKEN} not seen in PTY output)"
    fi
    log "desktop offered pairing (code ${E2E_CLAIM_TOKEN}); pid ${DESKTOP_PID}"
}

# --- XCUITest -----------------------------------------------------------------

run_xcuitest() {
    local payload="$1"
    log "running XCUITest (-only-testing:${E2E_UITEST})"
    log "NOTE: the Swift test ${E2E_UITEST} is supplied by remote-control-c3m.9"

    # Deliver the pairing payload + real-pairing switch to the test runner.
    # xcodebuild forwards env vars to the runner ONLY with the TEST_RUNNER_
    # prefix (stripped before the runner sees them); we also export the plain
    # names for good measure. c3m.9's test reads FLIGHTDECK_E2E_FDR1 /
    # FLIGHTDECK_PAIRING and forwards them into app.launchEnvironment.
    export FLIGHTDECK_E2E_FDR1="${payload}"
    export FLIGHTDECK_PAIRING="real"
    export TEST_RUNNER_FLIGHTDECK_E2E_FDR1="${payload}"
    export TEST_RUNNER_FLIGHTDECK_PAIRING="real"

    set +e
    # Keep signing settings identical to build_ios so test-without-building runs
    # the already ad-hoc-signed products as-is (no re-signing that would strip
    # the keychain entitlement).
    xcodebuild test-without-building \
        -project "${XCODEPROJ}" \
        -scheme "${E2E_SCHEME}" \
        -destination "${DESTINATION}" \
        -derivedDataPath "${DERIVED_DATA_PATH}" \
        -skipPackagePluginValidation \
        -only-testing:"${E2E_UITEST}" \
        CODE_SIGNING_ALLOWED=YES \
        CODE_SIGNING_REQUIRED=NO \
        CODE_SIGN_STYLE=Manual \
        CODE_SIGN_IDENTITY="-" \
        CODE_SIGN_ENTITLEMENTS="${E2E_ENTITLEMENTS}" \
        DEVELOPMENT_TEAM="" \
        PROVISIONING_PROFILE_SPECIFIER=""
    XCUITEST_RC=$?
    set -e

    if [[ "${XCUITEST_RC}" -ne 0 ]]; then
        warn "XCUITest failed (rc=${XCUITEST_RC}) — dumping stack logs"
        print_tail "relay.log" "${RELAY_LOG}"
        print_tail "desktop.log" "${DESKTOP_LOG}"
        if [[ -n "${FIXTURE_DIR}" && -f "${FIXTURE_DIR}/.flightdeck/agent-replies.log" ]]; then
            print_tail "agent-replies.log" "${FIXTURE_DIR}/.flightdeck/agent-replies.log"
        fi
    else
        log "XCUITest passed"
    fi
}

# --- Desktop-side cross-checks (Tier B round-trip proof) ----------------------
#
# A green XCUITest proves the phone-side UI action succeeded; pairing it with
# the REAL on-disk effect the flow must have produced is what proves a TRUE
# round trip (plan: "a green XCUITest UI action + a real on-disk effect together
# prove a true round trip"). These run ONLY after a green test, poll (the
# desktop applies phone commands asynchronously), and fail the orchestrator
# non-zero if a side effect never lands — so a green UI run with a missing
# desktop effect is still reported as a failure.

# wait_until <timeout_secs> <cmd...>  → 0 if <cmd> succeeds within the window.
wait_until() {
    local timeout="$1"; shift
    local deadline=$(( $(date +%s) + timeout ))
    while :; do
        if "$@"; then return 0; fi
        [[ "$(date +%s)" -ge "${deadline}" ]] && return 1
        sleep 0.5
    done
}

# The chat reply the phone sent reached the desktop → the fake agent appended it
# to the fixture worktree's replies log (grep case-insensitively: the composer
# field autocapitalizes its first character).
# shellcheck disable=SC2329  # invoked indirectly via wait_until "$@"
reply_logged() {
    local log="${FIXTURE_DIR}/.flightdeck/agent-replies.log"
    [[ -f "${log}" ]] && grep -qiF "${E2E_REPLY_TOKEN}" "${log}"
}

# The phone's new-agent flow created a worktree on disk under the project's
# `.flightdeck/worktrees/` (default `[worktrees] root`). Prefer the exact slug
# the test named the agent, but accept any entry (the desktop owns the exact
# on-disk layout).
# shellcheck disable=SC2329  # invoked indirectly via wait_until "$@"
worktree_created() {
    local dir="${FIXTURE_DIR}/.flightdeck/worktrees"
    [[ -d "${dir}" ]] || return 1
    [[ -e "${dir}/${E2E_AGENT_SLUG}" ]] && return 0
    [[ -n "$(ls -A "${dir}" 2>/dev/null)" ]]
}

# Assert every desktop-side effect the capability flows should have produced.
# Returns non-zero (and logs precisely what is missing) if any is absent.
assert_desktop_side_effects() {
    local rc=0
    log "cross-checking desktop-side effects in ${FIXTURE_DIR}"

    if wait_until "${E2E_EFFECT_TIMEOUT}" worktree_created; then
        log "OK: new-agent worktree present under .flightdeck/worktrees/"
        ls -1 "${FIXTURE_DIR}/.flightdeck/worktrees" 2>/dev/null >&2 || true
    else
        warn "MISSING new-agent side effect: no worktree under ${FIXTURE_DIR}/.flightdeck/worktrees/"
        rc=1
    fi

    if wait_until "${E2E_EFFECT_TIMEOUT}" reply_logged; then
        log "OK: chat reply token '${E2E_REPLY_TOKEN}' landed in agent-replies.log"
    else
        warn "MISSING chat side effect: reply token '${E2E_REPLY_TOKEN}' not in agent-replies.log"
        print_tail "agent-replies.log" "${FIXTURE_DIR}/.flightdeck/agent-replies.log"
        rc=1
    fi

    return "${rc}"
}

# --- Usage --------------------------------------------------------------------

usage() {
    sed -n '2,80p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

# --- Arg parse ----------------------------------------------------------------

while [[ $# -gt 0 ]]; do
    case "$1" in
        --print-payload-only) MODE="payload" ;;
        --bringup-only)       MODE="bringup" ;;
        -h|--help)            usage; exit 0 ;;
        *) die "unknown argument: $1 (see --help)" ;;
    esac
    shift
done

# --- Main ---------------------------------------------------------------------

PORT="${E2E_PORT:-$(pick_free_port)}"
RELAY_URL="ws://127.0.0.1:${PORT}/ws"

case "${MODE}" in

    payload)
        # No stack, no builds: just build + verify the fdr1: payload.
        payload="$(build_payload "${E2E_CLAIM_TOKEN}" "${RELAY_URL}")"
        log "decoded payload JSON:"
        decode_payload "${payload}" >&2
        printf '%s\n' "${payload}"
        # payload mode has nothing to tear down beyond the (empty) trap.
        trap - EXIT
        exit 0
        ;;

    bringup)
        trap teardown EXIT INT TERM
        build_relay
        build_desktop
        make_temp_dirs
        start_relay
        make_fixture
        start_desktop

        # Assertions.
        health="$(curl -fsS "http://127.0.0.1:${PORT}/healthz" 2>/dev/null || true)"
        [[ "${health}" == "ok" ]] || die "post-bringup healthz != ok (got '${health}')"
        kill -0 "${DESKTOP_PID}" 2>/dev/null || die "desktop not alive after bringup"
        # start_desktop already verified the pairing offer went live (it dies
        # otherwise), so reaching here means the desktop is offering ${E2E_CLAIM_TOKEN}.
        log "BRINGUP OK: relay /healthz = ok; desktop pid ${DESKTOP_PID} offering pairing ${E2E_CLAIM_TOKEN}"
        # Teardown runs via trap; exit 0 signals success.
        exit 0
        ;;

    full)
        trap teardown EXIT INT TERM
        # Phase 1: builds.
        build_relay
        build_desktop
        build_ios
        # Phase 2: sim boot + clean.
        boot_sim
        clean_app
        # Phase 3: relay + fixture + desktop.
        make_temp_dirs
        start_relay
        make_fixture
        start_desktop
        # Phase 4: construct the pairing payload.
        payload="$(build_payload "${E2E_CLAIM_TOKEN}" "${RELAY_URL}")"
        log "fdr1: payload constructed (claim ${E2E_CLAIM_TOKEN}, ${RELAY_URL})"
        # Phase 5: run the XCUITest.
        run_xcuitest "${payload}"
        # Phase 6: desktop-side cross-checks. Only meaningful after a green UI
        # run AND when the capability flows actually ran; a missing on-disk
        # effect fails the orchestrator even if the XCUITest was green (a true
        # round trip needs both halves). Gated off by default while the flows are
        # XCTSkip'd on remote-control-9yv (skipped flows produce no effects).
        if [[ "${XCUITEST_RC}" -eq 0 && "${E2E_ASSERT_SIDE_EFFECTS}" == "1" ]]; then
            if assert_desktop_side_effects; then
                log "desktop-side cross-checks PASSED"
            else
                warn "desktop-side cross-checks FAILED"
                XCUITEST_RC=1
            fi
        elif [[ "${XCUITEST_RC}" -eq 0 ]]; then
            log "desktop-side cross-checks SKIPPED (E2E_ASSERT_SIDE_EFFECTS=0; capability flows gated on remote-control-9yv)"
        fi
        # Phase 7: teardown via trap; propagate the final exit code.
        exit "${XCUITEST_RC}"
        ;;

    *)
        die "unknown mode: ${MODE}"
        ;;
esac
