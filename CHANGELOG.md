# Changelog

All notable changes to FlightDeck will be documented in this file.

Future releases should group notes under `New features`, `Improvements`, and `Bug fixes` so the repo changelog and GitHub Releases stay aligned.

## [Unreleased]

### New features

- **`flightdeck --isolated` / `-I`: a throwaway run.** Launches exactly one
  fresh Agent Session Tab, running the default agent in the repository root on
  the branch already checked out — no worktree, no git mutation, and nothing
  continued from a previous run (`state.json` and the workspace file are never
  read). FlightDeck writes nothing of its own for the whole run: no first-run
  `config.toml`, no `.gitignore` entry, no global config base, and nothing on
  exit either, though existing config on disk is still read and honoured.
  `ui.auto_continue` is forced off (so even Restart Agent stays a fresh
  session) and the update check is disabled (no network call, no cache
  write). Agent status plumbing (the Claude/Codex/OpenCode lifecycle hooks)
  is redirected to a per-process temp directory outside the project, removed
  after every session is terminated; a containerized run is the one
  exception and keeps writing into the bind-mounted worktree, since a temp
  directory outside it would not be reachable from inside the container.
  Open Project, Close Project, Next/Previous Project, and New Agent Session
  Tab are refused with one consistent message (and hidden from the command
  palette) since an isolated run is one session with nothing else to switch
  to; Finish/Local Merge, Rebase, and Abandon Worktree are already refused
  for free because the tab runs on the base branch. A permanent `ISOLATED`
  badge in the status bar and a leading note in the help overlay (`?`) make
  the mode unmistakable. Combining the flag with a subcommand
  (`flightdeck -I doctor`) is a startup error rather than a silent ignore.
  A normal run's status *root* is unchanged (still the worktree), but its
  generated hook bodies now carry an absolute status-file path instead of
  the old cwd-relative one, so a normal run's hooks are not byte-identical
  to before this series. "Open Configuration" stays available in an
  isolated run (it is a viewer, not a write action) but no longer creates
  `~/.flightdeck/config.toml` merely by being opened, and saving from it can
  no longer undo the forced `ui.auto_continue = false` for the run — both
  guards live where the config is replaced (`AppState::reload_config`), so
  they hold regardless of call site. Closing the only session tab quits the
  run — an isolated run is that one session, and every route back to an agent
  is refused, so staying open would leave an empty shell whose only exit is
  Ctrl-q. See SPECS §32.

### Improvements

- None yet.

### Bug fixes

- A base-branch Agent Session Tab now records the branch actually checked out
  rather than the configured base, so Push Branch pushes the right ref. A
  detached HEAD (which git reports as the literal string `"HEAD"`, not an
  error) and a genuine git failure both fall back to the base branch instead.

## [1.16.0] - 2026-08-24

### New features

- None yet.

### Improvements

- None yet.

### Bug fixes

- **Remote now works on Windows.** Enabling remote on a Windows build failed
  immediately with `wss is not supported on this build`, so the phone could
  never reach the hosted relay — the Windows binary was compiled without any TLS
  backend, because the one used everywhere else needs a C toolchain that the
  release build deliberately avoids. Windows now speaks `wss://` through
  SChannel, the TLS stack built into the OS, and the binary stays free of that C
  toolchain.

## [1.15.0] - 2026-08-21

### New features

- Open the selected agent session tab's worktree in the OS file manager, from
  the command palette or `Alt-O` (works with a terminal focused). Override the
  launcher with `ui.file_manager` in `config.toml`.

### Improvements

- None yet.

### Bug fixes

- **Remote: the phone no longer goes permanently silent until you re-pair.** A
  pairing could reach a state where the relay and the endpoint disagreed about
  where the envelope stream had got to, and nothing was allowed to fix it. The
  relay only ever adopts a sender's numbering for a stream it has never seen
  before; past that it insists on exactly the next sequence number. Both the Mac
  and the phone, meanwhile, were forbidden from renumbering a stream they were
  already sending — a deliberate rule, because blindly restarting at 1 against a
  relay that remembers its position is what caused an earlier livelock. So once a
  sender got ahead, the relay rejected every envelope, the sender counted merrily
  past it, and the gap grew without bound: one live pairing was found with the
  relay waiting for number 98 while the Mac had reached 38,315, and 65,379
  consecutive rejections over 17 days. Both directions could wedge independently,
  so the phone showed nothing and sent nothing while its connection, its
  authentication and its "connected" indicator all looked perfectly healthy. Only
  re-pairing helped, and only until it happened again — each fresh pairing
  survived about a hundred envelopes. The relay now names the sequence number it
  will accept next when it rejects one, and the Mac and the phone realign to it
  and re-send a full snapshot. Realigning to a position the relay itself supplied
  always converges on the very next envelope, which is what makes it safe where
  the old blind restart was not. A sender that keeps ignoring the correction is
  now logged as a wedged stream instead of passing for routine chatter.

## [1.14.0] - 2026-07-28

### New features

- None yet.

### Improvements

- None yet.

### Bug fixes

- **Remote: a second pairing to the same Mac no longer strands the phone on a
  dead session list.** Re-pairing a phone left the older pairing behind, and both
  stayed live: the Mac still had it, the relay still authenticated it, and the
  phone still opened a connection for it. But the desktop feeds exactly one
  pairing, so the leftover received nothing — while looking perfectly healthy,
  because it connected and authenticated like any other. The phone preferred the
  oldest connected pairing, which was reliably the leftover, so the Projects tab
  bound to a session list that could never update; pull-to-refresh couldn't help
  either, since the Mac can only answer on the pairing it feeds. Claiming a new
  pairing now retires any earlier one to the same phone — locally and at the relay
  — and the phone drops a pairing the relay reports as revoked. The phone also
  picks the pairing that is actually receiving data rather than the oldest one
  that merely connected, so an existing duplicate stops causing harm immediately,
  and it resolves that choice live instead of freezing the pre-connect answer.
  Pairing several *different* phones is unaffected.
- **Remote: the phone no longer shows agents for sessions that are gone.** After
  a reconnect the session list could stay frozen on whatever it last saw — an
  agent for a deleted worktree stayed on screen indefinitely — while the statuses
  of sessions it already knew kept updating live, which made it look like a
  rendering quirk rather than a sync failure. A full snapshot is the only message
  that can add or remove a session (a `status_update` can only change sessions
  the phone already knows), and two separate holes stopped one from being sent.
  On the desktop, a fresh snapshot was armed only when a phone went from absent
  to present; but when a phone's socket dies half-open — the normal case when iOS
  suspends the app — the relay holds the stale leg until its idle timeout, and the
  reconnecting phone supersedes it, which by design produces no disconnect. The
  desktop therefore saw "connected" with the phone already marked present, found
  no edge, and sent nothing. On the phone, the desktop's presence was remembered
  across sockets instead of per session, which defeated the backstop from both
  directions: a stale "absent" made the post-authentication snapshot request fail
  fast as *peer unavailable*, and a stale "present" made the desktop's return look
  like no change and skip the re-request. Every phone connection now re-arms the
  snapshot, and peer presence resets with each session.
- **Remote: opening the phone app after a while no longer flaps between
  "Reconnecting…" and a one-second connection.** Reopening the app after it had
  been idle put it in a loop: connected for about a second, back to
  "Reconnecting…", over and over, indefinitely. The silent APNs wake was the
  culprit. iOS delivers `content-available` pushes to a *foregrounded* app too,
  and the wake performer ended with an unconditional teardown of the whole
  transport — so a wake that landed as the user opened the app killed the link
  they were looking at. That detached the phone's leg at the relay, and the relay
  pushes a wake for every desktop→phone envelope that arrives with no peer
  attached, so each new event triggered another wake: connect, tear down,
  connect, tear down. Nothing broke the cycle, because the wake also claimed the
  foreground flag on its way in, which made the real foreground transition a
  no-op, and its teardown cancelled the network-path monitor that would otherwise
  have forced a reconnect. The wake is now bracketed by a begin/end pair that
  declines outright while the app is foregrounded and skips its teardown if the
  app came to the foreground mid-wake; foreground ownership of the transport now
  belongs to the scene-phase lifecycle alone.

## [1.13.0] - 2026-07-27

### New features

