//! The launcher's Workers section: a background link to every saved worker,
//! its row, and the notifications its status edges raise
//! (`specs/WORKER_PRD.md` §4.5).
//!
//! Which worker windows are open lives in the library, as
//! [`flightdeck_desktop::workers::registry`], so the remote window and this
//! module agree on one global without either depending on the other.

pub mod links;
pub mod view;

#[cfg(test)]
mod tests;
