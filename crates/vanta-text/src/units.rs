//! Unit-typed text positions.
//!
//! Vanta does not pass bare `usize` values for text positions. Every value
//! that refers to text carries its unit in the type, so a byte offset cannot
//! be mistaken for a character index, a line number or a screen column.

/// A byte offset from the start of a buffer, in UTF-8 bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BytePos(usize);

impl BytePos {
    /// The offset of the first byte in a buffer.
    pub const ZERO: Self = Self(0);

    /// Creates a byte offset from a raw value.
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    /// Returns the raw offset in bytes.
    pub const fn get(self) -> usize {
        self.0
    }
}

impl From<BytePos> for usize {
    fn from(pos: BytePos) -> Self {
        pos.0
    }
}

/// A Unicode scalar value index from the start of a buffer, in `char`s.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CharPos(usize);

impl CharPos {
    /// The index of the first scalar value in a buffer.
    pub const ZERO: Self = Self(0);

    /// Creates a scalar index from a raw value.
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    /// Returns the raw index in scalar values.
    pub const fn get(self) -> usize {
        self.0
    }
}

/// A zero-based line number.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LineNumber(usize);

impl LineNumber {
    /// The first line of a buffer.
    pub const ZERO: Self = Self(0);

    /// Creates a line number from a raw, zero-based value.
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    /// Returns the raw, zero-based line number.
    pub const fn get(self) -> usize {
        self.0
    }
}

/// A byte offset within a line's content.
///
/// A column is measured from the first byte of the line's content, which
/// excludes the line terminator. Valid values and the meaning of the
/// end-of-line column are defined by [`LineCol`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteCol(usize);

impl ByteCol {
    /// The first byte of a line.
    pub const ZERO: Self = Self(0);

    /// Creates a byte column from a raw value.
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    /// Returns the raw byte column.
    pub const fn get(self) -> usize {
        self.0
    }
}

/// A terminal cell column, measured in display cells from the left edge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DisplayCol(usize);

impl DisplayCol {
    /// The leftmost cell column.
    pub const ZERO: Self = Self(0);

    /// Creates a display column from a raw cell count.
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    /// Returns the raw cell column.
    pub const fn get(self) -> usize {
        self.0
    }
}

/// A line together with a byte column inside that line's content.
///
/// This type is the canonical definition of a text position:
///
/// * `col` is a byte offset in the line's **content**, which excludes the
///   line terminator. Valid columns are `0..=content_len`.
/// * `col == content_len` is the end-of-line position immediately before the
///   terminator, or the buffer end on the last line.
/// * Terminator bytes have no columns of their own. On a CRLF line the `\r`
///   byte is addressed by `col == content_len`; the `\n` byte has no
///   `LineCol` because it lies inside the CRLF grapheme cluster.
/// * A `LineCol` never resolves to a different line.
///
/// [`Buffer::byte_to_line_col`](crate::Buffer::byte_to_line_col) and
/// [`Buffer::line_col_to_byte`](crate::Buffer::line_col_to_byte) enforce this
/// rule and reject out-of-range input with [`TextError`](crate::TextError)
/// rather than clamping.
///
/// Columns are byte offsets, not display cells; convert with
/// [`Buffer::display_col_of`](crate::Buffer::display_col_of) for screen
/// placement. Edits additionally require grapheme cluster boundaries, so a
/// column can be a valid position without being an edit boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LineCol {
    /// The zero-based line number.
    pub line: LineNumber,
    /// The byte offset within the line's content.
    pub col: ByteCol,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newtypes_expose_their_raw_values() {
        assert_eq!(BytePos::new(3).get(), 3);
        assert_eq!(CharPos::new(4).get(), 4);
        assert_eq!(LineNumber::new(5).get(), 5);
        assert_eq!(ByteCol::new(6).get(), 6);
        assert_eq!(DisplayCol::new(7).get(), 7);
        assert_eq!(usize::from(BytePos::new(8)), 8);
    }

    #[test]
    fn positions_order_lexicographically() {
        let first = LineCol {
            line: LineNumber::new(1),
            col: ByteCol::new(9),
        };
        let second = LineCol {
            line: LineNumber::new(2),
            col: ByteCol::new(0),
        };
        assert!(first < second);
    }
}
