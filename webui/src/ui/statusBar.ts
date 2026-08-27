import type { AppState, ConnectionStatus } from "../state/types";
import { clear, el, separator } from "./dom";
import type { Child, Region } from "./dom";

/**
 * Region 7 of 7 — the status bar (1a/1b/1c, bottom strip).
 *
 * `18ms` and `(this tab + desktop)` are both lifted off `--fd-text-decor`,
 * where 1a drew them, and onto `--fd-text-quiet`. Both are facts about whether
 * what you are looking at can be trusted: the round-trip time is the difference
 * between "live" and "live-ish", and the viewer breakdown is how you know the
 * other viewer is your own desktop and not a second person. 2g's rule decides
 * it — delete either and a fact is gone.
 */

export type ModeChipTone = "terminal" | "app" | "drained";

/**
 * §5.1: **losing control drains the mode chip.** Any state that costs the user
 * control renders `MODE: —`, because naming a mode while keystrokes are not
 * arriving is a lie. Pure, and exported, so the rule is a unit test rather
 * than a screenshot.
 */
export function modeChip(state: AppState): {
  readonly text: string;
  readonly tone: ModeChipTone;
} {
  if (state.connection !== "connected") {
    return { text: "MODE: —", tone: "drained" };
  }
  return state.mode === "terminal"
    ? { text: "MODE: TERMINAL", tone: "terminal" }
    : { text: "MODE: APP", tone: "app" };
}

/**
 * The connection strip's words. The full connection-state family (2c) belongs
 * to `remote-control-l7ya`; what is here is the vocabulary 1a shows plus the
 * two §5.1 phrases that must not be invented twice ("keystrokes are being
 * held", "input queues until the replay lands").
 */
export function connectionLabel(status: ConnectionStatus): {
  readonly text: string;
  readonly tone: string;
} {
  switch (status) {
    case "connected":
      return { text: "connected", tone: "fd-tone-ok" };
    case "connecting":
      return { text: "connecting", tone: "fd-tone-accent" };
    case "reconnecting":
      return {
        text: "reconnecting · keystrokes are being held",
        tone: "fd-tone-stale",
      };
    case "catching_up":
      return {
        text: "catching up · input queues until the replay lands",
        tone: "fd-tone-accent",
      };
    case "disconnected":
      return { text: "disconnected", tone: "fd-tone-alert" };
  }
}

export function createStatusBar(): Region {
  const bar = el("div", {
    class: "fd-statusbar",
    attrs: { role: "status", "aria-label": "Status" },
  });

  function render(state: AppState): void {
    clear(bar);
    const chip = modeChip(state);
    const connection = connectionLabel(state.connection);

    const parts: Child[] = [
      el("span", {
        class: "fd-mode",
        text: chip.text,
        attrs: { "data-tone": chip.tone },
      }),
    ];

    for (const hint of hintsFor(state)) {
      parts.push(separator(), hint);
    }

    parts.push(
      el("div", { class: "fd-spacer" }),
      el("span", { class: "fd-conn" }, [
        el("span", {
          class: `fd-glyph ${connection.tone}`,
          text: "●",
          attrs: { "aria-hidden": "true" },
        }),
        connection.text,
        state.latencyMs === null
          ? null
          : el("span", {
              class: "fd-conn__latency",
              text: `${state.latencyMs}ms`,
            }),
      ]),
      separator(),
      el("span", { class: "fd-viewers" }, [
        `${state.viewers} viewer${state.viewers === 1 ? "" : "s"}`,
        /** D3's cost, made visible: the other viewer is normally your own
         * desktop, looking at the same session you are. */
        state.viewers > 1
          ? el("span", {
              class: "fd-viewers__detail",
              text: "(this tab + desktop)",
            })
          : null,
      ]),
      state.update === null
        ? el("span", { class: "fd-statusbar__pad" })
        : el("span", { class: "fd-update" }, [
            el("span", { text: "●", attrs: { "aria-hidden": "true" } }),
            `${state.update.version} available`,
          ]),
    );

    for (const part of parts) {
      if (part !== null && part !== undefined && part !== false) {
        bar.append(part);
      }
    }
  }

  return { el: bar, update: render };
}

/**
 * The hint row, straight from the artboards: 1a in Terminal mode, 1b in App
 * mode, 1c in split. `Ctrl-g` is the only chord the app claims (§5), so it is
 * the only one that appears in every variant.
 */
function hintsFor(state: AppState): readonly HTMLElement[] {
  if (state.layout === "split") {
    return [
      hint("SPLIT", "3 terminals"),
      hint("←/→", "move focus"),
      hint("Ctrl-g", "palette → “split”"),
    ];
  }
  if (state.mode === "app") {
    return [
      hint("Enter", "focus terminal"),
      hint("Ctrl-g", "command palette"),
      hint("?", "help"),
    ];
  }
  return [
    hint("Esc Esc", "app commands"),
    hint("Ctrl-g", "command palette"),
    hint("click outside", "release keys"),
  ];
}

function hint(key: string, label: string): HTMLElement {
  return el("span", { class: "fd-hint" }, [
    el("span", { class: "fd-key", text: key }),
    ` ${label}`,
  ]);
}
