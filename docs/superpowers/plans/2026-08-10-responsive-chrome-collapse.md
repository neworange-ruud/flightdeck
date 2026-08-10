# Responsive Chrome Collapse Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** In a small window, hide the project tab row and git info bar and shrink the agents sidebar to a 3-column indicator strip while the user is in Terminal mode, restoring full chrome in App mode.

**Architecture:** `src/tui/layout.rs` gains a `Chrome { Full, Collapsed }` enum and a pure `chrome_for(area, mode)` rule; `compute` takes the chrome as a second parameter and returns the same `MainLayout` struct with zero-height rects for hidden elements. Every consumer derives its chrome from the same helper on every frame, so a window resize (new `area`) and a mode toggle (new `mode`) both re-evaluate the rule with no stored state and no invalidation. PTY sizing follows for free because `sync_terminal_sizes` already runs every frame and resizes only on an actual change.

**Tech Stack:** Rust, ratatui (layout + `TestBackend` for buffer assertions), `cargo test`.

**Spec:** `docs/superpowers/specs/2026-08-10-responsive-chrome-collapse-design.md`

## Global Constraints

- CI runs `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, and `cargo test --locked`. All three must pass. Run `cargo fmt --all` before every commit.
- Thresholds are exact: `MIN_FULL_ROWS = 32`, `MIN_FULL_COLS = 108`, `COLLAPSED_SIDEBAR_WIDTH = 3`.
- Collapse applies **only** in `InputMode::Terminal`. `InputMode::App` is always `Chrome::Full`, regardless of window size.
- Collapsed hides exactly three things: `project_tabs`, `info_divider`, `info_bar`. The header, child tab bar, status divider, and status bar always stay.
- Hidden elements become **zero-sized rects**, never removed fields on `MainLayout`.
- `CHANGELOG.md` must be updated before the PR (project rule in `CLAUDE.md`). Task 5 does this.
- Doc comments in this codebase are full sentences ending in a period. Match the surrounding style.

---

### Task 1: Chrome enum, collapse rule, and chrome-aware layout

Introduces the whole geometry change behind an explicit parameter. Every existing call site passes `Chrome::Full`, so observable behaviour is unchanged after this task — only the new unit tests exercise `Collapsed`.

**Files:**
- Modify: `src/tui/layout.rs` (constants, `Chrome`, `chrome_for`, `compute`, tests)
- Modify: `src/tui/render.rs:297`, `src/tui/render.rs:506`, `src/tui/render.rs:2612` (pass `layout::Chrome::Full`)
- Modify: `src/lib.rs:1255`, `src/lib.rs:1462`, `src/lib.rs:1511`, `src/lib.rs:1768`, `src/lib.rs:1788`, `src/lib.rs:3570` (pass `crate::tui::layout::Chrome::Full`)

**Interfaces:**
- Produces:
  - `pub enum layout::Chrome { Full, Collapsed }` — derives `Debug, Clone, Copy, PartialEq, Eq`
  - `pub fn layout::chrome_for(area: Rect, mode: InputMode) -> Chrome`
  - `pub fn layout::compute(area: Rect, chrome: Chrome) -> MainLayout` (signature change)
  - `pub const layout::COLLAPSED_SIDEBAR_WIDTH: u16 = 3`
  - `pub const layout::MIN_FULL_ROWS: u16 = 32`
  - `pub const layout::MIN_FULL_COLS: u16 = 108`

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block at the bottom of `src/tui/layout.rs`, after the existing `minimum_area_does_not_panic` test:

```rust
    #[test]
    fn chrome_collapses_only_in_terminal_mode_below_the_thresholds() {
        let at_threshold = Rect::new(0, 0, MIN_FULL_COLS, MIN_FULL_ROWS);
        let one_row_short = Rect::new(0, 0, MIN_FULL_COLS, MIN_FULL_ROWS - 1);
        let one_col_narrow = Rect::new(0, 0, MIN_FULL_COLS - 1, MIN_FULL_ROWS);

        // App mode never collapses, however small the window.
        assert_eq!(
            chrome_for(Rect::new(0, 0, 20, 10), InputMode::App),
            Chrome::Full
        );
        // Exactly at the thresholds is still full chrome.
        assert_eq!(chrome_for(at_threshold, InputMode::Terminal), Chrome::Full);
        // One row short, or one column narrow, collapses.
        assert_eq!(
            chrome_for(one_row_short, InputMode::Terminal),
            Chrome::Collapsed
        );
        assert_eq!(
            chrome_for(one_col_narrow, InputMode::Terminal),
            Chrome::Collapsed
        );
    }

    #[test]
    fn collapsed_hides_project_row_and_info_bar_and_narrows_the_sidebar() {
        let l = compute(full_terminal(), Chrome::Collapsed);
        assert_eq!(l.project_tabs.height, 0);
        assert_eq!(l.info_divider.height, 0);
        assert_eq!(l.info_bar.height, 0);
        assert_eq!(l.sidebar.width, COLLAPSED_SIDEBAR_WIDTH);
        // Everything else keeps its full-chrome height.
        assert_eq!(l.header.height, HEADER_HEIGHT);
        assert_eq!(l.divider.height, DIVIDER_HEIGHT);
        assert_eq!(l.child_tabs.height, CHILD_TAB_BAR_HEIGHT);
        assert_eq!(l.status_divider.height, STATUS_DIVIDER_HEIGHT);
        assert_eq!(l.status_bar.height, STATUS_BAR_HEIGHT);
    }

    #[test]
    fn collapsing_grows_the_terminal_by_exactly_the_reclaimed_chrome() {
        let area = full_terminal();
        let full = compute(area, Chrome::Full);
        let collapsed = compute(area, Chrome::Collapsed);
        // Three rows back: project tabs, info divider, info bar.
        assert_eq!(collapsed.terminal.height, full.terminal.height + 3);
        assert_eq!(
            collapsed.terminal.width,
            full.terminal.width + (SIDEBAR_WIDTH - COLLAPSED_SIDEBAR_WIDTH)
        );
    }

    #[test]
    fn collapsed_rects_do_not_overlap_and_account_for_the_area() {
        let area = full_terminal();
        let l = compute(area, Chrome::Collapsed);
        assert!(l.sidebar.right() <= l.terminal.left());
        assert!(l.child_tabs.bottom() <= l.terminal.top());
        assert!(l.terminal.bottom() <= l.status_divider.top());
        assert!(l.status_divider.bottom() <= l.status_bar.top());
        assert_eq!(l.sidebar.width + l.child_tabs.width, area.width);
        assert_eq!(
            HEADER_HEIGHT
                + DIVIDER_HEIGHT
                + l.child_tabs.height
                + l.terminal.height
                + l.status_divider.height
                + l.status_bar.height,
            area.height
        );
    }

    #[test]
    fn collapsed_minimum_area_does_not_panic() {
        let _ = compute(Rect::new(0, 0, 0, 0), Chrome::Collapsed);
        let _ = compute(Rect::new(0, 0, 1, 1), Chrome::Collapsed);
        let _ = compute(Rect::new(0, 0, 10, 3), Chrome::Collapsed);
    }
