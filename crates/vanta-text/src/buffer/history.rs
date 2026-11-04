//! Buffer-owned undo tree.
//!
//! Each node describes the state after one change block. Undo moves to the
//! parent; redo follows the most recently visited child. Editing from an older
//! state creates a new branch instead of truncating the old one. The tree is
//! owned by [`Buffer`] because `PROJECT_SPEC` §5.12 assigns undo to the buffer.
//!
//! Persistence lives outside this crate: `Buffer::undo_snapshot` and
//! `Buffer::restore_undo` expose a validated value representation that the
//! editor serializes to the state directory. That keeps filesystem I/O out of
//! `vanta-text`.

use std::collections::HashSet;

use super::{Buffer, TextEdit};
use crate::error::TextError;
use crate::units::{BytePos, LineCol, LineNumber};

/// The default maximum number of retained undo states per buffer.
const DEFAULT_MAX_NODES: usize = 1000;
/// The default undo memory ceiling per buffer.
const DEFAULT_MAX_BYTES: usize = 64 * 1024 * 1024;

/// The kind of command that created an undo block.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UndoKind {
    /// An Insert-mode session or a change command.
    #[default]
    Insert,
    /// A delete or cut.
    Delete,
    /// A replacement that is not a plain insertion.
    Change,
    /// An indentation command.
    Indent,
    /// A put or paste.
    Paste,
    /// A rectangular Visual Block operation.
    Block,
    /// A filter or external command.
    Filter,
}

/// One edit in a serializable undo block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UndoEdit {
    /// Byte offset where the replacement began.
    pub start: usize,
    /// Exact bytes removed from the buffer.
    pub removed: String,
    /// Exact bytes inserted into the buffer.
    pub inserted: String,
}

/// One state in a serializable undo tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UndoNode {
    /// Parent state, or `None` for the root.
    pub parent: Option<usize>,
    /// Edits that transform the parent into this state, in application order.
    pub edits: Vec<UndoEdit>,
    /// Cursor before the first edit.
    pub cursor_before: LineCol,
    /// Cursor after the last edit.
    pub cursor_after: LineCol,
    /// Monotonic sequence number used by `g-` and `g+`.
    pub sequence: u64,
    /// Unix timestamp in milliseconds when the block was recorded.
    pub timestamp: u64,
    /// Command category shown by undo diagnostics.
    pub kind: UndoKind,
    /// Most recently selected child, used by redo.
    pub last_child: Option<usize>,
}

/// A complete, validated undo tree that can be persisted and restored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UndoSnapshot {
    /// Format version understood by the persistence layer.
    pub version: u32,
    /// The tree states; index 0 is always the root.
    pub nodes: Vec<UndoNode>,
    /// The state currently applied to the buffer.
    pub current: usize,
    /// The state that matches the file on disk, if reachable.
    pub saved: Option<usize>,
}

impl UndoSnapshot {
    /// Returns the number of non-root states.
    pub fn len(&self) -> usize {
        self.nodes.len().saturating_sub(1)
    }

    /// Returns `true` when the tree contains no change states.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Clone, Debug)]
struct Node {
    parent: Option<usize>,
    edits: Vec<TextEdit>,
    cursor_before: LineCol,
    cursor_after: LineCol,
    sequence: u64,
    timestamp: u64,
    kind: UndoKind,
    children: Vec<usize>,
    last_child: Option<usize>,
}

impl Node {
    fn root() -> Self {
        Self {
            parent: None,
            edits: Vec::new(),
            cursor_before: LineCol::default(),
            cursor_after: LineCol::default(),
            sequence: 0,
            timestamp: 0,
            kind: UndoKind::Insert,
            children: Vec::new(),
            last_child: None,
        }
    }

    fn memory_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self
                .edits
                .iter()
                .map(|edit| edit.removed().len() + edit.inserted().len())
                .sum::<usize>()
    }
}

/// The undo tree and saved-state marker owned by one buffer.
#[derive(Clone, Debug)]
pub(super) struct History {
    nodes: Vec<Node>,
    current: usize,
    saved: Option<usize>,
    join_next: bool,
    sequence: u64,
    memory_bytes: usize,
    max_nodes: usize,
    max_bytes: usize,
    last_line: Option<(LineNumber, String)>,
}

