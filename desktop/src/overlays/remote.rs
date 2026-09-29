//! The remote surfaces: the web-access overlay, phone pairing, the actions that
//! start and stop them, and the status-bar indicator.
//!
//! **QRs are drawn natively.** The TUI shows half-block art; here the host's
//! own payload (`WebAccessOverlay::qr_payload`, `PairingView::qr_payload`, the
//! `fdr1:` string) is encoded with the `qrcode` crate and painted as GPUI
//! quads, one per horizontal run of dark modules, on a light card with the
//! four-module quiet zone scanners need. Dark-on-light is deliberate: many
//! scanners do not read inverted codes, so the QR keeps its own two colours
//! instead of following the dark theme.
//!
//! Everything else is a plain render of the host's view model, answering with
//! the events the TUI's own keys produce: [`AccessKey`]s for the access overlay
//! (the host applies them through the same handler as a keypress, so a
//! credential decision stays the credential store's), `Cancel` for Esc, and
//! [`PaletteAction`]s for the workspace-level actions.

use flightdeck::host::{HostEvent, OverlayInput, PairingView, RemoteStatus, WebAccessOverlay};
use flightdeck::tui::palette::PaletteAction;
use flightdeck::web::access::{AccessKey, AccessMode, BrowserRow};
use gpui::{
    canvas, div, fill, point, px, size, App, Bounds, Div, FontWeight, InteractiveElement,
    IntoElement, ParentElement, Pixels, Point, SharedString, StatefulInteractiveElement, Styled,
};
use gpui_component::{h_flex, v_flex};
use qrcode::{Color, QrCode};

use super::help::{button, card, danger, section_label, ButtonKind, MONO};
use super::Emit;
use crate::theme::Palette;

/// Modules of blank border around a QR; the spec asks for four.
const QUIET_ZONE: usize = 4;
/// Pixels per QR module. An integer so the quads land on whole pixels and
/// neighbouring runs cannot show a hairline seam between them.
const MODULE_PX: f32 = 5.;
const CARD_WIDTH: Pixels = px(560.);

// ---------------------------------------------------------------------------
// QR
// ---------------------------------------------------------------------------

/// A QR code's modules, row-major, `true` = dark. Only the symbol: the quiet
/// zone is added when drawing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrMatrix {
    /// Modules per side.
    pub width: usize,
    /// `width * width` modules.
    pub dark: Vec<bool>,
}

impl QrMatrix {
    /// Encode `payload`; `None` when it cannot be encoded (too long for any QR
    /// version), in which case the view draws no QR rather than a wrong one.
    pub fn new(payload: &str) -> Option<QrMatrix> {
        let code = QrCode::new(payload.as_bytes()).ok()?;
        Some(QrMatrix {
            width: code.width(),
            dark: code
                .to_colors()
                .into_iter()
                .map(|c| c == Color::Dark)
                .collect(),
        })
    }

    /// The dark modules as horizontal runs `(x, y, length)` in module units,
    /// relative to the symbol's top-left. Fewer quads than one per module, and
    /// lossless: [`QrMatrix::from_runs`] inverts it.
    pub fn runs(&self) -> Vec<(usize, usize, usize)> {
        let mut runs = Vec::new();
        for y in 0..self.width {
            let mut x = 0;
            while x < self.width {
                if !self.dark[y * self.width + x] {
                    x += 1;
                    continue;
                }
                let start = x;
                while x < self.width && self.dark[y * self.width + x] {
                    x += 1;
                }
                runs.push((start, y, x - start));
            }
        }
        runs
    }

    /// Rebuild a matrix from [`QrMatrix::runs`]; used by the tests to prove the
    /// quads that are painted are exactly the code's modules.
    pub fn from_runs(width: usize, runs: &[(usize, usize, usize)]) -> QrMatrix {
        let mut dark = vec![false; width * width];
        for &(x, y, len) in runs {
            for module in &mut dark[y * width + x..y * width + x + len] {
                *module = true;
            }
        }
        QrMatrix { width, dark }
    }

