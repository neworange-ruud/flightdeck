import { decideEscape } from "../input/escape";
import { clampIndex, paletteColumns, paletteInventory } from "./commands";
import {
  editableText,
  NO_CONFIG_EDITS,
  nextConfigValue,
  resolveConfigRow,
} from "./config";
import type { ConfigEdit, ConfigScope } from "./config";
import { selectedChoice, visibleChoices } from "./dialog";
import { findProject, findSession, shouldRetry } from "./model";
import type { AccessState, Project, SeatInfo, Selection } from "./model";
import { incumbentFromSeats } from "./seats";
import { widthClass } from "./viewport";
import { ACCESS_CODE_LENGTH } from "./model";
import { dropAckedInput, isTerminalConnection } from "./types";
import type {
  AppAction,
  AppState,
  ConfigState,
  DialogState,
  PaletteState,
} from "./types";

/**
 * The one entry point later tasks dispatch through. Pure by construction: no
 * network, no DOM, no `Date.now()`/`Math.random()` — every input is in
 * `action`, every output is a new `AppState`. That purity is what D15 asks
 * `vitest` to exercise directly, with no server, socket or browser involved.
 *
 * Side effects (actually sending queued input over the websocket once
 * connected, actually constructing/resizing the xterm.js instance when
 * `geometry` changes, telling the host about a selection change so the desktop
 * follows it per D3) belong to the caller, not here — they read the new state
 * after `reduce` returns and act on it. Keeping that boundary is what lets
 * this function stay unit-testable once `src/web/protocol.rs` lands and real
 * actions replace these placeholders.
 */
export function reduce(state: AppState, action: AppAction): AppState {
  return clampSplitFocus(reduceAction(state, action));
}

/**
 * `remote-control-zbwx`, §6.5 R24: `splitFocus` is an index into the *selected
 * session's* terminals, so any action that moves the selection or replaces the
 * project list can leave it pointing past the last column — and then 1c's glow
 * is on no column at all, because `ui/splitView.ts` sets `data-focused` by
 * comparing each index against it.
 *
 * Applied here, once, rather than in each `selection/*` case: the reachable
 * paths are a sidebar click, a feed row's jump, a project switch and a
 * host-driven selection (which lands as a whole `snapshot/received` — see
 * `wire/socket.ts`'s `selection` delta), and a fix per case is a fix that the
 * next case added will not have. `moveSplitFocus` in `ui/app.ts` clamps as well
 * and stays as it is: it clamps *before stepping*, which is a different job.
 *
 * Identity when nothing moved, so this costs an unrelated action no re-render.
 */
function clampSplitFocus(state: AppState): AppState {
  const selection = state.selection;
  const session =
    selection === null
      ? null
      : findSession(state.projects, selection.projectId, selection.sessionId);
  const columns = session?.terminals.length ?? 0;
  /** No session, or a session with no terminals yet: column 0 is where the
   * first one will be, which is also the initial state's value. */
  const clamped = columns === 0 ? 0 : Math.min(state.splitFocus, columns - 1);
  return clamped === state.splitFocus ? state : { ...state, splitFocus: clamped };
}

