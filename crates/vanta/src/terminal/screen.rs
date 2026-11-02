//! Row-damage rendering for the crossterm backend.
//!
//! The previous frame is retained as rows of text plus overlay spans. A row is
//! repainted only when its text or an overlay on it changes; an unchanged
//! frame writes nothing. Cursor movement alone writes the status row and the
//! cursor, never a screen clear. Resize and the first frame are the only full
//! repaints.
//!
//! This is intentionally row-granular rather than cell-granular. It gives the
//! editor a bounded, testable damage boundary now; a cell grid can replace the
//! row state later without changing [`Frame`] or the editor loop.

use std::io;

use crossterm::cursor::{MoveTo, Show};
use crossterm::queue;
use crossterm::style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType};
use unicode_segmentation::UnicodeSegmentation;
use vanta_text::grapheme_display_width;

use super::capabilities::{Capabilities, ColorDepth};
use super::{Frame, SelectionSpan, Size, StyledSpan, TextStyle};

const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
const SYNC_END: &[u8] = b"\x1b[?2026l";

/// The retained front buffer and cursor for one terminal.
#[derive(Debug, Default)]
pub(crate) struct Screen {
    previous: Option<ScreenState>,
    cursor: Option<(u16, u16)>,
}

#[derive(Debug, PartialEq, Eq)]
struct ScreenState {
    size: Size,
    rows: Vec<RowState>,
}

#[derive(Debug, PartialEq, Eq)]
struct RowState {
    text: String,
    overlays: Vec<Overlay>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Overlay {
    Style(StyledSpan),
    SearchMatch(SelectionSpan),
    SearchCurrent(SelectionSpan),
    Selection(SelectionSpan),
}

impl Screen {
    /// Forgets the front buffer so the next frame repaints every row.
    pub(crate) fn reset(&mut self) {
        self.previous = None;
        self.cursor = None;
    }