- **Tagging a release now ships a TestFlight build.** Getting the iOS app to
  testers was a manual sequence — archive, export, upload — that only worked on
  one Mac with the right credentials in its keychain, so in practice the phone
  build drifted behind the desktop release. A `TestFlight` workflow now runs on
  the same version-tag push that triggers the relay and web deploys: it imports
  the Apple Distribution certificate into a throwaway keychain, installs the App
  Store provisioning profile, and archives, exports and uploads.
  `CFBundleVersion` comes from the workflow run number, which strictly increases
  per workflow run — `scripts/release` resets the value in `project.yml` to `1`
  on every version bump, so a second upload of one version would otherwise
  collide. See `ios/README.md` "Distribution (TestFlight)".
- **Remote: push notifications actually reach the phone now.** The relay has
  never been able to wake an offline phone. Two things were missing, and both
  failed silently: the deployed image was built without the `apns-live` feature,
  which compiles the APNs transport out and leaves a no-op sender in its place;
  and the container app had none of the four `APNS_*` variables, so
  `ApnsConfig::from_env` returned `None` — a deliberate "disable push rather than
  fail startup" choice that, with nothing reporting the decision, looked exactly
  like a healthy relay. The 1.10.0 notes said push "requires an Apple APNs auth
  key + signing team"; the signing team arrived in 1.12.0, the key half never
  did. The Dockerfile now builds with the feature, `relay-deploy.yml` injects and
  re-asserts the config on every deploy (and raises a workflow warning rather
  than quietly shipping a mute relay), and the auth key is injected inline from a
  secret via `APNS_AUTH_KEY_PEM` instead of needing a mounted volume for one
  file. Verified against Apple's production endpoint. See
  `remote/relay/deploy/README.md` "Push notifications (APNs)".

### Improvements

- **`ios/scripts/archive.sh` reads credentials from `.env`.** It required three
  `FD_ASC_*` variables exported by hand, and passed none of them to
  `xcodebuild` — so archiving depended on whichever Apple Account happened to be
  signed in to Xcode, and on a machine with no distribution certificate it
  quietly signed with the development identity and only failed later at export.
  It now loads a gitignored root `.env`, expands a `~` in the key path, fails
  fast if the `.p8` is missing, and authenticates `xcodebuild` with the same API
  key it uses for the upload. That is also what makes the CI path possible.
- **`.env`, `*.p8`, `*.p12` and `*.mobileprovision` are gitignored.** None of
  them were, and the App Store Connect key lands at the repo root by
  convention.

### Bug fixes

- **Remote: a phone that lost its send cursor no longer reconnects forever.**
  Installing a TestFlight build over an existing one left a phone whose pairing,
  keys and inbound feed all still worked, but whose every outbound command died
  — presenting as "reconnecting…" roughly 25×/second rather than as a rejection
  loop, because desktop→phone traffic kept flowing the whole time. The relay
  expected envelope seq 60 and the phone sent 1, so the relay answered
  `seq_violation`, whose contract is "drop your stale cursor and resync" — but a
  phone that already lost its cursor has nothing to drop and nothing to move
  forward to, so it restarted at 1 and was rejected again, at reconnect speed,
  indefinitely.

  The root cause was a single overloaded signal. `seq_violation` is emitted for
  two unrelated conditions — "your outbound seq is wrong" and, from the `resume`
  path, "your *inbound* cursor is stale" — and both endpoints responded to
  either by rewinding their **outbound** cursor to 0. So a phone that had simply
  been offline long enough for the desktop→phone queue to pass its 1000-envelope
  bound would destroy a perfectly good send cursor on its next reconnect. That
  rewind was correct against the original in-memory relay, which came back from
  a restart with no watermark at all; against the SQLite relay, which persists
  its watermark, it cannot terminate.

  The relay now treats its `high_water` as a cache of the sender's cursor rather
  than an independent authority. A stream it has no record of adopts whatever
  seq the first envelope carries (the relay-restart case, handled without any
  endpoint rewind), and a seq *below* the watermark is recognised as a sender
  that restarted: the relay abandons its own epoch for that direction, adopts
  the restarted cursor, and tells the peer to drop its now-stale inbound cursor
  before forwarding — otherwise the peer dedups the whole new epoch away as
  duplicates. A peer that was offline for that advisory is caught on its next
  `resume`, which now reports a resync when the cursor sits above the stream.
  Only a genuine forward gap is still rejected. `seq_violation` means exactly
  one thing on both endpoints now — "your inbound cursor is stale" — and neither
  ever renumbers a stream it is successfully sending. Both also accept a peer's
  restart at seq 1 instead of deduping it away; the phone already did, the
  desktop did not. Reconnect backoff additionally requires a session to *last*
  30s, not merely reach `auth_ok`, before it clears the schedule — a session
  that authenticates and dies instantly used to pin the retry at its 1s floor.
  See `specs/REMOTE_PROTOCOL.md` §6.1/§6.3/§6.4.
- **Remote: the iOS app really is iPhone-only now.** `project.yml` said "iPhone
  only, v1" and set `TARGETED_DEVICE_FAMILY` project-wide, but XcodeGen gives
  every iOS target a default of `"1,2"` and a target-level setting beats a
  project-level one — so the value never took effect and the app shipped
  declaring `UIDeviceFamily [1, 2]`. Being iPad-capable put it under Apple's
  iPad multitasking rules, which require all four interface orientations, and
  the first TestFlight upload was rejected with ITMS-90474. The setting now sits
  on the app target, the pointless `UISupportedInterfaceOrientations~ipad`
  override is gone, and two `DistributionMetadataTests` assert both so it cannot
  regress into another upload-time failure.
- **Remote: the iOS build number can actually be overridden now.**
  `ios/project.yml` wrote `CFBundleVersion` as a literal `"1"`, and a literal is
  baked into the generated Info.plist where no build setting can reach it — so
  `FD_BUILD_NUMBER` (and `CURRENT_PROJECT_VERSION` generally) was a silent
  no-op. Building with `CURRENT_PROJECT_VERSION=99` produced a build still
  numbered 1, which means every CI upload would have carried the same build
  number, and App Store Connect refuses a reused one. `CFBundleVersion` is now
  `"$(CURRENT_PROJECT_VERSION)"`, the default sits on the app target, and
  `scripts/release` resets that setting rather than rewriting the reference into
  a literal again. `DistributionMetadataTests.buildNumberIsAPositiveInteger`
  already covered the shape and now catches an unsubstituted reference too.

## [1.12.0] - 2026-07-26

### New features

- **Remote: start a new agent on any paired machine.** The New-Agent sheet's
  project picker only ever listed projects from a single machine, so a session
  could only be launched on that one Mac — every project on your other paired
  machines was silently unreachable. The picker now aggregates projects across
  every paired machine (matching the Projects tab), tags each with a
  machine-name indicator so same-named projects stay distinguishable, and routes
  the launch — and the commands-paused gate — to the selected project's own
  machine (remote-control-cyj).
- **Remote: choose the dictation input language.** The push-to-talk mic in
  chat used to transcribe in whatever language the phone's locale implied
  (English for most users). Settings → Voice now offers an explicit language
  picker (English / Nederlands); the choice is persisted and applies to the
  next hold. Adding more languages is a one-line change to `SpeechLanguage`.
- **Remote: the iOS app can be shipped to TestFlight.** The project had never
  been configured for distribution — no signing team, no privacy manifest, no
  export-compliance declaration — so no build could reach App Store Connect at
  all. It now signs against team `7NKCS4AZS9` with automatic signing, ships an
  `aps-environment` entitlement per configuration (development for Debug,
  production for Release, which is what TestFlight builds need), declares its
  required-reason API use in `PrivacyInfo.xcprivacy`, and answers export
  compliance once via `ITSAppUsesNonExemptEncryption`. `ios/scripts/archive.sh`
  archives, exports and optionally uploads a build;
  `FlightDeckRemoteTests/DistributionMetadataTests.swift` guards the bundle
  metadata that only fails at upload time. See `ios/README.md`
  "Distribution (TestFlight)".
- **A published privacy policy, linked from the app.**
  <https://www.flightdeckai.app/privacy> documents what actually leaves the
  device: messages are end-to-end encrypted and the relay holds only ciphertext,
  but it does see device ids, pairing ids, push tokens, machine names and
  message timing, and voice dictation goes to Apple's speech recognition rather
  than staying on-device. Settings → About links it, and App Store Connect
  requires it.

### Improvements

- **Remote: the iOS app reports a real version.** It claimed `1.0 (1)`
  regardless of the release it shipped from. The marketing version now tracks
  the FlightDeck release train (`1.11.0`), and `scripts/release` rewrites
  `ios/project.yml` alongside `Cargo.toml` so the phone and the desktop can no
  longer drift apart.
