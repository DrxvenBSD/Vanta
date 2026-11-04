//! Read-only frame composition.
//!
//! Rendering consumes the editor's current state and never mutates it. Buffer
//! text is sanitized here before it reaches a terminal; Visual-mode selection
//! spans and search matches are computed at this boundary as read-only overlay
//! state on [`Frame`].

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;
use vanta_text::{BytePos, LineNumber, display_col_of_byte, grapheme_width_at, line_display_width};

use super::{Editor, Mode};
use crate::document::{RecoveryInfo, RecoveryState, TextRange, VisualKind, literal_matches};
use crate::terminal::{Frame, ScreenPos, SelectionSpan, Size, StyledSpan, TextStyle};
use crate::view::TABSTOP;

const LOGO: [&str; 6] = [
    "██╗   ██╗ █████╗ ███╗   ██╗████████╗ █████╗ ",
    "██║   ██║██╔══██╗████╗  ██║╚══██╔══╝██╔══██╗",
    "██║   ██║███████║██╔██╗ ██║   ██║   ███████║",
    "╚██╗ ██╔╝██╔══██║██║╚██╗██║   ██║   ██╔══██║",
    " ╚████╔╝ ██║  ██║██║ ╚████║   ██║   ██║  ██║",
    "  ╚═══╝  ╚═╝  ╚═╝╚═╝  ╚═══╝   ╚═╝   ╚═╝  ╚═╝",
];
const ICON_OPEN: &str = "\u{f07c}";
const ICON_INSERT: &str = "\u{f15b}";
const ICON_SAVE: &str = "\u{f0c7}";
const ICON_FIND: &str = "\u{f002}";
const ICON_UNDO: &str = "\u{f0e2}";
const ICON_REDO: &str = "\u{f01e}";
const ICON_QUIT: &str = "\u{f011}";
const ICON_BOLT: &str = "\u{f0e7}";

