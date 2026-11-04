//! The minimal editor state, input handling and loop.
//!
//! [`Editor`] owns the mode state and command line and drives one
//! `Document`. All text changes go through `Editor::apply_edit`, which
//! delegates the buffer/cursor update to `Document::replace`; input handlers
//! never mutate the buffer directly. Rendering is a read-only child module.
//!
//! Modes are deliberately small: Normal moves, deletes and yanks by range,
//! puts the unnamed register, opens lines, enters Insert, Visual or Command,
//! and accepts `u` and `Ctrl-r`; Visual stores one anchor and resolves its
//! range through the same document verbs; Insert applies characters, Enter and
//! Backspace; Command accepts `w`, `q`, `q!`, `wq` and `x`. This is a small
//! Vim-shaped editing loop, not the full modal grammar from `PROJECT_SPEC`
//! §3A.

use std::collections::BTreeMap;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Instant;

use vanta_fs::{SaveOutcome, TextEncoding};
use vanta_text::{
    ByteCol, BytePos, LineCol, LineEnding, LineNumber, TextError, next_grapheme_boundary,
    prev_grapheme_boundary,
};

use crate::document::{Document, DocumentSaveError, Motion, RecoveryError, TextRange, VisualKind};
use crate::terminal::{InputEvent, Size, Terminal};
use crate::view;

mod grammar;
mod registers;
mod render;

use grammar::{
    Action, Command, InsertEntry, Operator, Outcome, Parser, SelectionMode, Target, WindowAction,
};
use registers::{RegisterKind, Registers};

