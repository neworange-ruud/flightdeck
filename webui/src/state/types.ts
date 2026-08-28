import type {
  AboutDoc,
  AccessScreen,
  AccessState,
  ActivityEvent,
  GitStatusPanel,
  HelpDoc,
  HostCommand,
  Incumbent,
  Project,
  ReplayProgress,
  Seat,
  SeatInfo,
  Selection,
  ShutdownState,
  Snapshot,
  Staleness,
  TakeoverState,
  UpdateInfo,
  VersionMismatch,
} from "./model";
import { shouldRetry } from "./model";
import type { ConfigEdit, ConfigScope } from "./config";
import type { ViewportWidth } from "./viewport";
import type {
  ConfirmGate,
  DialogChoice,
  DialogDraft,
  DialogKey,
  DialogOrigin,
} from "./dialog";

/**
 * App-state seam for the web SPA.
 *
 * These types are deliberately local and minimal, NOT imported from a
 * protocol module — `src/web/protocol.rs` (D12) is being written by a
 * concurrent task and does not exist as shippable TypeScript yet. When it
 * lands, expect:
 *
 *   - `ConnectionStatus` to be replaced/derived from the wire's connection
 *     lifecycle (`ServerMsg::Ack` / transport events), which turn 2 also
 *     requires a `catching_up` state for (§5.1: "input queues until the
 *     replay lands").
 *   - `TerminalGeometry` to arrive on `ServerMsg::Snapshot` /
 *     `ServerMsg::Delta` (host-owned cols/rows, D4) instead of being set
 *     locally.
 *   - Queued input (`pendingInput` below) to be flushed as
 *     `ClientMsg::Input` frames, preserving order — D15 turn 2 §5.1 requires
 *     keystrokes to be "queued, never dropped" and never reordered once the
 *     link returns.
 *
 * remote-control-sk4u (the main screen) extended this shape rather than
 * replacing it: the scaffold's five fields are all still here and still mean
 * what they meant. What it added is the *content* of artboards 1a–1c —
 * projects, the shared selection (D3), the mode chip and the view layout —
 * plus one `snapshot/received` action, which is the single seam
 * `remote-control-hgqy` needs to swap the fixture for the live socket.
 */

/** Mirrors the *coarse* lifecycle the status bar renders (turn 2, 2c/2d).
 * Real wire-level detail (byte cursors, ack sequence numbers) belongs to
 * `src/web/protocol.rs`, not here — the UI only needs to know which of these
 * coarse buckets it is in. */
export type ConnectionStatus =
  | "connecting"
  | "connected"
  | "reconnecting"
  | "catching_up"
  | "disconnected"
  /**
   * Turn 2, 2c. Two states the scaffold did not have, both *terminal* — the
   * browser stops retrying and names the reason:
   *
   * `revoked` — the credential was withdrawn from the desktop
   * (`ShutdownReason::TokenRevoked`, `ErrorCode::Unauthorized`). 2c: "access
   * withdrawn from the desktop — the host is fine". Amber, not red: this is a
   * decision someone made, not a failure.
   *
   * `stopped` — a `ServerMsg::Shutdown` arrived for any non-retryable reason
   * (Q5). The details live in `AppState.shutdown`; this is the bucket the
   * status bar switches on.
   *
   * Version mismatch is deliberately **not** here. 2c draws it as
   * `● connected 21ms` with the mode chip intact, because nothing about the
   * connection or about control is wrong — the tab is merely old. It lives in
   * `AppState.versionMismatch` instead. That is also why the reload chip's
   * `Enter` stays scoped to the chip's own focus rather than becoming a
   * global binding: see `specs/WEB_INTERFACE.md` §6.5 R9.
   */
  | "revoked"
  | "stopped";

/**
 * The PTY grid size. D4: the desktop always owns this — the browser never
 * negotiates or requests a size, it only ever *receives* one and letterboxes
 * around it (turn 2 revision: no scaling, no `FitAddon`). See
 * `src/term/terminal.ts` for the rendering side of that invariant.
 */
/** The retry counter 2c prints while reconnecting, and after giving up. */
export interface RetryInfo {
  /** Which attempt is in flight, 1-based, as 2c counts them. */
  readonly attempt: number;
  /** Seconds until the next attempt, or `null` once retrying has stopped. */
  readonly inSeconds: number | null;
}

export interface TerminalGeometry {
  readonly cols: number;
  readonly rows: number;
}

/**
 * Which surface has the keyboard (1a vs 1b).
 *
 * `terminal` — keystrokes go to the PTY; the viewport carries the focus glow.
 * `app` — keystrokes go to FlightDeck; the sidebar carries the glow and the
 * terminal renders asleep (`--fd-term-asleep`, 2d).
 *
 * §5.1 adds a third *rendered* value that is not a mode: any state that costs
 * the user control drains the chip to `MODE: —`, because the mode is a lie
 * while input is not arriving. That is derived from `connection`, not stored —
 * see `modeChipLabel` in `src/ui/statusBar.ts`.
 */
export type UiMode = "terminal" | "app";