```

Add the `InputMode` import to the test module — change the existing test-module preamble from:

```rust
    use super::*;
    use ratatui::layout::Rect;
```

to:

```rust
    use super::*;
    use crate::app::modes::InputMode;
    use ratatui::layout::Rect;
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib tui::layout`
Expected: compile error — `cannot find type Chrome in this scope` / `cannot find function chrome_for`.

- [ ] **Step 3: Add the constants, the enum, and the rule**

In `src/tui/layout.rs`, change the import line at the top from:

```rust
use ratatui::layout::{Constraint, Layout, Rect};
```

to:

```rust
use ratatui::layout::{Constraint, Layout, Rect};

use crate::app::modes::InputMode;
```

Then add, directly after the existing `pub const STATUS_DIVIDER_HEIGHT` / `INFO_DIVIDER_HEIGHT` constant block (before the `MainLayout` doc comment):

```rust
/// Width of the collapsed agent strip in columns: one glyph column, one
/// padding column, and the right border.
pub const COLLAPSED_SIDEBAR_WIDTH: u16 = 3;

/// Minimum rows for full chrome: the 8 chrome rows plus a 24-row terminal.
pub const MIN_FULL_ROWS: u16 = 32;

/// Minimum columns for full chrome: the 28-column sidebar plus an 80-column
/// terminal.
pub const MIN_FULL_COLS: u16 = 108;

/// How much chrome the main layout draws.
///
/// [`Chrome::Collapsed`] hides the project tab row and the git info bar and
/// shrinks the agents sidebar to [`COLLAPSED_SIDEBAR_WIDTH`], handing those
/// rows and columns to the terminal viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chrome {
    /// Every bar and the full-width sidebar.
    Full,
    /// Project row and git info bar hidden, sidebar reduced to a strip.
    Collapsed,
}

/// Decide how much chrome to draw for `area` in `mode`.
///
/// App mode always keeps full chrome — that is how a user gets the hidden bars
/// back. In terminal mode the chrome collapses once the window can no longer
/// afford an 80×24 terminal alongside it. Recomputed every frame from live
/// values, so a resize and a mode toggle both re-evaluate it with no stored
/// state to invalidate.
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

- [ ] **Step 4: Make `compute` chrome-aware**

Replace the whole body of `compute` in `src/tui/layout.rs` (currently lines 95–134) with:

