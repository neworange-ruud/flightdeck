//! `browser_screenshot` on macOS (`specs/SESSION_BROWSER_PRD.md` §6): the
//! engine's own capture, `-[WKWebView takeSnapshotWithConfiguration:
//! completionHandler:]`, encoded as PNG. wry exposes no capture, so this
//! talks to WebKit directly.
//!
//! The snapshot is taken at one pixel per point (the snapshot width is the
//! view's width divided by the screen's scale), so a Retina screen does not
//! hand the agent an image four times the size it needs.

use std::cell::Cell;
use std::ffi::c_void;

use block2::RcBlock;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::NSRect;

/// `NSBitmapImageFileTypePNG`.
const PNG: usize = 4;

/// Take a snapshot of `webview` and hand `done` its PNG bytes, or why there
/// are none. Called on the main thread; `done` is too, later.
pub fn capture(webview: &wry::WebView, done: impl FnOnce(Result<Vec<u8>, String>) + 'static) {
    use wry::WebViewExtMacOS;
    let wk = webview.webview();
    let wk: *const AnyObject = objc2::rc::Retained::as_ptr(&wk).cast();
    let done = Cell::new(Some(done));
    // SAFETY: every message below is sent on the main thread to objects
    // that are alive for the call (the web view is retained by wry; the
    // configuration and the block by WebKit once handed over), with the
    // argument and return types AppKit and WebKit declare.
    unsafe {
        let bounds: NSRect = msg_send![wk, bounds];
        let window: *mut AnyObject = msg_send![wk, window];
        let scale: f64 = if window.is_null() {
            1.0
        } else {
            msg_send![window, backingScaleFactor]
        };
        let config: *mut AnyObject = msg_send![class!(WKSnapshotConfiguration), new];
        if !config.is_null() && scale > 0.0 && bounds.size.width > 0.0 {
            let width: *mut AnyObject =
                msg_send![class!(NSNumber), numberWithDouble: bounds.size.width / scale];
            let _: () = msg_send![config, setSnapshotWidth: width];
        }
        let handler = RcBlock::new(move |image: *mut AnyObject, _error: *mut AnyObject| {
            let Some(done) = done.take() else { return };
            done(png_of(image));
        });
        let _: () =
            msg_send![wk, takeSnapshotWithConfiguration: config, completionHandler: &*handler];
        if !config.is_null() {
            let _: () = msg_send![config, release];
        }
    }
}

/// `image` (an `NSImage`, maybe null) as PNG bytes.
fn png_of(image: *mut AnyObject) -> Result<Vec<u8>, String> {
    const FAILED: &str = "The browser could not take a screenshot of this page.";
    if image.is_null() {
        return Err(FAILED.to_string());
    }
    // SAFETY: on the main thread (WebKit calls its completion handler
    // there), with the image WebKit handed over for the duration of the
    // call; every object read here is autoreleased and outlives this call.
    unsafe {
        let tiff: *mut AnyObject = msg_send![image, TIFFRepresentation];
        if tiff.is_null() {
            return Err(FAILED.to_string());
        }
        let rep: *mut AnyObject = msg_send![class!(NSBitmapImageRep), imageRepWithData: tiff];
        if rep.is_null() {
            return Err(FAILED.to_string());
        }
        let properties: *mut AnyObject = msg_send![class!(NSDictionary), dictionary];
        let png: *mut AnyObject =
            msg_send![rep, representationUsingType: PNG, properties: properties];
        if png.is_null() {
            return Err(FAILED.to_string());
        }
        let len: usize = msg_send![png, length];
        let bytes: *const c_void = msg_send![png, bytes];
        if bytes.is_null() || len == 0 {
            return Err(FAILED.to_string());
        }
        Ok(std::slice::from_raw_parts(bytes.cast::<u8>(), len).to_vec())
    }
}
