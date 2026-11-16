//! The native view the session browser's WKWebView lives in, and who gets the
//! keyboard.
//!
//! - **A container of its own.** wry adds the web view straight to GPUI's
//!   view. WebKit docks DevTools *beside* the web view, inside its superview,
//!   and lays both out from that superview's bounds, so with GPUI's view as
//!   the superview the page and the inspector spread over the whole window
//!   (over the sidebar) until the next resize. Here the web view fills a
//!   plain `NSView` that we place instead, so a docked inspector shares the
//!   browser's area and hides with it.
//! - **The keyboard is the page's only while you last clicked it.** The page
//!   takes the window's first responder when clicked, but also on its own: a
//!   page focusing a field as it loads, DevTools opening, DevTools closing
//!   (which leaves the window itself as first responder). GPUI's view never
//!   asks for it back (it does not accept first responder on a click), and
//!   while the page holds it, letters go to the page and only the keys it
//!   does not use (arrows, ⌘-shortcuts) reach GPUI. So a click monitor
//!   records whether the last click in this window was in the browser, and
//!   [`Host::enforce`] (run by the view's poll) gives the keyboard back to
//!   GPUI whenever it was not, the browser is hidden, or nobody has it.

use std::cell::Cell;
use std::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::MainThreadMarker;
use objc2::MainThreadOnly;
use objc2_app_kit::{NSAutoresizingMaskOptions, NSEvent, NSEventMask, NSView, NSWindow};
use objc2_foundation::{NSPoint, NSRect, NSSize};

/// See the module docs.
pub struct Host {
    /// Ours: the web view, and a docked inspector, live in it.
    container: Retained<NSView>,
    /// GPUI's view, which the container sits in and the keyboard goes back to.
    parent: Retained<NSView>,
    /// Whether the last click in this window was in the browser.
    clicked_in: Rc<Cell<bool>>,
    /// The click monitor, removed when the browser goes.
    monitor: Option<Retained<AnyObject>>,
}

impl Host {
    /// Move `webview` (a child of GPUI's view, as wry builds it) into a
    /// container of its own, hidden until shown, and start watching clicks.
    /// `None` off the main thread or for a web view with no parent.
    pub fn adopt(webview: &wry::WebView) -> Option<Host> {
        use wry::WebViewExtMacOS;
        let mtm = MainThreadMarker::new()?;
        let wk = webview.webview();
        let wk: &NSView = &wk;
        // SAFETY: on the main thread (`mtm`); the superview comes back
        // retained, so it outlives this call whatever AppKit does with it.
        let parent = unsafe { wk.superview() }?;
        let container = NSView::initWithFrame(NSView::alloc(mtm), wk.frame());
        // Kept to the top of GPUI's (unflipped) view as the window resizes,
        // until the next frame places it; as wry does for a child web view.
        container.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
        container.setHidden(true);
        parent.addSubview(&container);
        // `addSubview` takes it out of GPUI's view.
        container.addSubview(wk);
        wk.setFrame(container.bounds());
        wk.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );

        let clicked_in = Rc::new(Cell::new(false));
        let monitor = {
            let container = container.clone();
            let parent = parent.clone();
            let clicked_in = clicked_in.clone();
            let block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
                // SAFETY: AppKit hands the monitor a valid event for the
                // duration of the call.
                let ev = unsafe { event.as_ref() };
                if let (Some(window), Some(ours)) = (ev.window(mtm), container.window()) {
                    if Retained::as_ptr(&window) == Retained::as_ptr(&ours) {
                        let inside = !container.isHidden() && {
                            let at = container.convertPoint_fromView(ev.locationInWindow(), None);
                            let b = container.bounds();
                            at.x >= b.origin.x
                                && at.y >= b.origin.y
                                && at.x < b.origin.x + b.size.width
                                && at.y < b.origin.y + b.size.height
                        };
                        clicked_in.set(inside);
                        if !inside {
                            // Before GPUI sees the click, so its keys follow.
                            give_back(&container, &parent, false);
                        }
                    }
                }
                event.as_ptr()
            });
            let mask = NSEventMask::LeftMouseDown
                | NSEventMask::RightMouseDown
                | NSEventMask::OtherMouseDown;
            // SAFETY: the block returns the event it was given, unchanged,
            // and the monitor is removed in `Drop`.
            unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &block) }
        };
        Some(Host {
            container,
            parent,
            clicked_in,
            monitor,
        })
    }

    /// Place the browser at `x`, `y` (from the window's top left), `width` by
    /// `height`, in logical pixels as GPUI lays out.
    pub fn place(&self, x: f64, y: f64, width: f64, height: f64) {
        let y = if self.parent.isFlipped() {
            y
        } else {
            self.parent.frame().size.height - y - height
        };
        self.container
            .setFrame(NSRect::new(NSPoint::new(x, y), NSSize::new(width, height)));
    }

    /// Show or hide the web view and anything docked beside it.
    pub fn set_hidden(&self, hidden: bool) {
        if hidden {
            // Before hiding: a hidden first responder passes the keyboard
            // to the next key view, not to GPUI.
            self.clicked_in.set(false);
            self.release_keyboard();
        }
        self.container.setHidden(hidden);
    }

    /// Give the keyboard back to GPUI unless the browser is on screen and
    /// was the last thing clicked in this window. Also when nobody holds it.
    pub fn enforce(&self, shown: bool) {
        let page_may_keep = shown && self.clicked_in.get();
        give_back(&self.container, &self.parent, page_may_keep);
    }

    /// Whether the page or its inspector holds the keyboard, for
    /// `--spike-browser`.
    pub fn page_has_keyboard(&self) -> bool {
        let Some(window) = self.container.window() else {
            return false;
        };
        window.firstResponder().is_some_and(|responder| {
            responder
                .downcast_ref::<NSView>()
                .is_some_and(|view| view.isDescendantOf(&self.container))
        })
    }

    /// Give the keyboard back to GPUI if the page or its inspector has it.
    pub fn release_keyboard(&self) {
        give_back(&self.container, &self.parent, false);
    }
}

/// Make GPUI's `parent` the first responder when the window's is inside
/// `container` (unless `page_may_keep`), or is no view at all (the window
/// itself, after a first responder was hidden or removed). A first
/// responder in another browser's container is that browser's business.
fn give_back(container: &NSView, parent: &NSView, page_may_keep: bool) {
    let Some(window) = container.window() else {
        return;
    };
    let take = match window.firstResponder() {
        None => true,
        Some(responder) => match responder.downcast_ref::<NSView>() {
            Some(view) if view.isDescendantOf(container) => !page_may_keep,
            Some(_) => false,
            None => responder.downcast_ref::<NSWindow>().is_some(),
        },
    };
    if take {
        window.makeFirstResponder(Some(parent));
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        if let Some(monitor) = self.monitor.take() {
            // SAFETY: the object `addLocalMonitor…` returned, removed once.
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
        // A first responder taken out of the window leaves the window itself
        // as first responder, and GPUI would get no keys.
        self.release_keyboard();
        self.container.removeFromSuperview();
    }
}
