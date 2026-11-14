<h1 align="center">Vanta</h1>

<p align="center">
  <b>Vim's editing language in one ~950 KB native binary.</b><br>
  No plugin tree. No runtime to install. No startup script.<br>
  <code>vanta file.rs</code> and you are editing.
</p>

<p align="center">
  <a href="https://github.com/DrxvenBSD/vanta/actions/workflows/ci.yml"><img src="https://github.com/DrxvenBSD/vanta/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="#license"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg" alt="License: MIT OR Apache-2.0"></a>
  <a href="rust-toolchain.toml"><img src="https://img.shields.io/badge/rust-1.90%2B-orange.svg" alt="Rust 1.90+"></a>
  <a href="#platform-support"><img src="https://img.shields.io/badge/platform-windows%2011-0078D4.svg" alt="Platform: Windows 11"></a>
</p>

Vanta is a modal terminal text editor written in Rust. It aims at Vim 9.2
semantics, uses Neovim as its differential test oracle, and treats keyboard
latency, memory use and predictable behavior as features with budgets rather
than afterthoughts. The tooling people normally assemble from plugins — syntax,
LSP, Git, a picker, a plugin runtime — is planned as first-party modules instead
of a compatibility shim.

**Status: 0.2.0-alpha.2 — Windows 11 first.** A real modal editor with multiple
buffers, split windows, tab pages, a substantial Vim grammar, blockwise Visual
editing, Select mode, persistent undo, macros and dot-repeat. Not the 1.0
release described in [PROJECT_SPEC.md](PROJECT_SPEC.md): configuration, syntax
highlighting, LSP, Git and plugins are not implemented yet. The `1.0` version
arrives only after the spec's P0/P1 requirements and gates G1-G5 pass.

| Platform | State |
|---|---|
| Windows 11 x86_64 | **Supported.** The full 250-test suite and the real-binary PTY tests pass locally and in CI. |
| Linux x86_64, macOS arm64/x86_64 | **Builds, not supported yet.** CI compiles them and runs the suite as an experimental job; the save-status tests fail on Linux and untitled swap recovery fails on macOS, so no archives are published. |
| Everything else | Untested. |

<details open>
<summary><b>Contents</b></summary>

- [Why another editor](#why-another-editor)
- [What works today](#what-works-today)
- [Install](#install)
- [Usage](#usage)
- [Crash recovery](#crash-recovery)
- [Build and test](#build-and-test)
- [Repository layout](#repository-layout)
- [Limitations](#limitations)
- [Roadmap to 1.0](#roadmap-to-10)
- [License](#license)

</details>

## Why another editor

- **One process, one binary.** 949 KB release build, no runtime, no plugin host
  to keep alive.
- **Cell-level rendering.** The renderer keeps a grid and writes only the cells
  that changed; a cursor move emits no buffer bytes and a one-line scroll uses a
  scroll region. Measured: 1,871 bytes for that scroll frame on ConPTY.
- **1 GiB files.** `cargo xtask perf` opens a generated 1 GiB file in 1.895 s
  (8 s budget) at 1.165x RSS (2.5x budget), then edits and scrolls it.
- **Saves that do not corrupt data.** Temporary file, fsync, rename over the
  target, directory fsync — the original survives a failed write.
- **Crash recovery that refuses to lose work.** Checksummed swaps for named and
    untitled buffers, plus a quit guard that will not drop a modified buffer in
  the background.

## Try it in two minutes

```sh
git clone https://github.com/DrxvenBSD/vanta.git
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
- Windows 11 x86_64 is the supported platform: the whole suite and the
  real-binary PTY tests pass locally and on the CI job. Linux and macOS compile
  and are built by CI, but stay experimental — see
  [Platform support](#platform-support).

## Install

### Windows: prebuilt archive

Tagged releases publish `vanta-<version>-x86_64-pc-windows-msvc.tar.gz` with a
`SHA256SUMS.txt`. Extract it, check the hash and put `vanta.exe` on your `PATH`.

```powershell
Get-FileHash .\vanta-*.tar.gz -Algorithm SHA256
tar -xzf vanta-<version>-x86_64-pc-windows-msvc.tar.gz
```

Archives are unsigned; signatures are not yet produced. Linux and macOS archives
are not published until their save and recovery paths pass the suite. Until
then, build them from source — they compile and mostly work, but see
[Platform support](#platform-support) before you trust them with work.

### From source

Rust 1.90 or newer is required; the workspace pins 1.94.0 in
`rust-toolchain.toml`.

```sh
git clone https://github.com/DrxvenBSD/vanta.git
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

### Measured budgets

Recorded with `cargo xtask perf` on Windows 11 Pro 26200, AMD Ryzen 5 5600X,
16,306 MB RAM, ConPTY at 80x24 with `TERM=xterm-256color`.

| Metric | Measured | Budget | Headroom |
|---|---:|---:|---:|
| 1 GiB file → first editable frame | **1,895 ms** | 8,000 ms | 4.2x |
| Resident memory after that open | **1.25 GB** (1.165x file) | 2.5x file | 2.1x |
| 1,000 Insert-mode edits | **706 ms** | — | — |
| One-line scroll frame | **1,871 B** | 2,048 B | 11% |

The scroll number is a real-binary ceiling, not a claim that the host emits only
the cells Vanta diffed: ConPTY consumes the scroll-region sequence and repaints
the 23-row text area. The direct backend output is smaller, and a `Screen` unit
test asserts the scroll-region sequence with no full clear and no repaint of
unchanged rows.

## Repository layout

| Path | Contents |
|---|---|
| `crates/vanta` | Editor binary: modes, commands, document, swap, terminal |
| `crates/vanta-text` | Rope-backed buffer, positions, graphemes, display width |
| `crates/vanta-fs` | Atomic save and backup transactions |
| `xtask` | Developer task runner |
| `PROJECT_SPEC.md` | Behavioral specification, requirements and release gates |
| `ARCHITECTURE.md` | Runtime flow, state ownership and testing seams |
| `AGENTS.md` | Workflow, architecture and quality rules |
| `CHANGELOG.md` | User-visible changes |
| `SECURITY.md` | Private vulnerability reporting |

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
  text-area repaint rather than Vanta's direct cell output.
- Large-file mode activates at 20 MiB and opens through a chunked decoder; the
  1 GiB BUF-012 fixture is generated by `cargo xtask perf` and opens in 1.895 s
  on the recorded host; saving still builds the encoded output in memory.
  External-change detection and symlink-aware saves are not implemented.
- Cross-platform behavior is only partly evidenced. Windows 11 passes the
  whole suite in CI. The Linux and macOS jobs compile, but three save-status
  tests fail on Linux and untitled swap recovery fails on macOS (`E305`), so
  those platforms are experimental and publish no archives. Fixing them is the
  next cross-platform milestone.

## Roadmap to 1.0

The 1.0 contract is [PROJECT_SPEC.md](PROJECT_SPEC.md) section 10. Milestones
are ordered by dependency and cannot be waived because code exists.

| Milestone | Outcome | State |
|---|---|---|
| 1 | Core foundation: renderer, rope, file I/O, recovery, CI | Passed on Windows 11 evidence: text engine, atomic save, backups, swap recovery, encoding, 20 MiB large-file mode, active capability detection and cell-level rendering exist. The 1 GiB harness measured 1.895 s first frame and 1.165x RSS against 8 s and 2.5x budgets; one-line scroll is 1,871 bytes against 2,048. Windows CI runs green on GitHub; the Linux/macOS jobs are experimental and still fail three save-status tests and macOS untitled recovery. |
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
