//! Launchers for the terminal spike.
//!
//! - `flightdeck-desktop --spike-terminal [cmd args…]` opens one window with one
//!   terminal running `cmd` (default: the user's shell).
//! - `flightdeck-desktop --dump-grid [--emulator vt100|alacritty] [--wait-ms N]
//!   [--size ROWSxCOLS] [cmd args…]` runs the same PTY → grid → layout pipeline
//!   with no window: spawn, wait for the screen to settle, print the grid, the
//!   modes, the cursor and the frame layout the element would paint, then kill
//!   the process. It exists so the pipeline can be checked where no screen
//!   capture is possible (CI, a session without Screen Recording permission).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{
    px, size, App, AppContext, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions,
};
use gpui_component::Root;

use flightdeck::contracts::{PtySize, Result};
use flightdeck::terminal::grid::Emulator;
use flightdeck::terminal::pty::PortablePtyBackend;
use flightdeck::terminal::session::Terminal;
use flightdeck::terminal::shell::default_shell;

use super::layout::{layout, TermPalette};
use super::view::TerminalView;
use crate::app::Quit;
use crate::theme::{self, Palette};

/// Split `[cmd args…]` into a command, defaulting to the user's shell.
fn command(mut argv: Vec<String>) -> (String, Vec<String>) {
    if argv.is_empty() {
        (default_shell(), Vec::new())
    } else {
        let cmd = argv.remove(0);
        (cmd, argv)
    }
}

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn spawn(emulator: Emulator, argv: Vec<String>, size: PtySize) -> Result<Terminal> {
    let (cmd, args) = command(argv);
    Terminal::spawn(
        &PortablePtyBackend,
        emulator,
        &cmd,
        &args,
        &super::terminal_env(),
        &cwd(),
        size,
    )
}

/// Automation for `--spike-terminal`, read from leading flags (only honoured in
/// a build with the `spike-snapshot` feature; see desktop/Cargo.toml).
#[derive(Debug, Default, PartialEq)]
pub(crate) struct SnapshotOptions {
    /// `--spike-snapshot PATH`: write the rendered frame here, then quit.
    path: Option<PathBuf>,
    /// `--spike-keys "k1 k2 …"`: GPUI keystrokes (`a`, `enter`, `ctrl-c`) typed
    /// into the window after it opens, through the real key handler.
    keys: Vec<String>,
    /// `--spike-wait-ms N`: how long after opening to take the snapshot.
    wait: Option<Duration>,
}

impl SnapshotOptions {
    /// Whether any automation was asked for.
    pub(crate) fn is_requested(&self) -> bool {
        *self != SnapshotOptions::default()
    }
}

/// Peel the snapshot flags off the front of `argv`; the rest is the command.
/// The app window (`crate::app`) takes the same flags.
pub(crate) fn split_snapshot(
    argv: Vec<String>,
) -> std::result::Result<(SnapshotOptions, Vec<String>), String> {
    let mut opts = SnapshotOptions::default();
    let mut it = argv.into_iter().peekable();
    while let Some(flag) = it.peek().cloned() {
        match flag.as_str() {
            "--spike-snapshot" => {
                it.next();
                opts.path = Some(PathBuf::from(
                    it.next().ok_or("--spike-snapshot: expected a path")?,
                ));
            }
            "--spike-keys" => {
                it.next();
                let keys = it.next().ok_or("--spike-keys: expected keystrokes")?;
                opts.keys = keys.split_whitespace().map(str::to_string).collect();
            }
            "--spike-wait-ms" => {
                it.next();
                let ms = it
                    .next()
                    .and_then(|v| v.parse::<u64>().ok())
                    .ok_or("--spike-wait-ms: expected milliseconds")?;
                opts.wait = Some(Duration::from_millis(ms));
            }
            _ => break,
        }
    }
    Ok((opts, it.collect()))
}

