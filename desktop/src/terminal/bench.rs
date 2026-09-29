//! `flightdeck-desktop --bench …`: the M0 spike S4 measurements (beads
//! `remote-control-bmej.1.4`), re-runnable on any OS the app builds on.
//!
//! Each scenario opens a real window holding one or more [`TerminalView`]s on
//! real PTYs, drives it from a timeline, prints its numbers and quits. The
//! timestamps come from a [`Probe`] the view and element call into at the
//! points that matter; with no probe attached (every normal run) each hook is
//! one `None` check.
//!
//! What each timestamp means:
//!
//! - **key received**: entry of the view's key-down handler, i.e. after GPUI
//!   has dispatched the keystroke (the OS → GPUI hop is not included);
//! - **written**: `PtySession::write_input` returned;
//! - **parsed**: the end of the first PTY pump after the write that fed the
//!   emulator any bytes (the echo);
//! - **painted**: the end of the element's `paint` for the first frame whose
//!   prepaint started after that pump. GPUI hands the scene to Metal right
//!   after the window's paint pass, in the same call, so this is "frame ready
//!   to present"; the display's scan-out (up to one refresh interval) is not
//!   included, just as the host terminal's own drawing is not included for the
//!   TUI.
//!
//! Scenarios (all take `--seconds S` where it makes sense):
//!
//! - `throughput --cmd 'cat big.txt'`: types `CMD; printf '%s-%s\n' FD DONE`
//!   into a shell and times Enter → the painted frame that shows `FD-DONE`.
//! - `latency --keys N`: types N keys into `cat > /dev/null` (the tty echoes
//!   them), one at a time with a 30–70 ms gap, and reports each stage.
//! - `idle --terminals N [--ticker] [--fill]`: N shells (2x2 for 4),
//!   optionally each running `while true; do date; sleep 1; done`, left alone
//!   for S seconds; reports frames painted, PTY polls, element CPU per frame
//!   and rows re-laid per frame. `--fill` first fills the screen with the
//!   scroll scenario's coloured lines and makes the ticker rewrite one line in
//!   place (`printf '\r%s'`), the way an agent's spinner or status line does:
//!   the case damage tracking is for. CPU and RSS are sampled from outside
//!   (desktop/benches/perf.py) so the probe cannot flatter them.
//! - `scroll`: fills the scrollback with 3000 coloured lines, then scrolls up three
//!   lines per frame for S seconds and reports per-frame element CPU and the
//!   frame-to-frame interval.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    div, px, size, App, AppContext, AsyncApp, Bounds, Context, Entity, IntoElement, Keystroke,
    ParentElement, Render, Styled, TitlebarOptions, WeakEntity, Window, WindowBounds, WindowHandle,
    WindowOptions,
};

use flightdeck::contracts::PtySize;
use flightdeck::terminal::grid::GridView;
use flightdeck::terminal::pty::PortablePtyBackend;
use flightdeck::terminal::session::Terminal;

use super::view::TerminalView;
use crate::theme;

/// What the throughput scenario waits for. Typed as `FD-%s` + `DONE` so the
/// echoed command line itself never contains it.
const DONE_MARKER: &str = "FD-DONE";
/// What the scroll scenario fills the scrollback with: 3000 log-like lines of
/// about 120 columns, each with a coloured field.
const SCROLL_FILL: &str = "awk 'BEGIN { for (i = 1; i <= 3000; i++) printf \"%06d \\033[3%dmlorem ipsum dolor\\033[0m sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua %d\\n\", i, i % 7 + 1, i * 7 }'\r";
/// Time for a freshly spawned shell to print its prompt before a scenario
/// starts typing.
const SETTLE: Duration = Duration::from_millis(1500);

/// One keystroke's trip, in the order the stages happen.
#[derive(Debug, Clone, Copy)]
pub struct KeySample {
    pub received: Instant,
    pub written: Option<Instant>,
    pub parsed: Option<Instant>,
    pub painted: Option<Instant>,
}

/// Timestamps collected from one terminal view and its element.
#[derive(Debug, Default)]
pub struct Probe {
    /// Frames the element painted.
    pub frames: u64,
    /// Element CPU per frame: prepaint start → paint end.
    pub frame_cpu: Vec<Duration>,
    /// When each frame's paint ended.
    pub paint_ends: Vec<Instant>,
    prepaint_start: Option<Instant>,
    /// PTY polls, the time spent in them and the bytes they parsed.
    pub pumps: u64,
    pub pump_time: Duration,
    pub bytes: u64,
    /// Rows the element laid out (and shaped) again, over all frames.
    pub rows_laid_out: u64,
    /// The keystroke in flight, then every finished one.
    pending: Option<KeySample>,
    pub keys: Vec<KeySample>,
    /// Throughput: watch for [`DONE_MARKER`] once armed.
    watch_marker: bool,
    pub marker_parsed: Option<Instant>,
    pub marker_painted: Option<Instant>,
}

