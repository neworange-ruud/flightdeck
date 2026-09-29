//! The app's embedded assets: the monochrome SVG icons the views draw with
//! `svg().path(..)`. GPUI tints an SVG with the element's text colour, so every
//! icon's colour still comes from a theme token; the files only carry shape.
//!
//! Status icons differ in SHAPE, not only colour (design brief; WCAG 1.4.1):
//! an arc (working, spun), a filled triangle (needs you), a hollow ring
//! (idle), a check (done) and a cross (error).

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

/// Icon paths, as `svg().path(..)` names them.
pub mod icon {
    pub const STATUS_WORKING: &str = "icons/status-working.svg";
    pub const STATUS_ATTENTION: &str = "icons/status-attention.svg";
    pub const STATUS_IDLE: &str = "icons/status-idle.svg";
    pub const STATUS_DONE: &str = "icons/status-done.svg";
    pub const STATUS_ERROR: &str = "icons/status-error.svg";
    pub const PLUS: &str = "icons/plus.svg";
    pub const TERMINAL: &str = "icons/terminal.svg";
    pub const CLOSE: &str = "icons/close.svg";
    pub const SEARCH: &str = "icons/search.svg";
    pub const PROJECTS: &str = "icons/projects.svg";
    pub const MISSION: &str = "icons/mission.svg";
}

/// Every embedded file, by path.
const FILES: &[(&str, &[u8])] = &[
    (
        icon::STATUS_WORKING,
        include_bytes!("../assets/icons/status-working.svg"),
    ),
    (
        icon::STATUS_ATTENTION,
        include_bytes!("../assets/icons/status-attention.svg"),
    ),
    (
        icon::STATUS_IDLE,
        include_bytes!("../assets/icons/status-idle.svg"),
    ),
    (
        icon::STATUS_DONE,
        include_bytes!("../assets/icons/status-done.svg"),
    ),
    (
        icon::STATUS_ERROR,
        include_bytes!("../assets/icons/status-error.svg"),
    ),
    (icon::PLUS, include_bytes!("../assets/icons/plus.svg")),
    (
        icon::TERMINAL,
        include_bytes!("../assets/icons/terminal.svg"),
    ),
    (icon::CLOSE, include_bytes!("../assets/icons/close.svg")),
    (icon::SEARCH, include_bytes!("../assets/icons/search.svg")),
    (
        icon::PROJECTS,
        include_bytes!("../assets/icons/projects.svg"),
    ),
    (icon::MISSION, include_bytes!("../assets/icons/mission.svg")),
];

/// The asset source handed to `Application::with_assets`.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        Ok(FILES
            .iter()
            .find(|(p, _)| *p == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(FILES
            .iter()
            .filter(|(p, _)| p.starts_with(path))
            .map(|(p, _)| SharedString::from(*p))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_path_resolves_to_an_svg() {
        for (path, _) in FILES {
            let bytes = Assets.load(path).unwrap().expect("embedded");
            let text = std::str::from_utf8(&bytes).unwrap();
            assert!(text.starts_with("<svg"), "{path}");
        }
        assert!(Assets.load("icons/missing.svg").unwrap().is_none());
    }
}
