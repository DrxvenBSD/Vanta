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

use std::io;
use std::ops::Range;
use std::path::PathBuf;
use std::time::Instant;

use vanta_fs::{SaveOutcome, TextEncoding};
use vanta_text::{
    ByteCol, BytePos, LineCol, LineEnding, LineNumber, TextError, prev_grapheme_boundary,
};

use crate::document::{Document, DocumentSaveError, RecoveryError, TextRange, VisualKind};
use crate::terminal::{InputEvent, Size, Terminal};
use crate::view;

mod render;

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
    Insert,
    Command,
    Search,
}

/// The unnamed register and whether its contents are linewise.
#[derive(Clone, Debug, Default)]
struct Register {
    text: String,
    linewise: bool,
}

impl Register {
    fn set(&mut self, text: String, linewise: bool) {
        self.text = text;
        self.linewise = linewise;
    }
}

/// A Normal-mode operator waiting for its motion or doubled key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingOperator {
    Delete,
    Yank,
}

/// One document, one window and the modal editing state.
#[derive(Debug)]
pub struct Editor {
    document: Document,
    mode: Mode,
    command: Option<String>,
    message: Option<String>,
    should_quit: bool,
    register: Register,
    pending: Option<PendingOperator>,
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
        Self {
            document,
            mode: Mode::Normal,
            command: None,
            message: None,
            should_quit: false,
            register: Register::default(),
            pending: None,
            search: SearchState::default(),
            search_input: String::new(),
            left_col: 0,
        }
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
        self.document.close_session();
        Ok(())
    }

    /// Recomputes the horizontal viewport for the current cursor.
    ///
    /// This runs before every frame, so a motion, search, undo or resize can
    /// never leave the cursor outside the visible cells. The previous offset
    /// is kept when the cursor is still inside it, which avoids scrolling the
    /// line back on every leftward step.
    fn ensure_cursor_visible(&mut self, size: Size) {
        let line = self.document.line_text();
        self.left_col = view::keep_cursor_visible(
            &line,
            self.document.cursor().col.get(),
            usize::from(size.width.max(1)),
            self.left_col,
        );
    }

    fn handle(&mut self, event: InputEvent) -> Result<(), EditorError> {
        if self.document.recovery().is_some() {
            self.handle_recovery(event);
            return Ok(());
        }
        match self.mode {
            Mode::Normal => self.handle_normal(event),
            Mode::Visual(visual) => self.handle_visual(event, visual),
            Mode::Insert => self.handle_insert(event),
            Mode::Command => {
                self.handle_command(event);
                Ok(())
            }
            Mode::Search => self.handle_search(event),
        }
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
        self.pending = None;
        self.search = SearchState::default();
        self.search_input.clear();
        self.left_col = 0;
        self.message = Some(message.to_string());
    }

    fn recovery_error(&mut self, error: RecoveryError) {
        self.message = Some(format!("E325: {error}"));
    }

    fn handle_normal(&mut self, event: InputEvent) -> Result<(), EditorError> {
        if let Some(operator) = self.pending.take() {
            return self.apply_operator(operator, event);
        }

        match event {
            InputEvent::Char('i') => {
                self.document.break_undo();
                self.mode = Mode::Insert;
                self.message = None;
            }
            InputEvent::Char('a') => self.append(false),
            InputEvent::Char('A') => self.append(true),
            InputEvent::Char(':') => {
                self.mode = Mode::Command;
                self.command = Some(String::new());
                self.message = None;
            }
            InputEvent::Char('u') => self.undo()?,
            InputEvent::Ctrl('r') => self.redo()?,
            InputEvent::Char('d') => {
                self.pending = Some(PendingOperator::Delete);
                self.message = None;
            }
            InputEvent::Char('y') => {
                self.pending = Some(PendingOperator::Yank);
                self.message = None;
            }
            InputEvent::Char('x') => self.delete_at_cursor()?,
            InputEvent::Char('p') => self.put(false)?,
            InputEvent::Char('P') => self.put(true)?,
            InputEvent::Char('o') => self.open_line(true)?,
            InputEvent::Char('O') => self.open_line(false)?,
            InputEvent::Char('/') => self.start_search(),
            InputEvent::Char('n') => self.repeat_search(true)?,
            InputEvent::Char('N') => self.repeat_search(false)?,
            InputEvent::Char('v') => self.start_visual(VisualKind::Charwise),
            InputEvent::Char('V') => self.start_visual(VisualKind::Linewise),
            InputEvent::Char('w') => self.move_word(true)?,
            InputEvent::Char('b') => self.move_word(false)?,
            InputEvent::Char('0') => self.document.move_to_line_start(),
            InputEvent::Char('$') => self.document.move_to_last_char(),
            InputEvent::Char('h') | InputEvent::Left => self.document.move_left_in_line(),
            InputEvent::Char('j') | InputEvent::Down => self.document.move_vertical(1),
            InputEvent::Char('k') | InputEvent::Up => self.document.move_vertical(-1),
            InputEvent::Char('l') | InputEvent::Right => self.document.move_right_in_line(),
            InputEvent::Esc => self.message = None,
            _ => {}
        }
        Ok(())
    }

    /// Enters Insert after the cursor (`a`) or at the end of the line (`A`).
    fn append(&mut self, at_line_end: bool) {
        if at_line_end {
            self.document.move_to_line_end();
        } else {
            self.document.move_past_cursor();
        }
        self.document.break_undo();
        self.mode = Mode::Insert;
        self.message = None;
    }

    /// Starts a Visual selection at the current cursor.
    fn start_visual(&mut self, kind: VisualKind) {
        self.mode = Mode::Visual(Visual {
            anchor: self.document.cursor(),
            kind,
        });
        self.pending = None;
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
                self.mode = Mode::Normal;
                self.message = None;
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
            InputEvent::Char('$') => self.document.move_to_last_char(),
            _ => {}
        }
        Ok(())
    }

    fn visual_range(&self, visual: Visual) -> Result<TextRange, EditorError> {
        Ok(self.document.visual_range(visual.anchor, visual.kind)?)
    }

    fn delete_visual(&mut self, visual: Visual) -> Result<(), EditorError> {
        let range = self.visual_range(visual)?;
        let linewise = range.is_linewise();
        if let Some(text) = self.document.delete(range)? {
            self.register.set(text, linewise);
            self.invalidate_search_matches();
        }
        self.mode = Mode::Normal;
        self.pending = None;
        self.message = None;
        Ok(())
    }

    fn yank_visual(&mut self, visual: Visual) -> Result<(), EditorError> {
        let range = self.visual_range(visual)?;
        let text = self.document.yank(&range)?;
        self.register.set(text, range.is_linewise());
        self.document.move_to_range_start(&range)?;
        self.mode = Mode::Normal;
        self.pending = None;
        self.message = None;
        Ok(())
    }

    fn change_visual(&mut self, visual: Visual) -> Result<(), EditorError> {
        let range = self.visual_range(visual)?;
        let linewise = range.is_linewise();
        if let Some(text) = self.document.change(range)? {
            self.register.set(text, linewise);
            self.invalidate_search_matches();
        }
        // Do not break undo here: text typed into the changed selection joins
        // the delete/replace block, as `o` and `O` already do.
        self.mode = Mode::Insert;
        self.pending = None;
        self.message = None;
        Ok(())
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

    /// Applies a pending `d` or `y` to its motion.
    fn apply_operator(
        &mut self,
        operator: PendingOperator,
        event: InputEvent,
    ) -> Result<(), EditorError> {
        match (operator, event) {
            (PendingOperator::Delete, InputEvent::Char('d')) => {
                let range = self.document.current_line_range();
                self.delete_range(range)?;
            }
            (PendingOperator::Delete, InputEvent::Char('w')) => {
                let range = self.document.word_range(true)?;
                self.delete_range(range)?;
            }
            (PendingOperator::Delete, InputEvent::Char('b')) => {
                let range = self.document.word_range(false)?;
                self.delete_range(range)?;
            }
            (PendingOperator::Yank, InputEvent::Char('y')) => {
                let range = self.document.current_line_range();
                self.yank_range(&range)?;
            }
            (_, InputEvent::Esc) => self.message = None,
            _ => {}
        }
        Ok(())
    }

    fn delete_at_cursor(&mut self) -> Result<(), EditorError> {
        let range = self.document.char_range_at_cursor()?;
        self.delete_range(range)
    }

    fn delete_range(&mut self, range: TextRange) -> Result<(), EditorError> {
        let linewise = range.is_linewise();
        if let Some(text) = self.document.delete(range)? {
            self.register.set(text, linewise);
            self.invalidate_search_matches();
            self.message = None;
        }
        Ok(())
    }

    fn yank_range(&mut self, range: &TextRange) -> Result<(), EditorError> {
        let text = self.document.yank(range)?;
        self.register.set(text, range.is_linewise());
        self.message = None;
        Ok(())
    }
    fn put(&mut self, before_cursor: bool) -> Result<(), EditorError> {
        self.document
            .put(&self.register.text, self.register.linewise, before_cursor)?;
        self.invalidate_search_matches();
        self.message = None;
        Ok(())
    }

    fn open_line(&mut self, below: bool) -> Result<(), EditorError> {
        self.document.open_line(below)?;
        self.invalidate_search_matches();
        self.mode = Mode::Insert;
        self.message = None;
        Ok(())
    }

    fn move_word(&mut self, forward: bool) -> Result<(), EditorError> {
        if forward {
            self.document.move_word_forward()?;
        } else if let TextRange::Charwise(range) = self.document.word_range(false)? {
            self.document.move_to(range.start)?;
        }
        Ok(())
    }

    fn handle_insert(&mut self, event: InputEvent) -> Result<(), EditorError> {
        match event {
            InputEvent::Char(character) => self.insert_char(character)?,
            InputEvent::Enter => self.insert_newline()?,
            InputEvent::Backspace => self.backspace()?,
            InputEvent::Esc => {
                // Leaving Insert ends the undo session even when the cursor
                // does not move.
                self.document.break_undo();
                self.mode = Mode::Normal;
                // Vim leaves Insert mode without wrapping to the previous
                // line; only an explicit `h` or Left does that.
                self.document.move_left_in_line();
            }
            InputEvent::Left => self.document.move_left(),
            InputEvent::Right => self.document.move_right(),
            InputEvent::Up => self.document.move_vertical(-1),
            InputEvent::Down => self.document.move_vertical(1),
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
            "q" => self.request_quit(),
            "q!" => self.should_quit = true,
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
            "e" | "e!" => {
                if argument.is_empty() {
                    self.message = Some("E32: No file name".to_string());
                } else if name == "e" && self.document.is_modified() {
                    self.message =
                        Some("E37: No write since last change (add ! to override)".to_string());
                } else {
                    self.open_document(command_path(argument));
                }
            }
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
    fn open_document(&mut self, path: PathBuf) {
        let backup_options = self.document.backup_options().clone();
        match Document::open(path) {
            Ok(mut document) => {
                *document.backup_options_mut() = backup_options;
                self.document.close_session();
                let warning = document_warning(&document);
                self.document = document;
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
        self.pending = None;
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
    if warnings.is_empty() {
        None
    } else {
        Some(warnings.join(" | "))
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
