//! Literal search over the buffer.
//!
//! Search has no regex engine in this slice. Every occurrence of the pattern
//! is checked against the grapheme boundaries of the full text, so a literal
//! substring that starts or ends inside a combining sequence, a ZWJ emoji or a
//! CRLF pair is rejected. The scan advances one Unicode scalar at a time,
//! which keeps overlapping literal matches discoverable.

use std::ops::Range;

use vanta_text::{BytePos, is_grapheme_boundary};

use super::Document;

/// Returns every literal, grapheme-aligned occurrence of `pattern` in `text`.
///
/// Offsets are UTF-8 byte offsets. An empty pattern or a pattern longer than
/// the text yields no matches. Empty input is a valid no-match result.
pub(crate) fn literal_matches(text: &str, pattern: &str) -> Vec<Range<usize>> {
    if pattern.is_empty() || text.is_empty() {
        return Vec::new();
    }

    let mut matches = Vec::new();
    let mut offset = 0;
    while offset <= text.len() {
        let Some(relative) = text[offset..].find(pattern) else {
            break;
        };
        let start = offset + relative;
        let end = start + pattern.len();
        if is_grapheme_boundary(text, start) && is_grapheme_boundary(text, end) {
            matches.push(start..end);
        }

        // Advance by one scalar from the match start so overlapping matches
        // are not skipped. `start` is a char boundary returned by `find`.
        let Some(character) = text[start..].chars().next() else {
            break;
        };
        offset = start + character.len_utf8();
    }
    matches
}

impl Document {
    /// Returns every literal, grapheme-aligned occurrence in the buffer.
    ///
    /// Search is a user action rather than a per-keystroke path; the whole
    /// text is copied once so boundary checks can see the complete sequence.
    pub(crate) fn search_matches(&self, pattern: &str) -> Vec<Range<BytePos>> {
        let text = self.buffer.to_string();
        literal_matches(&text, pattern)
            .into_iter()
            .map(|range| BytePos::new(range.start)..BytePos::new(range.end))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_matches_finds_all_occurrences() {
        assert_eq!(
            literal_matches("foofoo", "foo"),
            vec![0..3, 3..6],
            "search must find non-first and adjacent literal matches"
        );
        assert_eq!(literal_matches("aaaa", "aa"), vec![0..2, 1..3, 2..4]);
    }

    #[test]
    fn empty_inputs_and_long_patterns_are_safe() {
        assert!(literal_matches("", "x").is_empty());
        assert!(literal_matches("abc", "").is_empty());
        assert!(literal_matches("abc", "abcdef").is_empty());
    }

    #[test]
    fn matches_must_start_and_end_on_grapheme_boundaries() {
        let decomposed = "e\u{301}x";
        assert!(
            literal_matches(decomposed, "e").is_empty(),
            "a match ending inside the combining cluster must be rejected"
        );
        assert_eq!(
            literal_matches(decomposed, "e\u{301}"),
            vec![0..3],
            "the complete cluster is a valid literal match"
        );
        assert_eq!(literal_matches(decomposed, "x"), vec![3..4]);
    }

    #[test]
    fn crlf_and_multibyte_text_use_byte_offsets() {
        assert_eq!(
            literal_matches("a\r\nb", "b"),
            vec![3..4],
            "a match on the line after CRLF starts after both terminator bytes"
        );
        assert_eq!(literal_matches("中文", "中"), vec![0..3]);
    }
}