impl Probe {
    pub fn key_received(&mut self) {
        // A key whose echo never came (Enter, say) is dropped, not averaged.
        self.pending = Some(KeySample {
            received: Instant::now(),
            written: None,
            parsed: None,
            painted: None,
        });
    }

    pub fn key_written(&mut self) {
        if let Some(key) = self.pending.as_mut() {
            key.written = Some(Instant::now());
        }
    }

    pub fn pumped(&mut self, bytes: usize, took: Duration, grid: &dyn GridView) {
        self.pumps += 1;
        self.pump_time += took;
        self.bytes += bytes as u64;
        if bytes == 0 {
            return;
        }
        let now = Instant::now();
        if let Some(key) = self.pending.as_mut() {
            if key.written.is_some() && key.parsed.is_none() {
                key.parsed = Some(now);
            }
        }
        if self.watch_marker
            && self.marker_parsed.is_none()
            && grid.contents().contains(DONE_MARKER)
        {
            self.marker_parsed = Some(now);
        }
    }

    pub fn prepaint_started(&mut self) {
        self.prepaint_start = Some(Instant::now());
    }

    pub fn painted(&mut self) {
        let now = Instant::now();
        self.frames += 1;
        self.paint_ends.push(now);
        let Some(start) = self.prepaint_start.take() else {
            return;
        };
        self.frame_cpu.push(now - start);
        // A stage counts as painted by the first frame that read the grid
        // after it, i.e. whose prepaint began later.
        if let Some(key) = self.pending {
            if key.parsed.is_some_and(|parsed| parsed <= start) {
                self.keys.push(KeySample {
                    painted: Some(now),
                    ..key
                });
                self.pending = None;
            }
        }
        if self.marker_painted.is_none() && self.marker_parsed.is_some_and(|p| p <= start) {
            self.marker_painted = Some(now);
        }
    }
}

/// Parsed `--bench` arguments.
#[derive(Debug, Clone, PartialEq)]
struct Options {
    scenario: Scenario,
    terminals: usize,
    ticker: bool,
    fill: bool,
    seconds: u64,
    keys: usize,
    cmd: String,
    /// Window content size in logical pixels.
    width: f32,
    height: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scenario {
    Throughput,
    Latency,
    Idle,
    Scroll,
}

fn parse(args: Vec<String>) -> Result<Options, String> {
    let mut it = args.into_iter();
    let scenario = match it.next().as_deref() {
        Some("throughput") => Scenario::Throughput,
        Some("latency") => Scenario::Latency,
        Some("idle") => Scenario::Idle,
        Some("scroll") => Scenario::Scroll,
        other => {
            return Err(format!(
                "expected throughput|latency|idle|scroll, got {other:?}"
            ))
        }
    };
    let mut opts = Options {
        scenario,
        terminals: 1,
        ticker: false,
        fill: false,
        seconds: 30,
        keys: 200,
        cmd: "seq 1 2000000".to_string(),
        // About 130x42 cells of Menlo 13: the terminal pane the TUI bench gets
        // in a 160x50 host terminal.
        width: 1040.,
        height: 700.,
    };
    let number = |flag: &str, v: Option<String>| -> Result<u64, String> {
        v.and_then(|v| v.parse().ok())
            .ok_or(format!("{flag}: expected a number"))
    };
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--terminals" => opts.terminals = number(&flag, it.next())?.clamp(1, 16) as usize,
            "--ticker" => opts.ticker = true,
            "--fill" => opts.fill = true,
            "--seconds" => opts.seconds = number(&flag, it.next())?,
            "--keys" => opts.keys = number(&flag, it.next())? as usize,
            "--cmd" => opts.cmd = it.next().ok_or("--cmd: expected a command")?,
            "--size" => {
                let v = it.next().ok_or("--size: expected WxH")?;
                let (w, h) = v.split_once('x').ok_or("--size: expected WxH")?;
                opts.width = w.parse().map_err(|_| "--size: bad width")?;
                opts.height = h.parse().map_err(|_| "--size: bad height")?;
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(opts)
}

/// The window's root: the terminals in a grid (two columns once there are
/// more than one).
struct BenchRoot {
    views: Vec<Entity<TerminalView>>,
}

impl Render for BenchRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let columns = if self.views.len() > 1 { 2 } else { 1 };
        let mut rows = Vec::new();
        for chunk in self.views.chunks(columns) {
            rows.push(
                div().flex().flex_row().flex_1().min_h_0().children(
                    chunk
                        .iter()
                        .map(|v| div().flex_1().min_w_0().child(v.clone())),
                ),
            );
        }
        div().size_full().flex().flex_col().children(rows)
    }
}

