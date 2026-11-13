/**
 * The web protocol on the wire, as TypeScript (`src/web/protocol.rs`).
 *
 * These are the shapes the host actually serializes, spelled `snake_case`
 * because that is what arrives — the translation into the app's own
 * `camelCase` model happens in `./adapt.ts` and nowhere else. Keeping the two
 * separate is what lets `src/state/model.ts` stay the shape the UI wants to
 * render rather than the shape the host happens to send (the mapping decision
 * `remote-control-hgqy` left to whoever wired the socket).
 *
 * The unions are deliberately **open**: every `switch` on `type` (and on a
 * delta's `change`) ends in a branch that ignores what it does not know,
 * because the host's forward-compatibility policy is "a newer host may send
 * frames an older tab has never heard of, and the tab must not drop the socket
 * over it".
 *
 * That openness is a **crash guard, not a substitute for the version check**.
 * Dropping an unknown frame is the right outcome only when the frame carried
 * news the tab can live without; a frame that answers something the tab
 * *asked for* becomes silence where the user expected an answer. So a new
 * frame kind still bumps `PROTOCOL_VERSION` — see below.
 */

/**
 * The version this tab speaks. Must match `protocol::PROTOCOL_VERSION`.
 *
 * v2 was D14 as revised: a seat is a writer or an observer, several writers may
 * be seated at once, and `holds_input` says which one has the turn. `Seat` and
 * `SeatRequest` are closed vocabularies and both grew a member, which the wire
 * protocol's forward-compatibility policy makes a bump by definition — a tab
 * left open across a host update is told to reload rather than served a
 * half-spoken protocol.
 *
 * **v3 adds `git_status`** (`remote-control-ll5.8`), and the reason is the
 * concrete one rather than the formal one. The palette is host-driven: this
 * tab renders whatever `Snapshot::commands` lists, and a v3 host lists
 * `show_git_status` as a row that runs. A stale v2 tab would therefore pass
 * the version check, be handed the row, send it, have it accepted — and then
 * hit `handle`'s `default` branch and drop the answer. The user clicks
 * **Show Git Status** and nothing happens, with no error anywhere. Reloading
 * is the honest outcome, and this constant is what produces it.
 *
 * `Snapshot.help` / `Snapshot.about` did *not* contribute to the bump: both
 * are additive optional fields, and a host that sends neither leaves this tab
 * rendering its "did not send its keybindings" path, which is a lesser panel
 * rather than a wrong one.
 *
 * **v4 adds `configuration`** (`remote-control-1p22`), the same argument one
 * step along. A v4 host lists `open_configuration` as a row that *runs* — it
 * used to refuse — so a stale v3 tab would send it, be acked `applied`, and
 * then drop the panel on `handle`'s `default` branch: an ack and no
 * configuration manager. The staged edits that travel the other way did not
 * contribute, because they ride inside `Command.args`, which has been
 * free-form since v1.
 *
 * **v5 makes the new-agent target a host-driven three-state cycle** after
 * `Change Project Default Base` added existing-branch attachment. A stale v4
 * tab only understands the old local boolean and would display the wrong form,
 * so it must reload rather than answer a target it did not render.
 *
 * **v6 is explicit command targets**: a palette command's `args` may name the
 * session or terminal it acts on. This tab sends no target and keeps the
 * shared selection; a native client always names one.
 *
 * **v7 is the FlightDeck Worker** (`remote-control-qlyd.9`): new delta and
 * frame kinds a headless worker sends its attached window. A worker serves this
 * SPA too (`remote-control-vjtb`), so the tab handles every one of them — but
 * only a worker ever sends them, and everything they drive switches on
 * `Snapshot.host_kind === "worker"`, so a TUI or Desktop host renders exactly
 * as it did at v6. v7 is also the host's first range: a TUI host still serves
 * v6, for a Desktop that has not updated yet, and answers each peer at the
 * version it asked for.
 *
 * **v8 is uploading files to a worker** (`remote-control-usem`): the window
 * asks with the `begin_upload` worker command and the worker answers with a
 * new frame, `upload_plan` — the target folder, each file's final name and a
 * single-use ticket for `PUT /upload/{ticket}`, or the names already taken, or
 * a refusal. A worker never sends `upload_plan` to a peer below v8, and
 * refuses `begin_upload` from one.
 *
 * **v9 is downloading files from a worker** (`remote-control-pz6d`, the
 * Transfer files panel): `browse_dir` takes `include_files` and a
 * `dir_listing` then carries the folder's `files`; the window asks with the
 * `begin_download` worker command and the worker answers with a new frame,
 * `download_plan` — each file's size and a single-use ticket for
 * `GET /download/{ticket}`, or a refusal. A worker never sends
 * `download_plan` to a peer below v9, and refuses `begin_download` from one.
 */