impl History {
    /// Creates a clean, empty tree for a freshly loaded buffer.
    pub(super) fn new() -> Self {
        let root = Node::root();
        let memory_bytes = root.memory_bytes();
        Self {
            nodes: vec![root],
            current: 0,
            saved: Some(0),
            join_next: true,
            sequence: 0,
            memory_bytes,
            max_nodes: DEFAULT_MAX_NODES,
            max_bytes: DEFAULT_MAX_BYTES,
            last_line: None,
        }
    }

    /// Restores a validated snapshot, or returns `false` for an inconsistent
    /// tree without changing the current history.
    pub(super) fn from_snapshot(snapshot: &UndoSnapshot) -> Option<Self> {
        if snapshot.version != 1 || snapshot.nodes.is_empty() {
            return None;
        }
        if snapshot.current >= snapshot.nodes.len()
            || snapshot
                .saved
                .is_some_and(|saved| saved >= snapshot.nodes.len())
        {
            return None;
        }

        let mut nodes = Vec::with_capacity(snapshot.nodes.len());
        for (index, entry) in snapshot.nodes.iter().enumerate() {
            if (index == 0) != entry.parent.is_none() {
                return None;
            }
            if entry.parent.is_some_and(|parent| parent >= index) {
                return None;
            }
            let edits = entry
                .edits
                .iter()
                .map(|edit| {
                    TextEdit::from_parts(
                        BytePos::new(edit.start),
                        edit.removed.clone(),
                        edit.inserted.clone(),
                    )
                })
                .collect::<Vec<_>>();
            nodes.push(Node {
                parent: entry.parent,
                edits,
                cursor_before: entry.cursor_before,
                cursor_after: entry.cursor_after,
                sequence: entry.sequence,
                timestamp: entry.timestamp,
                kind: entry.kind,
                children: Vec::new(),
                last_child: entry.last_child,
            });
        }
        for index in 1..nodes.len() {
            let parent = nodes[index]
                .parent
                .expect("every non-root node was checked to have a parent");
            nodes[parent].children.push(index);
        }
        for node in &nodes {
            if let Some(child) = node.last_child
                && !node.children.contains(&child)
            {
                return None;
            }
        }
        let memory_bytes = nodes.iter().map(Node::memory_bytes).sum();
        let sequence = snapshot
            .nodes
            .iter()
            .map(|node| node.sequence)
            .max()
            .unwrap_or(0);
        Some(Self {
            nodes,
            current: snapshot.current,
            saved: snapshot.saved,
            join_next: true,
            sequence,
            memory_bytes,
            max_nodes: DEFAULT_MAX_NODES,
            max_bytes: DEFAULT_MAX_BYTES,
            last_line: None,
        })
    }

    /// Returns a persistable copy of this tree.
    pub(super) fn snapshot(&self) -> UndoSnapshot {
        UndoSnapshot {
            version: 1,
            nodes: self
                .nodes
                .iter()
                .map(|node| UndoNode {
                    parent: node.parent,
                    edits: node
                        .edits
                        .iter()
                        .map(|edit| UndoEdit {
                            start: edit.start().get(),
                            removed: edit.removed().to_string(),
                            inserted: edit.inserted().to_string(),
                        })
                        .collect(),
                    cursor_before: node.cursor_before,
                    cursor_after: node.cursor_after,
                    sequence: node.sequence,
                    timestamp: node.timestamp,
                    kind: node.kind,
                    last_child: node.last_child,
                })
                .collect(),
            current: self.current,
            saved: self.saved,
        }
    }

