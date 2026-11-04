# Roadmap

Current release: `0.2.0-alpha.1` (2026-11-04). Current milestone: close G2.
The 1.0 contract is `PROJECT_SPEC.md` section 10. A milestone is not done
because code exists; its exit gate must pass.

## Done

- **G1 core foundation, Windows evidence.** Rope text substrate, typed
  positions, grapheme/display-width handling, cell renderer, atomic saves,
  backups, encodings, large-file streaming, swap recovery and persistent undo.
  The 248-test workspace suite is green; the 1 GiB performance harness is
  present but ignored by default. Three-platform CI has not run on GitHub.
- **G2 editor slice.** Normal/Insert/Replace/Visual/Visual Line/Visual Block/
  Select/Command modes, counted motions and operators, text objects, registers,
  macros, dot-repeat, `:normal`, undo tree, buffers, splits and tabs.
- **Release baseline.** Commit `41bd67e`, tag `v0.2.0-alpha.1`, release binary
  built and smoke-tested. The tag is local; no GitHub remote is configured.

## In progress

- **G2 closure.** The editing surface is usable, but the gate is still open.
  The mapping engine, key protocols, marks/jumplist/changelist/quickfix and the
  500-case differential corpus are missing. Mapping and corpus work has not
  started yet.

## Next: small steps

### 1. Mapping engine

- [ ] Add a mapping store and parse `:nmap` for Normal mode; test one mapping
      and one prefix conflict.
- [ ] Add `:vmap` and `:imap`, then `:map`; support recursive and non-recursive
      forms.
- [ ] Add `<Plug>`, `<leader>`/`mapleader`, timeout handling and map listing
      or removal.
- [ ] Prove mappings do not change default Vim bindings or prefix timing.

### 2. Differential corpus

- [ ] Add `tests/compat/cases/` and a harness that runs the pinned
      Neovim 0.12.6 oracle.
- [ ] Store reviewed expected buffer, cursor and mode for a small seed set of
      counts, operators, registers, text objects, Visual/Block/Select and undo.
- [ ] Grow the reviewed corpus to at least 500 cases; record every skip in the
      difference register.
- [ ] Run the corpus before closing G2.

### 3. Remaining G2 behavior

- [ ] Add legacy, `modifyOtherKeys` and kitty keyboard protocol decoding with
      PTY fixtures.
- [ ] Add marks, jumplist, changelist and quickfix/location lists.
- [ ] Finish `:earlier`/`:later`, `:undojoin` and remaining undo-branch parity
      and grammar edge cases.
- [ ] Close G2 only when the corpus, mapping and list work pass together.

## Not started

- **G3:** Vim regex search, `:s`/`:g`, typed options and `init.lua`, Lua API,
  tree-sitter syntax and themes.
- **G4:** module registry, picker, completion, LSP, Git, embedded terminal,
  jobs and the Lua plugin runtime.
- **G5:** full corpus on Linux/macOS/Windows, 72-hour soak, performance proof,
  release packaging, checksums, signatures and the 1.0 release.

## Finish line

Vanta is 1.0 only after every P0/P1 requirement is verified and G1-G5 pass.
Until then, releases stay in the `0.x` alpha series.
