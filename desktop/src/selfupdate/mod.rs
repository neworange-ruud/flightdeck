//! Self-update for the desktop app (beads `remote-control-bmej.6.5`).
//!
//! The rule that shapes everything here is the TUI's (SPECS §29/§30): a copy a
//! package manager owns is never replaced behind its back. The banner tells
//! such users which command to run; only a copy the user unpacked themselves
//! gets "Update now".
//!
//! ```text
//! start_check ──► check::newer_version      once a day, cached, silent on failure
//!                       │ newer
//!                       ▼
//!                 kind::detect              where do we run from?
//!                       │
//!        ┌──────────────┼───────────────┐
//!     Managed        Unsupported       Direct
//!  "brew upgrade…"  "see releases"   "Update now" ──► begin_install
//!                                                        │
//!                              release::select ◄─────────┤  asset + .sha256 by name
//!                              apply::Installer          │  download, verify, swap
//!                                                        ▼
//!                                              "Restart to use vX"
//! ```
//!
//! - [`kind`]: install-kind detection from paths (pure, behind `FileSystem`).
//! - [`release`]: versions, the asset naming contract, checksums, the GitHub
//!   response parser (pure).
//! - [`source`]: `ReleaseSource` and `Unzip`, the only code that touches the
//!   network or spawns a tool; the real ones use `curl`, `ditto`, `tar`.
//! - [`apply`]: staging layout, verification and the swap, over `UpdateFs`.
//! - [`check`]: the once-a-day cache and decision.
//! - [`state`]: the banner's status (a GPUI global) and the two entry points.
//!
//! The CLI's `flightdeck update` (axoupdater) is unchanged; this module never
//! touches it.

pub mod apply;
pub mod check;
pub mod kind;
pub mod release;
pub mod source;
pub mod state;

pub use apply::Outcome;
pub use kind::{InstallKind, PackageManager};
pub use state::{
    begin_install, finish_staged_update_at_launch, set_status, start_check, status, UpdateStatus,
};