    /// Records an edit, joining the current block when the cursor is
    /// continuous and nothing forced a boundary.
    fn record(
        &mut self,
        edit: TextEdit,
        cursor_before: LineCol,
        cursor_after: LineCol,
        kind: UndoKind,
        now_millis: u64,
    ) {
        let can_join = !self.join_next
            && self.current != 0
            && self.saved != Some(self.current)
            && self.nodes[self.current].cursor_after == cursor_before
            && self.nodes[self.current].kind == kind;

        if can_join {
            self.memory_bytes += edit.removed().len() + edit.inserted().len();
            self.nodes[self.current].edits.push(edit);
            self.nodes[self.current].cursor_after = cursor_after;
            self.nodes[self.current].timestamp = now_millis;
            self.join_next = false;
            self.prune();
            return;
        }

        self.sequence = self.sequence.saturating_add(1);
        let parent = self.current;
        let index = self.nodes.len();
        let node = Node {
            parent: Some(parent),
            edits: vec![edit],
            cursor_before,
            cursor_after,
            sequence: self.sequence,
            timestamp: now_millis,
            kind,
            children: Vec::new(),
            last_child: None,
        };
        self.memory_bytes += node.memory_bytes();
        self.nodes.push(node);
        self.nodes[parent].children.push(index);
        self.nodes[parent].last_child = Some(index);
        self.current = index;
        self.join_next = false;
        self.prune();
    }

    fn undo_candidate(&self) -> Option<usize> {
        self.nodes[self.current].parent
    }

    fn redo_candidate(&self) -> Option<usize> {
        let node = &self.nodes[self.current];
        node.last_child.or_else(|| node.children.last().copied())
    }

    fn commit_undo(&mut self, parent: usize) {
        self.current = parent;
        self.join_next = true;
    }

    fn commit_redo(&mut self, child: usize) {
        self.current = child;
        self.join_next = true;
    }

    fn mark_saved(&mut self) {
        self.saved = Some(self.current);
        self.join_next = true;
    }

    fn mark_modified(&mut self) {
        self.saved = None;
        self.join_next = true;
    }

    fn is_modified(&self) -> bool {
        self.saved != Some(self.current)
    }

    /// Moves to the state immediately before the current one in time order.
    fn earlier(&mut self) -> Option<(Vec<usize>, Vec<usize>, LineCol)> {
        let current_sequence = self.nodes[self.current].sequence;
        let target = self
            .nodes
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(_, node)| node.sequence < current_sequence)
            .max_by_key(|(_, node)| node.sequence)
            .map(|(index, _)| index)
            .or(Some(0))?;
        self.travel(target)
    }

    /// Moves to the next state in time order.
    fn later(&mut self) -> Option<(Vec<usize>, Vec<usize>, LineCol)> {
        let current_sequence = self.nodes[self.current].sequence;
        let target = self
            .nodes
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(_, node)| node.sequence > current_sequence)
            .min_by_key(|(_, node)| node.sequence)
            .map(|(index, _)| index)?;
        self.travel(target)
    }

    /// Returns the nodes that must be undone and reapplied to move between
    /// two arbitrary states in the tree.
    fn travel(&mut self, target: usize) -> Option<(Vec<usize>, Vec<usize>, LineCol)> {
        if target == self.current {
            return None;
        }
        let mut current_path = Vec::new();
        let mut cursor = Some(self.current);
        while let Some(index) = cursor {
            current_path.push(index);
            cursor = self.nodes[index].parent;
        }
        let mut target_path = Vec::new();
        cursor = Some(target);
        while let Some(index) = cursor {
            target_path.push(index);
            cursor = self.nodes[index].parent;
        }

        let mut common = 0;
        for (offset, (left, right)) in current_path.iter().zip(&target_path).enumerate() {
            if left != right {
                break;
            }
            common = offset;
        }
        let undo = current_path[..current_path.len().saturating_sub(common + 1)].to_vec();
        let redo = target_path[..target_path.len().saturating_sub(common + 1)]
            .iter()
            .rev()
            .copied()
            .collect::<Vec<_>>();
        let cursor = self.nodes[target].cursor_after;
        self.current = target;
        self.join_next = true;
        Some((undo, redo, cursor))
    }

    fn set_last_line(&mut self, line: LineNumber, text: String) {
        self.last_line = Some((line, text));
    }

    fn take_last_line(&mut self) -> Option<(LineNumber, String)> {
        self.last_line.clone()
    }

    fn clear_last_line(&mut self) {
        self.last_line = None;
    }

    /// Enforces the count and byte caps by removing the oldest unprotected
    /// leaf. The current path and the saved path are never removed.
    fn prune(&mut self) {
        while self.nodes.len() > self.max_nodes || self.memory_bytes > self.max_bytes {
            let protected = self.protected_nodes();
            let candidate = self
                .nodes
                .iter()
                .enumerate()
                .skip(1)
                .filter(|(index, node)| node.children.is_empty() && !protected.contains(index))
                .min_by_key(|(_, node)| node.sequence)
                .map(|(index, _)| index);
            let Some(index) = candidate else {
                break;
            };
            self.remove_leaf(index);
        }
    }

    fn protected_nodes(&self) -> HashSet<usize> {
        let mut protected = HashSet::new();
        protected.insert(0);
        let mut cursor = Some(self.current);
        while let Some(index) = cursor {
            protected.insert(index);
            cursor = self.nodes[index].parent;
        }
        let mut cursor = self.saved;
        while let Some(index) = cursor {
            protected.insert(index);
            cursor = self.nodes[index].parent;
        }
        protected
    }

    fn remove_leaf(&mut self, index: usize) {
        if index == 0 || index >= self.nodes.len() || !self.nodes[index].children.is_empty() {
            return;
        }
        self.memory_bytes = self
            .memory_bytes
            .saturating_sub(self.nodes[index].memory_bytes());
        self.nodes.remove(index);
        for node in &mut self.nodes {
            node.parent = node.parent.and_then(|parent| {
                if parent == index {
                    None
                } else {
                    Some(parent.saturating_sub(usize::from(parent > index)))
                }
            });
            for child in &mut node.children {
                if *child > index {
                    *child -= 1;
                }
            }
            node.children.retain(|child| *child != index);
            node.last_child = node.last_child.and_then(|child| {
                if child == index {
                    node.children.last().copied()
                } else {
                    Some(child.saturating_sub(usize::from(child > index)))
                }
            });
        }
        if self.current > index {
            self.current -= 1;
        }
        if self.saved.is_some_and(|saved| saved > index) {
            self.saved = self.saved.map(|saved| saved - 1);
        }
    }

    #[cfg(test)]
    fn current_sequence(&self) -> u64 {
        self.nodes[self.current].sequence
    }
}

