import { el } from "./dom";
import type { Child } from "./dom";

/**
 * The pieces both tabs of the Transfer files panel are built from
 * (`ui/uploadPanel.ts`, `ui/downloadView.ts`), so Upload and Download look
 * like one panel: the picker's section and button vocabulary.
 */

/** A titled section: `FILES`, `FOLDER`, `DONE`. */
export function section(title: string, children: Child[]): HTMLElement {
  return el("section", { class: "fd-picker__section" }, [
    el("div", { class: "fd-picker__section-head" }, [
      el("span", { class: "fd-picker__section-title", text: title }),
    ]),
    ...children,
  ]);
}

/**
 * A button that says what it does with `data-action`. The primary one is
 * filled and carries `data-primary`, where focus lands when a tab opens; the
 * rest are outlined.
 */
export function button(
  label: string,
  action: string,
  onClick: () => void,
  disabled = false,
  primary = false,
): HTMLButtonElement {
  const attrs: Record<string, string> = { type: "button", "data-action": action };
  if (disabled) {
    attrs.disabled = "";
  }
  if (primary) {
    attrs["data-primary"] = "true";
  }
  const node = el("button", {
    class: primary ? "fd-picker__button" : "fd-picker__button fd-upload__secondary",
    text: label,
    attrs,
  });
  node.addEventListener("click", onClick);
  return node;
}
