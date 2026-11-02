# AGENTS.md — Working Rules for AI Coding Agents
---



| Field | Value |
|---|---|
| Document | `AGENTS.md` |
| Project | Vanta |
| Version | 1.0 (Draft) |
| Date | 2026-10-31 |
| Audience | AI coding agents and human reviewers |
| Normative companion | [[PROJECT_SPEC\|PROJECT_SPEC.md]] |
| Applies to | Every change in this repository, including documentation and tooling |

This file defines **how** work on Vanta is performed. [[PROJECT_SPEC|PROJECT_SPEC.md]] defines **what** Vanta must do and why. Read both before writing code.

## 1. How to use this document

1. Read the specification section that governs your task, not just this file. Requirement IDs in [[PROJECT_SPEC#3. Requirements|§3]]–[[PROJECT_SPEC#9. Observability and diagnostics|§9]] of the specification are the acceptance contract.
2. Find the governing milestone in [[PROJECT_SPEC#10. Roadmap|§10]] and confirm the requirement priority. Implement P0 and P1 for 1.0; P2 work is designed but not enabled before its 1.x gate; P3 behavior is prohibited.
3. Check the architecture rules in [[#7. Architecture rules|§7]] and performance rules in [[#8. Performance rules|§8]] before designing a solution.
4. Implement the smallest complete change that satisfies the requirement. Do not add speculative abstractions, options or modules.
5. Verify with the checks in [[#11. Testing standards|§11]] and record the evidence in the pull request.
6. Update documentation when behavior, architecture, compatibility or performance changes.

### Precedence

When documents disagree, the order is:

1. `PROJECT_SPEC.md` — behavior, requirements, budgets, architecture.
2. Accepted ADRs in `Documentation/adr/` — decisions that refine the specification.
3. `AGENTS.md` — workflow, quality and coding rules.
4. Code comments and existing tests.

If two sources genuinely conflict, **stop and report the conflict** rather than choosing silently. A change that requires contradicting the specification needs a specification change and an ADR, not a code workaround.

## 2. Required reading and repository map

Before the first change, read:

- [[PROJECT_SPEC|PROJECT_SPEC.md]] [[PROJECT_SPEC#1. Vision and scope|§1]]–[[PROJECT_SPEC#2. Implementation language decision|§2]] for product intent and language policy.
- [[PROJECT_SPEC#3A Modal editing|§3A]]–[[PROJECT_SPEC#3H Neovim-inspired architecture|§3H]] for the behavior you are touching.
- [[PROJECT_SPEC#4. First-party modules|§4]] if your change is user-facing functionality.
- [[PROJECT_SPEC#5. Architecture|§5]] for crate boundaries, ownership and communication rules.
- [[PROJECT_SPEC#6. Performance requirements|§6]] for performance budgets.
- [[PROJECT_SPEC#10. Roadmap|§10]] for the governing milestone and its exit gate.
- [[PROJECT_SPEC#11. Compatibility and intentional differences|§11]] for compatibility and intentional differences.

The crate table, dependency direction, component responsibilities, event loop and state ownership are normative in [[PROJECT_SPEC#5.2 Crates and dependency direction|PROJECT_SPEC.md §5.2]] through [[PROJECT_SPEC#5.3 Component responsibilities|§5.3]] and [[PROJECT_SPEC#5.12 State ownership|§5.12]]. Do not duplicate or reinterpret them here; link to the specification instead.

If a path does not exist yet, create it only when the current milestone requires it. Do not scaffold future crates or empty modules "for later".

## 3. Non-negotiable project rules

1. **Read the spec before implementing.** Cite the requirement IDs your change satisfies. If no requirement covers the behavior, the task is either documentation-only or needs a specification update first.
2. **Follow the architecture.** Dependencies point in the direction defined in [[PROJECT_SPEC#5.2 Crates and dependency direction|§5.2]]. `vanta-core` has no I/O, threads, terminal or Lua. Do not introduce a cycle to avoid a refactor.
3. **Preserve Vim compatibility.** Treat Vim 9.2 as the reference and Neovim 0.12.6 as the differential test oracle. Unrecorded behavioral differences are bugs.
4. **Correctness over shortcuts.** A command that works for the common case but corrupts state on an edge case is not done. Do not special-case tests or hide failures.
5. **Keep modules independent.** A module talks to other modules through public module APIs, events or the command queue. It never reaches into private state or assumes another module is active.
6. **Avoid unnecessary dependencies.** Every new crate needs the justification in [[#13. Dependency and licensing policy|§13]]. Prefer the standard library and existing dependencies.
7. **Avoid unnecessary abstractions.** Introduce a trait when there are at least two real implementations or a test seam that cannot be achieved otherwise. Do not build frameworks for one caller.
8. **Do not introduce bloat.** No feature without a requirement, no option without a user, no icon set or theme bundle that inflates the binary without a budget justification.
9. **Do not silently change behavior.** Defaults, keymaps and option semantics are compatibility surface. A change needs a changelog entry, updated tests and, where relevant, a difference-register entry.
10. **Do not implement placeholders and claim completion.** `todo!()`, stubbed commands, fake data and "returns true for now" are not acceptable in a merged path. If something is deferred, it must be explicitly gated, documented and unreachable in the default build.
11. **No dead code.** Remove unused functions, flags and modules. Do not keep commented-out implementations.
12. **No warnings without justification.** Compiler and Clippy warnings are errors in CI. An `#[allow]` needs a comment explaining why the lint is wrong here.
13. **Errors are handled, not logged and ignored.** Every `Result` is propagated, converted with context or explicitly handled. No `let _ =` on an operation that can fail unless the failure is provably irrelevant, with a comment.
14. **Unicode is not an afterthought.** Every cursor, range, column and width calculation must state its unit (byte, scalar, grapheme, display cell).
15. **Cross-platform is the default.** Do not add `#[cfg(windows)]` branches outside the designated platform modules without an ADR and tests on both paths.

### Priority gate

- **P0** and **P1** requirements are implemented and verified for 1.0. An unmet P0 or P1 requirement blocks release. P0 covers data loss, crashes, corruption and the core editing contract; P1 covers daily development such as syntax, LSP, picker, Git signs and async jobs.
- **P2** requirements may be designed for and merged only behind a disabled, documented 1.x gate. They must not add default cost or expose incomplete behavior in 1.0. DAP, interactive Ex mode and the optional modules are P2.
- **P3** items are non-goals: Vimscript, Neovim plugin compatibility, a 1.0 GUI, an AI assistant and a package manager. Do not implement them, stub them or add hidden flags for them.
- Priority is scope, not defect severity. A defect that violates a P0 or P1 requirement is release-blocking regardless of its label.

## 4. Definition of done

A feature is complete only when **all** of the following are true:

1. **It works** for the documented behavior, including failure paths and empty inputs.
2. **Its tests pass**, including unit, property, PTY and differential tests where applicable.
3. **Its edge cases are considered**: empty buffers, last line, multibyte text, read-only buffers, unicode, large files, windows, and concurrent edits.
4. **Its documentation is updated**: specification requirement (if changed), help text, `Documentation/compatibility.md` for differences, changelog entry.
5. **It does not break existing functionality**: the full affected crate test suite and the compatibility corpus pass.
6. **Its performance impact is understood**: measured for hot paths, within budget, or explicitly declared with a follow-up.

### Required evidence

| Change type | Evidence in the pull request |
|---|---|
| Behavior change | Requirement ID, test names, before/after behavior, compatibility note |
| New module | Manifest, tier/default, startup measurement, disable test, docs |
| Performance change | Benchmark command, raw before/after, p50/p95/p99, hardware |
| New dependency | Justification, license, maintenance status, binary-size impact, advisory check |
| Bug fix | Failing test that reproduces the bug, root cause, fix, regression test |
| Architecture change | ADR, affected crate boundaries, migration notes |
| Compatibility fix | Differential test case, Vim/Neovim version, difference register entry if intentional |
| Documentation-only | Links checked, wikilink targets validated, no behavioral claims without sources |

## 5. Development workflow

Work in one vertical slice at a time. Every step below produces an artifact a reviewer can inspect: a requirement reference, a failing test, a small diff, a passing check or a measurement.

### 5.1 Scope the requirement

- Read the governing requirement and its acceptance criteria in [[PROJECT_SPEC|PROJECT_SPEC.md]].
- Record the requirement ID, priority and milestone/gate. P0 and P1 are 1.0 work; P2 may be designed but must stay disabled before its 1.x gate; P3 must not be implemented.
- Write the intended behavior and the failure modes before coding. For compatibility work, capture Vim/Neovim behavior first.
- If no requirement covers the behavior, stop and request a specification change. Do not invent scope in code.

**Output:** one requirement ID, one milestone, a one-paragraph behavior statement and a list of failure cases.

### 5.2 Add the failing test

- Bug fix: add a reproducer that fails for the right reason.
- Parser, buffer or undo change: add unit and property tests, plus a differential case where behavior is observable.
- UI or renderer change: add a PTY screen assertion.
- LSP, Git, DAP or job change: add a stub service test with delayed and out-of-order responses.

```bash
cargo test -p <affected-crate> <test_name>          # must fail before the change
cargo xtask compat --filter <area>                 # for compatibility changes
```

**Output:** a red test or differential case committed with the change. Do not weaken an existing assertion to make room for new behavior.

### 5.3 Implement the smallest complete change

- Follow the crate boundaries in [[PROJECT_SPEC#5.2 Crates and dependency direction|PROJECT_SPEC.md §5.2]] and the ownership rules in [[#7. Architecture rules|§7]].
- Search for an existing helper before writing a new one. Keep the change local to the cause.
- Do not add a dependency, crate, feature flag or public API without the review triggers in [[#13. Dependency and licensing policy|§13]].
- Make invalid states unrepresentable where cheap: typed IDs, enums instead of string modes, newtypes for byte offsets and display columns.
- Keep P2 code unreachable in the default 1.0 build. Do not add P3 behavior at all.
- Update docs in the same change when a user-visible behavior, option, command or compatibility difference changes.

**Output:** a focused diff with no unrelated formatting, refactoring or generated files.

### 5.4 Verify

Run the narrowest checks that cover the change:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p <affected-crate>
```

Before merging a multi-crate, compatibility or release-gate change:

```bash
cargo test --workspace
cargo xtask compat --filter <area>
cargo xtask check
```

- Run the tests you changed and the tests that depend on the changed behavior. "It compiles" is not verification.
- If a check cannot run locally, say so in the pull request and rely on CI; never report it as passed.
- If a test fails, fix the cause. Do not ignore it, loosen the assertion or delete the case without a reviewed reason.
- For documentation-only changes, run `cargo xtask check` so links, wikilink targets and formatting are validated.

**Output:** the exact commands, exit status and any skipped check in the pull request.

### 5.5 Measure hot paths

If the change touches input decoding, mapping, parsing, buffer edits, undo, extmark adjustment, layout, composition, diff, fuzzy matching, LSP handling or startup:

```bash
cargo bench -p <crate> -- <benchmark>
cargo xtask perf --scenario <name> --runs 30
```

- Report command, hardware, sample count and p50/p95/p99, plus RSS where relevant.
- Use the comparison protocol in [[PROJECT_SPEC#6.4 Comparison protocol|PROJECT_SPEC.md §6.4]]. Do not claim an improvement from one run.
- If the budget cannot be met, stop and escalate before merging.

**Output:** before/after raw numbers or an explicit "not on a hot path" statement.

### 5.6 Document and hand off

- Update `Documentation/compatibility.md` when behavior differs from Vim/Neovim.
- Add an ADR for architectural decisions, new dependencies that shape the design, and deviations from the specification.
- Update the changelog under `Unreleased` with the user-visible description.
- Commit one logical change with a message that explains **why** and references the requirement ID. Follow [[#16. Git, commit and pull request conventions|§16]].
- Re-read the diff before requesting review. Remove debug output, dead code and unrelated changes.

**Output:** a pull request that satisfies the checklist in [[#18.1 Pull request checklist|§18.1]].

## 6. Compatibility protocol

### 6.1 Reference sources

- Vim 9.2 `:help` is normative for editing semantics, option names, command syntax and defaults.
- Neovim 0.12.6 behavior is the differential oracle, except where Vim and Neovim differ and the specification names Vim.
- Record the source in the test or comment when behavior is subtle (for example `:help cw`, `:help o_v`, `:help 'formatoptions'`).
- Do not rely on memory for edge cases. Verify with the actual editor before encoding behavior.
- Never use a random blog post as the source of truth when official documentation or the binary is available.

### 6.2 Differential tests

Differential cases live in `tests/compat/cases/<area>.toml`:

```toml
[[case]]
id = "operators-ci-quotes"
requirement = "KEY-003"
initial = "let x = \"hello world\";\n"
keys = ["c", "i", "\""]
# Expected state is generated by Neovim 0.12.6, then reviewed and stored.
expected_buffer = "let x = \"\";\n"
expected_cursor = [0, 9]
expected_mode = "normal"
```

- Generate expected values with the pinned Neovim, inspect them, and commit them. Never hand-write an expectation that was not verified.
- Cover counts, registers, operators, text objects, Visual variants, undo grouping and failure cases.
- If a case is unsupported, give it a `skip` with a reason and add it to `Documentation/compatibility.md`. Skipped cases do not count as passing.
- Run the corpus on every platform before release; cursor and register assertions catch platform-specific Unicode or line-ending bugs.

### 6.3 Keymap policy

- Preserve Vim 9.2 defaults. The only additional defaults are the documented Neovim LSP bindings in spec [[PROJECT_SPEC#3B Keyboard controls|§3B]].
- A new default mapping requires a specification change, a compatibility review and a migration note. Prefer a `<Plug>` mapping or a command for optional modules.
- Never bind a key that is a valid prefix of an existing Vim command without verifying the timing and operator behavior.
- Optional modules must not install mappings when disabled. Document every mapping they add when enabled.
- Test command combinations, not isolated keys: `d` with each motion, `c` with text objects, counts with registers, Visual with operators, macros with mappings.

### 6.4 Intentional differences

- An intentional difference needs an entry in spec [[PROJECT_SPEC#11.2 Difference register|§11.2]], a rationale, user-visible help text and a test that asserts the Vanta behavior.
- Do not silently normalize Vim behavior because it seems inconsistent; users depend on the inconsistency.
- When Vim and Neovim disagree, follow Vim unless the specification names Neovim or records the difference.
- Compatibility regressions are release-blocking defects. A change that breaks a previously supported command requires an explicit decision and a changelog entry.

## 7. Architecture rules

1. **One owner.** The `Editor` owns buffers, windows, tabs, options, registers and marks. Nothing else mutates them.
2. **Core is pure.** `vanta-core` performs no I/O, spawns no threads, reads no environment variables and does not call Lua during iteration. It may call Lua only through an explicit callback boundary after state is consistent.
3. **Services emit messages.** Syntax, LSP, Git, jobs, picker and terminal receive snapshots and send results with a generation. They never hold a mutable borrow of core state.
4. **Observers run after edits.** Tree-sitter, LSP, Git, diagnostics and events observe a committed change, not an intermediate edit. One command bumps the change tick once.
5. **The command queue is the only mutation path from plugins and async results.** Do not call private core methods from a callback or another thread.
6. **The renderer is read-only.** It consumes a `ViewModel` and never mutates editor state, triggers LSP work or allocates unbounded caches.
7. **No locking in core.** Use ownership and message passing. A `Mutex` around editor state is a design failure.
8. **Bounded channels.** Every cross-thread channel has a bound. Coalesce intermediate work (LSP `didChange`, Git status, search results) rather than queueing it all.
9. **Isolate failures.** A plugin, job, LSP server, parser or theme error must degrade that subsystem only. Errors are reported, never panics in the main loop.
10. **No abstraction without a second use.** Traits for the buffer, renderer backend, jobs and Lua runtime are justified by real alternatives. Do not add a trait "for mocking" when a value type or function suffices.
11. **Testability by construction.** Core behavior must be testable without a terminal, real time, network or filesystem. Inject clocks, command runners and filesystem roots where needed.

### When to add a crate

Add a crate only when at least one is true: it has a distinct dependency set, it must be reused by an external tool, or it enforces a boundary that reviewers must see. Otherwise keep the module inside its current crate. A crate that exists only to hold one file is noise.

## 8. Performance rules

Performance is a feature with a budget ([[PROJECT_SPEC#6. Performance requirements|spec §6]]), not a post-release cleanup task. Every change must leave the relevant budget intact.

### 8.1 Budgets

The normative latency, memory, startup and workload table is [[PROJECT_SPEC#6.2 Targets|PROJECT_SPEC.md §6.2]]. Do not copy it here; a stale duplicate is a bug. Before touching a hot path, identify the exact metric and workload in that table, then measure against it.

### 8.2 Rules

- **No unnecessary allocations in hot paths.** Reuse buffers, use `String::with_capacity`, avoid `Vec` per keystroke, avoid cloning whole buffers for observers. Return slices or ranges instead.
- **Do not block the main event loop.** File I/O, process spawning, LSP, Git, search, diff, parsing and large formatting belong to workers. A blocking call on the main thread is a bug even if it is "fast on my machine".
- **Prefer incremental work.** Send LSP `didChange` ranges, tree-sitter `InputEdit`s, damage rectangles and Git status deltas. Do not reparse, redraw or rescan entire structures because it is simpler.
- **Keep startup lightweight.** No grammar loading, plugin scanning, repository walking, network access or timer creation during registration. Lazy-load by command, event, filetype or keymap.
- **Lazy-load optional modules.** A disabled module costs zero. An enabled but unused module may load on first use, with the trigger documented.
- **Avoid unnecessary background processes.** Do not poll Git, LSP or files on a timer when events or file watching suffice. Do not spawn a process for something the standard library can do.
- **Measure, do not assume.** Profile before optimizing. Report raw numbers. A change justified only by intuition is not accepted.
- **Bound every cache and queue.** State the eviction policy. Unbounded growth is a leak, even if it is "just metadata".
- **Do not trade correctness for speed.** Skipping Unicode checks, edit validation or error handling to hit a microbenchmark is prohibited.

### 8.3 Profiling workflow

1. Reproduce with a fixture from `benches/fixtures/`, not a random file.
2. Record a baseline on a quiet machine using the release profile.
3. Profile with `samply` (Linux/macOS) or `perf`/ETW (Linux/Windows); save the profile with the PR.
4. Fix the dominant cost, not the most visible microbenchmark.
5. Re-run the same command and fixture; report p50/p95/p99 and RSS.
6. Add or update a regression test if the fix could regress silently.

### 8.4 Benchmark report template

```text
Metric:        keystroke latency, insert 'x' in a 100k-line Rust file
Command:       cargo xtask perf --scenario insert-100k --runs 30
Hardware:      R1 (Ryzen 7 5800X, 32 GiB, NVMe, Alacritty 0.15, 1920x1080)
Before:        p50 <measured>, p95 <measured>, p99 <measured>, RSS <measured>
After:         p50 <measured>, p95 <measured>, p99 <measured>, RSS <measured>
Change:        <computed delta>
Verification:  cargo bench -p vanta-text -- edit_insert, 20 samples
Artifact:      CI perf run <id> (raw JSON attached)
```

Values are placeholders, not example results. Never publish a number that was not measured with this protocol.

## 9. Rust coding standards

### 9.1 Toolchain and formatting

- Edition 2024, stable toolchain pinned in `rust-toolchain.toml`, MSRV 1.90. Do not use a feature that requires a newer compiler without an MSRV decision.
- `rustfmt` is mandatory. Configuration lives in `rustfmt.toml`; do not reformat unrelated code in a feature PR.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` must pass. Do not add blanket allows.
- Keep the dependency graph flat where possible. New dependencies are reviewed under [[#13. Dependency and licensing policy|§13]].

### 9.2 Naming and structure

- Types: `UpperCamelCase`. Functions, modules, variables: `snake_case`. Constants: `SCREAMING_SNAKE_CASE`.
- Name by domain meaning, not type: `line_start_byte`, not `usize_value`; `extmark_id`, not `id2`.
- Booleans read as predicates: `is_modified`, `has_swap_file`, `can_undo`.
- Avoid abbreviations except established ones in this domain (`buf`, `win`, `cmd`, `extmark`, `lsp`, `dap`, `pty`).
- One public concept per module. Use `pub(crate)` by default; expose `pub` only what another crate needs.
- Functions should fit on a screen and do one thing. If a function needs a comment to explain its sections, split it.
- Prefer `match` on enums over stringly state. Prefer newtypes over raw integers for IDs, byte offsets and display columns.
- Do not use global mutable state. Module-level statics are limited to immutable tables and compile-time constants. Shared runtime state belongs to `Editor` or a service.

### 9.3 Errors

- Library crates define typed errors with `thiserror`; the binary and `xtask` may use `anyhow` for top-level context.
- Error variants describe what failed and carry the context needed to act: path, range, command, register.
- Use `?` and `map_err` with context. Do not convert an error into `()` and log it.
- User-facing errors are stable messages, optionally with a Vim-like code. Internal errors carry a correlation ID and are logged.
- `unwrap()` and `expect()` are allowed only for invariants proven by construction, with a message explaining the invariant. They are forbidden on user input, file contents, network data, Lua values and process output.
- Do not use `panic!` for recoverable conditions. `unreachable!()` must be genuinely unreachable and documented.

```rust
#[derive(Debug, thiserror::Error)]
pub enum BufferError {
    #[error("buffer {buffer} is not modifiable")]
    NotModifiable { buffer: BufferId },
    #[error("range {start}..{end} is out of bounds for buffer of {len} bytes")]
    RangeOutOfBounds { start: usize, end: usize, len: usize },
    #[error("edit would split a grapheme cluster at byte {offset}")]
    GraphemeBoundary { offset: usize },
    #[error("undo transaction failed: {0}")]
    Undo(#[from] UndoError),
}

pub fn apply(&mut self, edit: Edit) -> Result<(), BufferError> {
    if !self.modifiable {
        return Err(BufferError::NotModifiable { buffer: self.id });
    }
    self.validate_range(edit.start, edit.end)?;
    self.rope.remove(edit.start..edit.end);
    self.rope.insert(edit.start, &edit.replacement);
    Ok(())
}
```

### 9.4 Unsafe code

- `unsafe` is forbidden in `vanta-core`, `vanta-text`, `vanta-parse`, `vanta-ex`, `vanta-undo`, `vanta-config` and `vanta-render`.
- Allowed only in `vanta-fs`, `vanta-pty`, `vanta-term` and other FFI/platform code, with a `SAFETY:` comment on every block that states the invariant, why it holds, and what would break it.
- No `unsafe` for a micro-optimization without a benchmark and an ADR. Safe code that is fast enough is preferred.
- Run Miri on pure-Rust crates where feasible. FFI crates get targeted tests and sanitizer runs in CI.

### 9.5 Public APIs and documentation

- Every public item has a doc comment that states what it does, its units, panics, errors and invariants.
- Document the unit of every position and length: bytes, Unicode scalars, graphemes or cells.
- Examples in doc comments must compile and be tested (`cargo test --doc`).
- Do not expose internal types from another crate without a wrapper. Do not make a field public to avoid writing an accessor.
- Deprecations use `#[deprecated]` with a replacement and a removal version. The Lua API follows the same rule.

### 9.6 Tests and logging in code

- Unit tests live beside the code in `#[cfg(test)] mod tests`; integration tests live in `tests/`.
- Test names describe behavior: `undo_after_branch_preserves_original_path`, not `test_undo_2`.
- Logs use the structured logging facade with fields, not string concatenation. Never log file contents, registers, clipboard data or environment values.
- Temporary instrumentation is removed before review; permanent instrumentation is behind a debug flag and has a test.

## 10. Unicode and cross-platform rules

### 10.1 Unicode

- Internal text is UTF-8. Never index by character count in hot paths; byte offsets are the storage unit, and conversions are explicit.
- Cursor movement and edits operate on grapheme clusters. A deletion must never split a combining sequence, ZWJ emoji or variation selector.
- Display width is computed per grapheme cluster, not per byte or scalar. Tabs expand to the next `tabstop` column.
- Do not normalize text. Preserve bytes exactly; case-insensitive search uses Unicode case folding, not lowercasing.
- Handle invalid UTF-8 explicitly. File operations must not silently replace bytes; display may use lossy conversion, but only at the final rendering boundary.
- Control characters are rendered visibly and never written raw to the terminal. This includes `ESC`, `BEL`, `OSC`, `DCS`, `APC` and C1 controls.
- Test with ASCII, accented Latin, CJK wide characters, combining marks, emoji ZWJ sequences, flags, RTL text and invalid bytes. A feature that only works on ASCII is not done.

### 10.2 Cross-platform

- Put platform-specific code in `vanta-fs`, `vanta-term`, `vanta-pty` or a clearly named `platform` module. Do not scatter `cfg` branches through core logic.
- Use `Path` and `PathBuf`, not string concatenation. Do not use `to_string_lossy` for file operations; it is a display-only conversion.
- Treat filesystem case sensitivity, long paths, symlinks, line endings and atomic rename as platform-dependent. Test the Windows path explicitly.
- Clipboard, PTY, process termination and terminal capabilities go through traits or platform modules. A missing capability degrades; it does not crash.
- Use `cfg!(target_os = ...)` only for behavior, `#[cfg(...)]` for compilation. Avoid target-family assumptions that exclude WASM or BSD unnecessarily.
- Do not change behavior based on locale. Formatting uses explicit formats; sorting uses byte order unless the feature specifies otherwise.
- Run the affected tests on all three platforms. If a platform cannot be tested locally, say so and rely on CI rather than guessing.

## 11. Testing standards

### 11.1 Test layers

| Layer | Purpose | Location | Runs |
|---|---|---|---|
| Unit | Functions, parsers, range math, state machines | Beside code | Every change |
| Property | Invariants over generated inputs | `tests/property` or crate tests | Every change affecting core |
| PTY | Real binary, real terminal bytes, screen assertions | `tests/pty` | Every UI or keymap change |
| Differential | Same input in Vanta and Neovim | `tests/compat` | Every compatibility change |
| Fuzz | Attacker-controlled parsers and decoders | `fuzz/` | CI corpus + nightly |
| Benchmark | Latency, throughput, memory | `benches/` | Perf-sensitive changes and nightly |
| Soak | Long-running stability | `xtask soak` | Release candidate |

### 11.2 What to test for a change

- **Parser or mode change:** transition table, invalid input, count/register combinations, macro replay.
- **Buffer or undo change:** property tests for round-trip, line index, inverse edits, grapheme boundaries.
- **File or persistence change:** encoding fixtures, crash at every write step, permissions, corruption.
- **UI or renderer change:** PTY screen snapshots at multiple widths and color depths, resize, wide characters.
- **LSP, Git or DAP change:** a stub server/repository/adapter, delayed and out-of-order responses, cancellation, crash/restart.
- **Module change:** enabled behavior, disabled absence, startup cost, dependency interactions, config validation.
- **Performance change:** before/after benchmark plus a regression test where stable.

### 11.3 Property invariants

At minimum, maintain these:

1. Applying an edit and its inverse restores the exact bytes.
2. Line index queries agree with a naive line scan after random edits.
3. Every cursor position is on a grapheme boundary and within the buffer.
4. Undo followed by redo returns the exact buffer and cursor state.
5. Extmarks never point outside the buffer and preserve gravity semantics.
6. Random command sequences never panic, corrupt the undo tree or produce an invalid selection.
7. Swap/shada/undo serialization round-trips or fails with a typed error, never panics.

Use `proptest` with shrinking and commit discovered regressions as deterministic cases.

### 11.4 PTY tests

- Use `portable-pty` to spawn the release or debug binary with a fixed size (for example 120×40), feed keys, and parse output with `vte` into a screen grid.
- Assert on screen text, cursor position, attributes and mode, not on raw ANSI byte strings, except for dedicated damage tests.
- Golden snapshots live in `tests/pty/snapshots/`. Update them only with a reviewed diff; `UPDATE_SNAPSHOTS=1` is a local convenience, never a CI default.
- Wait for a frame marker or expected screen state, not a fixed sleep. Tests use fake clocks for timers; real timeouts are bounded.

### 11.5 Flakiness

- A flaky test is a bug. Do not add retries to hide it.
- Quarantine requires an issue link, an owner and a removal date. A quarantined test cannot cover a P0 or P1 requirement at release.
- Tests must not depend on network access, developer home directories, locale, current time or terminal size unless injected.
- If a test is slow, make it faster or mark it as an explicit soak/performance test; do not silently skip it.

### 11.6 CI matrix

- Linux x86_64 (glibc), Linux musl, macOS arm64 and x86_64, Windows x86_64.
- Stable and MSRV toolchains; `--all-features` for compile checks.
- Differential corpus with Neovim installed at the pinned version.
- Fuzz corpus regression and benchmark smoke tests.
- Cross-platform PTY tests for key decoding, Unicode width and resize.

Coverage is reported for core crates but is not a gate by itself. A high-coverage function with no edge-case assertions is not tested.

## 12. Module and plugin rules

### 12.1 First-party modules

- A new module needs a specification entry, a tier (Core/Optional/External), a default state, a dependency list, a cost tier and acceptance criteria.
- Registration performs no I/O, no parsing, no repository scan and no process spawn. Such work happens on first use or on a documented event.
- Disabled modules install no mappings, commands, autocmds, timers or jobs. Static manifest trigger stubs are allowed because they only load the module and are absent when it is unavailable.
- Modules never duplicate functionality. If a feature overlaps an existing module, extend that module or document why the overlap is not real.
- Modules expose a narrow Rust API and a validated Lua configuration schema. Unknown configuration keys are errors, not silently ignored.
- Module errors are isolated and reported. A module that fails to initialize is marked failed and does not break startup.
- Every module has tests for enabled behavior, disabled absence and dependency resolution.

### 12.2 Plugins

- Lua plugins use the documented `vanta.*` API only. They must not depend on internal crate names, unstable implementation details or Neovim APIs.
- Plugins are trusted code in 1.0; documentation must state that clearly. Do not imply sandboxing that does not exist.
- A plugin must handle errors without crashing the editor, and must not block the main thread with synchronous I/O.
- Plugin loading failures are logged with a traceback and isolated. Do not abort startup because one plugin is broken.
- Public plugin APIs are versioned with `vanta.api_version`; breaking changes require a deprecation period.
- Do not add a `vim` compatibility shim, `vim.cmd` or Vimscript evaluation. If a user asks for Neovim plugin compatibility, point to spec [[PROJECT_SPEC#11.4 Plugin compatibility statement|§11.4]].

### 12.3 Example Lua configuration

```lua
-- ~/.config/vanta/init.lua
vanta.opt.number = true
vanta.opt.relativenumber = true
vanta.opt.expandtab = true
vanta.opt.shiftwidth = 4

vanta.keymap.set("n", "<Leader>f", "<Cmd>Picker files<CR>", { desc = "Find files" })
vanta.keymap.set("n", "<Leader>e", "<Cmd>Explore<CR>", { desc = "File explorer" })

vanta.autocmd("FileType", {
  pattern = { "rust", "python", "go" },
  callback = function(args) vanta.opt[args.buf].format_on_save = false end,
})

vanta.lsp.config("rust", {
  cmd = { "rust-analyzer" },
  root_markers = { "Cargo.toml" },
  filetypes = { "rust" },
})
```

## 13. Dependency and licensing policy

### 13.1 Allowed licenses

Dependencies must be compatible with Vanta's MIT OR Apache-2.0 licensing. The allowlist is enforced by `cargo-deny`:

| Allowed | Notes |
|---|---|
| MIT, Apache-2.0, MIT OR Apache-2.0 | Preferred |
| BSD-2-Clause, BSD-3-Clause, ISC | Allowed |
| Unicode-3.0, Zlib | Allowed |
| MPL-2.0 | Allowed for libraries; file-level obligations are documented |
| CC0-1.0, Unlicense | Allowed for data and small utilities |

Denied: GPL, AGPL, LGPL, SSPL, BUSL, Commons Clause and any license that requires source disclosure of the editor. A transitive dependency with a denied license blocks the change until it is replaced or an explicit legal decision is recorded.

### 13.2 Adding a dependency

A new dependency requires a written justification covering:

1. **Need:** what it does that the standard library or an existing dependency cannot.
2. **Alternatives:** at least one alternative, including writing it in-house, and why it was rejected.
3. **Maintenance:** last release, issue activity, number of maintainers, known unmaintained status.
4. **License:** SPDX expression and allowlist check.
5. **Security:** open advisories, `unsafe` usage, native code, build scripts.
6. **Cost:** compile time, binary size, feature flags, MSRV, transitive dependency count.
7. **Exit plan:** how the dependency is isolated if it becomes unmaintained.

Dependencies that are already approved by the specification's architecture ([[PROJECT_SPEC#5.2 Crates and dependency direction|§5.2]]) do not need a new ADR, but version bumps and feature changes still require review. Do not add a second crate for a problem already solved by an approved crate.

### 13.3 Rules

- Commit `Cargo.lock`. Applications pin exact versions; libraries in the workspace use compatible ranges internally.
- `cargo deny check` runs in CI for licenses, advisories, bans and sources.
- Prefer crates with no build script or native dependency. Native dependencies must be documented in the platform matrix.
- Avoid features that pull in large runtime stacks (TLS, HTTP clients, async executors) unless the feature requires them.
- Dev-dependencies are subject to the same license policy but not the binary-size budget.
- Do not vendor or fork a dependency without an ADR and a documented patch-maintenance plan.
- Do not copy code from another project without recording its license in `THIRD_PARTY_LICENSES`.

## 14. Security rules

- **Never interpolate untrusted text into a shell command.** Use argv, pass `--` before paths, and validate paths used for file operations.
- **Never render buffer text directly to the terminal.** Build cells, sanitize control characters, and keep OSC/CSI sequences inert.
- **Never log secrets.** Registers, clipboard contents, environment values, tokens and file contents are off-limits in logs and crash reports.
- **Respect trust boundaries.** Do not auto-start LSP, Git, debug adapters or local config in an untrusted workspace.
- **Create files safely.** Temp, swap, undo and shada files use random names, `O_EXCL`/`CREATE_NEW`, and restrictive permissions.
- **Treat all external input as hostile:** files, LSP/DAP messages, Git output, process output, clipboard, Lua values and RPC frames. Validate lengths, bounds and UTF-8 before use.
- **Fuzz every parser boundary** you add or change. A parser that panics on malformed input is a security bug.
- **Do not add network access** to a feature that did not have it. Startup and idle states perform no network I/O.
- **Report vulnerabilities privately.** Do not open a public issue for an unpatched security problem; follow `SECURITY.md` once it exists.

## 15. Documentation duties


- You have to create Documentation for anything relevant for Developers and Users
- All generated documentation must reside in `G:\Obsidian\Vault\Vanta\Documentation`.
- Use Obsidian-style links: `[[Note Name]]`.
- Write in concise, professional technical prose using GitHub-Flavored Markdown.
- Use frontmatter

### 15.1 What must be updated

| Change | Required documentation |
|---|---|
| User-visible behavior | `runtime/doc/*.md`, changelog, migration note if it breaks workflows |
| New or changed option | Option reference, default value, scope, validation, help tag |
| New or changed command | Help entry with syntax, ranges, examples, errors |
| New module | [[PROJECT_SPEC#4. First-party modules\|Spec §4]] entry, module reference, keymaps, configuration schema |
| Architecture decision | ADR in `Documentation/adr/`, [[PROJECT_SPEC#5. Architecture\|spec §5]] if boundaries change |
| Compatibility difference | [[PROJECT_SPEC#11.2 Difference register\|Spec §11.2]], `Documentation/compatibility.md`, help entry |
| Performance change | Benchmark artifact linked in the PR and changelog for user-visible gains |
| Dependency change | ADR or dependency review note, license inventory update |
| Security behavior | `SECURITY.md` and the relevant help page |

### 15.2 ADR template

Create `Documentation/adr/NNNN-short-title.md`:

```markdown
# NNNN: Short decision title

- Status: Proposed | Accepted | Superseded by NNNN
- Date: YYYY-MM-DD
- Requirement: ARCH-xxx or N/A
- Supersedes: NNNN or none

## Context

What problem are we solving? What constraints (performance, platform, compatibility,
security, maintenance) apply? What did we try or reject?

## Decision

The decision, stated in one paragraph, followed by the concrete design.

## Consequences

Positive, negative and neutral consequences. Migration cost. What becomes harder.
What would make us revisit this decision.

## Alternatives considered

Each alternative, with the reason it was rejected.
```

### 15.3 Writing rules

- Write for an implementer who has not read the conversation history. Define terms, link requirement IDs, and include commands and file paths.
- Prefer concrete behavior over adjectives. "Starts in under 20 ms p50 on R1" beats "very fast".
- Do not duplicate the specification. Link to the relevant section and add only what is new.
- Keep examples runnable and up to date. A stale command in documentation is a bug.
- Use Obsidian wikilinks between repository documents: `[[Note#Heading|Label]]` for a section and `[[Note|Label]]` for a file. Use the heading text exactly as written, not a URL slug, and escape the alias pipe as `\|` inside tables. Validate targets in CI.
- Never claim a feature works because code exists. Documentation describes verified behavior.

## 16. Git, commit and pull request conventions

### 16.1 Branches

Use a short-lived branch named for the change:

- `feat/picker-preview`
- `fix/undo-branch-restore`
- `perf/rope-line-index`
- `docs/compatibility-register`
- `refactor/extmark-gravity`
- `test/pty-key-protocols`

Do not mix unrelated work. Rebase onto the current main branch before review; do not merge main into a feature branch repeatedly.

### 16.2 Commits

- Write imperative subjects: `add extmark gravity`, not `added` or `fixes`.
- Keep the subject under 72 characters. Use the body to explain **why**, the behavior change, and the requirement IDs.
- One logical change per commit. Formatting-only changes are separate.
- Do not commit generated files, build output, editor swap files, coverage data, secrets, local paths or machine-specific configuration.
- Do not commit, push, tag or open a pull request unless the user explicitly asks. When asked, stage only the files you changed.

```text
fix(undo): preserve branch cursor after redo

Redo restored the buffer bytes but reused the pre-undo cursor, so `g-`
landed on the wrong branch position. Store the post-edit cursor in the
undo block and apply it on both undo and redo.

Requirement: TXT-005
Tests: undo_redo_restores_cursor, undo_branch_after_edit
```

### 16.3 Pull request content

Every PR description includes:

1. **What changed** and why, in two or three sentences.
2. **Requirement IDs** and the governing milestone.
3. **Verification** — exact commands and results, including failures or skipped checks.
4. **Compatibility impact** — new differences, keymap changes, option changes.
5. **Performance impact** — benchmarks for hot paths, or "not on a hot path".
6. **Documentation** — files updated.
7. **Risks and rollback** — what could break and how to revert.

### 16.4 Review

- Review the diff against the specification, not just for style.
- Look for missing error handling, unbounded resources, stale generations, Unicode assumptions and platform branches.
- Verify that tests fail without the change and pass with it.
- Do not approve with unresolved P0/P1 defects or unexplained warnings.
- A review comment that identifies a repeated problem should lead to a rule in this document or a test, not only a one-off fix.

## 17. Prohibited actions and escalation

### 17.1 Prohibited

- Force-pushing, rewriting history or deleting branches that are not yours.
- Committing secrets, tokens, private keys, user data or absolute local paths.
- Disabling, deleting or weakening a test to make a check pass.
- Adding `#[allow(...)]`, `--no-verify`, `continue-on-error` or an ignore without a reviewed reason.
- Changing a default keybinding, option default or public API without a specification update and compatibility entry.
- Implementing a placeholder and describing it as complete.
- Adding telemetry, analytics, auto-update or hidden network calls.
- Auto-installing or auto-updating language servers, formatters, linters or plugins.
- Running destructive commands against user files, repositories or system configuration.
- Bypassing CI, review or the dependency policy "just this once".
- Claiming that a Vim/Neovim plugin runs because a first-party module provides similar functionality.

### 17.2 Escalate instead of guessing

Stop and ask when:

- The specification is ambiguous or two requirements conflict.
- The behavior differs between Vim and Neovim and [[PROJECT_SPEC#11. Compatibility and intentional differences|§11]] does not decide it.
- A performance target cannot be met without changing the architecture.
- A new dependency or crate boundary is needed.
- A change affects public API, default keymaps, file formats or user data.
- A security or data-loss risk appears.
- A platform cannot be tested and the change is platform-specific.
- The requested task conflicts with a documented non-goal.

When escalating, state what you tried, what you observed, the options and your recommendation. Do not silently choose a scope reduction.

## 18. Checklists

### 18.1 Pull request checklist

- [ ] Requirement ID and governing milestone are cited; the work is P0/P1 or has an approved P2/1.x gate.
- [ ] A failing test or differential case preceded the fix; coverage includes failure paths and edge cases.
- [ ] `cargo fmt --all --check` passes, the build has zero compiler warnings, Clippy with `-D warnings` is clean, and the affected crate tests pass.
- [ ] No unapproved dependency, crate, feature flag or public API was added.
- [ ] Compatibility impact is recorded in `Documentation/compatibility.md` when behavior differs.
- [ ] User-facing docs, help and changelog are updated; links and wikilink targets are valid.
- [ ] Hot-path work includes measured before/after numbers within the [[PROJECT_SPEC#6.2 Targets|PROJECT_SPEC.md §6.2]] budget.

### 18.2 Release candidate checklist

- [ ] Every P0 and P1 requirement is verified with automated evidence or a recorded manual procedure.
- [ ] Cross-platform CI is green on Linux, macOS and Windows, including PTY and Unicode fixtures.
- [ ] Performance benchmarks meet [[PROJECT_SPEC#6.2 Targets|PROJECT_SPEC.md §6.2]] with published raw data.
- [ ] The differential compatibility corpus is green on three platforms with every exclusion documented.
- [ ] The 72-hour soak passed with bounded memory, no stuck jobs and idle CPU within budget.
- [ ] Crash, swap and undo recovery were tested by killing the process at every write stage.
- [ ] Help, migration guide, compatibility report and changelog match verified behavior.
- [ ] Release artifacts are checksummed, signed and installable without a language runtime.

## 19. Quick reference

### 19.1 Commands

| Command | Purpose |
|---|---|
| `cargo xtask check` | Format, clippy, tests, docs, licenses |
| `cargo xtask test --all` | Full test suite with platform guards |
| `cargo xtask compat --filter <area>` | Differential corpus against pinned Neovim |
| `cargo xtask bench` | Criterion microbenchmarks |
| `cargo xtask perf --scenario <name>` | PTY macrobenchmark with latency and RSS |
| `cargo xtask startup` | Startup trace and hyperfine comparison |
| `cargo xtask fuzz --target <name>` | Fuzz a parser boundary |
| `cargo xtask soak --hours 72` | Release soak test |
| `cargo deny check` | License, advisory, ban and source policy |
| `cargo miri test -p <crate>` | Undefined-behavior check for safe crates |

### 19.2 Paths and environment

| Item | Location |
|---|---|
| Config | `~/.config/vanta/init.lua` (Windows: `%APPDATA%/vanta/init.lua`) |
| Data and site plugins | `~/.local/share/vanta/` |
| State (shada, undo, swap fallback) | `~/.local/state/vanta/` |
| Cache | `~/.cache/vanta/` |
| Logs | State directory, `log/` |
| Benchmark fixtures | `benches/fixtures/` |
| Differential cases | `tests/compat/cases/` |
| Documentation | `Documentation/` — ADRs, compatibility, verification |
| ADRs | `Documentation/adr/` |

Overrides: `VANTA_CONFIG_DIR`, `VANTA_DATA_DIR`, `VANTA_STATE_DIR`, `VANTA_CACHE_DIR`, `VANTA_LOG`.

### 19.3 Where to look

| Question | Section |
|---|---|
| What must this feature do? | [[PROJECT_SPEC#3A Modal editing\|Spec §3A]]–[[PROJECT_SPEC#3H Neovim-inspired architecture\|§3H]] |
| May this feature ship built in? | [[PROJECT_SPEC#4. First-party modules\|Spec §4]] |
| Which crate owns this code? | [[PROJECT_SPEC#5.2 Crates and dependency direction\|Spec §5.2]]–[[PROJECT_SPEC#5.3 Component responsibilities\|§5.3]] |
| What is the latency budget? | [[PROJECT_SPEC#6.2 Targets\|Spec §6.2]] |
| How does this behave on Windows? | [[PROJECT_SPEC#7. Cross-platform support\|Spec §7]] |
| Is this a security risk? | [[PROJECT_SPEC#8. Security and reliability\|Spec §8]] |
| When may this be implemented? | [[PROJECT_SPEC#10. Roadmap\|Spec §10]] |
| Is this difference acceptable? | [[PROJECT_SPEC#11. Compatibility and intentional differences\|Spec §11]] |
| How do I test this? | AGENTS [[#11. Testing standards\|§11]] |
| How do I commit this? | AGENTS [[#16. Git, commit and pull request conventions\|§16]] |

## 20. Maintaining this document

- `AGENTS.md` is normative for workflow. Changes require a review that checks alignment with `PROJECT_SPEC.md` and the current milestone.
- Keep it actionable. A rule that cannot be checked by a reviewer or a test is probably a preference, not a rule.
- Do not duplicate specification content. Link to the section instead; duplication drifts.
- When a recurring review comment appears twice, add a rule here and, where possible, a lint, test or CI check that enforces it.
- Update the version and date when the rules change. Record substantive changes in the repository changelog.
- If a rule is no longer true, remove it. A stale rule teaches agents to do the wrong thing.

End of agent instructions.