export const PROTOCOL_VERSION = 10;

/** `GET /ws` — JSON over **text** frames, no subprotocol. */
export const WS_PATH = "/ws";

export interface WireGeometry {
  readonly cols: number;
  readonly rows: number;
}

export type WireBucket =
  | "in_progress"
  | "idle"
  | "waiting"
  | "error"
  | "unknown";

export interface WireSessionStatus {
  /** FlightDeck's own spelling, with spaces: `needs attention`, `session lost`. */
  readonly interpreted: string;
  /** A status set by hand, or `null` when the agent's own is authoritative. */
  readonly manual: string | null;
  readonly bucket: WireBucket;
  readonly running_time_secs: number;
}

export interface WireGitBar {
  readonly branch: string | null;
  readonly added: number;
  readonly modified: number;
  readonly removed: number;
  readonly ahead: number;
  readonly behind: number;
  readonly drift: number;
  readonly has_upstream: boolean;
  readonly files_changed: number;
  /** `false` means git has not answered yet — `git: ?`, never `clean`. */
  readonly collected: boolean;
}

export interface WireTerminalView {
  readonly terminal_id: string;
  readonly session_id: string;
  readonly role: "primary" | "agent" | "shell";
  readonly title: string;
  readonly geometry: WireGeometry;
  readonly byte_len: number;
  readonly replay_from: number;
  readonly alive: boolean;
  readonly exit_code?: number | null;
}

export interface WireSessionView {
  readonly session_id: string;
  readonly project_id: string;
  readonly name: string;
  readonly agent: string;
  readonly agent_display_name: string;
  readonly phase: "creating" | "ready";
  readonly status: WireSessionStatus;
  readonly git: WireGitBar;
  readonly terminals: readonly WireTerminalView[];
  /** `false` is the fact behind `· <agent> reports no lifecycle` (§5.1). */
  readonly lifecycle_reporting: boolean;
  readonly recovered?: boolean;
  readonly attached_existing_branch?: boolean;
  /** The absolute folder the session runs in on the host: its worktree, or
   * the project root for a session on the base branch. Empty (or absent) from
   * a host that predates the field. */
  readonly worktree_path?: string;
}

export interface WireProjectView {
  readonly project_id: string;
  readonly name: string;
  readonly root: string;
  readonly base_branch: string;
  readonly dot?: WireBucket | null;
  readonly sessions: readonly WireSessionView[];
}

export interface WireSelection {
  readonly project_id?: string | null;
  readonly session_id?: string | null;
  readonly terminal_id?: string | null;
  readonly split_view?: boolean;
}

export interface WireSeatInfo {
  /** `null` is the desktop — the one seat that is never a browser. */
  readonly viewer_id: string | null;
  /** The compact one-line chip label, `192.168.2.20 · Chrome on macOS`. */
  readonly label: string;
  /**
   * The address the **host observed on the socket**, or absent/`null` for the
   * desktop row and for a host from before the split.
   *
   * This exists so 2f's panel never has to split `label` on its separator. A
   * user-agent string is attacker-supplied free text and can contain ` · `, so
   * a browser-side split is a parse the string itself gets to steer.
   */
  readonly address?: string | null;
  /**
   * What the browser said it is (`Chrome on macOS`) — the host's own word for
   * a claim it only ever displays. Absent or `null` means nobody knows, and 2f
   * drops the row rather than printing a guess.
   */
  readonly user_agent_label?: string | null;
  /**
   * The **role**: may this surface type at all. Several rows can be `writing`
   * at once — that is what lifting D14's single-controller restriction means.
   */
  readonly seat: "writing" | "observing";
  /**
   * The **turn**: is this surface typing *right now*. At most one row in a list
   * has it, and none does when the lock is free (nobody has typed for
   * `INPUT_LOCK_IDLE_MS`).
   *
   * Absent means the host did not say, which reads as `false` — not "the lock
   * is free" but "this row is not the one holding it", which is true of every
   * row on such a host.
   */
  readonly holds_input?: boolean;
  readonly since_ms: number;
  readonly is_you: boolean;
}

