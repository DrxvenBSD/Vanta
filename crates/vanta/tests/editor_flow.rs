//! End-to-end tests for the minimal editor loop.
//!
//! A scripted [`Terminal`] drives the same `Editor::run` loop the binary uses:
//! open a real temporary file, type, save, re-read it, and quit.

use std::collections::VecDeque;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use vanta::editor::Editor;
use vanta::terminal::{Frame, InputEvent, ScreenPos, Size, Terminal, TextStyle};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct ScriptedTerminal {
    size: Size,
    sizes: VecDeque<Size>,
    events: VecDeque<InputEvent>,
    frames: Vec<Frame>,
}

impl ScriptedTerminal {
    fn new(events: impl IntoIterator<Item = InputEvent>) -> Self {
        Self {
            size: Size {
                width: 200,
                height: 10,
            },
            sizes: VecDeque::new(),
            events: events.into_iter().collect(),
            frames: Vec::new(),
        }
    }

    fn with_sizes(
        events: impl IntoIterator<Item = InputEvent>,
        sizes: impl IntoIterator<Item = Size>,
    ) -> Self {
        let mut terminal = Self::new(events);
        terminal.sizes = sizes.into_iter().collect();
        terminal
    }
}

impl Terminal for ScriptedTerminal {
    fn size(&mut self) -> io::Result<Size> {
        if let Some(size) = self.sizes.pop_front() {
            self.size = size;
        }
        Ok(self.size)
    }

    fn draw(&mut self, frame: &Frame) -> io::Result<()> {
        self.frames.push(frame.clone());
        Ok(())
    }

    fn read_event(&mut self) -> io::Result<InputEvent> {
        self.events
            .pop_front()
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "input script exhausted"))
    }
}

fn scratch() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "vanta-editor-{}-{}-{nanos}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).expect("create scratch directory");
    path
}

fn command(text: &str) -> Vec<InputEvent> {
    let mut events = vec![InputEvent::Char(':')];
    events.extend(text.chars().map(InputEvent::Char));
    events.push(InputEvent::Enter);
    events
}

fn search(pattern: &str) -> Vec<InputEvent> {
    let mut events = vec![InputEvent::Char('/')];
    events.extend(pattern.chars().map(InputEvent::Char));
    events.push(InputEvent::Enter);
    events
}

/// Runs `events`, writes with `:wq` and leaves the file on disk.
fn run_and_save(path: &Path, events: Vec<InputEvent>) {
    let mut events = events;
    events.extend(command("wq"));
    let mut editor = Editor::open(path).expect("open file");
    let mut terminal = ScriptedTerminal::new(events);
    editor.run(&mut terminal).expect("run editor");
}