impl Buffer {
    /// Records an edit that was already applied through [`Buffer::apply`].
    ///
    /// The editor calls this immediately after applying an edit. Edits with a
    /// continuous cursor join the current block; [`Buffer::break_undo`], a
    /// save, undo/redo or a cursor discontinuity starts a new block. This
    /// method does not change the text.
    pub fn record_change(&mut self, edit: TextEdit, cursor_before: LineCol, cursor_after: LineCol) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
            .unwrap_or(0);
        self.history
            .record(edit, cursor_before, cursor_after, UndoKind::Insert, now);
    }

    /// Records an edit with an explicit command category.
    pub fn record_change_kind(
        &mut self,
        edit: TextEdit,
        cursor_before: LineCol,
        cursor_after: LineCol,
        kind: UndoKind,
    ) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
            .unwrap_or(0);
        self.history
            .record(edit, cursor_before, cursor_after, kind, now);
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

    /// Returns a persistable snapshot of the complete undo tree.
    pub fn undo_snapshot(&self) -> UndoSnapshot {
        self.history.snapshot()
    }

    /// Restores a previously persisted tree.
    ///
    /// Returns `false` and leaves the current history untouched when the
    /// snapshot is malformed or internally inconsistent.
    pub fn restore_undo(&mut self, snapshot: &UndoSnapshot) -> bool {
        let Some(history) = History::from_snapshot(snapshot) else {
            return false;
        };
        self.history = history;
        true
    }

    /// Returns the number of retained change states.
    pub fn undo_len(&self) -> usize {
        self.history.node_count_public()
    }

    /// Returns the sequence number of the current undo state.
    pub fn undo_sequence(&self) -> u64 {
        self.history.nodes[self.history.current].sequence
    }

    /// Undoes the most recent change block.
    ///
    /// Returns the cursor position recorded before the block, or `None` when
    /// there is nothing to undo. The inverse edits are applied without
    /// recording new history.
    pub fn undo(&mut self) -> Result<Option<LineCol>, TextError> {
        let Some(target) = self.history.undo_candidate() else {
            return Ok(None);
        };
        let change = self.history.nodes[self.history.current].clone();
        for edit in change.edits.iter().rev() {
            self.apply(edit.start()..edit.end(), edit.removed())?;
        }
        self.history.commit_undo(target);
        Ok(Some(change.cursor_before))
    }

    /// Redoes the most recently undone change block.
    ///
    /// Returns the cursor position recorded after the block, or `None` when
    /// there is nothing to redo. The forward edits are applied without
    /// recording new history.
    pub fn redo(&mut self) -> Result<Option<LineCol>, TextError> {
        let Some(child) = self.history.redo_candidate() else {
            return Ok(None);
        };
        let change = self.history.nodes[child].clone();
        for edit in &change.edits {
            let end = BytePos::new(edit.start().get() + edit.removed().len());
            self.apply(edit.start()..end, edit.inserted())?;
        }
        self.history.commit_redo(child);
        Ok(Some(change.cursor_after))
    }

    /// Moves to the previous undo state in time order (`g-`).
    pub fn undo_earlier(&mut self) -> Result<Option<LineCol>, TextError> {
        let Some((undo, redo, cursor)) = self.history.earlier() else {
            return Ok(None);
        };
        for index in undo {
            let node = self.history.nodes[index].clone();
            for edit in node.edits.iter().rev() {
                self.apply(edit.start()..edit.end(), edit.removed())?;
            }
        }
        for index in redo {
            let node = self.history.nodes[index].clone();
            for edit in &node.edits {
                let end = BytePos::new(edit.start().get() + edit.removed().len());
                self.apply(edit.start()..end, edit.inserted())?;
            }
        }
        Ok(Some(cursor))
    }

    /// Moves to the next undo state in time order (`g+`).
    pub fn undo_later(&mut self) -> Result<Option<LineCol>, TextError> {
        let Some((undo, redo, cursor)) = self.history.later() else {
            return Ok(None);
        };
        for index in undo {
            let node = self.history.nodes[index].clone();
            for edit in node.edits.iter().rev() {
                self.apply(edit.start()..edit.end(), edit.removed())?;
            }
        }
        for index in redo {
            let node = self.history.nodes[index].clone();
            for edit in &node.edits {
                let end = BytePos::new(edit.start().get() + edit.removed().len());
                self.apply(edit.start()..end, edit.inserted())?;
            }
        }
        Ok(Some(cursor))
    }

    /// Remembers the current line before `U` restores it.
    pub fn remember_last_line(&mut self, line: LineNumber, text: String) {
        self.history.set_last_line(line, text);
    }

    /// Takes and clears the last changed-line snapshot.
    pub fn take_last_line(&mut self) -> Option<(LineNumber, String)> {
        self.history.take_last_line()
    }

    /// Clears the `U` snapshot.
    pub fn clear_last_line(&mut self) {
        self.history.clear_last_line();
    }
}

