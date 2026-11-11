/**
 * A FlightDeck Worker's window, as pure rules (`specs/WORKER_PRD.md`,
 * `remote-control-vjtb`).
 *
 * A worker is headless: it has no pane, no status bar and no folder dialog of
 * its own, so the window attached to it is where its geometry, its projects and
 * its words come from. The native client says the same things in
 * `src/web/client/session.rs` ("A worker host") and `messages.rs`; this module
 * is the browser's copy of those rules, kept as functions of `AppState` so each
 * one is a unit test rather than a screenshot.
 *
 * **Everything here switches on `hostKind === "worker"`.** A TUI or Desktop
 * host never reaches a worker branch, so it renders exactly as it did before
 * this module existed.
 */

import type { HostCommand, HostMessage, WorkerHealth, WorkerShell } from "./model";
import type { AppState, TerminalGeometry } from "./types";

/** The worker shell's fixed terminal id (`WORKER_SHELL_TERMINAL_ID`, W15). */
export const WORKER_SHELL_TERMINAL_ID = "worker:shell";

/** The worker-only command names (`protocol::worker_command`). Not palette
 * rows: the picker and the worker shell tab send them. */
export const WORKER_COMMAND = {
  BROWSE_DIR: "browse_dir",
  SCAN_PROJECTS: "scan_projects",
  OPEN_PROJECT_PATH: "open_project_path",
  CLONE_PROJECT: "clone_project",
  RESTART_WORKER_SHELL: "restart_worker_shell",
} as const;

/** The palette row a worker refuses and this window answers itself, with the
 * project picker. */
export const OPEN_PROJECT_COMMAND = "open_project";

/**
 * The palette rows a worker refuses (`commands::worker_refusal`) that a
 * worker's window **hides** rather than explains: each is about the host's own
 * screen or process (quitting it, its web interface, pairing a phone to it),
 * which a worker does not have. The native window hides the same rows
 * (`views::palette_entries`). `open_project` is the one refused row that is
 * kept — this window runs it, as the picker.
 */
export const WORKER_HIDDEN_COMMANDS: ReadonlySet<string> = new Set([
  "quit",
  "start_web_interface",
  "stop_web_interface",
  "show_web_access",
  "pair_phone",
  "unpair_phone",
]);

export function isWorker(state: AppState): boolean {
  return state.hostKind === "worker";
}

/**
 * A host row as a worker's window offers it: `null` to hide it, the row with
 * its refusal lifted when this window runs it itself, else unchanged. On any
 * other host every row is returned untouched.
 */
export function workerPaletteRow(
  state: AppState,
  row: HostCommand,
): HostCommand | null {
  if (!isWorker(state)) {
    return row;
  }
  if (row.refusal !== null && WORKER_HIDDEN_COMMANDS.has(row.run.name)) {
    return null;
  }
  if (row.run.name === OPEN_PROJECT_COMMAND) {
    return { ...row, refusal: null, hostOnly: false };
  }
  return row;
}

/**
 * The terminal on screen in the single pane.
 *
 * On a worker, the shell's tab shows the worker shell (or nothing, when it has
 * not been started), and a project tab shows the shared selection's terminal —
 * `null` rather than `""` when the workspace has none. Any other host gets the
 * expression the pane has always used, `""` included, so nothing about a TUI
 * or Desktop host changes.
 */
export function onScreenTerminalId(state: AppState): string | null {
  if (!isWorker(state)) {
    return state.selection?.terminalId ?? null;
  }
  if (state.workerShellTab) {
    return state.worker?.shell?.terminalId ?? null;
  }
  const id = state.selection?.terminalId ?? "";
  return id === "" ? null : id;
}

/**
 * The grid to draw `terminalId` at. D4 on a TUI or Desktop host: the one
 * host-owned grid. On a worker every terminal has its own (W5), sized to the
 * window that drew it, so the terminal's own grid wins and the shared one is
 * only the fallback for a terminal the host has not described yet.
 */
export function stageGeometry(
  state: AppState,
  terminalId: string,
): TerminalGeometry | null {
  if (!isWorker(state)) {
    return state.geometry;
  }
  return state.terminalGeometry[terminalId] ?? state.geometry;
}

/** The smallest grid a worker is asked for. A pane squeezed below it is a
 * transient layout state, not a size a shell can be used at. */
export const MIN_COLS = 20;
export const MIN_ROWS = 5;

/**
 * Pixels → cells, for W5: how many whole cells fit the pane. Floors, so the
 * grid never overflows the pane it was measured from; `null` for a measurement
 * that cannot be a pane (nothing laid out yet, or no cell to divide by).
 */
