/**
 * Artboard `1f — CONFIGURATION MANAGER` (`specs/design/flightdeck-web-turn2.dc.html`
 * lines 1381-1433), and the layering rules in
 * `.agents/skills/flightdeck-config-conventions` / `specs/SPECS.md` §8.
 *
 * Pure module, no DOM, no store — the same split `state/commands.ts` uses for
 * the palette: this file owns the curated inventory and the origin-attribution
 * logic; `ui/configManager.ts` only ever renders what `resolveConfigRow` hands
 * it.
 *
 * ## Why the inventory is a curated constant, not derived from `AppState`
 *
 * There is no wire source for config today. `src/web/protocol.rs`'s
 * `Snapshot` carries projects/selection/activity/seats — nothing about
 * `config.toml` — and no `Command` exists yet that reads or writes it
 * (`remote-control-ll5.1` built host-side command dispatch concurrently with
 * this task). So `CONFIG_FIELDS` below is a curated constant: real field
 * names, real layering semantics, sample layered values chosen to reproduce
 * every origin tag 1f draws — not a live read. The palette no longer has a
 * curated list of its own — `remote-control-ll5.12` moved it onto
 * `Snapshot::commands` — and this one goes the same way once the host sends a
 * config inventory and accepts `save_config`/`SAVE_CONFIG_COMMAND`.
 *
 * ## Origin attribution
 *
 * `.agents/skills/flightdeck-config-conventions`: a per-project
 * `.flightdeck/config.toml` stores only overrides, and wins field-by-field
 * over the per-user global at `~/.flightdeck/config.toml`, which wins over the
 * shipped default. The global file is generated with every field *documented*
 * — not necessarily *live* — so a field can be absent from it (falls back to
 * the shipped default) or explicitly set (an intentional override). Three
 * facts, three tags:
 *
 *   - **`set_here`** — the active scope's own file has an explicit value for
 *     this field (Project scope: the project override; Global scope: the
 *     global file's own explicit value).
 *   - **`global`** — Project scope only: no project override, but the global
 *     file has an explicit value that is therefore what is actually in
 *     effect.
 *   - **`default`** — neither file has an explicit value; the shipped default
 *     is what is actually in effect.
 *
 * `resolveConfigRow` computes this from the three layers plus any local
 * (unsaved) edit — never from a value comparison, so a project override that
 * happens to equal the global value still reads `set_here`: it *is* set here,
 * regardless of what it is set to.
 */

export type ConfigScope = "global" | "project";
export type ConfigOrigin = "set_here" | "global" | "default";
export type ConfigValue = string | boolean;
export type ConfigFieldKind = "toggle" | "choice" | "text";

/**
 * The three layers SPECS §8 defines, as they apply to one field. `undefined`
 * means "this layer has no explicit value for this field" — never a stand-in
 * for `false`/`""`, which are legal explicit values.
 */
export interface ConfigLayers {
  readonly project?: ConfigValue;
  readonly global?: ConfigValue;
  readonly default: ConfigValue;
}

export interface ConfigField {
  readonly key: string;
  readonly label: string;
  readonly kind: ConfigFieldKind;
  /** For `kind: "choice"`, the legal values in cycle order. Metadata only
   * today — nothing in this build cycles a choice (see the ll5.6 report's
   * "could not do" section). */
  readonly choices?: readonly string[];
  readonly layers: ConfigLayers;
  /**
   * D16: `e edit in $EDITOR` and the 1f row it renders on
   * (`ui.use_f2_to_leave_terminal_focus`) — a desktop-only action/setting.
   * Rendered with a `host only` badge, never hidden. A host-only field has no
   * browser-editable value, so `resolveConfigRow` returns `value: null,
   * origin: null` for it rather than a fabricated tag.
   */
  readonly hostOnly?: boolean;
  /** D5: this field's resolved value is checked against loopback and warned
   * on when it is not — `web.bind` only, today. */
  readonly warnsIfRoutable?: boolean;
}

/** A staged, unsaved edit. `clear` is the only kind this build makes (`c`,
 * SPECS §8's "clears a project override so the value re-inherits") — no
 * `Space`-driven "set to a new value" edit exists yet, see the task report. */
export type ConfigEdit = { readonly kind: "clear" };

