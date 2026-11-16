# gpui-pre-windows 0.3.7, patched

A copy of [`gpui-pre-windows`](https://crates.io/crates/gpui-pre-windows) 0.3.7,
GPUI's Windows platform, as published on crates.io. The root `Cargo.toml` uses
it through `[patch.crates-io]`.

## The change

One line, in `src/directx_renderer.rs`, `DirectComposition::new`:

```diff
-        let comp_target = unsafe { comp_device.CreateTargetForHwnd(hwnd, true) }?;
+        let comp_target = unsafe { comp_device.CreateTargetForHwnd(hwnd, false) }?;
```

## Why

GPUI draws a window through a DirectComposition target. A **topmost** target
is composed over the window's child windows, so a child window, such as
the session browser's WebView2 (`desktop/src/browser/windows.rs`), loads its
page but never shows. Not topmost, child windows draw over GPUI, which is
how the web view sits on macOS: FlightDeck hides it whenever a GPUI overlay
covers its area. GPUI itself has no child windows, so nothing else changes.

## Keeping it

- On a GPUI upgrade, copy the new version of the crate here and make the same
  change, or drop the patch once GPUI lets a window choose (worth proposing
  upstream).
- The crate is excluded from the workspace (`[workspace] exclude`), so our
  `fmt` and `clippy` leave it as published.
- Tracked in `remote-control-cefv.12.2`.