impl Editor {
    /// Builds the complete frame for the current terminal size.
    pub(super) fn frame(&self, size: Size) -> Frame {
        if let Some(info) = self.document.recovery() {
            return self.recovery_frame(size, info);
        }
        if self.tabs[self.active_tab].windows.len() > 1 {
            return self.split_frame(size);
        }
        let height = usize::from(size.height.max(1));
        let width = usize::from(size.width.max(1));
        let text_rows = height - 1;
        let left = self.left_col;
        let cursor = self.document.cursor();
        let top = cursor
            .line
            .get()
            .saturating_sub(text_rows.saturating_sub(1));
        let mut rows = Vec::with_capacity(height);
        let buffer = self.document.buffer();

        for row in 0..text_rows {
            let line_index = top + row;
            let text = if line_index < buffer.len_lines() {
                buffer
                    .line_text(LineNumber::new(line_index))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            rows.push(render_line(&text, left, width));
        }

        let mut styles = Vec::new();
        if self.show_start_banner() {
            overlay_start_banner(&mut rows, &mut styles, width);
        }

        let (cursor_row, cursor_col) = if matches!(self.mode, Mode::Search) {
            let line = format!("/{}", self.search_input);
            rows.push(render_line(&line, 0, width));
            let column = line_display_width(&line, TABSTOP).get().min(width - 1);
            (height - 1, column)
        } else if let Some(command) = &self.command {
            let line = format!(":{command}");
            rows.push(render_line(&line, 0, width));
            let column = line_display_width(&line, TABSTOP).get().min(width - 1);
            (height - 1, column)
        } else {
            rows.push(render_line(&self.status_text(), 0, width));
            let line = self.document.line_text();
            let column = display_col_of_byte(&line, cursor.col.get(), TABSTOP)
                .get()
                .saturating_sub(left)
                .min(width - 1);
            let row = cursor
                .line
                .get()
                .saturating_sub(top)
                .min(text_rows.saturating_sub(1));
            (row, column)
        };

        Frame {
            size,
            top_line: top,
            rows,
            horizontal_offset: left,
            cursor: ScreenPos {
                row: u16::try_from(cursor_row).unwrap_or(u16::MAX),
                col: u16::try_from(cursor_col).unwrap_or(u16::MAX),
            },
            styles,
            selection: self.selection_spans(top, text_rows, width, left),
            search_matches: self.search_match_spans(top, text_rows, width, left),
            search_current: self.search_current_spans(top, text_rows, width, left),
        }
    }

    fn split_frame(&self, size: Size) -> Frame {
        let width = usize::from(size.width.max(1));
        let height = usize::from(size.height.max(1));
        let area = super::Rect {
            x: 0,
            y: 0,
            width,
            height: height.saturating_sub(1),
        };
        let tab = &self.tabs[self.active_tab];
        let mut rows = self.compose_layout(&tab.layout, area);
        let prompt = if matches!(self.mode, Mode::Search) {
            Some(format!("/{}", self.search_input))
        } else {
            self.command.as_ref().map(|command| format!(":{command}"))
        };
        let bottom = prompt.unwrap_or_else(|| self.status_text());
        rows.push(render_line(&bottom, 0, width));
        rows.resize(height, String::new());

        let mut rects = Vec::new();
        super::layout_rects(&tab.layout, area, &mut rects);
        let focus = tab.focus;
        let mut cursor = ScreenPos { row: 0, col: 0 };
        let mut top_line = 0;
        let mut horizontal_offset = 0;
        if let Some((_, rect)) = rects.iter().find(|(id, _)| *id == focus) {
            let window = self
                .window_by_id(self.active_tab, focus)
                .expect("the focused window belongs to the active tab");
            let text_rows = rect.height.saturating_sub(1).max(1);
            let line = self.document.line_text();
            let column = display_col_of_byte(&line, self.document.cursor().col.get(), TABSTOP)
                .get()
                .saturating_sub(window.left_col)
                .min(rect.width.saturating_sub(1));
            let row = self
                .document
                .cursor()
                .line
                .get()
                .saturating_sub(window.top_line)
                .min(text_rows - 1);
            cursor = ScreenPos {
                row: u16::try_from(rect.y + row).unwrap_or(u16::MAX),
                col: u16::try_from(rect.x + column).unwrap_or(u16::MAX),
            };
            top_line = window.top_line;
            horizontal_offset = window.left_col;
        }

        Frame {
            size,
            top_line,
            rows,
            horizontal_offset,
            cursor,
            styles: Vec::new(),
            selection: Vec::new(),
            search_matches: Vec::new(),
            search_current: Vec::new(),
        }
    }

    fn compose_layout(&self, layout: &super::Layout, area: super::Rect) -> Vec<String> {
        match layout {
            super::Layout::Leaf(id) => self.render_window(*id, area),
            super::Layout::Split {
                vertical,
                first,
                second,
            } => {
                if *vertical {
                    let separator = usize::from(area.width >= 3);
                    let available = area.width.saturating_sub(separator);
                    let first_width = available / 2;
                    let second_width = available - first_width;
                    let left = self.compose_layout(
                        first,
                        super::Rect {
                            width: first_width,
                            ..area
                        },
                    );
                    let right = self.compose_layout(
                        second,
                        super::Rect {
                            x: area.x + first_width + separator,
                            width: second_width,
                            ..area
                        },
                    );
                    (0..area.height)
                        .map(|row| {
                            let mut line = left.get(row).cloned().unwrap_or_default();
                            if separator > 0 {
                                line.push('│');
                            }
                            line.push_str(right.get(row).map(String::as_str).unwrap_or_default());
                            line
                        })
                        .collect()
                } else {
                    let first_height = area.height / 2;
                    let second_height = area.height - first_height;
                    let mut rows = self.compose_layout(
                        first,
                        super::Rect {
                            height: first_height,
                            ..area
                        },
                    );
                    rows.extend(self.compose_layout(
                        second,
                        super::Rect {
                            y: area.y + first_height,
                            height: second_height,
                            ..area
                        },
                    ));
                    rows
                }
            }
        }
    }

    fn render_window(&self, window_id: usize, area: super::Rect) -> Vec<String> {
        let Some(window) = self.window_by_id(self.active_tab, window_id) else {
            return vec![" ".repeat(area.width); area.height];
        };
        let Some(document) = self.document_by_id(window.buffer_id) else {
            return vec![" ".repeat(area.width); area.height];
        };
        let text_rows = area.height.saturating_sub(1);
        let mut rows = Vec::with_capacity(area.height);
        for offset in 0..text_rows {
            let line = LineNumber::new(window.top_line + offset);
            let text = document.buffer().line_text(line).unwrap_or_default();
            rows.push(pad_cells(
                &render_line(&text, window.left_col, area.width),
                area.width,
            ));
        }
        if area.height > 0 {
            let focused = self.tabs[self.active_tab].focus == window_id;
            let modified = if document.is_modified() { " [+]" } else { "" };
            let marker = if focused { "*" } else { " " };
            let status = format!(
                "{marker}{}{modified} {}:{}",
                document.display_name(),
                window.cursor.line.get() + 1,
                window.cursor.col.get() + 1
            );
            rows.push(pad_cells(&render_line(&status, 0, area.width), area.width));
        }
        rows
    }

    /// Converts the active Visual selection into screen-cell runs for the
    /// visible rows.
    ///
    /// Buffer bytes are converted to display columns through the same width
    /// helper the cursor uses, so tabs and wide graphemes select as one unit.
    /// A selected line terminator is represented by at least one cell at the
    /// end of the row, which keeps a newline-only selection visible.
    fn selection_spans(
        &self,
        top: usize,
        rows: usize,
        width: usize,
        left: usize,
    ) -> Vec<SelectionSpan> {
        let Mode::Visual(visual) = self.mode else {
            return Vec::new();
        };
        let Ok(range) = self.document.visual_range(visual.anchor, visual.kind) else {
            return Vec::new();
        };
        let TextRange::Linewise(lines) = &range else {
            if let TextRange::Charwise(bytes) = &range {
                return self.charwise_spans(bytes, top, rows, width, left, true);
            }
            return Vec::new();
        };

        let buffer = self.document.buffer();
        let mut spans = Vec::new();
        for offset in 0..rows {
            let line_index = top + offset;
            if line_index >= buffer.len_lines() {
                break;
            }
            let line = LineNumber::new(line_index);
            if line < lines.start || line >= lines.end {
                continue;
            }
            let Ok(text) = buffer.line_text(line) else {
                continue;
            };
            let row = u16::try_from(offset).unwrap_or(u16::MAX);
            let end = line_display_width(&text, TABSTOP)
                .get()
                .saturating_sub(left)
                .max(1)
                .min(width);
            push_span(&mut spans, row, 0, end, width);
        }
        spans
    }

    /// Converts one byte range into visible display-cell runs.
    ///
    /// `mark_newline` adds one cell at end of line when the range includes the
    /// terminator, so a newline-only Visual selection remains visible. Search
    /// matches never include a line terminator and pass `false`.
    fn charwise_spans(
        &self,
        bytes: &Range<BytePos>,
        top: usize,
        rows: usize,
        width: usize,
        left: usize,
        mark_newline: bool,
    ) -> Vec<SelectionSpan> {
        let buffer = self.document.buffer();
        let mut spans = Vec::new();
        for offset in 0..rows {
            let line_index = top + offset;
            if line_index >= buffer.len_lines() {
                break;
            }
            let line = LineNumber::new(line_index);
            let Ok(content) = buffer.line_byte_range(line) else {
                continue;
            };
            let Ok(text) = buffer.line_text(line) else {
                continue;
            };
            let row = u16::try_from(offset).unwrap_or(u16::MAX);
            let selected_start = bytes.start.max(content.start);
            let selected_end = bytes.end.min(content.end);
            if selected_start < selected_end {
                let start =
                    display_col_of_byte(&text, selected_start.get() - content.start.get(), TABSTOP)
                        .get()
                        .saturating_sub(left);
                let end =
                    display_col_of_byte(&text, selected_end.get() - content.start.get(), TABSTOP)
                        .get()
                        .saturating_sub(left);
                push_span(&mut spans, row, start, end, width);
            }

            if mark_newline && bytes.start <= content.end && bytes.end > content.end {
                let marker = display_col_of_byte(&text, text.len(), TABSTOP).get();
                if marker >= left && marker < left.saturating_add(width) {
                    push_span(&mut spans, row, marker - left, marker - left + 1, width);
                }
            }
        }
        spans
    }

    /// Highlights every visible occurrence of the current query.
    ///
    /// The command line cannot contain a newline, so scanning each visible
    /// line is equivalent to a whole-buffer scan for the patterns the editor
    /// accepts, and costs only the visible text per frame. Only visible lines
    /// are scanned; the active match is rendered separately from its buffer
    /// range, so scrolling cannot lose it.
    fn search_match_spans(
        &self,
        top: usize,
        rows: usize,
        width: usize,
        left: usize,
    ) -> Vec<SelectionSpan> {
        let Some(query) = self.search.query.as_deref() else {
            return Vec::new();
        };
        if query.is_empty() {
            return Vec::new();
        }
        let buffer = self.document.buffer();
        let mut spans = Vec::new();
        for offset in 0..rows {
            let line_index = top + offset;
            if line_index >= buffer.len_lines() {
                break;
            }
            let line = LineNumber::new(line_index);
            let Ok(text) = buffer.line_text(line) else {
                continue;
            };
            let row = u16::try_from(offset).unwrap_or(u16::MAX);
            for range in literal_matches(&text, query) {
                let start = display_col_of_byte(&text, range.start, TABSTOP)
                    .get()
                    .saturating_sub(left);
                let end = display_col_of_byte(&text, range.end, TABSTOP)
                    .get()
                    .saturating_sub(left);
                push_span(&mut spans, row, start, end, width);
            }
        }
        spans
    }

    fn search_current_spans(
        &self,
        top: usize,
        rows: usize,
        width: usize,
        left: usize,
    ) -> Vec<SelectionSpan> {
        let Some(current) = &self.search.current else {
            return Vec::new();
        };
        self.charwise_spans(current, top, rows, width, left, false)
    }

    /// Returns whether the no-file startup hint should replace the empty
    /// buffer rows for this frame.
    ///
    /// The hint is display-only: it requires an empty, clean untitled buffer in
    /// Normal mode with no status message. Starting any other mode removes it,
    /// so it can never become text or part of an undo block.
    fn show_start_banner(&self) -> bool {
        matches!(self.mode, Mode::Normal)
            && self.command.is_none()
            && self.message.is_none()
            && self.document.is_untitled()
            && self.document.buffer().len_bytes() == 0
            && !self.document.is_modified()
    }

    fn status_text(&self) -> String {
        let mode = match self.mode {
            Mode::Normal => "NORMAL",
            Mode::Visual(visual) => match visual.kind {
                VisualKind::Charwise => "VISUAL",
                VisualKind::Linewise => "V-LINE",
                VisualKind::Blockwise { .. } => "V-BLOCK",
            },
            Mode::Select(visual) => match visual.kind {
                VisualKind::Charwise => "SELECT",
                VisualKind::Linewise => "S-LINE",
                VisualKind::Blockwise { .. } => "S-BLOCK",
            },
            Mode::Insert => "INSERT",
            Mode::Replace => "REPLACE",
            Mode::Command => "COMMAND",
            Mode::Search => "SEARCH",
        };

        let flags = if self.document.is_modified() {
            " [+]"
        } else {
            ""
        };
        let large = if self.document.is_large_file() {
            " [LARGE]"
        } else {
            ""
        };
        let cursor = self.document.cursor();
        let position = format!("{}:{}", cursor.line.get() + 1, cursor.col.get() + 1);
        let mut message = self.message.clone().unwrap_or_default();
        if let Some(warning) = self.document.swap_warning() {
            if !message.is_empty() {
                message.push_str(" | ");
            }
            message.push_str(warning);
        }
        // Mode and cursor come first so they survive truncation on narrow
        // terminals; the file name follows.
        format!(
            "{mode}  {position}  {}{flags}{large}  {message}",
            self.document.display_name()
        )
    }

    /// Builds the blocking swap-recovery page.
    ///
    /// This page deliberately replaces the buffer frame: no normal editing
    /// key is interpreted until the user chooses what to do with the swap.
    fn recovery_frame(&self, size: Size, info: &RecoveryInfo) -> Frame {
        let height = usize::from(size.height.max(1));
        let width = usize::from(size.width.max(1));
        let mut lines = vec![
            "E325: ATTENTION".to_string(),
            format!("Found a swap file by the name {}", info.swap_path.display()),
            format!("Original file: {}", self.document.display_name()),
        ];

        match &info.state {
            RecoveryState::Recoverable { snapshot, stale } => {
                lines.push(format!("Owner: {}  PID {}", snapshot.host, snapshot.pid));
                lines.push(format!("Started: {} (unix seconds)", snapshot.started_at));
                lines.push(format!(
                    "Modified: {}",
                    if snapshot.modified { "YES" } else { "NO" }
                ));
                lines.push(match stale {
                    Some(reason) => format!("Stale: {reason}"),
                    None => "The writing process may still be running.".to_string(),
                });
                lines.push(String::new());
                lines.push(
                    "[R]ecover  [O]pen original  [E]dit anyway  [D]elete swap  [Q]uit  [A]bort"
                        .to_string(),
                );
            }
            RecoveryState::Mismatched { .. } => {
                lines.push(
                    info.problem()
                        .unwrap_or_else(|| "swap does not match this file".to_string()),
                );
                lines.push(String::new());
                lines.push("[D]elete swap  [Q]uit  [A]bort".to_string());
            }
            RecoveryState::Corrupt(error) => {
                lines.push(format!("Cannot parse swap: {error}"));
                lines.push(String::new());
                lines.push("[D]elete swap  [Q]uit  [A]bort".to_string());
            }
        }

        if let Some(message) = &self.message {
            lines.push(String::new());
            lines.push(message.clone());
        }

        let mut rows: Vec<String> = lines
            .into_iter()
            .take(height)
            .map(|line| render_line(&line, 0, width))
            .collect();
        rows.resize(height, String::new());
        let title_end = display_width(&rows[0]).min(width);
        let styles = if title_end > 0 {
            vec![StyledSpan {
                row: 0,
                start_col: 0,
                end_col: u16::try_from(title_end).unwrap_or(u16::MAX),
                style: TextStyle::Warm,
            }]
        } else {
            Vec::new()
        };

        Frame {
            size,
            top_line: 0,
            rows,
            horizontal_offset: 0,
            cursor: ScreenPos { row: 0, col: 0 },
            styles,
            selection: Vec::new(),
            search_matches: Vec::new(),
            search_current: Vec::new(),
        }
    }
}

/// One row of the startup page before it is centered in the terminal.
struct BannerLine {
    text: String,
    spans: Vec<(usize, usize, TextStyle)>,
}

impl BannerLine {
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            spans: Vec::new(),
        }
    }

    fn styled(text: impl Into<String>, style: TextStyle) -> Self {
        let text = text.into();
        let width = display_width(&text);
        Self {
            text,
            spans: vec![(0, width, style)],
        }
    }
}

