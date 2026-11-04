//! Vanta's text substrate.
//!
//! This crate owns text storage, measurement and the buffer-owned undo
//! history, and nothing else: no windows, no options and no I/O. It exists so
//! that every other crate can agree on what a position, a line, a grapheme and
//! a display cell mean.
//!
//! The rules implemented here come from `PROJECT_SPEC` §3C (Text manipulation)
//! and §5.3 (Component responsibilities):
//!
//! * Internal text is UTF-8. Byte offsets are the storage unit and are
//!   represented by [`BytePos`], never by a bare `usize`.
//! * Cursor movement and edits operate on grapheme clusters, so a deletion can
//!   never split a combining sequence, a ZWJ emoji or a CRLF pair.
//! * Display width is computed per grapheme cluster. Tabs expand to the next
//!   `tabstop` column.
//! * Text is never normalized. Bytes are preserved exactly; line endings are
//!   reported as metadata via [`LineEnding`].
//! * Undo and redo are owned by each [`Buffer`]: changes are grouped into
//!   blocks and replayed through the same validated edit primitive.
//!
//! ```
//! use vanta_text::{Buffer, BytePos, LineCol, LineNumber, ByteCol};
//!
//! let mut buffer = Buffer::from_text("first\nsecond\n");
//! assert_eq!(buffer.len_lines(), 3);
//!
//! let line = buffer.line(LineNumber::new(1)).expect("line 1 exists");
//! assert_eq!(line.to_string(), "second");
//!
//! buffer.insert(BytePos::ZERO, "# ").expect("start is an edit boundary");
//! assert_eq!(buffer.to_string(), "# first\nsecond\n");
//!
//! let start = buffer
//!     .line_col_to_byte(LineCol { line: LineNumber::new(0), col: ByteCol::new(0) })
//!     .expect("line start is valid");
//! assert_eq!(start, BytePos::ZERO);
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod buffer;
mod error;
mod grapheme;
mod line_ending;
mod units;
mod width;

pub use buffer::{Buffer, TextEdit, UndoEdit, UndoKind, UndoNode, UndoSnapshot};
pub use error::TextError;
pub use grapheme::{
    is_grapheme_boundary, next_grapheme_boundary, prev_grapheme_boundary, snap_to_grapheme_boundary,
};
pub use line_ending::LineEnding;
pub use units::{ByteCol, BytePos, CharPos, DisplayCol, LineCol, LineNumber};
pub use width::{
    byte_of_display_col, display_col_of_byte, grapheme_display_width, grapheme_width_at,
    line_display_width,
};
