# Changelog

All notable changes to Vanta are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## Unreleased

### Added

- Public-release scaffolding: `README.md`, dual `LICENSE-MIT`/`LICENSE-APACHE`
  files, `.gitattributes`, a three-platform CI workflow (format, Clippy with
  warnings denied, workspace tests) and a tag-triggered release workflow that
  builds Windows x86_64, Linux x86_64 and macOS x86_64/arm64 archives with
  SHA-256 checksums. `SECURITY.md` documents private vulnerability reporting.
- Cargo workspace pinned with `rust-toolchain.toml`, shared compile lints and
  a release profile with thin LTO.
- `cargo xtask check|test|fmt`, which runs formatting, Clippy with warnings
  denied and the workspace test suite.
- `vanta-text`: a rope-backed `Buffer` with unit-typed positions, line queries
  that exclude terminators, grapheme-validated edits, CRLF handling, display
  width per grapheme cluster and line-ending detection.
- Property tests for edit/model equivalence, insert/remove round-trips,
  position round-trips, line-range tiling and grapheme boundary validation.
- `vanta-fs`: `save_atomic`, which writes a temporary file in the target
  directory, fsyncs it, renames over the target and fsyncs the directory on
  Unix, leaving the original untouched on failure.
- Vim-compatible backups on the save path: `save_with_backup` writes the
  previous bytes before replacing the target, aborts on a backup failure,
  leaves a usable backup when the write is refused or fails, keeps a `backup`
  file after success, and removes a transient `writebackup` only after the new
  target is durable. `:set` covers `backup`, `writebackup`, `backupdir`
  (comma list, quoted paths) and `backupext`; read-only targets fail with the
  original intact.
- Named-file crash recovery: a checksummed, versioned swap file beside the
  canonical target records text, cursor, modified flag, PID, host and session
  start. It is written atomically with `0600` permissions on Unix, flushed as
  the buffer changes, removed on a clean save/quit, and discovered at startup.
  `E325: ATTENTION` offers recover, open original, edit anyway, delete swap,
  quit or abort; stale writers are reported and mismatched/corrupt swaps are
  refused. A discovered swap is never overwritten until `R`, `E` or `D`;
  `O` retains it even while the original is edited. Recovery marks the buffer
  modified and never writes the target until an explicit `:w`. `:preserve`
  forces a flush. Proven by a real kill/reopen/recover PTY test and headless
  lifecycle regressions.
- Untitled-buffer crash recovery: a no-argument `[No Name]` session gets a
  lazily created `.untitled-<hash>.swp` under the state directory's `swap/`
  subdirectory, which is overridable with `VANTA_STATE_DIR` and is `0700` on
  Unix (files `0600`). Untitled and large named buffers flush at 200 changed
  bytes or four seconds of idle input; an untouched session leaves no file.
  Named swaps fall back to the same directory when the target directory is not
  writable. `vanta -r [file]` and `:recover[!]` load retained swaps directly.
  A recovered untitled buffer is marked modified, plain `:q` still refuses, and
  only `:w <path>` writes it. Proven by a real-binary kill/recover/save PTY
  test and five headless regressions.
- `vanta`: a runnable single-buffer editor with Normal/Insert/Command modes,
  cursor movement, `i`/`a`/`A` insert entry, character insert, backspace, line
  join, atomic save, `:q`/`:w`/`:wq`/`:x`, a panic-safe terminal session and a
  headless `Terminal` trait (`size`, `draw`, `read_event`).
- Linear undo and redo in the editor: `u` restores the previous change block,
  `Ctrl-r` reapplies it, continuous Insert sessions coalesce into one block,
  a new edit after undo discards the redo branch, and undo/redo restore the
  cursor. The `[+]` modified flag is derived from the saved history position,
  so undoing back to the saved content clears it.
- Core Normal-mode editing verbs over one unnamed register: `x`, linewise
  `dd` and `yy`, wordwise `dw`/`db` with `w`/`b` motions, `p`/`P` preserving
  characterwise versus linewise puts, and `o`/`O` followed by Insert. Each
  mutating verb is one undo block; linewise operations preserve CRLF and
  handle a final line without an EOL without adding or losing a separator.
- Characterwise and linewise Visual mode: `v`/`V` start a selection, the
  cursor motions and `w`/`b` extend it, `d`/`x` delete it, `y` yanks it, `c`
  changes it and enters Insert, and `Esc` or the same kind key cancels it.
  Selections resolve to the existing `TextRange` verbs, include whole
  graphemes at both endpoints, work in either direction and across line ends,
  and are exposed as screen-cell spans on `Frame` and drawn in reverse video.
  Each mutating Visual verb is one undo block, and cancelling changes neither
  the buffer nor the modified flag.
- Literal `/pattern` search: the prompt edits and deletes characters live,
  Enter moves to the next grapheme-aligned match and wraps with a visible
  message, `n` and `N` repeat forward and backward, and `Esc` cancels without
  touching the buffer, cursor or modified flag. Matching is case-sensitive,
  has no regex engine and rejects matches that start or end inside a combining
  sequence, ZWJ emoji or CRLF pair. `Frame.search_current` and
  `Frame.search_matches` expose the active and visible matches; the crossterm
  backend underlines all visible matches and reverses the current one.
