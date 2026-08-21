# FlightDeck Remote — end-to-end test harness

This directory holds the harness that stands up the **real** FlightDeck Remote
stack together — relay + desktop + (optionally) the real iOS app — and exercises
every remote capability end to end. It is self-setup and self-verifying.

There are two layered tiers:

- **Tier A — protocol E2E (Rust, no simulator).** A real relay + a real desktop
  running under a PTY + a Rust "phone" driver. Fast, deterministic, CI-friendly.
  This is the CI gate.
- **Tier B — full-stack E2E (simulator).** The same real relay + real desktop,
  plus the **real iOS app** in the simulator, driven by XCUITest and paired live
  to the local relay.

Both tiers reuse the same fixture project, fake agent, and relay/desktop
launchers.

## Tier A — `cargo test --test remote_e2e`

```sh
cargo test --test remote_e2e
```

`tests/remote_e2e.rs` (support code under `tests/e2e/support/`) boots a real
relay on a free port, launches the real `flightdeck` desktop binary under a PTY
against a generated fixture repo, pairs a Rust phone driver over the real relay
(deterministic claim token `4729`), and asserts every capability against real
side effects: initial snapshot / `request_snapshot`, `new_agent` (a worktree
appears under `.flightdeck/worktrees/` and the fake agent goes `working`→`idle`),
`reply` (lands in `.flightdeck/agent-replies.log`), `permission_decision`,
set/clear `manual_status`, `restart_agent`, `close_session`, git
pull/merge/abandon (worktree removed, `confirm_name` echo), shell
open/input/interrupt/close, and transcript. Everything is torn down on drop; no
processes leak.

Nothing extra is required — the harness builds the relay and desktop itself.

## Tier B — `scripts/e2e/run-fullstack.sh`

```sh
scripts/e2e/run-fullstack.sh                 # full run: build, boot sim, pair live, run XCUITest
scripts/e2e/run-fullstack.sh --bringup-only  # relay + fixture + desktop offer only (fast; no iOS build)
scripts/e2e/run-fullstack.sh --print-payload-only   # just build + print the fdr1: pairing payload
```

The orchestrator builds the relay + desktop + the iOS app for testing, boots and
cleans the simulator, brings up a live relay + fixture + PTY-hosted desktop
(autopair `4729`), constructs the `fdr1:` pairing payload, runs the XCUITest
(`FlightDeckRemoteUITests/RemoteLiveE2EUITests`) against the live stack, and tears
everything down via an `EXIT` trap. It exits non-zero on any failure.

### Prerequisites

- Xcode with an **installed iOS Simulator runtime** and an `iPhone 16 Pro`
  device. The harness targets the installed runtime (default `iOS 26.5`), *not*
  the `OS=18.4` that `ios/scripts/test.sh` pins (18.4 is not installed here; the
  app's deployment target of 18.0 runs fine on 26.5).
- `xcodegen`, `cargo`, `python3`, `openssl`, `xcrun`/`simctl` (all standard on the
  dev machine).

### Environment knobs

| Var | Default | Meaning |
|---|---|---|
| `E2E_SIM_DEVICE` | `iPhone 16 Pro` | Simulator device name |
| `E2E_SIM_OS` | `26.5` | Installed simulator runtime to target |
| `E2E_SCHEME` | `FlightDeckRemote` | Xcode scheme |
| `E2E_UITEST` | `FlightDeckRemoteUITests/RemoteLiveE2EUITests` | Test to run |
| `E2E_CLAIM_TOKEN` | `4729` | Autopair code (`FLIGHTDECK_REMOTE_AUTOPAIR`) |
| `E2E_PORT` | auto (free) | Relay port |
| `E2E_OFFER_TIMEOUT` | `30` | Seconds to wait for the desktop pairing offer to go live |
| `E2E_ASSERT_SIDE_EFFECTS` | `0` | Assert desktop-side effects of the capability flows (see the gate below) |
| `E2E_KEEP_SIM` | unset | Leave the simulator booted after the run |

### How Tier B works around the environment

- **Desktop under a PTY.** The desktop is a TUI and needs a real terminal, so the
  orchestrator launches it under a `python3` `pty.fork()` (the child gets the pty
  slave as its controlling terminal, and its stdin never hits EOF — otherwise the
  crossterm event loop stalls and the autopair offer never fires). This mirrors
  the Rust harness's `tests/e2e/support/desktop.rs`.
- **Keychain on the simulator.** `RealPairingService` reads/writes the device
  identity via the data-protection Keychain, which requires an entitlement. An
  unsigned build (`CODE_SIGNING_ALLOWED=NO`) gets `errSecMissingEntitlement`, so
  the harness **ad-hoc signs** the simulator build (`CODE_SIGN_IDENTITY="-"`) with
  a dedicated `scripts/e2e/e2e.entitlements` (`keychain-access-groups` +
  `application-identifier`). The production entitlements and `ios/project.yml` are
  untouched.
- **fdr1: payload.** The phone learns the relay URL only from the QR/`fdr1:`
  payload, so the orchestrator constructs it (`fdr1:` + base64url-no-pad JSON with
  `claim_token`, `relay_url`, a random `pairing_secret`) and hands it to the
  XCUITest via `FLIGHTDECK_E2E_FDR1`. The test types it into the DEBUG QR paste
  field — no camera, no iOS production change.

### Known gate

`testLiveRemoteCapabilityFlows` (the chat / new-agent / shell / git flows) and the
orchestrator's desktop-side cross-checks (`E2E_ASSERT_SIDE_EFFECTS`, default off)
are currently gated with `XCTSkip` on **remote-control-9yv**: after a live
new-agent creation the phone link drops to non-`.connected` (which disables chat
send) and does not recover in time. Live pairing itself is proven by
`testLivePairingReachesMainTabView`. Re-enable the flows and set
`E2E_ASSERT_SIDE_EFFECTS=1` once 9yv is fixed.

## Building blocks

- `make-fixture-project.sh` — git-inits a temp repo with an initial commit on
  `main` and a `.flightdeck/config.toml` (remote enabled, `relay_url` with the
  chosen port, the fake agent under key `claude`, `default_agent = claude`).
  Env: `PORT`, `FAKE_AGENT`. Prints the fixture path as its last line.
- `fake-agent.sh` — a deterministic, network/LLM-free agent stand-in. Appends
  `working`/`idle`/`waiting` to `.flightdeck/agent-status`, logs phone replies to
  `.flightdeck/agent-replies.log`, and emits `waiting` on the `__WAIT__` sentinel.
- `e2e.entitlements` — the E2E-only keychain entitlement used to ad-hoc sign the
  simulator test build (see above).
