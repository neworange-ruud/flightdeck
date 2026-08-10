# Responsive chrome collapse

## Problem

FlightDeck's chrome costs a fixed 8 rows and 28 columns regardless of window
size: header, project tab row, divider, child tab bar, info divider, git info
bar, status divider, status bar, plus the agents sidebar. In a small window the
hosted agent or shell is squeezed into what is left, and below roughly 108×32
the terminal drops under the classic 80×24 floor.

## Solution

When the window is small *and* the user is in Terminal mode, collapse the chrome
that is not needed while typing into a terminal: hide the project tab row, hide
the git info bar, and reduce the agents sidebar to a 3-column indicator strip.
Switching to App mode always restores the full chrome, so nothing becomes
unreachable — it becomes momentarily invisible.

The rule is recomputed from live values on every frame. A window resize is a new
`area`; a mode toggle is a new `mode`. There is no stored collapse flag and
therefore no invalidation to get wrong.

## The collapse rule

A pure function in `src/tui/layout.rs`:

```rust
pub enum Chrome { Full, Collapsed }

/// 8 chrome rows + 24 terminal rows.
pub const MIN_FULL_ROWS: u16 = 32;
/// 28 sidebar columns + 80 terminal columns.
pub const MIN_FULL_COLS: u16 = 108;

pub fn chrome_for(area: Rect, mode: InputMode) -> Chrome {
    if mode == InputMode::App {
        return Chrome::Full;
    }
    if area.height < MIN_FULL_ROWS || area.width < MIN_FULL_COLS {
        Chrome::Collapsed
    } else {
        Chrome::Full
    }
}
```

The thresholds are derived, not chosen: they are exactly the sizes at which full
chrome would push the terminal below 80×24. Collapsing engages precisely when
that floor is at risk.

## Layout

`compute` takes a second parameter: `compute(area: Rect, chrome: Chrome)`. It
returns the same `MainLayout` struct with the same ten fields in both cases.
Hidden elements get **zero-height rects** rather than being removed from the
struct. This keeps every existing call site compiling, and it makes hit-testing
correct with no extra code — a click can never land inside a zero-sized rect.

| Field | `Full` | `Collapsed` |
| --- | --- | --- |
| `header` | 1 row | 1 row |
| `project_tabs` | 1 row | **0 rows** |
| `divider` | 1 row | 1 row |
| `sidebar` | 28 cols | **3 cols** |
| `child_tabs` | 1 row | 1 row |
| `terminal` | fills | fills (+3 rows, +25 cols) |
| `info_divider` | 1 row | **0 rows** |
| `info_bar` | 1 row | **0 rows** |
| `status_divider` | 1 row | 1 row |
| `status_bar` | 1 row | 1 row |

The status bar stays: it carries the current mode, the app commands, and the
command palette hint — the things you need most when chrome is disappearing
around you. Git information stays reachable through the git status overlay.

New constants alongside the existing sidebar geometry:

```rust
pub const COLLAPSED_SIDEBAR_WIDTH: u16 = 3;  // glyph + padding + right border
```

## Collapsed sidebar

One row per agent, in tab order, starting at the sidebar's first row. No
"Agents" heading (there is no room for the word) and no `✕` close control.

Glyph selection, in priority order:

1. Selected tab → `▸`, yellow bold.
2. `TabPhase::Creating`, or interpreted status `Starting` / `Running` /
   `Working` → the braille spinner frame, red.
3. Otherwise → `●` in its status colour, exactly as `status_label_color` maps
   it: green for `Idle` / `Completed` / `Stopped` / `Recovered` / `Unknown`, red
   for `WaitingForInput` / `NeedsAttention` / `Failed` / `SessionLost`, and cyan
   when a manual status override is set.

These are the same glyphs and colours the expanded sidebar already draws on each
agent's name line. The collapsed strip is that indicator with the text removed,
so nothing new has to be learned.