/// `--spike-terminal`: one window, one terminal. Blocks until quit.
pub fn run_window(argv: Vec<String>) {
    let (snapshot, argv) = match split_snapshot(argv) {
        Ok(split) => split,
        Err(e) => {
            eprintln!("flightdeck-desktop --spike-terminal: {e}");
            std::process::exit(2);
        }
    };
    #[cfg(not(feature = "spike-snapshot"))]
    if snapshot.is_requested() {
        eprintln!(
            "flightdeck-desktop: --spike-snapshot/--spike-keys need a build with \
             `--features spike-snapshot`"
        );
        std::process::exit(2);
    }

    gpui_platform::application().run(move |cx: &mut App| {
        gpui_component::init(cx);
        theme::init(cx);
        if let Err(e) = crate::fonts::register(cx) {
            eprintln!("flightdeck-desktop: could not register the bundled fonts: {e}");
        }
        cx.on_action(|_: &Quit, cx| cx.quit());
        #[cfg(target_os = "macos")]
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        #[cfg(not(target_os = "macos"))]
        cx.bind_keys([KeyBinding::new("ctrl-q", Quit, None)]);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let terminal = match spawn(super::EMULATOR, argv, PtySize { rows: 24, cols: 80 }) {
            Ok(terminal) => terminal,
            Err(e) => {
                eprintln!("flightdeck-desktop: could not start the terminal: {e}");
                cx.quit();
                return;
            }
        };

        // Native decorations on every OS: the spike has no titlebar view of its
        // own to draw under a transparent one.
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(900.), px(600.)),
                cx,
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some("FlightDeck — terminal spike".into()),
                appears_transparent: false,
                traffic_light_position: None,
            }),
            window_min_size: Some(size(px(320.), px(200.))),
            app_id: Some("flightdeck-desktop".into()),
            ..WindowOptions::default()
        };
        let opened = cx.open_window(options, |window, cx| {
            let view = cx.new(|cx| TerminalView::new(terminal, cx));
            window.focus(&view.read(cx).focus_handle().clone(), cx);
            cx.new(|cx| Root::new(view, window, cx))
        });
        let handle = match opened {
            Ok(handle) => handle,
            Err(e) => {
                eprintln!("flightdeck-desktop: could not open the window: {e}");
                cx.quit();
                return;
            }
        };
        cx.activate(true);

        #[cfg(feature = "spike-snapshot")]
        if snapshot.path.is_some() || !snapshot.keys.is_empty() {
            snapshot::schedule(handle.into(), snapshot, cx);
        }
        #[cfg(not(feature = "spike-snapshot"))]
        let _ = handle;
    });
}

/// The `spike-snapshot` automation: type keys, render the frame offscreen,
/// write it out, quit.
#[cfg(feature = "spike-snapshot")]
pub(crate) mod snapshot {
    use std::io::Write as _;
    use std::time::Duration;

    use gpui::{AnyWindowHandle, App, Keystroke};

    use super::SnapshotOptions;

    /// When keys are typed, relative to the window opening.
    const KEYS_AFTER: Duration = Duration::from_millis(1200);
    /// Default snapshot time, relative to the window opening.
    const SNAPSHOT_AFTER: Duration = Duration::from_millis(3000);

    pub fn schedule(window: AnyWindowHandle, opts: SnapshotOptions, cx: &mut App) {
        cx.spawn(async move |cx| {
            let executor = cx.background_executor().clone();
            let wait = opts.wait.unwrap_or(SNAPSHOT_AFTER);
            let mut elapsed = Duration::ZERO;
            if !opts.keys.is_empty() {
                executor.timer(KEYS_AFTER).await;
                elapsed = KEYS_AFTER;
                for key in &opts.keys {
                    let Ok(keystroke) = Keystroke::parse(key) else {
                        eprintln!("spike-keys: cannot parse {key:?}");
                        continue;
                    };
                    let _ = window.update(cx, |_, window, cx| {
                        window.dispatch_keystroke(keystroke, cx);
                    });
                    executor.timer(Duration::from_millis(15)).await;
                    elapsed += Duration::from_millis(15);
                }
            }
            executor.timer(wait.saturating_sub(elapsed)).await;
            if let Some(path) = &opts.path {
                let image = window.update(cx, |_, window, _| window.render_to_image());
                match image {
                    Ok(Ok(image)) => {
                        match write_ppm(path, image.width(), image.height(), image.as_raw()) {
                            Ok(()) => println!(
                                "spike-snapshot: wrote {}x{} to {}",
                                image.width(),
                                image.height(),
                                path.display()
                            ),
                            Err(e) => eprintln!("spike-snapshot: write failed: {e}"),
                        }
                    }
                    Ok(Err(e)) => eprintln!("spike-snapshot: render failed: {e}"),
                    Err(e) => eprintln!("spike-snapshot: window gone: {e}"),
                }
            }
            cx.update(|cx| cx.quit());
        })
        .detach();
    }