export interface WireActivityEvent {
  readonly event_id: string;
  readonly at_ms: number;
  readonly project_id: string;
  readonly project_name: string;
  readonly session_id: string;
  readonly session_name: string;
  readonly from: string;
  readonly to: string;
  readonly manual?: string | null;
  readonly reason?: string;
  readonly tier: "attention" | "finished" | "quiet";
  readonly read?: boolean;
}

/**
 * D13: who opened the dialog. Internally tagged on `origin`, so a `desktop`
 * origin carries no label at all rather than an empty one.
 */
export type WireDialogOrigin =
  | { readonly origin: "desktop" }
  | {
      readonly origin: "browser";
      readonly viewer_id?: string | null;
      readonly label: string;
    };

export interface WireDialogChoice {
  readonly label: string;
  readonly selected: boolean;
}

export interface WireDialogKey {
  readonly key: string;
  readonly label: string;
  /** Whether pressing this button **dismisses** the dialog rather than deciding
   * it (`protocol::DialogKey::cancels`, §6.5 R19). The dialogs do not agree on
   * a cancel key — `n` in the close confirmations, `c` in the push
   * confirmation, `Esc` in the forms — so the host says which one it is rather
   * than leaving the browser to read the labels. */
  readonly cancels?: boolean;
  /** Whether this button does nothing while the dialog's text field is blank
   * (`protocol::DialogKey::needs_input`): the New Agent form's Create. */
  readonly needs_input?: boolean;
}

/**
 * `DialogView::body` (`protocol::DialogBody`) — the dialog *shell* artboard 1d
 * describes, which every dialog uses and which 1e's new-agent form is one
 * instance of. Every field is optional on the wire (`skip_serializing_if`), so
 * every field is optional here.
 */
export interface WireDialogBody {
  readonly input?: string | null;
  readonly list?: readonly WireDialogChoice[];
  readonly list_filter?: boolean;
  readonly buttons?: readonly WireDialogKey[];
  /** `false` means the host will refuse a `dialog_confirm` for this dialog. */
  readonly confirmable?: boolean;
  readonly refusal?: string;
  /** Artboard 1g's second step, when one of this dialog's buttons has one
   * (`protocol::ConfirmGate`, `specs/WEB_INTERFACE.md` §6.5 R13). */
  readonly confirm_gate?: WireConfirmGate | null;
}

/**
 * The host's description of artboard 1g's step 2 (`protocol::ConfirmGate`).
 *
 * Every field is the host's: the button it guards, the exact name to type, and
 * the sentence saying why a second step exists. The browser words none of it —
 * a locally-authored "are you sure?" would be the browser claiming something the
 * host did not say, and the host is the only thing that knows what it is about
 * to destroy.
 */
export interface WireConfirmGate {
  readonly key: string;
  readonly expected: string;
  readonly instruction: string;
}

/** The one open dialog (D13). `kind` is open on purpose: an unknown one renders
 * the generic shell rather than failing. */
export interface WireDialogView {
  readonly dialog_id: string;
  readonly kind: string;
  readonly title: string;
  readonly origin: WireDialogOrigin;
  readonly body?: WireDialogBody | null;
}

/**
 * One row of the host's command inventory (`protocol::CommandView`).
 *
 * The whole palette is built from these (`remote-control-ll5.12`): the host is
 * the only thing that knows which names this build accepts, so a name it does
 * not send cannot appear in the palette at all. Every field except the first
 * four is `skip_serializing_if` on the host, so every one of them is optional
 * here.
 */
export interface WireCommandView {
  readonly id: string;
  readonly label: string;
  readonly group: string;
  readonly run: { readonly name: string; readonly args?: unknown };
  /** D16: the effect lands on the host's machine. */
  readonly host_only?: boolean;
  /** D13: answers the open dialog rather than being a palette row. */
  readonly answers_dialog?: boolean;
  /** 1d's right-hand tag (`destructive`, `next`, …). */
  readonly annotation?: string | null;
  /** `project` / `session` / `terminal` / `unread_activity`: the row is a
   * template the browser expands into one row per target. */
  readonly target?: string | null;
  /** The sentence the host answers with if this row is sent. */
  readonly refusal?: string | null;
}

