//! Text mutation and the edit record.
//!
//! [`Buffer::apply`] is the single replacement primitive. It validates the
//! whole range before touching the rope, so a rejected edit leaves the buffer
//! unchanged. Every edit produces a [`TextEdit`]. The sibling `history` module
//! owns [`Buffer`](super::Buffer)'s undo blocks (`PROJECT_SPEC` §5.12),
//! grouping one or more `TextEdit`s with the cursor before and after; input
//! code never builds its own edit record.

use std::ops::Range;

use super::Buffer;
use crate::error::TextError;
use crate::grapheme::is_grapheme_boundary;
use crate::units::BytePos;

/// A single text replacement.
///
/// The start is the byte offset where the replaced range began. `removed` is
/// the exact text taken out of the buffer and `inserted` is the exact text put
/// in its place, both in UTF-8 bytes.
///
/// A `TextEdit` is the unit an undo entry inverts. Replacing `start..end`
/// with `removed` restores the previous bytes; replacing
/// `start..start + removed.len()` with `inserted` restores the new bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    start: BytePos,
    removed: String,
    inserted: String,
}

impl TextEdit {
    /// Reconstructs an edit for undo persistence.
    ///
    /// The caller must have validated `start`, `removed` and `inserted` against
    /// the buffer state represented by the surrounding undo snapshot.
    pub fn from_parts(start: BytePos, removed: String, inserted: String) -> Self {
        Self {
            start,
            removed,
            inserted,
        }
    }

    /// Returns the byte offset where the replaced range began.
    pub fn start(&self) -> BytePos {
        self.start
    }

    /// Returns the byte offset immediately after the inserted text.
    pub fn end(&self) -> BytePos {
        BytePos::new(self.start.get() + self.inserted.len())
    }

    /// Returns the text removed from the buffer.
    pub fn removed(&self) -> &str {
        &self.removed
    }

    /// Returns the text inserted into the buffer.
    pub fn inserted(&self) -> &str {
        &self.inserted
    }
}

impl Buffer {
    /// Replaces `range` with `text` as one validated edit.
    ///
    /// This is the primitive behind [`Buffer::insert`] and
    /// [`Buffer::remove`]. Both endpoints must be UTF-8 and grapheme cluster
    /// boundaries. All validation happens before the rope is modified, so an
    /// error leaves the buffer byte-for-byte unchanged.
    ///
    /// The returned [`TextEdit`] is the text-level record an undo block
    /// stores.
    ///
    /// # Errors
    ///
    /// Returns [`TextError::InvalidRange`] when the end precedes the start,
    /// [`TextError::PositionOutOfBounds`] when the end is past the buffer,
    /// [`TextError::NotCharBoundary`] inside a multi-byte character and
    /// [`TextError::NotGraphemeBoundary`] inside a grapheme cluster.
    pub fn apply(&mut self, range: Range<BytePos>, text: &str) -> Result<TextEdit, TextError> {
        if range.start.get() > range.end.get() {
            return Err(TextError::InvalidRange {
                start: range.start.get(),
                end: range.end.get(),
            });
        }
        let len = self.text.len_bytes();
        if range.end.get() > len {
            return Err(TextError::PositionOutOfBounds {
                pos: range.end.get(),
                len,
            });
        }
        self.check_edit_position(range.start)?;
        if range.start != range.end {
            self.check_edit_position(range.end)?;
        }

        let removed = self
            .text
            .byte_slice(range.start.get()..range.end.get())
            .to_string();
        let char_start = self.text.byte_to_char(range.start.get());
        let char_end = self.text.byte_to_char(range.end.get());
        self.text.remove(char_start..char_end);
        self.text.insert(char_start, text);

        Ok(TextEdit {
            start: range.start,
            removed,
            inserted: text.to_owned(),
        })
    }

    /// Inserts `text` at `pos`.
    ///
    /// `pos` must be a grapheme cluster boundary. Inserting at the end of the
    /// buffer is always allowed.
    pub fn insert(&mut self, pos: BytePos, text: &str) -> Result<(), TextError> {
        self.apply(pos..pos, text).map(|_| ())
    }

    /// Removes the text in `range` and returns it.
    ///
    /// Both endpoints must be grapheme cluster boundaries. An empty range is
    /// a no-op that returns an empty string.
    pub fn remove(&mut self, range: Range<BytePos>) -> Result<String, TextError> {
        self.apply(range, "").map(|change| change.removed)
    }

    /// Rejects an edit position that is out of bounds, splits a UTF-8
    /// sequence or splits a grapheme cluster.
    fn check_edit_position(&self, pos: BytePos) -> Result<(), TextError> {
        if !self.is_char_boundary(pos) {
            return Err(TextError::NotCharBoundary { pos: pos.get() });
        }
        let position = self.byte_to_line_col(pos)?;
        // The raw line includes the terminator so a position between CR and
        // LF is correctly rejected as a cluster split.
        let raw = self.raw_line(position.line)?.to_string();
        if !is_grapheme_boundary(&raw, position.col.get()) {
            return Err(TextError::NotGraphemeBoundary { pos: pos.get() });
        }
        Ok(())
    }
}