    /// Side of the drawn square in pixels, quiet zone included.
    pub fn pixel_side(&self) -> f32 {
        (self.width + 2 * QUIET_ZONE) as f32 * MODULE_PX
    }
}

/// The QR as an element: a light square with the dark runs painted over it.
pub fn qr_element(matrix: QrMatrix, cx: &App) -> impl IntoElement {
    qr_canvas(matrix, *Palette::global(cx))
}

/// The quads to paint for the dark modules, given where the symbol's own
/// top-left (inside the quiet zone) is. The one place module coordinates become
/// pixels, so the tests read the same rectangles back into modules.
pub fn qr_run_bounds(matrix: &QrMatrix, symbol_origin: Point<Pixels>) -> Vec<Bounds<Pixels>> {
    matrix
        .runs()
        .into_iter()
        .map(|(x, y, len)| {
            Bounds::new(
                symbol_origin + point(px(x as f32 * MODULE_PX), px(y as f32 * MODULE_PX)),
                size(px(len as f32 * MODULE_PX), px(MODULE_PX)),
            )
        })
        .collect()
}

fn qr_canvas(matrix: QrMatrix, p: Palette) -> impl IntoElement {
    let side = matrix.pixel_side();
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            window.paint_quad(fill(bounds, p.ink.hsla()));
            let quiet = px(QUIET_ZONE as f32 * MODULE_PX);
            for run in qr_run_bounds(&matrix, bounds.origin + point(quiet, quiet)) {
                window.paint_quad(fill(run, p.surface_terminal.hsla()));
            }
        },
    )
    .size(px(side))
    .flex_shrink_0()
}

// ---------------------------------------------------------------------------
// Web access overlay
// ---------------------------------------------------------------------------

/// The web-access overlay (`specs/WEB_INTERFACE.md` D5, artboard `2a`).
pub fn web_access_view(overlay: &WebAccessOverlay, emit: Emit, cx: &App) -> impl IntoElement {
    let p = *Palette::global(cx);
    let view = &overlay.view;
    let key = |k: AccessKey| {
        let emit = emit.clone();
        move |_: &gpui::ClickEvent, w: &mut gpui::Window, cx: &mut App| {
            emit(HostEvent::Overlay(OverlayInput::WebAccess(k)), w, cx)
        }
    };

    let mut body = v_flex().gap_3();

    // What the current binding means: the host's own sentence, kept as the
    // first thing read (loopback is calm, routable is the D5 warning).
    let routable = view.mode == Some(AccessMode::Network);
    body = body.child(
        v_flex()
            .gap_0p5()
            .child(
                div()
                    .text_sm()
                    .text_color(
                        if routable {
                            p.status_attention
                        } else {
                            p.ink_2
                        }
                        .hsla(),
                    )
                    .child(view.exposure_line.clone()),
            )
            .child(
                div()
                    .text_xs()
                    .font_family(MONO)
                    .text_color(p.faint.hsla())
                    .child(format!("listening on {}", view.bound)),
            ),
    );

    body = body.child(
        v_flex().gap_1().child(section_label("Address", &p)).child(
            div()
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(p.border.hsla())
                .bg(p.surface_input.hsla())
                .font_family(MONO)
                .text_sm()
                .child(view.url.clone()),
        ),
    );

    let mut actions = h_flex().gap_2().flex_wrap();
    match view.mode {
        Some(AccessMode::LocalOnly) | None => {
            actions = actions
                .child(button(
                    "access-open",
                    "Open in browser",
                    ButtonKind::Primary,
                    &p,
                    key(AccessKey::Enter),
                ))
                .child(button(
                    "access-copy",
                    "Copy link",
                    ButtonKind::Secondary,
                    &p,
                    key(AccessKey::Char('c')),
                ))
                .child(button(
                    "access-network",
                    "Allow network access",
                    ButtonKind::Secondary,
                    &p,
                    key(AccessKey::Char('n')),
                ))
                .child(button(
                    "access-stop",
                    "Stop server",
                    ButtonKind::Quiet,
                    &p,
                    key(AccessKey::Char('s')),
                ));
        }
        Some(AccessMode::Network) => {
            body = body.child(address_picker(view, &emit, &p));
            body = body.child(credential_block(overlay, &p));
            actions = actions
                .child(button(
                    "access-newcode",
                    "New code",
                    ButtonKind::Primary,
                    &p,
                    key(AccessKey::Space),
                ))
                .child(button(
                    "access-hide",
                    if view.code_hidden {
                        "Show code"
                    } else {
                        "Hide code"
                    },
                    ButtonKind::Secondary,
                    &p,
                    key(AccessKey::Char('r')),
                ))
                .child(button(
                    "access-revoke-all",
                    "Revoke all",
                    ButtonKind::Secondary,
                    &p,
                    key(AccessKey::Char('x')),
                ))
                .child(button(
                    "access-local",
                    "Local only",
                    ButtonKind::Quiet,
                    &p,
                    key(AccessKey::Char('l')),
                ));
            body = body.child(browser_list(&view.browsers, &emit, &p));
        }
    }
    body = body.child(actions);

    if let Some(notice) = &view.notice {
        body = body.child(
            div()
                .text_xs()
                .text_color(p.muted.hsla())
                .child(notice.clone()),
        );
    }

    card("web-access-overlay", "Web interface", CARD_WIDTH, &p, &emit).child(body)
}