/**
 * SPECS §23's help screen, as `src/tui/help.rs` has it. The desktop's overlay
 * and this one are drawn from the same value: the host builds it once and
 * sends it, so neither surface can document a keyboard the other does not have
 * (`specs/WEB_INTERFACE.md` §6.5 R16).
 */
export interface WireHelpDoc {
  readonly title: string;
  /** Prose that must be read *before* the shortcuts — SPECS §32's isolated-run
   * note is the only one this build sends. Absent on an ordinary run. */
  readonly notes?: readonly WireHelpNote[];
  readonly sections: readonly WireHelpSection[];
}

export interface WireHelpNote {
  readonly title: string;
  readonly lines: readonly string[];
}

export interface WireHelpSection {
  readonly title: string;
  readonly rows: readonly WireHelpRow[];
}

export interface WireHelpRow {
  /** Not always a key: `Mouse click`, `Drag past edge` and `+ project` are
   * rows too, because SPECS §22's interaction model is not only the keyboard. */
  readonly keys: string;
  readonly description: string;
}

/** The About screen's version and credits, from the host's own build. */
export interface WireAboutDoc {
  readonly name: string;
  readonly version: string;
  readonly tagline: string;
  readonly credits: readonly WireAboutCredit[];
  readonly url: string;
}

export interface WireAboutCredit {
  readonly role: string;
  readonly name: string;
}

/**
 * SPECS §21's git status panel, in answer to one viewer's `show_git_status`
 * (`remote-control-ll5.8`, §6.5 R16).
 *
 * A per-viewer reply, not a delta: nothing changed, the host ran `git status`
 * because *this* tab asked. It arrives beside the `ack` for the same `seq` —
 * the ack says the command was applied, this says what it produced.
 *
 * Two fields are optional and both mean **absent**, never zero:
 * `upstream` is missing until the branch has been pushed (and the ahead/behind
 * counts live inside it, so there is no way to receive commits ahead of a
 * remote that does not exist), and `compare_url` is missing unless the remote
 * is a GitHub remote that has the branch.
 */
export interface WireGitStatus {
  readonly type: "git_status";
  readonly seq: number;
  readonly session_id: string;
  readonly session_name: string;
  readonly branch: string;
  readonly base_branch: string;
  readonly base_drift: number;
  readonly dirty: boolean;
  readonly changed_files: number;
  readonly upstream?: {
    readonly name: string;
    readonly ahead: number;
    readonly behind: number;
  } | null;
  readonly worktree_path: string;
  readonly compare_url?: string | null;
}

/**
 * SPECS §8's configuration manager, in answer to one viewer's
 * `open_configuration` (`remote-control-1p22`, `specs/WEB_INTERFACE.md`
 * §6.5 R22).
 *
 * A per-viewer reply like `git_status`, and for the same reason: the two
 * config files live on the host's disk and change under it, so this is a read
 * taken at the moment the panel opens rather than something a snapshot could
 * hold static or a delta could describe. One reader opened the manager, so one
 * reader is answered.
 *
 * **Both scopes ride in one frame.** Artboard 1f's `Tab` switches between them
 * and the origin tag column is the whole point of the screen, so the host
 * resolves the layering twice and sends both lists — the browser never walks a
 * layer itself.
 */
export interface WireConfiguration {
  readonly type: "configuration";
  readonly seq: number;
  readonly project_name: string;
  /** `null` when the host has no home directory to keep a global base in. */
  readonly global_path?: string | null;
  readonly project_path: string;
  readonly project_rows: readonly WireConfigRow[];
  readonly global_rows: readonly WireConfigRow[];
}

/** A config value is the JSON shape `config.toml` holds — a bare bool or a
 * bare string — so nothing has to be unwrapped before it can be drawn. */
export type WireConfigValue = boolean | string;

