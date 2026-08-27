import type { StatusTone } from "../state/model";

/**
 * Tone name -> class name. The indirection exists so that no component ever
 * names a colour: a component asks for `toneClass(statusTone(status))` and
 * `src/style/main.css` decides which `--fd-*` token that is. Renaming a token
 * touches one CSS rule, not thirty TypeScript files.
 */
export function toneClass(tone: StatusTone): string {
  return `fd-tone-${tone}`;
}
