//! The window's regions, one module each, drawn from the shared view models
//! (`flightdeck::view`) the host hands back. Colours come only from
//! [`crate::theme::Palette`]; controls act only through [`crate::commands`].

pub mod git_strip;
pub mod icons;
pub mod sidebar;
pub mod status_bar;
pub mod titlebar;