/// The picker of network addresses (State B). Clicking a row moves the host's
/// selection there with the same up/down keys it answers to.
fn address_picker(view: &flightdeck::web::access::WebAccessView, emit: &Emit, p: &Palette) -> Div {
    let selected = view.selected_address.unwrap_or(0);
    let mut rows = v_flex().gap_px();
    for (i, row) in view.addresses.iter().enumerate() {
        let active = view.selected_address == Some(i);
        let emit = emit.clone();
        rows = rows.child(
            h_flex()
                .id(SharedString::from(format!("access-address-{i}")))
                .debug_selector(|| format!("access-address-{i}"))
                .gap_3()
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .bg(if active {
                    p.surface_raised
                } else {
                    p.surface_window
                }
                .hsla())
                .on_click(move |_, w, cx| {
                    // The host moves one row per key, so walk to the row.
                    let (key, steps) = if i >= selected {
                        (AccessKey::Down, i - selected)
                    } else {
                        (AccessKey::Up, selected - i)
                    };
                    for _ in 0..steps {
                        emit(HostEvent::Overlay(OverlayInput::WebAccess(key)), w, cx);
                    }
                })
                .child(
                    div()
                        .w(px(20.))
                        .text_color(p.accent.hsla())
                        .child(if active { "●" } else { "" }),
                )
                .child(
                    div()
                        .font_family(MONO)
                        .text_sm()
                        .child(format!("{}  {}", row.name, row.address)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(p.faint.hsla())
                        .child(row.description.unwrap_or("").to_string()),
                ),
        );
    }
    v_flex()
        .gap_1()
        .child(section_label("Network address", p))
        .child(rows)
}

/// The QR and the code beside it, or why there is none.
fn credential_block(overlay: &WebAccessOverlay, p: &Palette) -> Div {
    let view = &overlay.view;
    if view.code_hidden {
        return placeholder("Code hidden. Show it to pair a browser.", p);
    }
    if view.code_expired {
        return placeholder("The code expired. Make a new one.", p);
    }
    let (Some(code), Some(payload)) = (&view.code, &overlay.qr_payload) else {
        return placeholder("No code on screen.", p);
    };
    h_flex()
        .gap_4()
        .items_center()
        .children(QrMatrix::new(payload).map(|m| qr_canvas(m, *p)))
        .child(
            v_flex()
                .gap_1()
                .child(section_label("Code", p))
                .child(
                    div()
                        .font_family(MONO)
                        .text_2xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(code.clone()),
                )
                .child(countdown(view.seconds_remaining.map(|s| s as i64), p)),
        )
}

fn placeholder(text: &str, p: &Palette) -> Div {
    div()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(p.border.hsla())
        .text_sm()
        .text_color(p.muted.hsla())
        .child(text.to_string())
}

/// `expires in 1:35`; empty when the host gave no countdown.
fn countdown_text(seconds: Option<i64>) -> String {
    match seconds {
        Some(s) if s >= 0 => format!("expires in {}:{:02}", s / 60, s % 60),
        Some(_) => "expired".to_string(),
        None => String::new(),
    }
}

fn countdown(seconds: Option<i64>, p: &Palette) -> Div {
    div()
        .text_xs()
        .text_color(p.muted.hsla())
        .child(countdown_text(seconds))
}

/// Who holds access, one row each with the digit key that revokes it.
fn browser_list(browsers: &[BrowserRow], emit: &Emit, p: &Palette) -> Div {
    let mut rows = v_flex().gap_1();
    for (i, b) in browsers.iter().enumerate() {
        let who = [
            b.address.clone(),
            b.browser.clone(),
            Some(flightdeck::web::access::age_label(b.granted_secs_ago)),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let revoke = b.key.map(|digit| {
            let emit = emit.clone();
            button(
                SharedString::from(format!("access-revoke-{i}")),
                "Revoke",
                ButtonKind::Quiet,
                p,
                move |_, w, cx| {
                    emit(
                        HostEvent::Overlay(OverlayInput::WebAccess(AccessKey::Char(digit))),
                        w,
                        cx,
                    )
                },
            )
            .py_0()
            .text_xs()
        });
        rows = rows.child(
            h_flex()
                .justify_between()
                .items_center()
                .px_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(p.ink_2.hsla())
                        .child(format!("● {who}")),
                )
                .children(revoke),
        );
    }
    let heading = if browsers.is_empty() {
        "No browsers have access".to_string()
    } else {
        format!("{} with access", browsers.len())
    };
    v_flex()
        .gap_1()
        .child(section_label(heading, p))
        .child(rows)
}

// ---------------------------------------------------------------------------
// Phone pairing
// ---------------------------------------------------------------------------

/// The phone pairing overlay: status, the QR from the `fdr1:` payload, the code
/// to type on the phone and the countdown.
pub fn pairing_view(view: &PairingView, emit: Emit, cx: &App) -> impl IntoElement {
    let p = *Palette::global(cx);
    let status_colour = if view.failed {
        danger(&p)
    } else if view.done {
        p.status_done.hsla()
    } else {
        p.ink_2.hsla()
    };

    let mut body = v_flex().gap_3().child(
        div()
            .text_sm()
            .text_color(status_colour)
            .child(view.status_line.clone()),
    );
    if let (Some(payload), false) = (&view.qr_payload, view.done || view.failed) {
        body = body.child(
            h_flex()
                .gap_4()
                .items_center()
                .children(QrMatrix::new(payload).map(|m| qr_canvas(m, p)))
                .child(
                    v_flex()
                        .gap_1()
                        .child(section_label("Or type this code", &p))
                        .children(view.code.as_ref().map(|code| {
                            div()
                                .font_family(MONO)
                                .text_2xl()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(code.clone())
                        }))
                        .child(countdown(view.seconds_remaining, &p)),
                ),
        );
    }
    let label = if view.done || view.failed {
        "Close"
    } else {
        "Cancel"
    };
    let close = {
        let emit = emit.clone();
        button(
            "pairing-cancel",
            label,
            ButtonKind::Secondary,
            &p,
            move |_, w, cx| emit(HostEvent::Overlay(OverlayInput::Cancel), w, cx),
        )
    };
    card("pairing-overlay", "Pair phone", px(480.), &p, &emit)
        .child(body)
        .child(h_flex().justify_end().child(close))
}

// ---------------------------------------------------------------------------
// Actions and the status-bar indicator
// ---------------------------------------------------------------------------

/// The remote menu: start or stop the web interface, show its access, take the
/// input lock, pair or unpair a phone. Each button runs a palette row; the host
/// refuses (with the palette's own reason) any that is not on offer, so a stale
/// menu cannot do something the palette would not.
pub fn remote_actions_view(status: &RemoteStatus, emit: Emit, cx: &App) -> impl IntoElement {
    let p = *Palette::global(cx);
    let run = |id: &'static str, label: &'static str, kind: ButtonKind, action: PaletteAction| {
        let emit = emit.clone();
        button(id, label, kind, &p, move |_, w, cx| {
            emit(HostEvent::RunPaletteAction(action.clone()), w, cx)
        })
    };
    let row = |title: &'static str, state: String, buttons: Div| {
        h_flex()
            .justify_between()
            .items_center()
            .gap_3()
            .child(
                v_flex()
                    .child(div().text_sm().child(title))
                    .child(div().text_xs().text_color(p.muted.hsla()).child(state)),
            )
            .child(buttons)
    };

    let web_state = if status.web_running {
        format!("running · {}", viewers_label(status.web_viewers))
    } else {
        "stopped".to_string()
    };
    let web_buttons = if status.web_running {
        h_flex()
            .gap_2()
            .child(run(
                "remote-web-access",
                "Show access",
                ButtonKind::Secondary,
                PaletteAction::ShowWebAccess,
            ))
            .child(run(
                "remote-web-stop",
                "Stop",
                ButtonKind::Secondary,
                PaletteAction::StopWebInterface,
            ))
    } else {
        h_flex().child(run(
            "remote-web-start",
            "Start",
            ButtonKind::Primary,
            PaletteAction::StartWebInterface,
        ))
    };

    let phone_state = if status.phone_paired {
        "paired"
    } else {
        "not paired"
    }
    .to_string();
    let phone_button = if status.phone_paired {
        run(
            "remote-phone-unpair",
            "Unpair",
            ButtonKind::Secondary,
            PaletteAction::UnpairPhone,
        )
    } else {
        run(
            "remote-phone-pair",
            "Pair phone",
            ButtonKind::Primary,
            PaletteAction::PairPhone,
        )
    };

    let mut body = v_flex()
        .gap_3()
        .child(row("Web interface", web_state, web_buttons))
        .child(row("Phone", phone_state, h_flex().child(phone_button)));
    if status.web_running {
        let holder = match &status.input_holder {
            Some(who) => format!("held by {who}"),
            None => "free".to_string(),
        };
        body = body.child(row(
            "Input",
            holder,
            h_flex().child(run(
                "remote-take-input",
                "Take input lock",
                ButtonKind::Secondary,
                PaletteAction::TakeInputLock,
            )),
        ));
    }
    card("remote-overlay", "Remote access", px(460.), &p, &emit).child(body)
}

