//! Range-based editing operations.
//!
//! Normal-mode keys resolve a [`TextRange`] and call these methods; the text
//! verbs themselves are not key-specific, so Visual mode can reuse the same
//! delete and yank paths. Every mutating operation here records exactly one
//! undo block and reports the register text it removed.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;
use vanta_text::{
    ByteCol, BytePos, DisplayCol, LineCol, LineNumber, TextError, byte_of_display_col,
    display_col_of_byte, line_display_width, next_grapheme_boundary,
};

use super::Document;

/// A resolved range of buffer text.
///
/// Characterwise ranges are exact byte ranges. Linewise ranges name whole
/// lines; the text layer resolves their register form and deletion bytes using
/// the buffer's line-ending style.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TextRange {
    /// An exact byte range.
    Charwise(Range<BytePos>),
    /// A half-open range of whole lines.
    Linewise(Range<LineNumber>),
}

impl TextRange {
    /// Returns `true` when this range addresses whole lines.
    pub(crate) fn is_linewise(&self) -> bool {
        matches!(self, Self::Linewise(_))
    }
}

/// The two Visual-mode selection granularities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VisualKind {
    /// Select the graphemes between the anchor and the cursor, inclusive.
    Charwise,
    /// Select whole lines between the anchor line and the cursor line.
    Linewise,
    /// A screen-column rectangle. `to_eol` extends every line to its end.
    Blockwise { to_eol: bool },
}

/// A resolved rectangular selection in display columns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BlockSelection {
    pub(crate) start_line: LineNumber,
    pub(crate) end_line: LineNumber,
    pub(crate) start_col: usize,
    pub(crate) end_col: usize,
    pub(crate) to_eol: bool,
}

impl Document {
    /// Transforms the text in `range` as one undo block.
    pub(crate) fn transform_text(
        &mut self,
        range: &TextRange,
        transform: impl Fn(&str) -> String,
    ) -> Result<bool, TextError> {
        match range {
            TextRange::Charwise(bytes) => {
                let text = self.buffer.slice_text(bytes.clone())?;
                let replacement = transform(&text);
                if replacement == text {
                    return Ok(false);
                }
                let start = bytes.start;
                self.break_undo();
                let (edit, before) = self.stage_edit(bytes.clone(), &replacement)?;
                let after = self.buffer.byte_to_line_col(start)?;
                self.commit_edit(edit, before, after);
                Ok(true)
            }
            TextRange::Linewise(lines) => {
                if lines.start >= lines.end {
                    return Ok(false);
                }
                let eol = self.line_ending_text();
                let mut replacement = String::new();
                let mut changed = false;
                for line in lines.start.get()..lines.end.get() {
                    let line = LineNumber::new(line);
                    let content = self.buffer.line_text(line)?;
                    let transformed = transform(&content);
                    changed |= transformed != content;
                    replacement.push_str(&transformed);
                    replacement.push_str(eol);
                }
                if !changed {
                    return Ok(false);
                }
                let bounds = self.linewise_delete_bounds(lines)?;
                if bounds.start == bounds.end {
                    return Ok(false);
                }
                self.break_undo();
                let (edit, before) = self.stage_edit(bounds.clone(), &replacement)?;
                let after = self
                    .buffer
                    .byte_to_line_col(self.line_start(lines.start)?)?;
                self.commit_edit(edit, before, after);
                Ok(true)
            }
        }
    }

    /// Indents (`right`) or dedents the addressed lines by eight cells.
    pub(crate) fn shift_lines(
        &mut self,
        range: &TextRange,
        right: bool,
    ) -> Result<bool, TextError> {
        self.transform_text(range, |line| {
            if right {
                format!("        {line}")
            } else {
                let mut remaining = line;
                let mut removed = 0;
                while removed < 8 {
                    if let Some(rest) = remaining.strip_prefix(' ') {
                        remaining = rest;
                        removed += 1;
                    } else if let Some(rest) = remaining.strip_prefix('\t') {
                        remaining = rest;
                        removed = 8;
                    } else {
                        break;
                    }
                }
                remaining.to_string()
            }
        })
    }

