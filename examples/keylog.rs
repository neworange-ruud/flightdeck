//! Key-event diagnostic. Run with `cargo run --example keylog`, then press
//! keys to see exactly what crossterm reports on your terminal. Press Ctrl-C
//! (or Ctrl-Q) to exit.
//!
//! Useful for checking how macOS delivers Option+Esc, Option+arrows, etc.

use std::io::{self, Write};

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, PushKeyboardEnhancementFlags,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use crossterm::{execute, queue};

fn main() -> io::Result<()> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    // Try to enable kitty disambiguation; harmless if the terminal ignores it.
    let _ = execute!(
        out,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );

    queue!(out, crossterm::style::Print("Press keys (Ctrl-C / Ctrl-Q to quit)\r\n"))?;
    out.flush()?;

    loop {
        if let Event::Key(KeyEvent {
            code,
            modifiers,
            kind,
            ..
        }) = event::read()?
        {
            if kind != KeyEventKind::Press {
                continue;
            }
            let line = format!("code={code:?}  modifiers={modifiers:?}\r\n");
            out.write_all(line.as_bytes())?;
            out.flush()?;

            let is_ctrl = modifiers.contains(KeyModifiers::CONTROL);
            if is_ctrl && matches!(code, KeyCode::Char('c') | KeyCode::Char('q')) {
                break;
            }
        }
    }

    let _ = execute!(out, PopKeyboardEnhancementFlags);
    disable_raw_mode()?;
    Ok(())
}
