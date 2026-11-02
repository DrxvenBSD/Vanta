//! One open file and its single-window view state.
//!
//! A [`Document`] owns the [`Buffer`], the file path, the cursor and the
//! crash-recovery swap lifecycle. It is internal to the editor; the public
//! surface remains `Editor`. When multiple buffers land, the editor stores
//! these values in its buffer set and switches the active one instead of
//! growing another set of per-file fields.
//!
//! Text changes go through [`Document::replace`] or the range operations in
//! the `range` module, including the resolver that turns a Visual anchor and
//! cursor into one of those ranges. Undo history lives in `vanta-text`, owned
//! by `Buffer` (`PROJECT_SPEC` §5.12); this type supplies the cursor before and
//! after each edit so the buffer can record a complete block.

use std::borrow::Cow;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use vanta_fs::{BackupOptions, SaveOutcome};
use vanta_text::{
    Buffer, ByteCol, BytePos, LineCol, LineEnding, LineNumber, TextEdit, TextError,
    display_col_of_byte, next_grapheme_boundary, prev_grapheme_boundary,
};

use crate::swap::{SwapError, SwapFile, SwapSnapshot, recoverable_untitled};

/// How long a deferred swap may wait for input before it is flushed.
const IDLE_SWAP_FLUSH: Duration = Duration::from_secs(4);

use crate::view::{self, TABSTOP};

mod range;
mod search;

pub(crate) use range::{TextRange, VisualKind};
pub(crate) use search::literal_matches;

use crate::editor::EditorError;

/// The state of a swap file discovered while opening a named buffer.
#[derive(Clone, Debug)]
pub(crate) enum RecoveryState {
    /// The swap belongs to this file and can be loaded.
    Recoverable {
        /// The recovered text and metadata.
        snapshot: SwapSnapshot,
        /// Why the writer is no longer considered active, if known.
        stale: Option<String>,
    },
    /// The header's path hash does not match this file.
    Mismatched {
        /// The hash encoded in this file's swap name.
        expected: u64,
        /// The hash found in the swap header.
        found: u64,
    },
    /// The swap exists but cannot be parsed or validated.
    Corrupt(String),
}

/// The one startup recovery decision the editor must resolve.
#[derive(Clone, Debug)]
pub(crate) struct RecoveryInfo {
    /// The swap file that triggered the prompt.
    pub(crate) swap_path: PathBuf,
    /// Its validated or rejected state.
    pub(crate) state: RecoveryState,
}

impl RecoveryInfo {
    /// Returns `true` when the swap can be loaded into the buffer.
    pub(crate) fn is_recoverable(&self) -> bool {
        matches!(self.state, RecoveryState::Recoverable { .. })
    }

    /// Returns the reason this swap cannot be recovered, if it cannot.
    pub(crate) fn problem(&self) -> Option<String> {
        match &self.state {
            RecoveryState::Recoverable { .. } => None,
            RecoveryState::Mismatched { expected, found } => Some(format!(
                "swap path hash {found:016x} does not match {expected:016x}"
            )),
            RecoveryState::Corrupt(error) => Some(error.clone()),
        }
    }
}

/// A failed swap recovery decision.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RecoveryError {
    /// The swap is not recoverable, with the reason.
    #[error("{0}")]
    Unavailable(String),
    /// The swap file could not be written or removed.
    #[error("{0}")]
    Swap(#[from] SwapError),
}

