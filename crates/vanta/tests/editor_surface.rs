//! Acceptance tests for the editor surface beyond single-buffer editing.
//!
//! Every test drives `Editor::run` with a scripted terminal and asserts the
//! bytes on disk. They cover the state users depend on: which buffer a window
//! shows, where each window's cursor is, tab lifecycle, rectangular edits and
//! Select-mode replacement.

use std::collections::VecDeque;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use vanta::editor::Editor;
use vanta::terminal::{Frame, InputEvent, Size, Terminal};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct ScriptedTerminal {
    events: VecDeque<InputEvent>,
    frames: Vec<Frame>,
}

impl ScriptedTerminal {
    fn new(events: impl IntoIterator<Item = InputEvent>) -> Self {
        Self {
            events: events.into_iter().collect(),
            frames: Vec::new(),
        }
    }
}

impl Terminal for ScriptedTerminal {
    fn size(&mut self) -> io::Result<Size> {
        Ok(Size {
            width: 100,
            height: 30,
        })
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
        "vanta-surface-{}-{}-{nanos}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).expect("create scratch directory");
    path
}

fn chars(text: &str) -> Vec<InputEvent> {
    text.chars().map(InputEvent::Char).collect()
}

fn command(text: &str) -> Vec<InputEvent> {
    let mut events = vec![InputEvent::Char(':')];
    events.extend(chars(text));
    events.push(InputEvent::Enter);
    events
}

fn run(path: &Path, events: Vec<InputEvent>) -> Vec<Frame> {
    let mut events = events;
    events.extend(command("wq"));
    let mut editor = Editor::open(path).expect("open file");
    let mut terminal = ScriptedTerminal::new(events);
    editor.run(&mut terminal).expect("run editor");
    terminal.frames
}

fn run_raw(path: &Path, events: Vec<InputEvent>) -> Vec<Frame> {
    let mut editor = Editor::open(path).expect("open file");
    let mut terminal = ScriptedTerminal::new(events);
    editor.run(&mut terminal).expect("run editor");
    terminal.frames
}

fn edit_file(path: &Path) -> String {
    fs::read_to_string(path).expect("read saved file")
}

#[test]
fn buffers_keep_independent_text_and_undo_history() {
    let directory = scratch();
    let first = directory.join("first.txt");
    let second = directory.join("second.txt");
    fs::write(&first, "alpha\n").expect("seed first");
    fs::write(&second, "beta\n").expect("seed second");

    run(
        &first,
        [
            command(&format!("e \"{}\"", first.display())),
            chars("iX"),
            vec![InputEvent::Esc],
            command("w"),
            command(&format!("e \"{}\"", second.display())),
            chars("iY"),
            vec![InputEvent::Esc],
            command("w"),
            command("b 0"),
            chars("u"),
            command("w"),
            command("b 1"),
        ]
        .concat(),
    );

    assert_eq!(edit_file(&first), "alpha\n");
    assert_eq!(edit_file(&second), "Ybeta\n");
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn buffer_list_and_delete_are_real() {
    let directory = scratch();
    let first = directory.join("one.txt");
    let second = directory.join("two.txt");
    fs::write(&first, "one\n").expect("seed first");
    fs::write(&second, "two\n").expect("seed second");

    let frames = run_raw(
        &first,
        [
            command(&format!("e \"{}\"", first.display())),
            command(&format!("e \"{}\"", second.display())),
            command("ls"),
            command("bd"),
            command("b 1"),
            command("q"),
        ]
        .concat(),
    );

    assert!(
        frames.iter().any(|frame| {
            frame
                .rows
                .last()
                .is_some_and(|row| row.contains("one.txt") && row.contains("two.txt"))
        }),
        ":ls must list both opened buffers"
    );
    assert!(
        frames
            .iter()
            .any(|frame| { frame.rows.last().is_some_and(|row| row.contains("E94")) }),
        "a deleted buffer must no longer resolve"
    );
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn split_windows_keep_independent_cursors_in_one_buffer() {
    let directory = scratch();
    let path = directory.join("split.txt");
    fs::write(&path, "abc\ndef\n").expect("seed file");

    run(
        &path,
        [
            vec![InputEvent::Ctrl('w'), InputEvent::Char('v')],
            chars("j"),
            vec![
                InputEvent::Ctrl('w'),
                InputEvent::Char('h'),
                InputEvent::Char('i'),
                InputEvent::Char('X'),
                InputEvent::Esc,
                InputEvent::Ctrl('w'),
                InputEvent::Char('l'),
                InputEvent::Char('i'),
                InputEvent::Char('Y'),
                InputEvent::Esc,
            ],
        ]
        .concat(),
    );

    assert_eq!(
        edit_file(&path),
        "Xabc\nYdef\n",
        "each split must edit at its own cursor"
    );
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn splits_can_show_different_buffers_and_edit_the_focused_one() {
    let directory = scratch();
    let first = directory.join("left.txt");
    let second = directory.join("right.txt");
    fs::write(&first, "left\n").expect("seed first");
    fs::write(&second, "right\n").expect("seed second");

    run(
        &first,
        [
            command(&format!("e \"{}\"", first.display())),
            vec![InputEvent::Ctrl('w'), InputEvent::Char('v')],
            command(&format!("e \"{}\"", second.display())),
            chars("iR"),
            vec![InputEvent::Esc],
            command("w"),
            vec![InputEvent::Ctrl('w'), InputEvent::Char('h')],
            chars("iL"),
            vec![InputEvent::Esc],
            command("w"),
        ]
        .concat(),
    );

    assert_eq!(edit_file(&first), "Lleft\n");
    assert_eq!(edit_file(&second), "Rright\n");
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn closing_one_split_keeps_the_buffer_open_and_saves_it() {
    let directory = scratch();
    let path = directory.join("close-split.txt");
    fs::write(&path, "abc\n").expect("seed file");

    run(
        &path,
        [
            vec![InputEvent::Ctrl('w'), InputEvent::Char('s')],
            vec![
                InputEvent::Ctrl('w'),
                InputEvent::Char('q'),
                InputEvent::Char('i'),
                InputEvent::Char('Z'),
                InputEvent::Esc,
            ],
        ]
        .concat(),
    );

    assert_eq!(
        edit_file(&path),
        "Zabc\n",
        "closing a split must not close or discard the buffer"
    );
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn tabs_create_switch_save_and_close_independently() {
    let directory = scratch();
    let first = directory.join("tab-one.txt");
    let second = directory.join("tab-two.txt");
    fs::write(&first, "one\n").expect("seed first");
    fs::write(&second, "two\n").expect("seed second");

    run(
        &first,
        [
            command(&format!("e \"{}\"", first.display())),
            command(&format!("tabnew \"{}\"", second.display())),
            chars("iT"),
            vec![InputEvent::Esc],
            command("w"),
            command("tabnext 1"),
            chars("iA"),
            vec![InputEvent::Esc],
            command("w"),
            command("tabnext 2"),
            command("tabclose"),
            command("w"),
        ]
        .concat(),
    );

    assert_eq!(edit_file(&first), "Aone\n");
    assert_eq!(edit_file(&second), "Ttwo\n");
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn visual_block_delete_uses_display_columns_across_ragged_lines() {
    let directory = scratch();
    let path = directory.join("block-delete.txt");
    fs::write(&path, "abcde\nxy\nuvwxyz\n").expect("seed file");

    run(
        &path,
        [
            vec![InputEvent::Char('l'), InputEvent::Ctrl('v')],
            chars("jjll"),
            chars("d"),
        ]
        .concat(),
    );

    assert_eq!(
        edit_file(&path),
        "ae\nx\nuyz\n",
        "a block delete must remove the same display columns from every row"
    );
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn visual_block_yank_and_put_round_trip_the_rectangle() {
    let directory = scratch();
    let path = directory.join("block-put.txt");
    fs::write(&path, "abcd\nefgh\nijkl\nmnop\n").expect("seed file");

    run(
        &path,
        [vec![InputEvent::Ctrl('v')], chars("ljy"), chars("3G$p")].concat(),
    );

    assert_eq!(
        edit_file(&path),
        "abcd\nefgh\nijklab\nmnopef\n",
        "a yanked block must paste as a rectangle"
    );
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn visual_block_insert_and_append_repeat_on_every_selected_line() {
    let directory = scratch();
    let insert_path = directory.join("block-insert.txt");
    let append_path = directory.join("block-append.txt");
    fs::write(&insert_path, "abcd\nefgh\nijkl\n").expect("seed insert");
    fs::write(&append_path, "abcd\nefgh\nijkl\n").expect("seed append");

    run(
        &insert_path,
        [
            vec![InputEvent::Ctrl('v')],
            chars("ljI"),
            chars("X"),
            vec![InputEvent::Esc],
        ]
        .concat(),
    );
    run(
        &append_path,
        [
            vec![InputEvent::Ctrl('v')],
            chars("ljA"),
            chars("X"),
            vec![InputEvent::Esc],
        ]
        .concat(),
    );

    assert_eq!(edit_file(&insert_path), "Xabcd\nXefgh\nijkl\n");
    assert_eq!(edit_file(&append_path), "abXcd\nefXgh\nijkl\n");
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn visual_block_change_replaces_each_row_and_undoes_once() {
    let directory = scratch();
    let path = directory.join("block-change.txt");
    fs::write(&path, "abcd\nefgh\nijkl\n").expect("seed file");

    let frames = run(
        &path,
        [
            vec![InputEvent::Ctrl('v')],
            chars("ljcZ"),
            vec![InputEvent::Esc],
            chars("u"),
        ]
        .concat(),
    );

    // `u` is appended before the automatic `:wq`, so the saved file must be the
    // fully restored source; the changed frame proves the block edit happened.
    assert_eq!(edit_file(&path), "abcd\nefgh\nijkl\n");
    assert!(
        frames.iter().any(|frame| {
            frame.rows.first().is_some_and(|row| row.starts_with("Zcd"))
                && frame.rows.get(1).is_some_and(|row| row.starts_with("Zgh"))
        }),
        "the block change must replace the same columns on both rows"
    );
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn visual_block_dollar_deletes_through_each_line_end() {
    let directory = scratch();
    let path = directory.join("block-dollar.txt");
    fs::write(&path, "abc\ndef\n").expect("seed file");

    run(
        &path,
        [
            vec![InputEvent::Ctrl('v'), InputEvent::Char('$')],
            chars("jd"),
        ]
        .concat(),
    );

    assert_eq!(edit_file(&path), "\n\n");
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn select_mode_replaces_the_selection_instead_of_inserting_beside_it() {
    let directory = scratch();
    let path = directory.join("select.txt");
    fs::write(&path, "abc\n").expect("seed file");

    run(
        &path,
        vec![
            InputEvent::Char('l'),
            InputEvent::Char('g'),
            InputEvent::Char('h'),
            InputEvent::Char('X'),
            InputEvent::Esc,
        ],
    );

    assert_eq!(
        edit_file(&path),
        "aXc\n",
        "a printable key in Select mode must replace the selection, not insert"
    );
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn select_mode_escape_cancels_without_editing() {
    let directory = scratch();
    let path = directory.join("select-cancel.txt");
    fs::write(&path, "abc\n").expect("seed file");

    run(
        &path,
        vec![
            InputEvent::Char('l'),
            InputEvent::Char('g'),
            InputEvent::Char('h'),
            InputEvent::Esc,
        ],
    );

    assert_eq!(edit_file(&path), "abc\n");
    fs::remove_dir_all(directory).expect("clean scratch directory");
}