    /// Joins the current line with the next `count - 1` lines.
    pub(crate) fn join_lines(&mut self, count: usize, spaces: bool) -> Result<bool, TextError> {
        let mut changed = false;
        let line = self.effective_line(self.cursor.line);
        for _ in 1..count.max(1) {
            if line.get() + 1 >= self.line_count() {
                break;
            }
            let next = LineNumber::new(line.get() + 1);
            let next_text = self.buffer.line_text(next)?;
            let leading = next_text.len() - next_text.trim_start().len();
            let end = self.buffer.line_byte_range(line)?.end;
            let start = self.buffer.line_byte_range(next)?.start.get() + leading;
            let replacement = if spaces { " " } else { "" };
            self.break_undo();
            let (edit, before) =
                self.stage_edit(BytePos::new(end.get())..BytePos::new(start), replacement)?;
            let after = self
                .buffer
                .byte_to_line_col(BytePos::new(end.get() + replacement.len()))?;
            self.commit_edit(edit, before, after);
            changed = true;
        }
        Ok(changed)
    }

    /// Moves to the first non-blank byte on the current line.
    pub(crate) fn move_to_first_nonblank(&mut self) {
        let text = self.line_text();
        let column = text
            .char_indices()
            .find(|(_, character)| !character.is_whitespace())
            .map_or(0, |(offset, _)| offset);
        self.cursor.col = ByteCol::new(column);
        self.preferred_col = None;
    }

    /// Resolves a Visual selection into the same range verbs used in Normal
    /// mode.
    ///
    /// Characterwise selections include the grapheme under the higher
    /// endpoint, regardless of whether the anchor is before or after the
    /// cursor. A cursor at end of line has no grapheme to add, so the range
    /// ends at that boundary. Linewise selections resolve both endpoints
    /// through the effective-line rule, so the synthetic final empty line of a
    /// trailing terminator selects the last visible line instead.
    pub(crate) fn visual_range(
        &self,
        anchor: LineCol,
        kind: VisualKind,
    ) -> Result<TextRange, TextError> {
        match kind {
            VisualKind::Linewise => {
                let anchor_line = self.effective_line(anchor.line);
                let cursor_line = self.effective_line(self.cursor.line);
                let (first, last) = if anchor_line <= cursor_line {
                    (anchor_line, cursor_line)
                } else {
                    (cursor_line, anchor_line)
                };
                Ok(TextRange::Linewise(first..LineNumber::new(last.get() + 1)))
            }
            VisualKind::Blockwise { .. } => {
                let first = self.effective_line(anchor.line.min(self.cursor.line));
                let last = self.effective_line(anchor.line.max(self.cursor.line));
                Ok(TextRange::Linewise(first..LineNumber::new(last.get() + 1)))
            }
            VisualKind::Charwise => {
                let anchor_byte = self.buffer.line_col_to_byte(anchor)?;
                let cursor_byte = self.buffer.line_col_to_byte(self.cursor)?;
                let (start, end_position) = if anchor_byte <= cursor_byte {
                    (anchor_byte, self.cursor)
                } else {
                    (cursor_byte, anchor)
                };
                let end_line = self.buffer.line_text(end_position.line)?;
                let column = end_position.col.get().min(end_line.len());
                let end = if column < end_line.len() {
                    let next = next_grapheme_boundary(&end_line, column).unwrap_or(end_line.len());
                    BytePos::new(self.line_start(end_position.line)?.get() + next)
                } else {
                    self.buffer.line_col_to_byte(end_position)?
                };
                Ok(TextRange::Charwise(start..end))
            }
        }
    }

    /// Resolves a block selection by display column. Short lines contribute
    /// empty cells; a `$` block extends each line independently.
    pub(crate) fn block_selection(
        &self,
        anchor: LineCol,
        kind: VisualKind,
    ) -> Result<BlockSelection, TextError> {
        let to_eol = matches!(kind, VisualKind::Blockwise { to_eol: true });
        let first = self.effective_line(anchor.line.min(self.cursor.line));
        let last = self.effective_line(anchor.line.max(self.cursor.line));
        let anchor_text = self.buffer.line_text(self.effective_line(anchor.line))?;
        let cursor_text = self.buffer.line_text(self.cursor.line)?;
        let anchor_col =
            display_col_of_byte(&anchor_text, anchor.col.get(), crate::view::TABSTOP).get();
        let cursor_col =
            display_col_of_byte(&cursor_text, self.cursor.col.get(), crate::view::TABSTOP).get();
        let start_col = anchor_col.min(cursor_col);
        let mut end_col = anchor_col.max(cursor_col).saturating_add(1);
        if to_eol {
            for line in first.get()..=last.get() {
                let text = self.buffer.line_text(LineNumber::new(line))?;
                end_col = end_col.max(line_display_width(&text, crate::view::TABSTOP).get());
            }
        }
        if end_col <= start_col {
            end_col = start_col + 1;
        }
        Ok(BlockSelection {
            start_line: first,
            end_line: last,
            start_col,
            end_col,
            to_eol,
        })
    }

