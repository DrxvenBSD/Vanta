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
use std::io::{self, Read};

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

    /// Builds a buffer from a UTF-8 byte stream without materializing the
    /// whole text as one string.
    ///
    /// The reader is consumed incrementally while the rope is built; the
    /// caller's source can therefore be a decoded file stream. The line ending
    /// is detected from the first `\n` in the stream. Invalid UTF-8 returns an
    /// I/O error and leaves no partially constructed buffer observable.
    ///
    /// # Errors
    ///
    /// Returns [`io::Error`] when the reader fails or supplies invalid UTF-8.
    pub fn from_reader<R: Read>(reader: R) -> io::Result<Self> {
        let mut reader = LineEndingReader::new(reader);
        let text = Rope::from_reader(&mut reader)?;
        Ok(Self {
            text,
            line_ending: reader.line_ending(),
            history: History::new(),
        })
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

/// Passes bytes through while recording the first line terminator.
struct LineEndingReader<R> {
    inner: R,
    line_ending: LineEnding,
    seen_newline: bool,
    previous_cr: bool,
}

impl<R: Read> LineEndingReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            line_ending: LineEnding::Lf,
            seen_newline: false,
            previous_cr: false,
        }
    }

    fn line_ending(&self) -> LineEnding {
        self.line_ending
    }
}

impl<R: Read> Read for LineEndingReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(output)?;
        for &byte in &output[..count] {
            if byte == b'\n' && !self.seen_newline {
                self.line_ending = if self.previous_cr {
                    LineEnding::Crlf
                } else {
                    LineEnding::Lf
                };
                self.seen_newline = true;
            }
            self.previous_cr = byte == b'\r';
        }
        Ok(count)
    }
}
