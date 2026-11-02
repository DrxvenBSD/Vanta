//! Property tests for the text substrate (AGENTS §11.3).
//!
//! These assert the invariants the rest of the editor relies on:
//!
//! 1. Applying edits to a buffer matches a naive `String` model.
//! 2. An insert followed by removing the same range restores the exact bytes.
//! 3. Byte positions round-trip through line/column positions.
//! 4. Line ranges tile the buffer exactly and agree with a naive line scan.
//! 5. Only grapheme cluster boundaries are accepted as edit positions.
//! 6. Line columns stay inside their line, and end-of-line is the boundary
//!    before the terminator.

use proptest::prelude::*;
use proptest::sample::Index;
use unicode_segmentation::UnicodeSegmentation;
use vanta_text::{Buffer, ByteCol, BytePos, LineCol, TextError};

/// Characters chosen so that every one is its own grapheme cluster.
///
/// CR is omitted so that random edits never need to reason about CRLF
/// clusters; CRLF has dedicated tests.
fn plain_text() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            Just('a'),
            Just('b'),
            Just(' '),
            Just('\t'),
            Just('\n'),
            Just('é'),
            Just('中'),
            Just('🙂'),
        ],
        0..60,
    )
    .prop_map(|chars| chars.into_iter().collect())
}

/// Grapheme clusters that exercise multi-byte and multi-scalar sequences.
fn grapheme_cluster() -> impl Strategy<Value = &'static str> {
    prop_oneof![
        Just("a"),
        Just("é"),
        Just("e\u{301}"),
        Just("中"),
        Just("🙂"),
        Just("👨\u{200d}👩\u{200d}👧\u{200d}👦"),
        Just("🇵🇱"),
        Just("\r\n"),
        Just("\n"),
        Just("\t"),
    ]
}

fn clustered_text() -> impl Strategy<Value = String> {
    proptest::collection::vec(grapheme_cluster(), 0..40).prop_map(|clusters| clusters.concat())
}

#[derive(Debug, Clone)]
enum Edit {
    Insert { at: Index, text: String },
    Remove { start: Index, end: Index },
}

fn edit() -> impl Strategy<Value = Edit> {
    prop_oneof![
        (any::<Index>(), plain_text()).prop_map(|(at, text)| Edit::Insert { at, text }),
        (any::<Index>(), any::<Index>()).prop_map(|(start, end)| Edit::Remove { start, end }),
    ]
}

/// Returns every byte offset that is a character boundary.
fn char_boundaries(text: &str) -> Vec<usize> {
    (0..=text.len())
        .filter(|&byte| text.is_char_boundary(byte))
        .collect()
}

/// Returns the content of `line` in a naive line scan, excluding terminators.
fn naive_line(model: &str, line: usize) -> Option<&str> {
    model
        .split('\n')
        .nth(line)
        .map(|text| text.strip_suffix('\r').unwrap_or(text))
}