/** Single viewport (1a/1b) or the three-column split (1c). Toggling split from
 * the browser was M2 (D8); `remote-control-ll5.7` closes it. `layout/set` is
 * dispatched from the host's own word on the matter — `snapshot/received`
 * and a live `Delta::Selection` (`wire/socket.ts`) — never from a click
 * handler guessing what the toggle did. */
export type ViewLayout = "single" | "split";

/**
 * A `ClientMsg::Command` this browser sent and has not heard back about yet.
 * `seq` is the transport's — assigned by `SessionSocket.sendCommand`, which is
 * the one place that owns the counter shared with `Input` frames — so the
 * reducer never invents one.
 */
export interface PendingPaletteCommand {
  readonly seq: number;
  readonly label: string;
}

/**
 * What actually happened to a command this browser ran, per the host's own
 * words — never guessed locally (requirement 4 of ll5.2: "reflects the host's
 * Ack, not optimistic local state").
 *
 * `applied` / `rejected` / `ignored` mirror `WireAck.outcome` verbatim.
 * `read_only` is a fourth case that arrives on a *different* frame —
 * `ServerMsg::Error { code: "read_only" }` (D14: an observer's `select_*`
 * is refused, never Ack'd) — folded in here because from the palette's point
 * of view it answers the same question ("what happened to the command I ran")
 * that an `Ack` does.
 */
export interface PaletteOutcome {
  readonly label: string;
  readonly outcome: "applied" | "rejected" | "ignored" | "read_only";
  readonly detail: string | null;
}

/** Artboard 1d, plus the outcome-reporting requirement 4 adds. `null` on
 * `AppState.palette` means the overlay is closed. */
export interface PaletteState {
  /** The `>` row's typed text. Matched against a command's label *and*
   * annotation (`state/commands.ts`), but only ever highlighted in the label —
   * see `matchCommand`. */
  readonly filter: string;
  /** Which of 1d's two columns `↑↓` and `Enter` act on; `Tab` moves it. */
  readonly column: 0 | 1;
  /** Index into that column's filtered rows, clamped on every change so a
   * component never has to guard against reading past the end of a shorter
   * list after a keystroke narrows it. */
  readonly index: number;
  /** Commands sent and not yet Ack'd/refused. Almost always at most one, but
   * a list rather than a single slot: nothing stops a fast typist from firing
   * a second `Enter` before the first command's `Ack` lands, and dropping the
   * first result on the floor would be exactly the optimism requirement 4
   * forbids. */
  readonly pending: readonly PendingPaletteCommand[];
  /** The most recent settled result, or `null` before any command has been
   * run this time the palette was opened. */
  readonly lastOutcome: PaletteOutcome | null;
}

/** The whole app's reducer-owned state. */
export interface AppState {
  readonly connection: ConnectionStatus;
  /** `null` until the first `Snapshot` arrives (D4/D12) — there is no
   * sensible default grid size to invent locally. */
  readonly geometry: TerminalGeometry | null;
  /** Keystrokes typed while not fully connected. FIFO order matters: this is
   * what "queued, never dropped, never reordered" (turn 2 §5.1) means at the
   * state layer. Drained by `input/flush` once the link is ready. */
  readonly pendingInput: readonly string[];
  /**
   * `Input::seq` of the **last** keystroke ever queued; `0` before the first.
   * Never reset — not on a reconnect, not on a flush — because a monotonic
   * counter is the whole mechanism behind §5.1's "never doubled".
   *
   * The queue carries no per-item seq of its own: `pendingInput[i]` has seq
   * `inputSeq - (pendingInput.length - 1 - i)`, so there is exactly one number
   * to keep honest instead of two that can drift. `firstPendingSeq` and
   * `dropAckedInput` below are the only code allowed to do that arithmetic.
   */
  readonly inputSeq: number;

  /** Everything the host told us about, empty until the first snapshot. */
  readonly projects: readonly Project[];
  /**
   * D3: the *instance-wide* selection, not a browser-local one. A selection
   * made here is also the desktop's selection; a selection made on the desktop
   * arrives as the same action. `null` only before the first snapshot.
   */
  readonly selection: Selection | null;
  readonly mode: UiMode;
  readonly layout: ViewLayout;
  /** Column index with focus in split view (1c: the glow tracks it). */
  readonly splitFocus: number;
  readonly viewers: number;
  readonly latencyMs: number | null;
  readonly update: UpdateInfo | null;
  /**
   * The named seats of the viewer chip (2f). Empty falls back to `viewers`.
   */
  readonly seats: readonly SeatInfo[];
  /** D14: what *this* browser is allowed to do. `observing` is a real mode
   * reachable from both directions of 2f, not a degraded error state. */
  readonly seat: Seat;
  /** 2f's prompt, or `null` when nobody is contending for the seat. */
  readonly takeover: TakeoverState | null;

