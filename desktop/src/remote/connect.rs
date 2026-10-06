//! "Connect to remote…": pair with a FlightDeck on another machine, or
//! reconnect to one paired before (`specs/DESKTOP_REMOTE_CONTROL_PLAN.md`
//! §2.4, M3; decisions R1, R3–R6).
//!
//! A small window of its own, opened from the launcher and the File menu:
//!
//! - **Pair:** the host's address (`host:port`, port 7420 by default) and the
//!   4-digit code its web access overlay shows in network mode (R3). The code
//!   is exchanged for a token, which is saved per remote in
//!   `~/.flightdeck/remotes.json` (R6), so the next connect needs no code.
//! - **Seat:** Control (R5's default — attach as a writer, the input lock keeps
//!   two people from typing over each other) or Observe.
//! - **Saved remotes:** connect again, or forget one.
//! - **The plain-WebSocket warning**, once per remote, unless the address is
//!   loopback or Tailscale (plan §4). Nothing connects on launch by itself:
//!   every connect is a click here.
//!
//! Each connect opens its own remote window (R4: one remote per window, never
//! mixed with local projects).

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::rc::Rc;

use flightdeck::contracts::FileSystem;
use flightdeck::web::client::store::{load_remotes, save_remotes, RemotesFile, SavedRemote};
use flightdeck::web::client::{
    exchange_code, link_is_unprotected, normalize_address, AccessToken, ExchangeError,
};
use flightdeck::web::protocol::SeatRequest;
use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, App, AppContext, Context, Entity, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, StatefulInteractiveElement, Styled, Window,
};
use gpui_component::input::{Input, InputState};
use gpui_component::{h_flex, v_flex};

use crate::fonts::MONO_FAMILY;
use crate::theme::Palette;

/// Trades a code for a token: `(address, code, label)`. The real one runs
/// [`exchange_code`] on the shared runtime; tests hand in their own.
pub type Exchange = Rc<
    dyn Fn(
        String,
        String,
        String,
    ) -> Pin<Box<dyn Future<Output = Result<AccessToken, ExchangeError>>>>,
>;

/// Opens a remote window for a saved remote and a seat.
pub type OpenRemote = Rc<dyn Fn(SavedRemote, SeatRequest, &mut App)>;

/// Where the saved remotes live.
#[derive(Clone)]
pub struct Store {
    pub fs: Rc<dyn FileSystem>,
    pub path: Option<PathBuf>,
}

impl Store {
    /// The real file, `~/.flightdeck/remotes.json`.
    pub fn real() -> Store {
        Store {
            fs: Rc::new(flightdeck::contracts::real::RealFs),
            path: flightdeck::web::client::store::remotes_path(),
        }
    }

    pub fn load(&self) -> RemotesFile {
        match &self.path {
            Some(path) => load_remotes(self.fs.as_ref(), path),
            None => RemotesFile::default(),
        }
    }

    pub fn save(&self, file: &RemotesFile) {
        if let Some(path) = &self.path {
            if let Err(e) = save_remotes(self.fs.as_ref(), path, file) {
                eprintln!("flightdeck-desktop: could not save remotes: {e}");
            }
        }
    }
}

/// The real exchange: [`exchange_code`] on the shared tokio runtime.
pub fn real_exchange() -> Exchange {
    Rc::new(|address, code, label| {
        Box::pin(async move {
            let Some(runtime) = flightdeck::remote::runtime::try_shared() else {
                return Err(ExchangeError::Unreachable(
                    "the async runtime could not start".to_string(),
                ));
            };
            match runtime
                .spawn(async move { exchange_code(&address, &code, &label).await })
                .await
            {
                Ok(result) => result,
                Err(e) => Err(ExchangeError::Unreachable(e.to_string())),
            }
        })
    })
}

/// The label a new remote gets: its host name or address, without the port.
pub fn host_label(address: &str) -> String {
    match address.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(rest).to_string(),
        None => address
            .rsplit_once(':')
            .map_or(address, |(host, _)| host)
            .to_string(),
    }
}

/// What a connect is waiting on the plain-WebSocket warning for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Pending {
    Pair { address: String, code: String },
    Saved { address: String },
}

pub struct ConnectView {
    address: Entity<InputState>,
    code: Entity<InputState>,
    seat: SeatRequest,
    store: Store,
    saved: RemotesFile,
    busy: bool,
    error: Option<String>,
    warning: Option<Pending>,
    exchange: Exchange,
    open: OpenRemote,
    /// Close the window on the next frame (a remote window opened).
    done: bool,
    /// The label this app gives itself to the host.
    client_label: String,
}