The selected agent shows the arrow even when it is busy. Its terminal is the one
on screen, so its activity is visible directly.

`sidebar_hit` branches on chrome: collapsed uses one row per tab with no header
offset and no close column, so a click on row *i* selects agent *i*. Because
selecting from the sidebar also focuses the app, the layout expands immediately
after such a click.

While collapsed there is no mouse affordance for closing an agent. Switch to App
mode to close one.

## PTY sizing

`sync_terminal_sizes` already runs every frame for the active project and
resizes a terminal only when its VT grid actually differs. Its non-split path
reads `state.pty_size`, which today is refreshed only on `Event::Resize`.

Adding one line at the top of `sync_terminal_sizes`:

```rust
state.pty_size = viewport_pty_size(full, state.mode());
```

makes the agent PTY follow collapse automatically on a mode toggle, with no new
plumbing and no resize storm — `resize_if_changed` absorbs the frames where
nothing moved. Keeping `state.pty_size` updated also means newly spawned
terminals start at the right size.

`viewport_pty_size` gains a `mode` parameter. The `Event::Resize` arm passes
each project's own mode, so background projects — which never reach
`sync_terminal_sizes` — stay correct as well.

## Call sites

`layout::compute` is called from eight places. Each derives its chrome from the
same helper rather than deciding independently:

- `render::draw` — `chrome_for(area, state.mode())`; also skips drawing the
  project row, info divider, and info bar when collapsed, and branches the
  sidebar renderer.
- `render::hit_test` — same derivation; branches `sidebar_hit`.
- `lib.rs` render loop (project tab row), `handle_mouse` (project tab hit test),
  `terminal_at`, `viewport_for_target`, `sync_terminal_sizes` (split path),
  `viewport_pty_size`.

In `handle_mouse` and the render loop the project row is zero-height when
collapsed, so its hit test returns `None` and its draw is a no-op without any
special casing.

Split view is unaffected: `split_region` and `split_columns` derive from
`MainLayout` and simply receive a larger region.

## Testing

Written before the implementation.

**`layout::chrome_for`**

- Truth table across the boundaries: 31 vs 32 rows, 107 vs 108 columns, with
  both dimensions independently short.
- App mode returns `Full` even for a 20×10 area.

**`layout::compute`**

- Collapsed: `project_tabs`, `info_divider`, and `info_bar` all have height 0;
  `sidebar.width == COLLAPSED_SIDEBAR_WIDTH`.
- Collapsed vs full on the same area: terminal is exactly 3 rows taller and 25
  columns wider.
- Collapsed rects do not overlap and account for the total area.
- Degenerate areas (0×0, 1×1, 10×3) do not panic collapsed.
- Existing full-chrome tests re-run unchanged under `Chrome::Full`.

**Hit-testing**

- Row *i* of the collapsed strip resolves to `AgentTab(i)`.
- The far-right column of a collapsed strip row is `AgentTab`, never
  `CloseAgentTab`.
- The row where the project tab bar used to be resolves to nothing collapsed.
- A click past the collapsed strip's 3 columns is not a sidebar hit.

**Rendering** (ratatui `TestBackend` buffer assertions)

- Collapsed sidebar shows `▸` on the selected agent's row, a spinner glyph on a
  working agent's row, and `●` on an idle agent's row.
- No "Agents" heading, no project tab row, and no git info bar in the collapsed
  buffer.
- The full-chrome buffer still contains all three.
- Collapsed draw does not panic with zero tabs, with a dialog overlay, or in
  split view.

**PTY sizing**

- `viewport_pty_size(small, Terminal)` is strictly larger in both dimensions
  than `viewport_pty_size(small, App)`.
- On a large window the two are identical.
- Both remain at least 1×1 for degenerate sizes.

## Out of scope

- Making the thresholds configurable. The derived values follow from the chrome
  cost; if they need tuning later, that is a small follow-up.
- Any intermediate breakpoint between full and collapsed.
- Collapsing the header, the child tab bar, or the status bar.
