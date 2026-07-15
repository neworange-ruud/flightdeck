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
#   — is delivered by a SEPARATE issue, remote-control-c3m.9. This orchestrator
#   references it by name (`-only-testing:FlightDeckRemoteUITests/RemoteLiveE2EUITests`)
#   but does NOT create it. Until c3m.9 lands, the full run (default mode) will
#   fail at the `test-without-building` phase with "test not found" — that is
#   expected. Use `--bringup-only` / `--print-payload-only` to exercise the
#   parts that do not depend on the Swift test.
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
#                         and the desktop process is alive, then tear down. Fast;
#                         for verifying the Rust bringup + teardown in isolation.
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
#   E2E_SETTLE_SECS  Seconds to let the desktop bring its pairing offer live
#                    after the relay is healthy. Default: 4
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
E2E_SETTLE_SECS="${E2E_SETTLE_SECS:-4}"

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

    # Clean temp dirs (fresh HOME sandbox + fixture + logs).
    if [[ -n "${TMP_ROOT}" && -d "${TMP_ROOT}" ]]; then
        rm -rf "${TMP_ROOT}" 2>/dev/null || true
    fi
    if [[ -n "${FIXTURE_DIR}" && -d "${FIXTURE_DIR}" && "${FIXTURE_DIR}" != "${TMP_ROOT}"* ]]; then
        rm -rf "${FIXTURE_DIR}" 2>/dev/null || true
    fi
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
    log "building iOS app for testing (${DESTINATION})"
    xcodebuild build-for-testing \
        -project "${XCODEPROJ}" \
        -scheme "${E2E_SCHEME}" \
        -destination "${DESTINATION}" \
        -derivedDataPath "${DERIVED_DATA_PATH}" \
        -skipPackagePluginValidation \
        CODE_SIGNING_ALLOWED=NO \
        || die "iOS build-for-testing failed"
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

start_desktop() {
    # Run the TUI desktop under a PTY (it needs a real TTY; macOS `script -q`
    # provides one). cwd = fixture, HOME = fresh sandbox, autopair = claim token.
    # stdin < /dev/null so backgrounding does not stop it on SIGTTIN.
    log "starting desktop under PTY (HOME sandbox, autopair ${E2E_CLAIM_TOKEN})"
    (
        cd "${FIXTURE_DIR}" || exit 1
        HOME="${TMP_HOME}" \
        FLIGHTDECK_REMOTE_AUTOPAIR="${E2E_CLAIM_TOKEN}" \
            exec script -q /dev/null "${DESKTOP_BIN}"
    ) </dev/null >"${DESKTOP_LOG}" 2>&1 &
    DESKTOP_PID=$!
    # Let the desktop reach its tick loop, register keys, and bring the pairing
    # offer live at the relay. Offer TTL is 120s, so a short settle is ample.
    sleep 1
    if ! kill -0 "${DESKTOP_PID}" 2>/dev/null; then
        print_tail "desktop.log" "${DESKTOP_LOG}"
        die "desktop process exited during startup"
    fi
    log "settling ${E2E_SETTLE_SECS}s for the pairing offer to go live"
    sleep "${E2E_SETTLE_SECS}"
    if ! kill -0 "${DESKTOP_PID}" 2>/dev/null; then
        print_tail "desktop.log" "${DESKTOP_LOG}"
        die "desktop process died before pairing could be offered"
    fi
    log "desktop alive (pid ${DESKTOP_PID})"
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
    xcodebuild test-without-building \
        -project "${XCODEPROJ}" \
        -scheme "${E2E_SCHEME}" \
        -destination "${DESTINATION}" \
        -derivedDataPath "${DERIVED_DATA_PATH}" \
        -skipPackagePluginValidation \
        -only-testing:"${E2E_UITEST}" \
        CODE_SIGNING_ALLOWED=NO
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
        log "BRINGUP OK: relay /healthz = ok; desktop pid ${DESKTOP_PID} alive"
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
        # Phase 6: teardown via trap; propagate the test's exit code.
        exit "${XCUITEST_RC}"
        ;;

    *)
        die "unknown mode: ${MODE}"
        ;;
esac
