# Architecture

Vanta is a Rust workspace with three product crates and a developer task crate.
The current design is a synchronous, single-editor loop. Services such as LSP,
Git, Lua and plugins are specified for later milestones but are not built yet.

## Main parts

```text
main
  -> Editor (modes, buffers, windows, tabs, registers)
       -> Document (one file, cursor, swap lifecycle)
            -> Buffer (rope, undo tree)          vanta-text
            -> vanta-fs (save, backup, encoding)
       -> Frame (read-only snapshot)
            -> Terminal (CrosstermTerminal)      vanta
                 -> Screen (cell diff, damage)
```

## Runtime flow

`main` parses the small CLI, opens an `Editor` (`open`, `untitled` or
`recover`), enters the terminal and calls `Editor::run`.

The loop is:

1. Read the terminal size and keep the cursor visible.
2. Build a `Frame` from the current state.
3. Draw the frame through the `Terminal` trait.
4. Wait for an `InputEvent`; if a swap flush is due, wait with a timeout.
5. Dispatch the event by mode and repeat.

On a normal quit, every open document closes its swap session. On an error or
crash, the swap remains for recovery. The crossterm backend restores the
terminal on normal exit and through a panic hook.

## Editor and documents

`Editor` is the owner of editing state: active mode, command/search input,
active buffer, inactive buffer entries, tab pages, window layout, mode parser,
registers, macros, last change, and search state.

A `Window` owns its cursor, top line and left column. A `TabPage` owns windows
and a focus ID. The active `Document` holds the buffer for the focused window;
inactive documents are kept in `Editor::inactive`. On every handled event the
focused window cursor is loaded into the active `Document`, and after the event
the `Document` cursor is saved back to the window. This is why two splits of
one buffer can keep independent cursors. `Document::sync_cursor_to` is used for
this handoff; it preserves the desired display column.

`Document` is internal to the editor. It owns the `Buffer`, path, cursor,
preferred column, swap file, encoding, backup options and persistent-undo path.
`Editor` exposes the public API used by the binary and tests.

## Input, grammar and editing

Input is normalized at the terminal boundary into `InputEvent` values. Each
mode has its own handler. Normal mode feeds keys into `editor::grammar::Parser`,
which is independent of editor state and returns `Pending`, `Invalid` or a
complete `Command`. Macros and `:normal` use the same parser path.

`Editor` executes a `Command`. Motions resolve to a target through
`document::motion`; operators and Visual ranges resolve through
`document::range`. All mutation goes through `Document` and then `Buffer`.
`Buffer` validates byte and grapheme boundaries, records one undo block per
operation and keeps the saved-state marker. The renderer only reads state.

## Text and files

`vanta-text` is the pure substrate. It uses a rope for storage and newtypes for
byte positions, line numbers, byte columns and display columns. It owns
grapheme-boundary checks, display-width calculation and the undo tree. It has
no I/O, threads, terminal access or unsafe code.

`vanta-fs` owns the durable file lifecycle: BOM-first encoding detection,
UTF-8/UTF-16/UTF-32/latin-1 conversion, streaming reads for large files,
atomic saves, permissions, backups and the write-abort transaction. Saves write
a temporary file, fsync it, replace the target by rename and fsync the parent
directory on Unix. A failed save leaves the previous file intact.

## Persistence and recovery

Swap files live beside named targets or under the state directory for untitled
buffers. They contain text, cursor, modified flag, PID, host and start time;
writes are atomic and owner-restricted. A startup swap can be recovered,
bypassed, replaced or deleted. Recovery never writes the original file.

Persistent undo files live under the state directory, are checksummed, bounded,
versioned and atomically replaced. They are ignored when corrupt or when their
text hash does not match the current file.

## Terminal and rendering

`Terminal` is the seam between the editor and the platform. `CrosstermTerminal`
owns raw mode, the alternate screen, capability detection and key translation.
`Screen` keeps a front buffer of grapheme cells and writes only changed runs;
a one-line viewport move uses a scroll region. `Frame` carries sanitized rows,
cursor position, styles, selection spans and search spans, so tests can assert
the rendered result without parsing raw ANSI.

## Testing seams

The `Terminal` trait lets integration tests drive the editor with a scripted
terminal and inspect frames. Real-binary tests use `portable-pty` for terminal
restoration, resize, scrolling and crash recovery. Property tests cover text
edits, line indexing and undo round-trips. There is no asynchronous service
layer yet; when one is added, services must return results with generations
and must not borrow editor state.

## Boundaries and current limits

The dependency direction is `vanta -> vanta-text` and `vanta -> vanta-fs`;
`vanta-text` and `vanta-fs` are independent. Do not introduce a cycle or move
I/O into the text crate.

The current layout is intentionally smaller than the specification's target
crate map. Do not scaffold future crates or empty service modules. G3-G5 work
(Lua/config, syntax, LSP, Git, terminal jobs, plugins and the differential
corpus) must be added through the boundaries and milestones in
`PROJECT_SPEC.md`.