impl ConnectView {
    pub fn new(
        prefill: Option<String>,
        store: Store,
        exchange: Exchange,
        open: OpenRemote,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let address = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("192.168.2.20 or studio.local:7420")
                .default_value(prefill.unwrap_or_default())
        });
        let code = cx.new(|cx| InputState::new(window, cx).placeholder("1234"));
        let saved = store.load();
        Self {
            address,
            code,
            seat: SeatRequest::Write,
            store,
            saved,
            busy: false,
            error: None,
            warning: None,
            exchange,
            open,
            done: false,
            client_label: flightdeck::web::client::user_agent(
                flightdeck::web::server::NATIVE_CLIENT_AGENT,
                env!("CARGO_PKG_VERSION"),
            ),
        }
    }

    /// The saved remotes.
    #[cfg(test)]
    pub fn saved(&self) -> &RemotesFile {
        &self.saved
    }

    #[cfg(test)]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    #[cfg(test)]
    pub fn warning_open(&self) -> bool {
        self.warning.is_some()
    }

    /// Type into both fields, as a person would (tests).
    #[cfg(test)]
    pub fn fill(&mut self, address: &str, code: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.address
            .update(cx, |s, cx| s.set_value(address.to_string(), window, cx));
        self.code
            .update(cx, |s, cx| s.set_value(code.to_string(), window, cx));
    }

    pub fn set_seat(&mut self, seat: SeatRequest, cx: &mut Context<Self>) {
        self.seat = seat;
        cx.notify();
    }

    fn needs_warning(&self, address: &str) -> bool {
        link_is_unprotected(address)
            && !self
                .saved
                .get(address)
                .is_some_and(|r| r.warned_unencrypted)
    }

    /// Pair with the address and code typed in.
    pub fn pair(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let raw = self.address.read(cx).value().to_string();
        let Some(address) = normalize_address(&raw) else {
            self.error = Some(
                "Type the host's address, for example 192.168.2.20 or studio.local:7420."
                    .to_string(),
            );
            cx.notify();
            return;
        };
        let code: String = self
            .code
            .read(cx)
            .value()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        if code.len() != 4 || !code.chars().all(|c| c.is_ascii_digit()) {
            self.error =
                Some("Type the 4-digit code the host's web access overlay shows.".to_string());
            cx.notify();
            return;
        }
        let pending = Pending::Pair { address, code };
        self.go(pending, window, cx);
    }

    /// Connect to a saved remote with its stored token.
    pub fn connect_saved(&mut self, address: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.go(
            Pending::Saved {
                address: address.to_string(),
            },
            window,
            cx,
        );
    }

    /// Forget a saved remote and its token.
    pub fn forget(&mut self, address: &str, cx: &mut Context<Self>) {
        if self.saved.forget(address) {
            self.store.save(&self.saved);
        }
        cx.notify();
    }

    /// The warning was read: carry on with what was waiting for it.
    pub fn accept_warning(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.warning.take() else {
            return;
        };
        if let Pending::Saved { address } = &pending {
            if let Some(remote) = self
                .saved
                .remotes
                .iter_mut()
                .find(|r| &r.address == address)
            {
                remote.warned_unencrypted = true;
            }
            self.store.save(&self.saved);
        }
        self.proceed(pending, window, cx);
    }

    pub fn decline_warning(&mut self, cx: &mut Context<Self>) {
        self.warning = None;
        cx.notify();
    }

    fn go(&mut self, pending: Pending, window: &mut Window, cx: &mut Context<Self>) {
        let address = match &pending {
            Pending::Pair { address, .. } | Pending::Saved { address } => address.clone(),
        };
        if self.needs_warning(&address) {
            self.warning = Some(pending);
            cx.notify();
            return;
        }
        self.proceed(pending, window, cx);
    }

    fn proceed(&mut self, pending: Pending, window: &mut Window, cx: &mut Context<Self>) {
        self.error = None;
        match pending {
            Pending::Saved { address } => {
                let Some(remote) = self.saved.get(&address).cloned() else {
                    self.error = Some("That remote is no longer saved.".to_string());
                    cx.notify();
                    return;
                };
                (self.open)(remote, self.seat, cx);
                self.done = true;
                cx.notify();
            }
            Pending::Pair { address, code } => {
                self.busy = true;
                cx.notify();
                let exchange = (self.exchange)(address.clone(), code, self.client_label.clone());
                cx.spawn_in(window, async move |this, cx| {
                    let result = exchange.await;
                    let _ = this.update_in(cx, |view, _, cx| view.exchanged(address, result, cx));
                })
                .detach();
            }
        }
    }

    fn exchanged(
        &mut self,
        address: String,
        result: Result<AccessToken, ExchangeError>,
        cx: &mut Context<Self>,
    ) {
        self.busy = false;
        match result {
            Ok(token) => {
                let remote = SavedRemote {
                    label: host_label(&address),
                    address,
                    token: token.reveal().to_string(),
                    last_seen: None,
                    viewer_id: None,
                    host_version: None,
                    // Either the link needs no warning, or it was just read.
                    warned_unencrypted: true,
                    ssh_target: None,
                };
                self.saved.upsert(remote.clone());
                self.store.save(&self.saved);
                (self.open)(remote, self.seat, cx);
                self.done = true;
            }
            Err(e) => self.error = Some(e.sentence()),
        }
        cx.notify();
    }
}