/// A failed `:recover` or `vanta -r` request.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RecoverDocumentError {
    /// The named original could not be opened.
    #[error("{0}")]
    Open(#[from] EditorError),
    /// The discovered swap could not be loaded.
    #[error("{0}")]
    Recovery(#[from] RecoveryError),
    /// The adopted swap could not be written back.
    #[error("{0}")]
    Swap(#[from] SwapError),
    /// The state swap directory could not be scanned.
    #[error("cannot read the swap directory: {0}")]
    State(#[from] io::Error),
}

/// A failed document save before it reaches the terminal.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DocumentSaveError {
    /// The document has no path yet.
    #[error("E32: No file name")]
    NoFileName,
    /// The atomic filesystem save failed.
    #[error("{0}")]
    Save(#[from] vanta_fs::SaveError),
}

/// One buffer, its optional path, its cursor and its swap-file lifecycle.
#[derive(Debug)]
pub(crate) struct Document {
    buffer: Buffer,
    path: Option<PathBuf>,
    cursor: LineCol,
    /// Display cell remembered across a vertical motion, matching Vim's
    /// `curswant`. Horizontal motion and jumps clear it; `j` and `k` keep it
    /// so crossing a short line does not lose the user's column.
    preferred_col: Option<usize>,
    /// The swap file for this path, once one exists or is discovered.
    swap: Option<SwapFile>,
    /// Whether this session created or adopted the swap and may remove it.
    /// A swap discovered at startup stays owned by the crashed session until
    /// the user explicitly recovers, deletes it or edits the original.
    swap_owned: bool,
    /// Bytes changed since the last swap flush, used to bound write cost on
    /// large buffers. Small buffers flush on every mutation.
    swap_pending_bytes: usize,
    /// The latest non-fatal swap failure, surfaced in the status row.
    swap_warning: Option<String>,
    /// A discovered swap that must be resolved before normal editing.
    recovery: Option<RecoveryInfo>,
    /// When a deferred swap flush becomes due while the editor is idle.
    swap_flush_deadline: Option<Instant>,
    /// Vim-compatible backup options for this document's save transaction.
    backup_options: BackupOptions,
}

impl Document {
    /// Opens `path`, or starts an empty document when it does not exist.
    ///
    /// Files must be valid UTF-8; other files are rejected rather than
    /// silently converted.
    pub(crate) fn open(path: impl Into<PathBuf>) -> Result<Self, EditorError> {
        let path = path.into();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(source) => return Err(EditorError::Read { path, source }),
        };

        let mut document = Self::from_parts(Some(path), &text);
        document.begin_swap();
        Ok(document)
    }

    /// Starts an empty document with no filename.
    pub(crate) fn untitled() -> Self {
        let mut document = Self::from_parts(None, "");
        document.begin_untitled_swap();
        document
    }

    fn from_parts(path: Option<PathBuf>, text: &str) -> Self {
        Self {
            buffer: Buffer::from_text(text),
            path,
            cursor: LineCol::default(),
            preferred_col: None,
            swap: None,
            swap_owned: false,
            swap_pending_bytes: 0,
            swap_warning: None,
            recovery: None,
            swap_flush_deadline: None,
            backup_options: BackupOptions::default(),
        }
    }

    /// Returns the text buffer.
    pub(crate) fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    /// Returns the cursor.
    pub(crate) fn cursor(&self) -> LineCol {
        self.cursor
    }

    /// Returns whether this document has a filename yet.
    pub(crate) fn is_untitled(&self) -> bool {
        self.path.is_none()
    }

    /// Returns the file name shown in the status row.
    pub(crate) fn display_name(&self) -> Cow<'_, str> {
        match self.path.as_deref().and_then(Path::file_name) {
            Some(name) => name.to_string_lossy(),
            None => match self.path.as_deref() {
                Some(path) => path.to_string_lossy(),
                None => Cow::Borrowed("[No Name]"),
            },
        }
    }

    /// Returns `true` when the buffer has unsaved changes.
    pub(crate) fn is_modified(&self) -> bool {
        self.buffer.is_modified()
    }

    /// Returns the recovery decision blocking normal editing, if any.
    pub(crate) fn recovery(&self) -> Option<&RecoveryInfo> {
        self.recovery.as_ref()
    }

    /// Returns the latest non-fatal swap error for the status row.
    pub(crate) fn swap_warning(&self) -> Option<&str> {
        self.swap_warning.as_deref()
    }

    /// Returns this document's backup options.
    pub(crate) fn backup_options(&self) -> &BackupOptions {
        &self.backup_options
    }

    /// Returns the backup options for `:set` and `:e` option propagation.
    pub(crate) fn backup_options_mut(&mut self) -> &mut BackupOptions {
        &mut self.backup_options
    }

    /// Forces a swap flush for `:preserve`.
    ///
    /// Returns `false` when this session does not own a swap: a pending
    /// recovery decision, a preserved startup swap, or a clean buffer with no
    /// swap must not be overwritten by a force flush.
    pub(crate) fn preserve_swap(&mut self) -> Result<bool, SwapError> {
        if self.recovery.is_some() || !self.swap_owned {
            return Ok(false);
        }
        if self.swap.is_none() && self.buffer.is_modified() {
            self.ensure_swap();
            if !self.swap_owned {
                return Ok(false);
            }
        }
        if self.swap.is_some() {
            self.write_swap()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Marks the current buffer state as saved.
    ///
    /// A successful save removes the swap file: there is no unsaved state
    /// left to recover. The next mutation starts a fresh swap.
    pub(crate) fn mark_saved(&mut self) {
        self.buffer.mark_saved();
        if self.swap_owned {
            self.remove_swap();
        }
        self.swap_pending_bytes = 0;
        self.swap_flush_deadline = None;
    }

    /// Loads the discovered swap into the buffer at its recorded cursor.
    ///
    /// Recovery never writes the target file. The buffer is marked modified
    /// immediately so quitting cannot silently discard the recovered text.
    pub(crate) fn recover_swap(&mut self) -> Result<(), RecoveryError> {
        let Some(info) = self.recovery.as_ref() else {
            return Ok(());
        };
        let RecoveryState::Recoverable { snapshot, .. } = &info.state else {
            return Err(RecoveryError::Unavailable(
                info.problem()
                    .unwrap_or_else(|| "swap cannot be recovered".to_string()),
            ));
        };
        let snapshot = snapshot.clone();

        self.buffer = Buffer::from_text(&snapshot.text);
        self.buffer.mark_modified();
        self.cursor = self
            .clamp_normal_position(snapshot.cursor)
            .unwrap_or_default();
        self.preferred_col = None;
        self.recovery = None;
        self.swap_owned = true;
        self.swap_flush_deadline = None;
        if let Err(error) = self.write_swap() {
            return Err(RecoveryError::Swap(error));
        }
        Ok(())
    }

    /// Opens the original text after a swap prompt.
    ///
    /// With `overwrite` false the discovered swap is left untouched, so the
    /// user can still delete or recover it later. With `overwrite` true the
    /// user chose "edit anyway": the old swap is replaced by a clean swap for
    /// the original contents.
    pub(crate) fn open_original(&mut self, overwrite: bool) -> Result<(), RecoveryError> {
        let Some(info) = self.recovery.as_ref() else {
            return Ok(());
        };
        if !info.is_recoverable() {
            return Err(RecoveryError::Unavailable(
                info.problem()
                    .unwrap_or_else(|| "swap cannot be recovered".to_string()),
            ));
        }

        if overwrite {
            let previous_owned = self.swap_owned;
            self.swap_owned = true;
            if let Err(error) = self.write_swap() {
                self.swap_owned = previous_owned;
                return Err(RecoveryError::Swap(error));
            }
        } else {
            // Keeping the swap means keeping the crashed session's data. This
            // session must not overwrite it on the next edit, so it never
            // becomes owned here.
            self.swap_owned = false;
            self.swap_warning =
                Some("opened original; swap retained, new edits are not swapped".to_string());
        }
        self.recovery = None;
        Ok(())
    }

    /// Deletes the discovered swap and starts a fresh clean swap.
    pub(crate) fn delete_swap(&mut self) -> Result<(), RecoveryError> {
        let Some(swap) = self.swap.take() else {
            self.recovery = None;
            return Ok(());
        };
        if let Err(error) = swap.remove() {
            self.swap = Some(swap);
            return Err(RecoveryError::Swap(error));
        }
        self.recovery = None;
        self.swap_owned = true;
        self.swap_pending_bytes = 0;
        self.swap_flush_deadline = None;
        self.swap_warning = None;
        self.ensure_swap();
        if !self.swap_owned {
            // Another session created the swap between the delete and this
            // refresh. Keep their recoverable data and report it instead.
            return Ok(());
        }
        if let Err(error) = self.write_swap() {
            return Err(RecoveryError::Swap(error));
        }
        Ok(())
    }

    /// Ends a normal editor session.
    ///
    /// Only a swap owned by this session is removed. A swap that is still
    /// waiting for a recovery decision belongs to the crashed session and is
    /// deliberately left on disk.
    pub(crate) fn close_session(&mut self) {
        if self.swap_owned {
            self.remove_swap();
        }
        self.swap_flush_deadline = None;
    }

    /// Creates this session's untitled swap identity without writing it.
    ///
    /// The first deferred flush creates the file, so an untitled session that
    /// never changes its buffer leaves nothing behind in the state directory.
    fn begin_untitled_swap(&mut self) {
        match SwapFile::untitled() {
            Some(swap) => {
                self.swap = Some(swap);
                self.swap_owned = true;
            }
            None => {
                self.swap_warning =
                    Some("swap: cannot create the state swap directory".to_string());
            }
        }
    }

    /// Returns the pending idle swap flush deadline, if a mutation deferred.
    pub(crate) fn swap_flush_deadline(&self) -> Option<Instant> {
        self.swap_flush_deadline
    }

    /// Flushes a deferred swap once `now` reaches its idle deadline.
    ///
    /// Returns `false` when no flush was due. A failed write is stored in
    /// `swap_warning` by [`Document::write_swap`] and returned here so the
    /// caller can report it without interrupting editing.
    pub(crate) fn flush_idle_swap(&mut self, now: Instant) -> Result<bool, SwapError> {
        let Some(deadline) = self.swap_flush_deadline else {
            return Ok(false);
        };
        if now < deadline {
            return Ok(false);
        }
        self.swap_flush_deadline = None;
        if self.recovery.is_some() || !self.swap_owned || !self.buffer.is_modified() {
            return Ok(false);
        }
        if self.swap.is_none() {
            self.ensure_swap();
            if !self.swap_owned {
                return Ok(false);
            }
        }
        self.write_swap()?;
        Ok(true)
    }

    /// Loads the swap of a named file without showing the recovery prompt.
    ///
    /// Returns `None` when the file has no swap, so the caller can report
    /// `E305` instead of silently opening the clean original.
    pub(crate) fn recover_named(
        path: impl Into<PathBuf>,
    ) -> Result<Option<Self>, RecoverDocumentError> {
        let mut document = Self::open(path)?;
        if document.recovery.is_none() {
            document.close_session();
            return Ok(None);
        }
        document.recover_swap()?;
        Ok(Some(document))
    }

    /// Loads the newest recoverable untitled swap from the state directory.
    ///
    /// The recovered buffer has no filename, is marked modified and keeps its
    /// adopted swap, so a second crash still recovers it. Saving requires an
    /// explicit `:w <path>`; `:w` alone reports E32.
    pub(crate) fn recover_untitled() -> Result<Option<Self>, RecoverDocumentError> {
        let Some((swap, snapshot)) = recoverable_untitled()?.into_iter().next() else {
            return Ok(None);
        };
        let mut document = Self::from_parts(None, &snapshot.text);
        document.buffer.mark_modified();
        document.cursor = document
            .clamp_normal_position(snapshot.cursor)
            .unwrap_or_default();
        document.swap = Some(swap);
        document.swap_owned = true;
        document.write_swap()?;
        Ok(Some(document))
    }

    /// Creates the initial swap for a named buffer, or records the discovered
    /// swap as a recovery decision.
    fn begin_swap(&mut self) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let Some(mut swap) = SwapFile::new(&path) else {
            return;
        };

        if swap.discover() {
            let state = match swap.read() {
                Ok(snapshot) if snapshot.path_hash == swap.hash() => {
                    let stale = swap.stale_reason(&snapshot);
                    RecoveryState::Recoverable { snapshot, stale }
                }
                Ok(snapshot) => RecoveryState::Mismatched {
                    expected: swap.hash(),
                    found: snapshot.path_hash,
                },
                Err(error) => RecoveryState::Corrupt(error.to_string()),
            };
            self.recovery = Some(RecoveryInfo {
                swap_path: swap.path().to_path_buf(),
                state,
            });
            self.swap = Some(swap);
            self.swap_owned = false;
            return;
        }

        self.swap = Some(swap);
        self.swap_owned = true;
        // The failure is stored in `swap_warning` and rendered by the editor;
        // opening the buffer still succeeds.
        let _ = self.write_swap();
    }

    /// Creates a new swap identity when the buffer has none.
    ///
    /// If another session created the name while this buffer was open, the
    /// swap is left untouched and the status row reports it rather than
    /// overwriting recoverable data.
    fn ensure_swap(&mut self) {
        if self.swap.is_some() {
            return;
        }
        if self.is_untitled() {
            self.begin_untitled_swap();
            return;
        }
        let Some(path) = self.path.clone() else {
            return;
        };
        let Some(mut swap) = SwapFile::new(&path) else {
            return;
        };
        if swap.discover() {
            self.swap = Some(swap);
            self.swap_owned = false;
            self.swap_warning =
                Some("swap file appeared while the buffer was open; it was left untouched".into());
            return;
        }
        self.swap = Some(swap);
        self.swap_owned = true;
    }

    /// Flushes the swap after a mutation, bounded by the 200-byte incremental
    /// rule for large buffers. Small buffers flush every keystroke so a crash
    /// loses no typed text.
    fn note_swap_change(&mut self, bytes: usize, force: bool) {
        if self.recovery.is_some() {
            return;
        }
        if !self.buffer.is_modified() {
            if self.swap_owned {
                self.remove_swap();
            }
            return;
        }

        if self.swap.is_none() {
            self.ensure_swap();
            if !self.swap_owned {
                return;
            }
        } else if !self.swap_owned {
            // A different session owns this swap. Editing continues, but this
            // session must not overwrite their recoverable text.
            return;
        }

        self.swap_pending_bytes = self.swap_pending_bytes.saturating_add(bytes);
        // A named small buffer flushes on every mutation. An untitled buffer,
        // and a large named buffer, follows the spec's incremental policy: a
        // flush at 200 changed bytes or after four seconds without input. The
        // editor loop wakes on the deadline and calls `flush_idle_swap`.
        let small_named = !self.is_untitled() && self.buffer.len_bytes() <= 16 * 1024;
        if force || small_named || self.swap_pending_bytes >= 200 {
            self.swap_flush_deadline = None;
            // `write_swap` records the failure in `swap_warning`; the edit
            // itself stays applied and the user can keep working.
            let _ = self.write_swap();
        } else {
            self.swap_flush_deadline = Some(Instant::now() + IDLE_SWAP_FLUSH);
        }
    }

    /// Writes the current buffer state through the swap primitive.
    ///
    /// Only an owned swap is written. A discovered or externally-created swap
    /// is not ours to replace until a recovery decision adopts it.
    fn write_swap(&mut self) -> Result<(), SwapError> {
        if !self.swap_owned {
            return Ok(());
        }
        let Some(swap) = self.swap.as_mut() else {
            return Ok(());
        };
        let result = swap.write(
            &self.buffer.to_string(),
            self.cursor,
            self.buffer.is_modified(),
        );
        match &result {
            Ok(()) => {
                self.swap_pending_bytes = 0;
                self.swap_warning = None;
                self.swap_flush_deadline = None;
            }
            Err(error) => {
                self.swap_warning = Some(format!("swap: {error}"));
                self.swap_flush_deadline = None;
            }
        }
        result
    }

    /// Removes the current swap, remembering a failure for the status row.
    fn remove_swap(&mut self) {
        if let Some(swap) = self.swap.take()
            && let Err(error) = swap.remove()
        {
            self.swap_warning = Some(format!("swap: {error}"));
        }
        self.swap_owned = false;
        self.swap_pending_bytes = 0;
        self.swap_flush_deadline = None;
    }

    /// Returns the cursor line's content, excluding its terminator.
    pub(crate) fn line_text(&self) -> String {
        self.buffer.line_text(self.cursor.line).unwrap_or_default()
    }

    /// Returns the number of user-visible lines, matching Vim.
    ///
    /// `Buffer` represents a trailing terminator as an extra empty rope line.
    /// That line is not cursor-addressable: Vim treats `"a\n"` as one line,
    /// not two. An empty buffer and text without a final terminator keep all
    /// their rope lines.
    pub(crate) fn line_count(&self) -> usize {
        let lines = self.buffer.len_lines();
        if lines > 1
            && self
                .buffer
                .line_len_bytes(LineNumber::new(lines - 1))
                .unwrap_or(1)
                == 0
        {
            lines - 1
        } else {
            lines
        }
    }

    /// Writes the whole buffer to its current path using the backup-aware
    /// atomic save transaction.
    pub(crate) fn save(&self) -> Result<SaveOutcome, DocumentSaveError> {
        let Some(path) = self.path.as_deref() else {
            return Err(DocumentSaveError::NoFileName);
        };
        self.write_to(path)
    }

    /// Writes the buffer under `path`, adopts it as the current filename and
    /// returns the backup outcome.
    pub(crate) fn save_as(
        &mut self,
        path: impl Into<PathBuf>,
    ) -> Result<SaveOutcome, DocumentSaveError> {
        let path = path.into();
        let outcome = self.write_to(&path)?;
        if self.swap_owned {
            self.remove_swap();
        } else {
            // A preserved recovery swap belongs to the old path; leave the
            // file alone but stop referring to it from the new path.
            self.swap = None;
            self.swap_owned = false;
            self.swap_flush_deadline = None;
        }
        self.path = Some(path);
        Ok(outcome)
    }

    fn write_to(&self, path: &Path) -> Result<SaveOutcome, DocumentSaveError> {
        let text = self.buffer.to_string();
        vanta_fs::save_with_backup(path, text.as_bytes(), &self.backup_options)
            .map_err(DocumentSaveError::from)
    }

    /// Replaces `range` with `text`, then places the cursor after the inserted
    /// text and marks the document modified.
    ///
    /// The editor only passes replacements whose end has a canonical
    /// [`LineCol`]: its insertions end at a UTF-8 character boundary and never
    /// inside a line terminator.
    pub(crate) fn replace(&mut self, range: Range<BytePos>, text: &str) -> Result<(), TextError> {
        let (edit, before) = self.stage_edit(range, text)?;
        let after = self.buffer.byte_to_line_col(edit.end())?;
        self.commit_edit(edit, before, after);
        Ok(())
    }

    /// Applies `text` over `range` without recording it yet.
    ///
    /// The returned cursor is the position before the change. Range
    /// operations use [`Document::commit_edit`] after choosing their own
    /// post-change cursor.
    fn stage_edit(
        &mut self,
        range: Range<BytePos>,
        text: &str,
    ) -> Result<(TextEdit, LineCol), TextError> {
        let before = self.cursor;
        let edit = self.buffer.apply(range, text)?;
        Ok((edit, before))
    }

    /// Records an applied edit as one undo block and moves the cursor.
    fn commit_edit(&mut self, edit: TextEdit, before: LineCol, after: LineCol) {
        let changed = edit.inserted().len().max(edit.removed().len());
        self.buffer.record_change(edit, before, after);
        self.cursor = after;
        self.preferred_col = None;
        self.note_swap_change(changed, false);
    }

    /// Moves the cursor to `pos` when it is a canonical position.
    pub(crate) fn move_to(&mut self, pos: BytePos) -> Result<(), TextError> {
        self.cursor = self.buffer.byte_to_line_col(pos)?;
        self.preferred_col = None;
        Ok(())
    }

    /// Returns the bytes written for a new line in this buffer.
    fn line_ending_text(&self) -> &'static str {
        match self.buffer.line_ending() {
            LineEnding::Crlf => "\r\n",
            LineEnding::Lf => "\n",
        }
    }

    /// Undoes the most recent change block and restores its cursor.
    ///
    /// Returns `false` when the buffer is already at its oldest change.
    pub(crate) fn undo(&mut self) -> Result<bool, TextError> {
        match self.buffer.undo()? {
            Some(cursor) => {
                self.cursor = self.clamp_normal_position(cursor)?;
                self.preferred_col = None;
                self.note_swap_change(200, true);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Redoes the most recently undone change block and restores its cursor.
    ///
    /// Returns `false` when the buffer is already at its newest change.
    pub(crate) fn redo(&mut self) -> Result<bool, TextError> {
        match self.buffer.redo()? {
            Some(cursor) => {
                // Insert-mode history stores the cursor after the inserted
                // bytes, which can be one cell past the last grapheme. Redo is
                // a Normal-mode command, so restore a valid Normal cursor.
                self.cursor = self.clamp_normal_position(cursor)?;
                self.preferred_col = None;
                self.note_swap_change(200, true);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Forces the next insert to start a new undo block.
    pub(crate) fn break_undo(&mut self) {
        self.buffer.break_undo();
    }

    /// Returns the current line, using the previous line when the cursor sits
    /// on the buffer's synthetic final empty line.
    ///
    /// `Buffer` represents a trailing terminator as a final empty line; Vim
    /// treats that terminator as belonging to the previous line. Linewise
    /// operations follow the Vim view so `dd` and `yy` on the last visible line
    /// do not target the synthetic line.
    fn effective_line(&self, line: LineNumber) -> LineNumber {
        let last = self.line_count().saturating_sub(1);
        if line.get() > last {
            LineNumber::new(last)
        } else {
            line
        }
    }

    /// Moves one grapheme left, wrapping to the previous line at column zero.
    ///
    /// This is the wrapping motion used by Insert and Visual mode. Normal
    /// mode uses [`Document::move_left_in_line`] so it never leaves its line.
    pub(crate) fn move_left(&mut self) {
        if self.cursor.col.get() > 0 {
            self.move_left_in_line();
        } else if self.cursor.line.get() > 0 {
            let line = LineNumber::new(self.cursor.line.get() - 1);
            let column = self.buffer.line_len_bytes(line).unwrap_or(0);
            self.cursor = LineCol {
                line,
                col: ByteCol::new(column),
            };
        }
        self.preferred_col = None;
    }

    /// Moves one grapheme left without leaving the current line.
    pub(crate) fn move_left_in_line(&mut self) {
        if self.cursor.col.get() > 0 {
            let line = self.line_text();
            let previous = prev_grapheme_boundary(&line, self.cursor.col.get()).unwrap_or(0);
            self.cursor.col = ByteCol::new(previous);
        }
        self.preferred_col = None;
    }

    /// Moves one grapheme right, wrapping to the next visible line at the end.
    ///
    /// This is the wrapping motion used by Insert and Visual mode. Normal mode
    /// uses [`Document::move_right_in_line`].
    pub(crate) fn move_right(&mut self) {
        let line = self.line_text();
        if self.cursor.col.get() < line.len() {
            let next = next_grapheme_boundary(&line, self.cursor.col.get()).unwrap_or(line.len());
            self.cursor.col = ByteCol::new(next);
        } else if self.cursor.line.get() + 1 < self.line_count() {
            self.cursor = LineCol {
                line: LineNumber::new(self.cursor.line.get() + 1),
                col: ByteCol::ZERO,
            };
        }
        self.preferred_col = None;
    }

    /// Moves one grapheme right for Normal-mode `a`.
    ///
    /// Unlike [`Document::move_right_in_line`], landing one cell past the last
    /// grapheme is intentional: Insert starts after the current character.
    /// An empty line stays at column zero instead of wrapping.
    pub(crate) fn move_past_cursor(&mut self) {
        let line = self.line_text();
        let column = self.cursor.col.get();
        if column < line.len() {
            let next = next_grapheme_boundary(&line, column).unwrap_or(line.len());
            self.cursor.col = ByteCol::new(next);
        }
        self.preferred_col = None;
    }

    /// Places the cursor at the line's insertion point after its last
    /// grapheme, for Normal-mode `A`.
    pub(crate) fn move_to_line_end(&mut self) {
        self.cursor.col = ByteCol::new(self.line_text().len());
        self.preferred_col = None;
    }

    /// Moves to the first column of the current line, for Normal-mode `0`.
    pub(crate) fn move_to_line_start(&mut self) {
        self.cursor.col = ByteCol::ZERO;
        self.preferred_col = None;
    }

    /// Moves to the start of the last grapheme on the current line, for
    /// Normal-mode `$`.
    ///
    /// The final character, not the insertion point after it, is the furthest
    /// a Normal-mode cursor may go. An empty line stays at column zero.
    pub(crate) fn move_to_last_char(&mut self) {
        let line = self.line_text();
        let column = prev_grapheme_boundary(&line, line.len()).unwrap_or(0);
        self.cursor.col = ByteCol::new(column);
        self.preferred_col = None;
    }

    /// Moves one grapheme right without leaving the line or reaching past the
    /// last grapheme. Used by Normal-mode `l` and Right.
    pub(crate) fn move_right_in_line(&mut self) {
        let line = self.line_text();
        let column = self.cursor.col.get().min(line.len());
        if column < line.len()
            && let Some(next) = next_grapheme_boundary(&line, column)
            && next < line.len()
        {
            self.cursor.col = ByteCol::new(next);
        }
        self.preferred_col = None;
    }

    /// Moves `delta` lines, preserving the desired display column across short
    /// lines.
    ///
    /// The target is clamped to the last cursor-addressable line. In
    /// particular, `j` on the last line of `"a\n"` stays on that line instead
    /// of moving onto the synthetic empty rope line. A short target clamps the
    /// cursor for display, but the next vertical motion still aims at the
    /// original column.
    pub(crate) fn move_vertical(&mut self, delta: isize) {
        if self.preferred_col.is_none() {
            self.preferred_col = Some(self.cursor_display_column());
        }
        let preferred = self.preferred_col.unwrap_or(0);

        let last = self.line_count().saturating_sub(1);
        let current = self.cursor.line.get().min(last);
        let target = (current as isize)
            .saturating_add(delta)
            .clamp(0, last as isize) as usize;
        let line = LineNumber::new(target);
        let text = self.buffer.line_text(line).unwrap_or_default();
        let column = view::normal_column_at_display(&text, preferred);
        self.cursor = LineCol {
            line,
            col: ByteCol::new(column),
        };
    }

    /// Returns the cursor's current display column, measured from the line
    /// start with tabs expanded.
    fn cursor_display_column(&self) -> usize {
        let line = self.line_text();
        display_col_of_byte(&line, self.cursor.col.get(), TABSTOP).get()
    }

    /// Clamps a position to a real Normal-mode character.
    ///
    /// The line is clamped to the last visible line, and a column at the end
    /// of the line moves back to the last grapheme. Insert-mode history and
    /// positional edits use this before restoring a Normal-mode cursor.
    fn clamp_normal_position(&self, position: LineCol) -> Result<LineCol, TextError> {
        let last = self.line_count().saturating_sub(1);
        let line = LineNumber::new(position.line.get().min(last));
        let content_len = self.buffer.line_len_bytes(line)?;
        let mut column = position.col.get().min(content_len);
        if column > 0 && column == content_len {
            let text = self.buffer.line_text(line)?;
            column = prev_grapheme_boundary(&text, column).unwrap_or(0);
        }
        Ok(LineCol {
            line,
            col: ByteCol::new(column),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after the Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "vanta-document-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).expect("create scratch directory");
        directory
    }

    #[test]
    fn swap_that_appears_while_open_is_not_overwritten() {
        let directory = scratch("external-swap");
        let path = directory.join("note.txt");
        fs::write(&path, "seed\n").expect("seed target");

        let mut document = Document::open(&path).expect("open document");
        document.close_session();

        let mut external = SwapFile::new(&path).expect("external swap identity");
        external
            .write("foreign\n", LineCol::default(), false)
            .expect("write external swap");
        let before = fs::read_to_string(external.path()).expect("read external swap");

        document
            .replace(BytePos::ZERO..BytePos::ZERO, "x")
            .expect("edit buffer");

        assert_eq!(
            fs::read_to_string(external.path()).expect("read external swap after edit"),
            before,
            "a swap created by another session must never be overwritten"
        );
        assert_eq!(document.buffer().to_string(), "xseed\n");
        assert!(document.swap_warning().is_some());

        fs::remove_dir_all(&directory).expect("clean scratch directory");
    }

    #[test]
    fn a_large_named_buffer_flushes_at_its_idle_deadline() {
        let directory = scratch("idle");
        let path = directory.join("large.txt");
        fs::write(&path, "x".repeat(20 * 1024)).expect("seed large target");

        let mut document = Document::open(&path).expect("open document");
        document
            .replace(BytePos::ZERO..BytePos::ZERO, "y")
            .expect("edit buffer");

        let deadline = document.swap_flush_deadline().expect("deferred flush");
        let swap = document.swap.as_ref().expect("swap identity");
        assert!(
            !swap
                .read()
                .expect("read initial swap")
                .text
                .starts_with('y'),
            "a sub-200-byte edit must not flush immediately"
        );

        assert!(
            !document
                .flush_idle_swap(deadline - Duration::from_millis(1))
                .expect("early check"),
            "the swap must not flush before its deadline"
        );
        assert!(
            document.flush_idle_swap(deadline).expect("idle flush"),
            "the idle deadline must flush the pending edit"
        );
        let text = document
            .swap
            .as_ref()
            .expect("swap identity")
            .read()
            .expect("read flushed swap")
            .text;
        assert!(text.starts_with('y'), "the idle flush must write the edit");
        assert!(document.swap_flush_deadline().is_none());

        document.close_session();

        fs::remove_dir_all(&directory).expect("clean scratch directory");
    }
}