fn viewers_label(n: usize) -> String {
    match n {
        1 => "1 viewer".to_string(),
        n => format!("{n} viewers"),
    }
}

/// One status-bar item: its text and whether it wants attention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndicatorItem {
    pub text: String,
    /// Drawn in the attention colour (somebody else holds the input lock).
    pub attention: bool,
}

/// What the status bar says about remote access, as the design brief lists it:
/// `web · 2 viewers`, `phone paired`, and who holds input when somebody other
/// than this desktop could be typing. Empty when nothing is on, so an ordinary
/// run's bar stays quiet.
pub fn remote_indicator_items(status: &RemoteStatus) -> Vec<IndicatorItem> {
    let mut items = Vec::new();
    if status.web_running {
        items.push(IndicatorItem {
            text: format!("web · {}", viewers_label(status.web_viewers)),
            attention: false,
        });
    }
    if status.phone_paired {
        items.push(IndicatorItem {
            text: "phone paired".to_string(),
            attention: false,
        });
    }
    if let Some(holder) = &status.input_holder {
        items.push(IndicatorItem {
            text: format!("input: {holder}"),
            attention: true,
        });
    }
    items
}

/// The right-hand status-bar items ([`remote_indicator_items`]) as an element.
pub fn remote_indicator(status: &RemoteStatus, cx: &App) -> Div {
    let p = Palette::global(cx);
    let mut row = h_flex().gap_4();
    for item in remote_indicator_items(status) {
        row = row.child(
            div()
                .text_xs()
                .text_color(
                    if item.attention {
                        p.status_attention
                    } else {
                        p.faint
                    }
                    .hsla(),
                )
                .child(item.text),
        );
    }
    row
}