  /**
   * The four 2b screens, or `null` once this browser holds a good cookie.
   * Non-null means the access overlay is up and the frame below it is a
   * photograph — which is exactly what 2b's revoked panel says in words.
   */
  readonly access: AccessState | null;
  /** Q5's terminal state. Non-null means **stop retrying**; see `reduce`. */
  readonly shutdown: ShutdownState | null;
  /** Turn 2 §4: the host updated under this tab. Reload, do not retry. */
  readonly versionMismatch: VersionMismatch | null;
  /** 2d's frozen clock, set while the picture is a photograph. */
  readonly staleness: Staleness | null;
  /** 2d's catching-up bar (Q3's byte cursor), or `null` when not replaying. */
  readonly replay: ReplayProgress | null;

  /**
   * The host this tab is talking to, e.g. `192.168.2.14:7420` — 2b's footer
   * strip (`no session · …`) and 2c's `attaching to …`.
   *
   * It comes from `location.host`, which is the only source that cannot be
   * wrong: it is literally the address the user reached. Empty until
   * `host/set`, and rendered as nothing rather than as a guess — a fabricated
   * address on a security screen would be the first lie the user sees.
   */
  readonly host: string;
  /**
   * 2c's `attempt 3 · retry in 4s` and `gave up after 6 attempts`. One field
   * for both, because they are the same two facts: `inSeconds: null` means the
   * retrying has stopped.
   */
  readonly retry: RetryInfo | null;

  /** D11's feed, oldest first exactly as the host sent it. */
  readonly activity: readonly ActivityEvent[];
  /** Whether the right-edge slide-over is open (2e). Never a modal. */
  readonly feedOpen: boolean;

  /**
   * Which of 1h's two layouts this viewport gets (`remote-control-eek.4`,
   * §6.5 R17). Derived from a measured pixel width by `widthClass`, so the
   * 900px boundary is a pure function and not a media query nothing in
   * `npm run test` can evaluate.
   */
  readonly width: ViewportWidth;
  /**
   * Whether 1h's slide-over sidebar is showing. Meaningful only while
   * `width === "narrow"`: at wide the sidebar is 1a's column and is always on
   * screen, so this is forced `false` the moment the viewport crosses back.
   *
   * It reuses 2e's slide-over mechanism rather than inventing a second one —
   * same `hidden` toggle, same "no scrim, no focus trap, no aria-modal"
   * posture, same `Esc`-closes rule. The one difference is the edge: 2e comes
   * from the right, and the sidebar comes from the **left**, because that is
   * where 1a's column is and because two panels arriving at the same edge
   * would fight for it.
   */
  readonly sidebarOpen: boolean;

  /**
   * When the last unpaired `Esc` was seen, for the 400 ms `Esc Esc` window
   * (§5). Stored rather than kept in a component so the decision is a pure
   * reduction; the timestamp always arrives on the action, never from a clock
   * read inside the reducer. `null` means no window is open.
   */
  readonly escArmedAt: number | null;

  /** Artboard 1d. `null` when the palette is closed — `Ctrl-g` is the only
   * chord that opens it (§5), and it is the only chord the app claims. */
  readonly palette: PaletteState | null;

  /** Artboard 1f (`remote-control-ll5.6`). `null` when the configuration
   * manager is closed — opened by the palette's "Open Configuration" row. */
  readonly config: ConfigState | null;

  /**
   * D13's shared dialog (artboards 1d/1e, `remote-control-ll5.3`). `null` when
   * none is open.
   *
   * Unlike every other overlay in this state, **this one is not the browser's
   * to open or close**: it is app state the host published, so it appears here
   * because a `Snapshot` or a `Delta::DialogOpened` said so and disappears
   * because a `Delta::DialogClosed` did. A local `dialog/close` action would be
   * the browser inventing a second source of truth, which is exactly what D13's
   * "no new state" rules out.
   */
  readonly dialog: DialogState | null;

  /**
   * The host's command inventory (`remote-control-ll5.12`), empty until the
   * first snapshot.
   *
   * The palette has no list of its own: `state/commands.ts` renders these rows
   * and nothing else, so a name this host does not implement cannot be offered,
   * and a row the host stops sending stops appearing with no browser change.
   */
  readonly commands: readonly HostCommand[];

  /* --- The read-only overlays (remote-control-ll5.8, §6.5 R16) ----------- */

  /**
   * Which read-only panel is open, or `null` when none is.
   *
   * **One field for three overlays, so "one at a time" is structural.** Help,
   * About and git status are the same kind of thing — a titled panel that
   * states facts and offers no answer — and two of them on screen at once
   * would be two panels the reader has to dismiss in order. The palette and
   * the configuration manager already work this way by convention
   * (`ui/app.ts`'s `runCommand` closes one before opening the other); here it
   * is a property of the type.
   *
   * They are *this browser's* overlays, unlike `dialog`: something local opens
   * them, `Esc` closes them, and the host is not told either way. R8 is why —
   * nothing is being asked, so there is nothing for a second surface to
   * answer, and publishing a reader's read would put a panel in front of
   * somebody who did not ask for one.
   */
  readonly readOnly: ReadOnlyOverlay | null;
  /**
   * SPECS §23's help as the host sent it, or `null` from a host that sent
   * none. Held from the snapshot so the overlay opens instantly and from
   * facts, rather than costing a round trip to say what the keys are.
   */
  readonly help: HelpDoc | null;
  /** The About screen's version and credits, from the host's own build. */
  readonly about: AboutDoc | null;
}

