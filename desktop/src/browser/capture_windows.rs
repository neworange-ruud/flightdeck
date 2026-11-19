//! `browser_screenshot` on Windows (`specs/SESSION_BROWSER_PRD.md` §6): the
//! engine's own capture, `ICoreWebView2::CapturePreview`, as PNG into a
//! memory stream. wry exposes no capture, so this talks to WebView2
//! directly.

use std::cell::Cell;

use webview2_com::CapturePreviewCompletedHandler;
use webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG;
use windows::Win32::System::Com::{IStream, STATFLAG_NONAME, STATSTG, STREAM_SEEK_SET};
use windows::Win32::UI::Shell::SHCreateMemStream;

const FAILED: &str = "The browser could not take a screenshot of this page.";

/// Take a snapshot of `webview` and hand `done` its PNG bytes, or why there
/// are none. Called on the main thread; `done` is too, later.
pub fn capture(webview: &wry::WebView, done: impl FnOnce(Result<Vec<u8>, String>) + 'static) {
    use wry::WebViewExtWindows;
    let core = webview.webview();
    // SAFETY: a fresh, empty memory stream; no borrowed data.
    let Some(stream) = (unsafe { SHCreateMemStream(None) }) else {
        done(Err(FAILED.to_string()));
        return;
    };
    let done = std::rc::Rc::new(Cell::new(Some(done)));
    let read = stream.clone();
    let finish = std::rc::Rc::clone(&done);
    let handler = CapturePreviewCompletedHandler::create(Box::new(move |result| {
        if let Some(done) = finish.take() {
            done(
                result
                    .map_err(|_| FAILED.to_string())
                    .and_then(|()| bytes_of(&read)),
            );
        }
        Ok(())
    }));
    // SAFETY: on the main thread, WebView2's own; the stream and the
    // handler are reference-counted COM objects WebView2 holds until it
    // calls back.
    let started = unsafe {
        core.CapturePreview(
            COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG,
            &stream,
            &handler,
        )
    };
    if started.is_err() {
        if let Some(done) = done.take() {
            done(Err(FAILED.to_string()));
        }
    }
}

/// Everything written to `stream`.
fn bytes_of(stream: &IStream) -> Result<Vec<u8>, String> {
    // SAFETY: a memory stream WebView2 finished writing; `buf` is as long
    // as the read asks for.
    unsafe {
        let mut stat = STATSTG::default();
        stream
            .Stat(&mut stat, STATFLAG_NONAME)
            .map_err(|_| FAILED.to_string())?;
        let len = usize::try_from(stat.cbSize).map_err(|_| FAILED.to_string())?;
        if len == 0 {
            return Err(FAILED.to_string());
        }
        stream
            .Seek(0, STREAM_SEEK_SET, None)
            .map_err(|_| FAILED.to_string())?;
        let mut buf = vec![0u8; len];
        let mut read = 0u32;
        stream
            .Read(buf.as_mut_ptr().cast(), len as u32, Some(&mut read))
            .ok()
            .map_err(|_| FAILED.to_string())?;
        buf.truncate(read as usize);
        Ok(buf)
    }
}
