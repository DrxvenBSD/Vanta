use super::*;
use crate::error::TextError;
use crate::line_ending::LineEnding;
use crate::units::{ByteCol, BytePos, CharPos, DisplayCol, LineCol, LineNumber};

fn line(value: usize) -> LineNumber {
    LineNumber::new(value)
}

fn pos(value: usize) -> BytePos {
    BytePos::new(value)
}

fn lc(line_number: usize, col: usize) -> LineCol {
    LineCol {
        line: line(line_number),
        col: ByteCol::new(col),
    }
}

#[test]
fn empty_buffer_has_one_empty_line() {
    let buffer = Buffer::new();
    assert_eq!(buffer.len_bytes(), 0);
    assert_eq!(buffer.len_chars(), 0);
    assert_eq!(buffer.len_lines(), 1);
    assert_eq!(buffer.line(line(0)).expect("line 0").to_string(), "");
    assert_eq!(
        buffer.line_byte_range(line(0)).expect("range"),
        pos(0)..pos(0)
    );
    assert_eq!(buffer.to_string(), "");
}

#[test]
fn trailing_terminator_creates_a_final_empty_line() {
    let buffer = Buffer::from_text("a\n");
    assert_eq!(buffer.len_lines(), 2);
    assert_eq!(buffer.line(line(1)).expect("line 1").to_string(), "");
    assert_eq!(
        buffer.line_byte_range(line(0)).expect("range"),
        pos(0)..pos(1)
    );
    assert_eq!(
        buffer.line_byte_range(line(1)).expect("range"),
        pos(2)..pos(2)
    );
}

#[test]
fn line_ranges_exclude_crlf_terminators() {
    let buffer = Buffer::from_text("one\r\ntwo\r\n");
    assert_eq!(buffer.line_ending(), LineEnding::Crlf);
    assert_eq!(buffer.len_lines(), 3);
    assert_eq!(buffer.line(line(0)).expect("line 0").to_string(), "one");
    assert_eq!(
        buffer.line_byte_range(line(0)).expect("range"),
        pos(0)..pos(3)
    );
    assert_eq!(
        buffer.line_byte_range(line(1)).expect("range"),
        pos(5)..pos(8)
    );
}

#[test]
fn byte_and_line_col_round_trip() {
    let buffer = Buffer::from_text("ab\ncd\r\nef");
    for (byte, expected) in [
        (
            0,
            LineCol {
                line: line(0),
                col: ByteCol::new(0),
            },
        ),
        (
            2,
            LineCol {
                line: line(0),
                col: ByteCol::new(2),
            },
        ),
        (
            3,
            LineCol {
                line: line(1),
                col: ByteCol::new(0),
            },
        ),
        (
            5,
            LineCol {
                line: line(1),
                col: ByteCol::new(2),
            },
        ),
        (
            4,
            LineCol {
                line: line(1),
                col: ByteCol::new(1),
            },
        ),
        (
            7,
            LineCol {
                line: line(2),
                col: ByteCol::new(0),
            },
        ),
        (
            9,
            LineCol {
                line: line(2),
                col: ByteCol::new(2),
            },
        ),
    ] {
        let position = buffer
            .byte_to_line_col(pos(byte))
            .expect("valid byte offset");
        assert_eq!(position, expected, "byte {byte}");
        assert_eq!(
            buffer.line_col_to_byte(position).expect("valid line col"),
            pos(byte)
        );
    }

    // The `\n` of a CRLF pair is inside the terminator's grapheme cluster
    // and therefore has no canonical line/column.
    assert_eq!(
        buffer.byte_to_line_col(pos(6)),
        Err(TextError::NotGraphemeBoundary { pos: 6 })
    );
}

#[test]
fn line_columns_never_resolve_into_a_neighbouring_line() {
    let crlf = Buffer::from_text("ab\r\ncd");
    // End of line is the boundary before the terminator.
    assert_eq!(crlf.line_col_to_byte(lc(0, 2)), Ok(pos(2)));
    // Columns in or past the terminator are rejected, never clamped.
    for col in [3, 4] {
        assert_eq!(
            crlf.line_col_to_byte(lc(0, col)),
            Err(TextError::ColumnOutOfBounds {
                line: 0,
                col,
                len: 2
            }),
            "CRLF line 0 column {col}"
        );
    }
    // The next line is reachable only through its own line number.
    assert_eq!(crlf.line_col_to_byte(lc(1, 0)), Ok(pos(4)));
    assert_eq!(crlf.byte_to_line_col(pos(2)), Ok(lc(0, 2)));
    assert_eq!(
        crlf.byte_to_line_col(pos(3)),
        Err(TextError::NotGraphemeBoundary { pos: 3 })
    );
    assert_eq!(crlf.byte_to_line_col(pos(4)), Ok(lc(1, 0)));

    let lf = Buffer::from_text("ab\ncd");
    assert_eq!(lf.line_col_to_byte(lc(0, 2)), Ok(pos(2)));
    assert_eq!(
        lf.line_col_to_byte(lc(0, 3)),
        Err(TextError::ColumnOutOfBounds {
            line: 0,
            col: 3,
            len: 2
        })
    );
    assert_eq!(lf.line_col_to_byte(lc(1, 0)), Ok(pos(3)));
    assert_eq!(lf.byte_to_line_col(pos(2)), Ok(lc(0, 2)));
    assert_eq!(lf.byte_to_line_col(pos(3)), Ok(lc(1, 0)));
}