/// A failed editor operation that ends the session.
#[derive(Debug, thiserror::Error)]
pub enum EditorError {
    /// The file could not be read.
    #[error("cannot read {}: {source}", path.display())]
    Read {
        /// The file that could not be read.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// The terminal failed while reading or drawing.
    #[error("terminal error: {0}")]
    Terminal(#[from] io::Error),

    /// An edit was rejected by the text layer. This indicates an editor bug,
    /// because every edit is derived from a valid cursor position.
    #[error("edit failed: {0}")]
    Edit(#[from] TextError),

    /// A recovery request found no retained swap.
    #[error("E305: No swap file found for {0}")]
    NoSwap(String),

    /// A recovery request failed while loading a retained swap.
    #[error("E325: {0}")]
    Recovery(String),

    /// A window referred to a buffer that is no longer open.
    #[error("E: buffer {0} is not open")]
    BufferMissing(usize),
}

/// The anchor and granularity of an active Visual selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Visual {
    anchor: LineCol,
    kind: VisualKind,
}

/// The last executed literal search and its current match.
///
/// `matches` is dropped when an edit changes the buffer but the query is kept,
/// so `n`/`N` can recompute against the new text instead of moving to a stale
/// byte offset.
#[derive(Debug, Default)]
struct SearchState {
    query: Option<String>,
    matches: Vec<Range<BytePos>>,
    current: Option<Range<BytePos>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Normal,
    Visual(Visual),
    Select(Visual),
    Insert,
    Replace,
    Command,
    Search,
}

/// A change remembered for `.`. The command carries its count and register;
/// the optional text is the Insert session's typed replacement.
#[derive(Clone, Debug)]
struct Repeat {
    command: Command,
    inserted: Option<String>,
}

#[derive(Clone, Copy, Debug)]
struct BlockInsert {
    line: LineNumber,
    lines: usize,
    column: usize,
}

/// A tab page's layout tree. Leaves index windows by stable ID, so closing a
/// middle window never invalidates the remaining leaves.
#[derive(Clone, Debug)]
enum Layout {
    Leaf(usize),
    Split {
        vertical: bool,
        first: Box<Layout>,
        second: Box<Layout>,
    },
}

/// One viewport onto a buffer. The cursor and scroll offsets belong to the
/// window, not the document, so two splits of one buffer stay independent.
#[derive(Clone, Debug)]
struct Window {
    id: usize,
    buffer_id: usize,
    cursor: LineCol,
    top_line: usize,
    left_col: usize,
}

/// One tab page and its focused window.
#[derive(Debug)]
struct TabPage {
    windows: Vec<Window>,
    focus: usize,
    layout: Layout,
}

/// A document that is open but not currently active.
#[derive(Debug)]
struct BufferEntry {
    id: usize,
    document: Document,
}

/// A rectangle in terminal cells.
#[derive(Clone, Copy, Debug)]
struct Rect {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

/// One document, one window and the modal editing state.
#[derive(Debug)]
pub struct Editor {
    document: Document,
    mode: Mode,
    command: Option<String>,
    message: Option<String>,
    should_quit: bool,
    inactive: Vec<BufferEntry>,
    active_buffer: usize,
    next_buffer_id: usize,
    tabs: Vec<TabPage>,
    active_tab: usize,
    next_window_id: usize,
    normal: Parser,
    registers: Registers,
    last_find: Option<(char, bool, bool)>,
    last_repeat: Option<Repeat>,
    insert_repeat: Option<Repeat>,
    insert_text: String,
    block_insert: Option<BlockInsert>,
    last_visual: Option<Visual>,
    recording: Option<(char, Vec<InputEvent>)>,
    macros: BTreeMap<char, Vec<InputEvent>>,
    last_macro: Option<char>,
    macro_depth: usize,
    search: SearchState,
    search_input: String,
    /// First buffer display cell shown in the text rows.
    left_col: usize,
}

impl Editor {
    /// Opens `path` into a document, or starts an empty buffer when it does
    /// not exist.
    ///
    /// Files are decoded with BOM-first encoding detection. UTF-16/32 with a
    /// BOM and latin-1 are converted to internal UTF-8; malformed data falls
    /// back to a lossless latin-1 read with a warning.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, EditorError> {
        let document = Document::open(path)?;
        let warning = document_warning(&document);
        let mut editor = Self::with_document(document);
        editor.message = warning;
        Ok(editor)
    }

    /// Starts an empty untitled document for the no-file startup path.
    ///
    /// The buffer is editable, but saving requires an explicit `:w <path>`
    /// because there is no current filename yet.
    pub fn untitled() -> Self {
        Self::with_document(Document::untitled())
    }

    /// Recovers the newest retained untitled swap for `vanta -r`.
    ///
    /// The recovered buffer is marked modified and has no filename, so it
    /// is only written by an explicit `:w <path>`.
    pub fn recover_untitled() -> Result<Self, EditorError> {
        match Document::recover_untitled() {
            Ok(Some(document)) => Ok(Self::recovered(
                document,
                "recovered untitled swap; save with :w <path>",
            )),
            Ok(None) => Err(EditorError::NoSwap("[No Name]".to_string())),
            Err(error) => Err(EditorError::Recovery(error.to_string())),
        }
    }

    /// Recovers the swap of `path` for `vanta -r <file>`.
    ///
    /// Recovery loads the swap text without opening the startup prompt and
    /// never writes the original file.
    pub fn recover(path: impl Into<PathBuf>) -> Result<Self, EditorError> {
        let path = path.into();
        match Document::recover_named(path.clone()) {
            Ok(Some(document)) => Ok(Self::recovered(document, "recovered swap file")),
            Ok(None) => Err(EditorError::NoSwap(path.display().to_string())),
            Err(error) => Err(EditorError::Recovery(error.to_string())),
        }
    }

    /// Wraps a recovered document with its status message.
    fn recovered(document: Document, message: &str) -> Self {
        let mut editor = Self::with_document(document);
        editor.message = Some(message.to_string());
        editor
    }

    fn with_document(document: Document) -> Self {
        let current_file = document.display_name().to_string();
        let cursor = document.cursor();
        let window = Window {
            id: 0,
            buffer_id: 0,
            cursor,
            top_line: 0,
            left_col: 0,
        };
        let tab = TabPage {
            windows: vec![window],
            focus: 0,
            layout: Layout::Leaf(0),
        };
        let mut editor = Self {
            document,
            mode: Mode::Normal,
            command: None,
            message: None,
            should_quit: false,
            inactive: Vec::new(),
            active_buffer: 0,
            next_buffer_id: 1,
            tabs: vec![tab],
            active_tab: 0,
            next_window_id: 1,
            normal: Parser::default(),
            registers: Registers::default(),
            last_find: None,
            last_repeat: None,
            insert_repeat: None,
            insert_text: String::new(),
            block_insert: None,
            last_visual: None,
            recording: None,
            macros: BTreeMap::new(),
            last_macro: None,
            macro_depth: 0,
            search: SearchState::default(),
            search_input: String::new(),
            left_col: 0,
        };
        editor.registers.set_current_file(Some(current_file), None);
        editor
    }

    fn focused_window(&self) -> &Window {
        let tab = &self.tabs[self.active_tab];
        tab.windows
            .iter()
            .find(|window| window.id == tab.focus)
            .expect("the focused window belongs to its tab")
    }

    fn focused_window_mut(&mut self) -> &mut Window {
        let tab = &mut self.tabs[self.active_tab];
        let focus = tab.focus;
        tab.windows
            .iter_mut()
            .find(|window| window.id == focus)
            .expect("the focused window belongs to its tab")
    }

    fn window_by_id(&self, tab: usize, id: usize) -> Option<&Window> {
        self.tabs
            .get(tab)?
            .windows
            .iter()
            .find(|window| window.id == id)
    }

    fn document_by_id(&self, id: usize) -> Option<&Document> {
        if id == self.active_buffer {
            Some(&self.document)
        } else {
            self.inactive
                .iter()
                .find(|entry| entry.id == id)
                .map(|entry| &entry.document)
        }
    }

    fn buffer_ids(&self) -> Vec<usize> {
        let mut ids: Vec<usize> = std::iter::once(self.active_buffer)
            .chain(self.inactive.iter().map(|entry| entry.id))
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Stores the active document cursor in the focused viewport.
    fn save_focus_cursor(&mut self) {
        let cursor = self.document.cursor();
        self.focused_window_mut().cursor = cursor;
    }

    /// Loads the focused viewport cursor into the active document.
    fn load_focus_cursor(&mut self) -> Result<(), EditorError> {
        let cursor = self.focused_window().cursor;
        self.document.sync_cursor_to(cursor)?;
        Ok(())
    }

    /// Makes `buffer_id` active and places the document cursor at `cursor`.
    fn activate_buffer(&mut self, buffer_id: usize, cursor: LineCol) -> Result<(), EditorError> {
        if buffer_id != self.active_buffer {
            let Some(index) = self.inactive.iter().position(|entry| entry.id == buffer_id) else {
                return Err(EditorError::BufferMissing(buffer_id));
            };
            let mut target = self.inactive.swap_remove(index);
            let old_id = self.active_buffer;
            std::mem::swap(&mut self.document, &mut target.document);
            self.inactive.push(BufferEntry {
                id: old_id,
                document: target.document,
            });
            self.active_buffer = buffer_id;
        }
        self.document.move_cursor_to(cursor)?;
        self.registers
            .set_current_file(Some(self.document.display_name().to_string()), None);
        Ok(())
    }

    /// Gives the focused window a new buffer without destroying the old one.
    fn show_buffer_in_focus(&mut self, buffer_id: usize) -> Result<(), EditorError> {
        let target_cursor = self
            .document_by_id(buffer_id)
            .ok_or(EditorError::BufferMissing(buffer_id))?
            .cursor();
        self.save_focus_cursor();
        {
            let window = self.focused_window_mut();
            window.buffer_id = buffer_id;
            window.cursor = target_cursor;
            window.top_line = 0;
            window.left_col = 0;
        }
        self.activate_buffer(buffer_id, target_cursor)
    }

    /// Moves focus to a window in another tab, activating its buffer.
    fn focus_window(&mut self, tab_index: usize, window_id: usize) -> Result<(), EditorError> {
        if tab_index >= self.tabs.len() {
            return Ok(());
        }
        self.save_focus_cursor();
        if !self.tabs[tab_index]
            .windows
            .iter()
            .any(|window| window.id == window_id)
        {
            return Ok(());
        }
        self.tabs[tab_index].focus = window_id;
        self.active_tab = tab_index;
        let (buffer_id, cursor) = {
            let window = self
                .window_by_id(tab_index, window_id)
                .expect("the window was checked above");
            (window.buffer_id, window.cursor)
        };
        self.activate_buffer(buffer_id, cursor)
    }

    fn switch_tab(&mut self, index: usize) -> Result<(), EditorError> {
        if self.tabs.is_empty() || index >= self.tabs.len() || index == self.active_tab {
            return Ok(());
        }
        self.save_focus_cursor();
        self.active_tab = index;
        let (buffer_id, cursor) = {
            let window = self.focused_window();
            (window.buffer_id, window.cursor)
        };
        self.activate_buffer(buffer_id, cursor)
    }

    /// Replaces the active document with `document`, keeping the old one open.
    fn open_buffer(&mut self, document: Document) -> usize {
        self.save_focus_cursor();
        let id = self.next_buffer_id;
        self.next_buffer_id += 1;
        let old = std::mem::replace(&mut self.document, document);
        let old_id = self.active_buffer;
        self.inactive.push(BufferEntry {
            id: old_id,
            document: old,
        });
        self.active_buffer = id;
        let cursor = self.document.cursor();
        {
            let window = self.focused_window_mut();
            window.buffer_id = id;
            window.cursor = cursor;
            window.top_line = 0;
            window.left_col = 0;
        }
        self.registers
            .set_current_file(Some(self.document.display_name().to_string()), None);
        id
    }

    fn split_focus(&mut self, vertical: bool) {
        let source = self.focused_window().clone();
        let id = self.next_window_id;
        self.next_window_id += 1;
        let window = Window { id, ..source };
        let focus = self.focused_window().id;
        let tab = &mut self.tabs[self.active_tab];
        tab.windows.push(window);
        replace_leaf(&mut tab.layout, focus, id, vertical);
        tab.focus = id;
    }

    fn move_focus(&mut self, direction: char) -> Result<(), EditorError> {
        let tab = &self.tabs[self.active_tab];
        if tab.windows.len() < 2 {
            return Ok(());
        }
        let mut rects = Vec::new();
        layout_rects(
            &tab.layout,
            Rect {
                x: 0,
                y: 0,
                width: usize::MAX,
                height: usize::MAX,
            },
            &mut rects,
        );
        let focus = tab.focus;
        let current = rects
            .iter()
            .find(|(id, _)| *id == focus)
            .map(|(_, rect)| *rect);
        let Some(current) = current else {
            return Ok(());
        };
        if direction == 'w' {
            let position = tab
                .windows
                .iter()
                .position(|window| window.id == focus)
                .unwrap_or(0);
            let next = tab.windows[(position + 1) % tab.windows.len()].id;
            return self.focus_window(self.active_tab, next);
        }
        let candidate = rects
            .iter()
            .filter(|(id, _)| *id != focus)
            .filter(|(_, rect)| match direction {
                'h' => rect.x + rect.width <= current.x,
                'l' => rect.x >= current.x + current.width,
                'k' => rect.y + rect.height <= current.y,
                'j' => rect.y >= current.y + current.height,
                _ => false,
            })
            .min_by_key(|(_, rect)| match direction {
                'h' => current.x.saturating_sub(rect.x + rect.width),
                'l' => rect.x.saturating_sub(current.x + current.width),
                'k' => current.y.saturating_sub(rect.y + rect.height),
                _ => rect.y.saturating_sub(current.y + current.height),
            })
            .map(|(id, _)| *id);
        if let Some(id) = candidate {
            self.focus_window(self.active_tab, id)?;
        }
        Ok(())
    }

    fn close_focused_window(&mut self, force: bool) -> Result<(), EditorError> {
        if !force && self.document.is_modified() {
            self.message = Some("E37: No write since last change (add ! to override)".to_string());
            return Ok(());
        }
        self.save_focus_cursor();
        let tab = &mut self.tabs[self.active_tab];
        if tab.windows.len() <= 1 {
            if self.tabs.len() > 1 {
                return self.close_tab();
            }
            self.should_quit = true;
            return Ok(());
        }
        let closing = tab.focus;
        tab.windows.retain(|window| window.id != closing);
        tab.layout = close_layout(&tab.layout, closing).unwrap_or(Layout::Leaf(tab.focus));
        tab.focus = tab.windows[0].id;
        let (buffer_id, cursor) = {
            let window = &tab.windows[0];
            (window.buffer_id, window.cursor)
        };
        self.activate_buffer(buffer_id, cursor)
    }

    fn close_tab(&mut self) -> Result<(), EditorError> {
        if self.tabs.len() <= 1 {
            self.message = Some("E784: Cannot close last tab page".to_string());
            return Ok(());
        }
        self.save_focus_cursor();
        self.tabs.remove(self.active_tab);
        self.active_tab = self.active_tab.min(self.tabs.len() - 1);
        let (buffer_id, cursor) = {
            let window = self.focused_window();
            (window.buffer_id, window.cursor)
        };
        self.activate_buffer(buffer_id, cursor)
    }

    fn tab_step(&mut self, count: usize, forward: bool) -> Result<(), EditorError> {
        let tab_count = self.tabs.len();
        if tab_count <= 1 {
            return Ok(());
        }
        let offset = count.max(1) % tab_count;
        let index = if forward {
            (self.active_tab + offset) % tab_count
        } else {
            (self.active_tab + tab_count - offset) % tab_count
        };
        self.switch_tab(index)
    }

    fn window_action(&mut self, action: WindowAction) -> Result<(), EditorError> {
        match action {
            WindowAction::SplitBelow => {
                self.split_focus(false);
                Ok(())
            }
            WindowAction::SplitRight => {
                self.split_focus(true);
                Ok(())
            }
            WindowAction::Next => self.move_focus('w'),
            WindowAction::Left => self.move_focus('h'),
            WindowAction::Right => self.move_focus('l'),
            WindowAction::Up => self.move_focus('k'),
            WindowAction::Down => self.move_focus('j'),
            WindowAction::Previous => {
                let tab = &self.tabs[self.active_tab];
                if tab.windows.len() < 2 {
                    return Ok(());
                }
                let position = tab
                    .windows
                    .iter()
                    .position(|window| window.id == tab.focus)
                    .unwrap_or(0);
                let previous =
                    tab.windows[(position + tab.windows.len() - 1) % tab.windows.len()].id;
                let tab_index = self.active_tab;
                self.focus_window(tab_index, previous)
            }
            WindowAction::Close => self.close_focused_window(false),
            WindowAction::Only => {
                self.save_focus_cursor();
                let focus = self.tabs[self.active_tab].focus;
                let tab = &mut self.tabs[self.active_tab];
                tab.windows.retain(|window| window.id == focus);
                tab.layout = Layout::Leaf(focus);
                Ok(())
            }
            WindowAction::Rotate => self.move_focus('w'),
            WindowAction::Equalize
            | WindowAction::Taller
            | WindowAction::Shorter
            | WindowAction::Wider
            | WindowAction::Narrower => Ok(()),
        }
    }

    fn parse_buffer_id(&self, argument: &str) -> Option<usize> {
        argument.trim().parse::<usize>().ok()
    }

    fn buffer_id_for_path(&self, path: &Path) -> Option<usize> {
        if self.document.path() == Some(path) {
            Some(self.active_buffer)
        } else {
            self.inactive
                .iter()
                .find(|entry| entry.document.path() == Some(path))
                .map(|entry| entry.id)
        }
    }

    fn buffer_command(&mut self, argument: &str) {
        let target = if let Some(id) = self.parse_buffer_id(argument) {
            Some(id)
        } else if argument.is_empty() {
            None
        } else {
            self.buffer_id_for_path(&command_path(argument))
        };
        match target {
            Some(id) if self.document_by_id(id).is_some() => {
                if let Err(error) = self.show_buffer_in_focus(id) {
                    self.message = Some(error.to_string());
                }
            }
            _ => self.message = Some(format!("E94: No matching buffer for {argument}")),
        }
    }

    fn cycle_buffer(&mut self, direction: isize) {
        let ids = self.buffer_ids();
        if ids.len() <= 1 {
            self.message = Some("E85: There is only one file to edit".to_string());
            return;
        }
        let position = ids
            .iter()
            .position(|id| *id == self.active_buffer)
            .unwrap_or(0);
        let next = (position as isize + direction).rem_euclid(ids.len() as isize) as usize;
        if let Err(error) = self.show_buffer_in_focus(ids[next]) {
            self.message = Some(error.to_string());
        }
    }

    fn list_buffers(&mut self) {
        let ids = self.buffer_ids();
        let mut lines = Vec::new();
        for id in ids {
            let Some(document) = self.document_by_id(id) else {
                continue;
            };
            let marker = if id == self.active_buffer { "%" } else { " " };
            let flags = if document.is_modified() { "a+" } else { "  " };
            lines.push(format!(
                "{id:>2} {marker}{flags} {}",
                document.display_name()
            ));
        }
        self.message = Some(lines.join(" | "));
    }

    fn tab_new_command(&mut self, argument: &str) {
        let document = if argument.is_empty() {
            Ok(Document::untitled())
        } else {
            Document::open(command_path(argument))
        };
        match document {
            Ok(document) => {
                if let Err(error) = self.new_tab(document) {
                    self.message = Some(error.to_string());
                }
            }
            Err(error) => self.message = Some(error.to_string()),
        }
    }

    fn tab_command(&mut self, argument: &str, direction: isize) {
        if let Ok(number) = argument.trim().parse::<usize>() {
            if number == 0 || number > self.tabs.len() {
                self.message = Some(format!("E16: Invalid range: {number}"));
            } else if let Err(error) = self.switch_tab(number - 1) {
                self.message = Some(error.to_string());
            }
        } else if let Err(error) = self.tab_step(1, direction > 0) {
            self.message = Some(error.to_string());
        }
    }

    fn delete_buffer(&mut self, id: usize, force: bool) -> Result<(), EditorError> {
        let Some(document) = self.document_by_id(id) else {
            self.message = Some(format!("E94: No matching buffer for {id}"));
            return Ok(());
        };
        if document.is_modified() && !force {
            self.message =
                Some("E89: No write since last change for buffer (add ! to override)".to_string());
            return Ok(());
        }
        self.save_focus_cursor();
        for tab in &mut self.tabs {
            let closing: Vec<usize> = tab
                .windows
                .iter()
                .filter(|window| window.buffer_id == id)
                .map(|window| window.id)
                .collect();
            tab.windows.retain(|window| window.buffer_id != id);
            for window in closing {
                if let Some(layout) = close_layout(&tab.layout, window) {
                    tab.layout = layout;
                }
            }
            if !tab.windows.is_empty() && !tab.windows.iter().any(|window| window.id == tab.focus) {
                tab.focus = tab.windows[0].id;
            }
        }
        self.tabs.retain(|tab| !tab.windows.is_empty());
        if self.tabs.is_empty() {
            if id == self.active_buffer {
                let mut old = std::mem::replace(&mut self.document, Document::untitled());
                old.close_session();
                self.active_buffer = self.next_buffer_id;
                self.next_buffer_id += 1;
            } else if let Some(index) = self.inactive.iter().position(|entry| entry.id == id) {
                self.inactive.swap_remove(index).document.close_session();
            }
            let cursor = self.document.cursor();
            let window_id = self.next_window_id;
            self.next_window_id += 1;
            self.tabs.push(TabPage {
                windows: vec![Window {
                    id: window_id,
                    buffer_id: self.active_buffer,
                    cursor,
                    top_line: 0,
                    left_col: 0,
                }],
                focus: window_id,
                layout: Layout::Leaf(window_id),
            });
            self.active_tab = 0;
            return Ok(());
        }
        self.active_tab = self.active_tab.min(self.tabs.len() - 1);
        let (target_tab, target_window, buffer_id, cursor) = {
            let tab = &self.tabs[self.active_tab];
            let window = tab
                .windows
                .iter()
                .find(|window| window.id == tab.focus)
                .unwrap_or(&tab.windows[0]);
            (self.active_tab, tab.focus, window.buffer_id, window.cursor)
        };
        self.focus_window(target_tab, target_window)?;
        if id == self.active_buffer {
            if let Some(index) = self.inactive.iter().position(|entry| entry.id == buffer_id) {
                let replacement = self.inactive.swap_remove(index);
                let mut old = std::mem::replace(&mut self.document, replacement.document);
                old.close_session();
                self.active_buffer = buffer_id;
            }
        } else if let Some(index) = self.inactive.iter().position(|entry| entry.id == id) {
            self.inactive.swap_remove(index).document.close_session();
        }
        self.document.move_cursor_to(cursor)?;
        Ok(())
    }

    fn new_tab(&mut self, document: Document) -> Result<(), EditorError> {
        self.save_focus_cursor();
        let id = self.next_buffer_id;
        self.next_buffer_id += 1;
        let old = std::mem::replace(&mut self.document, document);
        let old_id = self.active_buffer;
        self.inactive.push(BufferEntry {
            id: old_id,
            document: old,
        });
        self.active_buffer = id;
        let cursor = self.document.cursor();
        let window_id = self.next_window_id;
        self.next_window_id += 1;
        let tab = TabPage {
            windows: vec![Window {
                id: window_id,
                buffer_id: id,
                cursor,
                top_line: 0,
                left_col: 0,
            }],
            focus: window_id,
            layout: Layout::Leaf(window_id),
        };
        self.active_tab += 1;
        self.tabs.insert(self.active_tab, tab);
        self.registers
            .set_current_file(Some(self.document.display_name().to_string()), None);
        Ok(())
    }

    /// Runs the draw/read/handle loop until the editor quits.
    ///
    /// The loop blocks in [`Terminal::read_event`], so it consumes no CPU while
    /// idle.
    pub fn run(&mut self, terminal: &mut dyn Terminal) -> Result<(), EditorError> {
        while !self.should_quit {
            let size = terminal.size()?;
            self.ensure_cursor_visible(size);
            let frame = self.frame(size);
            terminal.draw(&frame)?;
            let event = match self.document.swap_flush_deadline() {
                Some(deadline) => {
                    let timeout = deadline.saturating_duration_since(Instant::now());
                    match terminal.poll_event(timeout)? {
                        Some(event) => Some(event),
                        None => {
                            if let Err(error) = self.document.flush_idle_swap(Instant::now()) {
                                self.message = Some(format!("swap: {error}"));
                            }
                            None
                        }
                    }
                }
                None => Some(terminal.read_event()?),
            };
            if let Some(event) = event {
                self.handle(event)?;
            }
        }
        // A normal quit owns the session and removes its swap. Any error path
        // returns before this point, leaving the swap for startup recovery.
        self.close_all_sessions();
        Ok(())
    }

    fn close_all_sessions(&mut self) {
        self.document.close_session();
        for entry in &mut self.inactive {
            entry.document.close_session();
        }
    }

    /// Recomputes the horizontal viewport for the current cursor.
    ///
    /// This runs before every frame, so a motion, search, undo or resize can
    /// never leave the cursor outside the visible cells. The previous offset
    /// is kept when the cursor is still inside it, which avoids scrolling the
    /// line back on every leftward step.
    fn ensure_cursor_visible(&mut self, size: Size) {
        if self.tabs[self.active_tab].windows.len() > 1 {
            let height = usize::from(size.height.max(1));
            let area = Rect {
                x: 0,
                y: 0,
                width: usize::from(size.width.max(1)),
                height: height.saturating_sub(1),
            };
            let tab = &self.tabs[self.active_tab];
            let mut rects = Vec::new();
            layout_rects(&tab.layout, area, &mut rects);
            let focus = tab.focus;
            let cursor = self.document.cursor();
            if let Some((_, rect)) = rects.iter().find(|(id, _)| *id == focus) {
                let text_rows = rect.height.saturating_sub(1).max(1);
                let width = rect.width.max(1);
                let line_text = self.document.line_text();
                let window = self.focused_window_mut();
                window.left_col =
                    view::keep_cursor_visible(&line_text, cursor.col.get(), width, window.left_col);
                if cursor.line.get() < window.top_line {
                    window.top_line = cursor.line.get();
                } else if cursor.line.get() >= window.top_line + text_rows {
                    window.top_line = cursor.line.get() + 1 - text_rows;
                }
                self.left_col = window.left_col;
                return;
            }
        }
        let line = self.document.line_text();
        self.left_col = view::keep_cursor_visible(
            &line,
            self.document.cursor().col.get(),
            usize::from(size.width.max(1)),
            self.left_col,
        );
        self.focused_window_mut().left_col = self.left_col;
    }

    fn handle(&mut self, event: InputEvent) -> Result<(), EditorError> {
        if self.document.recovery().is_some() {
            self.handle_recovery(event);
            return Ok(());
        }
        self.load_focus_cursor()?;
        let result = match self.mode {
            Mode::Normal => self.handle_normal(event),
            Mode::Visual(visual) => self.handle_visual(event, visual),
            Mode::Select(visual) => self.handle_select(event, visual),
            Mode::Insert => self.handle_insert(event),
            Mode::Replace => self.handle_replace(event),
            Mode::Command => {
                self.handle_command(event);
                Ok(())
            }
            Mode::Search => self.handle_search(event),
        };
        if result.is_ok() {
            self.save_focus_cursor();
        }
        result
    }

    /// Resolves the one startup swap decision before normal input resumes.
    ///
    /// `R` loads the recovered text, `O` opens the original while preserving
    /// the swap, `E` edits the original and replaces the swap, `D` deletes the
    /// swap, and `Q`/`A`/Esc leave the editor without touching it.
    fn handle_recovery(&mut self, event: InputEvent) {
        match event {
            InputEvent::Char('r' | 'R') => match self.document.recover_swap() {
                Ok(()) => self.finish_recovery("recovered swap file"),
                Err(error) => self.recovery_error(error),
            },
            InputEvent::Char('o' | 'O') => match self.document.open_original(false) {
                Ok(()) => self.finish_recovery("opened original; swap file retained"),
                Err(error) => self.recovery_error(error),
            },
            InputEvent::Char('e' | 'E') => match self.document.open_original(true) {
                Ok(()) => self.finish_recovery("editing original; swap file replaced"),
                Err(error) => self.recovery_error(error),
            },
            InputEvent::Char('d' | 'D') => match self.document.delete_swap() {
                Ok(()) => self.finish_recovery("swap file deleted"),
                Err(error) => self.recovery_error(error),
            },
            InputEvent::Char('q' | 'Q' | 'a' | 'A') | InputEvent::Esc => {
                self.should_quit = true;
            }
            _ => {}
        }
    }

    fn finish_recovery(&mut self, message: &str) {
        self.mode = Mode::Normal;
        self.command = None;
        self.normal.reset();
        self.search = SearchState::default();
        self.search_input.clear();
        self.left_col = 0;
        self.message = Some(message.to_string());
    }

    fn recovery_error(&mut self, error: RecoveryError) {
        self.message = Some(format!("E325: {error}"));
    }

    fn handle_normal(&mut self, event: InputEvent) -> Result<(), EditorError> {
        if event == InputEvent::Esc {
            self.normal.reset();
            self.message = None;
            return Ok(());
        }
        if event == InputEvent::Char('q') && self.recording.is_some() {
            self.stop_recording();
            return Ok(());
        }
        if let Some((_, keys)) = &mut self.recording
            && self.macro_depth == 0
        {
            keys.push(event);
        }
        match self.normal.feed(event) {
            Outcome::Pending => Ok(()),
            Outcome::Invalid => {
                self.normal.reset();
                self.message = Some("E: incomplete or invalid command".to_string());
                Ok(())
            }
            Outcome::Complete(command) => self.execute_normal(command),
        }
    }

    fn execute_normal(&mut self, command: Command) -> Result<(), EditorError> {
        self.message = None;
        match command {
            Command::Move { count, motion } => self.execute_motion(count, motion),
            Command::Operator {
                count,
                operator,
                register,
                target,
            } => self.execute_operator(
                count,
                operator,
                register,
                target,
                &Command::Operator {
                    count,
                    operator,
                    register,
                    target,
                },
            ),
            Command::Action {
                count,
                register,
                action,
            } => self.execute_action(
                count,
                register,
                action.clone(),
                &Command::Action {
                    count,
                    register,
                    action,
                },
            ),
        }
    }

    /// Executes a `:normal` argument through the same incremental parser as
    /// live keys, macros and dot-repeat.
    fn execute_normal_keys(&mut self, keys: &str) -> Result<(), EditorError> {
        for character in keys.chars() {
            self.handle(InputEvent::Char(character))?;
        }
        if matches!(self.mode, Mode::Insert | Mode::Replace) {
            self.finish_insert();
        }
        if self.normal.is_pending() {
            self.normal.reset();
            self.message = Some("E: incomplete :normal command".to_string());
        }
        Ok(())
    }

    fn execute_motion(&mut self, count: usize, motion: Motion) -> Result<(), EditorError> {
        if matches!(motion, Motion::SearchNext) {
            return self.repeat_search(true);
        }
        if matches!(motion, Motion::SearchPrevious) {
            return self.repeat_search(false);
        }
        if let Motion::WordUnderCursor { forward } = motion {
            let line = self.document.line_text();
            let cursor = self.document.cursor().col.get();
            let start = line[..cursor.min(line.len())]
                .char_indices()
                .rev()
                .find(|(_, character)| character.is_alphanumeric() || *character == '_')
                .map_or(cursor, |(offset, _)| offset);
            let end = line[start..]
                .char_indices()
                .find(|(_, character)| !(character.is_alphanumeric() || *character == '_'))
                .map_or(line.len(), |(offset, _)| start + offset);
            if start < end {
                self.execute_search(line[start..end].to_string())?;
            }
            if !forward {
                self.repeat_search(false)?;
            }
            return Ok(());
        }
        let resolved = match motion {
            Motion::RepeatFind { reverse } => {
                let Some((character, forward, until)) = self.last_find else {
                    self.message = Some("E34: No previous command".to_string());
                    return Ok(());
                };
                Motion::Find {
                    character,
                    forward: if reverse { !forward } else { forward },
                    until,
                }
            }
            motion => motion,
        };
        if matches!(resolved, Motion::Down | Motion::Up) {
            let delta = if resolved == Motion::Down {
                count.max(1) as isize
            } else {
                -(count.max(1) as isize)
            };
            self.document.move_vertical(delta);
            return Ok(());
        }
        if resolved == Motion::Right {
            for _ in 0..count.max(1) {
                self.document.move_right_in_line();
            }
            return Ok(());
        }
        if resolved == Motion::Left {
            for _ in 0..count.max(1) {
                self.document.move_left_in_line();
            }
            return Ok(());
        }
        if resolved == (Motion::WordForward { big: false }) {
            for _ in 0..count.max(1) {
                self.document.move_word_forward()?;
            }
            return Ok(());
        }
        if resolved == (Motion::WordBack { big: false }) {
            for _ in 0..count.max(1) {
                if let TextRange::Charwise(range) = self.document.word_range(false)? {
                    self.document.move_to(range.start)?;
                }
            }
            return Ok(());
        }
        if let Motion::Find {
            character,
            forward,
            until,
        } = resolved
        {
            self.last_find = Some((character, forward, until));
        }
        if let Some(target) = self.document.motion_target(resolved, count)? {
            self.document.move_to_motion(target)?;
        }
        Ok(())
    }

    fn execute_operator(
        &mut self,
        count: usize,
        operator: Operator,
        register: Option<char>,
        target: Target,
        repeat: &Command,
    ) -> Result<(), EditorError> {
        let start = self.document.cursor();
        let range = match target {
            Target::Line => {
                let last = self.document.line_count().saturating_sub(1);
                let first = start.line.get().min(last);
                let end = (first + count.max(1)).min(last + 1);
                TextRange::Linewise(LineNumber::new(first)..LineNumber::new(end))
            }
            Target::TextObject {
                count,
                around,
                object,
            } => {
                let Some(range) = self.document.text_object(object, around, count)? else {
                    self.message = Some("E: no text object under cursor".to_string());
                    return Ok(());
                };
                range
            }
            Target::Motion { count, motion } => {
                let mut motion = motion;
                if matches!(motion, Motion::RepeatFind { .. }) {
                    let Some((character, forward, until)) = self.last_find else {
                        self.message = Some("E34: No previous command".to_string());
                        return Ok(());
                    };
                    motion = Motion::Find {
                        character,
                        forward,
                        until,
                    };
                }
                if operator == Operator::Change
                    && let Motion::WordForward { big } = motion
                    && self
                        .document
                        .line_text()
                        .chars()
                        .nth(self.document.cursor().col.get())
                        .is_some_and(|character| !character.is_whitespace())
                {
                    motion = Motion::WordEnd { big };
                }
                let Some(target) = self.document.motion_target(motion, count)? else {
                    return Ok(());
                };
                match self.document.operator_range(start, target)? {
                    Some(range) => range,
                    None => {
                        if operator == Operator::Delete && matches!(motion, Motion::Right) {
                            self.document.char_range_at_cursor()?
                        } else {
                            return Ok(());
                        }
                    }
                }
            }
        };
        self.apply_operator_range(operator, register, range, repeat)
    }

    fn apply_operator_range(
        &mut self,
        operator: Operator,
        register: Option<char>,
        range: TextRange,
        repeat: &Command,
    ) -> Result<(), EditorError> {
        let linewise = range.is_linewise();
        match operator {
            Operator::Delete => {
                if let Some(text) = self.document.delete(range)? {
                    let small = !linewise && !text.contains('\n');
                    self.registers
                        .delete(register, text, self.register_kind(linewise), small);
                    self.invalidate_search_matches();
                    self.last_repeat = Some(Repeat {
                        command: repeat.clone(),
                        inserted: None,
                    });
                }
            }
            Operator::Change => {
                if let Some(text) = self.document.change(range)? {
                    let small = !linewise && !text.contains('\n');
                    self.registers
                        .change(register, text, self.register_kind(linewise), small);
                    self.invalidate_search_matches();
                }
                self.start_insert_session(repeat.clone());
            }
            Operator::Yank => {
                let text = self.document.yank(&range)?;
                self.registers
                    .yank(register, text, self.register_kind(linewise));
            }
            Operator::ShiftRight | Operator::ShiftLeft => {
                let right = operator == Operator::ShiftRight;
                if self.document.shift_lines(&range, right)? {
                    self.invalidate_search_matches();
                    self.last_repeat = Some(Repeat {
                        command: repeat.clone(),
                        inserted: None,
                    });
                }
            }
            Operator::ToggleCase | Operator::Lowercase | Operator::Uppercase | Operator::Rot13 => {
                let transform = |text: &str| match operator {
                    Operator::ToggleCase => text
                        .chars()
                        .flat_map(|character| {
                            if character.is_lowercase() {
                                character.to_uppercase().collect::<Vec<_>>()
                            } else {
                                character.to_lowercase().collect::<Vec<_>>()
                            }
                        })
                        .collect(),
                    Operator::Lowercase => text.to_lowercase(),
                    Operator::Uppercase => text.to_uppercase(),
                    Operator::Rot13 => text
                        .chars()
                        .map(|character| match character {
                            'a'..='m' | 'A'..='M' => {
                                char::from_u32(character as u32 + 13).unwrap_or(character)
                            }
                            'n'..='z' | 'N'..='Z' => {
                                char::from_u32(character as u32 - 13).unwrap_or(character)
                            }
                            other => other,
                        })
                        .collect(),
                    _ => text.to_string(),
                };
                if self.document.transform_text(&range, transform)? {
                    self.invalidate_search_matches();
                    self.last_repeat = Some(Repeat {
                        command: repeat.clone(),
                        inserted: None,
                    });
                }
            }
            Operator::Reindent | Operator::Format => {
                self.message = Some("E: this operator is not available".to_string());
            }
        }
        Ok(())
    }

    fn register_kind(&self, linewise: bool) -> RegisterKind {
        if linewise {
            RegisterKind::Linewise
        } else {
            RegisterKind::Charwise
        }
    }

    fn start_insert_session(&mut self, repeat: Command) {
        self.mode = Mode::Insert;
        self.insert_repeat = Some(Repeat {
            command: repeat,
            inserted: None,
        });
        self.insert_text.clear();
        self.message = None;
    }

    fn begin_insert(&mut self, entry: InsertEntry, repeat: Command) -> Result<(), EditorError> {
        match entry {
            InsertEntry::Before => {}
            InsertEntry::After => self.document.move_past_cursor(),
            InsertEntry::LineEnd => self.document.move_to_line_end(),
            InsertEntry::LineStart | InsertEntry::FirstNonBlank => {
                self.document.move_to_first_nonblank();
            }
            InsertEntry::OpenBelow | InsertEntry::OpenAbove => {
                self.document
                    .open_line(matches!(entry, InsertEntry::OpenBelow))?;
                self.invalidate_search_matches();
                self.start_insert_session(repeat);
                return Ok(());
            }
            InsertEntry::LastInsert => self.document.move_to_first_nonblank(),
        }
        self.document.break_undo();
        self.start_insert_session(repeat);
        Ok(())
    }

    fn finish_insert(&mut self) {
        let text = std::mem::take(&mut self.insert_text);
        if let Some(block) = self.block_insert.take()
            && !text.is_empty()
            && !text.contains('\n')
            && block.lines > 1
            && let Err(error) = self.document.insert_block_rows(
                LineNumber::new(block.line.get() + 1),
                block.lines - 1,
                block.column,
                &text,
            )
        {
            self.message = Some(error.to_string());
        }
        if !text.is_empty() {
            self.registers.set_last_insert(&text);
        }
        if let Some(mut repeat) = self.insert_repeat.take() {
            repeat.inserted = Some(text);
            self.last_repeat = Some(repeat);
        }
        self.document.break_undo();
        self.mode = Mode::Normal;
        self.document.move_left_in_line();
        self.message = None;
    }

    fn execute_action(
        &mut self,
        count: usize,
        register: Option<char>,
        action: Action,
        repeat: &Command,
    ) -> Result<(), EditorError> {
        match action {
            Action::Insert(entry) => self.begin_insert(entry, repeat.clone()),
            Action::Replace => {
                self.document.break_undo();
                self.mode = Mode::Replace;
                self.insert_repeat = Some(Repeat {
                    command: repeat.clone(),
                    inserted: None,
                });
                self.insert_text.clear();
                self.message = None;
                Ok(())
            }
            Action::Visual(mode) => {
                self.start_visual(selection_kind(mode));
                Ok(())
            }
            Action::Select(mode) => {
                self.mode = Mode::Select(Visual {
                    anchor: self.document.cursor(),
                    kind: selection_kind(mode),
                });
                self.message = None;
                Ok(())
            }
            Action::CommandLine => {
                self.mode = Mode::Command;
                self.command = Some(String::new());
                self.message = None;
                Ok(())
            }
            Action::SearchForward => {
                self.start_search();
                Ok(())
            }
            Action::SearchBackward => {
                self.start_search();
                self.message = Some("? search uses / direction in this build".to_string());
                Ok(())
            }
            Action::DeleteChar { before, count } => self.delete_characters(before, count, register),
            Action::DeleteToEnd { count } => {
                let command =
                    self.operator_command(count, Operator::Delete, register, Motion::LineEnd);
                self.execute_operator(
                    count,
                    Operator::Delete,
                    register,
                    Target::Motion {
                        count,
                        motion: Motion::LineEnd,
                    },
                    &command,
                )
            }
            Action::ChangeToEnd { count } => {
                let command =
                    self.operator_command(count, Operator::Change, register, Motion::LineEnd);
                self.execute_operator(
                    count,
                    Operator::Change,
                    register,
                    Target::Motion {
                        count,
                        motion: Motion::LineEnd,
                    },
                    &command,
                )
            }
            Action::ChangeLine { count } => {
                let last = self.document.line_count().saturating_sub(1);
                let first = self.document.cursor().line.get().min(last);
                let end = (first + count.max(1)).min(last + 1);
                let range = TextRange::Linewise(LineNumber::new(first)..LineNumber::new(end));
                let command = Command::Action {
                    count,
                    register,
                    action: Action::ChangeLine { count },
                };
                self.apply_operator_range(Operator::Change, register, range, &command)
            }
            Action::YankLine { count } => {
                let last = self.document.line_count().saturating_sub(1);
                let first = self.document.cursor().line.get().min(last);
                let end = (first + count.max(1)).min(last + 1);
                let range = TextRange::Linewise(LineNumber::new(first)..LineNumber::new(end));
                let text = self.document.yank(&range)?;
                self.registers.yank(register, text, RegisterKind::Linewise);
                self.last_repeat = Some(Repeat {
                    command: repeat.clone(),
                    inserted: None,
                });
                Ok(())
            }
            Action::Put { before, count } => self.put_register(register, before, count),
            Action::Join { spaces, count } => {
                if self.document.join_lines(count.max(1), spaces)? {
                    self.invalidate_search_matches();
                    self.last_repeat = Some(Repeat {
                        command: repeat.clone(),
                        inserted: None,
                    });
                }
                Ok(())
            }
            Action::ToggleCase { count } => self.toggle_case(count),
            Action::Undo => self.undo(),
            Action::Redo => self.redo(),
            Action::UndoEarlier => {
                if self.document.undo_earlier()? {
                    self.invalidate_search_matches();
                }
                Ok(())
            }
            Action::UndoLater => {
                if self.document.undo_later()? {
                    self.invalidate_search_matches();
                }
                Ok(())
            }
            Action::RestoreLine => {
                if self.document.restore_line()? {
                    self.invalidate_search_matches();
                }
                Ok(())
            }
            Action::RepeatChange => self.repeat_change(),
            Action::ReselectVisual => {
                if let Some(visual) = self.last_visual {
                    self.mode = Mode::Visual(visual);
                } else {
                    self.message = Some("E: no previous Visual selection".to_string());
                }
                Ok(())
            }
            Action::Record(Some(name)) => {
                self.recording = Some((name, Vec::new()));
                self.message = Some(format!("recording @{name}"));
                Ok(())
            }
            Action::Record(None) => {
                self.stop_recording();
                Ok(())
            }
            Action::Replay(name) => self.play_macro(name, count.max(1)),
            Action::RepeatMacro => {
                if let Some(name) = self.last_macro {
                    self.play_macro(name, count.max(1))
                } else {
                    self.message = Some("E748: No previously used register".to_string());
                    Ok(())
                }
            }
            Action::TabNext { count, forward } => self.tab_step(count.max(1), forward),
            Action::Window(action) => self.window_action(action),
            Action::Redraw => Ok(()),
            Action::FileInfo => {
                self.message = Some(format!(
                    "{} lines, {}",
                    self.document.line_count(),
                    self.document.display_name()
                ));
                Ok(())
            }
            Action::Quit { write } => {
                if write {
                    if self.save_reporting() {
                        self.should_quit = true;
                    }
                } else {
                    self.request_quit();
                }
                Ok(())
            }
        }
    }

    fn operator_command(
        &self,
        count: usize,
        operator: Operator,
        register: Option<char>,
        motion: Motion,
    ) -> Command {
        Command::Operator {
            count,
            operator,
            register,
            target: Target::Motion { count, motion },
        }
    }

    fn delete_characters(
        &mut self,
        before: bool,
        count: usize,
        register: Option<char>,
    ) -> Result<(), EditorError> {
        let cursor = self.document.cursor();
        let line = self.document.line_text();
        let col = cursor.col.get().min(line.len());
        let (start_col, end_col) = if before {
            let mut start = col;
            for _ in 0..count.max(1) {
                start = prev_grapheme_boundary(&line, start).unwrap_or(0);
            }
            (start, col)
        } else {
            let mut end = col;
            for _ in 0..count.max(1) {
                end = next_grapheme_boundary(&line, end).unwrap_or(line.len());
            }
            (col, end)
        };
        if start_col == end_col {
            return Ok(());
        }
        let start = self.document.buffer().line_col_to_byte(LineCol {
            line: cursor.line,
            col: ByteCol::new(start_col),
        })?;
        let end = self.document.buffer().line_col_to_byte(LineCol {
            line: cursor.line,
            col: ByteCol::new(end_col),
        })?;
        if let Some(text) = self.document.delete(TextRange::Charwise(start..end))? {
            self.registers
                .delete(register, text, RegisterKind::Charwise, true);
            self.invalidate_search_matches();
            self.last_repeat = Some(Repeat {
                command: Command::Action {
                    count,
                    register,
                    action: Action::DeleteChar { before, count },
                },
                inserted: None,
            });
        }
        Ok(())
    }

    fn put_register(
        &mut self,
        register: Option<char>,
        before: bool,
        count: usize,
    ) -> Result<(), EditorError> {
        let Some(value) = self.registers.read(register) else {
            self.message = Some("E353: Nothing in register".to_string());
            return Ok(());
        };
        for _ in 0..count.max(1) {
            match value.kind {
                RegisterKind::Linewise => self.document.put(&value.text, true, before)?,
                RegisterKind::Charwise => self.document.put(&value.text, false, before)?,
                RegisterKind::Blockwise { .. } => {
                    self.document.put_block(&value.text, !before)?;
                }
            }
        }
        self.invalidate_search_matches();
        self.last_repeat = Some(Repeat {
            command: Command::Action {
                count,
                register,
                action: Action::Put { before, count },
            },
            inserted: None,
        });
        Ok(())
    }

    fn toggle_case(&mut self, count: usize) -> Result<(), EditorError> {
        let cursor = self.document.cursor();
        let line = self.document.line_text();
        let start_col = cursor.col.get().min(line.len());
        let mut end_col = start_col;
        for _ in 0..count.max(1) {
            end_col = next_grapheme_boundary(&line, end_col).unwrap_or(line.len());
        }
        if start_col == end_col {
            return Ok(());
        }
        let start = self.document.buffer().line_col_to_byte(LineCol {
            line: cursor.line,
            col: ByteCol::new(start_col),
        })?;
        let end = self.document.buffer().line_col_to_byte(LineCol {
            line: cursor.line,
            col: ByteCol::new(end_col),
        })?;
        if self
            .document
            .transform_text(&TextRange::Charwise(start..end), |text| {
                text.chars()
                    .flat_map(|character| {
                        if character.is_lowercase() {
                            character.to_uppercase().collect::<Vec<_>>()
                        } else {
                            character.to_lowercase().collect::<Vec<_>>()
                        }
                    })
                    .collect()
            })?
        {
            self.invalidate_search_matches();
        }
        Ok(())
    }

    fn repeat_change(&mut self) -> Result<(), EditorError> {
        let Some(repeat) = self.last_repeat.clone() else {
            self.message = Some("E: no previous change".to_string());
            return Ok(());
        };
        self.execute_normal(repeat.command)?;
        if let Some(text) = repeat.inserted
            && matches!(self.mode, Mode::Insert | Mode::Replace)
        {
            for character in text.chars() {
                if self.mode == Mode::Replace {
                    self.replace_character(character)?;
                } else {
                    self.insert_char(character)?;
                    self.insert_text.push(character);
                }
            }
            self.finish_insert();
        }
        Ok(())
    }

    fn stop_recording(&mut self) {
        let Some((name, keys)) = self.recording.take() else {
            return;
        };
        self.macros.insert(name, keys);
        self.last_macro = Some(name);
        self.message = Some(format!("recorded @{name}"));
    }

    fn play_macro(&mut self, name: char, count: usize) -> Result<(), EditorError> {
        let Some(keys) = self.macros.get(&name).cloned() else {
            self.message = Some(format!("E748: No such macro: {name}"));
            return Ok(());
        };
        if self.macro_depth >= 100 {
            self.message = Some("E: recursive macro aborted".to_string());
            return Ok(());
        }
        self.last_macro = Some(name);
        self.macro_depth += 1;
        for _ in 0..count.max(1) {
            for event in &keys {
                self.handle(*event)?;
                if self.should_quit {
                    break;
                }
            }
        }
        self.macro_depth -= 1;
        Ok(())
    }

    fn handle_replace(&mut self, event: InputEvent) -> Result<(), EditorError> {
        match event {
            InputEvent::Char(character) => {
                self.replace_character(character)?;
                self.insert_text.push(character);
            }
            InputEvent::Enter => {
                self.insert_newline()?;
                self.insert_text.push('\n');
            }
            InputEvent::Backspace => {
                self.document.move_left_in_line();
                self.insert_text.pop();
            }
            InputEvent::Esc => self.finish_insert(),
            InputEvent::Left => self.document.move_left(),
            InputEvent::Right => self.document.move_right(),
            InputEvent::Up => self.document.move_vertical(-1),
            InputEvent::Down => self.document.move_vertical(1),
            InputEvent::Ctrl(_) => {}
        }
        Ok(())
    }

    fn replace_character(&mut self, character: char) -> Result<(), EditorError> {
        let cursor = self.document.cursor();
        let line = self.document.line_text();
        if cursor.col.get() >= line.len() {
            self.insert_char(character)?;
            return Ok(());
        }
        let range = self.document.char_range_at_cursor()?;
        let TextRange::Charwise(bytes) = range else {
            return Ok(());
        };
        let mut encoded = [0; 4];
        self.document
            .replace(bytes, character.encode_utf8(&mut encoded))?;
        self.invalidate_search_matches();
        Ok(())
    }

    /// Starts a Visual selection at the current cursor.
    fn start_visual(&mut self, kind: VisualKind) {
        self.last_visual = Some(Visual {
            anchor: self.document.cursor(),
            kind,
        });
        self.mode = Mode::Visual(Visual {
            anchor: self.document.cursor(),
            kind,
        });
        self.message = None;
    }

    /// Handles a key while a Visual selection is active.
    ///
    /// Motions only move the cursor; the anchor stays fixed, so the selection
    /// can extend backwards or across line ends. Text verbs resolve the
    /// current selection through `Document::visual_range` and the same range
    /// operations Normal mode uses.
    fn handle_visual(&mut self, event: InputEvent, visual: Visual) -> Result<(), EditorError> {
        match event {
            InputEvent::Char('v') => {
                self.mode = if visual.kind == VisualKind::Charwise {
                    Mode::Normal
                } else {
                    Mode::Visual(Visual {
                        kind: VisualKind::Charwise,
                        ..visual
                    })
                };
                self.message = None;
            }
            InputEvent::Char('V') => {
                self.mode = if visual.kind == VisualKind::Linewise {
                    Mode::Normal
                } else {
                    Mode::Visual(Visual {
                        kind: VisualKind::Linewise,
                        ..visual
                    })
                };
                self.message = None;
            }
            InputEvent::Esc => {
                self.last_visual = Some(visual);
                self.mode = Mode::Normal;
                self.message = None;
            }
            InputEvent::Ctrl('v') => {
                self.mode = Mode::Visual(Visual {
                    kind: VisualKind::Blockwise { to_eol: false },
                    ..visual
                });
                self.message = None;
            }
            InputEvent::Char('I') | InputEvent::Char('A') => {
                self.begin_block_insert(visual, event == InputEvent::Char('A'))?;
            }
            InputEvent::Char('d') | InputEvent::Char('x') => self.delete_visual(visual)?,
            InputEvent::Char('y') => self.yank_visual(visual)?,
            InputEvent::Char('c') => self.change_visual(visual)?,
            InputEvent::Char('h') | InputEvent::Left => self.document.move_left(),
            InputEvent::Char('j') | InputEvent::Down => self.document.move_vertical(1),
            InputEvent::Char('k') | InputEvent::Up => self.document.move_vertical(-1),
            InputEvent::Char('l') | InputEvent::Right => self.document.move_right(),
            InputEvent::Char('w') => self.move_word_visual(true)?,
            InputEvent::Char('b') => self.move_word_visual(false)?,
            InputEvent::Char('0') => self.document.move_to_line_start(),
            InputEvent::Char('$') => {
                if matches!(visual.kind, VisualKind::Blockwise { .. }) {
                    self.mode = Mode::Visual(Visual {
                        kind: VisualKind::Blockwise { to_eol: true },
                        ..visual
                    });
                } else {
                    self.document.move_to_last_char();
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn visual_range(&self, visual: Visual) -> Result<TextRange, EditorError> {
        Ok(self.document.visual_range(visual.anchor, visual.kind)?)
    }

    fn delete_visual(&mut self, visual: Visual) -> Result<(), EditorError> {
        if let VisualKind::Blockwise { .. } = visual.kind {
            let block = self.document.block_selection(visual.anchor, visual.kind)?;
            let width = block.end_col.saturating_sub(block.start_col).max(1);
            if let Some(text) = self.document.block_delete(&block)? {
                self.registers
                    .delete(None, text, RegisterKind::Blockwise { width }, false);
                self.invalidate_search_matches();
            }
            self.last_visual = Some(visual);
            self.mode = Mode::Normal;
            self.message = None;
            return Ok(());
        }
        let range = self.visual_range(visual)?;
        let linewise = range.is_linewise();
        if let Some(text) = self.document.delete(range)? {
            self.registers
                .delete(None, text, self.register_kind(linewise), !linewise);
            self.invalidate_search_matches();
        }
        self.mode = Mode::Normal;
        self.message = None;
        Ok(())
    }

    fn yank_visual(&mut self, visual: Visual) -> Result<(), EditorError> {
        if let VisualKind::Blockwise { .. } = visual.kind {
            let block = self.document.block_selection(visual.anchor, visual.kind)?;
            let width = block.end_col.saturating_sub(block.start_col).max(1);
            let text = self.document.block_text(&block)?;
            self.registers
                .yank(None, text, RegisterKind::Blockwise { width });
            self.document
                .move_to_display_column(block.start_line, block.start_col)?;
            self.last_visual = Some(visual);
            self.mode = Mode::Normal;
            self.message = None;
            return Ok(());
        }
        let range = self.visual_range(visual)?;
        let text = self.document.yank(&range)?;
        self.registers
            .yank(None, text, self.register_kind(range.is_linewise()));
        self.document.move_to_range_start(&range)?;
        self.mode = Mode::Normal;
        self.message = None;
        Ok(())
    }

    fn change_visual(&mut self, visual: Visual) -> Result<(), EditorError> {
        if let VisualKind::Blockwise { .. } = visual.kind {
            let block = self.document.block_selection(visual.anchor, visual.kind)?;
            let width = block.end_col.saturating_sub(block.start_col).max(1);
            if let Some(text) = self.document.block_delete(&block)? {
                self.registers
                    .change(None, text, RegisterKind::Blockwise { width }, false);
                self.invalidate_search_matches();
            }
            self.block_insert = Some(BlockInsert {
                line: block.start_line,
                lines: block.end_line.get() - block.start_line.get() + 1,
                column: block.start_col,
            });
            self.last_visual = Some(visual);
            self.insert_repeat = None;
            self.insert_text.clear();
            self.mode = Mode::Insert;
            self.message = None;
            return Ok(());
        }
        let range = self.visual_range(visual)?;
        let linewise = range.is_linewise();
        if let Some(text) = self.document.change(range)? {
            self.registers
                .change(None, text, self.register_kind(linewise), !linewise);
            self.invalidate_search_matches();
        }
        // Do not break undo here: text typed into the changed selection joins
        // the delete/replace block, as `o` and `O` already do.
        self.last_visual = Some(visual);
        self.insert_repeat = None;
        self.insert_text.clear();
        self.mode = Mode::Insert;
        self.message = None;
        Ok(())
    }

    fn begin_block_insert(&mut self, visual: Visual, append: bool) -> Result<(), EditorError> {
        let block = self.document.block_selection(visual.anchor, visual.kind)?;
        let column = if append {
            block.end_col
        } else {
            block.start_col
        };
        self.document
            .move_to_display_column(block.start_line, column)?;
        self.block_insert = Some(BlockInsert {
            line: block.start_line,
            lines: block.end_line.get() - block.start_line.get() + 1,
            column,
        });
        self.last_visual = Some(visual);
        self.insert_repeat = None;
        self.insert_text.clear();
        self.document.break_undo();
        self.mode = Mode::Insert;
        self.message = None;
        Ok(())
    }

    fn handle_select(&mut self, event: InputEvent, visual: Visual) -> Result<(), EditorError> {
        match event {
            InputEvent::Char(character) if !character.is_control() => {
                self.change_visual(visual)?;
                self.insert_char(character)?;
                self.insert_text.push(character);
                Ok(())
            }
            InputEvent::Backspace => {
                self.change_visual(visual)?;
                Ok(())
            }
            InputEvent::Esc => {
                self.last_visual = Some(visual);
                self.mode = Mode::Normal;
                self.message = None;
                Ok(())
            }
            other => self.handle_visual(other, visual),
        }
    }

    /// Moves by one word while extending a Visual selection.
    ///
    /// At a line boundary the within-line word motion is empty; wrapping one
    /// grapheme to the adjacent line keeps selection extension well defined
    /// without changing Normal-mode `w`/`b`.
    fn move_word_visual(&mut self, forward: bool) -> Result<(), EditorError> {
        let range = self.document.word_range(forward)?;
        let TextRange::Charwise(range) = range else {
            return Ok(());
        };
        if range.start == range.end {
            if forward {
                self.document.move_right();
            } else {
                self.document.move_left();
            }
        } else {
            self.document
                .move_to(if forward { range.end } else { range.start })?;
        }
        Ok(())
    }

    fn handle_insert(&mut self, event: InputEvent) -> Result<(), EditorError> {
        match event {
            InputEvent::Char(character) => {
                self.insert_char(character)?;
                self.insert_text.push(character);
            }
            InputEvent::Enter => {
                self.insert_newline()?;
                self.insert_text.push('\n');
            }
            InputEvent::Backspace => {
                self.backspace()?;
                self.insert_text.pop();
            }
            InputEvent::Esc => self.finish_insert(),
            InputEvent::Left => self.document.move_left(),
            InputEvent::Right => self.document.move_right(),
            InputEvent::Up => self.document.move_vertical(-1),
            InputEvent::Down => self.document.move_vertical(1),
            InputEvent::Ctrl('w') => {
                self.backspace()?;
                self.insert_text.pop();
            }
            InputEvent::Ctrl(_) => {}
        }
        Ok(())
    }

    fn handle_command(&mut self, event: InputEvent) {
        match event {
            InputEvent::Char(character) => {
                if let Some(command) = &mut self.command {
                    command.push(character);
                }
            }
            InputEvent::Backspace => {
                if self
                    .command
                    .as_ref()
                    .is_some_and(|command| command.is_empty())
                {
                    self.mode = Mode::Normal;
                    self.command = None;
                } else if let Some(command) = &mut self.command {
                    command.pop();
                }
            }
            InputEvent::Esc => {
                self.mode = Mode::Normal;
                self.command = None;
            }
            InputEvent::Enter => {
                let command = self.command.take().unwrap_or_default();
                self.mode = Mode::Normal;
                self.execute_command(&command);
            }
            _ => {}
        }
    }

    /// Opens the `/` prompt. The previous search stays active until Enter.
    fn start_search(&mut self) {
        self.mode = Mode::Search;
        self.search_input.clear();
        self.message = None;
    }

    /// Handles editing of the `/` pattern before it is executed.
    fn handle_search(&mut self, event: InputEvent) -> Result<(), EditorError> {
        match event {
            InputEvent::Char(character) => self.search_input.push(character),
            InputEvent::Backspace => {
                self.search_input.pop();
            }
            InputEvent::Esc => {
                self.mode = Mode::Normal;
                self.search_input.clear();
                self.message = None;
            }
            InputEvent::Enter => {
                let pattern = std::mem::take(&mut self.search_input);
                self.execute_search(pattern)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Executes a literal search from the current cursor, wrapping forward.
    fn execute_search(&mut self, pattern: String) -> Result<(), EditorError> {
        self.mode = Mode::Normal;
        self.search_input.clear();
        if pattern.is_empty() {
            self.message = Some("Search pattern is empty".to_string());
            return Ok(());
        }

        self.registers.set_last_search(&pattern);
        self.search.query = Some(pattern.clone());
        self.search.matches = self.document.search_matches(&pattern);
        self.search.current = None;
        if self.search.matches.is_empty() {
            self.message = Some(format!("E486: Pattern not found: {pattern}"));
            return Ok(());
        }
        self.advance_search(true)
    }

    /// Repeats the last search forward or backward.
    fn repeat_search(&mut self, forward: bool) -> Result<(), EditorError> {
        if self.search.query.is_none() {
            self.message = Some("E35: No previous search pattern".to_string());
            return Ok(());
        }
        self.advance_search(forward)
    }

    /// Moves to the next match in `forward` order, wrapping at the ends.
    fn advance_search(&mut self, forward: bool) -> Result<(), EditorError> {
        let Some(pattern) = self.search.query.clone() else {
            return Ok(());
        };
        if self.search.matches.is_empty() {
            self.search.matches = self.document.search_matches(&pattern);
        }
        if self.search.matches.is_empty() {
            self.search.current = None;
            self.message = Some(format!("E486: Pattern not found: {pattern}"));
            return Ok(());
        }

        let cursor = self
            .document
            .buffer()
            .line_col_to_byte(self.document.cursor())?;
        let Some((index, wrapped)) = next_search_match(&self.search.matches, cursor, forward)
        else {
            return Ok(());
        };
        let range = self.search.matches[index].clone();
        self.document.move_to(range.start)?;
        self.search.current = Some(range);
        self.message = wrapped.then(|| {
            if forward {
                "search hit BOTTOM, continuing at TOP".to_string()
            } else {
                "search hit TOP, continuing at BOTTOM".to_string()
            }
        });
        Ok(())
    }

    /// Drops cached match offsets after a text change while keeping the query
    /// available for the next `n`/`N`.
    fn invalidate_search_matches(&mut self) {
        self.search.matches.clear();
        self.search.current = None;
    }

    fn execute_command(&mut self, command: &str) {
        let command = command.trim();
        if command.is_empty() {
            return;
        }
        self.registers.set_last_command(command);
        let (name, argument) = split_command(command);

        match name {
            "w" => {
                if argument.is_empty() {
                    self.save_reporting();
                } else {
                    self.save_as_reporting(command_path(argument));
                }
            }
            "wq" => {
                let saved = if argument.is_empty() {
                    self.save_reporting()
                } else {
                    self.save_as_reporting(command_path(argument))
                };
                if saved {
                    self.should_quit = true;
                }
            }
            "q" => {
                if let Err(error) = self.close_focused_window(false) {
                    self.message = Some(error.to_string());
                }
            }
            "q!" => {
                if let Err(error) = self.close_focused_window(true) {
                    self.message = Some(error.to_string());
                }
            }
            "x" => {
                if !self.document.is_modified() || self.save_reporting() {
                    self.should_quit = true;
                }
            }
            "preserve" => match self.document.preserve_swap() {
                Ok(true) => self.message = Some("swap file preserved".to_string()),
                Ok(false) => self.message = Some("no active swap to preserve".to_string()),
                Err(error) => self.message = Some(format!("swap: {error}")),
            },
            "set" => self.execute_set(argument),
            "normal" => {
                if let Err(error) = self.execute_normal_keys(argument) {
                    self.message = Some(error.to_string());
                }
            }
            "registers" | "reg" => self.message = Some(self.registers.listing()),
            "e" | "e!" => {
                if argument.is_empty() {
                    self.message = Some("E32: No file name".to_string());
                } else {
                    self.open_document(command_path(argument), name == "e!");
                }
            }
            "ls" | "buffers" => self.list_buffers(),
            "b" => self.buffer_command(argument),
            "bnext" => self.cycle_buffer(1),
            "bprev" | "bprevious" => self.cycle_buffer(-1),
            "bd" | "bdelete" | "bd!" | "bdelete!" => {
                let force = name.ends_with('!');
                let target = if argument.is_empty() {
                    self.active_buffer
                } else {
                    self.parse_buffer_id(argument).unwrap_or(self.active_buffer)
                };
                if let Err(error) = self.delete_buffer(target, force) {
                    self.message = Some(error.to_string());
                }
            }
            "sp" | "split" => {
                self.split_focus(false);
                if !argument.is_empty() {
                    self.open_document(command_path(argument), false);
                }
            }
            "vsp" | "vsplit" => {
                self.split_focus(true);
                if !argument.is_empty() {
                    self.open_document(command_path(argument), false);
                }
            }
            "tabnew" => self.tab_new_command(argument),
            "tabclose" => {
                if let Err(error) = self.close_tab() {
                    self.message = Some(error.to_string());
                }
            }
            "tabnext" => self.tab_command(argument, 1),
            "tabprev" | "tabprevious" => self.tab_command(argument, -1),
            "recover" | "recover!" => {
                self.recover_document(argument, name == "recover!");
            }
            other => self.message = Some(format!("E492: Not an editor command: {other}")),
        }
    }

    /// Applies the backup and encoding options exposed by this minimal editor.
    ///
    /// This is deliberately not a general `:set` implementation. It covers
    /// the BUF-005 backup options and the BUF-007 `fileencoding`/`bomb`
    /// options, supports the Vim boolean negations, and accepts a
    /// comma-separated `backupdir` list.
    fn execute_set(&mut self, argument: &str) {
        if argument.is_empty() {
            let options = self.document.backup_options();
            self.message = Some(format!(
                "backup={} writebackup={} backupdir={} backupext={} fileencoding={} bomb={}",
                boolean_name(options.backup, "backup"),
                boolean_name(options.write_backup, "writebackup"),
                options
                    .backup_dirs
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(","),
                options.extension,
                self.document.encoding(),
                boolean_name(self.document.bomb(), "bomb"),
            ));
            return;
        }

        for token in split_set_arguments(argument) {
            let token = token.as_str();
            if let Some(query) = token.strip_suffix('?') {
                let options = self.document.backup_options();
                self.message = Some(match query {
                    "backup" => boolean_name(options.backup, "backup"),
                    "writebackup" => boolean_name(options.write_backup, "writebackup"),
                    "backupdir" => format!(
                        "backupdir={}",
                        options
                            .backup_dirs
                            .iter()
                            .map(|path| path.display().to_string())
                            .collect::<Vec<_>>()
                            .join(",")
                    ),
                    "backupext" => format!("backupext={}", options.extension),
                    "fileencoding" => {
                        format!("fileencoding={}", self.document.encoding())
                    }
                    "bomb" => boolean_name(self.document.bomb(), "bomb"),
                    _ => format!("E518: Unknown option: {query}"),
                });
                return;
            }

            match token {
                "backup" => self.document.backup_options_mut().backup = true,
                "nobackup" => self.document.backup_options_mut().backup = false,
                "writebackup" => self.document.backup_options_mut().write_backup = true,
                "nowritebackup" => self.document.backup_options_mut().write_backup = false,
                "bomb" => self.document.set_bomb(true),
                "nobomb" => self.document.set_bomb(false),
                _ => {
                    if let Some(value) = token.strip_prefix("backupdir=") {
                        let directories = value
                            .split(',')
                            .map(str::trim)
                            .filter(|part| !part.is_empty())
                            .map(PathBuf::from)
                            .collect();
                        self.document.backup_options_mut().backup_dirs = directories;
                    } else if let Some(value) = token.strip_prefix("backupext=") {
                        if value.is_empty() || value.contains('/') || value.contains('\\') {
                            self.message = Some(format!("E474: Invalid argument: {token}"));
                            return;
                        }
                        self.document.backup_options_mut().extension = value.to_string();
                    } else if let Some(value) = token.strip_prefix("fileencoding=") {
                        match TextEncoding::parse(value) {
                            Some(encoding) => self.document.set_encoding(encoding),
                            None => {
                                self.message = Some(format!("E474: Invalid argument: {token}"));
                                return;
                            }
                        }
                    } else {
                        self.message = Some(format!("E518: Unknown option: {token}"));
                        return;
                    }
                }
            }
        }
        self.message = None;
    }

    /// Replaces the current document with `path` after a successful read.
    ///
    /// A failed read or decoding leaves the current document, cursor and
    /// modified flag untouched and reports the error in the status row.
    fn open_document(&mut self, path: PathBuf, force: bool) {
        if !force && self.document.is_modified() {
            self.message = Some("E37: No write since last change (add ! to override)".to_string());
            return;
        }
        if let Some(id) = self.buffer_id_for_path(&path) {
            if let Err(error) = self.show_buffer_in_focus(id) {
                self.message = Some(error.to_string());
            }
            return;
        }
        let backup_options = self.document.backup_options().clone();
        match Document::open(path) {
            Ok(mut document) => {
                *document.backup_options_mut() = backup_options;
                let warning = document_warning(&document);
                self.open_buffer(document);
                self.reset_view_state();
                self.message = warning;
            }
            Err(error) => self.message = Some(error.to_string()),
        }
    }

    /// Loads a named or untitled swap into a fresh document.
    ///
    /// A modified current buffer is refused unless the command was forced,
    /// so `:recover` never silently replaces unsaved text. The retained
    /// swap is adopted and stays on disk until a successful save or quit.
    fn recover_document(&mut self, argument: &str, force: bool) {
        if !force && self.document.is_modified() {
            self.message = Some("E37: No write since last change (add ! to override)".to_string());
            return;
        }
        let backup_options = self.document.backup_options().clone();
        let recovered = if argument.is_empty() {
            Document::recover_untitled()
        } else {
            Document::recover_named(command_path(argument))
        };
        match recovered {
            Ok(Some(mut document)) => {
                *document.backup_options_mut() = backup_options;
                self.document.close_session();
                self.document = document;
                self.reset_view_state();
                self.message = Some(
                    if argument.is_empty() {
                        "recovered untitled swap; save with :w <path>"
                    } else {
                        "recovered swap file"
                    }
                    .to_string(),
                );
            }
            Ok(None) => {
                let target = if argument.is_empty() {
                    "[No Name]".to_string()
                } else {
                    argument.to_string()
                };
                self.message = Some(format!("E305: No swap file found for {target}"));
            }
            Err(error) => self.message = Some(format!("E325: {error}")),
        }
    }

    /// Resets transient input and view state for a new document.
    fn reset_view_state(&mut self) {
        self.mode = Mode::Normal;
        self.command = None;
        self.normal.reset();
        self.search = SearchState::default();
        self.search_input.clear();
        self.left_col = 0;
    }

    fn request_quit(&mut self) {
        if self.document.is_modified() {
            self.message = Some("E37: No write since last change (add ! to override)".to_string());
        } else {
            self.should_quit = true;
        }
    }

    /// Undoes the most recent change block.
    fn undo(&mut self) -> Result<(), EditorError> {
        if self.document.undo()? {
            self.invalidate_search_matches();
            self.message = None;
        } else {
            self.message = Some("Already at oldest change".to_string());
        }
        Ok(())
    }

    /// Redoes the most recently undone change block.
    fn redo(&mut self) -> Result<(), EditorError> {
        if self.document.redo()? {
            self.invalidate_search_matches();
            self.message = None;
        } else {
            self.message = Some("Already at newest change".to_string());
        }
        Ok(())
    }

    /// Saves the document and reports the outcome in the status row.
    ///
    /// A failed save keeps the editor open and the buffer marked modified, so
    /// the user never loses the ability to retry or copy the text out.
    fn save_reporting(&mut self) -> bool {
        match self.document.save() {
            Ok(outcome) => {
                self.document.mark_saved();
                self.report_save_outcome(&outcome);
                true
            }
            Err(DocumentSaveError::NoFileName) => {
                self.message = Some("E32: No file name".to_string());
                false
            }
            Err(DocumentSaveError::Encode(error)) => {
                self.message = Some(error.to_string());
                false
            }
            Err(error) => {
                self.message = Some(format!("save failed: {error}"));
                false
            }
        }
    }

    /// Saves the document under `path` and reports the outcome.
    fn save_as_reporting(&mut self, path: PathBuf) -> bool {
        match self.document.save_as(path) {
            Ok(outcome) => {
                self.document.mark_saved();
                self.report_save_outcome(&outcome);
                true
            }
            Err(DocumentSaveError::Encode(error)) => {
                self.message = Some(error.to_string());
                false
            }
            Err(error) => {
                self.message = Some(format!("save failed: {error}"));
                false
            }
        }
    }

    /// Reports a successful save, including a kept backup or cleanup warning.
    fn report_save_outcome(&mut self, outcome: &SaveOutcome) {
        let mut message = format!("\"{}\" written", self.document.display_name());
        if let Some(backup) = &outcome.backup {
            message.push_str(&format!("; backup: {}", backup.display()));
        }
        if let Some(warning) = &outcome.warning {
            message.push_str(&format!("; {warning}"));
        }
        self.message = Some(message);
    }

    /// Applies one text replacement and clears the status message.
    ///
    /// This is the editor's only text-mutation path. Undo history belongs to
    /// `Buffer`; `Document::replace` is where the pre- and post-edit cursor
    /// values are available to record in an undo block.
    fn apply_edit(&mut self, range: Range<BytePos>, text: &str) -> Result<(), EditorError> {
        self.document.replace(range, text)?;
        self.invalidate_search_matches();
        self.message = None;
        Ok(())
    }

    fn insert_char(&mut self, character: char) -> Result<(), EditorError> {
        let cursor = self.document.cursor();
        let position = self.document.buffer().line_col_to_byte(cursor)?;
        let mut encoded = [0; 4];
        let text = character.encode_utf8(&mut encoded);
        self.apply_edit(position..position, text)
    }

    fn insert_newline(&mut self) -> Result<(), EditorError> {
        let cursor = self.document.cursor();
        let position = self.document.buffer().line_col_to_byte(cursor)?;
        let newline = match self.document.buffer().line_ending() {
            LineEnding::Crlf => "\r\n",
            LineEnding::Lf => "\n",
        };
        self.apply_edit(position..position, newline)
    }

    fn backspace(&mut self) -> Result<(), EditorError> {
        let cursor = self.document.cursor();
        if cursor.col.get() == 0 {
            if cursor.line.get() == 0 {
                return Ok(());
            }
            let previous_line = LineNumber::new(cursor.line.get() - 1);
            let previous_end = self.document.buffer().line_col_to_byte(LineCol {
                line: previous_line,
                col: ByteCol::new(self.document.buffer().line_len_bytes(previous_line)?),
            })?;
            let line_start = self.document.buffer().line_col_to_byte(LineCol {
                line: cursor.line,
                col: ByteCol::ZERO,
            })?;
            self.apply_edit(previous_end..line_start, "")
        } else {
            let line = self.document.line_text();
            let previous = prev_grapheme_boundary(&line, cursor.col.get()).unwrap_or(0);
            let start = self.document.buffer().line_col_to_byte(LineCol {
                line: cursor.line,
                col: ByteCol::new(previous),
            })?;
            let end = self.document.buffer().line_col_to_byte(cursor)?;
            self.apply_edit(start..end, "")
        }
    }
}

fn replace_leaf(layout: &mut Layout, target: usize, new: usize, vertical: bool) -> bool {
    match layout {
        Layout::Leaf(id) if *id == target => {
            *layout = Layout::Split {
                vertical,
                first: Box::new(Layout::Leaf(target)),
                second: Box::new(Layout::Leaf(new)),
            };
            true
        }
        Layout::Leaf(_) => false,
        Layout::Split { first, second, .. } => {
            replace_leaf(first, target, new, vertical)
                || replace_leaf(second, target, new, vertical)
        }
    }
}

fn close_layout(layout: &Layout, closing: usize) -> Option<Layout> {
    match layout {
        Layout::Leaf(id) if *id == closing => None,
        Layout::Leaf(id) => Some(Layout::Leaf(*id)),
        Layout::Split {
            vertical,
            first,
            second,
        } => match (close_layout(first, closing), close_layout(second, closing)) {
            (Some(first), Some(second)) => Some(Layout::Split {
                vertical: *vertical,
                first: Box::new(first),
                second: Box::new(second),
            }),
            (Some(only), None) | (None, Some(only)) => Some(only),
            (None, None) => None,
        },
    }
}

fn layout_rects(layout: &Layout, area: Rect, output: &mut Vec<(usize, Rect)>) {
    match layout {
        Layout::Leaf(id) => output.push((*id, area)),
        Layout::Split {
            vertical,
            first,
            second,
        } => {
            if *vertical {
                let separator = usize::from(area.width >= 3);
                let available = area.width.saturating_sub(separator);
                let first_width = available / 2;
                let second_width = available - first_width;
                layout_rects(
                    first,
                    Rect {
                        width: first_width,
                        ..area
                    },
                    output,
                );
                layout_rects(
                    second,
                    Rect {
                        x: area.x + first_width + separator,
                        width: second_width,
                        ..area
                    },
                    output,
                );
            } else {
                let first_height = area.height / 2;
                let second_height = area.height - first_height;
                layout_rects(
                    first,
                    Rect {
                        height: first_height,
                        ..area
                    },
                    output,
                );
                layout_rects(
                    second,
                    Rect {
                        y: area.y + first_height,
                        height: second_height,
                        ..area
                    },
                    output,
                );
            }
        }
    }
}

/// Splits `:set` arguments, allowing spaces inside single or double quotes.
///
/// This is intentionally only the quoting needed by path-valued backup
/// options; it is not a general Vim command-line parser.
fn split_set_arguments(argument: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;

    for character in argument.chars() {
        match quote {
            Some(active) if character == active => quote = None,
            Some(_) => current.push(character),
            None if character == '"' || character == '\'' => quote = Some(character),
            None if character.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            None => current.push(character),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Formats a Vim boolean option with its `no` negation.
fn boolean_name(value: bool, name: &str) -> String {
    if value {
        name.to_string()
    } else {
        format!("no{name}")
    }
}

/// Combines the non-fatal notices produced while opening a document.
fn document_warning(document: &Document) -> Option<String> {
    let mut warnings = Vec::new();
    if let Some(warning) = document.encoding_warning() {
        warnings.push(warning.to_string());
    }
    if let Some(warning) = document.swap_warning() {
        warnings.push(warning.to_string());
    }
    if let Some(warning) = document.undo_warning() {
        warnings.push(warning.to_string());
    }
    if warnings.is_empty() {
        None
    } else {
        Some(warnings.join(" | "))
    }
}

/// Maps the grammar's selection shape onto the two ranges implemented by the
/// text layer. Blockwise is reported as characterwise until block primitives
/// land, so a count or register is never silently discarded.
fn selection_kind(mode: SelectionMode) -> VisualKind {
    match mode {
        SelectionMode::Linewise => VisualKind::Linewise,
        SelectionMode::Charwise => VisualKind::Charwise,
        SelectionMode::Blockwise => VisualKind::Blockwise { to_eol: false },
    }
}

/// Returns the next match strictly after (or before) `cursor`, wrapping at
/// the matching end of the list.
///
/// The second value is `true` when the search wrapped. Matches are ordered by
/// start offset, which `literal_matches` guarantees.
fn split_command(command: &str) -> (&str, &str) {
    match command.find(char::is_whitespace) {
        Some(index) => (&command[..index], command[index..].trim()),
        None => (command, ""),
    }
}

/// Returns the path argument, stripping one matching pair of quotes.
fn command_path(argument: &str) -> PathBuf {
    let argument = argument.trim();
    let unquoted = if argument.len() >= 2
        && ((argument.starts_with('"') && argument.ends_with('"'))
            || (argument.starts_with('\'') && argument.ends_with('\'')))
    {
        &argument[1..argument.len() - 1]
    } else {
        argument
    };
    PathBuf::from(unquoted)
}

fn next_search_match(
    matches: &[Range<BytePos>],
    cursor: BytePos,
    forward: bool,
) -> Option<(usize, bool)> {
    if matches.is_empty() {
        return None;
    }
    if forward {
        matches
            .iter()
            .position(|range| range.start > cursor)
            .map(|index| (index, false))
            .or(Some((0, true)))
    } else {
        matches
            .iter()
            .rposition(|range| range.start < cursor)
            .map(|index| (index, false))
            .or(Some((matches.len() - 1, true)))
    }
}
