//! Canonical byte, line/column and scalar conversions.
//!
//! `LineCol` is defined by the position units; this module is the only place
//! that applies its rule to a stored buffer. A position may name content
//! bytes and the end-of-line boundary, never a terminator byte or a byte in a
//! neighbouring line.

use super::Buffer;
use crate::error::TextError;
use crate::units::{ByteCol, BytePos, CharPos, LineCol, LineNumber};

impl Buffer {
    /// Converts a byte offset into a line and byte column.
    ///
    /// `pos` must be at or before the buffer end and on a character boundary.
    /// The result follows the canonical position rule defined by [`LineCol`]:
    /// content bytes and the end-of-line boundary have exactly one
    /// representation. Offsets inside a line terminator, such as the `\n` byte
    /// of a CRLF pair, have no `LineCol` and return
    /// [`TextError::NotGraphemeBoundary`].
    pub fn byte_to_line_col(&self, pos: BytePos) -> Result<LineCol, TextError> {
        let len = self.text.len_bytes();
        if pos.get() > len {
            return Err(TextError::PositionOutOfBounds {
                pos: pos.get(),
                len,
            });
        }
        if !self.is_char_boundary(pos) {
            return Err(TextError::NotCharBoundary { pos: pos.get() });
        }
        let line = LineNumber::new(self.text.byte_to_line(pos.get()));
        let line_start = self.text.line_to_byte(line.get());
        let col = pos.get() - line_start;
        if col > self.line_len_bytes(line)? {
            // Only the `\n` byte of a CRLF pair can land here. It is part of
            // the terminator's grapheme cluster and has no canonical position.
            return Err(TextError::NotGraphemeBoundary { pos: pos.get() });
        }
        Ok(LineCol {
            line,
            col: ByteCol::new(col),
        })
    }

    /// Converts a line and byte column into a byte offset.
    ///
    /// The column is content-relative, as defined by [`LineCol`]. A column
    /// past the line's content length is rejected with
    /// [`TextError::ColumnOutOfBounds`] instead of resolving into the next line
    /// or a terminator byte. A column inside a multi-byte character is
    /// rejected with [`TextError::NotCharBoundary`].
    pub fn line_col_to_byte(&self, position: LineCol) -> Result<BytePos, TextError> {
        let content_len = self.line_len_bytes(position.line)?;
        if position.col.get() > content_len {
            return Err(TextError::ColumnOutOfBounds {
                line: position.line.get(),
                col: position.col.get(),
                len: content_len,
            });
        }
        let pos = BytePos::new(self.text.line_to_byte(position.line.get()) + position.col.get());
        if !self.is_char_boundary(pos) {
            return Err(TextError::NotCharBoundary { pos: pos.get() });
        }
        Ok(pos)
    }

    /// Returns `true` when `pos` is at or before the end and on a UTF-8
    /// character boundary.
    pub fn is_char_boundary(&self, pos: BytePos) -> bool {
        let len = self.text.len_bytes();
        if pos.get() > len {
            return false;
        }
        if pos.get() == len {
            return true;
        }
        // `Rope::try_byte_to_char` clamps to the containing character, so it
        // cannot answer this question; inspect the chunk directly instead.
        let (chunk, chunk_byte_start, _, _) = self.text.chunk_at_byte(pos.get());
        chunk.is_char_boundary(pos.get() - chunk_byte_start)
    }

    /// Converts a byte offset into a scalar index.
    pub fn byte_to_char(&self, pos: BytePos) -> Result<CharPos, TextError> {
        let len = self.text.len_bytes();
        if pos.get() > len {
            return Err(TextError::PositionOutOfBounds {
                pos: pos.get(),
                len,
            });
        }
        if !self.is_char_boundary(pos) {
            return Err(TextError::NotCharBoundary { pos: pos.get() });
        }
        Ok(CharPos::new(self.text.byte_to_char(pos.get())))
    }

    /// Converts a scalar index into a byte offset.
    pub fn char_to_byte(&self, pos: CharPos) -> Result<BytePos, TextError> {
        if pos.get() > self.text.len_chars() {
            return Err(TextError::CharPositionOutOfBounds {
                pos: pos.get(),
                len_chars: self.text.len_chars(),
            });
        }
        Ok(BytePos::new(self.text.char_to_byte(pos.get())))
    }
}