function reduceAction(state: AppState, action: AppAction): AppState {
  switch (action.type) {
    case "connection/changed": {
      /**
       * Q5, enforced here rather than trusted to the transport: a host that
       * sent `Shutdown` is gone, so nothing may later claim the link is coming
       * back. The retry loop is *stopped by the state machine* — a transport
       * that keeps dialling still cannot paint "reconnecting", which is the
       * behaviour the requirement is actually about.
       *
       * `connected` is the one exception, and it is not a loophole: if the host
       * really did come back, it is back, and pretending otherwise would leave
       * a working session behind a dead-end screen. Reaching it clears the
       * terminal state.
       *
       * `catching_up` is the same exception wearing a different word, and for
       * the same reason: Q3's drain is only ever entered off a `Snapshot`, so
       * reaching it *is* the host answering. Refusing it would leave a tab that
       * had genuinely reattached painting a dead host while bytes arrived
       * behind the screen.
       */
      const live = action.status === "connected" || action.status === "catching_up";
      if (isTerminalConnection(state) && !live) {
        return state;
      }
      if (live) {
        return {
          ...state,
          connection: action.status,
          shutdown: null,
          /** The picture is live again, so the photograph's clock goes away.
           * 2d is explicit that catching-up counts as live: "colour is back,
           * so it is trustworthy". */
          staleness: null,
        };
      }
      return { ...state, connection: action.status };
    }

    case "geometry/set":
      return { ...state, geometry: action.geometry };

    case "input/queue":
      /** §5.1: **never dropped.** There is no branch here that discards a
       * keystroke because the link is down — that is the whole point of a
       * queue. The seq is assigned at queue time, so order is fixed before
       * any reconnect can shuffle it. */
      return {
        ...state,
        pendingInput: [...state.pendingInput, action.data],
        inputSeq: state.inputSeq + 1,
      };

    case "input/flush":
      return state.pendingInput.length === 0
        ? state
        : { ...state, pendingInput: [] };

    case "input/acked": {
      const kept = dropAckedInput(state, action.throughSeq);
      /** Identity when nothing was acknowledged, so a redundant ack from the
       * host is not a re-render. */
      return kept === state.pendingInput ? state : { ...state, pendingInput: kept };
    }

    case "snapshot/received": {
      const { snapshot } = action;
      return {
        ...state,
        projects: snapshot.projects,
        /** D3: the host's selection wins. A browser that kept its own would be
         * a second source of truth, which is the drift this decision removes. */
        selection: snapshot.selection,
        /**
         * D3/D8 (`remote-control-ll5.7`): split view is shared instance state,
         * so the host's own snapshot is what moves `layout` — never a local
         * toggle. A `Delta::Selection` does the same thing on a live update;
         * see `wire/socket.ts`'s `onDelta`.
         */
        layout: snapshot.splitView ? "split" : "single",
        geometry: snapshot.geometry,
        viewers: snapshot.viewers,
        /**
         * `latencyMs` is deliberately **not** here. It is not a host fact —
         * the host cannot see this link from this end — so it arrives from the
         * transport as `latency/set` and a snapshot must not clear it. It used
         * to be a field on this model that the adapter filled with `null` for
         * every host, which is why 2c never drew anything but a bare
         * `● connected`.
         */
        update: snapshot.update,
        seats: snapshot.seats,
        /** D14: the host says which seat we got; we never assume it. */
        seat: snapshot.seat,
        /**
         * D11's backfill replaces rather than appends: the host's store is the
         * record (min 200 events / 24h) and a reattach re-sends it, so
         * appending would double every row the tab had already seen.
         */
        activity: snapshot.activity,
        /**
         * D13: the dialog is app state, so the host's whole picture includes
         * it — a snapshot with none means none is open, and a snapshot that
         * carries one is how a freshly attached tab paints a dialog it never
         * saw open. The draft survives a re-announcement of the *same* dialog
         * (see `dialog/opened`), so a coalesced resync mid-typing does not
         * empty the branch field.
         */
        dialog: mergeDialog(state.dialog, snapshot.dialog),
        /**
         * `remote-control-ll5.12`: the palette's whole inventory, replaced
         * wholesale like every other fact on the snapshot. A host that lists
         * fewer rows than last time offers fewer rows — that is the point of
         * reading the palette off the wire instead of compiling it in.
         */
        commands: snapshot.commands,
        /**
         * `remote-control-ll5.8`: SPECS §23's help and the About screen, from
         * the host, replaced wholesale like everything else on the snapshot.
         * `null` from a host that sends neither, and nothing is substituted —
         * see `state/help.ts` for why a browser-authored keybinding list would
         * be worse than none.
         */
        help: snapshot.help,
        about: snapshot.about,
        /**
         * 1h position 4 (`remote-control-ecsv`, §6.5 R24): `[ui]
         * agent_tab_position` is the host's setting, so it arrives with every
         * other host fact and is replaced wholesale like them. The browser
         * never keeps a preference of its own here — the desktop and this tab
         * mirror on the same word out of the same file.
         */
        sidebarPosition: snapshot.sidebarPosition,
        /** A dated seat list completes an arriving takeover panel — see
         * `refreshArrivingIncumbent`. */
        takeover: refreshArrivingIncumbent(state.takeover, snapshot.seats),
      };
    }

    case "selection/project": {
      const project = findProject(state.projects, action.projectId);
      if (project === null) {
        return state;
      }
      return { ...state, selection: firstSelectionIn(project) };
    }

    case "selection/session": {
      if (state.selection === null) {
        return state;
      }
      const project = findProject(state.projects, state.selection.projectId);
      const session =
        project?.sessions.find((s) => s.id === action.sessionId) ?? null;
      if (project === null || session === null) {
        return state;
      }
      return {
        ...state,
        selection: {
          projectId: project.id,
          sessionId: session.id,
          /** Selecting a session selects its first terminal: the agent tab.
           * A selection that named a terminal from the previous session would
           * point at nothing. */
          terminalId: session.terminals[0]?.id ?? "",
        },
      };
    }

    case "selection/terminal": {
      if (state.selection === null) {
        return state;
      }
      return {
        ...state,
        selection: { ...state.selection, terminalId: action.terminalId },
      };
    }

    case "mode/set":
      /** Leaving terminal focus also closes any half-typed `Esc Esc` chord —
       * an armed window that outlived its mode would fire on the next visit. */
      return { ...state, mode: action.mode, escArmedAt: null };

    case "layout/set":
      return { ...state, layout: action.layout };

    case "split/focus":
      return { ...state, splitFocus: action.column };

    case "input/esc": {
      const outcome = decideEscape(state.escArmedAt, action.at);
      if (outcome.kind === "leave_focus") {
        return { ...state, mode: "app", escArmedAt: null };
      }
      /**
       * §5: a single `Esc` still reaches the agent — `esc to interrupt` is the
       * key users press most. It is queued like any other keystroke so the
       * "never dropped, never reordered" guarantee (§5.1) covers it too — which
       * means it must take a seq like any other keystroke. Appending without
       * one would shift `firstPendingSeq` by a keystroke and make the next
       * `input/acked` drop the wrong element.
       */
      return {
        ...state,
        pendingInput: [...state.pendingInput, "\x1b"],
        inputSeq: state.inputSeq + 1,
        escArmedAt: outcome.armedAt,
      };
    }

    case "connection/shutdown": {
      const { shutdown } = action;
      const revoked = shutdown.reason === "token_revoked";
      return {
        ...state,
        shutdown,
        /**
         * Q5: only a restart is worth waiting for. Everything else — including
         * a reason this build does not recognise — lands in a terminal state
         * that names itself, never in a retry loop.
         *
         * `token_revoked` gets the `revoked` bucket rather than `stopped`,
         * because 2b/2c give it different words and a different colour: the
         * host is fine, your credential is not, and the fix is a code rather
         * than restarting anything.
         */
        connection: revoked
          ? "revoked"
          : shouldRetry(shutdown.reason)
            ? "reconnecting"
            : "stopped",
        /**
         * And it raises 2b's revoked panel, here, from the frame — not only
         * from the HTTP refusal `main.ts` reads on a page load.
         *
         * The panel was drawn *over a live session* precisely for this case, and
         * for a long time the only way to reach it was to reload, which is the
         * one thing a user whose access just vanished has no reason to do
         * (`remote-control-glmt`, §6.5 R20). The host now closes the socket with
         * this reason the moment the desktop revokes, so the frame is the
         * earliest and most certain news there is.
         *
         * `revokedAgo` stays `null`: the frame carries no revocation time, and
         * 2b prints the sentence without its "12s ago" clause rather than with
         * an invented one. A reload afterwards goes through the refusal, which
         * does carry the time, and fills it in.
         *
         * Any access screen already up is replaced rather than merged over,
         * because a half-typed code on the entry screen is an answer to a
         * question that has just been overtaken.
         */
        access: revoked
          ? { ...blankAccess(), screen: "revoked", revokedAgo: null }
          : state.access,
      };
    }

    case "version/mismatch":
      /** 2c keeps the connection and the mode chip exactly as they were: the
       * link is fine and control was never lost. Only the tab is old. */
      return { ...state, versionMismatch: action.mismatch };

    case "staleness/set":
      return { ...state, staleness: action.staleness };

    case "replay/set":
      return { ...state, replay: action.replay };

    case "latency/set":
      return { ...state, latencyMs: action.latencyMs };

    case "access/required":
      return {
        ...state,
        access: {
          screen: action.screen,
          code: "",
          refused: "",
          attemptsRemaining: action.attemptsRemaining,
          lockoutSeconds: action.lockoutSeconds,
          lockoutLengthSeconds: action.lockoutLengthSeconds,
          codeTtlSeconds: action.codeTtlSeconds,
          revokedAgo: null,
        },
      };

    case "access/digit": {
      const access = state.access;
      /** A digit with no access screen up is not a mistake worth crashing on —
       * it is a keystroke that raced the overlay coming down. */
      if (access === null || !/^[0-9]$/.test(action.digit)) {
        return state;
      }
      if (access.code.length >= ACCESS_CODE_LENGTH) {
        return state;
      }
      return {
        ...state,
        access: {
          ...access,
          code: access.code + action.digit,
          /** Typing again leaves the rejected screen: the user is answering
           * it, so continuing to shout `That code did not work` at them is
           * both wrong and in the way. The attempt budget stays on screen. */
          screen: access.screen === "rejected" ? "code_entry" : access.screen,
        },
      };
    }

    case "access/backspace": {
      const access = state.access;
      if (access === null || access.code === "") {
        return state;
      }
      return {
        ...state,
        access: { ...access, code: access.code.slice(0, -1) },
      };
    }

    case "access/refused": {
      const access = state.access ?? blankAccess();
      return {
        ...state,
        access: {
          ...access,
          screen: action.screen,
          /** 2b's rejected screen shows the four digits that failed, in the
           * alert frame. Keeping them is what makes "it was mistyped" a claim
           * the user can check. */
          refused: access.code,
          code: "",
          attemptsRemaining: action.attemptsRemaining,
          lockoutSeconds: action.lockoutSeconds,
          lockoutLengthSeconds: action.lockoutLengthSeconds,
          codeTtlSeconds: action.codeTtlSeconds,
        },
      };
    }

    case "access/granted":
      return { ...state, access: null };

    case "access/dismiss":
      /** The overlay only. `connection` is untouched, so the strip keeps
       * saying `not allowed` and the pane stays a photograph — which is
       * exactly what the user asked to keep looking at. */
      return state.access === null ? state : { ...state, access: null };

    case "access/revoked":
      return {
        ...state,
        connection: "revoked",
        access: {
          ...(state.access ?? blankAccess()),
          screen: "revoked",
          code: "",
          revokedAgo: action.revokedAgo,
        },
        /** 2b: "Everything you can see below this dialog is a photograph from
         * the moment access ended." The pane's stale treatment is derived from
         * the connection, so there is nothing else to set here. */
      };

    case "access/retry":
      return {
        ...state,
        access: {
          ...(state.access ?? blankAccess()),
          screen: "code_entry",
          code: "",
          refused: "",
          revokedAgo: null,
        },
      };

    case "seats/changed":
      return {
        ...state,
        seats: action.seats,
        seat: action.seat,
        takeover: refreshArrivingIncumbent(state.takeover, action.seats),
      };

    case "takeover/held":
      /**
       * D14 as revised: a refused keystroke costs the **turn**, never the seat.
       * Dropping to `observing` here — which is what v1 did, because a refusal
       * meant the seat itself was taken — would paint `MODE: —` over a tab that
       * is still a writer and will be typing again the moment the other one
       * pauses. The seat only ever changes when the host says so.
       */
      return {
        ...state,
        takeover: { kind: "arriving", incumbent: action.incumbent },
      };

    case "takeover/evicted":
      /** Same rule from the other side: we lost the lock, not the seat. */
      return {
        ...state,
        takeover: {
          kind: "evicted",
          byAddress: action.byAddress,
          lastInputAgo: action.lastInputAgo,
        },
      };

    case "takeover/claim":
      /** The seat is claimed optimistically because the host answers with a
       * `Delta::Seats` either way: if the claim fails, `seats/changed` will say
       * so, and there is no frame to wait for in between (takeover is a
       * re-`Attach`, not a request/response of its own). */
      return { ...state, takeover: null, seat: "writing" };

    case "takeover/observe":
      /** D14: `w Watch read-only` is a real destination. Clearing the prompt is
       * what makes the terminal behind it trustworthy again — 2d's rule that
       * colour means "live". */
      return { ...state, takeover: null, seat: "observing" };

    case "takeover/dismiss":
      /** `Esc Cancel`, which is no longer the same act as `w` — see the action's
       * own note. The seat is untouched: we were refused a keystroke, not a
       * seat, and waiting is a real answer now that the lock frees itself. */
      return { ...state, takeover: null };

    case "activity/received":
      return { ...state, activity: [...state.activity, ...action.events] };

    case "activity/read": {
      const ids = new Set(action.ids);
      if (ids.size === 0) {
        return state;
      }
      let changed = false;
      const activity = state.activity.map((event) => {
        if (!event.read && ids.has(event.id)) {
          changed = true;
          return { ...event, read: true };
        }
        return event;
      });
      return changed ? { ...state, activity } : state;
    }

    case "feed/set":
      return state.feedOpen === action.open
        ? state
        : { ...state, feedOpen: action.open };

    /**
     * 1h's breakpoint, decided here rather than in CSS (`state/viewport.ts`
     * says why) and **only** here.
     *
     * Crossing in either direction closes the slide-over. Going wide, the
     * sidebar becomes 1a's column again and an "open" flag would be a
     * remembered state nothing renders; coming back narrow, reopening a panel
     * the reader dismissed three resizes ago would be the app deciding for
     * them. Closing on every crossing is the same rule stated once.
     */
    case "viewport/measured": {
      const width = widthClass(action.pixels);
      if (width === state.width) {
        return state;
      }
      return { ...state, width, sidebarOpen: false };
    }

    /**
     * The slide-over exists only below 900px, and this refusal is what makes
     * that structural instead of a convention: at wide there is no panel to
     * open, so a stray `sidebar/set` from a resize race or a future caller
     * cannot leave a flag set that the next crossing would honour.
     */
    case "sidebar/set":
      if (state.width !== "narrow") {
        return state.sidebarOpen ? { ...state, sidebarOpen: false } : state;
      }
      return state.sidebarOpen === action.open
        ? state
        : { ...state, sidebarOpen: action.open };

    case "host/set":
      return { ...state, host: action.host };

    case "retry/set":
      return { ...state, retry: action.retry };

    case "selection/jump": {
      const session = findSession(
        state.projects,
        action.projectId,
        action.sessionId,
      );
      if (session === null) {
        /** A feed row can outlive its session (the host retains 24h of
         * events). Doing nothing is the honest answer; inventing a selection
         * would move the desktop somewhere the user did not ask for. */
        return state;
      }
      return {
        ...state,
        selection: {
          projectId: action.projectId,
          sessionId: session.id,
          terminalId: session.terminals[0]?.id ?? "",
        },
      };
    }

    /* --- Command palette (1d) ----------------------------------------- */

    case "palette/open":
      return state.palette !== null
        ? state
        : {
            ...state,
            palette: {
              filter: "",
              column: 0,
              index: 0,
              pending: [],
              lastOutcome: null,
            },
          };

    case "palette/close":
      return state.palette === null ? state : { ...state, palette: null };

    case "palette/type": {
      if (state.palette === null) {
        return state;
      }
      const filter = state.palette.filter + action.char;
      /** A changed filter is read from the top: leaving the highlight at
       * whatever index a longer list had would point at a different, and
       * possibly wrong, row after the list moves under it. */
      return {
        ...state,
        palette: { ...state.palette, filter, column: 0, index: 0 },
      };
    }

    case "palette/backspace": {
      if (state.palette === null || state.palette.filter === "") {
        return state;
      }
      return {
        ...state,
        palette: {
          ...state.palette,
          filter: state.palette.filter.slice(0, -1),
          column: 0,
          index: 0,
        },
      };
    }

    case "palette/move": {
      const palette = state.palette;
      if (palette === null) {
        return state;
      }
      const { flat } = paletteColumns(paletteInventory(state), palette.filter);
      const index = clampIndex(
        palette.index + action.delta,
        flat[palette.column].length,
      );
      return index === palette.index
        ? state
        : { ...state, palette: { ...palette, index } };
    }

    case "palette/nextColumn": {
      const palette = state.palette;
      if (palette === null) {
        return state;
      }
      const other: 0 | 1 = palette.column === 0 ? 1 : 0;
      const { flat } = paletteColumns(paletteInventory(state), palette.filter);
      /** A `Tab` to an empty column would strand the highlight nowhere for
       * `Enter` to run — stay put instead. */
      if (flat[other].length === 0) {
        return state;
      }
      return {
        ...state,
        palette: {
          ...palette,
          column: other,
          index: clampIndex(palette.index, flat[other].length),
        },
      };
    }

    case "palette/dispatched": {
      if (state.palette === null) {
        return state;
      }
      const pending: PaletteState["pending"] = [
        ...state.palette.pending,
        { seq: action.seq, label: action.label },
      ];
      return { ...state, palette: { ...state.palette, pending } };
    }

    case "command/result": {
      /** One counter, one action, two possible owners (§5.1) — try the
       * palette first, then the config manager, and touch neither if `seq`
       * matches nothing in either: a seq that matches nothing pending is
       * never guessed at (requirement 4), the same rule for both queues. */
      const palette = state.palette;
      const paletteFound = palette?.pending.find(
        (item) => item.seq === action.seq,
      );
      if (palette !== null && paletteFound !== undefined) {
        return {
          ...state,
          palette: {
            ...palette,
            pending: palette.pending.filter((item) => item.seq !== action.seq),
            lastOutcome: {
              label: paletteFound.label,
              outcome: action.outcome,
              detail: action.detail ?? null,
            },
          },
        };
      }

      const config = state.config;
      const configFound = config?.pending.find(
        (item) => item.seq === action.seq,
      );
      if (config !== null && configFound !== undefined) {
        return {
          ...state,
          config: {
            ...config,
            pending: config.pending.filter((item) => item.seq !== action.seq),
            /**
             * Only a real `applied` Ack clears the staged edits — requirement
             * 5's "never optimism". A `rejected`/`ignored`/`read_only` result
             * leaves them staged so the user sees exactly what did not make
             * it and can retry with `s`.
             */
            edits: action.outcome === "applied" ? NO_CONFIG_EDITS : config.edits,
            lastOutcome: { outcome: action.outcome, detail: action.detail ?? null },
          },
        };
      }

      const dialog = state.dialog;
      const dialogFound = dialog?.pending.find(
        (item) => item.seq === action.seq,
      );
      if (dialog !== null && dialogFound !== undefined) {
        return {
          ...state,
          dialog: {
            ...dialog,
            pending: dialog.pending.filter((item) => item.seq !== action.seq),
            /**
             * D13: an `applied` answer does **not** close the dialog here. The
             * host closes it, and the browser learns that from
             * `Delta::DialogClosed` — which is the whole point of a dialog being
             * app state rather than an overlay. Closing it locally on the Ack
             * would be a second source of truth, and it would be wrong for the
             * one case that matters: a form the host kept open because it needs
             * something it did not get.
             */
            lastOutcome: {
              outcome: action.outcome,
              detail: action.detail ?? null,
            },
          },
        };
      }

      return state;
    }

    /* --- D13's shared dialog (1d/1e) ----------------------------------- */

    case "dialog/opened": {
      /** The same dialog re-announced (every coalesced snapshot does this)
       * keeps the local draft: a resync must not empty the branch field the
       * user is halfway through typing. A *different* dialog replaces it whole,
       * draft included — it is a different question. */
      const current = state.dialog;
      if (current !== null && current.id === action.dialog.id) {
        return {
          ...state,
          dialog: {
            ...action.dialog,
            draft: {
              ...current.draft,
              // `Tab` can replace the agent radios with filtered branches.
              // Their row indexes are different domains; the text is not.
              index:
                current.listFilter === action.dialog.listFilter
                  ? current.draft.index
                  : null,
            },
            pending: current.pending,
            lastOutcome: current.lastOutcome,
          },
        };
      }
      return { ...state, dialog: action.dialog };
    }

    case "dialog/closed": {
      /** A close for a dialog that is not the open one is a no-op: a late
       * `DialogClosed` for a dialog the host already replaced must not take the
       * live one down with it. */
      if (state.dialog === null || state.dialog.id !== action.dialogId) {
        return state;
      }
      return { ...state, dialog: null };
    }

    case "dialog/type": {
      const dialog = state.dialog;
      if (dialog === null || dialog.input === null) {
        return state;
      }
      return {
        ...state,
        dialog: {
          ...dialog,
          draft: {
            ...dialog.draft,
            text: dialog.draft.text + action.char,
            index: dialog.listFilter ? null : dialog.draft.index,
          },
        },
      };
    }

    case "dialog/backspace": {
      const dialog = state.dialog;
      if (dialog === null || dialog.input === null) {
        return state;
      }
      return {
        ...state,
        dialog: {
          ...dialog,
          draft: {
            ...dialog.draft,
            text: dialog.draft.text.slice(0, -1),
            index: dialog.listFilter ? null : dialog.draft.index,
          },
        },
      };
    }

    case "dialog/move": {
      const dialog = state.dialog;
      if (dialog === null || visibleChoices(dialog).length === 0) {
        return state;
      }
      const from = selectedChoice(dialog);
      /** Clamped, not wrapped — the same rule the palette's `move` follows. */
      const index = Math.min(
        Math.max(from + action.delta, 0),
        visibleChoices(dialog).length - 1,
      );
      return { ...state, dialog: { ...dialog, draft: { ...dialog.draft, index } } };
    }

    case "dialog/choose": {
      const dialog = state.dialog;
      if (
        dialog === null ||
        action.index < 0 ||
        action.index >= visibleChoices(dialog).length
      ) {
        return state;
      }
      return {
        ...state,
        dialog: { ...dialog, draft: { ...dialog.draft, index: action.index } },
      };
    }

    /* --- artboard 1g's second step (ll5.4, §6.5 R13) -------------------- */

    case "dialog/advance": {
      /** Only a gated dialog has a second panel to advance to. Everything else
       * decides on the first press, exactly as it does on the desktop. */
      const dialog = state.dialog;
      if (dialog === null || dialog.gate === null) {
        return state;
      }
      return {
        ...state,
        dialog: { ...dialog, draft: { ...dialog.draft, step: 2 } },
      };
    }

    case "dialog/gateType": {
      const dialog = state.dialog;
      if (dialog === null || dialog.gate === null) {
        return state;
      }
      return {
        ...state,
        dialog: {
          ...dialog,
          draft: {
            ...dialog.draft,
            confirmName: dialog.draft.confirmName + action.char,
          },
        },
      };
    }

    case "dialog/gateBackspace": {
      const dialog = state.dialog;
      if (dialog === null || dialog.gate === null) {
        return state;
      }
      return {
        ...state,
        dialog: {
          ...dialog,
          draft: {
            ...dialog.draft,
            confirmName: dialog.draft.confirmName.slice(0, -1),
          },
        },
      };
    }

    case "dialog/dispatched": {
      const dialog = state.dialog;
      if (dialog === null) {
        return state;
      }
      return {
        ...state,
        dialog: {
          ...dialog,
          pending: [...dialog.pending, { seq: action.seq, act: action.act }],
          lastOutcome: null,
        },
      };
    }

    /* --- Configuration manager (1f) ------------------------------------ */

    /**
     * The host answered. A closed panel opens on Project scope (1f's own
     * default); an open one keeps its scope, its cursor, its staged edits and
     * its pending queue and takes the new rows — which is what makes a save's
     * answer a repaint rather than a reset. The one thing it always discards is
     * an open inline edit: the value under it may have just changed on disk.
     */
    case "config/received": {
      const config = state.config;
      if (config === null) {
        return {
          ...state,
          palette: null,
          config: {
            doc: action.doc,
            scope: "project",
            selectedKey: action.doc.rows.project[0]?.key ?? "",
            edits: NO_CONFIG_EDITS,
            editing: null,
            pending: [],
            lastOutcome: null,
          },
        };
      }
      return {
        ...state,
        config: {
          ...config,
          doc: action.doc,
          selectedKey:
            action.doc.rows[config.scope].some(
              (row) => row.key === config.selectedKey,
            )
              ? config.selectedKey
              : (action.doc.rows[config.scope][0]?.key ?? ""),
          editing: null,
        },
      };
    }

    case "config/close":
      return state.config === null ? state : { ...state, config: null };

    case "config/scope": {
      const config = state.config;
      if (config === null) {
        return state;
      }
      const scope: ConfigScope =
        config.scope === "project" ? "global" : "project";
      /** The two scopes list the same fields, so the cursor survives the
       * switch — but it is clamped against the rows the host actually sent for
       * the scope being entered rather than assumed to. */
      const rows = config.doc.rows[scope];
      return {
        ...state,
        config: {
          ...config,
          scope,
          editing: null,
          selectedKey: rows.some((row) => row.key === config.selectedKey)
            ? config.selectedKey
            : (rows[0]?.key ?? ""),
        },
      };
    }

    case "config/move": {
      const config = state.config;
      if (config === null) {
        return state;
      }
      const rows = config.doc.rows[config.scope];
      const currentIndex = rows.findIndex(
        (row) => row.key === config.selectedKey,
      );
      const nextIndex = clampIndex(
        (currentIndex === -1 ? 0 : currentIndex) + action.delta,
        rows.length,
      );
      const key = rows[nextIndex]?.key ?? config.selectedKey;
      /** Moving discards an open inline edit, exactly as `select_next` does on
       * the desktop — an edit left behind on a row nobody is looking at is an
       * edit that gets committed by accident. */
      return key === config.selectedKey && config.editing === null
        ? state
        : { ...state, config: { ...config, selectedKey: key, editing: null } };
    }

    case "config/select": {
      const config = state.config;
      if (config === null || config.selectedKey === action.key) {
        return state;
      }
      return {
        ...state,
        config: { ...config, selectedKey: action.key, editing: null },
      };
    }

    /**
     * `Space`, routed on the host's own field kind: a toggle or a choice stages
     * the next value the host's options give; a text field opens the editor
     * instead, which is exactly the fork `toggle_selected` makes.
     */
    case "config/activate": {
      const config = state.config;
      const resolved = selectedConfigRow(config);
      if (config === null || resolved === null) {
        return state;
      }
      if (resolved.row.kind === "text") {
        return {
          ...state,
          config: { ...config, editing: editableText(resolved) },
        };
      }
      const staged = stageOnSelected(config, () => {
        const value = nextConfigValue(resolved);
        return value === null ? null : { kind: "set", value };
      });
      return staged === null ? state : { ...state, config: staged };
    }

    case "config/clear": {
      /** SPECS §8's `c`, in either scope — the same `clear_selected` the
       * desktop's `c` calls, which removes this scope's own override and lets
       * the value re-inherit. What it re-inherits *to* is the host's answer
       * (`ConfigRow.inherited`), not a layer this browser walked. */
      const staged = stageOnSelected(state.config, () => ({ kind: "clear" }));
      return staged === null ? state : { ...state, config: staged };
    }

    /* --- the inline text editor, key for key with `handle_config_key` ----- */

    case "config/editType": {
      const config = state.config;
      if (config === null || config.editing === null) {
        return state;
      }
      return {
        ...state,
        config: { ...config, editing: config.editing + action.char },
      };
    }

    case "config/editBackspace": {
      const config = state.config;
      if (config === null || config.editing === null) {
        return state;
      }
      return {
        ...state,
        config: { ...config, editing: config.editing.slice(0, -1) },
      };
    }

    case "config/editCommit": {
      const config = state.config;
      if (config === null || config.editing === null) {
        return state;
      }
      const buffer = config.editing;
      const staged = stageOnSelected(config, () => ({
        kind: "set",
        value: buffer,
      }));
      return staged === null
        ? { ...state, config: { ...config, editing: null } }
        : { ...state, config: { ...staged, editing: null } };
    }

    case "config/editCancel": {
      const config = state.config;
      if (config === null || config.editing === null) {
        return state;
      }
      return { ...state, config: { ...config, editing: null } };
    }

    case "config/dispatched": {
      const config = state.config;
      if (config === null) {
        return state;
      }
      const pending: ConfigState["pending"] = [
        ...config.pending,
        { seq: action.seq },
      ];
      return { ...state, config: { ...config, pending } };
    }

    /* --- The read-only overlays (remote-control-ll5.8, §6.5 R16) --------- */

    /**
     * Opening one closes the palette and replaces whatever other read-only
     * panel was up — `readOnly` holds one overlay, so that is the type doing
     * the work rather than a rule somebody has to remember.
     *
     * The palette closes because it is what opened this: 1d is the door, and
     * leaving it standing behind the panel would mean two overlays and two
     * `Esc`s to get back to the terminal. `config/received` makes the same
     * handoff when the host's answer opens 1f.
     */
    case "help/open":
      return { ...state, palette: null, readOnly: { kind: "help" } };

    case "about/open":
      return { ...state, palette: null, readOnly: { kind: "about" } };

    /**
     * SPECS §21's panel arrived from the host, so the overlay opens *with* its
     * facts. There is no state in which this panel is open and empty.
     */
    case "gitStatus/received":
      return {
        ...state,
        palette: null,
        readOnly: { kind: "git_status", panel: action.panel },
      };

    case "readOnly/close":
      return state.readOnly === null ? state : { ...state, readOnly: null };

    default: {
      // Exhaustiveness check: a new AppAction variant left unhandled above is
      // a compile error here, not a silently-ignored action at runtime.
      const unreachable: never = action;
      return unreachable;
    }
  }
}

