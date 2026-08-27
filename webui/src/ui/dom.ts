/**
 * Twelve lines of DOM helper, in place of a framework.
 *
 * D9 says "no framework beyond what the design needs", and what this design
 * needs is: build an element, give it a class, give it text. Every component in
 * `src/ui/` is a function returning a `Region` — an element plus an `update`
 * that re-reads state — which is enough structure for the whole main screen and
 * small enough that `remote-control-l7ya` can add screens without learning a
 * framework's rules first.
 *
 * Note what is *not* here: no `style` argument. Colour and size live in
 * `src/style/main.css` against the tokens in `tokens.css`, and a `style` escape
 * hatch is exactly how a hex literal gets into a component. The guard test in
 * `src/ui/tokens.guard.test.ts` fails the build if one does.
 */
import type { AppState } from "../state/types";

/** A live piece of UI: its root element, and how to re-render it from state. */
export interface Region<E extends HTMLElement = HTMLElement> {
  readonly el: E;
  update(state: AppState): void;
}

export interface ElOptions {
  readonly class?: string;
  readonly text?: string;
  readonly title?: string;
  readonly attrs?: Readonly<Record<string, string>>;
}

export type Child = Node | string | null | false | undefined;

export function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  options: ElOptions = {},
  children: readonly Child[] = [],
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (options.class !== undefined) {
    node.className = options.class;
  }
  if (options.text !== undefined) {
    node.textContent = options.text;
  }
  if (options.title !== undefined) {
    node.title = options.title;
  }
  for (const [name, value] of Object.entries(options.attrs ?? {})) {
    node.setAttribute(name, value);
  }
  append(node, children);
  return node;
}

export function append(parent: ParentNode, children: readonly Child[]): void {
  for (const child of children) {
    if (child === null || child === undefined || child === false) {
      continue;
    }
    parent.append(child);
  }
}

export function clear(node: Node): void {
  while (node.firstChild !== null) {
    node.firstChild.remove();
  }
}

/**
 * The `│` glyph 1a uses between status-bar and git-bar items.
 *
 * This is the canonical example of `--fd-text-decor` (2g): delete it and no
 * fact is lost, only the visual gap between two items. `aria-hidden` because a
 * screen reader gains nothing from hearing "vertical bar" six times.
 */
export function separator(): HTMLElement {
  return el("span", {
    class: "fd-sep",
    text: "│",
    attrs: { "aria-hidden": "true" },
  });
}

/**
 * D16: a desktop-only action stays visible and says where its effect lands,
 * rather than disappearing from the browser. 1d/1f render this badge; the main
 * screen's one instance is `+ project`, which needs a native directory picker
 * on the host.
 */
export function hostOnlyBadge(): HTMLElement {
  return el("span", { class: "fd-badge-host", text: "host only" });
}

/**
 * The pulsing glyph 1a draws as `{{ spinner }}`.
 *
 * The frame character is real text content (not a CSS `content:` animation) so
 * that a test — and a screen reader — can see it; the *motion* is a CSS opacity
 * pulse, matching the artboard's `animation: fdPulse`.
 */
export function spinnerGlyph(tone: string): HTMLElement {
  return el("span", {
    class: `fd-glyph fd-glyph--spinner ${tone}`,
    text: "⠿",
    attrs: { "aria-hidden": "true" },
  });
}