- **CI builds and tests the iOS app.** Nothing under `ios/` was covered by any
  workflow. A new `iOS` workflow runs the unit suite on a simulator and — the
  part that matters — compiles the Release configuration for a device, which is
  what TestFlight ships. It is path-filtered to `ios/**` because macOS runners
  bill at 10x. The UI test target stays excluded: `ShellUITests` alone takes
  ~25 minutes (remote-control-7lr), so UI regressions are still only caught by
  running `ios/scripts/test.sh` locally.
- **Remote: relay restarts are now covered end to end.** The "phone prompt runs
  on the desktop but nothing ever comes back" stall (remote-control-bbf) was
  fixed across all three tiers in 1.10.0, but only by unit tests on each side —
  nothing exercised a real relay actually restarting mid-pairing, which is how
  the bug reached users in the first place. The Tier A E2E suite now restarts the
  real relay binary under a live pairing and asserts the phone keeps receiving
  the desktop feed: once with the persistent store (everything survives, the
  stream continues), and once with the desktop→phone sequence state deliberately
  wiped, which reproduces the original divergence and proves the full recovery
  path — relay reports `seq_violation`, desktop re-syncs from a fresh snapshot,
  phone accepts the stream reset. The phone driver also mirrors the iOS receive
  cursor (persisted across reconnects, dedup + reset acceptance), so a
  regression on any tier fails the suite instead of only showing up in
  production.

### Bug fixes

- **Remote: the iOS app compiles in the Release configuration.** Every
  `#Preview` body referencing a DEBUG-only fixture broke the Release build,
  because the `#Preview` macro expands regardless of `ENABLE_PREVIEWS` while
  `DebugFixtures.swift` is wrapped in `#if DEBUG`. Debug and the simulator test
  suite stayed green throughout, so nothing surfaced it — the app simply could
  not be archived. All 31 preview blocks are now `#if DEBUG`-guarded, and CI
  builds Release for a device so it cannot regress.
- **Remote: the shipped `aps-environment` matches the signing profile.** The
  committed entitlement hardcoded `development` on the assumption that a
  distribution profile would override it at signing time; it does not — the
  entitlement has to match, or codesign fails. A Release build now carries
  `production`, which is what TestFlight and App Store builds are signed
  against.
- **Remote: your own reply now scrolls into view when you send it.** Opening a
  session that was waiting on a permission prompt scrolls the transcript to that
  prompt — which leaves the view parked away from the bottom, so the scroll-follow
  heuristic refused to follow the very next message you sent. Because the
  transcript renders lazily, that message was never even laid out: you typed a
  reply, tapped Send, and the screen showed no trace of it (the reply *was* sent —
  only its row was invisible), leaving no way to tell a sent message from a
  dropped one. Sending now always follows the conversation to the bottom, and
  re-enables follow for the agent's answer. This was the real cause behind the
  two chat-compose UI tests that had been failing on the iOS 26 simulator
  (remote-control-7lo) — the compose field and the send path were never at fault.
- **Remote: the UI-test fixture chat route survives the paired-app root swap.**
  The DEBUG `-uitest-fixture-transcript` seam pushed its route once from
  `onAppear` behind an `isEmpty` guard, so an append landing in the same
  transaction that installs the tab's `NavigationStack` — which is exactly what
  the pairing toggle's root swap provokes — was silently dropped, and the guard
  made the loss permanent for the rest of the run. It now retries briefly and
  re-checks the path, so a dropped push is recovered and a successful one stays
  idempotent. This cut the `ChatTranscriptUITests` timeout rate from 2-in-9 to
  1-in-9; the remaining share is still under investigation (remote-control-7lo).
  DEBUG-only — no Release behaviour changes.
- **Remote: pairings survive a relay restart or node reschedule.** The hosted
  relay ran an in-memory store with no persistent volume, so an Azure Container
  Apps node reschedule (a routine platform event) silently wiped every pairing —
  after which already-paired devices looped forever on "unknown device" auth
  failures until they re-paired (remote-control-bbf / remote-control-vp2). The
  live relay now runs the file-backed `SqliteStore` on a mounted Azure Files
  volume (`FLIGHTDECK_RELAY_STORE=sqlite:/data/relay.db`), so device
  registrations, pairings, claim tokens and per-pairing sequence high-water marks
  outlive a revision swap or reschedule. Because the volume is a network
  filesystem (which lacks the byte-range locking SQLite needs), `SqliteStore`
  opens the database with the no-locking `unix-none` VFS and a rollback journal —
  safe at the relay's `maxReplicas: 1` single-writer guarantee. Deployment is
  documented in `remote/relay/deploy/README.md` and re-asserted on every deploy
  in `relay-deploy.yml`.

## [1.11.0] - 2026-07-23

### New features

- **Remote: message an agent that hasn't started yet.** After FlightDeck
  restarts on Desktop, only the active project's agents are resumed; every other
  recovered tab is not-started but still shows as an idle agent on the phone.
  Sending a message to such an agent now transparently resumes its session — the
  same continuation the desktop performs when you navigate to the agent — and
  delivers the prompt once the terminal is ready, instead of rejecting with "the
  agent is not running; restart it first". Desktop-only, reusing the existing
  first-task readiness gate; no protocol or iOS change. A genuinely
  stopped/exited agent still asks you to restart it explicitly.
- **Remote: shared relay password replaces the IP allowlist.** The relay's
  deny-by-default IP allowlist (fundamentally incompatible with a roaming phone
  on cellular) is removed; the relay now gates the WebSocket `hello` on an
  optional shared password with a constant-time compare, sourced from
  `FLIGHTDECK_RELAY_PASSWORD` (wired as a GitHub Actions secret → Container App
  secret in `relay-deploy.yml`). A relay with no password configured stays open,
  so local/dev and older clients keep working. The desktop reads the password
  from `FLIGHTDECK_RELAY_PASSWORD` or `config.toml` `[remote].relay_password`;
  the iOS pairing screen has an optional relay-password field, stores it in the
  Keychain, and presents it in the `hello` at pairing and on every reconnect
  (JSON key `relay_password`, omitted entirely when unset).
- **Remote: pairings survive relay restarts/redeploys.** The relay gained an
  optional persistent `RelayStore` (SQLite-backed, behind the existing async
  trait, selectable via `FLIGHTDECK_RELAY_STORE=memory|sqlite:<path>`; in-memory
  remains the default). Device pubkeys, pairing membership, claim tokens (with
  TTL), and per-pairing sequence high-water marks now survive a restart, so a
  previously-paired desktop and phone reconnect without re-pairing.
- **Remote: multi-question AskUserQuestion prompts.** Prompts that carry several
  questions (a tabbed form) can now be answered end-to-end from the phone: the
  desktop captures every question and drives the real TUI (per-tab navigation +
  Enter-toggle, then Confirm), the protocol carries the full question list and
  per-question answers, and iOS renders a native tabbed form with a single
  Submit. Single-question prompts are unchanged.
- **iOS Remote: "Retry now" reconnect.** The reconnecting banner now has a
  "Retry now" button that resets the reconnect backoff (otherwise capped at
  60s) and forces an immediate reconnect, so the user is never stuck waiting
  out a long backoff or a silently-dead socket.
- **iOS Remote: proactive reconnect on network changes.** The transport now
  watches the phone's network path (`NWPathMonitor`) and forces an immediate
  reconnect on a cell↔Wi-Fi switch or connectivity-restored event, instead of
  waiting out the current attempt/backoff.

### Improvements

- **Deploy runbooks capture the ACR Tasks grant.** `remote/relay/deploy/README.md`
  and `web/deploy/setup.sh` now grant the deploy identity **Container Registry
  Tasks Contributor** on the registry alongside `AcrPush`, so a from-scratch
  rebuild no longer misses the role that `web-deploy`'s `az acr build` step needs.
- **Relay: graceful shutdown notifies clients.** On shutdown the relay now sends
  each connected client a `bye` + WebSocket Close frame (bounded by a grace
  period that begins only once shutdown is signalled) instead of a hard TCP
  reset on redeploy, so peers reconnect cleanly (remote-control-0ef.18).
- **Desktop: transcript sync throttled when no phone is paired.** The bridge no
  longer resolves + reads each agent's session file on every TUI render tick
  while no phone is attached; it throttles to a low cadence when unpaired and
  always syncs on (re)pair so a late-joining phone still gets full history
  (remote-control-0ef.13).
