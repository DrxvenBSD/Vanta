//! Grapheme cluster boundary helpers.
//!
//! A grapheme cluster is the unit a user perceives as one character. Cursor
//! movement and every edit operate on cluster boundaries, so deleting a
//! character can never split a combining sequence, a ZWJ emoji, a variation
//! selector or a CRLF pair (`PROJECT_SPEC` §3C, Text manipulation).

use unicode_segmentation::GraphemeCursor;

/// Returns `true` when `byte` is a grapheme cluster boundary in `text`.
///
/// `byte` equal to `text.len()` is a boundary; offsets past the end are not.
pub fn is_grapheme_boundary(text: &str, byte: usize) -> bool {
    // The segmenter panics on offsets that are not character boundaries, so
    // reject those first; such an offset is never a cluster boundary either.
    if byte > text.len() || !text.is_char_boundary(byte) {
        return false;
    }
    let mut cursor = GraphemeCursor::new(byte, text.len(), true);
    // The whole string is provided, so the cursor never lacks context and an
    // `Err` can only mean an internal offset problem; treat that as "not a
    // boundary" rather than asserting on user-controlled data.
    matches!(cursor.is_boundary(text, 0), Ok(true))
}

/// Returns the closest grapheme boundary before `byte`, or `None` at the start.
///
/// `byte` is clamped to the text length; when it is not itself a boundary the
/// search starts from the containing cluster.
pub fn prev_grapheme_boundary(text: &str, byte: usize) -> Option<usize> {
    let start = snap_to_char_boundary(text, byte);
    let mut cursor = GraphemeCursor::new(start, text.len(), true);
    // The whole string is supplied, so the cursor never asks for another
    // chunk; an error can only mean an invalid offset, which has no boundary
    // before it.
    cursor.prev_boundary(text, 0).unwrap_or_default()
}

/// Returns the closest grapheme boundary after `byte`, or `None` at the end.
///
/// `byte` is clamped to the text length and snapped backwards to a character
/// boundary before searching, so the result is always greater than `byte`.
pub fn next_grapheme_boundary(text: &str, byte: usize) -> Option<usize> {
    let start = snap_to_char_boundary(text, byte);
    if start >= text.len() {
        return None;
    }
    let mut cursor = GraphemeCursor::new(start, text.len(), true);
    // The whole string is supplied, so the cursor never asks for another
    // chunk; an error can only mean an invalid offset, which has no boundary
    // after it.
    cursor.next_boundary(text, 0).unwrap_or_default()
}

/// Moves `byte` backwards to the closest grapheme cluster boundary.
///
/// Offsets past the end clamp to `text.len()`, which is always a boundary.
pub fn snap_to_grapheme_boundary(text: &str, byte: usize) -> usize {
    let mut pos = snap_to_char_boundary(text, byte);
    while !is_grapheme_boundary(text, pos) {
        match prev_grapheme_boundary(text, pos) {
            Some(previous) => pos = previous,
            None => return 0,
        }
    }
    pos
}

/// Moves `byte` backwards to the closest UTF-8 character boundary.
///
/// Offsets past the end clamp to `text.len()`.
pub(crate) fn snap_to_char_boundary(text: &str, byte: usize) -> usize {
    let mut pos = byte.min(text.len());
    while !text.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_boundaries_are_every_byte() {
        let text = "word";
        for byte in 0..=text.len() {
            assert!(is_grapheme_boundary(text, byte), "byte {byte}");
        }
        assert_eq!(prev_grapheme_boundary(text, 3), Some(2));
        assert_eq!(next_grapheme_boundary(text, 3), Some(4));
        assert_eq!(prev_grapheme_boundary(text, 0), None);
        assert_eq!(next_grapheme_boundary(text, text.len()), None);
    }

    #[test]
    fn combining_sequence_is_one_cluster() {
        let text = "e\u{301}x"; // "é" decomposed, 3 bytes, then "x"
        assert!(is_grapheme_boundary(text, 0));
        assert!(!is_grapheme_boundary(text, 1));
        assert!(is_grapheme_boundary(text, 3));
        assert_eq!(prev_grapheme_boundary(text, 3), Some(0));
        assert_eq!(next_grapheme_boundary(text, 0), Some(3));
        assert_eq!(snap_to_grapheme_boundary(text, 2), 0);
    }

    #[test]
    fn zwj_emoji_is_one_cluster() {
        let text = "👨\u{200d}👩\u{200d}👧\u{200d}👦";
        assert_eq!(text.chars().count(), 7);
        assert!(is_grapheme_boundary(text, 0));
        assert!(is_grapheme_boundary(text, text.len()));
        for byte in 1..text.len() {
            if text.is_char_boundary(byte) {
                assert!(
                    !is_grapheme_boundary(text, byte),
                    "byte {byte} split the family"
                );
            }
        }
    }

    #[test]
    fn crlf_is_one_cluster() {
        let text = "a\r\nb";
        assert!(!is_grapheme_boundary(text, 2), "between CR and LF");
        assert!(is_grapheme_boundary(text, 3));
        assert_eq!(prev_grapheme_boundary(text, 3), Some(1));
    }

    #[test]
    fn out_of_range_offsets_are_not_boundaries() {
        let text = "ab";
        assert!(!is_grapheme_boundary(text, 3));
        assert_eq!(prev_grapheme_boundary(text, 99), Some(1));
        assert_eq!(next_grapheme_boundary(text, 99), None);
        assert_eq!(snap_to_grapheme_boundary(text, 99), 2);
    }
}
