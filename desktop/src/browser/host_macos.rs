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
//! - **The keyboard comes back.** Clicking the page or opening DevTools makes
//!   them the window's first responder. GPUI's view never asks for it back
//!   (it does not accept first responder on a click), so every key would go
//!   to the page for good. [`Host::release_keyboard`] hands it back to GPUI;
//!   the view calls it on any click in GPUI, and when the browser is hidden
//!   or closed.

use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2::MainThreadOnly;
use objc2_app_kit::{NSAutoresizingMaskOptions, NSView};
use objc2_foundation::{NSPoint, NSRect, NSSize};

/// See the module docs.
pub struct Host {
    /// Ours: the web view, and a docked inspector, live in it.
    container: Retained<NSView>,
    /// GPUI's view, which the container sits in and the keyboard goes back to.
    parent: Retained<NSView>,
}

impl Host {
    /// Move `webview` (a child of GPUI's view, as wry builds it) into a
    /// container of its own, hidden until shown. `None` off the main thread or
    /// for a web view with no parent.
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
        Some(Host { container, parent })
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
            self.release_keyboard();
        }
        self.container.setHidden(hidden);
    }

    /// Give the keyboard back to GPUI if the page or its inspector has it.
    pub fn release_keyboard(&self) {
        let Some(window) = self.container.window() else {
            return;
        };
        let Some(responder) = window.firstResponder() else {
            return;
        };
        let ours = responder
            .downcast_ref::<NSView>()
            .is_some_and(|view| view.isDescendantOf(&self.container));
        if ours {
            window.makeFirstResponder(Some(&self.parent));
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        // A first responder taken out of the window leaves the window itself
        // as first responder, and GPUI would get no keys.
        self.release_keyboard();
        self.container.removeFromSuperview();
    }
}