export interface ResolvedConfigRow {
  readonly field: ConfigField;
  /** `null` only for a host-only field — there is nothing this scope can show
   * as "the value", because the browser cannot set one. */
  readonly value: ConfigValue | null;
  readonly origin: ConfigOrigin | null;
  /** D5's routable-bind warning text, or `null` when the resolved value is
   * loopback (or the field does not carry this check at all). */
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

export const ROUTABLE_BIND_WARNING =
  "routable — reachable from other devices on this network, not just this machine (D5)";

/**
 * The one place a field's displayed value and origin tag are decided, for a
 * given scope and the edits staged so far. Pure: no clock, no DOM, no network.
 */
export function resolveConfigRow(
  field: ConfigField,
  scope: ConfigScope,
  edits: Readonly<Record<string, ConfigEdit>>,
): ResolvedConfigRow {
  if (field.hostOnly === true) {
    return { field, value: null, origin: null, warning: null };
  }

  const cleared = edits[field.key]?.kind === "clear";
  let value: ConfigValue;
  let origin: ConfigOrigin;

  if (scope === "project" && !cleared && field.layers.project !== undefined) {
    value = field.layers.project;
    origin = "set_here";
  } else if (field.layers.global !== undefined) {
    /** Global scope's own explicit value *is* "set here" from Global scope's
     * point of view; the same fact reads as "global" once Project scope's
     * override precedence puts it one layer down. */
    value = field.layers.global;
    origin = scope === "project" ? "global" : "set_here";
  } else {
    value = field.layers.default;
    origin = "default";
  }

  const warning =
    field.warnsIfRoutable === true &&
    typeof value === "string" &&
    value !== "" &&
    !isLoopbackAddress(value)
      ? ROUTABLE_BIND_WARNING
      : null;

  return { field, value, origin, warning };
}

/** Cursor-navigable fields (`↑↓`) — the host-only row is informational, never
 * a stop for the cursor `c`/`Space` could act on. */
export function selectableConfigFields(): readonly ConfigField[] {
  return CONFIG_FIELDS.filter((field) => field.hostOnly !== true);
}

/**
 * The command name `s` sends, kept in exactly one place so a future rename on
 * either side of the wire is a one-line change. **Placeholder**: the web protocol
 * defines no such command yet — see the ll5.6 task report for the shape this
 * build needs `remote-control-ll5.1` to add. Sending it today will get
 * whatever an unrecognised command name draws from the host (most likely
 * `rejected`/`ignored`, never a fabricated `applied`), which is exactly the
 * "never optimism" requirement: the UI reports what actually came back.
 */
export const SAVE_CONFIG_COMMAND = "save_config";

export interface ConfigChange {
  readonly key: string;
  readonly action: "clear";
}

/** The payload `AppOptions.onSaveConfig` (see `ui/app.ts`) hands the socket —
 * `{ name: SAVE_CONFIG_COMMAND, args: ConfigSaveRequest }` once wired. */
export interface ConfigSaveRequest {
  readonly scope: ConfigScope;
  readonly changes: readonly ConfigChange[];
}

/**
 * Artboard 1f's rows, in the order it draws them, plus the four `[web]`
 * fields `remote-control-mz40` is adding beside the existing "FlightDeck
 * Remote" row (per the ll5.6 task brief and SPECS §8's `[web]` subsection) —
 * 1f itself predates that section existing, so it draws only the master
 * "FlightDeck Remote" toggle.
 *
 * Sample layered values are chosen to reproduce every origin tag 1f shows,
 * and — for `notifications.on_finished` — to also carry a `global` value
 * hidden behind its `project` override, so clearing that one override
 * demonstrates `set_here` shadowing `global` rather than falling straight to
 * `default` (the fourth combination the task's test suite asks for).
 */
export const CONFIG_FIELDS: readonly ConfigField[] = [
  {
    key: "notifications.enabled",
    label: "OS notifications",
    kind: "toggle",
    layers: { default: false, global: true },
  },
  {
    key: "notifications.sound",
    label: "Notification sounds",
    kind: "toggle",
    layers: { default: false },
  },
  {
    key: "notifications.on_finished",
    label: "Notify when finished",
    kind: "toggle",
    /** `global: true` is shadowed by `project: true` today — see the module
     * doc: clearing this one override is the shadowing fixture. */
    layers: { default: false, global: true, project: true },
  },
  {
    key: "notifications.on_waiting",
    label: "Notify when waiting",
    kind: "toggle",
    layers: { default: false, project: true },
  },
  {
    key: "notifications.on_failed",
    label: "Notify when failed",
    kind: "toggle",
    layers: { default: false, global: true },
  },
  {
    key: "updates.check_for_updates",
    label: "Check for updates",
    kind: "toggle",
    layers: { default: true },
  },
  {
    key: "ui.agent_tab_position",
    label: "Agent tab position",
    kind: "choice",
    choices: ["left", "right"],
    layers: { default: "left", project: "right" },
  },
  {
    key: "ui.default_agent",
    label: "Default agent",
    kind: "choice",
    choices: ["OpenCode", "Claude Code", "Codex CLI"],
    layers: { default: "OpenCode", global: "Claude Code" },
  },
  {
    key: "ui.terminal_mode_color",
    label: "Terminal mode color",
    kind: "choice",
    choices: ["green", "cyan", "blue", "magenta", "yellow", "red", "white"],
    layers: { default: "green" },
  },
  {
    key: "ui.app_mode_color",
    label: "App mode color",
    kind: "choice",
    choices: ["green", "cyan", "blue", "magenta", "yellow", "red", "white"],
    layers: { default: "cyan", project: "magenta" },
  },
  {
    key: "ui.mode_border",
    label: "Mode border",
    kind: "choice",
    choices: ["off", "dim", "normal", "bright"],
    layers: { default: "off", global: "bright" },
  },
  {
    key: "ui.dim_terminal_in_app_mode",
    label: "Dim terminal in app mode",
    kind: "toggle",
    layers: { default: true },
  },
  {
    key: "remote.enabled",
    label: "FlightDeck Remote (phone + browser link)",
    kind: "toggle",
    layers: { default: false, project: true },
  },
  /* --- `[web]` (SPECS §8), beside the FlightDeck Remote row above -------- */
  {
    key: "web.enabled",
    label: "Web interface (browser control)",
    kind: "toggle",
    layers: { default: false },
  },
  {
    key: "web.port",
    label: "Web interface port",
    kind: "text",
    layers: { default: "7420" },
  },
  {
    key: "web.bind",
    label: "Web interface bind address",
    kind: "text",
    warnsIfRoutable: true,
    layers: { default: "127.0.0.1", project: "0.0.0.0" },
  },
  {
    key: "web.replay_bytes",
    label: "Web interface replay buffer (bytes)",
    kind: "text",
    layers: { default: "262144" },
  },
  {
    key: "remote.relay_url",
    label: "Relay URL",
    kind: "text",
    layers: { default: "wss://relay.flightdeck.dev" },
  },
  {
    key: "ui.use_f2_to_leave_terminal_focus",
    label: "Use F2 to leave terminal focus · web: Esc Esc or click outside",
    kind: "toggle",
    hostOnly: true,
    layers: { default: true },
  },
];