- **Remote: honest reconnect-banner copy.** The banner now names the right
  culprit — when the phone can't reach the relay (offline, or relay unreachable
  e.g. ingress-blocked on cellular) it points at the phone's own connection, and
  only asks "is FlightDeck running on your Mac?" when the relay is reachable but
  the desktop is actually absent.
- **Desktop: coalesced cursor persistence removes write amplification.** The
  relay client no longer rewrites the entire `~/.flightdeck/remote.json` (plus a
  `chmod 0600`) on every streamed envelope, inbound envelope, and peer ack —
  under shell streaming that was many full-file rewrites per second for a
  monotonic counter bump. A `CursorFlushGate` now debounces those cursor persists
  to at most one write every 2s and always flushes on session end, while
  pairing-lifecycle changes still persist immediately. A hard-crash loses at most
  a couple of seconds of resume/dedup cursor progress, which the relay's
  at-least-once redelivery already tolerates (remote-control-0ef.11).
- **Relay: SQLite store delegates to the canonical queue/claim logic.** The
  file-backed `SqliteStore` no longer re-expresses gapless-seq / dedup /
  drop-oldest-overflow / single-use-TTL in SQL. New rehydration constructors
  (`SenderQueue::from_snapshot`, `ClaimTable::from_records`) let each mutating
  method load the canonical type, run the one true algorithm, and write the
  snapshot back — so the two store implementations can no longer drift
  (remote-control-tvc). The multi-question protocol fields were also reviewed and
  deliberately kept at `PROTOCOL_VERSION` v3 (wire-compatible, no bump needed —
  remote-control-ssw).

### Bug fixes