impl Render for ConnectView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.done {
            window.remove_window();
        }
        let p = *Palette::global(cx);
        let this = cx.entity();

        let seat_choice = |id: &'static str, label: &'static str, seat: SeatRequest| {
            let chosen = self.seat == seat;
            let this = this.clone();
            h_flex()
                .id(id)
                .debug_selector(move || id.to_string())
                .gap_2()
                .px_3()
                .py_1()
                .rounded(px(6.))
                .border_1()
                .border_color(if chosen { p.accent } else { p.border }.hsla())
                .when(chosen, |d| d.bg(p.surface_raised.hsla()))
                .cursor_pointer()
                .text_size(px(12.5))
                .child(label)
                .on_click(move |_, _, cx| this.update(cx, |view, cx| view.set_seat(seat, cx)))
        };

        let pair_button = {
            let this = this.clone();
            div()
                .id("connect-pair")
                .debug_selector(|| "connect-pair".into())
                .flex_none()
                .h(px(32.))
                .px_4()
                .flex()
                .items_center()
                .rounded(px(7.))
                .bg(p.button_primary_bg.hsla())
                .text_color(p.button_primary_ink.hsla())
                .text_size(px(13.))
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .child(if self.busy { "Pairing…" } else { "Connect" })
                .on_click(move |_, window, cx| this.update(cx, |view, cx| view.pair(window, cx)))
        };

        let saved = self.saved.by_recent();
        let saved_list = (!saved.is_empty()).then(|| {
            v_flex()
                .w_full()
                .gap(px(2.))
                .child(
                    div()
                        .pb_1()
                        .text_size(px(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.muted.hsla())
                        .child("SAVED REMOTES"),
                )
                .children(saved.into_iter().enumerate().map(|(i, remote)| {
                    let connect = {
                        let this = this.clone();
                        let address = remote.address.clone();
                        div()
                            .id(("connect-saved", i))
                            .debug_selector(move || format!("connect-saved-{i}"))
                            .px_2()
                            .py(px(2.))
                            .rounded(px(5.))
                            .border_1()
                            .border_color(p.border.hsla())
                            .cursor_pointer()
                            .child("Connect")
                            .on_click(move |_, window, cx| {
                                this.update(cx, |view, cx| view.connect_saved(&address, window, cx))
                            })
                    };
                    let forget = {
                        let this = this.clone();
                        let address = remote.address.clone();
                        div()
                            .id(("connect-forget", i))
                            .debug_selector(move || format!("connect-forget-{i}"))
                            .px_2()
                            .py(px(2.))
                            .rounded(px(5.))
                            .text_color(p.muted.hsla())
                            .cursor_pointer()
                            .child("Forget")
                            .on_click(move |_, _, cx| {
                                this.update(cx, |view, cx| view.forget(&address, cx))
                            })
                    };
                    h_flex()
                        .w_full()
                        .gap_3()
                        .px_2()
                        .py_1()
                        .rounded(px(6.))
                        .text_size(px(12.5))
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .child(remote.label.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(MONO_FAMILY)
                                .text_size(px(11.5))
                                .text_color(p.muted.hsla())
                                .child(remote.address.clone()),
                        )
                        .child(connect)
                        .child(forget)
                }))
        });

        let warning = self.warning.as_ref().map(|pending| {
            let address = match pending {
                Pending::Pair { address, .. } | Pending::Saved { address } => address.clone(),
            };
            let accept = {
                let this = this.clone();
                div()
                    .id("connect-warning-accept")
                    .debug_selector(|| "connect-warning-accept".into())
                    .px_3()
                    .py_1()
                    .rounded(px(6.))
                    .bg(p.danger_bg.hsla())
                    .text_color(p.danger.hsla())
                    .cursor_pointer()
                    .child("Connect anyway")
                    .on_click(move |_, window, cx| {
                        this.update(cx, |view, cx| view.accept_warning(window, cx))
                    })
            };
            let back = {
                let this = this.clone();
                div()
                    .id("connect-warning-back")
                    .debug_selector(|| "connect-warning-back".into())
                    .px_3()
                    .py_1()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(p.border.hsla())
                    .cursor_pointer()
                    .child("Back")
                    .on_click(move |_, _, cx| this.update(cx, |view, cx| view.decline_warning(cx)))
            };
            v_flex()
                .debug_selector(|| "connect-warning".into())
                .w_full()
                .gap_2()
                .p_3()
                .rounded(px(8.))
                .border_1()
                .border_color(p.status_attention.hsla())
                .text_size(px(12.5))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format!("The link to {address} is not encrypted.")),
                )
                .child(div().text_color(p.ink_2.hsla()).child(
                    "FlightDeck Web speaks plain WebSocket. Anyone on this network can read \
                     the terminals and the access token. Use it on a network you trust, or \
                     over Tailscale or WireGuard. You are asked once per remote.",
                ))
                .child(h_flex().gap_2().child(accept).child(back))
        });

        let error = self.error.clone().map(|message| {
            div()
                .debug_selector(|| "connect-error".into())
                .text_size(px(12.))
                .text_color(p.danger.hsla())
                .child(message)
        });

        v_flex()
            .id("connect")
            .debug_selector(|| "connect".into())
            .size_full()
            .p_6()
            .pt(px(44.))
            .gap_3()
            .bg(p.surface_window.hsla())
            .text_color(p.ink.hsla())
            .font_family(crate::fonts::UI_FAMILY)
            .child(
                div()
                    .text_size(px(18.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Connect to another FlightDeck"),
            )
            .child(div().text_size(px(12.5)).text_color(p.ink_2.hsla()).child(
                "On the other machine, start the web interface, switch its access overlay \
                 to network mode (n), and read the address and code it shows.",
            ))
            .child(field(&p, "Address", Input::new(&self.address)))
            .child(field(&p, "Code", Input::new(&self.code)))
            .child(
                h_flex()
                    .gap_2()
                    .text_size(px(12.))
                    .text_color(p.muted.hsla())
                    .child("Seat")
                    .child(seat_choice(
                        "connect-seat-control",
                        "Control",
                        SeatRequest::Write,
                    ))
                    .child(seat_choice(
                        "connect-seat-observe",
                        "Observe",
                        SeatRequest::Observe,
                    )),
            )
            .children(warning)
            .child(h_flex().gap_3().child(pair_button).children(error))
            .children(saved_list.map(|list| div().mt_3().w_full().child(list)))
    }
}