#[cfg(test)]
mod tests {
    use flightdeck::web::access::{AddressRow, WebAccessView};
    use gpui::TestAppContext;

    use super::super::help::testkit::{click, mount, recorder, take};
    use super::*;

    const PAYLOAD: &str = "fdr1:eyJyZWxheSI6IndzczovL3JlbGF5LmV4YW1wbGUiLCJjb2RlIjoiNDgyMSJ9";

    fn key(k: AccessKey) -> HostEvent {
        HostEvent::Overlay(OverlayInput::WebAccess(k))
    }

    // --- QR ---------------------------------------------------------------

    /// The modules that get painted are exactly `QrCode::new(payload)`'s: the
    /// run rectangles, read back into a grid, equal the code module for module.
    #[test]
    fn the_painted_quads_are_the_codes_modules() {
        let matrix = QrMatrix::new(PAYLOAD).expect("a short payload encodes");
        let code = QrCode::new(PAYLOAD.as_bytes()).unwrap();
        assert_eq!(matrix.width, code.width());
        for y in 0..code.width() {
            for x in 0..code.width() {
                assert_eq!(
                    matrix.dark[y * matrix.width + x],
                    code[(x, y)] == Color::Dark
                );
            }
        }

        // Read the painted rectangles back into modules.
        let origin = point(px(100.), px(40.));
        let mut painted = vec![false; matrix.width * matrix.width];
        for b in qr_run_bounds(&matrix, origin) {
            let x0 = ((b.origin.x - origin.x) / px(MODULE_PX)) as usize;
            let y0 = ((b.origin.y - origin.y) / px(MODULE_PX)) as usize;
            let len = (b.size.width / px(MODULE_PX)) as usize;
            assert_eq!(b.size.height, px(MODULE_PX), "one module tall");
            for x in x0..x0 + len {
                assert!(!painted[y0 * matrix.width + x], "module painted twice");
                painted[y0 * matrix.width + x] = true;
            }
        }
        assert_eq!(painted, matrix.dark);
        assert_eq!(QrMatrix::from_runs(matrix.width, &matrix.runs()), matrix);
    }

