/**
 * Protocol v1 on the wire, as TypeScript (`src/web/protocol.rs`).
 *
 * These are the shapes the host actually serializes, spelled `snake_case`
 * because that is what arrives — the translation into the app's own
 * `camelCase` model happens in `./adapt.ts` and nowhere else. Keeping the two
 * separate is what lets `src/state/model.ts` stay the shape the UI wants to
 * render rather than the shape the host happens to send (the mapping decision
 * `remote-control-hgqy` left to whoever wired the socket).
 *
 * Only the frames M1's browser has to understand are typed here. The unions are
 * deliberately **open**: every `switch` on `type` (and on a delta's `change`)
 * ends in a branch that ignores what it does not know, because the host's
 * forward-compatibility policy is "a newer host may send frames an older tab
 * has never heard of, and the tab must not drop the socket over it".
 */

/** The version this tab speaks. Must match `protocol::PROTOCOL_VERSION`. */
export const PROTOCOL_VERSION = 1;

/** `GET /ws` — protocol v1, JSON over **text** frames, no subprotocol. */
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
  readonly label: string;
  readonly seat: "controlling" | "observing";
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
  readonly buttons?: readonly WireDialogKey[];
  /** `false` means the host will refuse a `dialog_confirm` for this dialog. */
  readonly confirmable?: boolean;
  readonly refusal?: string;
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

export interface WireSnapshot {
  readonly type: "snapshot";
  readonly protocol_version: number;
  readonly host_version: string;
  readonly server_time_ms: number;
  readonly viewer_id: string;
  readonly seat: "controlling" | "observing";
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
  readonly incumbent?: WireSeatInfo;
  readonly retry_after_ms?: number;
}

export interface WireShutdown {
  readonly type: "shutdown";
  readonly reason: string;
  readonly self_initiated?: boolean;
  readonly detail?: string;
}

/** `delta` is tagged twice: `type: "delta"` then `change: …`. */
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
  | WireDeltaEnvelope
  | { readonly type: string };

/** The opening frame. There is no `hello`: `attach` carries the version. */
export interface WireAttach {
  readonly type: "attach";
  readonly protocol_version: number;
  readonly seat: "control" | "take_over" | "observe";
  readonly cursors: readonly { terminal_id: string; next_offset: number }[];
  readonly resume_viewer: string | null;
  readonly viewport: WireGeometry | null;
  readonly client: { readonly user_agent: string | null } | null;
}

export interface WireInput {
  readonly type: "input";
  /** Monotonic per viewer, continuing across reconnects (§5.1). */
  readonly seq: number;
  readonly terminal_id: string;
  /** Standard padded base64 of the raw bytes. */
  readonly data: string;
}

export type ClientFrame = WireAttach | WireInput;

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