/**
 * The three read-only panels, as a union so each carries exactly what it
 * needs.
 *
 * Help and About carry nothing: their content is on `AppState.help` /
 * `AppState.about`, sent with the snapshot, so opening them is a pure UI
 * toggle. Git status carries its panel, because SPECS §21's facts are a fresh
 * `git` read the snapshot cannot hold — the upstream's name, the worktree path
 * and §14's compare URL — so it arrives with the frame that answered the
 * request (`ServerMsg::GitStatus`) and lives here until the panel closes.
 */
export type ReadOnlyOverlay =
  | { readonly kind: "help" }
  | { readonly kind: "about" }
  | { readonly kind: "git_status"; readonly panel: GitStatusPanel };

/** A `save_config` command this browser sent and has not heard back about. */
export interface PendingConfigSave {
  readonly seq: number;
}

/** The host's answer to a save, mirroring `PaletteOutcome` — never guessed at
 * locally (requirement 5 of ll5.6: "the host is the authority"). */
export interface ConfigOutcome {
  readonly outcome: "applied" | "rejected" | "ignored" | "read_only";
  readonly detail: string | null;
}

/** Artboard 1f's whole overlay state. */
export interface ConfigState {
  /** `Tab` switches between these two (SPECS §8's configuration manager). */
  readonly scope: ConfigScope;
  /** `↑↓` moves this among `selectableConfigFields()`; `c` acts on it. */
  readonly selectedKey: string;
  /**
   * Local, unsaved edits — keyed by `ConfigField.key`. `s` turns this into a
   * `save_config` command; a real `applied` `Ack` is what clears it, never
   * the keypress itself (requirement 5: no optimism). Non-empty is what 1f's
   * "Unsaved changes" banner renders on.
   */
  readonly edits: Readonly<Record<string, ConfigEdit>>;
  /** Almost always at most one, for the same reason `PaletteState.pending` is
   * a list: nothing stops a second `s` before the first save's `Ack` lands. */
  readonly pending: readonly PendingConfigSave[];
  readonly lastOutcome: ConfigOutcome | null;
}

/** A `dialog_confirm` / `dialog_cancel` this browser sent and has not heard
 * back about. Same shape as `PendingConfigSave`, for the same reason. */
export interface PendingDialogAnswer {
  readonly seq: number;
  /** Which half was sent, so the panel can say `confirming…` vs `cancelling…`. */
  readonly act: "confirm" | "cancel";
}

/** The host's answer to a dialog answer, mirroring `ConfigOutcome`. */
export interface DialogOutcome {
  readonly outcome: "applied" | "rejected" | "ignored" | "read_only";
  readonly detail: string | null;
}

/**
 * D13's shared dialog (artboards 1d/1e), as the browser holds it.
 *
 * Everything above `draft` is the **host's**, arriving on a `Snapshot` or a
 * `Delta::DialogOpened`. Nothing here is ever set by a local decision except
 * `draft` (what the user has typed and highlighted but not confirmed) and
 * `pending`/`lastOutcome` (which command was sent and what the host said about
 * it). See `state/dialog.ts` for why the draft is not optimism.
 */
export interface DialogState {
  readonly id: string;
  /** `new_agent`, `confirm_abandon`, … An unknown kind renders the generic
   * shell rather than failing, which is why this is a `string`. */
  readonly kind: string;
  readonly title: string;
  /** D13: who opened it. Load-bearing, not decoration. */
  readonly origin: DialogOrigin;
  /** The text field's host-side content, or `null` when it has no field. */
  readonly input: string | null;
  readonly list: readonly DialogChoice[];
  readonly buttons: readonly DialogKey[];
  /** `false` when the host will refuse a confirm from a browser. Cancelling
   * stays available, which is why this is not "read-only". */
  readonly confirmable: boolean;
  /** The sentence to show when `confirmable` is false. */
  readonly refusal: string | null;
  /** Artboard 1g's second step, when the host put one in front of one of this
   * dialog's buttons (`remote-control-ll5.4`, §6.5 R13). `null` — the common
   * case — means every button is one press away. */
  readonly gate: ConfirmGate | null;
  readonly draft: DialogDraft;
  readonly pending: readonly PendingDialogAnswer[];
  readonly lastOutcome: DialogOutcome | null;
}

