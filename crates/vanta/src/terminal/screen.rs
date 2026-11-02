//! Cell-level damage rendering for the crossterm backend.
//!
//! The front buffer is a grid of grapheme cells with a foreground style and
//! attributes. A frame compares cells, expands changed runs to whole wide
//! glyphs and writes only those runs. Identical frames write nothing.
//!
//! A one-line vertical viewport move is painted with a terminal scroll region:
//! the text area scrolls, the front buffer is shifted to match, and only the
//! newly exposed row plus any changed status cells are written. The first frame
//! and resizes are full repaints.

use std::io;

use crossterm::cursor::{MoveTo, Show};
use crossterm::queue;
use crossterm::style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType};
use unicode_segmentation::UnicodeSegmentation;
use vanta_text::grapheme_display_width;

use super::capabilities::{Capabilities, ColorDepth};
use super::{Frame, Size, TextStyle};

const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
const SYNC_END: &[u8] = b"\x1b[?2026l";
const SCROLL_UP: &[u8] = b"\x1b[S";
const SCROLL_DOWN: &[u8] = b"\x1b[T";
const RESET_SCROLL_REGION: &[u8] = b"\x1b[r";

/// The retained front buffer and cursor for one terminal.
#[derive(Debug, Default)]
pub(crate) struct Screen {
    previous: Option<Grid>,
    cursor: Option<(u16, u16)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Grid {
    size: Size,
    top_line: usize,
    cells: Vec<Cell>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Cell {
    text: String,
    width: u8,
    continuation: bool,
    style: CellStyle,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CellStyle {
    foreground: Option<Color>,
    bold: bool,
    underline: bool,
    reverse: bool,
}

impl Cell {
    fn blank() -> Self {
        Self {
            text: " ".to_string(),
            width: 1,
            continuation: false,
            style: CellStyle::default(),
        }
    }

    fn continuation(style: CellStyle) -> Self {
        Self {
            text: String::new(),
            width: 0,
            continuation: true,
            style,
        }
    }
}

impl Grid {
    fn blank(size: Size, top_line: usize) -> Self {
        let count = usize::from(size.width).saturating_mul(usize::from(size.height));
        Self {
            size,
            top_line,
            cells: vec![Cell::blank(); count],
        }
    }
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
        let next = build_grid(frame, capabilities);
        let previous = self.previous.clone();

        let mut full = match &previous {
            None => true,
            Some(previous) => {
                previous.size != next.size || previous.cells.len() != next.cells.len()
            }
        };
        let mut scroll = None;
        if !full {
            let previous = previous.as_ref().expect("full is false");
            if previous.top_line != next.top_line {
                if usize::from(next.size.height) > 1 && next.top_line == previous.top_line + 1 {
                    scroll = Some(1);
                } else if usize::from(next.size.height) > 1
                    && previous.top_line == next.top_line + 1
                {
                    scroll = Some(-1);
                } else {
                    full = true;
                }
            }
        }

        let mut body = Vec::new();
        let mut base = if full {
            Grid::blank(next.size, next.top_line)
        } else {
            previous.expect("a non-full frame has a previous grid")
        };
        if let Some(direction) = scroll {
            queue_scroll(&mut body, next.size, direction)?;
            shift_grid(&mut base, direction);
        }

        let changed = if full {
            full_damage(&next)
        } else {
            cell_damage(&base, &next)
        };
        let repainted = changed.iter().any(|changed| *changed);
        paint_cells(&mut body, &next, &changed, full)?;

        let cursor_moved = self.cursor != Some((frame.cursor.col, frame.cursor.row));
        if cursor_moved || full || repainted || scroll.is_some() {
            queue!(body, MoveTo(frame.cursor.col, frame.cursor.row), Show)?;
        }
        if body.is_empty() {
            return Ok(Vec::new());
        }

        self.previous = Some(next);
        self.cursor = Some((frame.cursor.col, frame.cursor.row));

        if !capabilities.synchronized_output {
            return Ok(body);
        }
        let mut framed = Vec::with_capacity(SYNC_BEGIN.len() + body.len() + SYNC_END.len());
        framed.extend_from_slice(SYNC_BEGIN);
        framed.extend_from_slice(&body);
        framed.extend_from_slice(SYNC_END);
        Ok(framed)
    }
}

fn build_grid(frame: &Frame, capabilities: Capabilities) -> Grid {
    let width = usize::from(frame.size.width);
    let height = usize::from(frame.size.height);
    let mut grid = Grid::blank(frame.size, frame.top_line);

    for row in 0..height {
        let Some(text) = frame.rows.get(row) else {
            continue;
        };
        let base = row * width;
        let mut column = 0usize;
        for grapheme in text.graphemes(true) {
            let cell_width = grapheme_display_width(grapheme).max(1);
            if column + cell_width > width {
                break;
            }
            let style = CellStyle::default();
            grid.cells[base + column] = Cell {
                text: grapheme.to_string(),
                width: u8::try_from(cell_width).unwrap_or(1),
                continuation: false,
                style,
            };
            for offset in 1..cell_width {
                grid.cells[base + column + offset] = Cell::continuation(style);
            }
            column += cell_width;
        }

        for span in frame
            .styles
            .iter()
            .copied()
            .filter(|span| span.row as usize == row)
        {
            let style = CellStyle {
                foreground: style_color(span.style, capabilities.color),
                bold: span.style == TextStyle::Title,
                underline: false,
                reverse: false,
            };
            apply_style(
                &mut grid.cells,
                base,
                width,
                span.start_col as usize,
                span.end_col as usize,
                style,
            );
        }
        for span in frame
            .search_matches
            .iter()
            .copied()
            .filter(|span| span.row as usize == row)
        {
            apply_attribute(
                &mut grid.cells,
                base,
                width,
                span.start_col as usize,
                span.end_col as usize,
                |style| style.underline = true,
            );
        }
        for span in frame
            .search_current
            .iter()
            .copied()
            .filter(|span| span.row as usize == row)
        {
            apply_attribute(
                &mut grid.cells,
                base,
                width,
                span.start_col as usize,
                span.end_col as usize,
                |style| style.reverse = true,
            );
        }
        for span in frame
            .selection
            .iter()
            .copied()
            .filter(|span| span.row as usize == row)
        {
            apply_attribute(
                &mut grid.cells,
                base,
                width,
                span.start_col as usize,
                span.end_col as usize,
                |style| style.reverse = true,
            );
        }
    }

    grid
}

fn apply_style(
    cells: &mut [Cell],
    base: usize,
    width: usize,
    start: usize,
    end: usize,
    style: CellStyle,
) {
    let start = start.min(width);
    let end = end.min(width);
    for column in start..end {
        cells[base + column].style = style;
    }
}

fn apply_attribute(
    cells: &mut [Cell],
    base: usize,
    width: usize,
    start: usize,
    end: usize,
    update: impl Fn(&mut CellStyle),
) {
    let start = start.min(width);
    let end = end.min(width);
    for column in start..end {
        update(&mut cells[base + column].style);
    }
}

fn full_damage(next: &Grid) -> Vec<bool> {
    vec![true; next.cells.len()]
}

fn cell_damage(previous: &Grid, next: &Grid) -> Vec<bool> {
    let width = usize::from(next.size.width);
    let height = usize::from(next.size.height);
    let mut changed = Vec::with_capacity(previous.cells.len());
    for row in 0..height {
        for column in 0..width {
            let index = row * width + column;
            changed.push(previous.cells[index] != next.cells[index]);
        }
    }

    // A changed cell inside a wide glyph must repaint the whole glyph. Apply
    // this to both grids so replacing a wide glyph cannot leave its trailing
    // cell behind.
    for grid in [previous, next] {
        for row in 0..height {
            let base = row * width;
            let mut column = 0;
            while column < width {
                let cell = &grid.cells[base + column];
                if cell.continuation {
                    column += 1;
                    continue;
                }
                let span = usize::from(cell.width.max(1)).min(width - column);
                if (column..column + span).any(|index| changed[base + index]) {
                    for index in column..column + span {
                        changed[base + index] = true;
                    }
                }
                column += span.max(1);
            }
        }
    }
    changed
}

fn paint_cells(output: &mut Vec<u8>, grid: &Grid, changed: &[bool], full: bool) -> io::Result<()> {
    let width = usize::from(grid.size.width);
    let height = usize::from(grid.size.height);
    if full {
        queue!(output, Clear(ClearType::All))?;
    }

    for row in 0..height {
        let base = row * width;
        let mut column = 0;
        while column < width {
            if !changed[base + column] {
                column += 1;
                continue;
            }
            let run_start = column;
            while column < width && changed[base + column] {
                column += 1;
            }
            paint_run(output, grid, row, run_start, column)?;
        }
    }
    Ok(())
}

fn paint_run(
    output: &mut Vec<u8>,
    grid: &Grid,
    row: usize,
    start: usize,
    end: usize,
) -> io::Result<()> {
    let width = usize::from(grid.size.width);
    let base = row * width;
    let mut column = start;
    while column < end {
        let cell = &grid.cells[base + column];
        if cell.continuation {
            column += 1;
            continue;
        }
        let style = cell.style;
        let mut text = String::new();
        let mut next_column = column;
        while next_column < end {
            let next = &grid.cells[base + next_column];
            if next.continuation {
                next_column += 1;
                continue;
            }
            if next.style != style {
                break;
            }
            text.push_str(&next.text);
            next_column += usize::from(next.width.max(1));
        }
        if text.is_empty() {
            column = column.saturating_add(1);
            continue;
        }
        queue!(output, MoveTo(column as u16, row as u16))?;
        queue_style(output, style)?;
        queue!(output, Print(text), SetAttribute(Attribute::Reset))?;
        if style.foreground.is_some() {
            queue!(output, ResetColor)?;
        }
        column = next_column;
    }
    Ok(())
}

fn queue_style(output: &mut Vec<u8>, style: CellStyle) -> io::Result<()> {
    if let Some(foreground) = style.foreground {
        queue!(output, SetForegroundColor(foreground))?;
    }
    if style.bold {
        queue!(output, SetAttribute(Attribute::Bold))?;
    }
    if style.underline {
        queue!(output, SetAttribute(Attribute::Underlined))?;
    }
    if style.reverse {
        queue!(output, SetAttribute(Attribute::Reverse))?;
    }
    Ok(())
}

fn queue_scroll(output: &mut Vec<u8>, size: Size, direction: i8) -> io::Result<()> {
    let bottom = size.height.saturating_sub(1);
    if bottom == 0 {
        return Ok(());
    }
    output.extend_from_slice(format!("\x1b[1;{bottom}r").as_bytes());
    output.extend_from_slice(if direction > 0 {
        SCROLL_UP
    } else {
        SCROLL_DOWN
    });
    output.extend_from_slice(RESET_SCROLL_REGION);
    Ok(())
}

fn shift_grid(grid: &mut Grid, direction: i8) {
    let width = usize::from(grid.size.width);
    let text_rows = usize::from(grid.size.height).saturating_sub(1);
    if width == 0 || text_rows == 0 {
        return;
    }
    if direction > 0 {
        for row in 0..text_rows.saturating_sub(1) {
            for column in 0..width {
                grid.cells[row * width + column] = grid.cells[(row + 1) * width + column].clone();
            }
        }
        for column in 0..width {
            grid.cells[(text_rows - 1) * width + column] = Cell::blank();
        }
    } else {
        for row in (1..text_rows).rev() {
            for column in 0..width {
                grid.cells[row * width + column] = grid.cells[(row - 1) * width + column].clone();
            }
        }
        for column in 0..width {
            grid.cells[column] = Cell::blank();
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{ScreenPos, StyledSpan};

    fn capabilities(color: ColorDepth, synchronized_output: bool) -> Capabilities {
        Capabilities {
            color,
            synchronized_output,
        }
    }

    fn frame_at(size: Size, top_line: usize, rows: &[&str], cursor: ScreenPos) -> Frame {
        Frame {
            size,
            top_line,
            rows: rows.iter().map(|row| (*row).to_string()).collect(),
            horizontal_offset: 0,
            cursor,
            styles: Vec::new(),
            selection: Vec::new(),
            search_matches: Vec::new(),
            search_current: Vec::new(),
        }
    }

    fn frame(size: Size, rows: &[&str], cursor: ScreenPos) -> Frame {
        frame_at(size, 0, rows, cursor)
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
    fn a_changed_cell_repaints_only_its_run_without_clearing_the_screen() {
        let size = Size {
            width: 20,
            height: 3,
        };
        let mut screen = Screen::default();
        let caps = capabilities(ColorDepth::Ansi16, false);
        let first = frame(size, &["one", "two", "three"], ScreenPos { row: 0, col: 0 });
        screen.render(&first, caps).expect("first frame");

        let second = frame(size, &["one", "tWo", "three"], ScreenPos { row: 1, col: 1 });
        let bytes = screen.render(&second, caps).expect("changed frame");
        let drawn = text(&bytes);
        assert!(drawn.contains('W'));
        assert!(drawn.contains("\x1b[2;2H"));
        assert!(!drawn.contains("one"));
        assert!(!drawn.contains("three"));
        assert!(
            !bytes.windows(4).any(|window| window == b"\x1b[2J"),
            "a changed cell must not clear the whole screen: {drawn:?}"
        );
    }

    #[test]
    fn a_cursor_only_move_does_not_emit_buffer_cells() {
        let size = Size {
            width: 20,
            height: 3,
        };
        let mut screen = Screen::default();
        let caps = capabilities(ColorDepth::Ansi16, false);
        screen
            .render(
                &frame(
                    size,
                    &["alpha", "beta", "NORMAL  1:1"],
                    ScreenPos { row: 0, col: 0 },
                ),
                caps,
            )
            .expect("first frame");

        let bytes = screen
            .render(
                &frame(
                    size,
                    &["alpha", "beta", "NORMAL  1:2"],
                    ScreenPos { row: 0, col: 1 },
                ),
                caps,
            )
            .expect("cursor-only frame");
        let drawn = text(&bytes);
        assert!(!drawn.contains("alpha"));
        assert!(!drawn.contains("beta"));
        assert!(drawn.contains("\x1b[3;11H"));
        assert!(drawn.contains('2'));
        assert!(!bytes.windows(4).any(|window| window == b"\x1b[2J"));
    }

    #[test]
    fn one_line_scroll_repaints_only_the_new_row() {
        let size = Size {
            width: 10,
            height: 4,
        };
        let mut screen = Screen::default();
        let caps = capabilities(ColorDepth::Ansi16, false);
        screen
            .render(
                &frame_at(
                    size,
                    0,
                    &["alpha", "bravo", "charlie", "NORMAL"],
                    ScreenPos { row: 0, col: 0 },
                ),
                caps,
            )
            .expect("first frame");

        let bytes = screen
            .render(
                &frame_at(
                    size,
                    1,
                    &["bravo", "charlie", "delta", "NORMAL"],
                    ScreenPos { row: 2, col: 0 },
                ),
                caps,
            )
            .expect("scroll frame");
        let drawn = text(&bytes);
        assert!(drawn.contains("delta"));
        assert!(!drawn.contains("bravo"));
        assert!(!drawn.contains("charlie"));
        assert!(drawn.contains("\x1b[1;3r\x1b[S\x1b[r"));
        assert!(!bytes.windows(4).any(|window| window == b"\x1b[2J"));
    }

    #[test]
    fn overlay_changes_redraw_their_cells_even_when_the_text_is_unchanged() {
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
            "a style change must repaint the affected cells"
        );
        let drawn = text(&bytes);
        assert!(drawn.contains("ha"));
        assert!(!drawn.contains("alpha"));
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
        assert!(drawn.contains("new"));
        assert!(drawn.contains("one"));
        assert!(drawn.contains("two"));
        assert!(drawn.contains("three"));
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