    /// Binary PPM (P6): RGB, no alpha. Trivial to write, and `sips` /
    /// ImageMagick / most viewers read it.
    fn write_ppm(
        path: &std::path::Path,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> std::io::Result<()> {
        let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
        write!(out, "P6\n{width} {height}\n255\n")?;
        for px in rgba.chunks_exact(4) {
            out.write_all(&px[..3])?;
        }
        out.flush()
    }
}

/// Options for `--dump-grid`.
struct DumpOptions {
    emulator: Emulator,
    wait: Duration,
    size: PtySize,
    argv: Vec<String>,
}

fn parse_dump(args: Vec<String>) -> std::result::Result<DumpOptions, String> {
    let mut opts = DumpOptions {
        emulator: super::EMULATOR,
        wait: Duration::from_millis(3000),
        size: PtySize { rows: 24, cols: 80 },
        argv: Vec::new(),
    };
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--emulator" => {
                opts.emulator = match it.next().as_deref() {
                    Some("vt100") => Emulator::Vt100,
                    Some("alacritty") => Emulator::Alacritty,
                    other => {
                        return Err(format!(
                            "--emulator: expected vt100|alacritty, got {other:?}"
                        ))
                    }
                }
            }
            "--wait-ms" => {
                let ms = it
                    .next()
                    .and_then(|v| v.parse::<u64>().ok())
                    .ok_or("--wait-ms: expected milliseconds")?;
                opts.wait = Duration::from_millis(ms);
            }
            "--size" => {
                let v = it.next().ok_or("--size: expected ROWSxCOLS")?;
                let (r, c) = v.split_once('x').ok_or("--size: expected ROWSxCOLS")?;
                opts.size = PtySize {
                    rows: r.parse().map_err(|_| "--size: bad rows")?,
                    cols: c.parse().map_err(|_| "--size: bad cols")?,
                };
            }
            "--" => {
                opts.argv.extend(it);
                break;
            }
            _ => {
                opts.argv.push(arg);
                opts.argv.extend(it);
                break;
            }
        }
    }
    Ok(opts)
}