    /// Returns the register text for a block, one line per selected row.
    pub(crate) fn block_text(&self, block: &BlockSelection) -> Result<String, TextError> {
        let mut text = String::new();
        for line in block.start_line.get()..=block.end_line.get() {
            if line > block.start_line.get() {
                text.push('\n');
            }
            let line = LineNumber::new(line);
            let content = self.buffer.line_text(line)?;
            let start = byte_of_display_col(
                &content,
                DisplayCol::new(block.start_col),
                crate::view::TABSTOP,
            );
            let end = if block.to_eol {
                content.len()
            } else {
                byte_of_display_col(
                    &content,
                    DisplayCol::new(block.end_col),
                    crate::view::TABSTOP,
                )
            }
            .min(content.len());
            if start < end {
                text.push_str(&content[start..end]);
            }
        }
        Ok(text)
    }

    /// Deletes a block as one whole-line replacement and returns its text.
    pub(crate) fn block_delete(
        &mut self,
        block: &BlockSelection,
    ) -> Result<Option<String>, TextError> {
        let text = self.block_text(block)?;
        let first = block.start_line;
        let last = block.end_line;
        let eol = self.line_ending_text();
        let mut replacement = String::new();
        for line in first.get()..=last.get() {
            if line > first.get() {
                replacement.push_str(eol);
            }
            let content = self.buffer.line_text(LineNumber::new(line))?;
            let start = byte_of_display_col(
                &content,
                DisplayCol::new(block.start_col),
                crate::view::TABSTOP,
            );
            let end = if block.to_eol {
                content.len()
            } else {
                byte_of_display_col(
                    &content,
                    DisplayCol::new(block.end_col),
                    crate::view::TABSTOP,
                )
            }
            .min(content.len());
            replacement.push_str(&content[..start]);
            replacement.push_str(&content[end..]);
        }
        let start = self.line_start(first)?;
        let end = self.buffer.line_byte_range(last)?.end;
        self.break_undo();
        let (edit, before) = self.stage_edit(start..end, &replacement)?;
        let line_text = self.buffer.line_text(first)?;
        let column = byte_of_display_col(
            &line_text,
            DisplayCol::new(block.start_col),
            crate::view::TABSTOP,
        );
        let after = LineCol {
            line: first,
            col: ByteCol::new(column.min(line_text.len())),
        };
        self.commit_edit(edit, before, after);
        Ok(Some(text))
    }

    /// Inserts block text at the cursor's display column across consecutive
    /// lines. Used for block puts; the first line sets the resulting cursor.
    pub(crate) fn put_block(&mut self, text: &str, after: bool) -> Result<(), TextError> {
        let lines: Vec<&str> = text.split('\n').collect();
        let start_line = self.effective_line(self.cursor.line);
        let first_text = self.buffer.line_text(start_line)?;
        let base =
            display_col_of_byte(&first_text, self.cursor.col.get(), crate::view::TABSTOP).get();
        let column = if after {
            let next = next_grapheme_boundary(&first_text, self.cursor.col.get())
                .unwrap_or(first_text.len());
            display_col_of_byte(&first_text, next, crate::view::TABSTOP)
                .get()
                .max(base + 1)
        } else {
            base
        };
        let eol = self.line_ending_text();
        let last_line = LineNumber::new(
            (start_line.get() + lines.len().saturating_sub(1))
                .min(self.buffer.len_lines().saturating_sub(1)),
        );
        let mut replacement = String::new();
        for offset in 0..=(last_line.get().saturating_sub(start_line.get())) {
            if offset > 0 {
                replacement.push_str(eol);
            }
            let line = LineNumber::new(start_line.get() + offset);
            let content = self.buffer.line_text(line)?;
            let byte = byte_of_display_col(&content, DisplayCol::new(column), crate::view::TABSTOP);
            replacement.push_str(&content[..byte.min(content.len())]);
            replacement.push_str(lines.get(offset).copied().unwrap_or(""));
            replacement.push_str(&content[byte.min(content.len())..]);
        }
        let start = self.line_start(start_line)?;
        let end = self.buffer.line_byte_range(last_line)?.end;
        self.break_undo();
        let (edit, before) = self.stage_edit(start..end, &replacement)?;
        let new_text = self.buffer.line_text(start_line)?;
        let byte = byte_of_display_col(&new_text, DisplayCol::new(column), crate::view::TABSTOP);
        let after = LineCol {
            line: start_line,
            col: ByteCol::new(byte.min(new_text.len())),
        };
        self.commit_edit(edit, before, after);
        Ok(())
    }

