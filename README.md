# Vanta

[![CI](https://github.com/vanta-editor/vanta/actions/workflows/ci.yml/badge.svg)](https://github.com/vanta-editor/vanta/actions/workflows/ci.yml)

Vanta is a modal terminal text editor written in Rust. It brings Vim's editing
language to a single native binary built for low latency, low memory use and
predictable behavior, with the development tooling users normally assemble from
plugins planned as first-party modules.

**Status: 0.2.0-alpha.1.** Vanta is a real modal editor with multiple buffers,
split windows, tab pages, a substantial Vim grammar, blockwise Visual editing,
Select mode, persistent undo, macros and dot-repeat. It is still an alpha, not
the 1.0 release described in [PROJECT_SPEC.md](PROJECT_SPEC.md): configuration,
syntax highlighting, LSP, Git integration and plugins are not implemented yet.
A release will carry the `1.0` version only after the spec's P0/P1 requirements
and gates G1-G5 pass.

## Try it in two minutes

```sh
git clone https://github.com/vanta-editor/vanta.git
cd vanta
cargo run --release -p vanta -- README.md
```

| Keys | What it shows |
|---|---|
| `ciw` then type | change a word through a text object |
| `yy` `p` | linewise register round trip |
| `Vjd` | linewise Visual delete |
| `:sp` then `Ctrl-w` `l` | split windows with directional focus |
| `u` `Ctrl-r` `g-` | undo-tree travel |
| `/crate` `n` | literal search with live highlights |
| `:wq` | atomic save and quit |

## What works today

- Normal, Insert, Replace, Visual, Visual Line, Visual Block, Select and
  Command-line modes with grapheme-safe editing.
- Multiple buffers (`:e`, `:ls`, `:b`, `:bnext`, `:bprev`, `:bd`) with
  independent cursor, undo and modified state.
- Horizontal and vertical splits (`<C-w>s`, `<C-w>v`) with directional focus,
  rotation, resize, close and only-this-window commands, plus tab pages
  (`:tabnew`, `:tabclose`, `gt`, `gT`).
- Cursor motions `h` `j` `k` `l`, arrows, `w` `b` `e` `ge`, `0` `^` `$`,
  `gg` `G`, `f` `t` `F` `T` `;` `,`, `%`, `{` `}`, with a desired display
  column across short lines and horizontal scrolling past the window.
- Counted operators over motions and text objects: `d` `c` `y` `>` `<` `=`
  `g~` `gu` `gU`; text objects `iw` `aw`, `is` `as`, `ip` `ap`, pairs, quoted
  strings and tags; `x` `X` `dd` `yy` `D` `C` `Y` `s` `S` `p` `P` `o` `O`
  `J` `gJ` `~` `r`.
- Named and numbered registers, black-hole and append registers, macros with
  `q`/`@`, dot-repeat and `:normal`.
- Visual Block delete/yank/change/insert/append/`$` and block paste; Select
  mode replacement and cancellation; `gv` reselect.
- An undo tree with `u`/`Ctrl-r`, `g-`/`g+`, persistent history, Insert-session
  coalescing and a truthful modified flag.
- Literal `/pattern` search with a live prompt, `n`/`N`, wrapping and visible
  match highlights.
- Atomic save that fsyncs a temporary file, renames over the target and never
  truncates the original on failure.
- Vim-compatible backups: `backup`, `writebackup`, `backupdir` and `backupext`.
- Crash recovery for named and untitled buffers, with `vanta -r` and
  `:recover`/`:recover!`.
- Encoding detection and preservation: UTF-8 with or without a BOM, UTF-16LE/BE
  and UTF-32LE/BE with a BOM, and a lossless latin-1 fallback. `:set fileencoding`
  selects the next write's encoding; `:set bomb` controls the UTF-8 BOM.
- Large-file mode at 20 MiB: chunked decoding into the rope, a `[LARGE]` status
  marker, lazy initial swaps and a wider 1 MiB/10-second swap bound. Opening is
  streamed; saving still materializes the encoded output.
- Cell-accurate terminal rendering: a retained grid stores text and style per
  cell and writes only changed cell runs. Unchanged frames write nothing and a
  cursor-only move emits no buffer cells. A one-line scroll uses a text-area
  scroll region and repaints only the newly exposed row plus changed status
  cells. Capability fallbacks cover `NO_COLOR`, 16/256/truecolor and
  synchronized output.
- Active terminal capability detection at startup: TTY attachment, ANSI
  support and advertised color count are queried through crossterm, with
  environment hints only for truecolor and synchronized-output refinement.
  Color depth, scroll regions, alternate screen and synchronized output fall
  back independently when a capability is absent.
- A panic-safe terminal session: raw mode, the alternate screen and the cursor
  are restored on normal exit, early errors and panic.
- Windows 11 is the verified platform. Linux and macOS CI is configured in
  this repository but has not run yet.

## Install

### Prebuilt archives

Tagged releases publish `vanta-<version>-<target>.tar.gz` archives for Windows
x86_64, Linux x86_64 and macOS x86_64/arm64, together with `SHA256SUMS.txt`.
Extract the archive, verify the checksum, and put `vanta` (or `vanta.exe`) on
your `PATH`.

```sh
sha256sum -c SHA256SUMS.txt   # macOS: shasum -a 256 -c SHA256SUMS.txt
tar -xzf vanta-<version>-x86_64-unknown-linux-gnu.tar.gz
install -m 755 vanta ~/.local/bin/
```

Archives are unsigned; signatures are not yet produced.

### From source

Rust 1.90 or newer is required; the workspace pins 1.94.0 in
`rust-toolchain.toml`.

```sh
git clone https://github.com/vanta-editor/vanta.git
cd vanta
cargo build --release -p vanta
./target/release/vanta --version
```

Or install into the Cargo bin directory already on `PATH`:

```sh
cargo install --path crates/vanta --locked
```

## Usage

```text
vanta [file]          open a file, or an empty [No Name] buffer
vanta -r [file]       recover the newest untitled swap, or a named file's swap
vanta --help          print help without entering the editor
vanta --version       print the version
```

Exit codes: `0` success, `1` startup/runtime error, `2` invalid arguments.

### Keys

| Context | Keys | Action |
|---|---|---|
| Normal | `h` `j` `k` `l`, arrows | Move the cursor |
| Normal | `w` `b` `e` `ge`, `0` `^` `$`, `gg` `G` | Word, line and file motions |
| Normal | `f` `t` `F` `T` `;` `,`, `%`, `{` `}` | Find, match-pair and paragraph motions |
| Normal | `i` `a` `I` `A` `o` `O` `gi` | Insert entry points |
| Normal | `R` | Replace mode |
| Normal | `x` `X` `dd` `yy` `dw` `db` `D` `C` `Y` `s` `S` `p` `P` | Edit, delete, yank or put text |
| Normal | `d` `c` `y` `>` `<` `=` `g~` `gu` `gU` with motions or text objects | Operators |
| Normal | `v` `V` `Ctrl-v` | Characterwise, linewise or blockwise Visual mode |
| Normal | `gh` `gH`, `gv` | Select characterwise/linewise, or reselect |
| Normal | `u`, `Ctrl-r`, `g-`, `g+` | Undo, redo, older/newer undo state |
| Normal | `q{0-9a-zA-Z"}` | Record a macro |
| Normal | `@{0-9a-zA-Z"}` | Replay a macro |
| Normal | `.` | Repeat the last change |
| Normal | `Ctrl-w` then `s` `v` `h` `j` `k` `l` `w` `p` `q` `o` `=` `r` | Window commands |
| Normal | `gt`, `gT` | Next/previous tab page |
| Normal | `/`, `?`, `n`, `N`, `*`, `#` | Search forward/backward and repeat |
| Normal | `:` | Enter a command |
| Visual | motions, `d`/`x`, `y`, `c`, `I`, `A`, `$`, `Esc` | Extend or operate on the selection |
| Insert | text, `Backspace`, `Enter`, `Esc` | Edit and return to Normal mode |

### Commands

| Command | Action |
|---|---|
| `:w` | Save the current buffer |
| `:w <path>` | Save as, and adopt the new path |
| `:wq`, `:wq <path>`, `:x` | Save and quit |
| `:q`, `:q!` | Quit, or force quit and discard changes |
| `:e <path>`, `:e! <path>` | Open a file, or discard changes and open |
| `:ls`, `:b <N>`, `:bnext`, `:bprev`, `:bd[!]` | List, select, cycle or delete buffers |
| `:sp [file]`, `:vsp [file]` | Split the focused window horizontally/vertically |
| `:tabnew [file]`, `:tabclose`, `:tabnext`, `:tabprev` | Create, close or switch tab pages |
| `:normal <keys>` | Run Normal-mode keys through the shared parser |
| `:registers` | Show register contents |
| `:recover [file]`, `:recover!` | Load a retained swap; `!` replaces a modified buffer |
| `:preserve` | Force a swap flush |
| `:set backup` / `:set writebackup` | Keep a permanent/transient backup on save |
| `:set backupdir=...`, `:set backupext=...` | Configure backup location and suffix |
| `:set fileencoding=<enc>` | Set the next write's encoding (`utf-8`, `utf-16le`, ...) |
| `:set bomb` / `:set nobomb` | Add or remove the UTF-8 BOM on write |

Quoted paths and paths containing spaces are accepted.

## Crash recovery

- A named buffer keeps a checksummed swap beside its target. Untitled buffers
  use `.untitled-<hash>.swp` under the state directory's `swap/` subdirectory;
  named swaps fall back there when the target directory is not writable.
- Untitled buffers and named buffers over 16 KiB flush after 200 changed bytes
  or four seconds without input. Small named buffers flush on each mutation.
- `vanta -r` loads the newest retained untitled swap. `vanta -r <file>` and
  `:recover <file>` load a named swap. `:recover!` may replace a modified
  buffer.
- A recovered buffer is marked modified. Plain `:q` refuses; only an explicit
  `:w` (or `:w <path>` for an untitled buffer) writes it.
- Opening a named file with a retained swap shows `E325: ATTENTION`: `R`
  recovers, `O` opens the original and keeps the swap, `E` edits anyway, `D`
  deletes it, `Q`/`A`/`Esc` abort. An untitled swap is recovered explicitly
  with `vanta -r`; a plain `vanta` starts with an empty buffer.

State locations, overridable with `VANTA_STATE_DIR`:

| Platform | Default |
|---|---|
| Linux/macOS | `$XDG_STATE_HOME/vanta` or `~/.local/state/vanta` |
| Windows | `%LOCALAPPDATA%\vanta\state` |

There is no configuration file or plugin runtime in this preview.

## Build and test

```sh
cargo xtask check      # fmt --check, Clippy -D warnings, workspace tests
cargo test --workspace
cargo build --release -p vanta
```

The suite currently contains 250 tests plus one ignored 1 GiB performance test,
including headless editor-loop tests, 13 end-to-end editor-surface tests for
buffers, splits, tabs, Visual Block and Select, and PTY tests that run the real
binary: save, undo/redo, Visual selections, search, horizontal scrolling,
cell-only cursor motion, one-line scrolling, kill-and-recover, and terminal
restoration on panic. `cargo xtask check` is the same gate CI runs.

The one-line scroll PTY test uses a 120-line file in an 80x24 terminal, moves
to the last visible text row and scrolls once with `j`. It caps the emitted
frame at 2,048 ANSI bytes. Windows 11 ConPTY measured 1,871 bytes for that
frame, leaving 208 bytes (about 11%) of headroom. The direct backend output is
smaller: a `Screen` unit test asserts the text-area scroll-region sequence, no
full-screen clear and no repaint of unchanged rows. ConPTY consumes the scroll
sequence and re-renders the 23-row text area, so 2,048 is a regression ceiling
for the observed host behavior, not a claim that the host emits only the cells
Vanta diffed.

`cargo xtask perf` builds the release binary and runs the ignored G1 harness. It
generates a 1 GiB fixture at run time, opens it through the real binary in an
80x24 ConPTY, performs 1,000 Insert-mode edits, scrolls one line, samples RSS,
and removes the fixture. The recorded Windows 11 run measured **1,894.870 ms**
to the first editable frame (8 s budget), **1,250,660,352 bytes RSS** (1.165x
the file; 2.5x budget), 705.801 ms for the 1,000 edits, and a **1,871-byte**
scroll frame. Host: Windows 11 Pro 26200, AMD Ryzen 5 5600X, 16,306 MB RAM,
ConPTY at 80x24 with `TERM=xterm-256color`.

## Repository layout

| Path | Contents |
|---|---|
| `crates/vanta` | Editor binary: modes, commands, document, swap, terminal |
| `crates/vanta-text` | Rope-backed buffer, positions, graphemes, display width |
| `crates/vanta-fs` | Atomic save and backup transactions |
| `xtask` | Developer task runner |
| `PROJECT_SPEC.md` | Behavioral specification, requirements and release gates |
| `AGENTS.md` | Workflow, architecture and quality rules |
| `CHANGELOG.md` | User-visible changes |

## Limitations

This is a 0.2 alpha, not yet a daily-driver Vim replacement.

- No user mapping engine (`:map`, `:nmap`, `:vmap`, `:imap`, `<Plug>`,
  `<leader>`), command abbreviations or configurable timeout behavior.
- No marks, jumplist, changelist or quickfix/location lists.
- The undo tree has persistent history, but `:earlier`/`:later`, `:undojoin`
  and complete Vim branch parity remain open.
- There is no 500-case Neovim differential corpus yet; the current
  compatibility evidence is the focused editor-flow and G2 suites.
- Terminal key protocols beyond the default legacy encoding
  (`modifyOtherKeys` and the kitty keyboard protocol) are not implemented.
- Search is literal: no regex, offsets, `:s`, `:g` or search history.
- Encoding detection is fixed to BOM-first UTF-8/UTF-16/UTF-32 and a latin-1
  fallback; `fileencodings` is not configurable. Malformed BOM data opens as
  lossless latin-1 rather than a read-only binary buffer.
- No configuration file, Lua API, plugins, syntax highlighting, LSP, Git,
  picker, completion, embedded terminal or jobs.
- No mouse, bracketed paste, clipboard integration or line-number gutter.
- Rendering diffs individual cells. Capability detection actively queries the
  attached terminal with crossterm; it does not parse XTGETTCAP or terminfo,
  and truecolor still has environment hints for terminals that under-report
  their color count. The real-binary scroll ceiling is set by ConPTY's
  text-area repaint rather than Vanta's direct cell output. Three-platform CI
  has not run on GitHub yet.
- Large-file mode activates at 20 MiB and opens through a chunked decoder; the
  1 GiB BUF-012 fixture is generated by `cargo xtask perf` and opens in 1.895 s
  on the recorded host; saving still builds the encoded output in memory.
  External-change detection and symlink-aware saves are not implemented.
- Cross-platform behavior is not yet evidenced. Windows 11 has the full local
  test run; the Linux/macOS workflow definitions have not completed on GitHub.

## Roadmap to 1.0

The 1.0 contract is [PROJECT_SPEC.md](PROJECT_SPEC.md) section 10. Milestones
are ordered by dependency and cannot be waived because code exists.

| Milestone | Outcome | State |
|---|---|---|
| 1 | Core foundation: renderer, rope, file I/O, recovery, CI | Passed on Windows 11 evidence: text engine, atomic save, backups, swap recovery, encoding, 20 MiB large-file mode, active capability detection and cell-level rendering exist. The 1 GiB harness measured 1.895 s first frame and 1.165x RSS against 8 s and 2.5x budgets; one-line scroll is 1,871 bytes against 2,048. Three-platform CI still needs a green GitHub run. |
| 2 | Vim grammar: motions, operators, text objects, registers, macros, undo tree, windows, buffers, tabs | Large slice implemented and green: counted grammar, text objects, registers, macros, dot-repeat, persistent undo tree, Visual Block, Select, buffers, splits and tabs. Gate remains open for mappings, key protocols, marks/jumplist/changelist/quickfix and the 500-case differential corpus. |
| 3 | Search, typed Lua configuration, tree-sitter syntax | Not started. |
| 4 | Modules, LSP, Git, picker, terminal and plugin runtime | Not started. |
| 5 | Reliability, performance, three-platform proof and 1.0 release | Not started. |

### G2 gap list (next)

G2 remains open. The next work is ordered by user-visible risk:

1. **Mapping engine** — `:map`/`:nmap`/`:vmap`/`:imap`, recursive and
   non-recursive forms, `<Plug>`, `<leader>`/`mapleader`, timeout handling and
   mappings that are valid prefixes of existing commands. No default Vim
   mapping is removed.
2. **Differential corpus** — at least 500 cases under `tests/compat/cases/`,
   generated and reviewed against Neovim 0.12.6, covering counts, registers,
   operators, text objects, Visual/Block/Select, undo groups and failure cases;
   every skip is recorded in the specification's difference register.
3. **Key protocols** — legacy, `modifyOtherKeys` and kitty keyboard protocol
   decoding, with an explicit fallback when the terminal advertises none.
4. **Marks and lists** — local/global marks, jumplist (`Ctrl-o`/`Ctrl-i`),
   changelist (`g;`/`g,`) and quickfix/location lists.
5. **Grammar matrix** — remaining operator/text-object edge cases, exact undo
   grouping and platform-specific Unicode/line-ending parity.

No `1.0` tag will be created before gates G1-G5 pass. Contributions toward
those milestones are welcome; read [AGENTS.md](AGENTS.md) first.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this project by you, as defined in the Apache-2.0
license, shall be dual-licensed as above, without any additional terms or
conditions.

Vanta has no telemetry, analytics, automatic update checks or hidden network
access. See [SECURITY.md](SECURITY.md) for private vulnerability reporting.
