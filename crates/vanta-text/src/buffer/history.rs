//! Buffer-owned undo and redo history.
//!
//! History is a linear list of [`Change`] blocks. Each block groups the
//! [`TextEdit`]s from one continuous edit session and stores the cursor before
//! and after the block. Undo applies the inverse edits in reverse order; redo
//! applies them forward again.
//!
//! The history is owned by [`Buffer`] because `PROJECT_SPEC` §5.12 assigns the
//! undo tree to the buffer. The editor only supplies cursor positions through
//! [`Buffer::record_change`]; it never builds edit records itself.

use super::{Buffer, TextEdit};
use crate::error::TextError;
use crate::units::{BytePos, LineCol};

/// One undo block: one or more edits plus the cursor on both sides.
///
/// The future undo tree adds the block's sequence number, timestamp and
/// command kind here; none of them has a consumer until branching and
/// diagnostics exist.
#[derive(Clone, Debug)]
struct Change {
    edits: Vec<TextEdit>,
    cursor_before: LineCol,
    cursor_after: LineCol,
}

/// The undo/redo state owned by a buffer.
#[derive(Clone, Debug)]
pub(super) struct History {
    changes: Vec<Change>,
    applied: usize,
    saved: Option<usize>,
    join_next: bool,
}

impl History {
    /// Creates a clean, empty history for a freshly loaded buffer.
    pub(super) fn new() -> Self {
        Self {
            changes: Vec::new(),
            applied: 0,
            saved: Some(0),
            join_next: true,
        }
    }

    /// Records an edit, truncating any redo branch and joining the current
    /// session only when the cursor is continuous and nothing forced a
    /// boundary.
    ///
    /// One Insert session may type, split lines and backspace, and remains one
    /// undo block (`PROJECT_SPEC` §5.6).
    fn record(&mut self, edit: TextEdit, cursor_before: LineCol, cursor_after: LineCol) {
        if self.applied < self.changes.len() {
            self.changes.truncate(self.applied);
            // A saved state that lived on the discarded redo path can no
            // longer be reached, so it is no longer a clean marker.
            if self.saved.is_some_and(|saved| saved > self.applied) {
                self.saved = None;
            }
            self.join_next = true;
        }

        let can_join = !self.join_next
            && self.applied == self.changes.len()
            && self.applied > 0
            && self.saved != Some(self.applied)
            && self
                .changes
                .last()
                .is_some_and(|change| change.cursor_after == cursor_before);

        if can_join {
            let change = self
                .changes
                .last_mut()
                .expect("the applied count proves a previous block exists");
            change.edits.push(edit);
            change.cursor_after = cursor_after;
        } else {
            self.changes.push(Change {
                edits: vec![edit],
                cursor_before,
                cursor_after,
            });
            self.applied += 1;
        }

        self.join_next = false;
    }

    fn undo_candidate(&self) -> Option<Change> {
        self.applied
            .checked_sub(1)
            .map(|index| self.changes[index].clone())
    }

    fn redo_candidate(&self) -> Option<Change> {
        self.changes.get(self.applied).cloned()
    }

    fn commit_undo(&mut self) {
        self.applied -= 1;
        self.join_next = true;
    }

    fn commit_redo(&mut self) {
        self.applied += 1;
        self.join_next = true;
    }

    fn mark_saved(&mut self) {
        self.saved = Some(self.applied);
        self.join_next = true;
    }

    /// Marks the current state as changed without inventing a text edit.
    ///
    /// Recovery needs this because loading a swap replaces the buffer with
    /// text that was never recorded in this buffer's history; the user must
    /// still see `[+]` until they write it.
    fn mark_modified(&mut self) {
        self.saved = None;
        self.join_next = true;
    }

    fn is_modified(&self) -> bool {
        self.saved != Some(self.applied)
    }
}

impl Buffer {
    /// Records an edit that was already applied through [`Buffer::apply`].
    ///
    /// The editor calls this immediately after applying an edit. Edits with a
    /// continuous cursor join the current session; [`Buffer::break_undo`], a
    /// save, undo/redo or a cursor discontinuity starts a new block. This
    /// method does not change the text.
    pub fn record_change(&mut self, edit: TextEdit, cursor_before: LineCol, cursor_after: LineCol) {
        self.history.record(edit, cursor_before, cursor_after);
    }

    /// Forces the next recorded edit to start a new undo block.
    pub fn break_undo(&mut self) {
        self.history.join_next = true;
    }

    /// Marks the current buffer state as the saved state.
    ///
    /// Undoing back to this state clears the modified flag again.
    pub fn mark_saved(&mut self) {
        self.history.mark_saved();
    }

    /// Marks the current state as modified without recording a text edit.
    ///
    /// This is used when recovery loads text from a swap file. The recovered
    /// text belongs to the saved file on disk only after the user writes it,
    /// so the buffer must report `[+]` immediately.
    pub fn mark_modified(&mut self) {
        self.history.mark_modified();
    }

    /// Returns `true` when the current state differs from the last saved state.
    pub fn is_modified(&self) -> bool {
        self.history.is_modified()
    }

    /// Undoes the most recent change block.
    ///
    /// Returns the cursor position recorded before the block, or `None` when
    /// there is nothing to undo. The inverse edits are applied without
    /// recording new history.
    pub fn undo(&mut self) -> Result<Option<LineCol>, TextError> {
        let Some(change) = self.history.undo_candidate() else {
            return Ok(None);
        };

        for edit in change.edits.iter().rev() {
            self.apply(edit.start()..edit.end(), edit.removed())?;
        }

        self.history.commit_undo();
        Ok(Some(change.cursor_before))
    }

    /// Redoes the most recently undone change block.
    ///
    /// Returns the cursor position recorded after the block, or `None` when
    /// there is nothing to redo. The forward edits are applied without
    /// recording new history.
    pub fn redo(&mut self) -> Result<Option<LineCol>, TextError> {
        let Some(change) = self.history.redo_candidate() else {
            return Ok(None);
        };

        for edit in &change.edits {
            let end = BytePos::new(edit.start().get() + edit.removed().len());
            self.apply(edit.start()..end, edit.inserted())?;
        }

        self.history.commit_redo();
        Ok(Some(change.cursor_after))
    }
}