    /// Inserts `text` at a display column on each of `count` lines, used to
    /// replicate one block-insert line after the first line was typed.
    pub(crate) fn insert_block_rows(
        &mut self,
        start_line: LineNumber,
        count: usize,
        column: usize,
        text: &str,
    ) -> Result<(), TextError> {
        if count == 0 || text.is_empty() {
            return Ok(());
        }
        let last = LineNumber::new(
            (start_line.get() + count - 1).min(self.buffer.len_lines().saturating_sub(1)),
        );
        if last < start_line {
            return Ok(());
        }
        let eol = self.line_ending_text();
        let mut replacement = String::new();
        for line in start_line.get()..=last.get() {
            if line > start_line.get() {
                replacement.push_str(eol);
            }
            let content = self.buffer.line_text(LineNumber::new(line))?;
            let byte = byte_of_display_col(&content, DisplayCol::new(column), crate::view::TABSTOP)
                .min(content.len());
            replacement.push_str(&content[..byte]);
            replacement.push_str(text);
            replacement.push_str(&content[byte..]);
        }
        let start = self.line_start(start_line)?;
        let end = self.buffer.line_byte_range(last)?.end;
        let (edit, before) = self.stage_edit(start..end, &replacement)?;
        self.commit_edit(edit, before, before);
        Ok(())
    }

    /// Moves to a display column on `line`, clamping short lines and tabs.
    pub(crate) fn move_to_display_column(
        &mut self,
        line: LineNumber,
        column: usize,
    ) -> Result<(), TextError> {
        let text = self.buffer.line_text(line)?;
        let byte = byte_of_display_col(&text, DisplayCol::new(column), crate::view::TABSTOP)
            .min(text.len());
        self.cursor = LineCol {
            line,
            col: ByteCol::new(byte),
        };
        self.preferred_col = None;
        Ok(())
    }

    /// Moves the cursor to the first byte of `range`.
    ///
    /// Yanking a Visual selection leaves the cursor at the start of the text
    /// that was taken, matching Vim.
    pub(crate) fn move_to_range_start(&mut self, range: &TextRange) -> Result<(), TextError> {
        let position = match range {
            TextRange::Charwise(range) => range.start,
            TextRange::Linewise(lines) => self.line_start(lines.start)?,
        };
        self.move_to(position)
    }

    /// Replaces a Visual selection for `c`.
    ///
    /// Characterwise change is a delete; the caller enters Insert at the
    /// resulting cursor. A linewise change replaces the selected lines with a
    /// single empty line so the user gets the blank line Vim leaves behind.
    /// Both forms are one edit and therefore one undo block.
    pub(crate) fn change(&mut self, range: TextRange) -> Result<Option<String>, TextError> {
        let TextRange::Linewise(lines) = &range else {
            return self.delete(range);
        };
        if lines.start >= lines.end {
            return Ok(None);
        }

        let text = self.linewise_text(lines)?;
        let bounds = self.linewise_delete_bounds(lines)?;
        if bounds.start == bounds.end {
            return Ok(None);
        }

        let eol = self.line_ending_text();
        self.break_undo();
        let (edit, before) = self.stage_edit(bounds, eol)?;
        let line = LineNumber::new(lines.start.get().min(self.buffer.len_lines() - 1));
        let after = self.buffer.byte_to_line_col(self.line_start(line)?)?;
        self.commit_edit(edit, before, after);
        Ok(Some(text))
    }

