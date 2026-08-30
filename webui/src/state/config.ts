/**
 * Artboard `1f — CONFIGURATION MANAGER` (`specs/design/flightdeck-web-turn1.dc.html`
 * lines 611-663), SPECS §8's layering rules, and
 * `specs/WEB_INTERFACE.md` §6.5 R22.
 *
 * Pure module, no DOM, no store — the same split `state/commands.ts` uses for
 * the palette: this file owns the shapes the host's answer arrives in and the
 * arithmetic of a *staged* edit; `ui/configManager.ts` only ever renders what
 * `resolveConfigRow` hands it.
 *
 * ## The browser owns no field, no key, no value and no layer
 *
 * This module used to carry a `CONFIG_FIELDS` constant with invented per-layer
 * values, and its keys did not even match the host's (`notifications.on_finished`
 * against the host's `notifications.on_finish`). It is gone. Every field, its
 * TOML path, its value, its choices and its origin tag now arrive on
 * `ServerMsg::Configuration` — the host reading its own two files through the
 * very `ConfigManager` the desktop's `Open Configuration` builds — for exactly
 * the reason R7 put the command inventory on the wire: the host is the only
 * thing that knows what it implements.
 *
 * That is also why `web.port` and `web.replay_bytes` are not here. The host
 * deliberately leaves them out of the curated set (`src/tui/config_manager.rs`
 * `:485-494`: this manager's text fields commit a TOML *string*, which would
 * corrupt a `u16`), and since the list is the host's, a browser cannot offer
 * them by accident.
 *
 * ## What a *staged* edit is, and why it is honest
 *
 * `Space`/`c` do not travel on their own. They stage, exactly as the desktop's
 * keys write into an in-memory table until `s`, and `s` sends the whole set as
 * one `open_configuration` frame carrying `ConfigSaveRequest` args. Until then
 * the panel shows what the save *will* produce, and it does so without walking
 * a layer:
 *
 *   - a staged **set** reads as the value that was set, `(set here)` — putting
 *     an explicit value in a scope is what "set here" means, whatever the value;
 *   - a staged **clear** reads as the row's own `inherited` / `inheritedOrigin`,
 *     which is the *host's* answer to what `c` leaves behind.
 *
 * A staged edit is shown in the scope it was made in and nowhere else. That is
 * not a limitation being papered over: nothing has been written yet, so the
 * other scope really does still read what is on disk — and once `s` lands, the
 * host answers with both scopes re-resolved and the panel repaints from that.
 */

import type { WireConfigValue } from "../wire/frames";

export type ConfigScope = "global" | "project";
export type ConfigOrigin = "set_here" | "global" | "default";
export type ConfigValue = WireConfigValue;
export type ConfigFieldKind = "bool" | "choice" | "text";

/** One curated setting as the host resolved it for one scope. */
export interface ConfigRow {
  /** The host's real TOML path (`notifications.on_finish`). */
  readonly key: string;
  readonly label: string;
  readonly kind: ConfigFieldKind;
  readonly value: ConfigValue;
  /** For `kind: "choice"`, the host's own cycle order. Empty otherwise. */
  readonly choices: readonly string[];
  readonly origin: ConfigOrigin;
  /** What this row would read if this scope's override were cleared. */
  readonly inherited: ConfigValue;
  readonly inheritedOrigin: ConfigOrigin;
}

/** The whole of the host's answer: both scopes, both file paths, one project. */
export interface ConfigDoc {
  readonly projectName: string;
  /** `null` when the host has no home directory to keep a global base in. */
  readonly globalPath: string | null;
  readonly projectPath: string;
  readonly rows: Readonly<Record<ConfigScope, readonly ConfigRow[]>>;
}

/**
 * A staged, unsaved edit. `set` carries the value to write; `clear` removes the
 * override in its scope so the value re-inherits (SPECS §8's `c`).
 *
 * `clear` is a variant rather than `value: null` because `false` is a perfectly
 * legal explicit value for a toggle — the wire makes the same distinction, and
 * for the same reason.
 */
export type ConfigEdit =
  | { readonly kind: "set"; readonly value: ConfigValue }
  | { readonly kind: "clear" };

/** Staged edits, per scope, keyed by the host's `ConfigRow.key`. */
export type ConfigEdits = Readonly<
  Record<ConfigScope, Readonly<Record<string, ConfigEdit>>>
>;

export const NO_CONFIG_EDITS: ConfigEdits = { global: {}, project: {} };

export interface ResolvedConfigRow {
  readonly row: ConfigRow;
  /** The value after any staged edit — the host's, or the one `s` will save. */
  readonly value: ConfigValue;
  readonly origin: ConfigOrigin;
  /** True when a staged edit is what produced `value`, so 1f can mark the row
   * as changed rather than leaving "Unsaved changes" to speak for all of it. */
  readonly staged: boolean;
  /** D5's routable-bind caution, or `null`. */
  readonly warning: string | null;
}