    /// Renders `frame` into ANSI bytes and updates the front buffer.
    ///
    /// The returned vector is empty when the frame is identical to the
    /// previous one. The caller writes it in one buffered operation.
    pub(crate) fn render(
        &mut self,
        frame: &Frame,
        capabilities: Capabilities,
    ) -> io::Result<Vec<u8>> {
        let next_rows: Vec<RowState> = frame
            .rows
            .iter()
            .enumerate()
            .map(|(row, text)| RowState {
                text: text.clone(),
                overlays: overlays_for(frame, row as u16),
            })
            .collect();

        let full = match &self.previous {
            None => true,
            Some(previous) => previous.size != frame.size || previous.rows.len() != next_rows.len(),
        };
        let changed: Vec<usize> = if full {
            (0..next_rows.len()).collect()
        } else {
            let previous = self.previous.as_ref().expect("full is false");
            next_rows
                .iter()
                .enumerate()
                .filter_map(|(row, state)| (previous.rows[row] != *state).then_some(row))
                .collect()
        };

        let cursor_moved = self.cursor != Some((frame.cursor.col, frame.cursor.row));
        if changed.is_empty() && !cursor_moved {
            return Ok(Vec::new());
        }

        let mut body = Vec::new();
        if full {
            queue!(body, Clear(ClearType::All))?;
        }
        let repainted = !changed.is_empty();
        for row in changed {
            let text = &frame.rows[row];
            queue!(
                body,
                MoveTo(0, row as u16),
                Clear(ClearType::CurrentLine),
                Print(text)
            )?;
            paint_overlays(&mut body, frame, row as u16, capabilities)?;
        }

        // Always place the cursor after painting a row: Print and overlay
        // commands leave it somewhere on that row, even when its logical
        // position did not change.
        if cursor_moved || full || repainted {
            queue!(body, MoveTo(frame.cursor.col, frame.cursor.row), Show)?;
        }

        self.previous = Some(ScreenState {
            size: frame.size,
            rows: next_rows,
        });
        self.cursor = Some((frame.cursor.col, frame.cursor.row));

        if !capabilities.synchronized_output || body.is_empty() {
            return Ok(body);
        }
        let mut framed = Vec::with_capacity(SYNC_BEGIN.len() + body.len() + SYNC_END.len());
        framed.extend_from_slice(SYNC_BEGIN);
        framed.extend_from_slice(&body);
        framed.extend_from_slice(SYNC_END);
        Ok(framed)
    }
}

fn overlays_for(frame: &Frame, row: u16) -> Vec<Overlay> {
    let mut overlays = Vec::new();
    overlays.extend(
        frame
            .styles
            .iter()
            .copied()
            .filter(|span| span.row == row)
            .map(Overlay::Style),
    );
    overlays.extend(
        frame
            .search_matches
            .iter()
            .copied()
            .filter(|span| span.row == row)
            .map(Overlay::SearchMatch),
    );
    overlays.extend(
        frame
            .search_current
            .iter()
            .copied()
            .filter(|span| span.row == row)
            .map(Overlay::SearchCurrent),
    );
    overlays.extend(
        frame
            .selection
            .iter()
            .copied()
            .filter(|span| span.row == row)
            .map(Overlay::Selection),
    );
    overlays
}

fn paint_overlays(
    output: &mut Vec<u8>,
    frame: &Frame,
    row: u16,
    capabilities: Capabilities,
) -> io::Result<()> {
    let text = &frame.rows[usize::from(row)];
    for span in frame.styles.iter().copied().filter(|span| span.row == row) {
        queue_style(output, text, span, capabilities)?;
    }
    for span in frame
        .search_matches
        .iter()
        .copied()
        .filter(|span| span.row == row)
    {
        queue_span(output, text, span, Attribute::Underlined)?;
    }
    for span in frame
        .search_current
        .iter()
        .copied()
        .filter(|span| span.row == row)
    {
        queue_span(output, text, span, Attribute::Reverse)?;
    }
    for span in frame
        .selection
        .iter()
        .copied()
        .filter(|span| span.row == row)
    {
        queue_span(output, text, span, Attribute::Reverse)?;
    }
    Ok(())
}

/// Draws one semantic color run over an already-rendered row.
fn queue_style(
    output: &mut Vec<u8>,
    row: &str,
    span: StyledSpan,
    capabilities: Capabilities,
) -> io::Result<()> {
    let text = selected_text(
        row,
        SelectionSpan {
            row: span.row,
            start_col: span.start_col,
            end_col: span.end_col,
        },
    );
    if text.is_empty() {
        return Ok(());
    }

    queue!(output, MoveTo(span.start_col, span.row))?;
    let color = style_color(span.style, capabilities.color);
    if let Some(color) = color {
        queue!(output, SetForegroundColor(color))?;
    }
    if span.style == TextStyle::Title {
        queue!(output, SetAttribute(Attribute::Bold))?;
    }
    queue!(output, Print(text), SetAttribute(Attribute::Reset))?;
    if color.is_some() {
        queue!(output, ResetColor)?;
    }
    Ok(())
}

/// Draws one highlighted span over an already-rendered row.
fn queue_span(
    output: &mut Vec<u8>,
    row: &str,
    span: SelectionSpan,
    attribute: Attribute,
) -> io::Result<()> {
    let text = selected_text(row, span);
    if text.is_empty() {
        return Ok(());
    }
    queue!(
        output,
        MoveTo(span.start_col, span.row),
        SetAttribute(attribute),
        Print(text),
        SetAttribute(Attribute::Reset)
    )
}

/// Returns the terminal color for a semantic tone, or `None` in monochrome
/// mode. The title remains bold when color is disabled.
fn style_color(style: TextStyle, depth: ColorDepth) -> Option<Color> {
    match (style, depth) {
        (_, ColorDepth::None) => None,
        (TextStyle::Title, ColorDepth::TrueColor) => Some(Color::Rgb {
            r: 86,
            g: 156,
            b: 214,
        }),
        (TextStyle::Muted, ColorDepth::TrueColor) => Some(Color::Rgb {
            r: 128,
            g: 128,
            b: 128,
        }),
        (TextStyle::Icon, ColorDepth::TrueColor) => Some(Color::Rgb {
            r: 78,
            g: 201,
            b: 176,
        }),
        (TextStyle::Key, ColorDepth::TrueColor) => Some(Color::Rgb {
            r: 86,
            g: 156,
            b: 214,
        }),
        (TextStyle::Warm, ColorDepth::TrueColor) => Some(Color::Rgb {
            r: 229,
            g: 192,
            b: 123,
        }),
        (TextStyle::Title | TextStyle::Key, ColorDepth::Ansi256) => Some(Color::AnsiValue(75)),
        (TextStyle::Muted, ColorDepth::Ansi256) => Some(Color::AnsiValue(244)),
        (TextStyle::Icon, ColorDepth::Ansi256) => Some(Color::AnsiValue(79)),
        (TextStyle::Warm, ColorDepth::Ansi256) => Some(Color::AnsiValue(179)),
        (TextStyle::Title | TextStyle::Key, ColorDepth::Ansi16) => Some(Color::Blue),
        (TextStyle::Muted, ColorDepth::Ansi16) => Some(Color::Grey),
        (TextStyle::Icon, ColorDepth::Ansi16) => Some(Color::Cyan),
        (TextStyle::Warm, ColorDepth::Ansi16) => Some(Color::Yellow),
    }
}

/// Returns the cells covered by `span` as text that can be drawn over an
/// already-rendered row.
///
/// `row` is sanitized and tab-expanded, so display cells and grapheme widths
/// line up. A wide grapheme is included whole when any of its cells intersect
/// the span; the renderer builds spans on grapheme boundaries, so this is
/// normally an exact cell slice.
fn selected_text(row: &str, span: SelectionSpan) -> String {
    let start = usize::from(span.start_col);
    let end = usize::from(span.end_col);
    let mut output = String::new();
    let mut column = 0usize;

    for grapheme in row.graphemes(true) {
        if column >= end {
            break;
        }
        let width = grapheme_display_width(grapheme).max(1);
        if column + width > start {
            output.push_str(grapheme);
        }
        column += width;
    }
    if output.is_empty() && end > start {
        // A selected empty line or a selected line terminator has no text
        // cells to copy; draw reverse spaces so the selection is still
        // visible.
        output.push_str(&" ".repeat(end - start));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::ScreenPos;

    fn capabilities(color: ColorDepth, synchronized_output: bool) -> Capabilities {
        Capabilities {
            color,
            synchronized_output,
        }
    }

    fn frame(size: Size, rows: &[&str], cursor: ScreenPos) -> Frame {
        Frame {
            size,
            rows: rows.iter().map(|row| (*row).to_string()).collect(),
            horizontal_offset: 0,
            cursor,
            styles: Vec::new(),
            selection: Vec::new(),
            search_matches: Vec::new(),
            search_current: Vec::new(),
        }
    }

    fn text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }

    #[test]
    fn identical_frames_write_nothing() {
        let size = Size {
            width: 20,
            height: 2,
        };
        let frame = frame(size, &["alpha", "NORMAL"], ScreenPos { row: 0, col: 1 });
        let mut screen = Screen::default();
        let caps = capabilities(ColorDepth::Ansi16, false);

        let first = screen.render(&frame, caps).expect("first frame");
        assert!(text(&first).contains("alpha"));
        assert!(screen.render(&frame, caps).expect("same frame").is_empty());
    }

    #[test]
    fn a_changed_row_is_repainted_without_clearing_the_screen() {
        let size = Size {
            width: 20,
            height: 3,
        };
        let mut screen = Screen::default();
        let caps = capabilities(ColorDepth::Ansi16, false);
        let first = frame(size, &["one", "two", "three"], ScreenPos { row: 0, col: 0 });
        screen.render(&first, caps).expect("first frame");

        let second = frame(size, &["one", "TWO", "three"], ScreenPos { row: 1, col: 1 });
        let bytes = screen.render(&second, caps).expect("changed frame");
        let drawn = text(&bytes);
        assert!(drawn.contains("TWO"));
        assert!(!drawn.contains("one"));
        assert!(!drawn.contains("three"));
        assert!(
            !bytes.windows(4).any(|window| window == b"\x1b[2J"),
            "a changed row must not clear the whole screen: {drawn:?}"
        );
    }

    #[test]
    fn overlay_changes_redraw_their_row_even_when_the_text_is_unchanged() {
        let size = Size {
            width: 20,
            height: 2,
        };
        let mut screen = Screen::default();
        let caps = capabilities(ColorDepth::Ansi16, false);
        let mut first = frame(size, &["alpha", "NORMAL"], ScreenPos { row: 0, col: 0 });
        first.styles.push(StyledSpan {
            row: 0,
            start_col: 0,
            end_col: 5,
            style: TextStyle::Title,
        });
        screen.render(&first, caps).expect("first frame");

        let mut second = first.clone();
        second.styles[0].end_col = 3;
        let bytes = screen.render(&second, caps).expect("overlay change");
        assert!(
            !bytes.is_empty(),
            "a style change must repaint the affected row"
        );
        assert!(text(&bytes).contains("alpha"));
    }

    #[test]
    fn a_resize_repaints_every_row() {
        let mut screen = Screen::default();
        let caps = capabilities(ColorDepth::Ansi16, false);
        let first = frame(
            Size {
                width: 10,
                height: 2,
            },
            &["old one", "old two"],
            ScreenPos { row: 0, col: 0 },
        );
        screen.render(&first, caps).expect("first frame");

        let second = frame(
            Size {
                width: 20,
                height: 3,
            },
            &["new one", "new two", "new three"],
            ScreenPos { row: 2, col: 0 },
        );
        let bytes = screen.render(&second, caps).expect("resize");
        let drawn = text(&bytes);
        assert!(drawn.contains("new one"));
        assert!(drawn.contains("new two"));
        assert!(drawn.contains("new three"));
        assert!(bytes.windows(4).any(|window| window == b"\x1b[2J"));
    }

    #[test]
    fn synchronized_output_brackets_a_nonempty_frame_only() {
        let size = Size {
            width: 10,
            height: 1,
        };
        let frame = frame(size, &["alpha"], ScreenPos { row: 0, col: 0 });
        let mut screen = Screen::default();
        let caps = capabilities(ColorDepth::Ansi16, true);

        let first = screen.render(&frame, caps).expect("first frame");
        assert!(first.starts_with(SYNC_BEGIN));
        assert!(first.ends_with(SYNC_END));
        assert!(screen.render(&frame, caps).expect("same frame").is_empty());
    }

    #[test]
    fn monochrome_keeps_attributes_without_foreground_colors() {
        assert_eq!(style_color(TextStyle::Title, ColorDepth::None), None);
        assert_eq!(
            style_color(TextStyle::Title, ColorDepth::TrueColor),
            Some(Color::Rgb {
                r: 86,
                g: 156,
                b: 214
            })
        );
    }
}