impl History {
    fn node_count_public(&self) -> usize {
        self.nodes.len() - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ByteCol, LineNumber};

    fn edit(buffer: &mut Buffer, start: usize, removed: &str, inserted: &str) -> TextEdit {
        let edit = buffer
            .apply(
                BytePos::new(start)..BytePos::new(start + removed.len()),
                inserted,
            )
            .expect("test edit is valid");
        buffer.record_change(
            edit.clone(),
            LineCol {
                line: LineNumber::new(0),
                col: ByteCol::new(start),
            },
            LineCol {
                line: LineNumber::new(0),
                col: ByteCol::new(start + inserted.len()),
            },
        );
        edit
    }

    #[test]
    fn branch_preserves_the_abandoned_redo_path() {
        let mut buffer = Buffer::from_text("a");
        edit(&mut buffer, 1, "", "b");
        buffer.mark_saved();
        edit(&mut buffer, 2, "", "c");

        assert!(buffer.undo().expect("undo").is_some());
        assert_eq!(buffer.to_string(), "ab");
        assert!(buffer.undo().expect("undo again").is_some());
        assert_eq!(buffer.to_string(), "a");

        edit(&mut buffer, 0, "a", "z");
        assert_eq!(buffer.to_string(), "z");

        buffer.undo().expect("undo branch");
        assert_eq!(buffer.to_string(), "a");
        buffer.redo().expect("redo branch");
        assert_eq!(buffer.to_string(), "z");
        assert_eq!(buffer.undo_len(), 3, "both branches remain retained");
    }