/** One curated setting resolved for one scope (`protocol::ConfigRowView`). */
export interface WireConfigRow {
  /** The host's real TOML path, `section.key`. What a staged edit names. */
  readonly key: string;
  readonly label: string;
  readonly kind: "bool" | "choice" | "text";
  readonly value: WireConfigValue;
  /** For `kind: "choice"`, the host's own cycle order; absent otherwise. */
  readonly choices?: readonly string[];
  readonly origin: "set_here" | "global" | "default";
  /** What this row would read if the override in *this* scope were cleared —
   * the host's answer to `c`, so a browser staging one need not resolve a
   * layer to show what it will produce. */
  readonly inherited: WireConfigValue;
  readonly inherited_origin: "set_here" | "global" | "default";
}

export interface WireSnapshot {
  readonly type: "snapshot";
  readonly protocol_version: number;
  readonly host_version: string;
  readonly server_time_ms: number;
  readonly viewer_id: string;
  readonly seat: "writing" | "observing";
  readonly seats: readonly WireSeatInfo[];
  readonly last_input_seq: number;
  readonly projects: readonly WireProjectView[];
  readonly selection: WireSelection;
  /** D4: the host's authoritative grid for the selected terminal. */
  readonly geometry: WireGeometry;
  readonly replay_capacity_bytes: number;
  readonly activity: readonly WireActivityEvent[];
  /**
   * D13: the open dialog, or absent when none is. It rides on the snapshot
   * because a dialog is state — a tab that attaches while one is open has to
   * paint it, and it never saw the `Delta::DialogOpened`.
   */
  readonly dialog?: WireDialogView | null;
  /**
   * The command inventory the palette renders (`remote-control-ll5.12`). It
   * rides on the snapshot rather than on a delta because it is static for the
   * life of the host build — there is no change for a `Delta` to describe.
   *
   * Absent from a host that predates it, which means an empty palette rather
   * than a locally-authored stand-in: a row this host cannot execute is exactly
   * what the host-sent inventory exists to prevent.
   */
  readonly commands?: readonly WireCommandView[];
  /**
   * SPECS §23's help screen (`remote-control-ll5.8`). It rides here for
   * `commands`'s reason exactly: static for the life of the host build, so
   * there is no change for a `Delta` to describe.
   *
   * Absent from a host that predates it, and the browser then has no help to
   * show — which is the honest answer. A locally-authored keybinding list
   * would be this tab documenting a FlightDeck it cannot see, which is the
   * same failure the host-sent `commands` inventory exists to prevent.
   */
  readonly help?: WireHelpDoc | null;
  /** The About screen, on the snapshot for `help`'s reason. */
  readonly about?: WireAboutDoc | null;
  /**
   * SPECS §30's update notice — 1a's `● v1.16.0 available` chip
   * (`remote-control-gk94`, §6.5 R25). Rides on the snapshot because the host
   * checks once, at startup, so there is no later change for a delta to carry.
   *
   * Absent or `null` is the host saying **it has no notice**, which is not the
   * same claim as "you are up to date": the check may be off, the run may be
   * `--isolated`, or the answer may not have come back. The chip is simply not
   * drawn, exactly as the desktop's status bar draws nothing.
   */
  readonly update?: WireUpdateNotice | null;
  /**
   * `[ui] agent_tab_position` (`remote-control-ecsv`, §6.5 R24) — artboard 1h
   * position 4's mirror, as the host has the setting.
   *
   * Rides on the snapshot rather than on `ServerMsg::Configuration` because
   * that frame only arrives while 1f is open, and the body row has to be right
   * in the first frame. Absent from a host that predates it, and `left` is then
   * what is laid out — the default the setting itself has, so an old host gets
   * the layout it has always drawn rather than a guess.
   */
  readonly sidebar_position?: "left" | "right" | null;
  /**
   * v7: what kind of FlightDeck this is. Absent from a v6 host, which is a TUI
   * or a Desktop. Everything worker-specific in this tab switches on
   * `"worker"`, and nothing else.
   */
  readonly host_kind?: WireHostKind | null;
  /** v7: a worker's own state (`protocol::WorkerSnapshot`). Only a worker
   * sends it. */
  readonly worker?: WireWorkerSnapshot | null;
}

/** `protocol::HostKind`. */
export type WireHostKind = "tui" | "desktop" | "worker";

