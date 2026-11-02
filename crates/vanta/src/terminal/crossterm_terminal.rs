//! Crossterm-backed terminal used by the `vanta` binary.
//!
//! The backend owns raw mode and the alternate screen. [`CrosstermTerminal::enter`]
//! switches into the editor screen, [`CrosstermTerminal::restore`] switches
//! back, and [`install_panic_hook`] restores the terminal before a panic is
//! reported. Restoring is idempotent so the normal path, the `Drop` guard and
//! the panic hook can all call it.

use std::io::{self, Write};
use std::time::Duration;

use crossterm::cursor::{MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, queue};
use unicode_segmentation::UnicodeSegmentation;
use vanta_text::grapheme_display_width;

use super::{Frame, InputEvent, SelectionSpan, Size, StyledSpan, Terminal, TextStyle};

/// A terminal driven by crossterm on the process's standard output.
#[derive(Debug, Default)]
pub struct CrosstermTerminal {
    entered: bool,
}

impl CrosstermTerminal {
    /// Creates a terminal that is not yet in editor mode.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enters raw mode and the alternate screen.
    ///
    /// If switching screens fails after raw mode was enabled, raw mode is
    /// disabled again before the error is returned. The caller must call
    /// [`CrosstermTerminal::restore`] on the normal exit path.
    pub fn enter(&mut self) -> io::Result<()> {
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
        let mut stdout = io::stdout().lock();
        queue!(stdout, MoveTo(0, 0), Clear(ClearType::All))?;
        for (row, text) in frame.rows.iter().enumerate() {
            queue!(stdout, MoveTo(0, row as u16), Print(text))?;
        }
        for span in &frame.styles {
            if let Some(row) = frame.rows.get(usize::from(span.row)) {
                queue_style(&mut stdout, row, *span)?;
            }
        }
        for span in &frame.search_matches {
            if let Some(row) = frame.rows.get(usize::from(span.row)) {
                queue_span(&mut stdout, row, *span, Attribute::Underlined)?;
            }
        }
        for span in &frame.search_current {
            if let Some(row) = frame.rows.get(usize::from(span.row)) {
                queue_span(&mut stdout, row, *span, Attribute::Reverse)?;
            }
        }
        for span in &frame.selection {
            if let Some(row) = frame.rows.get(usize::from(span.row)) {
                queue_span(&mut stdout, row, *span, Attribute::Reverse)?;
            }
        }
        queue!(stdout, MoveTo(frame.cursor.col, frame.cursor.row), Show)?;
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

/// Draws one semantic color run over an already-rendered row.
fn queue_style<W: Write>(stdout: &mut W, row: &str, span: StyledSpan) -> io::Result<()> {
    let text = selected_text(
        row,
        SelectionSpan {
            row: span.row,
            start_col: span.start_col,
            end_col: span.end_col,
        },
    );
    if text.is_empty() {
        return Ok(());
    }

    let (color, bold) = match span.style {
        TextStyle::Title => (Color::Blue, true),
        TextStyle::Muted => (Color::Grey, false),
        TextStyle::Icon => (Color::Cyan, false),
        TextStyle::Key => (Color::Blue, false),
        TextStyle::Warm => (Color::Yellow, false),
    };
    queue!(
        stdout,
        MoveTo(span.start_col, span.row),
        SetForegroundColor(color)
    )?;
    if bold {
        queue!(stdout, SetAttribute(Attribute::Bold))?;
    }
    queue!(
        stdout,
        Print(text),
        SetAttribute(Attribute::Reset),
        ResetColor
    )
}

/// Draws one highlighted span over an already-rendered row.
fn queue_span<W: Write>(
    stdout: &mut W,
    row: &str,
    span: SelectionSpan,
    attribute: Attribute,
) -> io::Result<()> {
    let text = selected_text(row, span);
    if text.is_empty() {
        return Ok(());
    }
    queue!(
        stdout,
        MoveTo(span.start_col, span.row),
        SetAttribute(attribute),
        Print(text),
        SetAttribute(Attribute::Reset),
    )
}

/// Returns the cells covered by `span` as text that can be drawn over an
/// already-rendered row.
///
/// `row` is sanitized and tab-expanded, so display cells and grapheme widths
/// line up. A wide grapheme is included whole when any of its cells intersect
/// the span; the renderer builds spans on grapheme boundaries, so this is
/// normally an exact cell slice.
fn selected_text(row: &str, span: SelectionSpan) -> String {
    let start = usize::from(span.start_col);
    let end = usize::from(span.end_col);
    let mut output = String::new();
    let mut column = 0usize;

    for grapheme in row.graphemes(true) {
        if column >= end {
            break;
        }
        let width = grapheme_display_width(grapheme).max(1);
        if column + width > start {
            output.push_str(grapheme);
        }
        column += width;
    }
    if output.is_empty() && end > start {
        // A selected empty line or a selected line terminator has no text
        // cells to copy; draw reverse spaces so the selection is still
        // visible.
        output.push_str(&" ".repeat(end - start));
    }
    output
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