    /// Returns the grapheme cluster under the cursor.
    ///
    /// The range is empty at the end of a line, so `x` becomes a no-op there.
    pub(crate) fn char_range_at_cursor(&self) -> Result<TextRange, TextError> {
        let line = self.line_text();
        let line_start = self.line_start(self.cursor.line)?;
        let column = self.cursor.col.get().min(line.len());
        if column >= line.len() {
            let position = BytePos::new(line_start.get() + column);
            return Ok(TextRange::Charwise(position..position));
        }

        let end = next_grapheme_boundary(&line, column).unwrap_or(line.len());
        Ok(TextRange::Charwise(
            BytePos::new(line_start.get() + column)..BytePos::new(line_start.get() + end),
        ))
    }

    /// Returns the range from the cursor to the next word start, or from the
    /// previous word start to the cursor.
    ///
    /// Words are runs of non-whitespace grapheme clusters. The search stays on
    /// the cursor's line; a missing word yields an empty range. `w`, `b`, `dw`
    /// and `db` all consume this one motion result.
    /// Moves Normal-mode `w` to the next word start, crossing line ends.
    ///
    /// Inside a line this uses the same [`Document::word_range`] result as
    /// `dw`. At the end of a line or word, Vim moves to the first non-blank of
    /// the next line, stops on an empty line, and moves to the last grapheme
    /// when no further word exists. Without this crossing, `w` dead-ends on
    /// the last word of every line.
    pub(crate) fn move_word_forward(&mut self) -> Result<(), TextError> {
        self.preferred_col = None;
        let line = self.cursor.line;
        let text = self.line_text();
        if let TextRange::Charwise(range) = self.word_range(true)? {
            let line_start = self.line_start(line)?;
            let end_col = range.end.get() - line_start.get();
            if end_col < text.len() {
                self.cursor = LineCol {
                    line,
                    col: ByteCol::new(end_col),
                };
                return Ok(());
            }
        }

        let last = self.line_count().saturating_sub(1);
        let mut next = line.get() + 1;
        while next <= last {
            let target = LineNumber::new(next);
            let content = self.buffer.line_text(target)?;
            if content.is_empty() {
                self.cursor = LineCol {
                    line: target,
                    col: ByteCol::ZERO,
                };
                return Ok(());
            }
            if let Some((offset, _)) = content
                .grapheme_indices(true)
                .find(|(_, grapheme)| !grapheme.chars().all(char::is_whitespace))
            {
                self.cursor = LineCol {
                    line: target,
                    col: ByteCol::new(offset),
                };
                return Ok(());
            }
            next += 1;
        }

        if let Some((offset, _)) = text.grapheme_indices(true).next_back() {
            self.cursor = LineCol {
                line,
                col: ByteCol::new(offset),
            };
        }
        Ok(())
    }

    pub(crate) fn word_range(&self, forward: bool) -> Result<TextRange, TextError> {
        let line = self.line_text();
        let line_start = self.line_start(self.cursor.line)?;
        let cursor = self.cursor.col.get().min(line.len());
        let clusters: Vec<(usize, usize, bool)> = line
            .grapheme_indices(true)
            .map(|(start, grapheme)| {
                (
                    start,
                    start + grapheme.len(),
                    grapheme.chars().all(char::is_whitespace),
                )
            })
            .collect();

        let mut index = clusters.partition_point(|(_, end, _)| *end <= cursor);
        if forward {
            if index >= clusters.len() {
                let position = BytePos::new(line_start.get() + cursor);
                return Ok(TextRange::Charwise(position..position));
            }
            if clusters[index].2 {
                while index < clusters.len() && clusters[index].2 {
                    index += 1;
                }
            } else {
                while index < clusters.len() && !clusters[index].2 {
                    index += 1;
                }
                while index < clusters.len() && clusters[index].2 {
                    index += 1;
                }
            }
            let end = clusters
                .get(index)
                .map_or(line.len(), |(start, _, _)| *start);
            Ok(TextRange::Charwise(
                BytePos::new(line_start.get() + cursor)..BytePos::new(line_start.get() + end),
            ))
        } else {
            while index > 0 && clusters[index - 1].2 {
                index -= 1;
            }
            while index > 0 && !clusters[index - 1].2 {
                index -= 1;
            }
            let start = clusters.get(index).map_or(cursor, |(start, _, _)| *start);
            Ok(TextRange::Charwise(
                BytePos::new(line_start.get() + start)..BytePos::new(line_start.get() + cursor),
            ))
        }
    }