#[test]
fn opens_types_saves_and_reloads() {
    let directory = scratch();
    let path = directory.join("note.txt");
    fs::write(&path, "hello\nworld\n").expect("seed file");

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new(
        [
            vec![
                InputEvent::Char('i'),
                InputEvent::Char('X'),
                InputEvent::Esc,
            ],
            command("w"),
            command("q"),
        ]
        .concat(),
    );

    editor.run(&mut terminal).expect("run editor");

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "Xhello\nworld\n"
    );
    assert!(
        terminal
            .frames
            .iter()
            .any(|frame| frame.rows.iter().any(|row| row.contains("Xhello"))),
        "the edited text must be drawn before saving"
    );
    assert!(
        terminal
            .frames
            .iter()
            .any(|frame| { frame.rows.last().is_some_and(|row| row.contains("NORMAL")) }),
        "the status row must show the mode"
    );
    assert!(
        terminal.frames.iter().any(|frame| {
            frame.cursor == ScreenPos { row: 0, col: 1 }
                && frame
                    .rows
                    .first()
                    .is_some_and(|row| row.starts_with("Xhello"))
        }),
        "the edited text must be drawn with the cursor after the inserted character"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn backspace_at_line_start_joins_the_previous_line() {
    let directory = scratch();
    let path = directory.join("join.txt");
    fs::write(&path, "ab\ncd\n").expect("seed file");

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new(
        [
            vec![
                InputEvent::Char('j'),
                InputEvent::Char('i'),
                InputEvent::Backspace,
                InputEvent::Esc,
            ],
            command("w"),
            command("q"),
        ]
        .concat(),
    );

    editor.run(&mut terminal).expect("run editor");

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "abcd\n"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn escape_at_line_start_does_not_wrap_to_the_previous_line() {
    let directory = scratch();
    let path = directory.join("escape.txt");
    fs::write(&path, "ab\ncd\n").expect("seed file");

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new(
        [
            vec![
                InputEvent::Char('j'),
                InputEvent::Char('i'),
                InputEvent::Esc,
                InputEvent::Char('i'),
                InputEvent::Char('X'),
                InputEvent::Esc,
            ],
            command("w"),
            command("q"),
        ]
        .concat(),
    );

    editor.run(&mut terminal).expect("run editor");

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "ab\nXcd\n",
        "Escape must keep the cursor on its line before the next insert"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn creates_and_saves_a_new_file() {
    let directory = scratch();
    let path = directory.join("new.txt");

    let mut editor = Editor::open(&path).expect("open a missing file");
    let mut terminal = ScriptedTerminal::new(
        [
            vec![
                InputEvent::Char('i'),
                InputEvent::Char('n'),
                InputEvent::Char('e'),
                InputEvent::Char('w'),
                InputEvent::Esc,
            ],
            command("w"),
            command("q"),
        ]
        .concat(),
    );

    editor.run(&mut terminal).expect("run editor");

    assert_eq!(fs::read_to_string(&path).expect("read created file"), "new");
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn undo_removes_a_whole_insert_session_and_stops_at_the_beginning() {
    let directory = scratch();
    let path = directory.join("undo.txt");
    fs::write(&path, "hello\n").expect("seed file");

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new(
        [
            vec![
                InputEvent::Char('i'),
                InputEvent::Char('a'),
                InputEvent::Char('b'),
                InputEvent::Esc,
                InputEvent::Char('u'),
            ],
            command("w"),
            vec![InputEvent::Char('u')],
            command("q"),
        ]
        .concat(),
    );

    editor.run(&mut terminal).expect("run editor");

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "hello\n",
        "one undo must remove the whole coalesced insert session, and the next undo must be a no-op"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn undo_steps_back_one_insert_session_at_a_time() {
    let directory = scratch();
    let path = directory.join("sessions.txt");
    fs::write(&path, "x\n").expect("seed file");

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new(
        [
            vec![
                InputEvent::Char('i'),
                InputEvent::Char('A'),
                InputEvent::Esc,
                InputEvent::Char('l'),
                InputEvent::Char('i'),
                InputEvent::Char('B'),
                InputEvent::Esc,
                InputEvent::Char('u'),
            ],
            command("w"),
            command("q"),
        ]
        .concat(),
    );

    editor.run(&mut terminal).expect("run editor");

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "Ax\n",
        "one undo must remove only the most recent insert session"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn undo_twice_steps_back_through_two_insert_sessions() {
    let directory = scratch();
    let path = directory.join("two-sessions.txt");
    fs::write(&path, "x\n").expect("seed file");

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new(
        [
            vec![
                InputEvent::Char('i'),
                InputEvent::Char('A'),
                InputEvent::Esc,
                InputEvent::Char('l'),
                InputEvent::Char('i'),
                InputEvent::Char('B'),
                InputEvent::Esc,
                InputEvent::Char('u'),
                InputEvent::Char('u'),
            ],
            command("w"),
            command("q"),
        ]
        .concat(),
    );

    editor.run(&mut terminal).expect("run editor");

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "x\n",
        "two undos must remove both insert sessions"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn one_insert_session_with_a_backspace_is_one_undo_block() {
    let directory = scratch();
    let path = directory.join("insert-session.txt");
    fs::write(&path, "x\n").expect("seed file");

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new(
        [
            vec![
                InputEvent::Char('i'),
                InputEvent::Char('A'),
                InputEvent::Char('B'),
                InputEvent::Backspace,
                InputEvent::Char('C'),
                InputEvent::Esc,
                InputEvent::Char('u'),
            ],
            command("w"),
            command("q"),
        ]
        .concat(),
    );

    editor.run(&mut terminal).expect("run editor");

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "x\n",
        "one undo must revert the whole Insert session, including its backspace"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn cursor_discontinuity_starts_a_new_undo_block() {
    let directory = scratch();
    let path = directory.join("cursor-break.txt");
    fs::write(&path, "x\n").expect("seed file");

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new(
        [
            vec![
                InputEvent::Char('i'),
                InputEvent::Char('A'),
                InputEvent::Left,
                InputEvent::Char('B'),
                InputEvent::Esc,
                InputEvent::Char('u'),
            ],
            command("w"),
            command("q"),
        ]
        .concat(),
    );

    editor.run(&mut terminal).expect("run editor");

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "Ax\n",
        "typing after a cursor move must be a separate undo block"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn undo_before_the_first_change_is_a_no_op() {
    let directory = scratch();
    let path = directory.join("beginning.txt");
    fs::write(&path, "hello\n").expect("seed file");

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new(
        [
            vec![InputEvent::Char('u'), InputEvent::Char('u')],
            command("q"),
        ]
        .concat(),
    );

    editor.run(&mut terminal).expect("run editor");

    assert_eq!(
        fs::read_to_string(&path).expect("read untouched file"),
        "hello\n"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn redo_after_a_new_edit_is_discarded() {
    let directory = scratch();
    let path = directory.join("redo.txt");
    fs::write(&path, "hello\n").expect("seed file");

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new(
        [
            vec![
                InputEvent::Char('i'),
                InputEvent::Char('X'),
                InputEvent::Esc,
                InputEvent::Char('u'),
                InputEvent::Char('i'),
                InputEvent::Char('Y'),
                InputEvent::Esc,
                InputEvent::Ctrl('r'),
            ],
            command("w"),
            command("q"),
        ]
        .concat(),
    );

    editor.run(&mut terminal).expect("run editor");

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "Yhello\n",
        "a new edit must discard the redo branch"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn undo_to_saved_content_clears_modified_and_allows_quit() {
    let directory = scratch();
    let path = directory.join("dirty.txt");
    fs::write(&path, "hello\n").expect("seed file");

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new(
        [
            vec![
                InputEvent::Char('i'),
                InputEvent::Char('X'),
                InputEvent::Esc,
            ],
            command("q"),
            vec![InputEvent::Char('u')],
            command("q"),
        ]
        .concat(),
    );

    editor.run(&mut terminal).expect("run editor");

    assert!(
        terminal
            .frames
            .iter()
            .any(|frame| { frame.rows.last().is_some_and(|row| row.contains("E37")) }),
        "the first quit must refuse because the buffer is modified"
    );
    assert_eq!(
        fs::read_to_string(&path).expect("read untouched file"),
        "hello\n",
        "undoing back to the saved content must make quit succeed without writing"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn undo_restores_the_cursor_before_the_change() {
    let directory = scratch();
    let path = directory.join("undo-cursor.txt");
    fs::write(&path, "ab\ncd\n").expect("seed file");

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new(
        [
            vec![
                InputEvent::Char('j'),
                InputEvent::Char('l'),
                InputEvent::Char('i'),
                InputEvent::Char('X'),
                InputEvent::Esc,
                InputEvent::Char('u'),
                InputEvent::Char('i'),
                InputEvent::Char('Y'),
                InputEvent::Esc,
            ],
            command("w"),
            command("q"),
        ]
        .concat(),
    );

    editor.run(&mut terminal).expect("run editor");

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "ab\ncYd\n",
        "undo must put the cursor back at the pre-change position"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn redo_restores_the_cursor_after_the_change() {
    let directory = scratch();
    let path = directory.join("redo-cursor.txt");
    fs::write(&path, "ab\ncd\n").expect("seed file");

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new(
        [
            vec![
                InputEvent::Char('j'),
                InputEvent::Char('l'),
                InputEvent::Char('i'),
                InputEvent::Char('X'),
                InputEvent::Esc,
                InputEvent::Char('u'),
                InputEvent::Ctrl('r'),
                InputEvent::Char('i'),
                InputEvent::Char('Y'),
                InputEvent::Esc,
            ],
            command("w"),
            command("q"),
        ]
        .concat(),
    );

    editor.run(&mut terminal).expect("run editor");

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "ab\ncXYd\n",
        "redo must put the cursor after the restored edit, before the following character"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn quit_refuses_to_discard_unsaved_changes() {
    let directory = scratch();
    let path = directory.join("guard.txt");
    fs::write(&path, "a\n").expect("seed file");

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new(
        [
            vec![
                InputEvent::Char('i'),
                InputEvent::Char('X'),
                InputEvent::Esc,
            ],
            command("q"),
            command("q!"),
        ]
        .concat(),
    );

    editor.run(&mut terminal).expect("run editor");

    assert_eq!(
        fs::read_to_string(&path).expect("read untouched file"),
        "a\n",
        "a refused quit must not write or discard anything"
    );
    assert!(
        terminal
            .frames
            .iter()
            .any(|frame| { frame.rows.last().is_some_and(|row| row.contains("E37")) }),
        "the refusal must be visible in the status row"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn linewise_yank_and_put_round_trip() {
    let directory = scratch();
    let path = directory.join("linewise-put.txt");
    fs::write(&path, "a\nb\nc\n").expect("seed file");

    run_and_save(
        &path,
        vec![
            InputEvent::Char('j'),
            InputEvent::Char('y'),
            InputEvent::Char('y'),
            InputEvent::Char('p'),
        ],
    );

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "a\nb\nb\nc\n",
        "yy p must insert a whole line after the current line"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn linewise_yank_and_put_after_a_final_line_without_eol() {
    let directory = scratch();
    let path = directory.join("linewise-put-no-eol.txt");
    fs::write(&path, "a\nb").expect("seed file");

    run_and_save(
        &path,
        vec![
            InputEvent::Char('j'),
            InputEvent::Char('y'),
            InputEvent::Char('y'),
            InputEvent::Char('p'),
        ],
    );

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "a\nb\nb",
        "linewise p at a no-EOL end must add one separator, not a trailing EOL"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn linewise_yank_does_not_dirty_or_create_history() {
    let directory = scratch();
    let path = directory.join("yank-clean.txt");
    fs::write(&path, "a\nb\n").expect("seed file");

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new(
        [
            vec![
                InputEvent::Char('y'),
                InputEvent::Char('y'),
                InputEvent::Char('u'),
            ],
            command("q"),
        ]
        .concat(),
    );
    editor.run(&mut terminal).expect("run editor");

    assert_eq!(
        fs::read_to_string(&path).expect("read untouched file"),
        "a\nb\n",
        "yy must not change the buffer or make quit refuse"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn linewise_delete_then_put_restores_exact_bytes() {
    let cases = [
        (
            "middle",
            "a\nb\nc\n",
            vec![
                InputEvent::Char('j'),
                InputEvent::Char('d'),
                InputEvent::Char('d'),
                InputEvent::Char('P'),
            ],
        ),
        (
            "last-without-eol",
            "a\nb",
            vec![
                InputEvent::Char('j'),
                InputEvent::Char('d'),
                InputEvent::Char('d'),
                InputEvent::Char('p'),
            ],
        ),
    ];

    for (name, original, events) in cases {
        let directory = scratch();
        let path = directory.join(format!("linewise-delete-put-{name}.txt"));
        fs::write(&path, original).expect("seed file");
        run_and_save(&path, events);
        assert_eq!(
            fs::read_to_string(&path).expect("read saved file"),
            original,
            "{name} must round-trip through dd and put"
        );
        fs::remove_dir_all(&directory).expect("clean scratch directory");
    }
}

#[test]
fn charwise_delete_then_put_restores_exact_bytes() {
    let cases = [
        (
            "after",
            vec![
                InputEvent::Char('l'),
                InputEvent::Char('x'),
                InputEvent::Char('p'),
            ],
            "ab\n",
        ),
        (
            "before",
            vec![
                InputEvent::Char('l'),
                InputEvent::Char('x'),
                InputEvent::Char('P'),
            ],
            "abc\n",
        ),
    ];

    for (name, events, original) in cases {
        let directory = scratch();
        let path = directory.join(format!("charwise-delete-put-{name}.txt"));
        fs::write(&path, original).expect("seed file");
        run_and_save(&path, events);
        assert_eq!(
            fs::read_to_string(&path).expect("read saved file"),
            original,
            "charwise {name} put must restore the deleted character"
        );
        fs::remove_dir_all(&directory).expect("clean scratch directory");
    }
}

#[test]
fn crlf_linewise_operations_preserve_crlf() {
    let cases = [
        (
            "yank-put",
            "a\r\nb\r\nc\r\n",
            vec![
                InputEvent::Char('j'),
                InputEvent::Char('y'),
                InputEvent::Char('y'),
                InputEvent::Char('j'),
                InputEvent::Char('p'),
            ],
            "a\r\nb\r\nc\r\nb\r\n",
        ),
        (
            "delete-put",
            "a\r\nb\r\nc\r\n",
            vec![
                InputEvent::Char('j'),
                InputEvent::Char('d'),
                InputEvent::Char('d'),
                InputEvent::Char('P'),
            ],
            "a\r\nb\r\nc\r\n",
        ),
        (
            "open-line",
            "a\r\nb\r\n",
            vec![
                InputEvent::Char('o'),
                InputEvent::Char('X'),
                InputEvent::Esc,
            ],
            "a\r\nX\r\nb\r\n",
        ),
    ];

    for (name, initial, events, expected) in cases {
        let directory = scratch();
        let path = directory.join(format!("{name}.txt"));
        fs::write(&path, initial).expect("seed file");
        run_and_save(&path, events);
        assert_eq!(
            fs::read_to_string(&path).expect("read saved file"),
            expected,
            "{name} must preserve CRLF"
        );
        fs::remove_dir_all(&directory).expect("clean scratch directory");
    }
}

#[test]
fn open_line_below_and_above_inserts_whole_lines() {
    let cases = [
        (
            "below",
            vec![
                InputEvent::Char('j'),
                InputEvent::Char('o'),
                InputEvent::Char('X'),
                InputEvent::Esc,
            ],
            "a\nb\nX\nc\n",
        ),
        (
            "above",
            vec![
                InputEvent::Char('j'),
                InputEvent::Char('O'),
                InputEvent::Char('X'),
                InputEvent::Esc,
            ],
            "a\nX\nb\nc\n",
        ),
    ];

    for (name, events, expected) in cases {
        let directory = scratch();
        let path = directory.join(format!("open-{name}.txt"));
        fs::write(&path, "a\nb\nc\n").expect("seed file");
        run_and_save(&path, events);
        assert_eq!(
            fs::read_to_string(&path).expect("read saved file"),
            expected,
            "o/O must open a line {name} the cursor and enter Insert"
        );
        fs::remove_dir_all(&directory).expect("clean scratch directory");
    }
}

#[test]
fn word_delete_uses_the_word_motion() {
    let cases = [
        (
            "dw",
            vec![
                InputEvent::Char('w'),
                InputEvent::Char('d'),
                InputEvent::Char('w'),
            ],
        ),
        (
            "db",
            vec![
                InputEvent::Char('w'),
                InputEvent::Char('w'),
                InputEvent::Char('d'),
                InputEvent::Char('b'),
            ],
        ),
    ];

    for (name, events) in cases {
        let directory = scratch();
        let path = directory.join(format!("word-{name}.txt"));
        fs::write(&path, "foo bar baz\n").expect("seed file");
        run_and_save(&path, events);
        assert_eq!(
            fs::read_to_string(&path).expect("read saved file"),
            "foo baz\n",
            "{name} must delete the motion's word range"
        );
        fs::remove_dir_all(&directory).expect("clean scratch directory");
    }
}

#[test]
fn x_deletes_a_whole_grapheme_cluster() {
    let directory = scratch();
    let path = directory.join("grapheme.txt");
    fs::write(&path, "e\u{301}x\n").expect("seed file");

    run_and_save(&path, vec![InputEvent::Char('x')]);

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "x\n",
        "x must not split a base character from its combining mark"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn empty_buffer_and_last_line_deletes_do_not_panic() {
    let directory = scratch();

    let empty = directory.join("empty.txt");
    fs::write(&empty, "").expect("seed empty file");
    run_and_save(
        &empty,
        vec![
            InputEvent::Char('x'),
            InputEvent::Char('d'),
            InputEvent::Char('d'),
            InputEvent::Char('d'),
            InputEvent::Char('w'),
            InputEvent::Char('b'),
            InputEvent::Char('u'),
        ],
    );
    assert_eq!(
        fs::read_to_string(&empty).expect("read empty file"),
        "",
        "empty-buffer verbs must be no-ops"
    );

    let without_eol = directory.join("last-without-eol.txt");
    fs::write(&without_eol, "a\nb").expect("seed file");
    run_and_save(
        &without_eol,
        vec![
            InputEvent::Char('j'),
            InputEvent::Char('d'),
            InputEvent::Char('d'),
        ],
    );
    assert_eq!(
        fs::read_to_string(&without_eol).expect("read file"),
        "a",
        "dd on a final line without an EOL must remove its preceding separator"
    );

    let trailing_empty = directory.join("trailing-empty.txt");
    fs::write(&trailing_empty, "a\n").expect("seed file");
    run_and_save(
        &trailing_empty,
        vec![
            InputEvent::Char('j'),
            InputEvent::Char('d'),
            InputEvent::Char('d'),
        ],
    );
    assert_eq!(
        fs::read_to_string(&trailing_empty).expect("read file"),
        "",
        "dd on the synthetic final empty line must target the last visible line"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn undo_restores_each_verb_in_one_step() {
    let cases = [
        (
            "x",
            "abc\n",
            vec![InputEvent::Char('l'), InputEvent::Char('x')],
        ),
        (
            "dd",
            "a\nb\nc\n",
            vec![
                InputEvent::Char('j'),
                InputEvent::Char('d'),
                InputEvent::Char('d'),
            ],
        ),
        (
            "dd-last-no-eol",
            "a\nb",
            vec![
                InputEvent::Char('j'),
                InputEvent::Char('d'),
                InputEvent::Char('d'),
            ],
        ),
        (
            "p",
            "a\nb\n",
            vec![
                InputEvent::Char('y'),
                InputEvent::Char('y'),
                InputEvent::Char('p'),
            ],
        ),
        (
            "P",
            "a\nb\n",
            vec![
                InputEvent::Char('y'),
                InputEvent::Char('y'),
                InputEvent::Char('P'),
            ],
        ),
        (
            "o",
            "a\nb\n",
            vec![
                InputEvent::Char('o'),
                InputEvent::Char('X'),
                InputEvent::Esc,
            ],
        ),
        (
            "O",
            "a\nb\n",
            vec![
                InputEvent::Char('O'),
                InputEvent::Char('X'),
                InputEvent::Esc,
            ],
        ),
        (
            "dw",
            "foo bar baz\n",
            vec![
                InputEvent::Char('w'),
                InputEvent::Char('d'),
                InputEvent::Char('w'),
            ],
        ),
        (
            "db",
            "foo bar baz\n",
            vec![
                InputEvent::Char('w'),
                InputEvent::Char('w'),
                InputEvent::Char('d'),
                InputEvent::Char('b'),
            ],
        ),
    ];

    for (name, initial, keys) in cases {
        let directory = scratch();
        let path = directory.join(format!("undo-{name}.txt"));
        fs::write(&path, initial).expect("seed file");

        let mut events = keys;
        events.push(InputEvent::Char('u'));
        run_and_save(&path, events);

        assert_eq!(
            fs::read_to_string(&path).expect("read saved file"),
            initial,
            "one u must undo {name} completely"
        );
        fs::remove_dir_all(&directory).expect("clean scratch directory");
    }
}

#[test]
fn visual_charwise_selection_crosses_a_line_boundary_and_deletes() {
    let directory = scratch();
    let path = directory.join("visual-cross-line.txt");
    fs::write(&path, "abc\ndef\n").expect("seed file");

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new(
        [
            vec![
                InputEvent::Char('v'),
                InputEvent::Char('j'),
                InputEvent::Char('d'),
            ],
            command("wq"),
        ]
        .concat(),
    );
    editor.run(&mut terminal).expect("run editor");

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "ef\n",
        "a Visual delete from line 0 col 0 through line 1 col 0 must remove both endpoints"
    );
    assert!(
        terminal.frames.iter().any(|frame| {
            frame
                .rows
                .last()
                .is_some_and(|row| row.contains("VISUAL  2:1"))
                && frame
                    .selection
                    .iter()
                    .any(|span| span.row == 0 && span.start_col == 0 && span.end_col == 3)
                && frame
                    .selection
                    .iter()
                    .any(|span| span.row == 1 && span.start_col == 0 && span.end_col == 1)
        }),
        "the frame must expose selected cells on both rows and show VISUAL mode"
    );
    assert!(
        terminal
            .frames
            .last()
            .is_some_and(|frame| frame.selection.is_empty()),
        "leaving Visual mode must clear the selection marker"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn visual_linewise_yank_across_lines_and_put_elsewhere() {
    let directory = scratch();
    let path = directory.join("visual-linewise-put.txt");
    fs::write(&path, "a\nb\nc\n").expect("seed file");

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new(
        [
            vec![
                InputEvent::Char('V'),
                InputEvent::Char('j'),
                InputEvent::Char('y'),
                InputEvent::Char('j'),
                InputEvent::Char('j'),
                InputEvent::Char('p'),
            ],
            command("wq"),
        ]
        .concat(),
    );
    editor.run(&mut terminal).expect("run editor");

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "a\nb\nc\na\nb\n",
        "V must yank whole lines and p must insert them linewise elsewhere"
    );
    assert!(
        terminal.frames.iter().any(|frame| {
            frame
                .rows
                .last()
                .is_some_and(|row| row.contains("V-LINE  2:1"))
                && frame
                    .selection
                    .iter()
                    .any(|span| span.row == 0 && span.start_col == 0 && span.end_col == 1)
                && frame
                    .selection
                    .iter()
                    .any(|span| span.row == 1 && span.start_col == 0 && span.end_col == 1)
        }),
        "linewise Visual must mark every selected line"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn visual_change_enters_insert_and_undoes_as_one_block() {
    let directory = scratch();
    let path = directory.join("visual-change.txt");
    fs::write(&path, "abcdef\n").expect("seed file");

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new(
        [
            vec![
                InputEvent::Char('v'),
                InputEvent::Char('l'),
                InputEvent::Char('l'),
                InputEvent::Char('c'),
                InputEvent::Char('X'),
                InputEvent::Esc,
                InputEvent::Char('u'),
            ],
            command("wq"),
        ]
        .concat(),
    );
    editor.run(&mut terminal).expect("run editor");

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "abcdef\n",
        "c plus the typed replacement must undo as one block"
    );
    assert!(
        terminal.frames.iter().any(|frame| {
            frame.rows.last().is_some_and(|row| row.contains("INSERT"))
                && frame.selection.is_empty()
        }),
        "c must enter Insert with the selection gone"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn visual_charwise_yank_across_lines_then_put_elsewhere() {
    let directory = scratch();
    let path = directory.join("visual-charwise-put.txt");
    fs::write(&path, "ab\ncd\n").expect("seed file");

    run_and_save(
        &path,
        vec![
            InputEvent::Char('v'),
            InputEvent::Char('j'),
            InputEvent::Char('y'),
            InputEvent::Char('j'),
            InputEvent::Char('p'),
        ],
    );

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "ab\ncab\ncd\n",
        "a characterwise yank across lines must put its exact newline-containing text"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn visual_linewise_change_opens_one_empty_line() {
    let directory = scratch();
    let path = directory.join("visual-linewise-change.txt");
    fs::write(&path, "a\nb\nc\n").expect("seed file");

    run_and_save(
        &path,
        vec![
            InputEvent::Char('j'),
            InputEvent::Char('V'),
            InputEvent::Char('c'),
            InputEvent::Char('X'),
            InputEvent::Esc,
        ],
    );

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "a\nX\nc\n",
        "Vc must replace the selected line with one empty line for Insert"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn visual_linewise_change_undoes_in_one_step() {
    let directory = scratch();
    let path = directory.join("visual-linewise-change-undo.txt");
    fs::write(&path, "a\nb\nc\n").expect("seed file");

    run_and_save(
        &path,
        vec![
            InputEvent::Char('j'),
            InputEvent::Char('V'),
            InputEvent::Char('c'),
            InputEvent::Char('X'),
            InputEvent::Esc,
            InputEvent::Char('u'),
        ],
    );

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "a\nb\nc\n",
        "Vc and its typed text must undo as one block"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn visual_yank_does_not_dirty_or_create_history() {
    let directory = scratch();
    let path = directory.join("visual-yank-clean.txt");
    fs::write(&path, "a\nb\n").expect("seed file");

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new(
        [
            vec![
                InputEvent::Char('V'),
                InputEvent::Char('y'),
                InputEvent::Char('u'),
            ],
            command("q"),
        ]
        .concat(),
    );
    editor.run(&mut terminal).expect("run editor");

    assert_eq!(
        fs::read_to_string(&path).expect("read untouched file"),
        "a\nb\n",
        "a Visual yank must not change the buffer or make quit refuse"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn visual_linewise_delete_preserves_crlf() {
    let directory = scratch();
    let path = directory.join("visual-crlf.txt");
    fs::write(&path, "a\r\nb\r\nc\r\n").expect("seed file");

    run_and_save(
        &path,
        vec![
            InputEvent::Char('V'),
            InputEvent::Char('j'),
            InputEvent::Char('d'),
        ],
    );

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "c\r\n",
        "a linewise Visual delete in a CRLF buffer must not split or normalize CRLF"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn visual_word_motion_wraps_past_a_line_end() {
    let directory = scratch();
    let path = directory.join("visual-word-wrap.txt");
    fs::write(&path, "foo\nbar\n").expect("seed file");

    run_and_save(
        &path,
        vec![
            InputEvent::Char('v'),
            InputEvent::Char('w'),
            InputEvent::Char('w'),
            InputEvent::Char('d'),
        ],
    );

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "ar\n",
        "w must extend the selection to the next line when the current line has no word left"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn visual_selection_inversion_resolves_the_same_range() {
    let cases = [
        (
            "forward",
            vec![
                InputEvent::Char('l'),
                InputEvent::Char('l'),
                InputEvent::Char('v'),
                InputEvent::Char('l'),
                InputEvent::Char('l'),
                InputEvent::Char('d'),
            ],
        ),
        (
            "reverse",
            vec![
                InputEvent::Char('l'),
                InputEvent::Char('l'),
                InputEvent::Char('l'),
                InputEvent::Char('l'),
                InputEvent::Char('v'),
                InputEvent::Char('h'),
                InputEvent::Char('h'),
                InputEvent::Char('d'),
            ],
        ),
    ];

    for (name, events) in cases {
        let directory = scratch();
        let path = directory.join(format!("visual-inversion-{name}.txt"));
        fs::write(&path, "abcdef\n").expect("seed file");
        run_and_save(&path, events);
        assert_eq!(
            fs::read_to_string(&path).expect("read saved file"),
            "abf\n",
            "{name} selection must resolve to the same cde range"
        );
        fs::remove_dir_all(&directory).expect("clean scratch directory");
    }
}

#[test]
fn visual_select_never_splits_a_grapheme_cluster() {
    let directory = scratch();
    let path = directory.join("visual-grapheme.txt");
    fs::write(&path, "e\u{301}xy\n").expect("seed file");

    run_and_save(
        &path,
        vec![
            InputEvent::Char('v'),
            InputEvent::Char('l'),
            InputEvent::Char('d'),
        ],
    );

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "y\n",
        "selecting from the decomposed e through x must remove whole clusters"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn visual_cancel_and_toggle_do_not_dirty_the_buffer() {
    let cases = [
        (
            "escape",
            vec![
                InputEvent::Char('v'),
                InputEvent::Char('l'),
                InputEvent::Esc,
            ],
        ),
        (
            "v-toggle",
            vec![InputEvent::Char('v'), InputEvent::Char('v')],
        ),
        (
            "V-toggle",
            vec![InputEvent::Char('V'), InputEvent::Char('V')],
        ),
    ];

    for (name, events) in cases {
        let directory = scratch();
        let path = directory.join(format!("visual-cancel-{name}.txt"));
        fs::write(&path, "abc\n").expect("seed file");

        let mut editor = Editor::open(&path).expect("open file");
        let mut terminal = ScriptedTerminal::new([events, command("q")].concat());
        editor.run(&mut terminal).expect("run editor");

        assert_eq!(
            fs::read_to_string(&path).expect("read untouched file"),
            "abc\n",
            "{name} must cancel without changing the buffer"
        );
        assert!(
            terminal
                .frames
                .last()
                .is_some_and(|frame| frame.selection.is_empty()),
            "{name} must clear the selection marker"
        );
        fs::remove_dir_all(&directory).expect("clean scratch directory");
    }
}

#[test]
fn visual_deletes_undo_in_one_step() {
    let cases = [
        (
            "charwise",
            "abc\ndef\n",
            vec![
                InputEvent::Char('v'),
                InputEvent::Char('j'),
                InputEvent::Char('d'),
                InputEvent::Char('u'),
            ],
        ),
        (
            "linewise",
            "a\nb\nc\n",
            vec![
                InputEvent::Char('V'),
                InputEvent::Char('j'),
                InputEvent::Char('d'),
                InputEvent::Char('u'),
            ],
        ),
        (
            "x-alias",
            "abc\n",
            vec![
                InputEvent::Char('v'),
                InputEvent::Char('l'),
                InputEvent::Char('x'),
                InputEvent::Char('u'),
            ],
        ),
    ];

    for (name, initial, events) in cases {
        let directory = scratch();
        let path = directory.join(format!("visual-undo-{name}.txt"));
        fs::write(&path, initial).expect("seed file");
        run_and_save(&path, events);
        assert_eq!(
            fs::read_to_string(&path).expect("read saved file"),
            initial,
            "one u must undo the {name} Visual delete completely"
        );
        fs::remove_dir_all(&directory).expect("clean scratch directory");
    }
}

#[test]
fn visual_operations_on_an_empty_buffer_do_not_panic() {
    let directory = scratch();
    let path = directory.join("visual-empty.txt");
    fs::write(&path, "").expect("seed empty file");

    run_and_save(
        &path,
        vec![
            InputEvent::Char('v'),
            InputEvent::Char('l'),
            InputEvent::Char('d'),
            InputEvent::Char('V'),
            InputEvent::Char('d'),
            InputEvent::Char('c'),
            InputEvent::Char('X'),
            InputEvent::Esc,
            InputEvent::Char('u'),
        ],
    );

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "",
        "Visual verbs on an empty buffer must be no-ops"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn search_finds_across_lines_and_exposes_match_spans() {
    let directory = scratch();
    let path = directory.join("search-lines.txt");
    fs::write(&path, "alpha\nbeta\ngamma\n").expect("seed file");

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new([search("beta"), command("q")].concat());
    editor.run(&mut terminal).expect("run editor");

    assert_eq!(
        fs::read_to_string(&path).expect("read untouched file"),
        "alpha\nbeta\ngamma\n",
        "search must not change the buffer or make quit refuse"
    );
    let frame = terminal
        .frames
        .iter()
        .find(|frame| {
            frame.cursor == ScreenPos { row: 1, col: 0 }
                && frame
                    .rows
                    .last()
                    .is_some_and(|row| row.contains("NORMAL  2:1"))
        })
        .expect("a frame after the search must show the matching cursor line");
    assert!(
        frame
            .search_current
            .iter()
            .any(|span| span.row == 1 && span.start_col == 0 && span.end_col == 4),
        "the current match must be exposed on Frame.search_current: {frame:?}"
    );
    assert!(
        frame
            .search_matches
            .iter()
            .any(|span| span.row == 1 && span.start_col == 0 && span.end_col == 4),
        "all visible matches must include the current match: {frame:?}"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn search_prompt_shows_live_editing_and_backspace() {
    let directory = scratch();
    let path = directory.join("search-prompt.txt");
    fs::write(&path, "alpha\nbeta\n").expect("seed file");

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new(
        [
            vec![
                InputEvent::Char('/'),
                InputEvent::Char('b'),
                InputEvent::Char('e'),
                InputEvent::Char('x'),
                InputEvent::Backspace,
                InputEvent::Enter,
            ],
            command("q"),
        ]
        .concat(),
    );
    editor.run(&mut terminal).expect("run editor");

    assert!(
        terminal
            .frames
            .iter()
            .any(|frame| frame.rows.last().is_some_and(|row| row.contains("/bex"))),
        "the search prompt must redraw after each typed character"
    );
    assert!(
        terminal
            .frames
            .iter()
            .any(|frame| frame.rows.last().is_some_and(|row| row.contains("/be"))),
        "backspace must remove the last prompt character before Enter"
    );
    assert!(
        terminal.frames.iter().any(|frame| {
            frame.cursor == ScreenPos { row: 1, col: 0 }
                && frame
                    .rows
                    .last()
                    .is_some_and(|row| row.contains("NORMAL  2:1"))
        }),
        "the edited pattern must find the later line"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn search_wraps_at_the_end_and_reports_a_message() {
    let directory = scratch();
    let path = directory.join("search-wrap.txt");
    fs::write(&path, "foo\nbar\nfoo\n").expect("seed file");

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new(
        [
            vec![InputEvent::Char('j'), InputEvent::Char('j')],
            search("foo"),
            command("q"),
        ]
        .concat(),
    );
    editor.run(&mut terminal).expect("run editor");

    assert!(
        terminal.frames.iter().any(|frame| {
            frame.cursor == ScreenPos { row: 0, col: 0 }
                && frame
                    .rows
                    .last()
                    .is_some_and(|row| row.contains("search hit BOTTOM, continuing at TOP"))
                && frame
                    .search_current
                    .iter()
                    .any(|span| span.row == 0 && span.start_col == 0 && span.end_col == 3)
        }),
        "wrapping forward must land on the top match and show the search message"
    );
    assert!(
        terminal.frames.iter().any(|frame| {
            frame
                .search_matches
                .iter()
                .any(|span| span.row == 2 && span.start_col == 0 && span.end_col == 3)
        }),
        "visible matches on later rows must also be exposed"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn search_n_and_n_cycle_through_matches() {
    let cases = [
        (
            "forward-one",
            [
                search("foo"),
                vec![InputEvent::Char('n')],
                vec![
                    InputEvent::Char('i'),
                    InputEvent::Char('X'),
                    InputEvent::Esc,
                ],
            ]
            .concat(),
            "foo\nfoo\nXfoo\n",
        ),
        (
            "forward-wrap",
            [
                search("foo"),
                vec![InputEvent::Char('n'), InputEvent::Char('n')],
                vec![
                    InputEvent::Char('i'),
                    InputEvent::Char('X'),
                    InputEvent::Esc,
                ],
            ]
            .concat(),
            "Xfoo\nfoo\nfoo\n",
        ),
        (
            "backward-one",
            [
                search("foo"),
                vec![InputEvent::Char('N')],
                vec![
                    InputEvent::Char('i'),
                    InputEvent::Char('X'),
                    InputEvent::Esc,
                ],
            ]
            .concat(),
            "Xfoo\nfoo\nfoo\n",
        ),
        (
            "backward-wrap",
            [
                search("foo"),
                vec![InputEvent::Char('N'), InputEvent::Char('N')],
                vec![
                    InputEvent::Char('i'),
                    InputEvent::Char('X'),
                    InputEvent::Esc,
                ],
            ]
            .concat(),
            "foo\nfoo\nXfoo\n",
        ),
    ];

    for (name, events, expected) in cases {
        let directory = scratch();
        let path = directory.join(format!("search-{name}.txt"));
        fs::write(&path, "foo\nfoo\nfoo\n").expect("seed file");
        run_and_save(&path, events);
        assert_eq!(
            fs::read_to_string(&path).expect("read saved file"),
            expected,
            "{name} must move the cursor to the expected match before Insert"
        );
        fs::remove_dir_all(&directory).expect("clean scratch directory");
    }
}

#[test]
fn search_cancel_leaves_the_cursor_and_dirty_flag_alone() {
    let directory = scratch();
    let clean = directory.join("search-cancel-clean.txt");
    fs::write(&clean, "one\ntwo\n").expect("seed file");

    let mut editor = Editor::open(&clean).expect("open file");
    let mut terminal = ScriptedTerminal::new(
        [
            vec![
                InputEvent::Char('j'),
                InputEvent::Char('/'),
                InputEvent::Char('n'),
                InputEvent::Char('e'),
                InputEvent::Char('e'),
                InputEvent::Char('d'),
                InputEvent::Char('l'),
                InputEvent::Char('e'),
                InputEvent::Esc,
            ],
            command("q"),
        ]
        .concat(),
    );
    editor.run(&mut terminal).expect("run editor");
    assert_eq!(
        fs::read_to_string(&clean).expect("read untouched file"),
        "one\ntwo\n",
        "cancelling a search must not dirty the buffer"
    );
    assert!(
        terminal.frames.iter().any(|frame| {
            frame.cursor == ScreenPos { row: 1, col: 0 }
                && frame
                    .rows
                    .last()
                    .is_some_and(|row| row.contains("NORMAL  2:1"))
                && frame.search_current.is_empty()
        }),
        "cancel must leave the cursor on the original line"
    );

    let cursor = directory.join("search-cancel-cursor.txt");
    fs::write(&cursor, "one\ntwo\n").expect("seed file");
    run_and_save(
        &cursor,
        [
            vec![
                InputEvent::Char('j'),
                InputEvent::Char('/'),
                InputEvent::Char('n'),
                InputEvent::Char('e'),
                InputEvent::Char('e'),
                InputEvent::Char('d'),
                InputEvent::Char('l'),
                InputEvent::Char('e'),
                InputEvent::Esc,
            ],
            vec![
                InputEvent::Char('i'),
                InputEvent::Char('X'),
                InputEvent::Esc,
            ],
        ]
        .concat(),
    );
    assert_eq!(
        fs::read_to_string(&cursor).expect("read saved file"),
        "one\nXtwo\n",
        "an insert after cancelling search must start on the original cursor line"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn search_rejects_matches_inside_graphemes_and_finds_whole_ones() {
    let cases = [
        (
            "combining-no-partial",
            "e\u{301}x\n",
            "e",
            None,
            ScreenPos { row: 0, col: 0 },
        ),
        (
            "combining-whole-neighbour",
            "e\u{301}x\n",
            "x",
            Some((1, 2)),
            ScreenPos { row: 0, col: 1 },
        ),
        (
            "zwj-no-partial",
            "👨\u{200d}👩\u{200d}👧\u{200d}👦x\n",
            "👨",
            None,
            ScreenPos { row: 0, col: 0 },
        ),
    ];

    for (name, text, pattern, expected_span, expected_cursor) in cases {
        let directory = scratch();
        let path = directory.join(format!("search-{name}.txt"));
        fs::write(&path, text).expect("seed file");

        let mut editor = Editor::open(&path).expect("open file");
        let mut terminal = ScriptedTerminal::new([search(pattern), command("q")].concat());
        editor.run(&mut terminal).expect("run editor");

        match expected_span {
            Some((start_col, end_col)) => {
                let frame = terminal
                    .frames
                    .iter()
                    .find(|frame| {
                        frame.cursor == expected_cursor
                            && frame.search_current.iter().any(|span| {
                                span.row == 0
                                    && span.start_col == start_col
                                    && span.end_col == end_col
                            })
                    })
                    .expect("the whole-grapheme match must be visible at the expected cursor");
                assert!(
                    frame.search_current.iter().any(|span| {
                        span.row == 0 && span.start_col == start_col && span.end_col == end_col
                    }),
                    "{name} must expose the whole-grapheme match span: {frame:?}"
                );
            }
            None => {
                let frame = terminal
                    .frames
                    .iter()
                    .find(|frame| {
                        frame.cursor == expected_cursor
                            && frame.rows.last().is_some_and(|row| row.contains("E486"))
                    })
                    .expect("the rejected partial-grapheme search must report E486");
                assert!(
                    frame.search_current.is_empty(),
                    "{name} must reject the partial-grapheme match: {frame:?}"
                );
            }
        }

        fs::remove_dir_all(&directory).expect("clean scratch directory");
    }
}

#[test]
fn search_finds_across_crlf_and_preserves_the_terminator() {
    let directory = scratch();
    let path = directory.join("search-crlf.txt");
    fs::write(&path, "alpha\r\nbeta\r\ngamma\r\n").expect("seed file");

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new(
        [
            search("beta"),
            vec![
                InputEvent::Char('i'),
                InputEvent::Char('X'),
                InputEvent::Esc,
            ],
            command("wq"),
        ]
        .concat(),
    );
    editor.run(&mut terminal).expect("run editor");

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "alpha\r\nXbeta\r\ngamma\r\n",
        "search must find the later CRLF line without changing either terminator"
    );
    assert!(
        terminal.frames.iter().any(|frame| {
            frame.cursor == ScreenPos { row: 1, col: 0 }
                && frame
                    .search_current
                    .iter()
                    .any(|span| span.row == 1 && span.start_col == 0 && span.end_col == 4)
        }),
        "the CRLF search frame must expose the match on line 2"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn search_handles_empty_patterns_and_empty_buffers() {
    let directory = scratch();
    let path = directory.join("search-empty.txt");
    fs::write(&path, "").expect("seed empty file");

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new(
        [
            vec![InputEvent::Char('/'), InputEvent::Enter],
            search("a-pattern-longer-than-the-empty-buffer"),
            command("q"),
        ]
        .concat(),
    );
    editor.run(&mut terminal).expect("run editor");

    assert!(
        terminal.frames.iter().any(|frame| {
            frame
                .rows
                .last()
                .is_some_and(|row| row.contains("Search pattern is empty"))
        }),
        "an empty pattern must report a message instead of panicking"
    );
    assert!(
        terminal
            .frames
            .iter()
            .any(|frame| { frame.rows.last().is_some_and(|row| row.contains("E486")) }),
        "a pattern longer than the buffer must report not found"
    );
    assert!(
        terminal
            .frames
            .iter()
            .all(|frame| frame.search_current.is_empty()),
        "failed searches must not leave a current match"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn normal_horizontal_motion_stays_within_the_line() {
    let cases = [
        (
            "right-stops-on-last-character",
            vec![
                InputEvent::Char('l'),
                InputEvent::Char('l'),
                InputEvent::Char('l'),
                InputEvent::Char('i'),
                InputEvent::Char('X'),
                InputEvent::Esc,
            ],
            "aXb\ncd\n",
        ),
        (
            "left-stops-at-line-start",
            vec![
                InputEvent::Char('j'),
                InputEvent::Char('h'),
                InputEvent::Char('i'),
                InputEvent::Char('X'),
                InputEvent::Esc,
            ],
            "ab\nXcd\n",
        ),
    ];

    for (name, events, expected) in cases {
        let directory = scratch();
        let path = directory.join(format!("normal-horizontal-{name}.txt"));
        fs::write(&path, "ab\ncd\n").expect("seed file");
        run_and_save(&path, events);
        assert_eq!(
            fs::read_to_string(&path).expect("read saved file"),
            expected,
            "{name} must insert on the cursor's own line at a real character"
        );
        fs::remove_dir_all(&directory).expect("clean scratch directory");
    }
}

#[test]
fn vertical_motion_stops_on_the_last_visible_line() {
    let directory = scratch();
    let path = directory.join("last-visible-line.txt");
    fs::write(&path, "a\nb\nc\n").expect("seed file");

    run_and_save(
        &path,
        vec![
            InputEvent::Char('j'),
            InputEvent::Char('j'),
            InputEvent::Char('j'),
            InputEvent::Char('i'),
            InputEvent::Char('X'),
            InputEvent::Esc,
        ],
    );

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "a\nb\nXc\n",
        "j past the last visible line must not create an addressable trailing line"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn normal_w_crosses_line_boundaries() {
    let cases = [
        (
            "next-line-word",
            vec![
                InputEvent::Char('w'),
                InputEvent::Char('w'),
                InputEvent::Char('w'),
                InputEvent::Char('i'),
                InputEvent::Char('X'),
                InputEvent::Esc,
            ],
            "one two\nthree Xfour\n",
        ),
        (
            "last-word-on-last-line",
            vec![
                InputEvent::Char('w'),
                InputEvent::Char('w'),
                InputEvent::Char('w'),
                InputEvent::Char('w'),
                InputEvent::Char('i'),
                InputEvent::Char('X'),
                InputEvent::Esc,
            ],
            "one two\nthree fouXr\n",
        ),
    ];

    for (name, events, expected) in cases {
        let directory = scratch();
        let path = directory.join(format!("normal-w-{name}.txt"));
        fs::write(&path, "one two\nthree four\n").expect("seed file");
        run_and_save(&path, events);
        assert_eq!(
            fs::read_to_string(&path).expect("read saved file"),
            expected,
            "{name} must move w across the line boundary instead of dead-ending"
        );
        fs::remove_dir_all(&directory).expect("clean scratch directory");
    }
}

#[test]
fn opening_a_line_at_buffer_end_preserves_the_opened_line() {
    let cases = [
        (
            "empty-below",
            "",
            vec![InputEvent::Char('o'), InputEvent::Esc],
            "\n\n",
        ),
        (
            "empty-above",
            "",
            vec![InputEvent::Char('O'), InputEvent::Esc],
            "\n\n",
        ),
        (
            "no-eol-below",
            "a\nb\nc",
            vec![
                InputEvent::Char('j'),
                InputEvent::Char('j'),
                InputEvent::Char('o'),
                InputEvent::Esc,
            ],
            "a\nb\nc\n\n",
        ),
        (
            "crlf-no-eol-below",
            "a\r\nb",
            vec![
                InputEvent::Char('j'),
                InputEvent::Char('o'),
                InputEvent::Esc,
            ],
            "a\r\nb\r\n\r\n",
        ),
    ];

    for (name, initial, events, expected) in cases {
        let directory = scratch();
        let path = directory.join(format!("open-line-end-{name}.txt"));
        fs::write(&path, initial).expect("seed file");
        run_and_save(&path, events);
        assert_eq!(
            fs::read_to_string(&path).expect("read saved file"),
            expected,
            "{name} must save the opened line, not a bare final terminator"
        );
        fs::remove_dir_all(&directory).expect("clean scratch directory");
    }
}

#[test]
fn deleting_an_opened_line_on_an_empty_buffer_leaves_one_empty_line() {
    let directory = scratch();
    let path = directory.join("open-then-dd.txt");
    fs::write(&path, "").expect("seed empty file");

    run_and_save(
        &path,
        vec![
            InputEvent::Char('o'),
            InputEvent::Esc,
            InputEvent::Char('d'),
            InputEvent::Char('d'),
        ],
    );

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "\n",
        "dd after o must delete the opened line, not the line above it"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn append_enters_insert_after_the_cursor_or_at_line_end() {
    let cases = [
        (
            "after-cursor",
            "abc\n",
            vec![
                InputEvent::Char('a'),
                InputEvent::Char('X'),
                InputEvent::Esc,
            ],
            "aXbc\n",
        ),
        (
            "after-last-character",
            "abc\n",
            vec![
                InputEvent::Char('l'),
                InputEvent::Char('l'),
                InputEvent::Char('a'),
                InputEvent::Char('X'),
                InputEvent::Esc,
            ],
            "abcX\n",
        ),
        (
            "at-line-end",
            "abc\n",
            vec![
                InputEvent::Char('A'),
                InputEvent::Char('X'),
                InputEvent::Esc,
            ],
            "abcX\n",
        ),
        (
            "empty-line-does-not-wrap",
            "a\n\nb\n",
            vec![
                InputEvent::Char('j'),
                InputEvent::Char('a'),
                InputEvent::Char('X'),
                InputEvent::Esc,
            ],
            "a\nX\nb\n",
        ),
    ];

    for (name, initial, events, expected) in cases {
        let directory = scratch();
        let path = directory.join(format!("append-{name}.txt"));
        fs::write(&path, initial).expect("seed file");
        run_and_save(&path, events);
        assert_eq!(
            fs::read_to_string(&path).expect("read saved file"),
            expected,
            "{name} must enter Insert on the intended side of the cursor"
        );
        fs::remove_dir_all(&directory).expect("clean scratch directory");
    }
}

#[test]
fn redo_places_the_cursor_on_a_real_character_at_end_of_line() {
    let directory = scratch();
    let path = directory.join("redo-at-end.txt");
    fs::write(&path, "").expect("seed empty file");

    run_and_save(
        &path,
        vec![
            InputEvent::Char('i'),
            InputEvent::Char('a'),
            InputEvent::Char('b'),
            InputEvent::Char('c'),
            InputEvent::Esc,
            InputEvent::Char('u'),
            InputEvent::Ctrl('r'),
            InputEvent::Char('i'),
            InputEvent::Char('X'),
            InputEvent::Esc,
        ],
    );

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "abXc",
        "redo at end of line must leave the cursor on the last character, not past it"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn long_lines_scroll_horizontally_and_recenter_on_zero() {
    let directory = scratch();
    let path = directory.join("long-line.txt");
    fs::write(&path, format!("{}\n", "a".repeat(500))).expect("seed file");

    let mut events: Vec<InputEvent> = std::iter::repeat_n(InputEvent::Char('l'), 100).collect();
    events.push(InputEvent::Char('$'));
    events.push(InputEvent::Char('0'));
    events.extend(command("q"));

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::with_sizes(
        events,
        [Size {
            width: 20,
            height: 6,
        }],
    );
    editor.run(&mut terminal).expect("run editor");

    let frames = &terminal.frames;
    let scrolled = frames
        .iter()
        .find(|frame| frame.rows.last().is_some_and(|row| row.contains("1:101")))
        .expect("a frame after 100 l");
    assert_eq!(scrolled.horizontal_offset, 81);
    assert_eq!(scrolled.cursor, ScreenPos { row: 0, col: 19 });
    assert_eq!(scrolled.rows[0].len(), 20, "the row fills the viewport");

    let dollar = frames
        .iter()
        .position(|frame| frame.rows.last().is_some_and(|row| row.contains("1:500")))
        .expect("a frame after $");
    assert_eq!(frames[dollar].horizontal_offset, 480);
    assert_eq!(frames[dollar].cursor, ScreenPos { row: 0, col: 19 });

    let zero = frames[dollar + 1..]
        .iter()
        .find(|frame| {
            frame.horizontal_offset == 0
                && frame.cursor == ScreenPos { row: 0, col: 0 }
                && frame
                    .rows
                    .last()
                    .is_some_and(|row| row.contains("NORMAL  1:1"))
        })
        .expect("a frame after 0");
    assert_eq!(zero.rows[0].len(), 20);

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn horizontal_scroll_keeps_tabs_and_wide_glyphs_visible() {
    let directory = scratch();
    let path = directory.join("tab-wide.txt");
    fs::write(&path, format!("a\tb中文{}\n", "x".repeat(40))).expect("seed file");

    let mut events = vec![
        InputEvent::Char('l'),
        InputEvent::Char('l'),
        InputEvent::Char('l'),
        InputEvent::Char('l'),
    ];
    events.extend(command("q"));

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::with_sizes(
        events,
        [Size {
            width: 10,
            height: 5,
        }],
    );
    editor.run(&mut terminal).expect("run editor");

    let frames = &terminal.frames;
    assert_eq!(frames[1].cursor, ScreenPos { row: 0, col: 1 });
    assert_eq!(frames[1].horizontal_offset, 0);
    assert_eq!(
        frames[1].rows[0], "a       b",
        "the tab expands to the stop"
    );

    assert_eq!(frames[2].cursor, ScreenPos { row: 0, col: 8 });
    assert_eq!(frames[2].horizontal_offset, 0);
    assert!(
        frames[2].cursor.col < 10,
        "the cursor after the tab must be on screen"
    );

    assert_eq!(frames[3].horizontal_offset, 1);
    assert_eq!(frames[3].cursor, ScreenPos { row: 0, col: 8 });
    assert_eq!(
        frames[3].rows[0], "       b中",
        "the wide glyph must be drawn whole at the right edge"
    );

    assert_eq!(frames[4].horizontal_offset, 8);
    assert_eq!(frames[4].cursor, ScreenPos { row: 0, col: 3 });
    assert_eq!(frames[4].rows[0], "b中文xxxxx");
    assert!(
        frames[4].cursor.col < 10,
        "the cursor on a wide glyph must be on screen"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn shrinking_the_terminal_keeps_the_cursor_visible() {
    let directory = scratch();
    let path = directory.join("resize.txt");
    fs::write(&path, format!("{}\n", "a".repeat(100))).expect("seed file");

    let mut events: Vec<InputEvent> = std::iter::repeat_n(InputEvent::Char('l'), 30).collect();
    events.extend(command("q"));

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::with_sizes(
        events,
        [
            Size {
                width: 80,
                height: 6,
            },
            Size {
                width: 12,
                height: 6,
            },
        ],
    );
    editor.run(&mut terminal).expect("run editor");

    let normal_frames: Vec<_> = terminal
        .frames
        .iter()
        .filter(|frame| frame.rows.last().is_some_and(|row| row.contains("NORMAL")))
        .collect();
    assert_eq!(normal_frames.first().expect("initial frame").cursor.col, 0);
    assert_eq!(
        normal_frames
            .first()
            .expect("initial frame")
            .horizontal_offset,
        0
    );

    let shrunk = normal_frames.last().expect("last normal frame");
    assert_eq!(shrunk.horizontal_offset, 19);
    assert_eq!(shrunk.cursor, ScreenPos { row: 0, col: 11 });
    assert!(
        shrunk.rows[0].len() <= 12,
        "the row must fit the narrowed terminal"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn search_and_undo_into_a_long_line_keep_the_cursor_visible() {
    let directory = scratch();
    let path = directory.join("search-long.txt");
    fs::write(
        &path,
        format!("{}needle{}\n", "x".repeat(420), "y".repeat(60)),
    )
    .expect("seed file");

    let mut events = search("needle");
    events.extend([
        InputEvent::Char('$'),
        InputEvent::Char('i'),
        InputEvent::Char('X'),
        InputEvent::Esc,
        InputEvent::Char('u'),
    ]);
    events.extend(command("q"));

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::with_sizes(
        events,
        [Size {
            width: 40,
            height: 6,
        }],
    );
    editor.run(&mut terminal).expect("run editor");

    let frames = &terminal.frames;
    let searched = frames
        .iter()
        .find(|frame| frame.rows.last().is_some_and(|row| row.contains("1:421")))
        .expect("a frame after the search");
    assert_eq!(searched.horizontal_offset, 381);
    assert_eq!(searched.cursor, ScreenPos { row: 0, col: 39 });
    assert!(
        searched
            .search_current
            .iter()
            .any(|span| span.row == 0 && span.start_col == 39 && span.end_col == 40),
        "the current match must be clipped to the scrolled viewport"
    );

    let changed = frames
        .iter()
        .position(|frame| frame.rows.last().is_some_and(|row| row.contains("[+]")))
        .expect("a frame with the inserted X");
    let undone = frames
        .iter()
        .rposition(|frame| {
            frame
                .rows
                .last()
                .is_some_and(|row| row.contains("1:486") && !row.contains("[+]"))
        })
        .expect("a frame after undo");
    assert!(undone > changed, "undo must be drawn after the edit");
    assert!(frames[undone].horizontal_offset > 0);
    assert!(
        frames[undone].cursor.col < 40,
        "undo into a long line must leave the cursor on screen"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn vertical_motion_keeps_the_desired_column_across_a_short_line() {
    let directory = scratch();
    let path = directory.join("desired-column.txt");
    fs::write(&path, "12345678901234567890\nshort\nabcdefghijklmnopqrst\n").expect("seed file");

    let mut events: Vec<InputEvent> = std::iter::repeat_n(InputEvent::Char('l'), 15).collect();
    events.extend([
        InputEvent::Char('j'),
        InputEvent::Char('j'),
        InputEvent::Char('i'),
        InputEvent::Char('X'),
        InputEvent::Esc,
    ]);
    events.extend(command("wq"));

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new(events);
    editor.run(&mut terminal).expect("run editor");

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "12345678901234567890\nshort\nabcdefghijklmnoXpqrst\n",
        "j must remember display column 15 instead of drifting to the short line's column"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn vertical_move_from_a_scrolled_long_line_resets_offset_on_a_short_line() {
    let directory = scratch();
    let path = directory.join("scroll-to-short.txt");
    fs::write(&path, format!("{}\nshort\n", "a".repeat(100))).expect("seed file");

    let mut events: Vec<InputEvent> = std::iter::repeat_n(InputEvent::Char('l'), 50).collect();
    events.push(InputEvent::Char('j'));
    events.extend(command("q"));

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::with_sizes(
        events,
        [Size {
            width: 20,
            height: 6,
        }],
    );
    editor.run(&mut terminal).expect("run editor");

    let long = terminal
        .frames
        .iter()
        .find(|frame| frame.rows.last().is_some_and(|row| row.contains("1:51")))
        .expect("a frame on the scrolled long line");
    assert_eq!(long.horizontal_offset, 31);
    assert_eq!(long.cursor, ScreenPos { row: 0, col: 19 });

    let short = terminal
        .frames
        .iter()
        .find(|frame| frame.rows.last().is_some_and(|row| row.contains("2:5")))
        .expect("a frame after moving onto the short line");
    assert_eq!(
        short.horizontal_offset, 0,
        "a short line must clamp the offset all the way back to zero"
    );
    assert_eq!(short.cursor, ScreenPos { row: 1, col: 4 });
    assert!(short.cursor.col < 20, "the cursor must remain on screen");
    assert!(short.rows[1].starts_with("short"));

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn visual_zero_and_dollar_select_to_line_boundaries() {
    let directory = scratch();

    let dollar_path = directory.join("visual-dollar.txt");
    fs::write(&dollar_path, "abcdef\n").expect("seed file");
    let mut editor = Editor::open(&dollar_path).expect("open file");
    let mut terminal = ScriptedTerminal::new(
        [
            vec![
                InputEvent::Char('l'),
                InputEvent::Char('l'),
                InputEvent::Char('v'),
                InputEvent::Char('$'),
                InputEvent::Char('d'),
            ],
            command("wq"),
        ]
        .concat(),
    );
    editor.run(&mut terminal).expect("run editor");

    assert_eq!(
        fs::read_to_string(&dollar_path).expect("read saved file"),
        "ab\n",
        "Visual $ must extend the selection through the last character"
    );
    assert!(
        terminal.frames.iter().any(|frame| {
            frame
                .rows
                .last()
                .is_some_and(|row| row.contains("VISUAL  1:6"))
                && frame.cursor == ScreenPos { row: 0, col: 5 }
                && frame
                    .selection
                    .iter()
                    .any(|span| span.row == 0 && span.start_col == 2 && span.end_col == 6)
        }),
        "the forward $ selection must expose cells 2 through 5"
    );

    let zero_path = directory.join("visual-zero.txt");
    fs::write(&zero_path, "abcdef\n").expect("seed file");
    let mut editor = Editor::open(&zero_path).expect("open file");
    let mut terminal = ScriptedTerminal::new(
        [
            vec![
                InputEvent::Char('l'),
                InputEvent::Char('l'),
                InputEvent::Char('l'),
                InputEvent::Char('v'),
                InputEvent::Char('0'),
                InputEvent::Char('d'),
            ],
            command("wq"),
        ]
        .concat(),
    );
    editor.run(&mut terminal).expect("run editor");

    assert_eq!(
        fs::read_to_string(&zero_path).expect("read saved file"),
        "ef\n",
        "Visual 0 must extend the selection backwards through the anchor"
    );
    assert!(
        terminal.frames.iter().any(|frame| {
            frame
                .rows
                .last()
                .is_some_and(|row| row.contains("VISUAL  1:1"))
                && frame.cursor == ScreenPos { row: 0, col: 0 }
                && frame
                    .selection
                    .iter()
                    .any(|span| span.row == 0 && span.start_col == 0 && span.end_col == 4)
        }),
        "the reversed 0 selection must expose cells 0 through 3"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn dollar_on_a_tab_and_a_wide_final_grapheme_keeps_them_fully_visible() {
    let cases = [
        (
            "tab",
            "12345678\t\n",
            12_u16,
            4_usize,
            4_u16,
            8_u16,
            "5678        ",
        ),
        (
            "wide",
            "1234567890中\n",
            11_u16,
            1_usize,
            9_u16,
            2_u16,
            "234567890中",
        ),
    ];

    let directory = scratch();
    for (name, initial, width, offset, cursor_col, glyph_width, expected_row) in cases {
        let path = directory.join(format!("dollar-{name}.txt"));
        fs::write(&path, initial).expect("seed file");

        let mut events = vec![InputEvent::Char('$')];
        events.extend(command("q"));

        let mut editor = Editor::open(&path).expect("open file");
        let mut terminal = ScriptedTerminal::with_sizes(events, [Size { width, height: 5 }]);
        editor.run(&mut terminal).expect("run editor");

        let frame = terminal
            .frames
            .iter()
            .find(|frame| frame.horizontal_offset == offset && frame.cursor.col == cursor_col)
            .unwrap_or_else(|| panic!("{name}: missing the frame after $"));
        assert_eq!(
            frame.rows[0], expected_row,
            "{name}: the final grapheme must be drawn whole at the scrolled edge"
        );
        assert_eq!(frame.cursor.row, 0);
        assert!(
            frame.cursor.col + glyph_width <= width,
            "{name}: the final grapheme must be fully visible"
        );

        fs::remove_file(&path).expect("clean seed file");
    }
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn vertical_motion_through_a_tab_heavy_short_line_keeps_the_display_column() {
    let directory = scratch();
    let path = directory.join("desired-column-tabs.txt");
    fs::write(
        &path,
        "\tabcdefghij0123456789\n\tab\n0123456789abcdefghij\n",
    )
    .expect("seed file");

    let mut events: Vec<InputEvent> = std::iter::repeat_n(InputEvent::Char('l'), 9).collect();
    events.extend([
        InputEvent::Char('j'),
        InputEvent::Char('k'),
        InputEvent::Char('j'),
        InputEvent::Char('j'),
        InputEvent::Char('i'),
        InputEvent::Char('X'),
        InputEvent::Esc,
    ]);
    events.extend(command("wq"));

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new(events);
    editor.run(&mut terminal).expect("run editor");

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "\tabcdefghij0123456789\n\tab\n0123456789abcdefXghij\n",
        "j/k must carry display column 16 across a tab-heavy line shorter than it, not the byte column"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn no_argument_starts_untitled_with_a_banner_and_save_as() {
    let directory = scratch();
    let path = directory.join("notes file.txt");

    let mut editor = Editor::untitled();
    let mut terminal = ScriptedTerminal::with_sizes(
        [
            vec![
                InputEvent::Char('i'),
                InputEvent::Char('H'),
                InputEvent::Char('i'),
                InputEvent::Esc,
            ],
            command(&format!("w \"{}\"", path.display())),
            command("q"),
        ]
        .concat(),
        [Size {
            width: 80,
            height: 24,
        }],
    );
    editor.run(&mut terminal).expect("run editor");

    let first = terminal.frames.first().expect("initial frame");
    assert!(
        first.rows.iter().any(|row| row.contains("██")),
        "the no-file frame must show the large block title"
    );
    assert!(
        first
            .rows
            .iter()
            .any(|row| row.contains("a modal terminal editor")),
        "the no-file frame must identify the editor"
    );
    assert!(
        first.rows.iter().any(|row| row.contains('\u{f07c}')),
        "the menu must use the folder icon"
    );
    for label in [
        "Open file",
        "Insert text",
        "Save as",
        "Find",
        "Undo",
        "Redo",
        "Quit",
    ] {
        assert!(
            first.rows.iter().any(|row| row.contains(label)),
            "the start page must list {label}"
        );
    }
    assert!(
        first.rows.iter().any(|row| row.trim_end().ends_with(":q")),
        "the quit shortcut must be right-aligned on its menu row"
    );
    assert!(
        first
            .rows
            .last()
            .is_some_and(|row| row.contains("[No Name]")),
        "the status row must mark the buffer untitled"
    );
    for style in [
        TextStyle::Title,
        TextStyle::Muted,
        TextStyle::Icon,
        TextStyle::Key,
        TextStyle::Warm,
    ] {
        assert!(
            first.styles.iter().any(|span| span.style == style),
            "the start page must expose the {style:?} style"
        );
    }
    assert!(
        terminal.frames.iter().skip(1).all(|frame| {
            frame
                .rows
                .iter()
                .all(|row| !row.contains("a modal terminal editor"))
        }),
        "the banner must disappear once editing starts"
    );
    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "Hi",
        "a quoted path with a space must be accepted by :w"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn edit_command_opens_a_file_and_requires_force_to_discard_changes() {
    let directory = scratch();
    let first = directory.join("first.txt");
    let second = directory.join("second.txt");
    fs::write(&first, "one\n").expect("seed first file");
    fs::write(&second, "two\n").expect("seed second file");

    let mut editor = Editor::untitled();
    let mut terminal = ScriptedTerminal::new(
        [
            command(&format!("e \"{}\"", first.display())),
            vec![
                InputEvent::Char('i'),
                InputEvent::Char('X'),
                InputEvent::Esc,
            ],
            command(&format!("e \"{}\"", second.display())),
            command(&format!("e! \"{}\"", second.display())),
            command("q"),
        ]
        .concat(),
    );
    editor.run(&mut terminal).expect("run editor");

    assert!(
        terminal.frames.iter().any(|frame| {
            frame.rows.last().is_some_and(|row| row.contains("E37"))
                && frame.rows[0].contains("Xone")
        }),
        ":e must refuse to discard a modified buffer and keep it on screen"
    );
    assert!(
        terminal
            .frames
            .iter()
            .any(|frame| frame.rows[0].starts_with("two")),
        ":e! must open the requested file after the refusal"
    );
    assert_eq!(
        fs::read_to_string(&first).expect("read untouched first file"),
        "one\n",
        "the refused edit must not write the modified buffer"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn writing_an_untitled_buffer_without_a_name_reports_e32_and_keeps_editing() {
    let mut editor = Editor::untitled();
    let mut terminal = ScriptedTerminal::new(
        [
            vec![
                InputEvent::Char('i'),
                InputEvent::Char('X'),
                InputEvent::Esc,
            ],
            command("w"),
            command("q"),
            command("q!"),
        ]
        .concat(),
    );
    editor.run(&mut terminal).expect("run editor");

    assert!(
        terminal
            .frames
            .iter()
            .any(|frame| frame.rows.last().is_some_and(|row| row.contains("E32"))),
        "a nameless :w must report E32"
    );
    assert!(
        terminal
            .frames
            .iter()
            .any(|frame| frame.rows.last().is_some_and(|row| row.contains("E37"))),
        "q after the failed write must still guard the modified buffer"
    );
}

#[test]
fn startup_banner_survives_tiny_frames() {
    let mut editor = Editor::untitled();
    let mut terminal = ScriptedTerminal::with_sizes(
        command("q"),
        [
            Size {
                width: 1,
                height: 1,
            },
            Size {
                width: 5,
                height: 3,
            },
        ],
    );
    editor.run(&mut terminal).expect("run editor");

    assert_eq!(terminal.frames[0].rows.len(), 1);
    assert_eq!(terminal.frames[1].rows.len(), 3);
    assert!(
        terminal.frames.iter().all(|frame| frame
            .rows
            .iter()
            .all(|row| row.chars().all(|c| !c.is_control()))),
        "the banner must never leak control characters"
    );
}