```rust
pub fn compute(area: Rect, chrome: Chrome) -> MainLayout {
    let collapsed = chrome == Chrome::Collapsed;
    // Hidden bars keep their field on `MainLayout` but shrink to zero, so every
    // caller keeps compiling and `rect_contains` can never match them.
    let project_tabs_h = if collapsed {
        0
    } else {
        PROJECT_TAB_BAR_HEIGHT
    };
    let info_divider_h = if collapsed { 0 } else { INFO_DIVIDER_HEIGHT };
    let info_bar_h = if collapsed { 0 } else { INFO_BAR_HEIGHT };
    let sidebar_w = if collapsed {
        COLLAPSED_SIDEBAR_WIDTH
    } else {
        SIDEBAR_WIDTH
    };

    // Full-width top band: header (logo) | project tabs | divider | body.
    let [header, project_tabs, divider, body] = Layout::vertical([
        Constraint::Length(HEADER_HEIGHT),
        Constraint::Length(project_tabs_h),
        Constraint::Length(DIVIDER_HEIGHT),
        Constraint::Fill(1),
    ])
    .areas(area);

    // Split the body horizontally: sidebar | main pane.
    let [sidebar, main] =
        Layout::horizontal([Constraint::Length(sidebar_w), Constraint::Fill(1)]).areas(body);

    // Split main pane vertically: child_tabs | terminal | info_divider
    // | info_bar | status_divider | status_bar.
    let [child_tabs, terminal, info_divider, info_bar, status_divider, status_bar] =
        Layout::vertical([
            Constraint::Length(CHILD_TAB_BAR_HEIGHT),
            Constraint::Fill(1),
            Constraint::Length(info_divider_h),
            Constraint::Length(info_bar_h),
            Constraint::Length(STATUS_DIVIDER_HEIGHT),
            Constraint::Length(STATUS_BAR_HEIGHT),
        ])
        .areas(main);

    MainLayout {
        header,
        project_tabs,
        divider,
        sidebar,
        child_tabs,
        terminal,
        info_divider,
        info_bar,
        status_divider,
        status_bar,
    }
}
```

Also update `compute`'s doc comment: after the existing first paragraph, add a line reading:

```rust
/// `chrome` selects the full layout or the collapsed one (see [`chrome_for`]).
```

- [ ] **Step 5: Update every existing call site to `Chrome::Full`**

Behaviour must not change yet. In `src/tui/layout.rs`'s own test module, every existing `compute(...)` call takes a second argument — replace:

- `compute(area)` → `compute(area, Chrome::Full)`
- `compute(full_terminal())` → `compute(full_terminal(), Chrome::Full)`
- `compute(Rect::new(0, 0, 0, 0))` → `compute(Rect::new(0, 0, 0, 0), Chrome::Full)` (and the `1, 1` / `10, 3` variants)

In `src/tui/render.rs`:
- line 297 (in `hit_test`): `let ml = layout::compute(area);` → `let ml = layout::compute(area, layout::Chrome::Full);`
- line 506 (in `draw`): `let ml = layout::compute(area);` → `let ml = layout::compute(area, layout::Chrome::Full);`
- line 2612 (in a test): `layout::split_region(&layout::compute(area))` → `layout::split_region(&layout::compute(area, layout::Chrome::Full))`

In `src/lib.rs`, each of lines 1255, 1462, 1511, 1768, 1788, 3570 reads `crate::tui::layout::compute(<expr>)`. Add `, crate::tui::layout::Chrome::Full` as a second argument to each.

- [ ] **Step 6: Run the tests**

Run: `cargo test --lib tui::layout`
Expected: PASS, including the five new tests.

Run: `cargo test --locked`
Expected: PASS — no existing test changes behaviour.

- [ ] **Step 7: Lint, format, and commit**

```bash
cargo fmt --all
cargo clippy --all-targets --locked -- -D warnings
git add src/tui/layout.rs src/tui/render.rs src/lib.rs
git commit -m "feat(layout): add Chrome enum and chrome-aware compute"
```

---

### Task 2: Collapsed sidebar rendering and hit-testing

Builds the collapsed strip and its click mapping. Nothing passes `Chrome::Collapsed` from production code yet, so this task is exercised entirely by its own tests calling `draw_sidebar` and `sidebar_hit` directly.

**Files:**
- Modify: `src/tui/render.rs` — `draw_sidebar` (line 752), new `draw_sidebar_collapsed` and `collapsed_agent_span`, `sidebar_hit` (line 358), the two callers of each
- Test: `src/tui/render.rs` `mod tests`