/// Draws the no-file startup page centered in the text rows.
///
/// Rows outside the available text area are skipped, so a terminal with fewer
/// rows than the page simply shows its leading lines. The caller has already
/// established that the buffer is empty and untitled; this function only
/// formats display rows and never mutates editor state.
fn overlay_start_banner(rows: &mut [String], styles: &mut Vec<StyledSpan>, width: usize) {
    let compact = width < 48 || rows.len() < 18;
    let mut lines = start_page(compact, width.saturating_sub(2).min(44));

    // Very short frames cannot hold the full page. Keep the leading content
    // rather than panicking or writing outside the text area; the tests still
    // assert that the frame is well formed at 1x1 and 5x3.
    if rows.len() < 9 {
        let visible = lines.len().min(rows.len());
        let top = rows.len().saturating_sub(visible) / 2;
        for (offset, line) in lines.iter().take(visible).enumerate() {
            place_banner_line(rows, styles, width, top + offset, line);
        }
        return;
    }

    let hint = lines.pop().expect("the start page always ends with a hint");
    let top = 1;
    for (offset, line) in lines.iter().enumerate() {
        place_banner_line(rows, styles, width, top + offset, line);
    }

    let hint_row = rows.len().saturating_sub(2).max(top + lines.len());
    if hint_row < rows.len() {
        place_banner_line(rows, styles, width, hint_row, &hint);
    }
}