export function createInitialState(): AppState {
  return {
    connection: "connecting",
    geometry: null,
    pendingInput: [],
    inputSeq: 0,
    projects: [],
    selection: null,
    /** Nothing is focused before the first snapshot, and App mode is the
     * honest default: the app cannot promise keystrokes reach a PTY it has not
     * heard about yet. */
    mode: "app",
    layout: "single",
    splitFocus: 0,
    viewers: 0,
    latencyMs: null,
    update: null,
    seats: [],
    /** Optimistic-free default: a browser that has not attached yet holds no
     * seat it can prove, and `observing` is the honest weaker claim. */
    seat: "observing",
    takeover: null,
    /**
     * `null`, not a code-entry screen: whether this browser needs a code is the
     * host's answer (`GET /auth/session`), and guessing "you are locked out"
     * before asking would flash an access screen at every authenticated user on
     * every reload.
     */
    access: null,
    shutdown: null,
    versionMismatch: null,
    staleness: null,
    replay: null,
    activity: [],
    feedOpen: false,
    /**
     * `wide` before anything has been measured, and momentarily so: `main.ts`
     * dispatches `viewport/measured` from the real `window.innerWidth` before
     * the first paint. It is the honest default of the two — wide is the
     * layout every artboard actually draws, so a viewport nobody managed to
     * measure gets the drawn one rather than the derived one.
     */
    width: "wide",
    sidebarOpen: false,
    host: "",
    retry: null,
    escArmedAt: null,
    palette: null,
    config: null,
    dialog: null,
    commands: [],
    readOnly: null,
    /** `null`, not a locally-authored keybinding list: what the host binds is
     * the host's to say, and a stand-in would document a FlightDeck this tab
     * is not attached to (`remote-control-ll5.8`). */
    help: null,
    about: null,
  };
}