    #[test]
    fn a_payload_too_long_for_any_qr_draws_nothing() {
        assert!(QrMatrix::new(&"x".repeat(10_000)).is_none());
    }

    // --- Pairing ----------------------------------------------------------

    fn pairing(done: bool, failed: bool) -> PairingView {
        PairingView {
            status_line: "Scan with the FlightDeck app".to_string(),
            code: Some("4821".to_string()),
            qr_payload: Some(PAYLOAD.to_string()),
            seconds_remaining: Some(95),
            done,
            failed,
        }
    }

    #[gpui::test]
    fn pairing_cancel_answers_cancel(cx: &mut TestAppContext) {
        let view = pairing(false, false);
        let (emit, events) = recorder();
        let cx = mount(cx, move |_, cx| {
            pairing_view(&view, emit.clone(), cx).into_any_element()
        });
        click(cx, "pairing-cancel");
        assert_eq!(
            take(&events),
            vec![HostEvent::Overlay(OverlayInput::Cancel)]
        );
    }

    #[test]
    fn the_countdown_reads_as_minutes_and_seconds() {
        assert_eq!(countdown_text(Some(95)), "expires in 1:35");
        assert_eq!(countdown_text(Some(5)), "expires in 0:05");
        assert_eq!(countdown_text(Some(-1)), "expired");
        assert_eq!(countdown_text(None), "");
    }