/// `--dump-grid`: returns the process exit code.
pub fn dump(args: Vec<String>) -> i32 {
    let opts = match parse_dump(args) {
        Ok(opts) => opts,
        Err(e) => {
            eprintln!("flightdeck-desktop --dump-grid: {e}");
            return 2;
        }
    };
    let mut terminal = match spawn(opts.emulator, opts.argv.clone(), opts.size) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("flightdeck-desktop --dump-grid: spawn failed: {e}");
            return 1;
        }
    };
    let palette = TermPalette::from_palette(&Palette::dark());
    let fg = palette.fg.0;
    let bg = palette.bg.0;
    terminal.screen_mut().set_default_colors(
        ((fg >> 16) as u8, (fg >> 8) as u8, fg as u8),
        ((bg >> 16) as u8, (bg >> 8) as u8, bg as u8),
    );

    // Poll like the window does until the deadline; note when output last
    // arrived so the dump says whether the screen had settled.
    let started = Instant::now();
    let mut bytes_seen = false;
    let mut last_output = started;
    while started.elapsed() < opts.wait {
        if super::pump(&mut terminal) {
            bytes_seen = true;
            last_output = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(8));
    }
    let state = terminal.process_state();
    let _ = terminal.session_mut().terminate_tree();

    let grid = terminal.screen();
    let (rows, cols) = grid.size();
    let cursor = grid.cursor();
    let modes = grid.modes();
    let frame = layout(grid, None, &palette, true);
    println!(
        "emulator={:?} size={rows}x{cols} process={state:?} output={} last_output_ms={}",
        opts.emulator,
        bytes_seen,
        last_output.duration_since(started).as_millis()
    );
    println!(
        "cursor: row={} col={} shape={:?} blinking={} visible={}",
        cursor.row, cursor.col, cursor.shape, cursor.blinking, cursor.visible
    );
    println!("modes: {modes:?}");
    println!("scrollback_len={}", terminal.screen().scrollback_len());
    println!("--- grid ({rows} rows) ---");
    for (i, line) in grid.contents().lines().enumerate() {
        println!("{i:>3}|{line}");
    }
    println!("--- frame layout ---");
    let mut fgs: Vec<u32> = frame.texts.iter().map(|t| t.fg.0).collect();
    fgs.sort_unstable();
    fgs.dedup();
    println!(
        "text_spans={} wide_spans={} box_cells={} background_spans={} distinct_fg={} cursor={:?}",
        frame.texts.len(),
        frame.texts.iter().filter(|t| t.wide).count(),
        frame.boxes.len(),
        frame.backgrounds.len(),
        fgs.len(),
        frame.cursor
    );
    for (i, line) in frame.text_rows().iter().enumerate() {
        if !line.trim().is_empty() {
            println!("{i:>3}|{line}");
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dump_options_parse() {
        let o = parse_dump(
            [
                "--emulator",
                "vt100",
                "--wait-ms",
                "10",
                "--size",
                "30x100",
                "vim",
                "-u",
                "NONE",
            ]
            .map(String::from)
            .to_vec(),
        )
        .unwrap();
        assert_eq!(o.emulator, Emulator::Vt100);
        assert_eq!(o.wait, Duration::from_millis(10));
        assert_eq!((o.size.rows, o.size.cols), (30, 100));
        assert_eq!(o.argv, ["vim", "-u", "NONE"]);

        let o = parse_dump(["--", "--weird"].map(String::from).to_vec()).unwrap();
        assert_eq!(o.argv, ["--weird"]);
        assert!(parse_dump(["--emulator", "xterm"].map(String::from).to_vec()).is_err());
    }

    #[test]
    fn no_command_means_the_users_shell() {
        let (cmd, args) = command(Vec::new());
        assert_eq!(cmd, default_shell());
        assert!(args.is_empty());
    }

    /// The whole headless pipeline against a real PTY: spawn, pump, parse,
    /// lay out. Unix-only because it drives `/bin/sh`.
    #[cfg(unix)]
    #[test]
    fn a_real_pty_reaches_the_frame_layout() {
        let mut terminal = spawn(
            super::super::EMULATOR,
            ["/bin/sh", "-c", r"printf '\033[31mred\033[0m \342\224\214\342\224\200\342\224\220 wide:\345\256\275\n'; sleep 2"]
                .map(String::from)
                .to_vec(),
            PtySize { rows: 5, cols: 40 },
        )
        .expect("spawn /bin/sh");
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !terminal.screen().contents().contains("wide:") {
            super::super::pump(&mut terminal);
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = terminal.session_mut().terminate_tree();

        let grid = terminal.screen();
        assert_eq!(grid.row_text(0, 0, 39), "red ┌─┐ wide:宽");
        let palette = TermPalette::from_palette(&Palette::dark());
        let frame = layout(grid, None, &palette, true);
        assert_eq!(frame.texts[0].text, "red");
        assert_eq!(frame.texts[0].fg, palette.ansi[1]);
        assert_eq!(frame.boxes.len(), 3);
        assert!(frame.texts.iter().any(|t| t.wide && t.text == "宽"));
    }
}
