//! Display-width queries for whole lines.
//!
//! The cluster and tabstop math lives in the width module; these methods only
//! connect it to a line of the buffer.

use super::Buffer;
use crate::error::TextError;
use crate::units::{BytePos, DisplayCol, LineNumber};
use crate::width;

impl Buffer {
    /// Returns the display width of `line` when rendered from column zero.
    ///
    /// Tabs expand to the next `tabstop` boundary; see
    /// [`crate::line_display_width`].
    pub fn line_display_width(
        &self,
        line: LineNumber,
        tabstop: usize,
    ) -> Result<DisplayCol, TextError> {
        Ok(width::line_display_width(&self.line_text(line)?, tabstop))
    }

    /// Returns the display column of `pos` within its line.
    ///
    /// A position inside a line terminator reports the column at the end of
    /// the line's content. See [`crate::display_col_of_byte`].
    pub fn display_col_of(&self, pos: BytePos, tabstop: usize) -> Result<DisplayCol, TextError> {
        let position = self.byte_to_line_col(pos)?;
        let text = self.line_text(position.line)?;
        Ok(width::display_col_of_byte(
            &text,
            position.col.get(),
            tabstop,
        ))
    }
}