    #[test]
    fn redo_follows_the_most_recently_created_branch() {
        let mut buffer = Buffer::from_text("a");
        edit(&mut buffer, 0, "a", "b");
        assert!(buffer.undo().expect("undo").is_some());
        edit(&mut buffer, 0, "a", "c");
        buffer.undo().expect("undo new branch");
        buffer.redo().expect("redo new branch");
        assert_eq!(buffer.to_string(), "c");
    }

    #[test]
    fn snapshot_round_trip_preserves_branches_and_saved_state() {
        let mut buffer = Buffer::from_text("a");
        edit(&mut buffer, 0, "a", "b");
        buffer.mark_saved();
        edit(&mut buffer, 1, "", "c");
        buffer.undo().expect("undo");
        edit(&mut buffer, 1, "", "d");
        let snapshot = buffer.undo_snapshot();
        let text = buffer.to_string();

        let mut restored = Buffer::from_text(&text);
        assert!(restored.restore_undo(&snapshot));
        assert_eq!(restored.undo_snapshot(), snapshot);
        assert_eq!(restored.undo_len(), 3);
        assert!(restored.is_modified());
    }

    #[test]
    fn malformed_snapshot_is_rejected_without_replacing_history() {
        let mut buffer = Buffer::from_text("a");
        edit(&mut buffer, 1, "", "b");
        let mut snapshot = buffer.undo_snapshot();
        snapshot.current = 99;
        assert!(!buffer.restore_undo(&snapshot));
        assert_eq!(buffer.to_string(), "ab");
    }

    #[test]
    fn time_order_helpers_visit_other_branches() {
        let mut buffer = Buffer::from_text("a");
        edit(&mut buffer, 0, "a", "b");
        let first = buffer.undo_sequence();
        buffer.undo().expect("undo first");
        edit(&mut buffer, 0, "a", "c");
        let second = buffer.undo_sequence();
        assert!(second > first);
        buffer.undo_earlier().expect("g-");
        assert_eq!(buffer.to_string(), "b");
        buffer.undo_earlier().expect("g- to root");
        assert_eq!(buffer.to_string(), "a");
        assert!(
            buffer
                .undo_earlier()
                .expect("root is not earlier than root")
                .is_none()
        );
    }

    #[test]
    fn undo_memory_cap_prunes_abandoned_branches_but_keeps_the_current_path() {
        let mut buffer = Buffer::from_text("x");
        buffer.history.max_nodes = 4;
        for _ in 0..10 {
            edit(&mut buffer, 0, "x", "a");
            buffer.break_undo();
            buffer.undo().expect("undo branch");
            edit(&mut buffer, 0, "x", "b");
            buffer.break_undo();
            buffer.undo().expect("undo branch");
        }
        assert!(buffer.undo_len() <= 4);
        assert_eq!(buffer.to_string(), "x");
    }

    #[test]
    fn joining_uses_the_command_kind() {
        let mut buffer = Buffer::from_text("");
        let first = buffer
            .apply(BytePos::ZERO..BytePos::ZERO, "a")
            .expect("edit");
        buffer.record_change_kind(
            first,
            LineCol::default(),
            LineCol {
                line: LineNumber::new(0),
                col: ByteCol::new(1),
            },
            UndoKind::Insert,
        );
        let second = buffer
            .apply(BytePos::new(1)..BytePos::new(1), "b")
            .expect("edit");
        buffer.record_change_kind(
            second,
            LineCol {
                line: LineNumber::new(0),
                col: ByteCol::new(1),
            },
            LineCol {
                line: LineNumber::new(0),
                col: ByteCol::new(2),
            },
            UndoKind::Delete,
        );
        assert_eq!(buffer.undo_len(), 2);
    }

    #[test]
    fn current_sequence_is_monotonic() {
        let mut buffer = Buffer::from_text("");
        edit(&mut buffer, 0, "", "a");
        buffer.break_undo();
        edit(&mut buffer, 1, "", "b");
        let snapshot = buffer.undo_snapshot();
        assert_eq!(snapshot.nodes[2].sequence, 2);
        assert_eq!(buffer.history.current_sequence(), 2);
    }
}