#[test]
fn insert_and_remove_preserve_exact_bytes() {
    let mut buffer = Buffer::from_text("hello world");
    buffer.insert(pos(5), ",").expect("ascii boundary");
    assert_eq!(buffer.to_string(), "hello, world");
    let removed = buffer.remove(pos(5)..pos(6)).expect("whole characters");
    assert_eq!(removed, ",");
    assert_eq!(buffer.to_string(), "hello world");
}

#[test]
fn insert_rejects_splitting_characters_and_clusters() {
    let mut buffer = Buffer::from_text("e\u{301}x");
    assert_eq!(
        buffer.insert(pos(1), "!"),
        Err(TextError::NotGraphemeBoundary { pos: 1 }),
        "inside a combining sequence"
    );

    let mut crlf = Buffer::from_text("a\r\nb");
    assert_eq!(
        crlf.insert(pos(2), "!"),
        Err(TextError::NotGraphemeBoundary { pos: 2 }),
        "between CR and LF"
    );

    let mut wide = Buffer::from_text("中");
    assert_eq!(
        wide.insert(pos(1), "!"),
        Err(TextError::NotCharBoundary { pos: 1 })
    );
}

#[test]
fn remove_rejects_reversed_and_out_of_bounds_ranges() {
    let mut buffer = Buffer::from_text("abc");
    assert_eq!(
        buffer.remove(pos(2)..pos(1)),
        Err(TextError::InvalidRange { start: 2, end: 1 })
    );
    assert_eq!(
        buffer.remove(pos(0)..pos(9)),
        Err(TextError::PositionOutOfBounds { pos: 9, len: 3 })
    );
    assert_eq!(buffer.remove(pos(2)..pos(2)).expect("empty range"), "");
    assert_eq!(buffer.to_string(), "abc");
}

#[test]
fn unicode_width_queries_agree_with_the_text_module() {
    let buffer = Buffer::from_text("\t中文e\u{301}");
    assert_eq!(
        buffer.line_display_width(line(0), 8).expect("line 0"),
        DisplayCol::new(13)
    );
    assert_eq!(
        buffer.display_col_of(pos(7), 8).expect("valid offset"),
        DisplayCol::new(12)
    );
}

#[test]
fn line_accessors_report_bounds() {
    let buffer = Buffer::from_text("one\n");
    assert_eq!(
        buffer.line(line(2)),
        Err(TextError::LineOutOfBounds {
            line: 2,
            line_count: 2
        })
    );
    assert_eq!(
        buffer.byte_to_line_col(pos(9)),
        Err(TextError::PositionOutOfBounds { pos: 9, len: 4 })
    );
    assert_eq!(
        buffer.line_col_to_byte(LineCol {
            line: line(0),
            col: ByteCol::new(9)
        }),
        Err(TextError::ColumnOutOfBounds {
            line: 0,
            col: 9,
            len: 3
        })
    );
}

#[test]
fn scalar_conversions_round_trip() {
    let buffer = Buffer::from_text("a中🙂");
    for (byte, scalar) in [(0, 0), (1, 1), (4, 2), (8, 3)] {
        assert_eq!(
            buffer.byte_to_char(pos(byte)).expect("char boundary"),
            CharPos::new(scalar)
        );
        assert_eq!(
            buffer.char_to_byte(CharPos::new(scalar)).expect("scalar"),
            pos(byte)
        );
    }
    assert_eq!(
        buffer.char_to_byte(CharPos::new(9)),
        Err(TextError::CharPositionOutOfBounds {
            pos: 9,
            len_chars: 3
        })
    );
}

#[test]
fn mark_modified_sets_the_saved_state_without_an_edit() {
    let mut buffer = Buffer::from_text("recovered");
    assert!(!buffer.is_modified());

    buffer.mark_modified();
    assert!(buffer.is_modified());
    assert_eq!(buffer.to_string(), "recovered");
    assert_eq!(buffer.undo().expect("no history"), None);

    buffer.mark_saved();
    assert!(!buffer.is_modified());
}

#[test]
fn apply_replaces_a_range_and_returns_an_invertible_edit() {
    let mut buffer = Buffer::from_text("hello");

    let edit = buffer
        .apply(pos(1)..pos(4), "i")
        .expect("replacement covers whole clusters");

    assert_eq!(buffer.to_string(), "hio");
    assert_eq!(edit.start(), pos(1));
    assert_eq!(edit.end(), pos(2));
    assert_eq!(edit.removed(), "ell");
    assert_eq!(edit.inserted(), "i");

    buffer
        .apply(edit.start()..edit.end(), edit.removed())
        .expect("the inverse edit restores the original bytes");
    assert_eq!(buffer.to_string(), "hello");
}