fn place_banner_line(
    rows: &mut [String],
    styles: &mut Vec<StyledSpan>,
    width: usize,
    row: usize,
    line: &BannerLine,
) {
    let rendered = render_line(&line.text, 0, width);
    let cells = display_width(&rendered);
    let padding = width.saturating_sub(cells) / 2;
    let Some(target) = rows.get_mut(row) else {
        return;
    };
    *target = if rendered.is_empty() {
        String::new()
    } else {
        format!("{}{}", " ".repeat(padding), rendered)
    };
    for (start, end, style) in &line.spans {
        push_style(
            styles,
            row,
            padding.saturating_add(*start).min(width),
            padding.saturating_add(*end).min(width),
            *style,
        );
    }
}

fn start_page(compact: bool, menu_width: usize) -> Vec<BannerLine> {
    let version = env!("CARGO_PKG_VERSION");
    let mut lines = Vec::new();

    if compact {
        lines.push(BannerLine::styled(
            format!("VANTA {version}"),
            TextStyle::Title,
        ));
    } else {
        for row in LOGO {
            lines.push(BannerLine::styled(row, TextStyle::Title));
        }
    }

    let subtitle = if compact {
        "[ a modal terminal editor ]".to_string()
    } else {
        format!("[ Vanta {version} • a modal terminal editor ]")
    };
    lines.push(BannerLine::styled(subtitle, TextStyle::Muted));
    lines.push(BannerLine::plain(""));

    for (icon, label, key) in [
        (ICON_OPEN, "Open file", ":e"),
        (ICON_INSERT, "Insert text", "i"),
        (ICON_SAVE, "Save as", ":w"),
        (ICON_FIND, "Find", "/"),
        (ICON_UNDO, "Undo", "u"),
        (ICON_REDO, "Redo", "Ctrl-r"),
        (ICON_QUIT, "Quit", ":q"),
    ] {
        lines.push(menu_line(icon, label, key, menu_width));
    }

    lines.push(BannerLine::plain(""));
    let hint = format!("{ICON_BOLT}  Vanta {version} ready");
    let icon_width = display_width(ICON_BOLT);
    lines.push(BannerLine {
        spans: vec![
            (0, icon_width, TextStyle::Warm),
            (icon_width + 2, display_width(&hint), TextStyle::Muted),
        ],
        text: hint,
    });
    lines
}