    // --- Web access -------------------------------------------------------

    fn local_only() -> WebAccessOverlay {
        WebAccessOverlay {
            view: WebAccessView {
                mode: Some(AccessMode::LocalOnly),
                bound: "127.0.0.1:7420".into(),
                exposure_line: "loopback only — nothing off this machine can reach it".into(),
                url: "http://127.0.0.1:7420".into(),
                keys: vec![],
                ..WebAccessView::default()
            },
            qr_payload: None,
        }
    }

    fn network() -> WebAccessOverlay {
        WebAccessOverlay {
            view: WebAccessView {
                mode: Some(AccessMode::Network),
                bound: "0.0.0.0:7420".into(),
                exposure_line: "reachable by anyone on this network who has the code".into(),
                url: "http://192.168.2.10:7420".into(),
                code: Some("WXYZ-2481".into()),
                seconds_remaining: Some(110),
                addresses: vec![
                    AddressRow {
                        name: "en0".into(),
                        address: "192.168.2.10".into(),
                        description: Some("Wi-Fi"),
                    },
                    AddressRow {
                        name: "en5".into(),
                        address: "10.0.0.4".into(),
                        description: None,
                    },
                    AddressRow {
                        name: "utun3".into(),
                        address: "100.64.0.9".into(),
                        description: Some("VPN"),
                    },
                ],
                selected_address: Some(0),
                browsers: vec![BrowserRow {
                    key: Some('1'),
                    address: Some("192.168.2.20".into()),
                    browser: Some("Safari on iOS".into()),
                    granted_secs_ago: 840,
                }],
                ..WebAccessView::default()
            },
            qr_payload: Some("http://192.168.2.10:7420/#WXYZ-2481".into()),
        }
    }

    fn draw_access(
        cx: &mut TestAppContext,
        overlay: WebAccessOverlay,
    ) -> (
        &mut gpui::VisualTestContext,
        super::super::help::testkit::Events,
    ) {
        let (emit, events) = recorder();
        let cx = mount(cx, move |_, cx| {
            web_access_view(&overlay, emit.clone(), cx).into_any_element()
        });
        (cx, events)
    }

    #[gpui::test]
    fn local_only_buttons_send_the_overlays_own_keys(cx: &mut TestAppContext) {
        let (cx, events) = draw_access(cx, local_only());
        for id in [
            "access-open",
            "access-copy",
            "access-network",
            "access-stop",
        ] {
            click(cx, id);
        }
        assert_eq!(
            take(&events),
            vec![
                key(AccessKey::Enter),
                key(AccessKey::Char('c')),
                key(AccessKey::Char('n')),
                key(AccessKey::Char('s')),
            ]
        );
        assert!(cx.debug_bounds("access-newcode").is_none(), "state B only");
    }

    #[gpui::test]
    fn network_buttons_send_the_overlays_own_keys(cx: &mut TestAppContext) {
        let (cx, events) = draw_access(cx, network());
        for id in [
            "access-newcode",
            "access-hide",
            "access-revoke-all",
            "access-revoke-0",
            "access-local",
        ] {
            click(cx, id);
        }
        assert_eq!(
            take(&events),
            vec![
                key(AccessKey::Space),
                key(AccessKey::Char('r')),
                key(AccessKey::Char('x')),
                key(AccessKey::Char('1')),
                key(AccessKey::Char('l')),
            ]
        );
        assert!(cx.debug_bounds("access-open").is_none(), "state A only");
    }

