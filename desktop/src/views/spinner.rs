//! The working arc's animation, at a fixed low rate, and only while anyone
//! can see it.
//!
//! A working session shows a spinning arc in its sidebar row, on its project
//! tab and on its Mission control tile and rail row. GPUI's own animations
//! (`with_animation`) ask for a frame on every display refresh, 120 times a
//! second on a ProMotion display, and each of those frames re-renders and
//! repaints the window. Four working tiles cost 6–21 % of a core that way
//! (desktop/NOTES-M0.md, "Performance (S4)").
//!
//! Instead the arc turns in [`STEPS`] discrete steps, one every [`STEP`]
//! (8 fps, one turn a second), driven by ONE clock for the whole app
//! ([`SpinnerClock`], a GPUI global):
//!
//! - Every arc is its own tiny entity ([`SpinnerView`]), created on first
//!   sight by the element [`spinner`] returns (`Window::use_keyed_state`, so
//!   it lives exactly as long as the arc stays on screen) and embedded as a
//!   *cached* view.
//! - A step notifies only those entities. GPUI then redraws the frame with the
//!   arcs re-rendered and every other cached view replayed: the Mission
//!   control tiles' terminals, the other arcs. The views that contain an arc
//!   (the window root, Mission control) are marked dirty with it — GPUI has no
//!   narrower repaint than that — but no terminal grid is laid out or shaped
//!   again for a step.
//! - The clock runs only while [`SpinnerSchedule::running`] says so: at least
//!   one arc is alive, and the window is both active (the key window) and
//!   visible (not minimised, fully covered, or on another Space —
//!   `Window::is_visible`, `NSWindow.occlusionState` on macOS). Otherwise it
//!   stops altogether (no timer, no wakeups) and the arcs hold their current
//!   step; the shell reports the window's state as it changes
//!   ([`SpinnerClock::set_window`]).
//!
//! A still arc still reads as "working": the glyph's shape carries the
//! status (see [`crate::views::icons`]), the motion is only emphasis.

use std::time::Duration;

use gpui::{
    percentage, App, Bounds, Context, Element, ElementId, Entity, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, Render, StyleRefinement, Styled,
    Transformation, WeakEntity, Window,
};

use crate::assets::icon;
use crate::theme::Hex;

/// Steps in one turn of the arc (45° each).
pub const STEPS: u8 = 8;

/// Time between steps: 8 fps, one turn a second (the old continuous
/// animation took 1.1 s a turn).
pub const STEP: Duration = Duration::from_millis(125);

/// Whether the spinner clock should run: the pure half of the decision, so
/// the rule is tested without a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpinnerSchedule {
    /// The window is the active (key) window.
    pub window_active: bool,
    /// The platform is presenting the window's frames.
    pub window_visible: bool,
    /// Arcs currently on screen.
    pub spinners: usize,
}

impl Default for SpinnerSchedule {
    /// Until the shell reports otherwise, a window is taken to be shown: a
    /// window opens active and visible.
    fn default() -> Self {
        Self {
            window_active: true,
            window_visible: true,
            spinners: 0,
        }
    }
}

impl SpinnerSchedule {
    /// Step the arcs only when there is one to step and someone can see it
    /// move. An inactive or hidden window holds still.
    pub fn running(&self) -> bool {
        self.spinners > 0 && self.window_active && self.window_visible
    }
}

/// The step after `phase`, wrapping at [`STEPS`].
pub fn next_phase(phase: u8) -> u8 {
    (phase + 1) % STEPS
}

/// The arc's rotation at `phase`, as a fraction of a turn.
pub fn turn_fraction(phase: u8) -> f32 {
    f32::from(phase % STEPS) / f32::from(STEPS)
}

/// The app's one spinner clock. See the module docs.
#[derive(Default)]
pub struct SpinnerClock {
    phase: u8,
    window_active: bool,
    window_visible: bool,
    /// Every arc created so far; dead handles (arcs that left the screen) are
    /// dropped on the next step.
    spinners: Vec<WeakEntity<SpinnerView>>,
    /// The step loop is running.
    ticking: bool,
    /// Steps taken, for the tests.
    steps: u64,
}

impl gpui::Global for SpinnerClock {}

impl SpinnerClock {
    fn get(cx: &mut App) -> &mut SpinnerClock {
        if !cx.has_global::<SpinnerClock>() {
            cx.set_global(SpinnerClock {
                window_active: true,
                window_visible: true,
                ..SpinnerClock::default()
            });
        }
        cx.global_mut::<SpinnerClock>()
    }

    /// The current step (0 before the clock ever ran).
    pub fn phase(cx: &App) -> u8 {
        cx.try_global::<SpinnerClock>().map_or(0, |c| c.phase)
    }

    /// Steps taken since start-up.
    #[cfg(test)]
    pub fn steps(cx: &App) -> u64 {
        cx.try_global::<SpinnerClock>().map_or(0, |c| c.steps)
    }

    /// Whether the step loop is running now.
    #[cfg(test)]
    pub fn is_ticking(cx: &App) -> bool {
        cx.try_global::<SpinnerClock>().is_some_and(|c| c.ticking)
    }

