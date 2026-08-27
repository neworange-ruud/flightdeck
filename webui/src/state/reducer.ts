import { decideEscape } from "../input/escape";
import { buildCommandInventory, clampIndex, paletteColumns } from "./commands";
import { CONFIG_FIELDS, selectableConfigFields } from "./config";
import { findProject, findSession, shouldRetry } from "./model";
import type { AccessState, Project, Selection } from "./model";
import { ACCESS_CODE_LENGTH } from "./model";
import { dropAckedInput, isTerminalConnection } from "./types";
import type { AppAction, AppState, ConfigState, PaletteState } from "./types";

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
       */
      if (isTerminalConnection(state) && action.status !== "connected") {
        return state;
      }
      if (action.status === "connected") {
        return {
          ...state,
          connection: "connected",
          shutdown: null,
          /** The picture is live again, so the photograph's clock goes away. */
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
        latencyMs: snapshot.latencyMs,
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
        connection: shouldRetry(shutdown.reason)
          ? "reconnecting"
          : shutdown.reason === "token_revoked"
            ? "revoked"
            : "stopped",
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

    case "access/required":
      return {
        ...state,
        access: {
          screen: action.screen,
          code: "",
          refused: "",
          attemptsRemaining: action.attemptsRemaining,
          lockoutSeconds: action.lockoutSeconds,
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
      return { ...state, seats: action.seats, seat: action.seat };

    case "takeover/held":
      return {
        ...state,
        takeover: { kind: "arriving", incumbent: action.incumbent },
        /** We asked for control and were refused, so we do not have it. */
        seat: "observing",
      };

    case "takeover/evicted":
      return {
        ...state,
        takeover: {
          kind: "evicted",
          byAddress: action.byAddress,
          lastInputAgo: action.lastInputAgo,
        },
        seat: "observing",
      };

    case "takeover/claim":
      /** The seat is claimed optimistically because the host answers with a
       * `Delta::Seats` either way: if the claim fails, `seats/changed` will say
       * so, and there is no frame to wait for in between (takeover is a
       * re-`Attach`, not a request/response of its own). */
      return { ...state, takeover: null, seat: "controlling" };

    case "takeover/observe":
      /** D14: `w Watch read-only` and `Esc Cancel` land in the same place, a
       * live read-only view. Clearing the prompt is what makes the terminal
       * behind it trustworthy again — 2d's rule that colour means "live". */
      return { ...state, takeover: null, seat: "observing" };

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
      const { flat } = paletteColumns(buildCommandInventory(state), palette.filter);
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
      const { flat } = paletteColumns(buildCommandInventory(state), palette.filter);
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
            edits: action.outcome === "applied" ? {} : config.edits,
            lastOutcome: { outcome: action.outcome, detail: action.detail ?? null },
          },
        };
      }

      return state;
    }

    /* --- Configuration manager (1f) ------------------------------------ */

    case "config/open":
      return state.config !== null
        ? state
        : {
            ...state,
            config: {
              scope: "project",
              selectedKey: selectableConfigFields()[0]?.key ?? "",
              edits: {},
              pending: [],
              lastOutcome: null,
            },
          };

    case "config/close":
      return state.config === null ? state : { ...state, config: null };

    case "config/scope": {
      const config = state.config;
      if (config === null) {
        return state;
      }
      return {
        ...state,
        config: {
          ...config,
          scope: config.scope === "project" ? "global" : "project",
        },
      };
    }

    case "config/move": {
      const config = state.config;
      if (config === null) {
        return state;
      }
      const fields = selectableConfigFields();
      const currentIndex = fields.findIndex((f) => f.key === config.selectedKey);
      const nextIndex = clampIndex(
        (currentIndex === -1 ? 0 : currentIndex) + action.delta,
        fields.length,
      );
      const key = fields[nextIndex]?.key ?? config.selectedKey;
      return key === config.selectedKey
        ? state
        : { ...state, config: { ...config, selectedKey: key } };
    }

    case "config/select": {
      const config = state.config;
      if (config === null || config.selectedKey === action.key) {
        return state;
      }
      return { ...state, config: { ...config, selectedKey: action.key } };
    }

    case "config/clear": {
      const config = state.config;
      /** SPECS §8: `c` clears a *project* override. Global scope, or a field
       * with no project override to clear, is a no-op rather than a guess at
       * what else `c` might mean there. */
      if (config === null || config.scope !== "project") {
        return state;
      }
      const field = CONFIG_FIELDS.find((f) => f.key === config.selectedKey);
      if (field === undefined || field.hostOnly === true) {
        return state;
      }
      return {
        ...state,
        config: {
          ...config,
          edits: { ...config.edits, [field.key]: { kind: "clear" } },
        },
      };
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

    default: {
      // Exhaustiveness check: a new AppAction variant left unhandled above is
      // a compile error here, not a silently-ignored action at runtime.
      const unreachable: never = action;
      return unreachable;
    }
  }
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
    revokedAgo: null,
  };
}
