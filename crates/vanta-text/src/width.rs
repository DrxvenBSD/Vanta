//! Display width and display-column mapping.
//!
//! Width is measured in terminal cells, per grapheme cluster, never per byte
//! or per scalar value (`PROJECT_SPEC` §3F, Terminal interface). A wide CJK
//! character occupies two cells; a combining mark occupies none of its own.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::grapheme::snap_to_char_boundary;
use crate::units::DisplayCol;

/// Returns the terminal width of one grapheme cluster, in cells.
///
/// Control characters are rendered as a single visible cell (AGENTS §10.1:
/// control characters are never written raw to the terminal), so they report
/// a width of one instead of their Unicode width.
pub fn grapheme_display_width(grapheme: &str) -> usize {
    match grapheme.chars().next() {
        Some(first) if first.is_control() => 1,
        _ => grapheme.width(),
    }
}

/// Returns the width of one grapheme cluster when it starts at `column`.
///
/// A tab advances to the next multiple of `tabstop`. A `tabstop` of zero is
/// treated as one so this function cannot divide by zero.
pub fn grapheme_width_at(grapheme: &str, column: usize, tabstop: usize) -> usize {
    if grapheme == "\t" {
        let tabstop = tabstop.max(1);
        tabstop - (column % tabstop)
    } else {
        grapheme_display_width(grapheme)
    }
}

/// Returns the display column reached after rendering `text` from column 0.
///
/// Tabs expand to the next `tabstop` boundary; a `tabstop` of zero is treated
/// as one. The text is expected to be one line without its terminator.
pub fn line_display_width(text: &str, tabstop: usize) -> DisplayCol {
    let mut column = 0;
    for grapheme in text.graphemes(true) {
        column += grapheme_width_at(grapheme, column, tabstop);
    }
    DisplayCol::new(column)
}

/// Returns the display column of the byte offset `byte` within `text`.
///
/// `byte` is clamped to the text length and snapped back to a character
/// boundary. Offsets inside a grapheme cluster report the column where the
/// cluster starts.
pub fn display_col_of_byte(text: &str, byte: usize, tabstop: usize) -> DisplayCol {
    let limit = snap_to_char_boundary(text, byte);
    let mut column = 0;
    for (offset, grapheme) in text.grapheme_indices(true) {
        if offset >= limit {
            break;
        }
        // An offset inside a cluster reports the column where it starts; do
        // not advance past the cluster the offset belongs to.
        if offset + grapheme.len() > limit {
            break;
        }
        column += grapheme_width_at(grapheme, column, tabstop);
    }
    DisplayCol::new(column)
}

/// Returns the byte offset of the grapheme cluster covering display column
/// `target` within `text`.
///
/// When `target` falls in the right half of a double-width cluster, the start
/// of that cluster is returned. A target past the end clamps to the text
/// length. Use the result to compute horizontal scrolling without splitting a
/// wide character.
pub fn byte_of_display_col(text: &str, target: DisplayCol, tabstop: usize) -> usize {
    let target = target.get();
    let mut column = 0;
    for (offset, grapheme) in text.grapheme_indices(true) {
        if column >= target {
            return offset;
        }
        let width = grapheme_width_at(grapheme, column, tabstop);
        if column + width > target {
            return offset;
        }
        column += width;
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(value: usize) -> DisplayCol {
        DisplayCol::new(value)
    }

    #[test]
    fn ascii_width_matches_byte_length() {
        assert_eq!(line_display_width("hello", 8), col(5));
        assert_eq!(display_col_of_byte("hello", 3, 8), col(3));
        assert_eq!(byte_of_display_col("hello", col(3), 8), 3);
    }

    #[test]
    fn tabs_expand_to_the_next_stop() {
        assert_eq!(line_display_width("\tx", 8), col(9));
        assert_eq!(line_display_width("a\tx", 8), col(9));
        assert_eq!(line_display_width("12345678\tx", 8), col(17));
        assert_eq!(display_col_of_byte("\tx", 1, 8), col(8));
        assert_eq!(byte_of_display_col("\tx", col(8), 8), 1);
        assert_eq!(byte_of_display_col("\tx", col(4), 8), 0, "inside the tab");
        assert_eq!(line_display_width("\t", 0), col(1), "zero tabstop clamps");
    }

    #[test]
    fn wide_characters_occupy_two_cells() {
        assert_eq!(line_display_width("中文", 8), col(4));
        assert_eq!(display_col_of_byte("中文", 3, 8), col(2));
        assert_eq!(
            display_col_of_byte("中文", 4, 8),
            col(2),
            "inside 文 snaps back"
        );
        assert_eq!(display_col_of_byte("中文", 6, 8), col(4));
        assert_eq!(
            byte_of_display_col("中文", col(1), 8),
            0,
            "right half of 中"
        );
        assert_eq!(byte_of_display_col("中文", col(2), 8), 3);
    }

    #[test]
    fn combining_marks_add_no_width() {
        let decomposed = "e\u{301}";
        assert_eq!(line_display_width(decomposed, 8), col(1));
        assert_eq!(display_col_of_byte(decomposed, decomposed.len(), 8), col(1));
    }

    #[test]
    fn emoji_presentation_is_two_cells() {
        assert_eq!(grapheme_display_width("🙂"), 2);
        assert_eq!(line_display_width("🙂🙂", 8), col(4));
    }

    #[test]
    fn control_characters_render_as_one_cell() {
        assert_eq!(grapheme_display_width("\u{7}"), 1);
        assert_eq!(grapheme_display_width("\r\n"), 1);
    }

    #[test]
    fn byte_of_display_col_clamps_past_the_end() {
        assert_eq!(byte_of_display_col("ab", col(99), 8), 2);
    }
}
