# Changelog

All notable changes to Vanta are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## Unreleased

### Changed

- Version metadata moved to `0.2.0-alpha.1` for the G2 editor-surface alpha.

### Fixed

- The workspace now compiles on Linux and macOS. The terminal capability probe
  called `crossterm::ansi_support`, which crossterm 0.29 only ships for
  Windows, and two `std::process::Command` imports were unused on the platforms
  whose liveness and RSS probes read `/proc`. Windows behavior is unchanged.
- Quitting, `:x`, `:wq` and `ZZ` no longer discard unsaved text from a buffer
  that is not the focused one, and `:tabclose` refuses to close a tab whose
  loaded buffer is modified. The refusal names the dirty background buffer with
  `E162`; `:q!` and `ZQ` still discard on purpose. Requirement: BUF-003.
- `ZQ` quits without writing, as Vim does, instead of refusing when the buffer
  is modified. Requirement: BUF-003.

## [0.2.0-alpha.1] - 2026-11-04

### Added

- G2 editor surface: counted motions/operators and text objects, named and
  numbered registers, macros, dot-repeat, `:normal`, Replace mode, blockwise
  Visual editing, Select mode and `gv`.
- Multiple buffers with independent cursor, undo and modified state, plus
  `:ls`/`:b`/`:bnext`/`:bprev`/`:bd`.
- Horizontal and vertical splits with directional focus, resize, rotate, close
  and only-this-window commands; tab pages with `:tabnew`/`:tabclose` and
  `gt`/`gT`.
- A persistent undo tree with `g-`/`g+` and saved history.
- Thirteen end-to-end editor-surface acceptance tests covering buffers,
  splits, tabs, Visual Block and Select.

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
- Large-file mode (`BUF-012`): files at or above 20 MiB open through a bounded,
  chunked encoding decoder into the rope, show a `[LARGE]` status marker, defer
  the initial swap until the first unsaved change, and use a 1 MiB/10-second
  swap bound. The generated 20 MiB tests cover UTF-8, UTF-16LE, lossless latin-1
  and byte-exact unchanged saves; a just-below-threshold file stays in normal
  mode. The ignored G1 harness now generates a 1 GiB fixture at run time and
  measures first-frame latency, 1,000 edits, scrolling and RSS.
- Cell-accurate terminal rendering (`TERM-004`): the backend retains a grid
  of character-and-style cells and rewrites only changed cells, coalesced into
  horizontal runs. An unchanged frame writes no bytes; a cursor-only move
  never clears the screen or repaints buffer cells. A one-line vertical
  viewport move sets a text-area scroll region, scrolls the front buffer and
  paints only the newly exposed row and changed status cells. The first frame
  and a resize remain full repaints. Selection and search overlays stay
  reverse-video/underlined, and wide glyphs repaint as complete pairs.
- A real-binary one-line-scroll budget test caps the frame at 2,048 ANSI bytes;
  Windows 11 ConPTY measured 1,871 bytes. The direct `Screen` path emits a
  scroll-region sequence and no full clear, proven by a unit test. ConPTY
  re-renders the 23-row text area, so the PTY ceiling is a host-behavior
  regression guard, not a cell-minimal byte count.
- Active capability detection: at startup the terminal boundary queries TTY
  attachment, ANSI support and the advertised color count through crossterm.
  `NO_COLOR`/`TERM=dumb` still disable color; environment hints refine
  truecolor and synchronized output. Color depth, scroll regions, alternate
  screen and synchronized output fall back independently when absent, and the
  probe is nonblocking so it adds no first-frame wait. Unit tests cover full,
  256/16-color, no-TTY/no-ANSI and synchronized-override profiles.
- G1 large-file performance harness: `cargo xtask perf` builds the release
  binary, generates a 1 GiB fixture, drives the real binary in an 80x24 ConPTY,
  performs 1,000 edits, scrolls one line, samples RSS and removes the fixture.
  Windows 11 Pro 26200 / Ryzen 5 5600X measured 1,894.870 ms to first frame
  (8 s budget), 1,250,660,352 bytes RSS (1.165x, 2.5x budget), 705.801 ms for
  1,000 edits and 1,871 bytes for one scroll frame.
- Headless editor-loop tests and PTY tests that run the real binary, verify
  alternate-screen restoration on exit and on panic, exercise undo/redo,
  range operators, Visual selections, literal search, CRLF and grapheme edge
  cases, the dirty-flag round trip, horizontal scrolling past the terminal
  width, no-file startup, save-as, and the debug-only
  `VANTA_TEST_PANIC_AFTER_INIT` hook.

### Changed

- A clean named buffer no longer creates a swap file on open. The first
  unsaved change creates it; large-file mode widens the deferred flush bound to
  1 MiB or 10 seconds.
- Internal structure only: `Buffer` is split into storage, position, measure
  and edit modules; editor state lives in a `Document`, and every text change
  goes through `Editor::apply_edit` -> `Document::replace` -> `Buffer::apply`.
  No user-visible behavior changed.

### Fixed

- Cursor synchronization no longer snaps every movement back to the previous
  grapheme boundary, so Normal-mode `l`/`j`/`k` movements, insert positions,
  Visual ranges and `dw` addresses are preserved into the next command.
- `:e <path>` now refuses to discard a modified buffer unless the command is
  `:e! <path>`; the same guard covers window and tab file opens.
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
- The undo tree has persistent history, but `:earlier`/`:later`, `:undojoin`,
  `undolevels` and complete Vim branch parity remain open. Counts, named and
  numbered registers, text objects, dot-repeat, macros, blockwise Visual,
  Replace and Select modes are implemented; the complete Vim grammar matrix
  and differential corpus are not. Search is literal and both `/` and `?`
  work, but there is no regex, `:s`, search offsets or incremental matching.
  Marks, jumplist, changelist and quickfix are absent. There is no mapping
  engine or configurable keymap. Encoding detection is BOM-first
  UTF-8/UTF-16/UTF-32 with a latin-1 fallback; `fileencodings` is not
  configurable. There is no configuration file, Lua runtime, syntax
  highlighting, LSP, Git integration, picker, completion, embedded terminal or
  job runner. Atomic save does not yet write through symlinks. Swap recovery
  covers the open buffer rather than a multi-buffer swap set, and the
  `directory` option is absent.
- Large-file mode starts at 20 MiB with streamed opening. Saving still
  materializes the encoded output. The 1 GiB PERF-006/PERF-007 evidence is
  reproducible with `cargo xtask perf`.
- Rendering now diffs individual cells and optimizes one-line vertical scrolls.
  Active terminal probes cover TTY, ANSI and advertised color count; XTGETTCAP,
  terminfo color negotiation and a synchronized-output tearing test remain
  open. Real-binary scroll frames on Windows ConPTY are capped at 2,048 bytes
  (1,871 measured) because ConPTY repaints the text area; the direct backend
  output is smaller and covered by `Screen` unit tests.
- The workspace version is `0.2.0-alpha.1`. A `1.0` release waits for the
  P0/P1 requirements and gates G1-G5 in `PROJECT_SPEC.md` section 10.