fn menu_line(icon: &str, label: &str, key: &str, width: usize) -> BannerLine {
    let left = format!("{icon}  {label}");
    let icon_width = display_width(icon);
    let key_width = display_width(key);
    let gap = width.saturating_sub(display_width(&left) + key_width);
    let text = format!("{left}{}{key}", " ".repeat(gap));
    let key_start = width.saturating_sub(key_width);

    BannerLine {
        spans: vec![
            (0, icon_width, TextStyle::Icon),
            (key_start, key_start + key_width, TextStyle::Key),
        ],
        text,
    }
}

fn display_width(text: &str) -> usize {
    line_display_width(text, TABSTOP).get()
}

fn pad_cells(text: &str, width: usize) -> String {
    let cells = display_width(text);
    if cells >= width {
        text.to_string()
    } else {
        format!("{text}{}", " ".repeat(width - cells))
    }
}

/// Appends one clamped, non-empty styled span.
fn push_style(spans: &mut Vec<StyledSpan>, row: usize, start: usize, end: usize, style: TextStyle) {
    if end <= start {
        return;
    }
    let (Ok(row), Ok(start_col), Ok(end_col)) =
        (u16::try_from(row), u16::try_from(start), u16::try_from(end))
    else {
        return;
    };
    spans.push(StyledSpan {
        row,
        start_col,
        end_col,
        style,
    });
}