/** `protocol::WorkerSnapshot` — everything a worker's window paints from. */
export interface WireWorkerSnapshot {
  /** `$HOME` on the worker: the picker's root and its boundary (W16). */
  readonly home: string;
  /** The first shell, for a pre-v10 window. */
  readonly shell?: WireWorkerShellView | null;
  /** Every shell, in the order they were opened (W35, v10). */
  readonly shells?: readonly WireWorkerShellView[];
  readonly health?: WireHealth | null;
  readonly status: WireWorkerStatusView;
  readonly connections: WireConnectionsView;
  /** Kept while nothing was attached, oldest first. Carried once: the worker
   * marks them read as this window attaches. */
  readonly host_messages?: readonly WireHostMessage[];
}

/** `protocol::WorkerShellView`. Also the body of `Delta::WorkerShell`. */
export interface WireWorkerShellView {
  readonly terminal_id: string;
  readonly title: string;
  readonly geometry: WireGeometry;
  readonly byte_len: number;
  readonly replay_from: number;
  readonly alive: boolean;
  readonly exit_code?: number | null;
}

/** `protocol::Health` (PRD W29). Every reading the platform cannot give is
 * absent rather than a guess. Also the body of `Delta::Health`. */
export interface WireHealth {
  readonly sampled_at_ms: number;
  readonly uptime_secs: number;
  readonly version: string;
  /** The one-minute load average × 100. */
  readonly load_avg_1m_centi?: number | null;
  readonly cpu_count: number;
  readonly mem_total_bytes?: number | null;
  readonly mem_available_bytes?: number | null;
  readonly disk_free_home_bytes?: number | null;
  readonly disk_free_projects_root_bytes?: number | null;
  readonly agents_total: number;
  readonly agents_running: number;
}

/** `protocol::WorkerStatusView`. Also the body of `Delta::WorkerStatus`. */
export interface WireWorkerStatusView {
  readonly listening?: readonly string[];
  readonly advertised?: string | null;
  readonly not_listening_reason?: string | null;
  readonly unread_messages?: number;
  readonly projects_root: string;
}

/** `protocol::ConnectionsView`, as seen by this recipient. Also the body of
 * `Delta::Connections`. */
export interface WireConnectionsView {
  readonly window?: WireWindowInfo | null;
  readonly background_links?: number;
}

/** `protocol::WindowInfo`. */
export interface WireWindowInfo {
  readonly label: string;
  readonly address?: string | null;
  readonly since_ms: number;
  readonly same_device?: boolean;
}

/** `protocol::HostMessage`. Also the body of `Delta::HostMessage`. */
export interface WireHostMessage {
  /** Monotonic per worker run, so a replay can be de-duplicated. */
  readonly id: number;
  readonly at_ms: number;
  readonly level: "info" | "warning" | "error";
  readonly text: string;
}

/** `protocol::NotificationView`. The body of `Delta::Notification`. */
export interface WireNotificationView {
  readonly at_ms: number;
  readonly title: string;
  readonly body: string;
  readonly sound: "none" | "completion" | "input_required";
}

/** `ServerMsg::DirListing` — one directory under `$HOME`, beside the `ack`
 * for the `browse_dir` it answers (PRD W9, W16). */
export interface WireDirListing {
  readonly type: "dir_listing";
  readonly seq: number;
  readonly path: string;
  /** `null` at `$HOME`: the picker goes no higher. */
  readonly parent?: string | null;
  readonly home: string;
  readonly entries: readonly WireDirEntry[];
  /** v9: the folder's regular files, when `browse_dir` said
   * `include_files: true`. Absent or empty otherwise. */
  readonly files?: readonly WireFileEntry[];
  readonly truncated?: boolean;
}

/** `protocol::FileEntryView` (v9): one regular file in a `dir_listing`. */
export interface WireFileEntry {
  readonly name: string;
  /** Absolute, as listed (not resolved). */
  readonly path: string;
  readonly size?: number;
}

export interface WireDirEntry {
  readonly name: string;
  readonly path: string;
  readonly is_git_repo?: boolean;
  readonly is_open_project?: boolean;
}

/** `ServerMsg::ProjectScan` — the repositories under the projects root. */
export interface WireProjectScan {
  readonly type: "project_scan";
  readonly seq: number;
  readonly root: string;
  readonly root_exists: boolean;
  readonly repos: readonly WireScannedRepo[];
  readonly refusal?: string | null;
}