    /// Picking the third address walks the host's selection there one key at a
    /// time; picking one above walks back up.
    #[gpui::test]
    fn the_address_picker_walks_the_hosts_selection(cx: &mut TestAppContext) {
        let (cx, events) = draw_access(cx, network());
        click(cx, "access-address-2");
        assert_eq!(
            take(&events),
            vec![key(AccessKey::Down), key(AccessKey::Down)]
        );
    }

    #[gpui::test]
    fn the_address_picker_walks_back_up(cx: &mut TestAppContext) {
        let mut moved = network();
        moved.view.selected_address = Some(2);
        let (cx, events) = draw_access(cx, moved);
        click(cx, "access-address-0");
        assert_eq!(take(&events), vec![key(AccessKey::Up), key(AccessKey::Up)]);
    }

    // --- Actions and the indicator ------------------------------------------

    fn status(web: bool, viewers: usize, phone: bool, holder: Option<&str>) -> RemoteStatus {
        RemoteStatus {
            web_running: web,
            web_viewers: viewers,
            phone_paired: phone,
            input_holder: holder.map(str::to_string),
        }
    }

    fn run(cx: &mut TestAppContext, status: RemoteStatus, ids: &[&'static str]) -> Vec<HostEvent> {
        let (emit, events) = recorder();
        let cx = mount(cx, move |_, cx| {
            remote_actions_view(&status, emit.clone(), cx).into_any_element()
        });
        for id in ids {
            click(cx, id);
        }
        take(&events)
    }

    #[gpui::test]
    fn a_stopped_unpaired_host_offers_start_and_pair(cx: &mut TestAppContext) {
        let events = run(
            cx,
            status(false, 0, false, None),
            &["remote-web-start", "remote-phone-pair"],
        );
        assert_eq!(
            events,
            vec![
                HostEvent::RunPaletteAction(PaletteAction::StartWebInterface),
                HostEvent::RunPaletteAction(PaletteAction::PairPhone),
            ]
        );
    }

    #[gpui::test]
    fn a_running_paired_host_offers_stop_access_take_input_and_unpair(cx: &mut TestAppContext) {
        let events = run(
            cx,
            status(true, 2, true, Some("Safari on iOS")),
            &[
                "remote-web-access",
                "remote-web-stop",
                "remote-take-input",
                "remote-phone-unpair",
            ],
        );
        assert_eq!(
            events,
            vec![
                HostEvent::RunPaletteAction(PaletteAction::ShowWebAccess),
                HostEvent::RunPaletteAction(PaletteAction::StopWebInterface),
                HostEvent::RunPaletteAction(PaletteAction::TakeInputLock),
                HostEvent::RunPaletteAction(PaletteAction::UnpairPhone),
            ]
        );
    }

    #[gpui::test]
    fn actions_the_palette_would_not_offer_are_not_drawn(cx: &mut TestAppContext) {
        let (emit, _) = recorder();
        let stopped = status(false, 0, false, None);
        let cx = mount(cx, move |_, cx| {
            remote_actions_view(&stopped, emit.clone(), cx).into_any_element()
        });
        for id in [
            "remote-web-stop",
            "remote-take-input",
            "remote-phone-unpair",
        ] {
            assert!(
                cx.debug_bounds(id).is_none(),
                "{id} needs a running web interface"
            );
        }
    }

    #[test]
    fn the_indicator_says_what_is_on_and_nothing_when_nothing_is() {
        assert!(remote_indicator_items(&status(false, 0, false, None)).is_empty());
        let items = remote_indicator_items(&status(true, 2, true, Some("192.168.2.20")));
        let texts: Vec<_> = items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(
            texts,
            ["web · 2 viewers", "phone paired", "input: 192.168.2.20"]
        );
        assert!(items[2].attention && !items[0].attention);
        let one = remote_indicator_items(&status(true, 1, false, None));
        assert_eq!(one[0].text, "web · 1 viewer");
    }
}