    /// Returns the text a yank of `range` would place in the unnamed register.
    ///
    /// A linewise range is normalized to full lines using the buffer's line
    /// ending, so the register remembers line structure even when the source
    /// last line had no terminator.
    pub(crate) fn yank(&self, range: &TextRange) -> Result<String, TextError> {
        match range {
            TextRange::Charwise(range) => self.buffer.slice_text(range.clone()),
            TextRange::Linewise(lines) => self.linewise_text(lines),
        }
    }

    /// Deletes `range` as one undo block and returns its register text.
    ///
    /// Returns `None` when the range changes no bytes, in which case the undo
    /// history and cursor are untouched.
    pub(crate) fn delete(&mut self, range: TextRange) -> Result<Option<String>, TextError> {
        match range {
            TextRange::Charwise(range) => {
                if range.start == range.end {
                    return Ok(None);
                }
                let start = range.start;
                self.break_undo();
                let (edit, before) = self.stage_edit(range, "")?;
                let removed = edit.removed().to_string();
                let after = self.buffer.byte_to_line_col(start)?;
                let after = self.clamp_normal_position(after)?;
                self.commit_edit(edit, before, after);
                Ok(Some(removed))
            }
            TextRange::Linewise(lines) => {
                if lines.start >= lines.end {
                    return Ok(None);
                }
                let text = self.linewise_text(&lines)?;
                let bounds = self.linewise_delete_bounds(&lines)?;
                if bounds.start == bounds.end {
                    return Ok(None);
                }

                let first = lines.start;
                self.break_undo();
                let (edit, before) = self.stage_edit(bounds, "")?;
                let after = self.linewise_cursor_after(first)?;
                self.commit_edit(edit, before, after);
                Ok(Some(text))
            }
        }
    }

    /// Puts register contents before or after the cursor.
    ///
    /// Characterwise text is inserted after the grapheme under the cursor for
    /// `p` and at the cursor for `P`. Linewise text is inserted as whole lines
    /// before or after the effective current line and keeps the buffer's line
    /// ending. The whole put is one undo block.
    pub(crate) fn put(
        &mut self,
        text: &str,
        linewise: bool,
        before_cursor: bool,
    ) -> Result<(), TextError> {
        if text.is_empty() && !linewise {
            return Ok(());
        }

        if linewise {
            self.put_linewise(text, before_cursor)
        } else {
            self.put_charwise(text, before_cursor)
        }
    }

    /// Opens a line below or above the effective current line and places the
    /// cursor on it.
    ///
    /// The opened line is one undo block. The caller switches to Insert mode
    /// afterwards, so text typed next joins that block.
    ///
    /// A line opened below the final line of a buffer without a trailing
    /// terminator needs two terminators: one ends the existing line and one
    /// ends the new empty line. Without the second, saving and reopening would
    /// silently drop the opened line. Opening above the only line of an empty
    /// buffer has the same requirement.
    pub(crate) fn open_line(&mut self, below: bool) -> Result<(), TextError> {
        let line = self.effective_line(self.cursor.line);
        let eol = self.line_ending_text();
        let buffer_len = self.buffer.len_bytes();
        let (insertion, cursor_byte, inserted) = if below {
            let end = self.buffer.line_byte_range(line)?.end;
            let at_buffer_end = end.get() == buffer_len;
            let inserted = if at_buffer_end {
                format!("{eol}{eol}")
            } else {
                eol.to_string()
            };
            (end, BytePos::new(end.get() + eol.len()), inserted)
        } else {
            let start = self.line_start(line)?;
            let inserted = if buffer_len == 0 {
                format!("{eol}{eol}")
            } else {
                eol.to_string()
            };
            (start, start, inserted)
        };

        self.break_undo();
        let (edit, before) = self.stage_edit(insertion..insertion, &inserted)?;
        let after = self.buffer.byte_to_line_col(cursor_byte)?;
        self.commit_edit(edit, before, after);
        Ok(())
    }