export interface WireScannedRepo {
  readonly name: string;
  readonly path: string;
  readonly is_open_project?: boolean;
}

/** `ServerMsg::CloneProgress`. `clone_id` is the seq of the `clone_project`. */
export interface WireCloneProgress {
  readonly type: "clone_progress";
  readonly clone_id: number;
  readonly url: string;
  readonly phase: string;
  readonly percent?: number | null;
}

/** `ServerMsg::CloneResult`. On success the worker has opened the clone. */
export interface WireCloneResult {
  readonly type: "clone_result";
  readonly clone_id: number;
  readonly url: string;
  readonly ok: boolean;
  readonly path?: string | null;
  readonly project_id?: string | null;
  readonly error?: string | null;
}

/** What a worker does with a file whose name is already taken (v8). */
export type WireOnConflict = "ask" | "replace" | "keep_both";

/** One file in a `begin_upload`'s `files`: its name and exact size. */
export interface WireUploadFile {
  readonly name: string;
  readonly size: number;
}

/**
 * `ServerMsg::UploadPlan` (v8) — the worker's answer to this window's
 * `begin_upload`, beside the `ack` for the same `seq`. Exactly one of: every
 * file has a `ticket` (granted — `PUT` its bytes to `/upload/{ticket}` with
 * the cookie, `Content-Length` = its size, within two minutes), `conflicts`
 * is true (`on_conflict: "ask"` and some names are taken, `exists` says
 * which; nothing granted), or `refusal` says why nothing was granted.
 */
export interface WireUploadPlan {
  readonly type: "upload_plan";
  readonly seq: number;
  /** The target folder, canonical and absolute on the worker. */
  readonly dir: string;
  readonly files: readonly WireUploadPlanFile[];
  readonly conflicts?: boolean;
  readonly refusal?: string | null;
}

export interface WireUploadPlanFile {
  /** The name asked for. */
  readonly name: string;
  /** The name it gets: the same, or `name (n).ext` under `keep_both`. */
  readonly final_name: string;
  readonly ticket?: string | null;
  /** Whether something by the asked name is already there. */
  readonly exists?: boolean;
}

/**
 * `ServerMsg::DownloadPlan` (v9) — the worker's answer to this window's
 * `begin_download`, beside the `ack` for the same `seq`. All or nothing:
 * every file has a `ticket` (granted — `GET /download/{ticket}` with the
 * cookie, within two minutes, once), or `refusal` says why nothing was.
 */
export interface WireDownloadPlan {
  readonly type: "download_plan";
  readonly seq: number;
  /** The source folder, canonical and absolute on the worker. */
  readonly dir: string;
  readonly files: readonly WireDownloadPlanFile[];
  readonly refusal?: string | null;
}

export interface WireDownloadPlanFile {
  /** The name asked for, and the name the download suggests. */
  readonly name: string;
  /** Bytes at grant time; the `GET`'s `Content-Length` wins if it changed. */
  readonly size?: number;
  readonly ticket?: string | null;
}

/**
 * The reply to `PUT /upload/{ticket}` (v8). `reason` is a stable code
 * (`invalid_ticket`, `size_mismatch`, `length_required`, `exists`, …) and
 * `message` a sentence; an authentication refusal carries the same body as
 * `/auth/session`'s, without `message`.
 */
export type WireUploadReply =
  | { readonly ok: true; readonly path: string; readonly bytes: number }
  | {
      readonly ok: false;
      readonly reason: string;
      readonly message?: string;
    };

/** A newer release than the host is running (SPECS §30). */
export interface WireUpdateNotice {
  /** Bare, like `host_version` (`1.16.0`). The `v` belongs to the chip. */
  readonly latest_version: string;
}

export interface WireTermBytes {
  readonly type: "term_bytes";
  readonly terminal_id: string;
  /** Offset of the **first** byte of `data`, monotonic per terminal. */
  readonly offset: number;
  /** Standard padded base64 — a keystroke is not necessarily a character. */
  readonly data: string;
  /** `true` on a resume that outran the ring: `offset` is past our cursor. */
  readonly truncated?: boolean;
}

export interface WireAck {
  readonly type: "ack";
  readonly seq: number;
  readonly outcome: "applied" | "rejected" | "ignored";
  readonly detail?: string;
}