    /// The schedule as it stands, counting only arcs still alive.
    pub fn schedule(cx: &App) -> SpinnerSchedule {
        match cx.try_global::<SpinnerClock>() {
            None => SpinnerSchedule::default(),
            Some(c) => SpinnerSchedule {
                window_active: c.window_active,
                window_visible: c.window_visible,
                spinners: c.spinners.iter().filter(|w| w.upgrade().is_some()).count(),
            },
        }
    }

    /// The window became (in)active or (in)visible: stop the clock, or
    /// restart it.
    pub fn set_window(active: bool, visible: bool, cx: &mut App) {
        let clock = Self::get(cx);
        if (clock.window_active, clock.window_visible) == (active, visible) {
            return;
        }
        clock.window_active = active;
        clock.window_visible = visible;
        Self::ensure_ticking(cx);
    }

    fn register(spinner: WeakEntity<SpinnerView>, cx: &mut App) {
        Self::get(cx).spinners.push(spinner);
        Self::ensure_ticking(cx);
    }

    /// Start the step loop if the schedule wants it and it is not running.
    fn ensure_ticking(cx: &mut App) {
        if Self::get(cx).ticking || !Self::schedule(cx).running() {
            return;
        }
        Self::get(cx).ticking = true;
        cx.spawn(async move |cx| loop {
            cx.background_executor().timer(STEP).await;
            if !cx.update(Self::step) {
                break;
            }
        })
        .detach();
    }

    /// One step: advance the phase and notify every live arc. Returns whether
    /// the loop goes on; when the schedule no longer runs, the loop ends here
    /// and the arcs keep the step they are on.
    fn step(cx: &mut App) -> bool {
        let clock = Self::get(cx);
        clock.spinners.retain(|w| w.upgrade().is_some());
        if !Self::schedule(cx).running() {
            Self::get(cx).ticking = false;
            return false;
        }
        let clock = Self::get(cx);
        clock.phase = next_phase(clock.phase);
        clock.steps += 1;
        let live: Vec<Entity<SpinnerView>> =
            clock.spinners.iter().filter_map(|w| w.upgrade()).collect();
        for spinner in live {
            spinner.update(cx, |_, cx| cx.notify());
        }
        true
    }
}

/// One arc on screen: a cached view that re-renders only when the clock
/// steps (or its size or colour changes).
pub struct SpinnerView {
    size: Pixels,
    colour: Hex,
}

impl Render for SpinnerView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::views::icons::icon(icon::STATUS_WORKING, self.size, self.colour).with_transformation(
            Transformation::rotate(percentage(turn_fraction(SpinnerClock::phase(cx)))),
        )
    }
}

/// The working arc, turning with the app's [`SpinnerClock`]. `id` must be
/// unique among its siblings (it keys the arc's entity).
pub fn spinner(id: impl Into<ElementId>, size: Pixels, colour: Hex) -> SpinnerSlot {
    SpinnerSlot {
        id: id.into(),
        size,
        colour,
    }
}

/// The element [`spinner`] returns: finds (or creates) the arc's entity for
/// this frame and lays it out as a cached view of a fixed size.
pub struct SpinnerSlot {
    id: ElementId,
    size: Pixels,
    colour: Hex,
}

impl IntoElement for SpinnerSlot {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for SpinnerSlot {
    type RequestLayoutState = gpui::AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let (size, colour) = (self.size, self.colour);
        let view = window.use_keyed_state(self.id.clone(), cx, |_, cx| {
            SpinnerClock::register(cx.weak_entity(), cx);
            SpinnerView { size, colour }
        });
        view.update(cx, |view, cx| {
            if (view.size, view.colour) != (size, colour) {
                (view.size, view.colour) = (size, colour);
                cx.notify();
            }
        });
        let mut element = view
            .cached(StyleRefinement::default().size(size).flex_none())
            .into_any_element();
        let layout = element.request_layout(window, cx);
        (layout, element)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        element: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        element: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.paint(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_runs_only_for_a_visible_active_window_with_an_arc() {
        let on = SpinnerSchedule {
            window_active: true,
            window_visible: true,
            spinners: 3,
        };
        assert!(on.running());
        assert!(
            !SpinnerSchedule {
                window_active: false,
                ..on
            }
            .running(),
            "an inactive window holds still"
        );
        assert!(
            !SpinnerSchedule {
                window_visible: false,
                ..on
            }
            .running(),
            "a hidden (occluded, minimised) window holds still"
        );
        assert!(
            !SpinnerSchedule { spinners: 0, ..on }.running(),
            "nothing to turn"
        );
        assert!(
            !SpinnerSchedule::default().running(),
            "a fresh window has no arc yet"
        );
        assert!(SpinnerSchedule {
            spinners: 1,
            ..SpinnerSchedule::default()
        }
        .running());
    }

    #[test]
    fn a_turn_is_eight_steps_at_eight_per_second() {
        let mut phase = 0;
        let mut seen = Vec::new();
        for _ in 0..STEPS {
            seen.push(turn_fraction(phase));
            phase = next_phase(phase);
        }
        assert_eq!(phase, 0, "wraps after one turn");
        assert_eq!(seen[0], 0.0);
        assert_eq!(seen[2], 0.25);
        assert!(seen.windows(2).all(|w| w[1] > w[0]), "always forward");
        assert_eq!(STEP * u32::from(STEPS), Duration::from_secs(1));
    }
}