export function cellsFor(
  available: { readonly width: number; readonly height: number },
  cell: { readonly width: number; readonly height: number },
): TerminalGeometry | null {
  if (
    !(cell.width > 0) ||
    !(cell.height > 0) ||
    !(available.width > 0) ||
    !(available.height > 0)
  ) {
    return null;
  }
  return {
    cols: Math.max(MIN_COLS, Math.floor(available.width / cell.width)),
    rows: Math.max(MIN_ROWS, Math.floor(available.height / cell.height)),
  };
}

/**
 * What a worker's main area shows instead of (or under) a terminal:
 *
 * - `empty` — no projects yet, on a project tab: the "No projects yet" card;
 * - `shell_missing` — the shell's tab, and the worker has no shell to show;
 * - `shell_exited` — the shell's tab, and the shell has exited: the terminal
 *   stays (it is the last thing it printed) with a Restart shell bar under it.
 *
 * `null` everywhere else, and always on a TUI or Desktop host.
 */
export type WorkerCard = "empty" | "shell_missing" | "shell_exited";

export function workerCard(state: AppState): WorkerCard | null {
  if (!isWorker(state)) {
    return null;
  }
  if (state.workerShellTab) {
    const shell = state.worker?.shell ?? null;
    if (shell === null) {
      return "shell_missing";
    }
    return shell.alive ? null : "shell_exited";
  }
  return state.projects.length === 0 ? "empty" : null;
}

/** The exited shell's line, the native window's words. */
export function shellExitLine(shell: WorkerShell | null): string {
  if (shell === null) {
    return "The worker shell is not running.";
  }
  return shell.exitCode === null
    ? "The worker shell exited."
    : `The worker shell exited (${shell.exitCode}).`;
}

// ---------------------------------------------------------------------------
// Host messages (`messages.rs`)
// ---------------------------------------------------------------------------

/** One message as a line: its level when louder than information, then the
 * worker's words verbatim (`messages::line`). */
export function messageLine(message: HostMessage): string {
  switch (message.level) {
    case "info":
      return message.text;
    case "warning":
      return `Warning: ${message.text}`;
    case "error":
      return `Error: ${message.text}`;
  }
}

/** The heading of the messages kept while nothing was attached
 * (`messages::away_text`'s first line). */
export function awayHeading(count: number): string {
  return count === 1
    ? "1 message while you were away"
    : `${count} messages while you were away`;
}

/** The whole "while you were away" text: the heading, then one line each,
 * oldest first (`messages::away_text`). */
export function awayText(messages: readonly HostMessage[]): string {
  return [awayHeading(messages.length), ...messages.map(messageLine)].join("\n");
}

// ---------------------------------------------------------------------------
// Health (`messages::health_line`)
// ---------------------------------------------------------------------------

/** Health in one line: `load 0.42 · 18 GB free · 3 agents`. A reading the
 * worker could not take is left out rather than shown as zero. */
export function healthLine(health: WorkerHealth): string {
  const parts: string[] = [];
  if (health.loadCenti !== null) {
    const whole = Math.floor(health.loadCenti / 100);
    const centi = String(health.loadCenti % 100).padStart(2, "0");
    parts.push(`load ${whole}.${centi}`);
  }
  const free = health.diskFreeProjectsRootBytes ?? health.diskFreeHomeBytes;
  if (free !== null) {
    parts.push(`${diskBytes(free)} free`);
  }
  if (health.agentsTotal === 1) {
    parts.push("1 agent");
  } else if (health.agentsTotal > 1) {
    parts.push(`${health.agentsTotal} agents`);
  }
  return parts.join(" · ");
}

/** A size as people read a disk's: decimal units, whole numbers from 10 up. */
export function diskBytes(n: number): string {
  const GB = 1e9;
  const MB = 1e6;
  if (n >= 10 * GB) {
    return `${Math.round(n / GB)} GB`;
  }
  if (n >= GB) {
    return `${(n / GB).toFixed(1)} GB`;
  }
  return `${Math.round(n / MB)} MB`;
}

// ---------------------------------------------------------------------------
// Browser notifications (W18)
// ---------------------------------------------------------------------------

/**
 * What the browser lets this page do about notifications, as `main.ts` read it
 * (`Notification.permission`). `unsupported` covers both a browser without the
 * API and an insecure context, where the API refuses to ask at all — plain
 * `http://` to a worker on a tailnet address is exactly that case.
 */
export type NotificationPermissionState =
  | "unsupported"
  | "default"
  | "granted"
  | "denied";

/** Whether the status bar offers "Enable notifications": a worker, a context
 * that can ask, and a person who has not answered yet. */
export function offersNotifications(state: AppState): boolean {
  return isWorker(state) && state.notifications === "default";
}