export interface WireError {
  readonly type: "error";
  readonly code: string;
  readonly message: string;
  readonly seq?: number;
  readonly version?: {
    readonly local: number;
    readonly peer: number;
    readonly min_supported: number;
    readonly max_supported: number;
  };
  /**
   * Set with `code: "seat_held"`: the writer that holds the input lock and is
   * mid-burst, so the keystroke `seq` names was refused rather than mixed into
   * theirs. It does **not** cost us our seat.
   */
  readonly incumbent?: WireSeatInfo;
  readonly retry_after_ms?: number;
}

export interface WireShutdown {
  readonly type: "shutdown";
  readonly reason: string;
  readonly self_initiated?: boolean;
  readonly detail?: string;
}

/**
 * v7: another window took a worker over (PRD W17), and the socket closes after
 * this. Terminal: the tab does **not** redial on its own, because attaching
 * again as a window would take the worker straight back. Only the user's
 * "Take it back" does that.
 */
export interface WireTakenOver {
  readonly type: "taken_over";
  /** The device label of the window that took over. */
  readonly by: string;
}

/**
 * `delta` is tagged twice: `type: "delta"` then `change: …`.
 *
 * `change: "seats"` carries `seats: WireSeatInfo[]`, `you`, and a
 * `server_time_ms` — the host clock its rows' `since_ms` are dated against, the
 * same pairing `WireSnapshot` has always had. Absent or `0` means an older host
 * sent none, and the rows stay undated.
 */
export interface WireDeltaEnvelope {
  readonly type: "delta";
  readonly change: string;
  readonly [key: string]: unknown;
}

export type ServerFrame =
  | WireSnapshot
  | WireTermBytes
  | WireAck
  | WireError
  | WireShutdown
  | WireTakenOver
  | WireGitStatus
  | WireConfiguration
  | WireDirListing
  | WireProjectScan
  | WireCloneProgress
  | WireCloneResult
  | WireUploadPlan
  | WireDownloadPlan
  | WireDeltaEnvelope
  | { readonly type: string };

/** The opening frame. There is no `hello`: `attach` carries the version. */
export interface WireAttach {
  readonly type: "attach";
  readonly protocol_version: number;
  /**
   * `write` seats us as a writer and is never refused; `take_over` does that
   * *and* takes the input lock now, which is the one explicit override in the
   * model; `observe` never contends at all.
   */
  readonly seat: "write" | "take_over" | "observe";
  readonly cursors: readonly { terminal_id: string; next_offset: number }[];
  readonly resume_viewer: string | null;
  readonly viewport: WireGeometry | null;
  readonly client: { readonly user_agent: string | null } | null;
  /**
   * v7: a remote window or a worker's background link. Absent means a window,
   * which is what this tab always is.
   */
  readonly connection?: "window" | "background";
}

export interface WireInput {
  readonly type: "input";
  /** Monotonic per viewer, continuing across reconnects (§5.1). */
  readonly seq: number;
  readonly terminal_id: string;
  /** Standard padded base64 of the raw bytes. */
  readonly data: string;
}

/**
 * v7, worker hosts only (PRD W5): size one terminal's PTY. The one relaxation
 * of D4 — a worker has no pane of its own, so its window's pane is the only
 * size there is. Never sent to a TUI or Desktop host, which drops it anyway.
 */
export interface WireResizeTerminal {
  readonly type: "resize_terminal";
  readonly terminal_id: string;
  readonly cols: number;
  readonly rows: number;
}

export type ClientFrame = WireAttach | WireInput | WireResizeTerminal;

// ---------------------------------------------------------------------------
// base64, both directions
//
// `atob`/`btoa` are byte-oriented (each char is one byte 0..255), which is
// exactly right here: PTY output is arbitrary bytes and a UTF-8 sequence may
// straddle two frames. So the bytes are decoded to a `Uint8Array` and handed to
// xterm.js, which owns the UTF-8 decoder and its partial-sequence state. Going
// via a JS string would corrupt any multi-byte character split across frames.
// ---------------------------------------------------------------------------

/** Standard padded base64 → raw bytes. */
export function decodeBase64(data: string): Uint8Array {
  const binary = atob(data);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

/** Raw bytes → standard padded base64. */
export function encodeBase64(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary);
}
