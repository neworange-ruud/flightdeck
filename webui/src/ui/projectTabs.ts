import { projectGlyph, projectTone } from "../state/status";
import type { Project } from "../state/model";
import type { AppState } from "../state/types";
import { clear, el, hostOnlyBadge, separator, spinnerGlyph } from "./dom";
import type { Child, Region } from "./dom";
import { toneClass } from "./tone";
import type { Store } from "./store";

/**
 * Region 2 of 7 — the project tab row with status dots (1a).
 *
 * Two things here are graded and easy to get wrong:
 *
 * **D3, shared selection.** Clicking a project tab does not switch a
 * browser-local view; it moves the selection for the whole instance, desktop
 * included. Every tab therefore says so in its tooltip, and the click goes
 * through the reducer (`selection/project`) so the store's `onDispatch` seam
 * can turn it into a `ClientMsg::Command` the moment `remote-control-hgqy`
 * wires the socket. Nothing about this row implies "my tab, my view".
 *
 * **D16, host-only actions.** `+ project` needs the host's native directory
 * picker, so it carries a `host only` badge instead of quietly not working or
 * being hidden. The per-tab `✕` is a different case: closing a project is a
 * destructive operation, which D8 puts outside M1 — so it renders, disabled,
 * and says which milestone owns it. It is not badged `host only`, because it
 * is not a desktop-only action; it is an unbuilt one.
 */
export function createProjectTabs(store: Store): Region {
  const row = el("div", {
    class: "fd-projects",
    attrs: { role: "tablist", "aria-label": "Projects" },
  });

  function render(state: AppState): void {
    clear(row);
    const selectedId = state.selection?.projectId ?? null;
    const children: Child[] = [];

    state.projects.forEach((project, index) => {
      if (index > 0) {
        children.push(separator());
      }
      children.push(projectTab(project, project.id === selectedId, store));
    });

    children.push(el("div", { class: "fd-spacer" }));
    children.push(newProjectAction());
    for (const child of children) {
      if (child !== null && child !== undefined && child !== false) {
        row.append(child);
      }
    }
  }

  return { el: row, update: render };
}

function projectTab(
  project: Project,
  selected: boolean,
  store: Store,
): HTMLElement {
  const glyph = projectGlyph(project);
  const tone = toneClass(projectTone(project));

  const select = el(
    "button",
    {
      class: "fd-project__select",
      title: "select this project — this also moves the desktop's selection",
      attrs: {
        type: "button",
        role: "tab",
        "aria-selected": String(selected),
      },
    },
    [
      glyph === "spinner"
        ? spinnerGlyph(tone)
        : el("span", {
            class: `fd-glyph ${tone}`,
            text: glyph === "hollow" ? "○" : "●",
            attrs: { "aria-hidden": "true" },
          }),
      el("span", { text: project.name }),
    ],
  );
  select.addEventListener("click", () => {
    store.dispatch({ type: "selection/project", projectId: project.id });
  });

  const close = el("button", {
    class: "fd-project__close",
    text: "✕",
    title: "closing a project is a destructive operation — M2 (D8)",
    attrs: {
      type: "button",
      disabled: "",
      "aria-label": `Close project ${project.name}`,
    },
  });

  return el(
    "div",
    {
      class: "fd-project",
      attrs: { role: "presentation", "data-selected": String(selected) },
    },
    [select, close],
  );
}

function newProjectAction(): HTMLElement {
  return el(
    "button",
    {
      class: "fd-action fd-action--project",
      title:
        "adding a project opens a directory picker on the machine running FlightDeck",
      attrs: { type: "button", disabled: "", "aria-disabled": "true" },
    },
    [el("span", { text: "+ project" }), hostOnlyBadge()],
  );
}
