//! Crossterm-backed terminal used by the `vanta` binary.
//!
//! The backend owns raw mode and the alternate screen. [`CrosstermTerminal::enter`]
//! switches into the editor screen, [`CrosstermTerminal::restore`] switches
//! back, and [`install_panic_hook`] restores the terminal before a panic is
//! reported. Restoring is idempotent so the normal path, the `Drop` guard and
//! the panic hook can all call it.
//!
//! Frame painting lives in [`super::Screen`]; this file only owns platform
//! state, capability detection and event translation.

use std::io::{self, Write};
use std::time::Duration;

use crossterm::cursor::Show;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};

use super::{Capabilities, Frame, InputEvent, Screen, Size, Terminal};

/// A terminal driven by crossterm on the process's standard output.
#[derive(Debug)]
pub struct CrosstermTerminal {
    entered: bool,
    screen: Screen,
    capabilities: Capabilities,
}

impl Default for CrosstermTerminal {
    fn default() -> Self {
        Self::new()
    }
}

impl CrosstermTerminal {
    /// Creates a terminal that is not yet in editor mode.
    pub fn new() -> Self {
        Self {
            entered: false,
            screen: Screen::default(),
            capabilities: Capabilities::detect(),
        }
    }

    /// Enters raw mode and the alternate screen.
    ///
    /// If switching screens fails after raw mode was enabled, raw mode is
    /// disabled again before the error is returned. The caller must call
    /// [`CrosstermTerminal::restore`] on the normal exit path.
    pub fn enter(&mut self) -> io::Result<()> {
        self.capabilities = Capabilities::detect();
        self.screen.reset();
        terminal::enable_raw_mode()?;
        let switched = execute!(io::stdout(), EnterAlternateScreen, Show);
        if let Err(error) = switched {
            // The alternate screen may have been entered before a later
            // command failed. Best effort: restore both the screen and raw
            // mode; the primary failure is returned.
            let _ = execute!(io::stdout(), Show, LeaveAlternateScreen);
            let _ = terminal::disable_raw_mode();
            return Err(error);
        }
        self.entered = true;
        Ok(())
    }

    /// Restores the cursor, leaves the alternate screen and disables raw mode.
    ///
    /// Safe to call more than once; a terminal that was never entered is left
    /// untouched.
    pub fn restore(&mut self) -> io::Result<()> {
        if !self.entered {
            return Ok(());
        }
        let raw = terminal::disable_raw_mode();
        let screen = execute!(io::stdout(), Show, LeaveAlternateScreen);
        self.entered = false;
        self.screen.reset();
        raw.and(screen)
    }
}

impl Drop for CrosstermTerminal {
    fn drop(&mut self) {
        // Last line of defence for early returns and unwinds. The explicit
        // restore path reports failures; a drop cannot, so this is best effort.
        let _ = self.restore();
    }
}

impl Terminal for CrosstermTerminal {
    fn size(&mut self) -> io::Result<Size> {
        let (width, height) = terminal::size()?;
        Ok(Size { width, height })
    }

    fn draw(&mut self, frame: &Frame) -> io::Result<()> {
        let bytes = self.screen.render(frame, self.capabilities)?;
        if bytes.is_empty() {
            return Ok(());
        }
        let mut stdout = io::stdout().lock();
        stdout.write_all(&bytes)?;
        stdout.flush()
    }

    fn poll_event(&mut self, timeout: Duration) -> io::Result<Option<InputEvent>> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
                return Ok(None);
            };
            if !event::poll(remaining)? {
                return Ok(None);
            }
            // Read directly instead of calling `read_event`: that method skips
            // non-key events and would block past the deadline.
            if let Event::Key(key) = event::read()?
                && key.kind != KeyEventKind::Release
                && let Some(event) = translate_key(key)
            {
                return Ok(Some(event));
            }
        }
    }

    fn read_event(&mut self) -> io::Result<InputEvent> {
        loop {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if let Some(event) = translate_key(key) {
                        return Ok(event);
                    }
                }
                _ => {}
            }
        }
    }
}

/// Translates one key event, ignoring combinations the editor does not map.
fn translate_key(key: KeyEvent) -> Option<InputEvent> {
    if key.modifiers.contains(KeyModifiers::ALT) {
        return None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char(character) => Some(InputEvent::Ctrl(character.to_ascii_lowercase())),
            _ => None,
        };
    }
    match key.code {
        KeyCode::Char(character) => Some(InputEvent::Char(character)),
        KeyCode::Enter => Some(InputEvent::Enter),
        KeyCode::Backspace => Some(InputEvent::Backspace),
        KeyCode::Esc => Some(InputEvent::Esc),
        KeyCode::Left => Some(InputEvent::Left),
        KeyCode::Right => Some(InputEvent::Right),
        KeyCode::Up => Some(InputEvent::Up),
        KeyCode::Down => Some(InputEvent::Down),
        _ => None,
    }
}

/// Installs a panic hook that restores the terminal before the panic is
/// reported.
///
/// Release builds use `panic = "abort"`, so the hook is the only restoration
/// path available there; in debug builds the `Drop` guard also runs. Every
/// restoration call is best effort because a panic hook must not panic.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), Show, LeaveAlternateScreen);
        previous(info);
    }));
}