**Interfaces:**
- Consumes: `layout::Chrome`, `layout::COLLAPSED_SIDEBAR_WIDTH` (Task 1)
- Produces:
  - `pub fn draw_sidebar(frame: &mut Frame, state: &AppState, cache: &GitStatusCache, area: Rect, chrome: layout::Chrome, now_ms: u64)` — signature change, `chrome` inserted before `now_ms`
  - `fn sidebar_hit(area: Rect, tab_count: usize, chrome: layout::Chrome, col: u16, row: u16) -> Option<HitTarget>` — signature change, `chrome` inserted before `col`

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/tui/render.rs`, after `hit_test_empty_sidebar_resolves_to_chrome`:

```rust
    // --- Collapsed chrome (small windows in terminal mode) -----------------

    /// Read the glyph in the first column of `row` from a rendered buffer.
    fn strip_glyph(buffer: &ratatui::buffer::Buffer, row: u16) -> String {
        buffer[(0, row)].symbol().to_string()
    }

    #[test]
    fn collapsed_sidebar_draws_one_glyph_per_agent() {
        let mut state = state_with_tabs(3);
        state.selected_tab = Some(1);
        // Tab 2 is mid-creation, so it must show a spinner rather than a dot.
        state.tabs[2].phase = TabPhase::Creating;

        let mut term = test_terminal(layout::COLLAPSED_SIDEBAR_WIDTH, 6);
        term.draw(|frame| {
            draw_sidebar(
                frame,
                &state,
                &empty_cache(),
                Rect::new(0, 0, layout::COLLAPSED_SIDEBAR_WIDTH, 6),
                layout::Chrome::Collapsed,
                0,
            )
        })
        .unwrap();
        let buffer = term.backend().buffer().clone();

        // Row 0 is tab 0 (idle → dot), row 1 the selection arrow, row 2 a spinner.
        assert_eq!(strip_glyph(&buffer, 0), "●");
        assert_eq!(strip_glyph(&buffer, 1), "▸");
        assert_ne!(
            strip_glyph(&buffer, 2),
            "●",
            "a tab being created shows a spinner, not a status dot"
        );
        // No heading: there is no room for the word "Agents".
        let all: String = (0..6_u16)
            .flat_map(|y| (0..layout::COLLAPSED_SIDEBAR_WIDTH).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol().to_string())
            .collect();
        assert!(!all.contains("Agents"));
    }

    #[test]
    fn collapsed_sidebar_hit_maps_one_row_per_agent_and_never_closes() {
        let area = Rect::new(0, 0, layout::COLLAPSED_SIDEBAR_WIDTH, 6);
        // One row per tab, starting at the sidebar's first row — no heading offset.
        assert_eq!(
            sidebar_hit(area, 3, layout::Chrome::Collapsed, 0, 0),
            Some(HitTarget::AgentTab(0))
        );
        assert_eq!(
            sidebar_hit(area, 3, layout::Chrome::Collapsed, 0, 2),
            Some(HitTarget::AgentTab(2))
        );
        // Past the last agent resolves to nothing (the caller falls back to chrome).
        assert_eq!(sidebar_hit(area, 3, layout::Chrome::Collapsed, 0, 3), None);
        // The rightmost inner column selects; it is never a close control.
        assert_eq!(
            sidebar_hit(area, 3, layout::Chrome::Collapsed, 1, 1),
            Some(HitTarget::AgentTab(1))
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib tui::render::tests::collapsed`
Expected: compile error — `draw_sidebar` takes 5 arguments, `sidebar_hit` takes 4.

- [ ] **Step 3: Add the collapsed renderer**

In `src/tui/render.rs`, change the `draw_sidebar` signature (line 752) and insert the collapsed branch at the very top of its body, before `let block = Block::default().borders(Borders::RIGHT);`:

```rust
pub fn draw_sidebar(
    frame: &mut Frame,
    state: &AppState,
    cache: &GitStatusCache,
    area: Rect,
    chrome: layout::Chrome,
    now_ms: u64,
) {
    if chrome == layout::Chrome::Collapsed {
        draw_sidebar_collapsed(frame, state, area, now_ms);
        return;
    }

    let block = Block::default().borders(Borders::RIGHT);
    // ... rest of the existing body unchanged ...
```

Then add these two functions directly after `draw_sidebar` ends (before `sidebar_name_line`):

```rust
/// Draw the collapsed agent strip: one indicator glyph per agent, no heading
/// and no close control, for windows too small to afford the full sidebar.
fn draw_sidebar_collapsed(frame: &mut Frame, state: &AppState, area: Rect, now_ms: u64) {
    let block = Block::default().borders(Borders::RIGHT);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines: Vec<Line> = state
        .tabs
        .iter()
        .enumerate()
        .map(|(i, tab)| {
            Line::from(collapsed_agent_span(
                tab,
                state.selected_tab == Some(i),
                now_ms,
            ))
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

/// The single-cell indicator for one agent in the collapsed strip: the
/// selection arrow, a spinner while it works, or its status dot. Same glyphs
/// and colours the full sidebar draws on each agent's name line, with the text
/// removed.
fn collapsed_agent_span(
    tab: &crate::app::state::RuntimeTab,
    selected: bool,
    now_ms: u64,
) -> Span<'static> {
    use crate::contracts::InterpretedStatus::{Running, Starting, Working};

    if selected {
        return Span::styled(
            "▸",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
    }
    let spinner = || {
        Span::styled(
            spinner_frame(now_ms).to_string(),
            Style::default().fg(Color::Red),
        )
    };
    if tab.phase == TabPhase::Creating {
        return spinner();
    }
    let ds = tab.display_status(now_ms);
    if matches!(ds.interpreted, Starting | Running | Working) {
        return spinner();
    }
    let color = ds
        .manual
        .map(|_| Color::Cyan)
        .unwrap_or_else(|| status_label_color(ds.interpreted).1);
    Span::styled("●", Style::default().fg(color))
}
```

- [ ] **Step 4: Branch `sidebar_hit` on chrome**

Replace `sidebar_hit` (line 358) with:

```rust
fn sidebar_hit(
    area: Rect,
    tab_count: usize,
    chrome: layout::Chrome,
    col: u16,
    row: u16,
) -> Option<HitTarget> {
    let inner = Block::default().borders(Borders::RIGHT).inner(area);
    if col < inner.x || col >= inner.x.saturating_add(inner.width) {
        return None;
    }
    // The collapsed strip is one row per agent with no heading above it.
    let (header_rows, rows_per_tab) = match chrome {
        layout::Chrome::Full => (SIDEBAR_HEADER_ROWS, SIDEBAR_ROWS_PER_TAB),
        layout::Chrome::Collapsed => (0, 1),
    };
    let first = inner.y.saturating_add(header_rows);
    if row < first {
        return None;
    }
    let rel = row - first;
    let idx = (rel / rows_per_tab) as usize;
    if idx >= tab_count {
        return None;
    }
    // Within a full tab block the rows are: divider(0), name(1), agent(2),
    // git(3). The `✕` lives on the name row at the far right; give it a
    // forgiving 3-column target so it stays easy to click. The collapsed strip
    // has no close control — use APP mode to close an agent.
    if chrome == layout::Chrome::Full && rel % rows_per_tab == 1 {
        let close_col = inner.x.saturating_add(inner.width).saturating_sub(1);
        if col >= close_col.saturating_sub(2) {
            return Some(HitTarget::CloseAgentTab(idx));
        }
    }
    Some(HitTarget::AgentTab(idx))
}
```

Also update its doc comment's last line to mention the collapsed form:

```rust
/// (header/heading/empty space below the tabs). In the collapsed strip each
/// agent occupies a single row and there is no close control.
```

- [ ] **Step 5: Update the two callers**

In `src/tui/render.rs`:
- In `hit_test` (around line 305): `sidebar_hit(ml.sidebar, state.tabs.len(), col, row)` → `sidebar_hit(ml.sidebar, state.tabs.len(), layout::Chrome::Full, col, row)`
- In `draw` (line 511): `draw_sidebar(frame, state, cache, ml.sidebar, now_ms);` → `draw_sidebar(frame, state, cache, ml.sidebar, layout::Chrome::Full, now_ms);`

(Both become `chrome_for`-derived in Task 3.)

- [ ] **Step 6: Run the tests**

Run: `cargo test --lib tui::render`
Expected: PASS, including the two new tests.

Run: `cargo test --locked`
Expected: PASS.

- [ ] **Step 7: Lint, format, and commit**

```bash
cargo fmt --all
cargo clippy --all-targets --locked -- -D warnings
git add src/tui/render.rs
git commit -m "feat(render): draw and hit-test the collapsed agent strip"
```

---

### Task 3: Wire the collapse rule into drawing and hit-testing

This is the task that makes collapse actually happen on screen.

**Files:**
- Modify: `src/tui/render.rs` — `hit_test` (line 297), `draw` (line 498)
- Test: `src/tui/render.rs` `mod tests`

**Interfaces:**
- Consumes: `layout::chrome_for` (Task 1), `draw_sidebar`/`sidebar_hit` with `chrome` (Task 2)
- Produces: no signature changes; `draw` and `hit_test` now honour `state.mode()` and the window size.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/tui/render.rs`, after the two tests from Task 2:

```rust
    /// A window below both thresholds, so terminal mode collapses the chrome.
    const SMALL: (u16, u16) = (100, 24);

    #[test]
    fn small_window_in_terminal_mode_hides_the_info_bar_and_narrows_the_sidebar() {
        let (w, h) = SMALL;
        let mut state = state_with_tabs(2);
        state.selected_tab = Some(0);
        state.focus_terminal();

        let mut term = test_terminal(w, h);
        term.draw(|frame| draw(frame, &state, &empty_cache(), &UiOverlay::None, 0))
            .unwrap();
        let buffer = term.backend().buffer().clone();
        let all: String = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol().to_string())
            .collect();

        // The sidebar heading and the git info bar are both gone. "⎇" is the
        // info bar's branch marker and appears nowhere else in the main view.
        assert!(!all.contains("Agents"), "collapsed sidebar has no heading");
        assert!(!all.contains('⎇'), "collapsed layout has no git info bar");
        // The status bar stays — it carries the mode and the command hints.
        assert!(
            !buffer_row(&buffer, h - 1).trim().is_empty(),
            "the status bar must still be drawn when collapsed"
        );
    }

    #[test]
    fn same_small_window_in_app_mode_keeps_full_chrome() {
        let (w, h) = SMALL;
        let mut state = state_with_tabs(2);
        state.selected_tab = Some(0);
        state.focus_app();

        let mut term = test_terminal(w, h);
        term.draw(|frame| draw(frame, &state, &empty_cache(), &UiOverlay::None, 0))
            .unwrap();
        let buffer = term.backend().buffer().clone();
        let all: String = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol().to_string())
            .collect();

        assert!(all.contains("Agents"), "app mode keeps the full sidebar");
        assert!(all.contains('⎇'), "app mode keeps the git info bar");
    }

    #[test]
    fn hit_test_uses_the_collapsed_strip_in_a_small_terminal_mode_window() {
        let (w, h) = SMALL;
        let mut state = state_with_tabs(2);
        state.focus_terminal();
        let area = Rect::new(0, 0, w, h);

        // Body starts at row 2 (header row 0, divider row 1) — no project row.
        assert_eq!(hit_test(area, &state, 0, 2), Some(HitTarget::AgentTab(0)));
        assert_eq!(hit_test(area, &state, 0, 3), Some(HitTarget::AgentTab(1)));
        // The strip's border column is sidebar chrome, not an agent.
        assert_eq!(
            hit_test(area, &state, layout::COLLAPSED_SIDEBAR_WIDTH - 1, 2),
            Some(HitTarget::Sidebar)
        );
        // Just past the strip is the main pane, not the sidebar.
        assert_ne!(
            hit_test(area, &state, layout::COLLAPSED_SIDEBAR_WIDTH, 2),
            Some(HitTarget::Sidebar)
        );

        // App mode restores the full sidebar geometry on the same window.
        state.focus_app();
        assert_eq!(hit_test(area, &state, 0, 2), None);
    }
```

Add this helper next to `strip_glyph` in the test module:

```rust
    /// Read a full row of a rendered buffer as a string.
    fn buffer_row(buffer: &ratatui::buffer::Buffer, row: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, row)].symbol().to_string())
            .collect()
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib tui::render::tests::small_window`
Expected: FAIL — the collapsed assertions fail because `draw` still hardcodes `Chrome::Full`, so "Agents" and "git: ?" are present.

- [ ] **Step 3: Derive the chrome in `hit_test`**

In `src/tui/render.rs`, replace the first line of `hit_test`'s body (line 297) with:

```rust
    let chrome = layout::chrome_for(area, state.mode());
    let ml = layout::compute(area, chrome);
```

and change the `sidebar_hit` call from `layout::Chrome::Full` to `chrome`:

```rust
        return Some(
            sidebar_hit(ml.sidebar, state.tabs.len(), chrome, col, row)
                .unwrap_or(HitTarget::Sidebar),
        );
```

- [ ] **Step 4: Derive the chrome in `draw` and skip the hidden bars**

In `draw` (line 498), replace:

```rust
    let area = frame.area();
    let ml = layout::compute(area, layout::Chrome::Full);
```

with:

```rust
    let area = frame.area();
    let chrome = layout::chrome_for(area, state.mode());
    let ml = layout::compute(area, chrome);
```

Change the sidebar call to pass `chrome`:

```rust
    draw_sidebar(frame, state, cache, ml.sidebar, chrome, now_ms);
```

And wrap the info divider and info bar so they are skipped when collapsed — replace:

```rust
    let info_divider = Paragraph::new(divider_line(ml.info_divider.width as usize));
    frame.render_widget(info_divider, ml.info_divider);
    draw_info_bar(frame, state, cache, ml.info_bar);
```

with:

```rust
    // Collapsed chrome gives the git info bar's rows to the terminal; git
    // details stay reachable through the git status overlay.
    if chrome == layout::Chrome::Full {
        let info_divider = Paragraph::new(divider_line(ml.info_divider.width as usize));
        frame.render_widget(info_divider, ml.info_divider);
        draw_info_bar(frame, state, cache, ml.info_bar);
    }
```

- [ ] **Step 5: Run the tests**

Run: `cargo test --lib tui::render`
Expected: PASS, including the three new tests.

Run: `cargo test --locked`
Expected: PASS. Existing render tests use 80×24 areas with the default `InputMode::App`, so they stay on full chrome.

- [ ] **Step 6: Lint, format, and commit**

```bash
cargo fmt --all
cargo clippy --all-targets --locked -- -D warnings
git add src/tui/render.rs
git commit -m "feat(render): collapse chrome in small terminal-mode windows"
```

---

### Task 4: Wire the collapse rule into the wiring layer

Mouse routing and the project tab row live in `src/lib.rs` and compute their own layout. They must agree with what was drawn.

**Files:**
- Modify: `src/lib.rs:1255` (render loop, project tab row), `src/lib.rs:1511` (`handle_mouse`), `src/lib.rs:1768` (`terminal_at`), `src/lib.rs:1788` (`viewport_for_target`), `src/lib.rs:3570` (`sync_terminal_sizes`, split path)
- Test: `src/lib.rs` `mod tests`

**Interfaces:**
- Consumes: `layout::chrome_for` (Task 1)
- Produces: no signature changes.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/lib.rs`, after `viewport_size_is_smaller_than_full_terminal`:

```rust
    #[test]
    fn terminal_at_follows_the_collapsed_sidebar_in_terminal_mode() {
        use crate::persistence::project_state::default_state;

        let mut state = AppState::new(
            Config::default(),
            default_state("main"),
            "/repo",
            "/repo/state.json",
        );
        // Below both collapse thresholds (108 cols, 32 rows).
        let area = Rect::new(0, 0, 100, 24);

        // App mode keeps the 28-column sidebar, so column 5 is not the terminal.
        state.focus_app();
        assert!(terminal_at(area, &state, 5, 10).is_none());

        // Terminal mode collapses the sidebar to a 3-column strip, so the same
        // point is now inside the viewport.
        state.focus_terminal();
        let (_, viewport) =
            terminal_at(area, &state, 5, 10).expect("collapsed viewport covers column 5");
        assert_eq!(viewport.x, crate::tui::layout::COLLAPSED_SIDEBAR_WIDTH);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib tests::terminal_at_follows`
Expected: FAIL — `terminal_at` still computes full chrome, so the terminal-mode lookup returns `None`.

- [ ] **Step 3: Derive the chrome at each wiring call site**

In `src/lib.rs`, replace each of these five `crate::tui::layout::Chrome::Full` arguments (added in Task 1) with a locally derived chrome.

Line ~1255, inside the `terminal.draw` closure:

```rust
                let area = frame.area();
                let chrome = crate::tui::layout::chrome_for(area, p.state.mode());
                let ml = crate::tui::layout::compute(area, chrome);
```

Line ~1511, in `handle_mouse`:

```rust
        let chrome =
            crate::tui::layout::chrome_for(area, workspace.active_project().state.mode());
        let ml = crate::tui::layout::compute(area, chrome);
```

Line ~1768, in `terminal_at`:

```rust
    let ml = crate::tui::layout::compute(
        area,
        crate::tui::layout::chrome_for(area, state.mode()),
    );
```

Line ~1788, in `viewport_for_target`:

```rust
    let ml = crate::tui::layout::compute(
        area,
        crate::tui::layout::chrome_for(area, state.mode()),
    );
```

Line ~3570, in `sync_terminal_sizes`'s split-view branch:

```rust
        let area = Rect::new(0, 0, full.cols, full.rows);
        let ml = crate::tui::layout::compute(
            area,
            crate::tui::layout::chrome_for(area, state.mode()),
        );
```

Leave line 1462 (`viewport_pty_size`) alone — Task 5 changes it.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib tests::terminal_at_follows`
Expected: PASS.

Run: `cargo test --locked`
Expected: PASS.

- [ ] **Step 5: Lint, format, and commit**

```bash
cargo fmt --all
cargo clippy --all-targets --locked -- -D warnings
git add src/lib.rs
git commit -m "feat(wiring): route mouse and project row through the collapse rule"
```

---

### Task 5: PTY sizing follows the collapse, plus changelog

The agent's PTY must be resized when collapse changes the viewport — including on a mode toggle, which is not a terminal resize event.

**Files:**
- Modify: `src/lib.rs:1461` (`viewport_pty_size`), `src/lib.rs:234` (startup seed), `src/lib.rs:1285` (`Event::Resize` arm), `src/lib.rs:3563` (`sync_terminal_sizes`)
- Modify: `CHANGELOG.md`
- Test: `src/lib.rs` `mod tests`

**Interfaces:**
- Consumes: `layout::chrome_for`, `layout::compute` (Task 1)
- Produces: `fn viewport_pty_size(full: PtySize, mode: InputMode) -> PtySize` — signature change.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/lib.rs`, next to `viewport_size_is_smaller_than_full_terminal`:

```rust
    #[test]
    fn collapsed_viewport_is_larger_only_where_the_window_is_small() {
        // Below both thresholds: terminal mode collapses and reclaims space.
        let small = PtySize {
            rows: 24,
            cols: 100,
        };
        let app = viewport_pty_size(small, InputMode::App);
        let terminal = viewport_pty_size(small, InputMode::Terminal);
        assert!(terminal.rows > app.rows, "collapsing reclaims chrome rows");
        assert!(
            terminal.cols > app.cols,
            "collapsing reclaims sidebar columns"
        );

        // A large window never collapses, so both modes agree exactly.
        let large = PtySize {
            rows: 50,
            cols: 200,
        };
        assert_eq!(
            viewport_pty_size(large, InputMode::App),
            viewport_pty_size(large, InputMode::Terminal)
        );
    }
```

Update the existing `viewport_size_is_smaller_than_full_terminal` test to pass a mode — change the line

```rust
        let vp = viewport_pty_size(full);
```

to

```rust
        let vp = viewport_pty_size(full, InputMode::App);
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib tests::collapsed_viewport`
Expected: compile error — `viewport_pty_size` takes 1 argument.

- [ ] **Step 3: Make `viewport_pty_size` mode-aware**

Replace `viewport_pty_size` (line 1461) with:

```rust
/// Compute the PTY/terminal-viewport size from the full terminal size. Agents
/// must wrap at the viewport width (total minus the sidebar/borders), not the
/// whole screen. `mode` matters because collapsed chrome hands the sidebar's
/// columns and the hidden bars' rows to the viewport.
fn viewport_pty_size(full: PtySize, mode: InputMode) -> PtySize {
    let area = Rect::new(0, 0, full.cols, full.rows);
    let ml = crate::tui::layout::compute(area, crate::tui::layout::chrome_for(area, mode));
    PtySize {
        rows: ml.terminal.height.max(1),
        cols: ml.terminal.width.max(1),
    }
}
```

- [ ] **Step 4: Update the three production call sites**

Startup seed (line ~234) — the size now differs per project, so move it inside the loop:

```rust
    if let Ok(size) = terminal.size() {
        let full = PtySize {
            rows: size.height,
            cols: size.width,
        };
        for p in workspace.projects.iter_mut() {
            let vp = viewport_pty_size(full, p.state.mode());
            p.state.set_pty_size(vp);
        }
    }
```

`Event::Resize` arm (line ~1284) — same, each project uses its own mode:

```rust
            Event::Resize(cols, rows) => {
                let full = PtySize { rows, cols };
                // Resize every project's sessions so a background agent's output
                // wraps correctly the moment the user switches back to it.
                for p in workspace.projects.iter_mut() {
                    let size = viewport_pty_size(full, p.state.mode());
                    p.state.set_pty_size(size);
                    resize_sessions(&mut p.state, size);
                }
            }
```

`sync_terminal_sizes` (line 3563) — add one line at the top of the body, **before** the `selected_tab` early return so newly spawned terminals also start at the right size:

```rust
fn sync_terminal_sizes(state: &mut AppState, full: PtySize) {
    // Collapse follows the input mode, not just the window size, so re-derive
    // the viewport every frame rather than only on `Event::Resize`.
    // `resize_if_changed` below makes the frames where nothing moved free.
    state.pty_size = viewport_pty_size(full, state.mode());

    let Some(idx) = state.selected_tab else {
        return;
    };
```

Extend the function's doc comment with a sentence:

```rust
/// Also re-derives `state.pty_size` from the current mode, so toggling between
/// APP and TERMINAL resizes the agent PTY to match the chrome that is drawn.
```

- [ ] **Step 5: Run the tests**

Run: `cargo test --lib tests::collapsed_viewport tests::viewport_size`
Expected: PASS.

Run: `cargo test --locked`
Expected: PASS.

- [ ] **Step 6: Update the changelog**

In `CHANGELOG.md`, under `## [Unreleased]` → `### New features`, replace `- None yet.` with:

```markdown
- Collapse FlightDeck's chrome in small windows while in terminal focus: the
  project tab row and git info bar hide and the agents sidebar shrinks to a
  one-glyph strip, giving the agent three more rows and 25 more columns.
  Switching to app mode restores everything.
```

- [ ] **Step 7: Verify the whole suite, then commit**

```bash
cargo fmt --all
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
git add src/lib.rs CHANGELOG.md
git commit -m "feat(pty): size the agent viewport from the collapsed chrome"
```

- [ ] **Step 8: Manual verification with the user**

Build and run FlightDeck, then check by hand:

```bash
cargo run
```

1. In a window larger than 108×32, press the terminal-focus key. Nothing changes.
2. Shrink the window below 108 columns or 32 rows while in terminal focus. The project row and git info bar disappear; the sidebar becomes a narrow strip of glyphs; the agent reflows to the wider viewport.
3. Leave terminal focus (`Alt+Esc` / `Shift+Esc`, or F2 if configured). Full chrome returns and the agent reflows back.
4. Re-enter terminal focus. It collapses again.
5. Click an agent glyph in the collapsed strip — it selects that agent and the layout expands (selecting from the sidebar focuses the app).
6. With two agents, confirm the selected one shows `▸` and a working one shows a spinner.

Report the result to the user and get approval before opening a PR.

---

## Self-Review

**Spec coverage:**

| Spec section | Task |
| --- | --- |
| Collapse rule (`Chrome`, `chrome_for`, thresholds) | 1 |
| Layout table (zero-sized rects, 3-col sidebar) | 1 |
| Collapsed sidebar glyphs and colours | 2 |
| Collapsed sidebar hit-testing, no close control | 2 |
| `draw` skips info divider + info bar | 3 |
| `hit_test` honours mode and size | 3 |
| Call sites: render loop, `handle_mouse`, `terminal_at`, `viewport_for_target`, `sync_terminal_sizes` split path | 4 |
| PTY sizing (`viewport_pty_size` mode param, per-frame re-derive, `Event::Resize` per project) | 5 |
| Test plan (chrome truth table, layout geometry, degenerate areas, hit-testing, rendering, PTY sizing) | 1, 2, 3, 4, 5 |

The project tab row is drawn from `src/lib.rs`, not `render::draw`. Task 1 gives it a zero-height rect, and Task 4 makes the wiring layer derive the same chrome; together these make its *hit test* a no-op for free when collapsed. Its *draw* does not get this for free: `draw_project_tab_bar` builds a fixed-height sub-rect for its "+ project" button, so it needs an explicit zero-area guard even though it derives its rect from `MainLayout`.

**Type consistency:** `Chrome`, `chrome_for`, `COLLAPSED_SIDEBAR_WIDTH`, `MIN_FULL_ROWS`, `MIN_FULL_COLS` are defined in Task 1 and used with the same names in Tasks 2–5. `draw_sidebar` and `sidebar_hit` take `chrome` in the same position (before the trailing arguments) everywhere. `viewport_pty_size(full, mode)` is defined and called consistently in Task 5.