/**
 * The row `↑↓` is pointing at, resolved through any staged edit — the value
 * `Space` and `c` act on, which is the value on screen and not the one the
 * host last sent.
 */
function selectedConfigRow(
  config: ConfigState | null,
): ReturnType<typeof resolveConfigRow> | null {
  if (config === null) {
    return null;
  }
  const row = config.doc.rows[config.scope].find(
    (candidate) => candidate.key === config.selectedKey,
  );
  return row === undefined
    ? null
    : resolveConfigRow(row, config.scope, config.edits);
}

/**
 * Stage one edit against the selected row in the active scope, or leave the
 * state alone when there is nothing to stage (no panel, no cursor, or a key
 * that does not act on this kind of field — `Space` on a text row, which opens
 * the editor instead).
 *
 * One helper rather than three copies of the same spread, because the scope
 * bookkeeping is the part that would rot: an edit written into the wrong
 * scope's record is a save that writes the wrong file.
 */
function stageOnSelected(
  config: ConfigState | null,
  edit: (resolved: ReturnType<typeof resolveConfigRow>) => ConfigEdit | null,
): ConfigState | null {
  const resolved = selectedConfigRow(config);
  if (config === null || resolved === null) {
    return null;
  }
  const staged = edit(resolved);
  if (staged === null) {
    return null;
  }
  return {
    ...config,
    editing: null,
    edits: {
      ...config.edits,
      [config.scope]: {
        ...config.edits[config.scope],
        [resolved.row.key]: staged,
      },
    },
  };
}