/// Appends one clamped, non-empty selection span.
fn push_span(spans: &mut Vec<SelectionSpan>, row: u16, start: usize, end: usize, width: usize) {
    let start = start.min(width);
    let end = end.min(width);
    if end <= start {
        return;
    }
    let (Ok(start_col), Ok(end_col)) = (u16::try_from(start), u16::try_from(end)) else {
        return;
    };
    spans.push(SelectionSpan {
        row,
        start_col,
        end_col,
    });
}

/// Renders one buffer line from display cell `left`, expanding tabs and
/// replacing control characters with `?`.
///
/// `left` is a grapheme boundary chosen by [`crate::view`]. A grapheme that
/// would be clipped by either edge is omitted whole, never split; the same
/// rule applies to tabs and double-width glyphs. Buffer text is never written
/// to the terminal raw, so this remains the single sanitization point for
/// buffer content in the minimal editor.
fn render_line(text: &str, left: usize, width: usize) -> String {
    let right = left.saturating_add(width);
    let mut output = String::new();
    let mut cells = 0;

    for grapheme in text.graphemes(true) {
        let advance = grapheme_width_at(grapheme, cells, TABSTOP);
        let end = cells.saturating_add(advance);
        if cells < left {
            cells = end;
            continue;
        }
        if end > right {
            break;
        }

        if grapheme == "\t" {
            output.extend(std::iter::repeat_n(' ', advance));
        } else if grapheme.chars().next().is_some_and(char::is_control) {
            output.push('?');
        } else {
            output.push_str(grapheme);
        }
        cells = end;
    }

    output
}
