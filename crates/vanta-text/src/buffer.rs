//! The rope-backed [`Buffer`] facade.
//!
//! The implementation is split by concern so a change to one part does not
//! require reading the others:
//!
//! * `storage` owns the rope and line-index queries.
//! * `position` owns byte, line/column and scalar conversions.
//! * `measure` owns the display-width queries.
//! * `edit` owns mutation and the [`TextEdit`] record an undo block stores.
//! * `history` owns the undo/redo blocks and the saved-state marker
//!   (`PROJECT_SPEC` §5.12).
//!
//! Only the public [`Buffer`] type and [`TextEdit`] are exposed. Undo history
//! is owned by `Buffer` and records the [`TextEdit`] values produced by
//! [`Buffer::apply`].

mod edit;
mod history;
mod measure;
mod position;
mod storage;

#[cfg(test)]
mod tests;

pub use edit::TextEdit;

use ropey::Rope;

use crate::line_ending::LineEnding;
use history::History;

/// A rope-backed text buffer: the storage substrate behind a Vim buffer.
///
/// The buffer owns text, its line-ending metadata, its undo history and the
/// saved-state marker that drives the modified flag. Cursors, options and
/// extmarks belong to the editor and refer to this text through
/// [`LineCol`](crate::LineCol) and [`BytePos`](crate::BytePos) values.
///
/// Guarantees:
///
/// * Text is stored as UTF-8 and never normalized.
/// * Line queries exclude the line terminator, so `line(n)` returns the text
///   a user sees on that line.
/// * Edits are rejected unless both endpoints are UTF-8 and grapheme cluster
///   boundaries, so a deletion cannot split a combining sequence or a CRLF
///   pair.
/// * Line numbers are zero-based.
///
/// ```
/// use vanta_text::{Buffer, BytePos, LineNumber};
///
/// let mut buffer = Buffer::from_text("alpha\nbeta\n");
/// assert_eq!(buffer.len_lines(), 3);
/// assert_eq!(buffer.line(LineNumber::new(1)).expect("line exists").to_string(), "beta");
///
/// let removed = buffer
///     .remove(BytePos::new(0)..BytePos::new(6))
///     .expect("range covers whole characters and the newline");
/// assert_eq!(removed, "alpha\n");
/// assert_eq!(buffer.to_string(), "beta\n");
/// ```
#[derive(Clone)]
pub struct Buffer {
    text: Rope,
    line_ending: LineEnding,
    history: History,
}

impl Buffer {
    /// Creates an empty buffer with LF line endings.
    pub fn new() -> Self {
        Self {
            text: Rope::new(),
            line_ending: LineEnding::Lf,
            history: History::new(),
        }
    }

    /// Creates a buffer from `text`, preserving every byte.
    ///
    /// The line ending is detected from the first line terminator; see
    /// [`LineEnding::detect`].
    pub fn from_text(text: &str) -> Self {
        Self {
            text: Rope::from_str(text),
            line_ending: LineEnding::detect(text),
            history: History::new(),
        }
    }

    /// Returns the detected line ending of the buffer.
    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}