/**
 * The dialog after a whole-picture snapshot.
 *
 * Same rule as `dialog/opened`: the host's facts win, and the local draft
 * survives only for the dialog it belongs to.
 */
function mergeDialog(
  current: DialogState | null,
  next: DialogState | null,
): DialogState | null {
  if (next === null) {
    return null;
  }
  if (current === null || current.id !== next.id) {
    return next;
  }
  return {
    ...next,
    draft: current.draft,
    pending: current.pending,
    lastOutcome: current.lastOutcome,
  };
}

function firstSelectionIn(project: Project): Selection {
  const session = project.sessions[0];
  return {
    projectId: project.id,
    sessionId: session?.id ?? "",
    terminalId: session?.terminals[0]?.id ?? "",
  };
}

/**
 * Complete an open *arriving* takeover panel from a seat list.
 *
 * `WireError::seat_held` names the incumbent, but it is not a seat *list*: it
 * carries no `server_time_ms`, so the `connected` row opens blank. The seat list
 * that follows — a snapshot on the observe-attach, then a `Delta::Seats` — has
 * the host's clock beside it, and naming the same seat from the more complete
 * frame is what stops 2f drawing two rows on one path and three on another.
 *
 * Only the `arriving` panel, and only when the list still names a lock holder:
 * a list with the lock free is not a reason to blank out the name of the writer
 * we were just refused by — the lock frees itself the moment they go quiet, and
 * the panel would then be a dialog about nobody. The panel is closed by the
 * user's own answer (`takeover/claim`, `takeover/observe`), never by a seat
 * list.
 */
function refreshArrivingIncumbent(
  takeover: AppState["takeover"],
  seats: readonly SeatInfo[],
): AppState["takeover"] {
  if (takeover === null || takeover.kind !== "arriving") {
    return takeover;
  }
  const incumbent = incumbentFromSeats(seats);
  return incumbent === null ? takeover : { kind: "arriving", incumbent };
}

/**
 * The access screen a refusal lands on when none was up yet — a `401` on a
 * request this browser made before it had ever asked for a code.
 */
function blankAccess(): AccessState {
  return {
    screen: "code_entry",
    code: "",
    refused: "",
    attemptsRemaining: null,
    lockoutSeconds: null,
    /** Not known until the host has refused us once and said so. Every sentence
     * that would use these renders without its clause meanwhile. */
    lockoutLengthSeconds: null,
    codeTtlSeconds: null,
    revokedAgo: null,
  };
}