    fn put_charwise(&mut self, text: &str, before_cursor: bool) -> Result<(), TextError> {
        let insertion = if before_cursor {
            self.buffer.line_col_to_byte(self.cursor)?
        } else {
            let line = self.line_text();
            let column = self.cursor.col.get().min(line.len());
            let insertion = if column < line.len() {
                next_grapheme_boundary(&line, column).unwrap_or(line.len())
            } else {
                column
            };
            BytePos::new(self.line_start(self.cursor.line)?.get() + insertion)
        };

        self.break_undo();
        let (edit, before) = self.stage_edit(insertion..insertion, text)?;
        let end = BytePos::new(insertion.get() + text.len());
        let after = self.buffer.byte_to_line_col(end)?;
        let after = self.clamp_normal_position(after)?;
        self.commit_edit(edit, before, after);
        Ok(())
    }

    fn put_linewise(&mut self, text: &str, before_cursor: bool) -> Result<(), TextError> {
        let line = self.effective_line(self.cursor.line);
        let old_len = self.buffer.len_bytes();
        let insertion = if before_cursor {
            self.line_start(line)?
        } else if line.get() + 1 < self.buffer.len_lines() {
            self.line_start(LineNumber::new(line.get() + 1))?
        } else {
            BytePos::new(old_len)
        };

        let eol = self.line_ending_text();
        let final_line = LineNumber::new(self.buffer.len_lines() - 1);
        let at_end_without_eol =
            insertion.get() == old_len && self.buffer.line_len_bytes(final_line)? > 0;
        let prefix = if at_end_without_eol { eol } else { "" };
        let body = if at_end_without_eol {
            text.strip_suffix(eol).unwrap_or(text)
        } else {
            text
        };
        let combined = format!("{prefix}{body}");
        if combined.is_empty() {
            return Ok(());
        }

        self.break_undo();
        let (edit, before) = self.stage_edit(insertion..insertion, &combined)?;
        let after = self
            .buffer
            .byte_to_line_col(BytePos::new(insertion.get() + prefix.len()))?;
        self.commit_edit(edit, before, after);
        Ok(())
    }

    fn linewise_text(&self, lines: &Range<LineNumber>) -> Result<String, TextError> {
        let eol = self.line_ending_text();
        let mut text = String::new();
        for line in lines.start.get()..lines.end.get() {
            text.push_str(&self.buffer.line_text(LineNumber::new(line))?);
            text.push_str(eol);
        }
        Ok(text)
    }

    /// Returns the bytes removed by a linewise delete.
    ///
    /// A final line without a terminator takes the preceding separator with
    /// it, matching Vim: deleting the last line of `a\nb` leaves `a`, not a
    /// trailing blank line.
    fn linewise_delete_bounds(
        &self,
        lines: &Range<LineNumber>,
    ) -> Result<Range<BytePos>, TextError> {
        let line_count = self.buffer.len_lines();
        if lines.start >= lines.end {
            let line = LineNumber::new(lines.start.get().min(line_count - 1));
            let start = self.line_start(line)?;
            return Ok(start..start);
        }

        let first = lines.start.get();
        let last = lines.end.get() - 1;
        if first >= line_count || last >= line_count {
            return Err(TextError::LineOutOfBounds {
                line: last.max(first),
                line_count,
            });
        }

        let first_line = LineNumber::new(first);
        let last_line = LineNumber::new(last);
        let last_len = self.buffer.line_len_bytes(last_line)?;
        let last_is_final = last + 1 == line_count;
        let mut start = self.line_start(first_line)?;
        let end = if last_is_final {
            BytePos::new(self.buffer.len_bytes())
        } else {
            self.line_start(LineNumber::new(last + 1))?
        };

        if last_is_final && last_len > 0 && first > 0 {
            start = self.buffer.line_byte_range(LineNumber::new(first - 1))?.end;
        }
        Ok(start..end)
    }

    /// Places the cursor on the first line at or after `first` that survives
    /// the delete, skipping the synthetic final empty line.
    fn linewise_cursor_after(&self, first: LineNumber) -> Result<LineCol, TextError> {
        let mut line = first.get().min(self.buffer.len_lines() - 1);
        if line > 0
            && line + 1 == self.buffer.len_lines()
            && self.buffer.line_len_bytes(LineNumber::new(line))? == 0
        {
            line -= 1;
        }
        Ok(LineCol {
            line: LineNumber::new(line),
            col: ByteCol::ZERO,
        })
    }

    pub(crate) fn line_start(&self, line: LineNumber) -> Result<BytePos, TextError> {
        self.buffer.line_col_to_byte(LineCol {
            line,
            col: ByteCol::ZERO,
        })
    }
}