fn field(p: &Palette, label: &'static str, input: Input) -> impl IntoElement {
    v_flex()
        .w_full()
        .gap_1()
        .child(
            div()
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.muted.hsla())
                .child(label),
        )
        .child(input)
}

/// Open the connect window, optionally with an address filled in (Pair
/// again, from a remote window whose access was withdrawn).
pub fn open_connect_window(prefill: Option<String>, cx: &mut App) {
    let options = connect_window_options(cx);
    let open: OpenRemote = Rc::new(super::open_remote_window);
    let opened = cx.open_window(options, |window, cx| {
        let view = cx
            .new(|cx| ConnectView::new(prefill, Store::real(), real_exchange(), open, window, cx));
        cx.new(|cx| gpui_component::Root::new(view, window, cx))
    });
    if let Err(e) = opened {
        eprintln!("flightdeck-desktop: could not open the connect window: {e}");
    }
}

fn connect_window_options(cx: &App) -> gpui::WindowOptions {
    let mut options = crate::app::window_options(cx);
    options.window_bounds = Some(gpui::WindowBounds::Windowed(gpui::Bounds::centered(
        None,
        gpui::size(px(560.), px(620.)),
        cx,
    )));
    options.window_min_size = Some(gpui::size(px(460.), px(460.)));
    if let Some(titlebar) = options.titlebar.as_mut() {
        titlebar.title = Some("Connect to remote".into());
    }
    options
}