const LOOPBACK_HOSTS: ReadonlySet<string> = new Set([
  "127.0.0.1",
  "::1",
  "localhost",
]);

/** D5 / SPECS §8: loopback is the safe default; anything else is a routable
 * address someone typed on purpose. */
export function isLoopbackAddress(value: string): boolean {
  return LOOPBACK_HOSTS.has(value.trim().toLowerCase());
}

/**
 * The host's key for the bind address. Matched by name so the caution below
 * attaches to the row the host really sent — if a later build drops the field,
 * the note disappears with it rather than hanging on a row that is gone.
 */
export const WEB_BIND_KEY = "web.bind";

export const ROUTABLE_BIND_WARNING =
  "routable — reachable from other devices on this network, not just this machine (D5)";

/**
 * The one place a row's displayed value and origin tag are decided, given the
 * edits staged so far. Pure: no clock, no DOM, no network — and no layer walk,
 * because both answers it can give came from the host (see the module doc).
 */
export function resolveConfigRow(
  row: ConfigRow,
  scope: ConfigScope,
  edits: ConfigEdits,
): ResolvedConfigRow {
  const edit = edits[scope][row.key];
  let value = row.value;
  let origin = row.origin;
  if (edit?.kind === "set") {
    value = edit.value;
    origin = "set_here";
  } else if (edit?.kind === "clear") {
    value = row.inherited;
    origin = row.inheritedOrigin;
  }

  const warning =
    row.key === WEB_BIND_KEY &&
    typeof value === "string" &&
    value !== "" &&
    !isLoopbackAddress(value)
      ? ROUTABLE_BIND_WARNING
      : null;

  return { row, value, origin, staged: edit !== undefined, warning };
}

/**
 * What `Space` sets a row to: a toggle flips, a choice advances through the
 * host's own options, and a text field is not set by `Space` at all — it opens
 * the inline editor, which is what the desktop's `toggle_selected` does too.
 *
 * `null` therefore means "this key does not set a value here". The host
 * validates whatever does come back against the same field table, so a browser
 * that got this wrong would be refused rather than obeyed.
 */
export function nextConfigValue(
  resolved: ResolvedConfigRow,
): ConfigValue | null {
  const { row, value } = resolved;
  if (row.kind === "bool") {
    return value !== true;
  }
  if (row.kind === "choice") {
    if (row.choices.length === 0) {
      return null;
    }
    const at = row.choices.indexOf(String(value));
    return row.choices[(at + 1) % row.choices.length] ?? null;
  }
  return null;
}

/** The value an inline editor opens seeded with — the effective one, exactly
 * as `toggle_selected` seeds the desktop's. */
export function editableText(resolved: ResolvedConfigRow): string {
  return typeof resolved.value === "string" ? resolved.value : "";
}

/**
 * The command name the manager sends, kept in one place so a rename on either
 * side of the wire is a one-line change.
 *
 * **One name for open and for save.** The answer to a save *is* the manager —
 * the host applies the staged edits, writes them and replies with the
 * re-resolved layering — so a browser that saved never has to ask again, and
 * never paints a stale picture in between (§6.5 R22).
 */
export const OPEN_CONFIGURATION_COMMAND = "open_configuration";

/** One staged edit as it travels (`protocol::ConfigChange`). `value` absent is
 * a clear. */
export interface ConfigChange {
  readonly scope: ConfigScope;
  readonly key: string;
  readonly value?: ConfigValue;
}

/** The `args` of a saving `open_configuration` frame. Empty `changes` is a
 * plain read, and `AppOptions.onOpenConfig` sends none at all for one. */
export interface ConfigSaveRequest {
  readonly changes: readonly ConfigChange[];
}

/**
 * The staged edits as one request, in a stable order — global scope first, then
 * project, each in the host's own row order — so two runs of the same panel
 * produce the same frame and a test can read it.
 */
export function stagedChanges(
  doc: ConfigDoc,
  edits: ConfigEdits,
): readonly ConfigChange[] {
  const changes: ConfigChange[] = [];
  for (const scope of ["global", "project"] as const) {
    for (const row of doc.rows[scope]) {
      const edit = edits[scope][row.key];
      if (edit === undefined) {
        continue;
      }
      changes.push(
        edit.kind === "clear"
          ? { scope, key: row.key }
          : { scope, key: row.key, value: edit.value },
      );
    }
  }
  return changes;
}

/** How many edits are staged across both scopes — 1f's "Unsaved changes". */
export function stagedCount(edits: ConfigEdits): number {
  return Object.keys(edits.global).length + Object.keys(edits.project).length;
}
