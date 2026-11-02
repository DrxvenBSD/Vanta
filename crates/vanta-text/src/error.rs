//! Typed errors returned by [`Buffer`](crate::Buffer).

/// A failed text operation.
///
/// Every variant names the offending position and the bound it violated, so
/// the command layer can report an actionable message without re-deriving the
/// failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TextError {
    /// A byte offset was past the end of the buffer.
    #[error("byte position {pos} is outside the buffer length {len}")]
    PositionOutOfBounds {
        /// The requested byte offset.
        pos: usize,
        /// The buffer length in bytes.
        len: usize,
    },

    /// A scalar index was past the end of the buffer.
    #[error("character position {pos} is outside the buffer length {len_chars}")]
    CharPositionOutOfBounds {
        /// The requested scalar index.
        pos: usize,
        /// The buffer length in scalar values.
        len_chars: usize,
    },

    /// A byte offset fell inside a multi-byte UTF-8 sequence.
    #[error("byte position {pos} is not on a UTF-8 character boundary")]
    NotCharBoundary {
        /// The requested byte offset.
        pos: usize,
    },

    /// An edit position fell inside a grapheme cluster.
    #[error("byte position {pos} would split a grapheme cluster")]
    NotGraphemeBoundary {
        /// The requested byte offset.
        pos: usize,
    },

    /// A line number was past the end of the buffer.
    #[error("line {line} is outside the buffer line count {line_count}")]
    LineOutOfBounds {
        /// The requested zero-based line.
        line: usize,
        /// The number of lines in the buffer.
        line_count: usize,
    },

    /// A byte column was past the end of its line's content.
    #[error("byte column {col} is outside line {line} of content length {len}")]
    ColumnOutOfBounds {
        /// The requested zero-based line.
        line: usize,
        /// The requested byte column.
        col: usize,
        /// The content length of the line, excluding its terminator.
        len: usize,
    },

    /// An edit range ended before it started.
    #[error("invalid edit range {start}..{end}: the end precedes the start")]
    InvalidRange {
        /// The range start byte offset.
        start: usize,
        /// The range end byte offset.
        end: usize,
    },
}