export type AppAction =
  | { readonly type: "connection/changed"; readonly status: ConnectionStatus }
  | { readonly type: "geometry/set"; readonly geometry: TerminalGeometry }
  | { readonly type: "input/queue"; readonly data: string }
  | { readonly type: "input/flush" }
  /**
   * One coherent picture of the host replaces the previous one. Today the
   * fixture dispatches it once at boot; `remote-control-hgqy` dispatches it on
   * every `ServerMsg::Snapshot`. Deliberately whole-picture rather than a
   * dozen fine-grained actions: the host is the source of truth (D3) and a
   * partial update is how two surfaces drift apart.
   */
  | { readonly type: "snapshot/received"; readonly snapshot: Snapshot }
  /**
   * D3: selecting for the whole instance. The reducer only moves local state;
   * the caller is responsible for telling the host, which is where
   * `ClientMsg::Command { select_session }` goes (see `src/ui/app.ts`).
   */
  | { readonly type: "selection/project"; readonly projectId: string }
  | { readonly type: "selection/session"; readonly sessionId: string }
  | { readonly type: "selection/terminal"; readonly terminalId: string }
  | { readonly type: "mode/set"; readonly mode: UiMode }
  /** D3/D8: dispatched from the host's word — `snapshot/received`'s
   * `splitView` and a live `Delta::Selection` — never from the palette row
   * that *asks* for a toggle. See `commands.ts`'s `toggle_split_view` entry
   * and `wire/socket.ts`'s `onDelta`. */
  | { readonly type: "layout/set"; readonly layout: ViewLayout }
  | { readonly type: "split/focus"; readonly column: number }
  /**
   * An `Esc` arrived while the terminal had focus. `at` is a timestamp the
   * caller read (`performance.now()`), which keeps `reduce` pure: the 400 ms
   * window is evaluated by `decideEscape`, and either the key is queued for
   * the agent or focus is released.
   */
  | { readonly type: "input/esc"; readonly at: number }

  /* --- Turn 2: the states 2b–2f render (remote-control-l7ya) -------------- */

  /**
   * §5.1's other half. `input/flush` says "everything queued was delivered";
   * this says "the host has applied everything up to `throughSeq`", which is
   * what `Snapshot { last_input_seq }` reports on a **reattach**.
   *
   * Both exist because a reconnect is not a flush: some prefix of the queue was
   * already applied before the socket died, and re-sending it would double the
   * user's keystrokes. Dropping the acknowledged prefix and re-sending the rest
   * *in order* is what makes "never dropped, never reordered, never doubled"
   * one behaviour rather than three hopes.
   */
  | { readonly type: "input/acked"; readonly throughSeq: number }

  /**
   * Q5. `ServerMsg::Shutdown` arrived, so this is a deliberate end, not a
   * network failure: the reducer moves to a terminal `connection` and — for
   * every reason except `restarting` — **refuses later `reconnecting`
   * transitions**, which is where "stop retrying" is actually enforced. A
   * transport that keeps trying anyway cannot un-stop the UI.
   */
  | { readonly type: "connection/shutdown"; readonly shutdown: ShutdownState }
  /** Turn 2 §4: `check_version()` / `ErrorCode::VersionMismatch`. */
  | {
      readonly type: "version/mismatch";
      readonly mismatch: VersionMismatch;
    }
  /** 2d's frozen clock. `null` clears it (the picture is live again). */
  | { readonly type: "staleness/set"; readonly staleness: Staleness | null }
  /** 2d's catching-up bar. `null` clears it (the replay landed). */
  | { readonly type: "replay/set"; readonly replay: ReplayProgress | null }

  /* --- Access (2b) ------------------------------------------------------- */

  /**
   * The host says this browser needs a code: `GET /auth/session` answered
   * `authenticated: false`, or a request came back `401`/`429`. `screen` is the
   * host's own `AccessScreen` spelling — never a guess made here.
   */
  | {
      readonly type: "access/required";
      readonly screen: AccessScreen;
      readonly attemptsRemaining: number | null;
      readonly lockoutSeconds: number | null;
      /** `lockout_seconds` / `code_ttl_seconds` from the refusal body — the
       * host's own policy numbers, which the browser no longer mirrors as
       * constants of its own. `null` means it did not say. */
      readonly lockoutLengthSeconds: number | null;
      readonly codeTtlSeconds: number | null;
    }
  /** A digit was typed into 2b's four boxes. Extra digits are ignored, not
   * wrapped: a fifth keystroke means the user mistyped, not that they meant to
   * start over. */
  | { readonly type: "access/digit"; readonly digit: string }
  | { readonly type: "access/backspace" }
  /**
   * `POST /auth/exchange` refused. The body's numbers are all the host's
   * (`attempts_remaining`, `retry_after_ms`, `lockout_seconds`,
   * `code_ttl_seconds`); the browser renders them and computes nothing.
   */
  | {
      readonly type: "access/refused";
      readonly screen: AccessScreen;
      readonly attemptsRemaining: number | null;
      readonly lockoutSeconds: number | null;
      readonly lockoutLengthSeconds: number | null;
      readonly codeTtlSeconds: number | null;
    }
  /** The exchange succeeded — the cookie is set and the overlay comes down. */
  | { readonly type: "access/granted" }
  /**
   * Someone withdrew this browser's access from the desktop (2b/2c).
   *
   * `revokedAgo` is nullable because it may genuinely not be known. A
   * `Shutdown { reason: TokenRevoked }` arrives the moment it happens, so
   * "12s ago" is knowable; an HTTP refusal now carries `revoked_at_ms` beside
   * the host's own `server_time_ms`, so it is knowable there too — but a host
   * that keeps no tombstone time, or one from before that field, sends neither.
   * `null` renders the sentence without a time rather than with an invented one.
   */
  | { readonly type: "access/revoked"; readonly revokedAgo: string | null }
  /** 2b's `Enter a new code` — back to a blank keypad from any other screen. */
  | { readonly type: "access/retry" }
  /**
   * 2b's `Esc Stay here`, and **not** the same thing as `access/granted`.
   *
   * The overlay comes down; nothing about the credential changed. The user
   * asked to keep reading the photograph underneath, which 2b treats as a real
   * choice, and the connection stays `revoked` so the strip still offers
   * `Enter a code`. Reporting this as "granted" would be the app lying about
   * its own auth state.
   */
  | { readonly type: "access/dismiss" }

  /* --- Seats and takeover (2f, D14) -------------------------------------- */

  /** A `Delta::Seats` arrived: the chip's named occupants, and our own seat. */
  | {
      readonly type: "seats/changed";
      readonly seats: readonly SeatInfo[];
      readonly seat: Seat;
    }
  /** `ErrorCode::SeatHeld` — 2f's arriving panel. */
  | { readonly type: "takeover/held"; readonly incumbent: Incumbent }
  /**
   * A `Delta::Seats` took control away from us. **Not** a `Shutdown`: the
   * socket stays open, so this is a prompt over a live connection and the
   * evicted browser can keep watching.
   */
  | {
      readonly type: "takeover/evicted";
      readonly byAddress: string;
      readonly lastInputAgo: string;
    }
  /**
   * 2f's `Take over` / `Take it back`. There is no takeover frame in protocol
   * v1 — the caller re-sends `Attach { seat: SeatRequest::TakeOver }` — so this
   * action only moves local state and lets the seam do the asking.
   */
  | { readonly type: "takeover/claim" }
  /**
   * 2f's `w Watch read-only`: give up contending for input altogether and take
   * a seat that never will. D14 makes observation a real mode, so this is a
   * destination rather than a dead end.
   */
  | { readonly type: "takeover/observe" }
  /**
   * 2f's `Esc Cancel`, which under D14 as revised is **not** the same act as
   * `w`.
   *
   * v1 folded the two together, correctly: being refused meant the seat itself
   * was gone, so the only thing `Cancel` could leave you was a read-only view.
   * A refusal now costs the turn and not the seat, so cancelling means *fine,
   * I will wait* — the panel closes, the seat stays, and the lock comes back
   * the moment the other writer goes quiet. Dropping to read-only here would
   * take away something the host never took.
   */
  | { readonly type: "takeover/dismiss" }

  /* --- Activity feed (2e, D11) ------------------------------------------- */

  /** A `Delta::Activity` (or the snapshot's backfill) — appended, oldest first. */
  | {
      readonly type: "activity/received";
      readonly events: readonly ActivityEvent[];
    }
  /** Read-marking. The host owns the record; this is the local half. */
  | { readonly type: "activity/read"; readonly ids: readonly string[] }
  | { readonly type: "feed/set"; readonly open: boolean }

  /* --- The narrow layout (1h, remote-control-eek.4, §6.5 R17) ------------- */

  /**
   * The viewport's **pixel** width, not the layout it implies.
   *
   * The measurement is the caller's impurity (`src/main.ts` reads
   * `window.innerWidth` and listens for `resize`); which layout that means is
   * `widthClass`, a pure function the reducer calls — the same split
   * `input/esc` makes when it carries `at` rather than a decision about the
   * 400 ms window. It is what lets the 900px boundary be a unit test.
   */
  | { readonly type: "viewport/measured"; readonly pixels: number }
  /**
   * 1h's slide-over, open or closed. Only meaningful below 900px: at wide the
   * sidebar is a column that is always there, so the reducer refuses this
   * outright rather than storing a flag nothing reads.
   */
  | { readonly type: "sidebar/set"; readonly open: boolean }
  | { readonly type: "host/set"; readonly host: string }
  | { readonly type: "retry/set"; readonly retry: RetryInfo | null }

  /**
   * D3 across projects: a feed row names a session in a project that is not the
   * selected one, so `selection/session` (which searches only inside the
   * current project) cannot express it. Selecting from the feed also moves the
   * desktop, which is why the row says `jump · also moves the desktop`.
   */
  | {
      readonly type: "selection/jump";
      readonly projectId: string;
      readonly sessionId: string;
    }

  /* --- Command palette (1d, remote-control-ll5.2) ------------------------ */

  /** `Ctrl-g`, the only chord the app claims (§5). `app.ts` toggles: this
   * action only ever opens, closed by `palette/close`. */
  | { readonly type: "palette/open" }
  /** `Esc`, or click-outside — see `app.ts`'s palette key handler. */
  | { readonly type: "palette/close" }
  /** A printable character landed in the `>` row. Resets the cursor to the
   * top row of column 0: a filter that just changed should be read from the
   * top, not leave the highlight wherever a longer list had left it. */
  | { readonly type: "palette/type"; readonly char: string }
  | { readonly type: "palette/backspace" }
  /** `↑↓`. `delta` is `-1`/`+1`; the reducer clamps rather than wraps. */
  | { readonly type: "palette/move"; readonly delta: number }
  /** `Tab`. A no-op when the other column has nothing to move to. */
  | { readonly type: "palette/nextColumn" }
  /**
   * `Enter` ran a command and the transport assigned it a seq
   * (`SessionSocket.sendCommand`'s return value) — see `main.ts`'s
   * `onRunCommand`. Queues it in `pending` rather than guessing an outcome;
   * `command/result` is the only thing allowed to resolve it.
   */
  | {
      readonly type: "palette/dispatched";
      readonly seq: number;
      readonly label: string;
    }
  /**
   * The host's answer to a queued command arrived — either `ServerMsg::Ack`
   * (`applied`/`rejected`/`ignored`) or `ServerMsg::Error { code: "read_only" }`
   * folded into `outcome: "read_only"` (see `PaletteOutcome`). A `seq` that
   * matches nothing in `state.palette.pending` (the palette was closed and
   * reopened, or this is some other feature's frame) is a no-op — never
   * guessed at, per requirement 4.
   *
   * `seq` is one counter shared by every `Command` this tab sends (§5.1), so
   * this action also settles a config save (`state.config.pending`,
   * `remote-control-ll5.6`) — the reducer checks both queues rather than the
   * palette owning the action type outright.
   */
  | {
      readonly type: "command/result";
      readonly seq: number;
      readonly outcome: "applied" | "rejected" | "ignored" | "read_only";
      readonly detail?: string;
    }

  /* --- Configuration manager (1f, remote-control-ll5.6) ------------------ */

  /** Opened by the palette's "Open Configuration" row. Always opens on
   * Project scope, matching 1f's own default. */
  | { readonly type: "config/open" }
  | { readonly type: "config/close" }
  /** `Tab`. */
  | { readonly type: "config/scope" }
  /** `↑↓`. `delta` is `-1`/`+1`, clamped like the palette's own `move`. */
  | { readonly type: "config/move"; readonly delta: number }
  /** A row was clicked directly — same destination as `config/move`, without
   * requiring the keyboard to get there first. */
  | { readonly type: "config/select"; readonly key: string }
  /** `c`: clears the *project* override on the selected field, staged locally
   * until `s` saves it. A no-op in Global scope, or on a field with nothing to
   * clear — SPECS §8 ties `c` to a *project* override specifically. */
  | { readonly type: "config/clear" }
  /** `s` sent the staged edits as a `save_config` command and the transport
   * assigned it `seq` — mirrors `palette/dispatched`. */
  | {
      readonly type: "config/dispatched";
      readonly seq: number;
    }

  /* --- D13's shared dialog (1d/1e, remote-control-ll5.3) ----------------- */

  /**
   * The host says a dialog is open: `Snapshot { dialog }` on attach, or a
   * `Delta::DialogOpened` while attached.
   *
   * There is deliberately no `dialog/open` a component could dispatch. A
   * browser asks for a dialog by *running the command that opens one* (D13:
   * "no new state"), and learns it worked when the host publishes it.
   *
   * Re-dispatching for the dialog that is already open — which every coalesced
   * snapshot does — keeps the local `draft`, so a resync mid-typing does not
   * empty the branch field the user is in the middle of.
   */
  | { readonly type: "dialog/opened"; readonly dialog: DialogState }
  /**
   * A `Delta::DialogClosed`, or a `Snapshot` with no dialog. `outcome` is the
   * host's own word: `confirmed`/`cancelled` when somebody decided, and
   * `superseded` when a dialog was replaced without a decision — which the
   * browser must not report as an answer, because nobody gave one.
   *
   * A `dialogId` that is not the open one is a no-op: a `DialogClosed` for a
   * dialog we already replaced would otherwise close the live one.
   */
  | {
      readonly type: "dialog/closed";
      readonly dialogId: string;
      readonly outcome: "confirmed" | "cancelled" | "superseded";
    }
  /** A printable character landed in the dialog's text field (1e's branch). */
  | { readonly type: "dialog/type"; readonly char: string }
  | { readonly type: "dialog/backspace" }
  /** `↑↓` over the choice rows (1e's agent radio). Clamped, never wrapped. */
  | { readonly type: "dialog/move"; readonly delta: number }
  /** A choice row was clicked — the mouse half of `dialog/move`. */
  | { readonly type: "dialog/choose"; readonly index: number }
  /** `Tab`: 1e's `run from base branch`. Local until the confirm carries it. */
  | { readonly type: "dialog/toggle" }
  /**
   * Artboard 1g's step 1 → step 2: the gated button was pressed, so the name
   * field opens. Deliberately **not** a frame — the host is told nothing until
   * the name is typed and the confirm carries it, so pressing `y` on a
   * destructive dialog from a browser commits to nothing at all.
   */
  | { readonly type: "dialog/advance" }
  /** A printable character landed in 1g's name field. */
  | { readonly type: "dialog/gateType"; readonly char: string }
  | { readonly type: "dialog/gateBackspace" }
  /** A `dialog_confirm` / `dialog_cancel` was sent and the transport assigned
   * it `seq` — mirrors `palette/dispatched` and `config/dispatched`. */
  | {
      readonly type: "dialog/dispatched";
      readonly seq: number;
      readonly act: "confirm" | "cancel";
    }

  /* --- The read-only overlays (1d's shell, remote-control-ll5.8) --------- */

  /**
   * `?` in App mode, or the palette's "Show Help" row.
   *
   * Local, and sends nothing: the keybindings are already on `state.help`
   * from the snapshot, and the host's own row refuses to be forwarded for
   * exactly that reason (`HELP_REFUSAL`) — dispatching it would open a panel
   * on the desktop that the person who asked cannot read.
   */
  | { readonly type: "help/open" }
  /** The palette's "About FlightDeck" row. Local, for `help/open`'s reason. */
  | { readonly type: "about/open" }
  /**
   * `ServerMsg::GitStatus` arrived: the host ran SPECS §21's collection
   * because this tab sent `show_git_status`, and this is what it found.
   *
   * There is deliberately no `gitStatus/open` a component could dispatch. The
   * browser has none of these facts until the host sends them, so the only way
   * this panel opens is with a panel in hand — which is what makes "never
   * render a fact the host did not send" true of this overlay by construction
   * rather than by discipline.
   */
  | { readonly type: "gitStatus/received"; readonly panel: GitStatusPanel }
  /** `Esc`, the panel's own close button, or a click outside it. */
  | { readonly type: "readOnly/close" };

