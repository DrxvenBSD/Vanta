//! Rope storage and line-index queries.
//!
//! This module owns the rope and the definition of a line's content range.
//! Position conversion, display measurement and editing consume these queries
//! rather than reaching into the rope themselves.

use std::fmt;
use std::ops::Range;

use ropey::RopeSlice;

use super::Buffer;
use crate::error::TextError;
use crate::units::{BytePos, LineNumber};

impl Buffer {
    /// Returns the buffer length in bytes.
    pub fn len_bytes(&self) -> usize {
        self.text.len_bytes()
    }

    /// Returns the buffer length in Unicode scalar values.
    pub fn len_chars(&self) -> usize {
        self.text.len_chars()
    }

    /// Returns the buffer length in lines.
    ///
    /// An empty buffer has one line, matching Vim's empty buffer. Text that
    /// ends with a terminator has a final empty line.
    pub fn len_lines(&self) -> usize {
        self.text.len_lines()
    }

    /// Returns the content of `line`, excluding its terminator.
    ///
    /// The returned slice borrows the rope without copying. For an owned copy
    /// use [`Buffer::line_text`].
    pub fn line(&self, line: LineNumber) -> Result<RopeSlice<'_>, TextError> {
        let raw = self.raw_line(line)?;
        let content_len = raw.len_bytes() - terminator_len(&raw);
        // ropey's `RopeSlice::slice` takes a char range; Vanta positions are
        // byte offsets, so byte slicing is explicit here.
        Ok(raw.byte_slice(..content_len))
    }

    /// Returns the content of `line` as an owned string, without its
    /// terminator.
    pub fn line_text(&self, line: LineNumber) -> Result<String, TextError> {
        Ok(self.line(line)?.to_string())
    }

    /// Returns the byte range of `line`'s content, excluding its terminator.
    pub fn line_byte_range(&self, line: LineNumber) -> Result<Range<BytePos>, TextError> {
        let raw = self.raw_line(line)?;
        let start = self.text.line_to_byte(line.get());
        let end = start + raw.len_bytes() - terminator_len(&raw);
        Ok(BytePos::new(start)..BytePos::new(end))
    }

    /// Returns the content length of `line` in bytes, excluding its
    /// terminator.
    pub fn line_len_bytes(&self, line: LineNumber) -> Result<usize, TextError> {
        let raw = self.raw_line(line)?;
        Ok(raw.len_bytes() - terminator_len(&raw))
    }

    /// Returns a copy of the text in `range`.
    ///
    /// Used for registers, clipboard payloads and diagnostics; hot-path
    /// rendering should read [`Buffer::line`] slices instead.
    pub fn slice_text(&self, range: Range<BytePos>) -> Result<String, TextError> {
        if range.start.get() > range.end.get() {
            return Err(TextError::InvalidRange {
                start: range.start.get(),
                end: range.end.get(),
            });
        }
        if range.end.get() > self.text.len_bytes() {
            return Err(TextError::PositionOutOfBounds {
                pos: range.end.get(),
                len: self.text.len_bytes(),
            });
        }
        if !self.is_char_boundary(range.start) {
            return Err(TextError::NotCharBoundary {
                pos: range.start.get(),
            });
        }
        if !self.is_char_boundary(range.end) {
            return Err(TextError::NotCharBoundary {
                pos: range.end.get(),
            });
        }
        Ok(self
            .text
            .byte_slice(range.start.get()..range.end.get())
            .to_string())
    }

    /// Returns the raw rope line, including its terminator.
    ///
    /// Editing also needs the terminator to reject a position between CR and
    /// LF, so this is visible to the sibling edit module.
    pub(super) fn raw_line(&self, line: LineNumber) -> Result<RopeSlice<'_>, TextError> {
        if line.get() >= self.text.len_lines() {
            return Err(TextError::LineOutOfBounds {
                line: line.get(),
                line_count: self.text.len_lines(),
            });
        }
        Ok(self.text.line(line.get()))
    }
}

impl fmt::Display for Buffer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for chunk in self.text.chunks() {
            formatter.write_str(chunk)?;
        }
        Ok(())
    }
}

impl fmt::Debug for Buffer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Buffer")
            .field("len_bytes", &self.len_bytes())
            .field("len_lines", &self.len_lines())
            .field("line_ending", &self.line_ending)
            .finish()
    }
}

/// Returns the byte length of the line terminator at the end of `line`.
fn terminator_len(line: &RopeSlice<'_>) -> usize {
    let len = line.len_bytes();
    if len == 0 || line.byte(len - 1) != b'\n' {
        return 0;
    }
    if len >= 2 && line.byte(len - 2) == b'\r' {
        2
    } else {
        1
    }
}