proptest! {
    #[test]
    fn edits_match_a_naive_model(
        initial in plain_text(),
        edits in proptest::collection::vec(edit(), 0..20),
    ) {
        let mut buffer = Buffer::from_text(&initial);
        let mut model = initial;

        for edit in edits {
            let boundaries = char_boundaries(&model);
            match edit {
                Edit::Insert { at, text } => {
                    let offset = boundaries[at.index(boundaries.len())];
                    buffer.insert(BytePos::new(offset), &text)
                        .expect("every character in the generated text is its own cluster");
                    model.insert_str(offset, &text);
                }
                Edit::Remove { start, end } => {
                    let a = boundaries[start.index(boundaries.len())];
                    let b = boundaries[end.index(boundaries.len())];
                    let (start, end) = if a <= b { (a, b) } else { (b, a) };
                    let removed = buffer
                        .remove(BytePos::new(start)..BytePos::new(end))
                        .expect("range endpoints are character boundaries");
                    prop_assert_eq!(removed, model[start..end].to_string());
                    model.replace_range(start..end, "");
                }
            }
            prop_assert_eq!(buffer.to_string(), model.clone());
        }

        prop_assert_eq!(buffer.len_bytes(), model.len());
        prop_assert_eq!(buffer.len_lines(), model.split('\n').count());
        for line in 0..buffer.len_lines() {
            prop_assert_eq!(
                buffer.line_text(vanta_text::LineNumber::new(line)).expect("line exists"),
                naive_line(&model, line).expect("model has the same lines").to_string()
            );
        }
    }

    #[test]
    fn insert_then_remove_restores_exact_bytes(
        initial in plain_text(),
        at in any::<Index>(),
        payload in plain_text(),
    ) {
        let boundaries = char_boundaries(&initial);
        let offset = boundaries[at.index(boundaries.len())];
        let mut buffer = Buffer::from_text(&initial);

        buffer.insert(BytePos::new(offset), &payload).expect("cluster boundary");
        let removed = buffer
            .remove(BytePos::new(offset)..BytePos::new(offset + payload.len()))
            .expect("the inserted text is whole clusters");
        prop_assert_eq!(removed, payload);
        prop_assert_eq!(buffer.to_string(), initial);
    }

    #[test]
    fn byte_and_line_col_positions_round_trip(initial in plain_text()) {
        let buffer = Buffer::from_text(&initial);
        for byte in char_boundaries(&initial) {
            let position = buffer.byte_to_line_col(BytePos::new(byte)).expect("char boundary");
            prop_assert_eq!(buffer.line_col_to_byte(position).expect("valid position"), BytePos::new(byte));
        }
    }

    #[test]
    fn line_ranges_tile_the_buffer(initial in plain_text()) {
        let buffer = Buffer::from_text(&initial);
        let mut expected_start = 0;

        for line in 0..buffer.len_lines() {
            let range = buffer
                .line_byte_range(vanta_text::LineNumber::new(line))
                .expect("line exists");
            prop_assert_eq!(range.start.get(), expected_start);
            prop_assert!(buffer.is_char_boundary(range.start));
            prop_assert!(buffer.is_char_boundary(range.end));
            prop_assert_eq!(
                &initial[range.start.get()..range.end.get()],
                naive_line(&initial, line).expect("model line")
            );

            let raw_len = buffer
                .line_text(vanta_text::LineNumber::new(line))
                .expect("line exists")
                .len();
            prop_assert_eq!(raw_len, range.end.get() - range.start.get());
            expected_start = range.end.get();

            // Skip the terminator that follows this line.
            if initial[expected_start..].starts_with("\r\n") {
                expected_start += 2;
            } else if initial[expected_start..].starts_with('\n') {
                expected_start += 1;
            }
        }

        prop_assert_eq!(expected_start, initial.len());
    }

    #[test]
    fn line_columns_stay_within_their_line(initial in clustered_text()) {
        let buffer = Buffer::from_text(&initial);
        for line_index in 0..buffer.len_lines() {
            let line = vanta_text::LineNumber::new(line_index);
            let content_len = buffer.line_len_bytes(line).expect("line exists");

            for col in 0..=content_len {
                let position = LineCol {
                    line,
                    col: ByteCol::new(col),
                };
                match buffer.line_col_to_byte(position) {
                    Ok(byte) => prop_assert_eq!(
                        buffer.byte_to_line_col(byte).expect("valid byte"),
                        position
                    ),
                    Err(TextError::NotCharBoundary { .. }) => {}
                    Err(error) => prop_assert!(false, "unexpected {error:?} for {position:?}"),
                }
            }

            prop_assert_eq!(
                buffer.line_col_to_byte(LineCol {
                    line,
                    col: ByteCol::new(content_len + 1),
                }),
                Err(TextError::ColumnOutOfBounds {
                    line: line_index,
                    col: content_len + 1,
                    len: content_len,
                })
            );
        }
    }

    #[test]
    fn only_grapheme_boundaries_are_accepted(initial in clustered_text()) {
        let buffer = Buffer::from_text(&initial);
        let segmentation: Vec<usize> = initial
            .grapheme_indices(true)
            .map(|(offset, _)| offset)
            .chain(std::iter::once(initial.len()))
            .collect();

        for byte in char_boundaries(&initial) {
            let mut candidate = buffer.clone();
            let result = candidate.insert(BytePos::new(byte), "");
            if segmentation.contains(&byte) {
                prop_assert_eq!(result, Ok(()), "boundary {} was rejected", byte);
            } else {
                prop_assert_eq!(
                    result,
                    Err(TextError::NotGraphemeBoundary { pos: byte }),
                    "split position {} was accepted",
                    byte
                );
            }
        }
    }

    #[test]
    fn display_columns_map_back_to_the_cluster_start(
        initial in clustered_text(),
        at in any::<Index>(),
    ) {
        let line_text = initial.split('\n').next().unwrap_or_default();
        let content = line_text.strip_suffix('\r').unwrap_or(line_text);
        let positions = char_boundaries(content);
        let byte = positions[at.index(positions.len())];
        let column = vanta_text::display_col_of_byte(content, byte, 8);
        let mapped = vanta_text::byte_of_display_col(content, column, 8);
        prop_assert!(mapped <= byte, "mapped {mapped} past source {byte}");
        prop_assert!(content.is_char_boundary(mapped));

        let line_col = LineCol { line: vanta_text::LineNumber::ZERO, col: ByteCol::new(byte) };
        let buffer = Buffer::from_text(&initial);
        let position = buffer.line_col_to_byte(line_col).expect("line content boundary");
        prop_assert_eq!(position, BytePos::new(byte));
    }
}
