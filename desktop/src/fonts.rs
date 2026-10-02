//! The bundled typefaces: Geist for the UI, Geist Mono for terminals, keycaps
//! and meta text (design brief, "Fonts").
//!
//! Both are embedded in the binary (`include_bytes!`) and registered with
//! GPUI's text system at start-up, so the app looks the same on a machine that
//! has never installed them, on every OS. Source: the official
//! `vercel/geist-font` release v1.7.2 (`geist-font-v1.7.2.zip`, the static
//! `ttf/` cuts), under the SIL Open Font License 1.1 — see
//! `desktop/assets/fonts/OFL.txt` and desktop/NOTES-M2.md.
//!
//! Only the weights the design uses are shipped: Geist 400/500/600, and Geist
//! Mono 400/500/700 plus the two italics a terminal program can ask for
//! (`ESC[3m`). A weight that is not bundled is synthesised by the platform
//! from the nearest one.

use std::borrow::Cow;

use gpui::App;

/// The UI family name, as the font files declare it.
pub const UI_FAMILY: &str = "Geist";
/// The monospace family name, as the font files declare it.
pub const MONO_FAMILY: &str = "Geist Mono";

/// Every bundled font file, embedded.
pub const FONTS: &[(&str, &[u8])] = &[
    (
        "Geist-Regular.ttf",
        include_bytes!("../assets/fonts/Geist-Regular.ttf"),
    ),
    (
        "Geist-Medium.ttf",
        include_bytes!("../assets/fonts/Geist-Medium.ttf"),
    ),
    (
        "Geist-SemiBold.ttf",
        include_bytes!("../assets/fonts/Geist-SemiBold.ttf"),
    ),
    (
        "GeistMono-Regular.ttf",
        include_bytes!("../assets/fonts/GeistMono-Regular.ttf"),
    ),
    (
        "GeistMono-Medium.ttf",
        include_bytes!("../assets/fonts/GeistMono-Medium.ttf"),
    ),
    (
        "GeistMono-Bold.ttf",
        include_bytes!("../assets/fonts/GeistMono-Bold.ttf"),
    ),
    (
        "GeistMono-Italic.ttf",
        include_bytes!("../assets/fonts/GeistMono-Italic.ttf"),
    ),
    (
        "GeistMono-BoldItalic.ttf",
        include_bytes!("../assets/fonts/GeistMono-BoldItalic.ttf"),
    ),
];

/// Register the bundled fonts with GPUI. Call once at start-up, before the
/// first window draws. A failure is not fatal — text falls back to the
/// platform's fonts — so it is returned for the caller to log.
pub fn register(cx: &App) -> anyhow::Result<()> {
    cx.text_system().add_fonts(
        FONTS
            .iter()
            .map(|(_, bytes)| Cow::Borrowed(*bytes))
            .collect(),
    )
}
