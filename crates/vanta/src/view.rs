//! Window geometry shared by cursor motion and rendering.
//!
//! The editor keeps one horizontal offset per window. This module owns the
//! rules for choosing it: the offset is a display-cell boundary, it never
//! splits a grapheme cluster, and it changes only when the cursor would leave
//! the visible cells. Both the renderer and the desired-column calculation use
//! the same tab stop and width helpers.

use unicode_segmentation::UnicodeSegmentation;
use vanta_text::{
    DisplayCol, byte_of_display_col, display_col_of_byte, grapheme_width_at, line_display_width,
    prev_grapheme_boundary,
};

/// The tab width used for display. It is fixed until the option system exists.
pub(crate) const TABSTOP: usize = 8;

/// Returns the display column and cell width of the grapheme at `byte`.
///
/// A byte at the end of the line has no grapheme and reports width zero. A
/// byte inside a cluster reports the whole cluster, so a caller cannot
/// accidentally treat a combining sequence or a wide glyph as narrower than it
/// is.
pub(crate) fn cursor_cells(line: &str, byte: usize) -> (usize, usize) {
    let byte = byte.min(line.len());
    let start = display_col_of_byte(line, byte, TABSTOP).get();
    if byte >= line.len() {
        return (start, 0);
    }

    let next = vanta_text::next_grapheme_boundary(line, byte).unwrap_or(line.len());
    let end = display_col_of_byte(line, next, TABSTOP).get();
    (start, end.saturating_sub(start))
}

/// Chooses the leftmost display cell to show so the cursor remains visible.
///
/// The previous `left` is kept while the cursor stays inside the viewport, so
/// moving left across a scrolled line does not pin the cursor to the right
/// edge. Moving to a shorter line clamps the offset back, including all the
/// way to zero when the line fits. The result is always a grapheme boundary
/// within the current line.
pub(crate) fn keep_cursor_visible(
    line: &str,
    cursor_byte: usize,
    width: usize,
    left: usize,
) -> usize {
    let width = width.max(1);
    let line_width = line_display_width(line, TABSTOP).get();
    let (cursor_cell, cursor_width) = cursor_cells(line, cursor_byte);
    let cursor_width = cursor_width.max(1);

    // An Insert cursor may sit one cell past the final grapheme. Reserve that
    // cell so the cursor is still addressable when the line exactly fills the
    // window.
    let trailing_cursor_cell = usize::from(cursor_byte >= line.len());
    let content_width = line_width.saturating_add(trailing_cursor_cell);
    let max_left = content_width.saturating_sub(width);
    let mut left = boundary_at_or_before(line, left.min(max_left));

    if cursor_cell < left {
        left = cursor_cell;
    }

    let cursor_end = cursor_cell.saturating_add(cursor_width);
    if cursor_end > left.saturating_add(width) {
        let target = cursor_end.saturating_sub(width);
        left = boundary_at_or_after(line, target).min(cursor_cell);
    }

    left.min(max_left)
}

/// Returns the Normal-mode byte column on `line` at or before display column
/// `target`.
///
/// A target past the end lands on the final grapheme, which is the furthest a
/// Normal-mode cursor may go. A target inside a wide glyph or a tab lands on
/// that cluster's start so vertical movement never splits it.
pub(crate) fn normal_column_at_display(line: &str, target: usize) -> usize {
    if line.is_empty() {
        return 0;
    }
    let byte = byte_of_display_col(line, DisplayCol::new(target), TABSTOP);
    if byte >= line.len() {
        prev_grapheme_boundary(line, line.len()).unwrap_or(0)
    } else {
        byte
    }
}

/// Returns the grapheme-boundary display column at or before `target`.
fn boundary_at_or_before(line: &str, target: usize) -> usize {
    let byte = byte_of_display_col(line, DisplayCol::new(target), TABSTOP);
    display_col_of_byte(line, byte, TABSTOP).get()
}

/// Returns the first grapheme-boundary display column at or after `target`.
fn boundary_at_or_after(line: &str, target: usize) -> usize {
    let mut column = 0;
    for grapheme in line.graphemes(true) {
        if column >= target {
            return column;
        }
        column += grapheme_width_at(grapheme, column, TABSTOP);
    }
    column
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_cells_measure_tabs_and_wide_glyphs() {
        assert_eq!(cursor_cells("a\tb", 0), (0, 1));
        assert_eq!(cursor_cells("a\tb", 1), (1, 7));
        assert_eq!(cursor_cells("a\tb", 2), (8, 1));
        assert_eq!(cursor_cells("a中b", 1), (1, 2));
        assert_eq!(cursor_cells("ab", 2), (2, 0));
    }

    #[test]
    fn vertical_target_uses_the_cluster_covering_a_display_column() {
        assert_eq!(normal_column_at_display("", 10), 0);
        assert_eq!(normal_column_at_display("a\tb", 4), 1);
        assert_eq!(normal_column_at_display("a中b", 2), 1);
        assert_eq!(normal_column_at_display("a中b", 99), 4);
    }

    #[test]
    fn offset_is_grapheme_aligned_and_only_moves_when_needed() {
        let line = "a中b中c中d中e中";
        let width = 4;

        assert_eq!(keep_cursor_visible(line, 0, width, 0), 0);
        assert_eq!(
            keep_cursor_visible(line, 5, width, 0),
            3,
            "the offset snaps to the wide glyph's start"
        );
        assert_eq!(
            keep_cursor_visible(line, 0, width, 3),
            0,
            "moving to column zero resets the viewport"
        );
        assert_eq!(
            keep_cursor_visible(line, 4, width, 3),
            3,
            "an offset inside the viewport is kept"
        );
    }
}
