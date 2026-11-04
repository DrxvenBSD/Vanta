//! Vanta's editor loop and terminal boundary.
//!
//! The editor owns one internal document (buffer, path, cursor, modified flag
//! and swap lifecycle) plus the minimal Normal/Visual/Insert/Command mode
//! state. It talks to terminals only through [`Terminal`](terminal::Terminal),
//! so the complete open, edit, save, recover and quit loop can be driven
//! headlessly in tests while the binary supplies a crossterm implementation.
//!
//! Text mutation has one path: `Editor::apply_edit` delegates to
//! `Document::replace`, which calls `Buffer::apply`. Frame composition is the
//! read-only child module `editor::render`. Undo history belongs to `Buffer`
//! in `vanta-text` (`PROJECT_SPEC` §5.12); multiple buffers will make the
//! document a set the editor selects from, not another set of editor fields.
//!
//! This is deliberately a small runnable slice: one buffer, one window,
//! character and line editing through the unnamed register, characterwise and
//! linewise Visual mode, literal `/` search with `n`/`N`, cursor and word
//! motions including `0`/`$`, a horizontal viewport, line opening, undo/redo,
//! `:w`, `:q`, `:q!`, `:wq` and `:x`. Regex search, syntax highlighting,
//! configuration and multiple buffers are not part of this slice.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod document;
pub mod editor;
mod swap;
pub mod terminal;
mod undo_file;
mod view;