- **FlightDeck Remote (desktop): the connection survives sleep/wake, flaps, and
  outages instead of silently wedging.** Five transport-durability fixes on the
  desktop client: (1) a half-open socket (laptop sleep/wake, Wi-Fi↔cell handoff)
  is now detected — the client tears the session down and reconnects if no
  inbound frame arrives within a 60s liveness window, instead of looping on idle
  reads forever while the UI still shows "connected" (remote-control-0ef.1,
  desktop portion); (2) a relay that authenticates then immediately drops no
  longer resets the reconnect backoff to zero — backoff only resets after a
  session stays healthy for ≥10s, so a crash/redeploy loop no longer hammers
  reconnects ~once a second (remote-control-0ef.2); (3) an outbound envelope
  whose write fails is now held and re-sent on the next session before any newer
  traffic, so its `seq` slots back in contiguously and the phone's dedup never
  stalls on a gap (remote-control-0ef.9); (4) while the relay link is down the
  bridge pauses sealing/queueing status, rollup, shell, and transcript envelopes
  — during an outage it no longer burns crypto/CPU building a backlog into an
  unbounded channel that floods out on reconnect (remote-control-0ef.10); (5) an
  incompatible relay protocol version is now a distinct terminal state ("update
  FlightDeck") rather than an invisible forever-retry, and connect/DNS errors are
  surfaced for diagnostics instead of being discarded (remote-control-0ef.20).
- **FlightDeck Remote (desktop): no status spam when no phone is connected.** The
  desktop used to seal and send a `status_update` every render tick — about once
  a second, for hours — into an empty relay queue even when no phone was attached
  (seen throughout the 2026-07-22 incident). It now only seals+sends the per-tick
  snapshot/status/rollup deltas while a phone peer is actually attached; a phone
  that (re)connects still gets a fresh full snapshot the instant it attaches
  (remote-control-uqa).
- **FlightDeck Remote (relay): dead or slow peers no longer freeze a healthy
  peer, silently linger, or go undetected.** Three connection-lifecycle fixes on
  the relay: (1) a slow/half-open receiver can no longer head-of-line-block the
  sender — the relay now forwards to a peer with a non-blocking `try_send` and
  lets the buffered envelope be replayed on `resume` instead of awaiting a jammed
  outbox (remote-control-0ef.6); (2) a reconnecting client now actively
  supersedes its previous connection (the old reader/writer tasks are signalled
  to shut down) rather than leaving two same-role legs coexisting
  (remote-control-0ef.8); (3) authenticated connections are now liveness-checked
  — the relay sends periodic WebSocket pings and tears a connection down after
  60s with no inbound traffic, announcing `Disconnected` to the peer, so a
  half-open socket is reclaimed instead of leaking a session, writer task, and
  registry handle (remote-control-0ef.1, relay portion).
- **FlightDeck Remote (relay): pending-queue and claim-table durability.** Three
  fixes that stop silent data loss and unbounded memory growth on the relay: (1)
  when a `(pairing, sender)` queue overflows and drop-oldest sheds un-acked
  envelopes, a resuming receiver is now told to **resync** (request a fresh
  snapshot) instead of being handed a stream with a permanent hole its
  gapless-seq enforcement stalls on — an ack-pruned resume is still a clean
  replay (remote-control-0ef.7); (2) a periodic sweep now evicts expired
  claim tokens, so an abandoned `pairing_offer` code that is never entered no
  longer leaks an entry for the life of the process, and an expired-but-unswept
  token no longer blocks reuse of its 4-digit code (remote-control-0ef.16); (3)
  revoking a pairing now garbage-collects the device identity and key-agreement
  keys of any member no surviving pairing still references (remote-control-0ef.17).
- **FlightDeck Remote (relay): APNs wake pushes are more reliable.** Three fixes
  to the offline-wake path: (1) the wake push now uses a non-zero
  `apns-expiration` (a ~5-minute store-and-forward window) instead of
  deliver-once, so a momentarily-unreachable phone still gets woken
  (remote-control-0ef.5); (2) a transient push failure is now retried (bounded,
  with backoff) instead of being dropped, and a permanent `410`/`BadDeviceToken`
  response purges the dead token so the relay stops firing at it
  (remote-control-0ef.14); (3) the APNs provider JWT is now cached and refreshed
  on a ~20-minute cadence rather than minted on every push, avoiding wasted CPU
  and APNs `TooManyProviderTokenUpdates` (429) under a burst (remote-control-0ef.15).
- **Deploy image tags no longer get a trailing dash.** The "Resolve image tag"
  step in `relay-deploy.yml` and `web-deploy.yml` used `echo` into `tr`, whose
  trailing newline became a `-` (e.g. `v1.8.0-`). Switched to `printf '%s'`.
- **iOS Remote: no reconnect churn on transient interruptions.** The transport
  is now torn down only when the app truly backgrounds, not on the transient
  `.inactive` phase (Control Center pull, app-switcher glance, incoming call,
  Face ID prompt), eliminating needless reconnect/re-auth churn.
- **iOS Remote: silent wake push now reconnects.** A backgrounded silent wake
  push now spins up a background task that reconnects the (torn-down) transport,
  replays queued envelopes, and schedules their local notifications before
  reporting completion — previously it returned synchronously and never
  reconnected, making background notifications inert.
- **iOS Remote: stale UI when the desktop returns.** When the desktop's presence
  comes back after being absent, the phone now re-issues resume + snapshot so a
  live link with an online desktop no longer shows stale/empty state.

## [1.10.1] - 2026-07-21

### New features

- **About dialog.** A new "About FlightDeck" entry in the command palette opens
  a dialog showing the version and credits — FlightDeck is built by Ruud van
  Falier, with collaboration from Sander Langhorst.
- **Configure FlightDeck Remote from the configuration manager.** The in-app
  configuration manager now edits both the remote master switch
  (`remote.enabled`) and the relay URL (`remote.relay_url`). Text fields are
  edited inline — press `Space` to start, type, `Enter` to save, `Esc` to
  cancel.

### Improvements

- **Web app ingress is now public.** The Azure Container App serving the landing
  page and `/docs` (`ca-neworange-web-dev-neu`) no longer inherits the relay's
  deny-by-default IP allowlist — it's a public site, so anyone can reach it. The
  relay (`ca-neworange-flightdeck-dev-neu`) stays IP-restricted. `web/deploy/setup.sh`
  no longer mirrors the allowlist onto the web app, and the deploy docs/workflow
  reflect the public ingress.
- **Document that the default relay is not public.** FlightDeck ships with a
  default relay (`relay.flightdeckai.app`) that is **restricted and not
  accessible to the public**; you can host your own relay instead, but
  self-hosting is unsupported by the author. This is now stated across the docs
  (with references wherever FlightDeck Remote is mentioned), in the in-app
  configuration manager, and as a comment in the generated global `config.toml`.
- **First-run global `config.toml` now documents the `[remote]` section.** It is
  written with `enabled = false` and the default `relay_url`, alongside a comment
  explaining the relay restriction.

### Bug fixes

- None yet.

## [1.10.0] - 2026-07-21

### New features

- **FlightDeck Remote: agent replies render as rich text.** The desktop sends
  agent responses as Markdown; the iOS app now parses and renders them instead
  of showing raw syntax. Agent chat bubbles and the activity pill's expandable
  prose format headings, **bold**/*italic*, `inline code`, bullet and numbered
  lists, fenced code blocks, blockquotes, and links using the app's Geist /
  Geist Mono type and Theme colors. The eyes-free focus-mode "Recently" peek
  strips Markdown syntax so its one-line summaries stay clean. User messages are
  still shown verbatim.
- **FlightDeck Remote: answer an agent's multiple-choice prompts from the
  phone.** When an agent asks a real multiple-choice question (Claude Code's
  `AskUserQuestion`, OpenCode's `question.asked`) it now reaches the phone as a
  selectable list of the agent's actual options — each with its label and
  description — instead of a generic Allow-once / Deny card. Pick an option (or
  type your own answer when the question allows it) and FlightDeck drives the
  agent's TUI to that choice. Binary permission prompts keep the familiar
  two-button Allow / Deny card. Spans the wire protocol (v2: `PromptKind`,
  indexed options, an `option_index` / `free_text` decision), desktop capture
  for Claude and OpenCode, the decision→keystroke mapping, and the iOS prompt
  card.
- **FlightDeck Remote: answer multi-select (checklist) questions from the
  phone.** When an agent asks a question that allows several answers (Claude
  Code's `AskUserQuestion` with `multiSelect`, or an OpenCode multi-select
  question), the phone now renders it as a checklist — toggle any number of
  options on, then tap **Submit** to send them together. Single-select
  questions still submit on the first tap, and binary Allow / Deny is
  unchanged. Bumps the wire protocol to v3 (`multi_select` on the question,
  `option_indices` on the decision) and drives the agent's TUI to toggle and
  submit the whole set. (remote-control-dc9)

### Improvements

- **FlightDeck Remote: OpenCode questions surface their real options.** The
  OpenCode status plugin read the question's options from the wrong field, so an
  OpenCode question reached the phone as an empty Allow/Deny card. It now reads
  OpenCode's actual payload (`questions[]` / `choices`, `label`/`value`/`hint`),
  so the real options are offered. (remote-control-qa1)

### Bug fixes

- **FlightDeck Remote: a follow-up question in the same session shows as a new
  prompt.** After you answered one question, a second question the agent asked
  in the same session reused the first (already-answered) card instead of
  surfacing the new one — most visible on OpenCode. The desktop now clears its
  open-prompt de-duplication guard once a prompt is answered, so every new
  question appears fresh. (remote-control-dc9)
- **FlightDeck Remote: answering a multiple-choice question now selects the
  right option.** Selecting an option on the phone drove the agent's list by
  arrow-key navigation, which mis-landed (you picked option 1 but the agent
  registered option 3). Claude's `AskUserQuestion` numbers its options `1..N` and
  pressing the number selects that option directly, so the phone now sends the
  option's number — robust to the cursor position and terminal mode. (OpenCode
  keeps arrow navigation, encoded for its live cursor-keys mode.) (remote-control-qa1)
- **FlightDeck Remote: base-branch agents now show a transcript at all.** An
  agent running on the base branch (worktree `.`) had its worktree built as
  `repo_root/.`, whose string-mangled session-store path never matched the clean
  path Claude/OpenCode actually record under — so the desktop found no session
  file and the phone showed nothing: no agent responses, no user messages, and
  no questions (only an empty Allow/Deny fallback). The lookup now normalizes
  away the trailing `.`, so these agents' conversations reach the phone. This was
  the primary reason `main` agents appeared silent on the phone. (remote-control-ou3)
- **FlightDeck Remote: agent questions now reach the phone reliably.** A Claude
  Code `AskUserQuestion` used to be invisible on the phone: it was captured but
  only surfaced on a "waiting-for-input" status edge that, for a question (as
  opposed to a permission prompt), no status hook ever fired — so the prompt was
  never shown, in the chat or as a row status. Questions are now emitted as an
  answerable prompt the moment the agent asks (they live in the session file),
  independent of the status edge, and a new Claude `PreToolUse`/`AskUserQuestion`
  hook flips the agent to `waiting` so the row status, rollup, and notifications
  are correct too. (remote-control-z30)
- **FlightDeck Remote: a question no longer shows a phantom permission prompt
  first.** Claude writes an `AskUserQuestion` to its session log only *after* it
  is answered, so the phone used to show a generic (empty) Allow/Deny card while
  the agent waited — and accepting it drove the live selector (its "Allow"
  keystroke lands as an answer), marking the question answered with the wrong
  option. The Claude `AskUserQuestion` hook now writes the question to a sidecar
  the instant it is asked (mirroring OpenCode), so the desktop surfaces the real
  question to the phone immediately, with no binary card in between. A genuine
  permission prompt still shows the Allow/Deny card as before. (remote-control-qa1)
- **FlightDeck Remote: the Projects tab now lists projects from every paired
  Mac.** It previously bound to a single machine, so with more than one Mac
  paired it silently hid all but one machine's projects (and could strand on an
  abandoned session from an orphaned pairing). It now aggregates across all
  paired machines — matching the Feed — and each project opens against the
  machine it belongs to. The Projects and Sessions screens also gained real
  pull-to-refresh. (remote-control-aj2)

## [1.9.0] - 2026-07-20

### New features

- **Repository hooks that run at worktree lifecycle points.** A repo can ship a
  `.flightdeck/hooks.toml` with commands that FlightDeck runs automatically:
  `[worktree_created]` runs in a new worktree right after it is created for an
  Agent Tab (e.g. `npm install`), and `[worktree_update]` runs in a worktree
  after it is rebased onto an updated base branch. Commands run sequentially
  through your shell and stop at the first non-zero exit; a single command may
  span multiple lines using TOML triple-quoted strings. Hooks are best-effort —
  a failing hook is surfaced as a warning but never rolls back the worktree. The
  file is created (empty, commented) on first run and is `.gitignore`d by
  default, so hooks stay opt-in per machine until you un-ignore and commit it to
  share them with your team.

- **FlightDeck Remote: pair one phone with multiple Macs.** The iOS app now
  pairs with several FlightDeck desktops at once (up to four) instead of exactly
  one. A new unified feed interleaves activity from every paired machine into a
  single list ordered by recency, each row tagged with a machine chip; offline
  machines still appear from their last-known snapshot, dimmed with an "offline"
  badge and tap-to-retry. Each machine keeps its own live connection (its own
  relay URL) while the app is foreground and hands off to push when backgrounded.
  Add a machine at any time from the feed or Settings; opening a session, chat,
  or shell from a feed row drives that specific machine with no cross-talk.
  Each Mac reports its own name (auto-updating when you rename the Mac, with an
  optional per-machine override), push is per-machine with individual mute, and
  you can unpair one machine — which revokes it on the relay so the desktop
  learns — without disturbing the others or re-pairing the rest. Existing
  single-machine pairings migrate automatically on upgrade; no re-pair needed.
  Protocol additions are backward compatible: the desktop carries its machine
  name on every connect, and the relay gained membership-verified,
  idempotent revoke and push-token unregister paths.

- **Run an agent directly on the base branch, in the project root.** The New
  Agent flow now offers a "run from base branch" option that starts the agent —
  and any child shells you open in that tab — in the repository root on the base
  branch, with no dedicated worktree. Handy for quick base-branch tasks (pulls,
  chores, exploration) without spinning up a worktree. Only one base-branch tab
  runs at a time, and the worktree-only actions (Abandon / Local merge / Rebase)
  are refused for it so the project root is never touched.

### Improvements

- **FlightDeck Remote: the Activity tab is folded into the unified Feed.** There
  is now one surface instead of two overlapping lists. Feed rows carry each
  project's latest agent event, so a row shows an unread dot, highlights
  needs-input and errors, and reads the event summary inline; the unread badge
  now lives on the Feed tab and counts unseen activity across *every* paired
  machine (the old Activity tab only tracked one). Tapping a needs-input or
  error row jumps straight to that session. Unread state persists across
  launches and clears per row as you open it.

- **Pull base now stashes uncommitted changes instead of refusing.** When the
  base folder has uncommitted (tracked) changes, "Pull base" used to refuse and
  ask you to commit or stash first. It now stashes those changes automatically,
  runs `git pull --rebase`, and re-applies them on top — so you can pull merged
  PRs into the base without interrupting your work. If the changes can't be
  re-applied cleanly (they conflict with what was pulled), the stash is kept and
  you're told how to recover it by hand. Untracked-only changes don't block a
  rebase, so they're left in place and the pull just proceeds.

- **The New Agent dialog is now a single combined form.** Picking the agent and
  naming the branch used to be two sequential prompts; they are now one dialog
  with a radio list of agents (↑/↓ to choose), a branch-name field, and a
  "run from base branch" toggle (Tab) that disables the branch field when on.

- **Docs: FlightDeck Remote documentation covers multi-pairing.** The Remote
  docs now describe pairing with multiple Macs (add/rename/mute/unpair a
  machine), and the Activity page became **The Feed & Notifications** to match
  the unified Feed. Stale mobile screenshots were regenerated.

### Bug fixes

- None yet.

## [1.8.1] - 2026-07-18

### New features

- **FlightDeck Remote: Codex and OpenCode chats now reconstruct on the phone.**
  The remote transcript understood only Claude Code's session format, so Codex
  and OpenCode agents' mobile chats stayed empty. Codex is now parsed from its
  rollout session log (user prompts and assistant replies from its `event_msg`
  stream, tool activity like shell commands and patches from its `response_item`
  stream). OpenCode — which moved its conversation into a live SQLite database —
  is now read directly from that database, streaming user prompts, assistant
  prose, and tool activity (reads, edits, searches, commands, skills, and MCP
  tools) as they happen. Both mirror the Claude experience. (OpenCode
  reconstruction is a macOS/Linux desktop feature; on Windows its chat stays
  empty, matching how the relay's secure connection is already non-Windows.)

### Improvements

- None yet.

### Bug fixes

- **FlightDeck Remote: the agent's replies now actually appear in the phone
  chat.** The remote transcript was reconstructed by scraping the agent's raw
  terminal output, but the coding agents (Claude Code, Codex, OpenCode) paint a
  full-screen UI on the alternate screen and almost never emit plain lines — so
  the reconstruction produced nothing and the chat stayed empty even though the
  agent was replying and every message was being delivered to the phone. The
  transcript is now rebuilt from the agent's own structured session log (the
  same file FlightDeck already uses to resume a session), so user prompts,
  assistant prose, and tool activity stream to the phone as they happen.
  (Claude Code, Codex, and OpenCode are all supported.)
- **FlightDeck Remote: agent feedback now keeps reaching the phone across relay
  restarts.** The hosted relay tracks a per-pairing message sequence number in
  memory only, so a restart/redeploy reset it while the desktop and phone kept
  their persisted cursors. The desktop's next message was then rejected as
  out-of-sequence and the desktop reconnected into the same rejection forever —
  the phone would send a prompt, watch the agent run on the desktop, but never
  receive any of the agent's replies. The relay now reports this divergence with
  a dedicated, recoverable `seq_violation` (instead of a fatal error), and both
  ends re-sync automatically: the desktop restarts its outbound stream with a
  fresh snapshot, and the phone accepts the reset instead of discarding it as a
  duplicate. Re-deriving the encrypted channel for an already-paired phone also
  no longer rewinds the sequence number, which could stall delivery the same way.
- **FlightDeck Remote: transcript requests always get an answer.** When the
  phone asked for a session's transcript before the agent's session file existed,
  the desktop silently dropped the request and the phone waited forever; it now
  replies with an empty transcript so the phone can render and catch up as
  history streams in. Claude session-file lookup also folds Windows path
  separators, so reconstruction resolves the right project directory on Windows
  as well as macOS/Linux.

## [1.8.0] - 2026-07-17

### New features

- Resume agent sessions across restarts: FlightDeck reads the session id from
  the agent's own on-disk session store (Claude `~/.claude/projects/<cwd>`,
  Codex `~/.codex/sessions`) for the tab's worktree and relaunches with
  `claude --resume <id>` / `codex resume <id>`, so a tab continues its previous
  conversation however it was closed (clean exit, killed on shutdown, or the
  terminal window closed). Each agent's session is pinned per tab, so multiple
  agents in one worktree each resume their own. Controlled by `ui.auto_continue`
  (default on); set it to `false` to always start fresh.
- **FlightDeck Remote** — pair a phone to your desktop over an end-to-end-encrypted
  relay to monitor and control your agent sessions from anywhere. Pair from
  Settings → Remote by scanning a QR code or entering a 4-digit code; from the
  iOS app you can then:
  - **Monitor** your projects and agent sessions with rolled-up status and
    plain-language summaries.
  - **Chat with agents** through a cleaned-up transcript — reply, follow up, and
    approve or deny permission prompts inline, with hold-to-talk voice dictation
    and an eyes-free Focus mode for hands-free approvals.
  - **Open a live shell** into a session, with ANSI colours, scrollback, and an
    accessory key bar.
  - **Run guarded git actions** — pull base, merge back, and abandon worktree
    (push/PR stay the agent's job).
  - **Control sessions** — start, restart, or close agents and set a manual
    status override.
  - **Get push notifications** when an agent needs input or finishes, deep-linking
    straight to the agent.

  Connection state is shown honestly: commands pause while reconnecting (nothing
  is sent blind), and a cached read-only view stays available offline, clearly
  marked stale. The relay runs on Azure Container Apps behind a stable custom
  domain (`wss://relay.flightdeckai.app/ws`). Push notifications on device require
  an Apple APNs auth key + signing team.
- Add a Next.js documentation site under `web/`, including a Flightdeck landing
  page and MDX documentation at `/docs`.
- Write the full documentation content for the site: an Overview and Core
  Concepts, a Get Started section (install, first run, interface tour), an
  in-depth Desktop Guide (agent tabs & worktrees, agents, terminals & split
  view, the Git workflow, multiple projects, configuration, notifications,
  containers, and the CLI), a FlightDeck Remote (iOS) guide (pairing, monitoring,
  chat/focus/voice, session control, shell, activity & notifications, settings &
  security), and a Reference section (keyboard shortcuts, configuration
  reference, troubleshooting). Every page is illustrated: iOS screenshots are
  generated from the app in the Simulator, and desktop screenshots use the main
  layout capture plus branded placeholders for shots that still need to be taken.

### Improvements

- The input mode (APP / TERMINAL) is now more visible at a glance: a
  mode-colored status chip, an opt-in colored border around the pane that has
  keyboard focus (`ui.mode_border`, off by default), and terminal dimming in
  APP mode (`ui.dim_terminal_in_app_mode`, on by default). New `[ui]` settings:
  `terminal_mode_color`, `app_mode_color`, `mode_border`,
  `dim_terminal_in_app_mode`.
- Rebuild the `web/` landing page into a full marketing home page: hero, a real
  desktop screenshot, the "one tab = one worktree = one branch = one agent"
  mental model, a six-feature grid, and install commands for Homebrew, macOS/Linux,
  and Windows.
- Docs site: the sidebar navigation now groups pages into labelled sections
  (via a `section` frontmatter field), and Previous/Next links follow the
  navigation order rather than filesystem order.
- Deploy the `web/` app (landing page + docs) to Azure Container Apps as a
  separate Container App sharing the relay's resource group, registry, and
  environment, behind the same deny-by-default IP allowlist as the relay.
  Served on `https://www.flightdeckai.app` (a subdomain gets an auto-renewing
  managed TLS cert via CNAME validation, which works behind the allowlist); the
  apex `flightdeckai.app` 301-redirects to www at the registrar. Added
  `web/Dockerfile` (Next.js standalone), `web/deploy/{setup,bind-custom-domain}.sh`,
  and a release-triggered `web-deploy.yml` GitHub Actions workflow (GitHub-OIDC,
  no stored Azure secret).

### Bug fixes

- Restore the marketing site favicon: `web/src/app/favicon.ico` and `icon.svg`
  were never committed, so browsers showed no icon. Both are now checked in.
- Continue a recovered worktree that has no stored agent by falling back to the
  configured default agent, so restarting/resuming actually launches a terminal.
- Refuse to start an agent when its worktree directory is missing instead of
  silently launching it in the user's home directory (a case-sensitive-filesystem
  path mismatch could otherwise drop the agent in `~/` on Linux).
- Write `state.json` atomically (temp file + rename) so an abrupt shutdown
  mid-write can no longer corrupt or truncate it.
- Persist state and terminate agents on `SIGTERM`/`SIGINT`/`SIGHUP` (terminal
  closed, `kill`, service stop) instead of dying without cleanup.
- Terminate agents gracefully on shutdown: send `SIGTERM` to the whole process
  group, allow a short grace period, then `SIGKILL` — so agents can exit cleanly
  and child processes are no longer orphaned.
- Exit cleanly and still save `state.json` when the terminal window is closed
  (e.g. Konsole), where stdin/stdout/stderr are all severed. Input is now read on
  a dedicated thread so the loop always notices the shutdown signal instead of
  hanging on crossterm's EOF busy-loop; state is persisted before the terminal is
  restored; and the cursor-restore is made panic-safe on a dead tty.
- Fixed the terminal being clipped by 2 columns/rows after enabling
  `ui.mode_border` until the next window resize: the terminal PTY is now
  resized immediately when the border setting changes, instead of using a
  stale cached size.
- CI ran the entire cross-platform matrix twice per commit on feature branches:
  the `push` (on `flightdeck/**`) and `pull_request` events fired for the same
  commit under different refs, so their concurrency groups never collided and both
  full runs proceeded in parallel — doubling runner minutes and PR check entries.
  `push` is now scoped to `main` only (PRs own feature-branch CI) and the
  concurrency key uses `head_ref`; the Relay workflow got the same fix
  (remote-control-dwb).
- Fixed the `remote_e2e` suite hanging until the CI timeout on macOS: now that
  the desktop traps `SIGHUP`, the harness's portable-pty `kill()` (which sends
  `SIGHUP`) no longer terminates it, and a session-leader desktop that began a
  graceful exit while the harness still held the PTY master open wedged
  permanently in the kernel exit path. The harness now tears the desktop down
  with `SIGKILL` directly.
- Fixed background projects hanging on "(terminal starting…)" after startup:
  resuming agents only for the active project (a follow-up to the session-resume
  work) left other open projects' tabs unspawned, and switching to them never
  triggered the on-demand resume. Switching to a project — via keyboard, the
  command palette, or clicking its tab — now resumes its recovered agents
  (remote-control-4by).
- Release deploys now actually fire and verify correctly. The relay and web
  Azure Container Apps deploy workflows were wired only to `release: published`,
  but cargo-dist creates the GitHub Release with `GITHUB_TOKEN`, which by design
  never fires that event — so no deploy ran on a release. Both workflows now also
  trigger on the release tag push (`push: tags`), resolving the image tag from
  the tag name. Separately, the relay deploy's post-deploy check curled
  `/version` through the deny-by-default ingress IP allowlist and always got a
  403 (a GitHub runner isn't allowlisted); it now verifies the new revision via
  the control plane (latest active revision `Healthy` with the deployed
  `GIT_SHA`), matching how the web check already tolerated the allowlist. Also
  granted the deploy identity `Container Registry Tasks Contributor` on the ACR,
  which `az acr build` (web) requires beyond `AcrPush` (remote-control-35t).

## [1.7.2] - 2026-07-14

### New features

- None yet.

### Improvements

- Add agent-harness project skills under `.agents/skills/` capturing FlightDeck's
  recurring conventions (shipping/definition-of-done, cross-platform parity,
  the trait-seam architecture and git safety boundary, and config conventions),
  plus a fast-check `Stop` hook that runs `cargo fmt --check` and `cargo clippy`
  when Rust files change. Developer tooling only — no change to the shipped app.
- Keep `Alt+Esc` (macOS) and `Shift+Esc` (Windows/Linux) as the default way to
  leave terminal focus, with an optional **F2** binding for terminals that
  cannot distinguish modified Esc. The F2 preference is available in the
  configuration manager and can be set globally or per project.

### Bug fixes

- Size the configuration manager to its content instead of stretching it
  vertically in tall terminals.

## [1.7.1] - 2026-07-13

### New features

- Waiting-for-input alerts now post an OS notification and play a distinctive
  three-pulse sound, separate from the completion chime, including OpenCode
  question prompts.

### Improvements

- None yet.

### Bug fixes

- Fix image paste for Codex CLI: local sessions now receive Codex's native
  clipboard paste shortcut (`Ctrl-V` and reported `Cmd-V` on macOS), while
  containerized sessions receive a path to a safely shared temporary image file.

## [1.7.0] - 2026-07-13

### New features

- Add a **configuration manager**, opened from the command palette
  ("Open Configuration"). It edits the common settings as toggles/choices —
  OS notifications and per-category alerts, the finish chime, update checks,
  agent tab position, and the default agent. `Tab` switches between the
  **Global** and **Project** scope (the header always names the file being
  edited and, for a project, which project), `Space` toggles, `c` clears a
  project override so it re-inherits, `s` saves, and `e` opens the raw
  `config.toml` in `$EDITOR` for the full surface. Saving reloads every open
  project's effective config immediately.
- Introduce a per-user **global config** at `~/.flightdeck/config.toml`,
  created on first run with every setting present and documented so it is clear
  what can be overridden. Each project's `.flightdeck/config.toml` now only
  needs to store the values it overrides; everything else is inherited from the
  global base. The project layer wins field-by-field, except `[agents]`, which
  a project replaces wholesale when it defines any of its own. Existing
  fully-populated project configs keep working unchanged.

### Improvements

- OS notifications are now **on by default** (previously opt-in), including the
  finish chime (`sound`). Turn them off with `enabled = false` under
  `[notifications]` in the global or a project config, or from the
  configuration manager.
- OS notifications now include the project name, e.g. `myproject: my-agent`,
  so alerts are unambiguous when several projects are open.

### Bug fixes

- None yet.

## [1.6.0] - 2026-07-13

### New features

- Play a distinctive two-note "ding" chime when an agent finishes its turn
  (transitions from working to idle/completed). The sound is embedded in the
  binary, plays on macOS, Linux, and Windows, and can be turned off with
  `sound = false` under `[notifications]`.

### Improvements

- Show a compact red animated Braille spinner on working Agent and Project
  tabs, with green dots for idle projects and a high-contrast white active
  Project tab with dark navy text.

### Bug fixes

- Detect working and waiting states from explicit Claude Code, Codex, and
  OpenCode lifecycle events instead of terminal output/silence, preventing typed
  prompts from arming false completion notifications and making project-tab
  progress indicators dependable.
- Fix the `create_tab_happy_path` test failing on Windows by normalizing path
  separators when asserting the OpenCode config directory environment variable.

## [1.5.0] - 2026-07-12

### New features

- **Multiple projects in one window.** FlightDeck can now run several project
  folders side by side. A new **project tab row** at the top of the screen
  switches between them; the folder you launch from is the first (active)
  project. Each project keeps its own Agent Session Tabs, worktrees, git status,
  and base branch — and every open project stays **live in the background**, so
  agents in a project you're not looking at keep running and still fire OS
  notifications when they finish or need input.
  - **Open another project** with the **`+ project`** button on the tab row or
    the **Open Project** palette command. A folder picker lets you **type a
    path** or **browse** directories (↑↓ select · → open folder · ← parent ·
    Enter to open).
  - **Switch projects** with **`Shift`+`Left` / `Shift`+`Right`** (works while a
    terminal is focused too), by clicking a project tab, or via the **Next/
    Previous Project** palette commands.
  - **Close a project** with the tab's `✕` (confirmed first — it stops that
    project's agents) or the **Close Project** palette command.
  - **Open projects are remembered across restarts** (per-user
    `~/.flightdeck/workspace.json`); each project's own tabs are still recovered
    from its `state.json`, and agents are never auto-relaunched.

### Improvements

- Enable the once-a-day update notice by default and tell Homebrew users to run
  `brew update && brew upgrade flightdeck` so stale tap metadata is refreshed.

### Bug fixes

- None yet.

## [1.4.0] - 2026-07-09

### New features

- **Mouse-driven tab management on the child tab bar.** The horizontal tab bar
  now carries **`+ agent`** and **`+ shell`** buttons, right-aligned and styled
  distinctly from the tabs. **`+ agent`** first asks which **backend** to use
  (Claude, OpenCode, …) then spawns an *additional agent* in the **same
  worktree** as another `agent` tab on the row (agents number `agent`, `agent 2`,
  `agent 3`, …); **`+ shell`** opens a child shell. Each tab shows a `✕` close
  control you can click to close it. (With no session yet, `+ agent` creates a
  fresh Agent Session Tab/worktree.) New palette commands **New Agent** and
  **Close Agent** cover the same in-session agents from the keyboard.
- **Sidebar close control.** Each Agent Session Tab in the sidebar shows a
  right-aligned `✕` on its name row. Clicking it asks whether to **Abandon** the
  worktree, just **Close** the agent, or **Cancel**.
- **Clearer terminology.** The worktree-level tabs (and their palette commands)
  are now called **"Agent Session Tab"** — *New/Rename/Close/Switch Agent Session
  Tab* — to distinguish them from the individual agent tabs on the horizontal
  row within a session.

### Improvements

- Add a code-review topic breakdown that splits the codebase into small,
  independently reviewable scopes.
- Refresh the code-review topic breakdown for the current codebase, including
  container runtime, update, guarded rebase, pull-base, PTY, and TUI changes.
- Complete a full code review across all topics; the fixes below are its result.
- Harden the container security guardrails to also reject the `--flag=value`
  form of `--privileged` and `--env-host` (previously only the bare flag was
  caught).
- The Git Status overlay now shows the GitHub PR compare URL once the branch
  has been pushed (SPECS §21).
- Clearer error messages: distinguish "podman not installed" from "podman not
  ready" (and drop the macOS/Windows-only `podman machine start` hint on Linux),
  surface the underlying cause when a repository can't be discovered, and
  include the agent name in the "build the image first" guidance.
- **Confirmations and notifications now appear as a centered modal dialog** that
  overlays the UI, instead of a single line at the bottom of the screen. Every
  dialog shows a clickable button for each available action (Abandon, Close,
  Cancel, …) while keeping the existing keyboard shortcuts, and long messages
  wrap across lines inside the box instead of being truncated.
- **Closing always confirms first.** Clicking a shell/agent tab's `✕` (or
  pressing `Ctrl-w`) asks for confirmation before closing the terminal, matching
  the existing confirmation flow for closing an Agent Tab. Routine actions no
  longer pop a follow-up notification — opening a shell/agent or closing a tab is
  its own confirmation, so those toasts are gone.
- New agent sessions now **symlink** the base folder's `.env` and `.env.local`
  into the worktree automatically, instead of requiring a manual copy. The link
  keeps secrets in sync with the base and is best-effort — sessions where the
  base has no `.env`/`.env.local` are created silently, with nothing to do. The
  now-redundant *Copy .env(.local)* command is hidden from the palette.

### Bug fixes

- Use `Shift+Esc` to leave terminal focus on Linux, where the window manager
  (e.g. GNOME) reserves `Alt+Esc` for cycling windows and FlightDeck never
  receives it. Matches the existing Windows behaviour; macOS keeps `Alt+Esc`.
- Container child terminals now launch a Linux shell inside the container via
  `podman exec` instead of the host shell, so child shells work on Windows hosts.
- Local merge and worktree rebase now verify the target worktree actually has
  the expected branch checked out before acting, preventing a merge from landing
  on — or a rebase from rewriting — the wrong branch.
- Force-terminate and quit now signal every terminal (primary and all children)
  even when one has already exited, so tabs close reliably and no child
  processes are left running.
- Restarting the primary agent stops the previous process first, preventing two
  agent instances from running against the same worktree.
- Container teardown no longer leaks a running container when spawn/attach fails
  partway, and container-removal failures on close/finish/abandon are now
  reported instead of silently succeeding.
- The base repository is no longer falsely reported as dirty on first run (the
  check now runs before FlightDeck writes its own config and `.gitignore`).
- Appending to a `.gitignore` whose last line lacks a trailing newline no longer
  glues the new entry onto that line.
- Stale recovered-tab entries are now surfaced as warnings instead of being
  silently dropped.
- Windows clipboard copy no longer corrupts non-ASCII text and correctly falls
  back to OSC 52 on failure.
- Windows clipboard handling is now clean under platform-specific Clippy checks.
- `Shift+Tab` is now forwarded to the terminal; the cursor is no longer drawn
  over scrollback when scrolled into history; and pasting while an overlay is
  open now dismisses it instead of swallowing the paste.
- Podman image-existence checks distinguish "not found" from runtime errors,
  agent keys are sanitized into valid image tags, and `flightdeck image build`
  validates the `[containers]` config even when containers are disabled.
- The once-a-day update-check cache now has a Windows fallback path
  (`USERPROFILE`).
- `scripts/release` accepts SemVer versions with dotted pre-release/build
  metadata, and the `keylog` example restores the terminal on error.

## [1.3.0] - 2026-07-01

### New features

- Add **Pull base**: run `git pull --rebase` on the base folder to bring the
  local base branch current after a PR is merged, without leaving FlightDeck.
  Available from the command palette (*Pull Base*) and `Ctrl-u`; refuses on a
  dirty base folder and aborts on conflict, leaving the base folder untouched.
- First-class Linux support: ship an `x86_64-unknown-linux-gnu` release binary,
  run clippy and tests on `ubuntu-latest` in CI, and post desktop notifications
  via `notify-send` (libnotify).

### Improvements

- Automate release-time changelog rollover so `./scripts/release <version>`
  moves `Unreleased` notes into the new version entry and resets the template.
- Clicking anywhere in the agent sidebar — the heading or empty space, not just
  an agent row — now switches to APP mode, so it works with zero or one agents.
- Lay out the command palette across two columns so more entries are visible at
  once without scrolling. Left/right arrow keys move the selection between the
  two columns.

### Bug fixes

- Restore mouse text selection in Split View and make wheel scrolling target
  the column under the pointer.
- Continue a recovered worktree that has no stored agent by falling back to the
  configured default agent, so restarting/resuming actually launches a terminal.
- Refuse to start an agent when its worktree directory is missing instead of
  silently launching it in the user's home directory (a case-sensitive-filesystem
  path mismatch could otherwise drop the agent in `~/` on Linux).
- Write `state.json` atomically (temp file + rename) so an abrupt shutdown
  mid-write can no longer corrupt or truncate it.
- Persist state and terminate agents on `SIGTERM`/`SIGINT`/`SIGHUP` (terminal
  closed, `kill`, service stop) instead of dying without cleanup.
- Terminate agents gracefully on shutdown: send `SIGTERM` to the whole process
  group, allow a short grace period, then `SIGKILL` — so agents can exit cleanly
  and child processes are no longer orphaned.

## [1.2.0] - 2026-06-29

Initial release.

### Supported features

#### Parallel agent workflows

- Run multiple local AI coding agents in parallel against the same Git repository.
- Create an isolated Git worktree and branch for each agent tab.
- Choose the agent per tab from configured agents, with OpenCode, Claude Code, and Codex CLI supported out of the box.
- Open additional shell tabs inside the same worktree.
- Recover saved tabs and managed worktrees when FlightDeck restarts.

#### Git-safe workflow

- Auto-initialize `.flightdeck/` inside a Git repository on first run.
- Append FlightDeck runtime entries to `.gitignore` without overwriting existing content.
- Show per-tab Git status including branch, file-change counts, ahead/behind, base drift, and upstream state.
- Push branches with confirmation and show a GitHub compare URL for pull request creation.
- Support a guarded local merge-back flow when strict preconditions are met.
- Abandon managed worktrees safely, with confirmation before discarding uncommitted changes.
- Enforce a no-history-rewrite boundary: FlightDeck does not stage files, create commits, amend commits, rebase, squash, force-push, or create pull requests.

#### Terminal UI and controls

- Provide a keyboard-first terminal UI with app mode, terminal mode, a command palette, and inline help.
- Support fast tab and terminal navigation with keyboard shortcuts.
- Support mouse selection for agent tabs and child terminals.
- Show a per-tab sidebar with agent process and status indicators.

#### Agent status and notifications

- Track live agent activity with default `working` and `idle` states.
- Allow manual status overrides.
- Offer optional precise agent status integrations via `flightdeck setup-status`.
- Offer optional macOS notifications when an agent finishes, waits for input, or fails.

#### Container support

- Run agents inside isolated rootless Podman containers.
- Bind-mount the host worktree into the container at `/workspace`.
- Reuse the same container for child shells.
- Reattach to still-running containers after restarting FlightDeck.
- Build agent images with `flightdeck image build` and validate readiness with `flightdeck doctor`.
- Support resource limits, localhost-only port forwarding, and controlled credential mounts or environment allowlists.
- Enforce container guardrails such as no `--privileged`, no container socket mounts, no home-directory mounts, `--cap-drop all`, and `no-new-privileges`.

#### Installation, updates, and platform support

- Install via Homebrew, the shell installer, or the Windows PowerShell installer.
- Self-update installer-based macOS and Linux installs with `flightdeck update`.
- Offer an opt-in once-daily update notice with `flightdeck setup-update`.
- Ship macOS and Windows builds from GitHub Releases.