fn spawn_shell(argv: &[&str]) -> flightdeck::contracts::Result<Terminal> {
    let mut env = super::terminal_env();
    env.push(("PS1".to_string(), "$ ".to_string()));
    let args: Vec<String> = argv[1..].iter().map(|s| s.to_string()).collect();
    Terminal::spawn(
        &PortablePtyBackend,
        super::EMULATOR,
        argv[0],
        &args,
        &env,
        &std::env::current_dir().unwrap_or_else(|_| ".".into()),
        PtySize { rows: 24, cols: 80 },
    )
}

/// `--bench SCENARIO [flags]`: returns the process exit code.
pub fn run(args: Vec<String>) -> i32 {
    let opts = match parse(args) {
        Ok(opts) => opts,
        Err(e) => {
            eprintln!("flightdeck-desktop --bench: {e}");
            return 2;
        }
    };
    gpui_platform::application().run(move |cx: &mut App| {
        gpui_component::init(cx);
        theme::init(cx);
        if let Err(e) = crate::fonts::register(cx) {
            eprintln!("flightdeck-desktop --bench: could not register the bundled fonts: {e}");
        }
        let shell: &[&str] = match opts.scenario {
            Scenario::Latency => &["/bin/sh", "-c", "exec cat > /dev/null"],
            _ => &["/bin/sh"],
        };
        let mut terminals = Vec::new();
        for _ in 0..opts.terminals {
            match spawn_shell(shell) {
                Ok(t) => terminals.push(t),
                Err(e) => {
                    eprintln!("flightdeck-desktop --bench: spawn failed: {e}");
                    cx.quit();
                    return;
                }
            }
        }
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(opts.width), px(opts.height)),
                cx,
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some("FlightDeck — bench".into()),
                appears_transparent: false,
                traffic_light_position: None,
            }),
            app_id: Some("flightdeck-desktop".into()),
            ..WindowOptions::default()
        };
        let mut probes = Vec::new();
        let opened = cx.open_window(options, |window, cx| {
            let views: Vec<Entity<TerminalView>> = terminals
                .into_iter()
                .map(|t| {
                    let probe = Rc::new(RefCell::new(Probe::default()));
                    probes.push(probe.clone());
                    cx.new(|cx| {
                        let mut view = TerminalView::new(t, cx);
                        view.set_probe(probe);
                        view
                    })
                })
                .collect();
            window.focus(&views[0].read(cx).focus_handle().clone(), cx);
            cx.new(|_| BenchRoot { views })
        });
        let handle = match opened {
            Ok(handle) => handle,
            Err(e) => {
                eprintln!("flightdeck-desktop --bench: could not open the window: {e}");
                cx.quit();
                return;
            }
        };
        cx.activate(true);
        let views = handle
            .update(cx, |root, _, _| {
                root.views.iter().map(|v| v.downgrade()).collect()
            })
            .unwrap_or_default();
        cx.spawn(async move |cx| {
            drive(opts, handle, views, probes, cx).await;
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
    0
}

/// Write bytes into terminal `i`'s PTY as if typed.
fn type_into(views: &[WeakEntity<TerminalView>], i: usize, text: &str, cx: &mut AsyncApp) {
    if let Some(view) = views.get(i) {
        let _ = view.update(cx, |view, _| {
            let _ = view
                .owned_terminal_mut()
                .expect("the bench owns its terminals")
                .session_mut()
                .write_input(text.as_bytes());
        });
    }
}

async fn drive(
    opts: Options,
    window: WindowHandle<BenchRoot>,
    views: Vec<WeakEntity<TerminalView>>,
    probes: Vec<Rc<RefCell<Probe>>>,
    cx: &mut AsyncApp,
) {
    let executor = cx.background_executor().clone();
    executor.timer(SETTLE).await;
    let grid = views
        .first()
        .and_then(|v| {
            v.update(cx, |v, _| v.owned_terminal().map(|t| t.screen().size()))
                .ok()
        })
        .flatten()
        .unwrap_or_default();
    println!(
        "bench={:?} terminals={} grid={}x{} window={}x{}",
        opts.scenario, opts.terminals, grid.0, grid.1, opts.width, opts.height
    );
    match opts.scenario {
        Scenario::Throughput => {
            probes[0].borrow_mut().watch_marker = true;
            let line = format!("{}; printf '%s-%s\\n' FD DONE\r", opts.cmd);
            let started = Instant::now();
            let (frames0, cpu0, pump0, bytes0) = {
                let p = probes[0].borrow();
                (p.frames, p.frame_cpu.len(), p.pump_time, p.bytes)
            };
            type_into(&views, 0, &line, cx);
            let deadline = started + Duration::from_secs(opts.seconds.max(1) * 10);
            while probes[0].borrow().marker_painted.is_none() && Instant::now() < deadline {
                executor.timer(Duration::from_millis(5)).await;
            }
            let p = probes[0].borrow();
            let Some(done) = p.marker_painted else {
                println!("result=timeout");
                return;
            };
            let cpu: Duration = p.frame_cpu[cpu0..].iter().sum();
            println!(
                "cmd={:?} total_ms={:.1} parsed_ms={:.1} frames={} element_cpu_ms={:.1} pump_ms={:.1} bytes={}",
                opts.cmd,
                ms(done - started),
                ms(p.marker_parsed.unwrap_or(done) - started),
                p.frames - frames0,
                ms(cpu),
                ms(p.pump_time - pump0),
                p.bytes - bytes0
            );
        }
        Scenario::Latency => {
            let mut sent = 0;
            let mut seed: u32 = 0x2545_f491;
            while sent < opts.keys {
                // A small LCG for the gap, so keys do not phase-lock with the
                // poll timer or the display refresh.
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let gap = 30 + u64::from((seed >> 16) % 41);
                executor.timer(Duration::from_millis(gap)).await;
                let key = if sent % 50 == 49 { "enter" } else { "a" };
                if let Ok(keystroke) = Keystroke::parse(key) {
                    // Through the untyped handle, as the spike's `--spike-keys`
                    // does: the typed one would hold the root view leased
                    // while the dispatch wants to render it.
                    let any: gpui::AnyWindowHandle = window.into();
                    let _ = any.update(cx, |_, window, cx| {
                        window.dispatch_keystroke(keystroke, cx);
                    });
                }
                sent += 1;
            }
            executor.timer(Duration::from_millis(200)).await;
            let p = probes[0].borrow();
            let stage = |f: &dyn Fn(&KeySample) -> Option<Duration>| -> Vec<Duration> {
                p.keys.iter().filter_map(f).collect()
            };
            let write = stage(&|k| k.written.map(|w| w - k.received));
            let echo = stage(&|k| Some(k.parsed? - k.written?));
            let frame = stage(&|k| Some(k.painted? - k.parsed?));
            let total = stage(&|k| k.painted.map(|p| p - k.received));
            println!("samples={}", total.len());
            let mut cpu = p.frame_cpu.clone();
            println!("element_cpu_per_frame: {}", summary(&mut cpu));
            for (name, mut v) in [
                ("key_to_write", write),
                ("write_to_parsed", echo),
                ("parsed_to_painted", frame),
                ("key_to_painted", total),
            ] {
                println!("{name}: {}", summary(&mut v));
            }
        }
        Scenario::Idle => {
            if opts.fill {
                for i in 0..views.len() {
                    type_into(&views, i, SCROLL_FILL, cx);
                }
                executor.timer(Duration::from_millis(1500)).await;
            }
            let ticker = if opts.fill {
                "while true; do printf '\\r%s' \"$(date)\"; sleep 1; done\r"
            } else {
                "while true; do date; sleep 1; done\r"
            };
            if opts.ticker {
                for i in 0..views.len() {
                    type_into(&views, i, ticker, cx);
                }
            }
            // Let the typed line's own echo and first tick settle.
            executor.timer(Duration::from_millis(1200)).await;
            let before: Vec<(u64, u64, usize, u64)> = probes
                .iter()
                .map(|p| {
                    let p = p.borrow();
                    (p.frames, p.pumps, p.frame_cpu.len(), p.rows_laid_out)
                })
                .collect();
            let started = Instant::now();
            println!("idle_start");
            executor.timer(Duration::from_secs(opts.seconds)).await;
            let secs = started.elapsed().as_secs_f64();
            for (i, (p, (f0, p0, c0, r0))) in probes.iter().zip(before).enumerate() {
                let p = p.borrow();
                let frames = p.frames - f0;
                let mut cpu = p.frame_cpu[c0..].to_vec();
                println!(
                    "terminal={i} frames_per_s={:.2} polls_per_s={:.1} rows_relaid_per_frame={:.1} element_cpu: {}",
                    frames as f64 / secs,
                    (p.pumps - p0) as f64 / secs,
                    (p.rows_laid_out - r0) as f64 / frames.max(1) as f64,
                    summary(&mut cpu)
                );
            }
        }
        Scenario::Scroll => {
            type_into(&views, 0, SCROLL_FILL, cx);
            executor.timer(Duration::from_millis(2500)).await;
            let first = probes[0].borrow().frame_cpu.len();
            let ends0 = probes[0].borrow().paint_ends.len();
            let started = Instant::now();
            // Up through the whole history, then back down, and so on.
            let up = Rc::new(std::cell::Cell::new(true));
            while started.elapsed() < Duration::from_secs(opts.seconds) {
                // One scroll step per frame: at the next frame, scroll and
                // notify, then wait for that to have happened.
                let view = views[0].clone();
                let done = Rc::new(std::cell::Cell::new(false));
                let (up, flag) = (up.clone(), done.clone());
                let _ = window.update(cx, move |_, window, _| {
                    window.on_next_frame(move |_, cx| {
                        let _ = view.update(cx, |view, cx| {
                            let Some(t) = view.owned_terminal_mut() else {
                                return;
                            };
                            let (at, top) = (t.screen().scrollback(), t.screen().scrollback_len());
                            if at >= top {
                                up.set(false);
                            } else if at == 0 {
                                up.set(true);
                            }
                            if up.get() {
                                t.scroll_up(3);
                            } else {
                                t.scroll_down(3);
                            }
                            cx.notify();
                        });
                        flag.set(true);
                    });
                });
                while !done.get() {
                    executor.timer(Duration::from_micros(500)).await;
                }
            }
            let p = probes[0].borrow();
            let mut cpu: Vec<Duration> = p.frame_cpu[first..].to_vec();
            let mut gaps: Vec<Duration> = p.paint_ends[ends0..]
                .windows(2)
                .map(|w| w[1] - w[0])
                .collect();
            println!("frames={}", cpu.len());
            println!("element_cpu: {}", summary(&mut cpu));
            println!("frame_interval: {}", summary(&mut gaps));
        }
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// `p50 p95 max` in milliseconds.
fn summary(v: &mut [Duration]) -> String {
    if v.is_empty() {
        return "n=0".to_string();
    }
    v.sort_unstable();
    format!(
        "n={} p50={:.2}ms p95={:.2}ms max={:.2}ms",
        v.len(),
        ms(percentile(v, 50)),
        ms(percentile(v, 95)),
        ms(v[v.len() - 1])
    )
}

/// Nearest-rank percentile of a sorted slice.
fn percentile(sorted: &[Duration], pct: usize) -> Duration {
    let rank = (pct * sorted.len()).div_ceil(100).max(1);
    sorted[rank.min(sorted.len()) - 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_parse() {
        let o = parse(
            ["idle", "--terminals", "4", "--ticker", "--seconds", "5"]
                .map(String::from)
                .to_vec(),
        )
        .unwrap();
        assert_eq!(o.scenario, Scenario::Idle);
        assert_eq!((o.terminals, o.ticker, o.seconds), (4, true, 5));
        let o = parse(
            ["throughput", "--cmd", "cat x", "--size", "800x600"]
                .map(String::from)
                .to_vec(),
        )
        .unwrap();
        assert_eq!((o.cmd.as_str(), o.width, o.height), ("cat x", 800., 600.));
        assert!(parse(vec!["nope".to_string()]).is_err());
        assert!(parse(["idle", "--bogus"].map(String::from).to_vec()).is_err());
    }

    #[test]
    fn percentiles_use_nearest_rank() {
        let v: Vec<Duration> = (1..=100).map(Duration::from_millis).collect();
        assert_eq!(percentile(&v, 50), Duration::from_millis(50));
        assert_eq!(percentile(&v, 95), Duration::from_millis(95));
        assert_eq!(percentile(&v[..1], 95), Duration::from_millis(1));
    }

    #[test]
    fn a_key_is_painted_by_the_first_frame_that_read_its_echo() {
        let grid = flightdeck::terminal::grid::Emulator::Alacritty.build(2, 10);
        let mut p = Probe::default();
        p.key_received();
        p.key_written();
        // A frame that began before the echo was parsed does not count.
        p.prepaint_started();
        p.pumped(1, Duration::ZERO, grid.as_ref());
        p.painted();
        assert!(p.keys.is_empty());
        p.prepaint_started();
        p.painted();
        assert_eq!(p.keys.len(), 1);
        let k = p.keys[0];
        assert!(k.received <= k.written.unwrap());
        assert!(k.parsed.unwrap() <= k.painted.unwrap());
    }
}
