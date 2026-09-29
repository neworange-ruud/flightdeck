//! The desktop's overlay views: pure render functions of a `flightdeck::host`
//! view model that answer through [`Emit`].
//!
//! (Minimal on purpose: the overlay dispatcher that owns the rest of this file
//! is merged over it.)

use std::rc::Rc;

use flightdeck::host::HostEvent;
use gpui::{App, Window};

pub mod about;
pub mod config;
pub mod help;
pub mod remote;
pub mod update;

/// How a view hands the host an answer. A view never touches the host: it calls
/// this with the event a click or key means, and the caller applies it with
/// `AppHost::handle`.
pub type Emit = Rc<dyn Fn(HostEvent, &mut Window, &mut App)>;
