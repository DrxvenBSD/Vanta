# Vanta Knowledge

## What it is

Vanta is a modal terminal text editor written in Rust. It targets Vim 9.2
editing behavior, uses Neovim 0.12.6 as the differential test oracle, and ships
as a single native binary.

The current version is `0.2.0-alpha.1`. It has real buffers, splits, tabs, a
substantial Vim grammar, registers, macros, persistent undo and block/Select
editing. It is not 1.0 yet. Version 1.0 requires the P0/P1 requirements and
gates G1-G5 in `PROJECT_SPEC.md`.

## Code organization

- `crates/vanta-text/` — the pure text substrate. Rope storage, typed positions
  (bytes, lines, scalars, display cells), grapheme boundaries, display width
  and the buffer-owned undo tree. No filesystem or terminal I/O.
- `crates/vanta-fs/` — the file lifecycle. Atomic save, backups, encoding
  detection/conversion and streaming reads for large files.
- `crates/vanta/` — the editor. `Editor` owns modes, windows, tabs, buffers,
  registers and input. `Document` owns one open file, its cursor and its swap
  lifecycle. `editor/grammar.rs` parses Normal-mode commands; `editor/render.rs`
  builds frames; `terminal/` is the platform boundary; `swap.rs` and
  `undo_file.rs` persist recovery state.
- `crates/vanta/tests/` — editor-flow, G2, surface, PTY, recovery, encoding and
  performance acceptance tests.
- `xtask/` — `cargo xtask check`, `test`, `fmt` and the ignored 1 GiB `perf`
  harness.
- `PROJECT_SPEC.md` — behavior, requirements, architecture and release gates.
  It is the source of truth. `AGENTS.md` defines the working rules.
- `README.md`, `CHANGELOG.md` and `SECURITY.md` — public status and project
  policy. `target/` is build output and is ignored.

## How a change flows

The binary creates an `Editor` and a `CrosstermTerminal`. The editor loop reads
an `InputEvent`, dispatches by mode, and for Normal mode feeds the shared
`Parser`. A complete `Command` is executed by `Editor`; text changes go through
`Document` and then `Buffer`. Rendering is read-only: `Editor` builds a `Frame`,
the terminal backend draws it, and the backend diffs cells. Undo belongs to the
buffer. Saves and recovery go through `vanta-fs`; swap and undo files live in
the state directory.

## Rules

1. Work on one small task at a time. Finish and verify it before starting the
   next one.
2. Run `cargo test --workspace` and `cargo build` before finishing a change.
   For hot paths, also run the narrow benchmark or `cargo xtask perf`.
3. Never rewrite files the user did not ask about. Keep the diff focused; do
   not reformat, rename or refactor unrelated code.
4. Read the governing section of `PROJECT_SPEC.md` and `AGENTS.md` before
   changing behavior. Cite requirement IDs in the change.
5. Do not weaken, delete or ignore a failing test. Fix the cause.
6. P0/P1 requirements are 1.0 scope. P2 needs a disabled, documented 1.x gate.
   P3 behavior is prohibited.
7. Preserve Vim compatibility. Record intentional differences; unrecorded
   differences are bugs.
8. Keep `vanta-text` free of I/O and unsafe code. Dependencies point from
   `vanta` to `vanta-text` and `vanta-fs`, never back.
9. State the unit of every position or width: byte, scalar, grapheme or display
   cell. Never split a grapheme cluster.
10. No placeholders, dead code, fake success or silent error handling. Errors
    are propagated or handled with context.
11. Update user-facing documentation and the changelog when behavior changes.
    Do not commit or push unless the user asks.
