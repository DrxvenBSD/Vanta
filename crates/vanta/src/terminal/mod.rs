//! The terminal boundary between the editor loop and a real terminal.
//!
//! The editor reads events and draws frames through [`Terminal`]; it never
//! calls crossterm or any platform API directly. Tests implement the trait
//! with a scripted terminal, and the binary uses [`CrosstermTerminal`].

use std::io;
use std::time::Duration;

mod capabilities;
pub mod crossterm_terminal;
mod screen;

pub(crate) use capabilities::Capabilities;
pub use crossterm_terminal::{CrosstermTerminal, install_panic_hook};
pub(crate) use screen::Screen;

/// Terminal dimensions in display cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    /// Width in cells.
    pub width: u16,
    /// Height in rows.
    pub height: u16,
}

/// A cursor position in screen cells, zero-based from the top-left corner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenPos {
    /// Row from the top.
    pub row: u16,
    /// Column from the left.
    pub col: u16,
}

/// One selected or highlighted run of cells on a frame row.
///
/// `start_col` is inclusive and `end_col` is exclusive; both are display
/// cells after tab expansion and truncation. The range is empty when
/// `start_col == end_col` and is never stored empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionSpan {
    /// Screen row, zero-based from the top.
    pub row: u16,
    /// First selected display column.
    pub start_col: u16,
    /// Display column one past the selection.
    pub end_col: u16,
}

/// A semantic color applied to one run of cells.
///
/// The renderer emits tones rather than terminal color codes, so tests and
/// future renderers can assert the visual hierarchy without depending on a
/// specific terminal palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextStyle {
    /// The large product wordmark.
    Title,
    /// Secondary, low-emphasis text.
    Muted,
    /// A menu or status icon.
    Icon,
    /// A keyboard hint or command key.
    Key,
    /// A warm accent, used for the small status bolt.
    Warm,
}

/// One styled run of cells on a frame row.
///
/// `start_col` is inclusive and `end_col` is exclusive; both are display
/// cells after tab expansion and truncation. Styled spans are painted beneath
/// selection and search overlays by the crossterm backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StyledSpan {
    /// Screen row, zero-based from the top.
    pub row: u16,
    /// First styled display column.
    pub start_col: u16,
    /// Display column one past the style.
    pub end_col: u16,
    /// Semantic tone for the run.
    pub style: TextStyle,
}

/// One complete frame to draw.
///
/// `rows` has exactly the terminal height in rows: the visible buffer rows
/// followed by one status or command row. Rows are already sanitized, with
/// tabs expanded, control characters replaced and text truncated to the
/// terminal width, so a terminal implementation only has to place them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// The terminal size this frame was composed for.
    ///
    /// The crossterm backend compares this value with the previous frame and
    /// invalidates its front buffer on a resize.
    pub size: Size,
    /// The complete screen, top row first.
    pub rows: Vec<String>,
    /// The first buffer display column shown on every text row.
    ///
    /// Zero means an unscrolled line. The offset is a grapheme boundary and
    /// never splits a tab or wide glyph; the renderer already removed the
    /// cells before it, so this value is for assertions and later consumers,
    /// not an instruction to scroll again.
    pub horizontal_offset: usize,
    /// Where the terminal cursor should be placed.
    pub cursor: ScreenPos,
    /// Visible styled runs, ordered by row and column.
    ///
    /// The startup page uses these for its title, subtitle, icons and keys.
    /// The crossterm backend maps each semantic tone to a terminal color; an
    /// empty vector means no styling is active.
    pub styles: Vec<StyledSpan>,
    /// Visible Visual-mode selection cells, ordered by row and column.
    ///
    /// This is the headless assertion surface for selection rendering; the
    /// crossterm backend draws these spans with reverse video. An empty vector
    /// means no selection is active or none is visible.
    pub selection: Vec<SelectionSpan>,
    /// Visible occurrences of the current search query.
    ///
    /// All rows are ordered top-to-bottom and columns left-to-right. The
    /// active match is also present here and separately in
    /// [`Frame::search_current`].
    pub search_matches: Vec<SelectionSpan>,
    /// Visible cells of the current search match.
    ///
    /// The backend draws these spans distinctly from other visible matches.
    /// An empty vector means there is no active match.
    pub search_current: Vec<SelectionSpan>,
}

/// A normalized input event from the terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// A printable character.
    Char(char),
    /// A control-modified character, normalized to lowercase.
    Ctrl(char),
    /// Enter / return.
    Enter,
    /// Backspace.
    Backspace,
    /// Escape.
    Esc,
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
}

/// A terminal the editor can draw to and read from.
///
/// Implementations own their platform state. The editor calls
/// [`Terminal::size`] before every frame, draws one frame, then blocks in
/// [`Terminal::read_event`].
pub trait Terminal {
    /// Returns the current terminal size in cells.
    fn size(&mut self) -> io::Result<Size>;

    /// Draws a complete frame.
    fn draw(&mut self, frame: &Frame) -> io::Result<()>;

    /// Blocks until the next input event.
    fn read_event(&mut self) -> io::Result<InputEvent>;

    /// Waits up to `timeout` for the next input event.
    ///
    /// Returns `None` when the timeout expires without input. The default
    /// implementation ignores the timeout and blocks in
    /// [`Terminal::read_event`], which is correct for scripted terminals
    /// that always have an event ready; the real terminal implements a
    /// timed wait so the editor can flush an idle swap.
    fn poll_event(&mut self, _timeout: Duration) -> io::Result<Option<InputEvent>> {
        self.read_event().map(Some)
    }
}