- Horizontal viewport for lines wider than the terminal: one grapheme-aligned
  offset keeps the cursor visible through tabs and double-width glyphs, shifts
  only when the cursor would leave the screen, and is exposed as
  `Frame.horizontal_offset`. `0` and `$` move to the line start and last
  grapheme, and `j`/`k` preserve the desired display column across short lines.
- Launchable binary: `vanta <file>` opens a file directly, while `vanta`
  with no argument opens an empty `[No Name]` buffer with a centered start
  page: a six-line block VANTA wordmark, subtitle, Nerd Font menu icons with
  right-aligned keys and a warm status hint. Semantic `TextStyle` spans on
  `Frame` let the crossterm backend color the page without putting ANSI in
  model state. The page is display-only, requires a clean empty Normal-mode
  buffer and is never part of the buffer, undo history or a saved file.
  `vanta --help` and `vanta --version` print without entering the TUI.
- Untitled-buffer workflow: `:e <path>` opens a file and refuses to discard a
  modified buffer with `E37`; `:e! <path>` discards it. `:w <path>` saves the
  untitled buffer with the existing atomic writer, adopts the name and accepts
  quoted or space-containing paths. `:w` without a name reports `E32`.
  `:wq <path>` saves as before quitting.
- Release and install path: `cargo build --release -p vanta` produces
  `target/release/vanta.exe`, and `cargo install --path crates/vanta --locked`
  installs `vanta` in the user cargo bin already on `PATH`.
- BOM-first file encoding detection and conversion (`BUF-007`): UTF-8 with or
  without a BOM, UTF-16LE/BE and UTF-32LE/BE with a BOM round-trip, and invalid
  UTF-8 falls back to a byte-preserving latin-1 read with a warning.
  `:set fileencoding=<encoding>` selects the write encoding and fails with `E513`
  plus the offending byte offset when latin-1 cannot represent a character;
  `:set bomb`/`:set nobomb` controls a UTF-8 BOM. Malformed BOM data also falls
  back losslessly instead of being partially decoded.
- Headless editor-loop tests and PTY tests that run the real binary, verify
  alternate-screen restoration on exit and on panic, exercise undo/redo,
  range operators, Visual selections, literal search, CRLF and grapheme edge
  cases, the dirty-flag round trip, horizontal scrolling past the terminal
  width, no-file startup, save-as, and the debug-only
  `VANTA_TEST_PANIC_AFTER_INIT` hook.

### Changed

- Internal structure only: `Buffer` is split into storage, position, measure
  and edit modules; editor state lives in a `Document`, and every text change
  goes through `Editor::apply_edit` -> `Document::replace` -> `Buffer::apply`.
  No user-visible behavior changed.

### Fixed

- `vanta-text` position conversion no longer resolves an out-of-range line
  column into the next line. Columns are content-relative, end of line is the
  boundary before the terminator, and the `\n` byte of a CRLF pair has no
  `LineCol`.
- Insert-mode `Esc` keeps the cursor on the current line; only an explicit
  `h` or Left wraps to the previous line.
- Normal-mode `h`/`l` stay on their line and stop on a real grapheme; they no
  longer move onto the line terminator or jump to a neighbouring line. `j`/`k`
  stop on the last visible line instead of addressing the synthetic empty line
  after a trailing terminator. Normal `w` crosses to the next line's first
  word instead of dead-ending at the end of the current line.
- Opening a line with `o`/`O` on an empty buffer or after a final line without
  an EOL now writes the second terminator, so the opened line survives save
  and reload; `dd` after `o` on an empty buffer deletes the opened line rather
  than the line above it.
- Undo and redo clamp a restored Insert-mode cursor to a real Normal-mode
  character, so redo at end of line no longer leaves the cursor one cell past
  the last character.

### Notes

- `rust-toolchain.toml` pins 1.94.0 because that is the installed compiler;
  PROJECT_SPEC §2.9 names 1.99.1. Align them once 1.99.1 is available.
- Undo is linear, not the full undo tree: `g-`/`g+`, `U`, `:earlier`/`:later`,
  `:undojoin`, `undolevels`, persistent undo and branch preservation are not
  implemented. Registers are limited to the unnamed register; counts, named
  registers, text objects, dot-repeat, macros, blockwise/rectangular Visual
  mode, Replace/Select modes and additional motions are not implemented.
  Search is literal and the `/` prompt always searches forward: there is no
  regex, `?`, `:s`, search offsets or incremental matching while typing.
  `n`/`N` repeat both directions and wrap. The editor is single-buffer and
  single-window: there are no splits, tabs, buffer commands, marks, jumplist
  or quickfix. Encoding detection is BOM-first UTF-8/UTF-16/UTF-32 with a
  latin-1 fallback; `fileencodings` is not configurable. There is no
  configuration file, Lua runtime, syntax highlighting, LSP, Git integration,
  picker, completion,
  embedded terminal or job runner. Atomic save does not yet write through
  symlinks. Swap recovery covers the open buffer rather than a multi-buffer
  swap set, and the `directory` option is absent.
- The workspace version is `0.1.0` and this is a preview. A `1.0` release waits
  for the P0/P1 requirements and gates G1-G5 in `PROJECT_SPEC.md` section 10.