/**
 * Seq of `pendingInput[0]`, or the seq the *next* keystroke will get when the
 * queue is empty. Exported because the transport needs it to label the frames
 * it sends, and because the arithmetic must exist in exactly one place.
 */
export function firstPendingSeq(state: AppState): number {
  return state.inputSeq - state.pendingInput.length + 1;
}

/**
 * The queue with everything the host has already applied removed, preserving
 * the order of the rest. A `throughSeq` behind the queue drops nothing; one
 * ahead of it drops everything. Both are normal on a reconnect.
 */
export function dropAckedInput(
  state: AppState,
  throughSeq: number,
): readonly string[] {
  const first = firstPendingSeq(state);
  const drop = Math.min(
    state.pendingInput.length,
    Math.max(0, throughSeq - first + 1),
  );
  return drop === 0 ? state.pendingInput : state.pendingInput.slice(drop);
}

/**
 * Q5 enforced in one place: whether a *later* connection transition is allowed
 * to claim the link is coming back.
 *
 * A browser that has been told `Shutdown { reason }` for anything but a restart
 * is looking at a host that is gone, and "reconnecting…" would be a lie that
 * wastes the user's time. `revoked` is the same shape of fact: the credential
 * is dead, so retrying the socket cannot help — the user needs a code.
 */
export function isTerminalConnection(state: AppState): boolean {
  if (state.connection === "revoked") {
    return true;
  }
  return state.shutdown !== null && !shouldRetry(state.shutdown.reason);
}
