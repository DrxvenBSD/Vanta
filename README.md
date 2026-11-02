# Vanta

Vanta is a modal terminal text editor written in Rust. It brings Vim's editing
language to a single native binary built for low latency, low memory use and
predictable behavior, with the development tooling users normally assemble from
plugins planned as first-party modules.

**Status: 0.1 preview.** Vanta is usable today for single-file modal editing,
atomic saves, backups and crash recovery. It is **not** the 1.0 release
described in [PROJECT_SPEC.md](PROJECT_SPEC.md): multiple buffers and windows,
the full operator/text-object grammar, configuration, syntax highlighting, LSP,
Git integration and plugins are still in progress. A release will carry the
`1.0` version only after the spec's P0/P1 requirements and gates G1-G5 pass.

## What works today

- Normal, Insert, Visual and Visual Line modes with grapheme-safe editing.
- Cursor motions `h` `j` `k` `l`, arrows, `w` `b`, `0` `$`, with a desired
  display column across short lines and horizontal scrolling past the window.
- Normal verbs `x`, `dd`, `yy`, `dw`, `db`, `p`, `P`, `o`, `O`; Visual verbs
  `d`/`x`, `y`, `c`; one unnamed register that preserves characterwise versus
  linewise contents.
- Linear undo/redo with Insert-session coalescing and a truthful modified flag.
- Literal `/pattern` search with a live prompt, `n`/`N`, wrapping and visible
  match highlights.
- Atomic save that fsyncs a temporary file, renames over the target and never
  truncates the original on failure.
- Vim-compatible backups: `backup`, `writebackup`, `backupdir` and `backupext`.
- Crash recovery for named and untitled buffers, with `vanta -r` and
  `:recover`/`:recover!`.
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
| Normal | `w` `b` | Next/previous word start |
| Normal | `0` `$` | Line start / last character |
| Normal | `i` `a` `A` | Insert before, after, or at end of line |
| Normal | `o` `O` | Open a line below/above and insert |
| Normal | `x` | Delete the character under the cursor |
| Normal | `dd` `yy` `dw` `db` | Delete/yank line or word range |
| Normal | `p` `P` | Put after/before the cursor |
| Normal | `v` `V` | Start characterwise/linewise Visual mode |
| Normal | `u`, `Ctrl-r` | Undo, redo |
| Normal | `/`, `n`, `N` | Search, next match, previous match |
| Normal | `:` | Enter a command |
| Visual | motions, `d`/`x`, `y`, `c`, `Esc` | Extend, delete, yank, change, cancel |
| Insert | text, `Backspace`, `Enter`, `Esc` | Edit and return to Normal mode |

### Commands

| Command | Action |
|---|---|
| `:w` | Save the current buffer |
| `:w <path>` | Save as, and adopt the new path |
| `:wq`, `:wq <path>`, `:x` | Save and quit |
| `:q`, `:q!` | Quit, or force quit and discard changes |
| `:e <path>`, `:e! <path>` | Open a file, or discard changes and open |
| `:recover [file]`, `:recover!` | Load a retained swap; `!` replaces a modified buffer |
| `:preserve` | Force a swap flush |
| `:set backup` / `:set writebackup` | Keep a permanent/transient backup on save |
| `:set backupdir=...`, `:set backupext=...` | Configure backup location and suffix |

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

The suite currently contains 163 tests, including headless editor-loop tests
and PTY tests that run the real binary: save, undo/redo, Visual selections,
search, horizontal scrolling, kill-and-recover, and terminal restoration on
panic. `cargo xtask check` is the same gate CI runs.

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

This is a 0.1 preview, not a daily-driver Vim replacement.

- One buffer, one window: no splits, tabs, `:b` or window commands.
- No counts, text objects, dot-repeat, macros, named/numbered registers,
  Replace/Select/Visual Block modes, marks, jumplist or quickfix.
- Undo is linear; there is no undo tree, persistent undo or `:earlier`/`:later`.
- Search is literal: no regex, `?`, offsets, `:s`, `:g` or search history.
- UTF-8 only; other encodings are rejected rather than converted.
- No configuration file, Lua API, plugins, syntax highlighting, LSP, Git,
  picker, completion, embedded terminal or jobs.
- No mouse, bracketed paste, clipboard integration or line-number gutter.
- Large-file mode, external-change detection and symlink-aware saves are not
  implemented.
- Cross-platform behavior is not yet evidenced. Windows 11 has the full local
  test run; the Linux/macOS workflow definitions have not completed on GitHub.

## Roadmap to 1.0

The 1.0 contract is [PROJECT_SPEC.md](PROJECT_SPEC.md) section 10. Milestones
are ordered by dependency and cannot be waived because code exists.

| Milestone | Outcome | State |
|---|---|---|
| 1 | Core foundation: renderer, rope, file I/O, recovery, CI | Partial: text engine, atomic save, backups and swap recovery exist; encoding, large-file mode, damage renderer and capability probe do not. |
| 2 | Vim grammar: motions, operators, text objects, registers, macros, undo tree, windows, buffers, tabs | Early slice: basic modes and linear undo only. |
| 3 | Search, typed Lua configuration, tree-sitter syntax | Not started. |
| 4 | Modules, LSP, Git, picker, terminal and plugin runtime | Not started. |
| 5 | Reliability, performance, three-platform proof and 1.0 release | Not started. |

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
