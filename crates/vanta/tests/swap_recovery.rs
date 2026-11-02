//! Headless regressions for the crash-recovery swap lifecycle.
//!
//! A crashed editor is simulated by exhausting the scripted terminal, which
//! returns an error before the normal `close_session` path. The next
//! `Editor::open` therefore sees exactly the swap the killed process left.

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
    size: Size,
    events: VecDeque<InputEvent>,
    frames: Vec<Frame>,
}

impl ScriptedTerminal {
    fn new(events: impl IntoIterator<Item = InputEvent>) -> Self {
        Self {
            size: Size {
                width: 100,
                height: 24,
            },
            events: events.into_iter().collect(),
            frames: Vec::new(),
        }
    }

    fn screen(&self) -> String {
        self.frames
            .iter()
            .map(|frame| frame.rows.join("\n"))
            .collect::<Vec<_>>()
            .join("\n---\n")
    }
}

impl Terminal for ScriptedTerminal {
    fn size(&mut self) -> io::Result<Size> {
        Ok(self.size)
    }

    fn draw(&mut self, frame: &Frame) -> io::Result<()> {
        self.frames.push(frame.clone());
        Ok(())
    }

    fn read_event(&mut self) -> io::Result<InputEvent> {
        self.events
            .pop_front()
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "simulated crash"))
    }
}

fn scratch(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "vanta-recovery-{}-{}-{nanos}-{label}",
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

fn swap_files(directory: &Path) -> Vec<PathBuf> {
    fs::read_dir(directory)
        .expect("read scratch directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with('.') && name.ends_with(".swp"))
        })
        .collect()
}

fn crash_while_typing(path: &Path, keys: impl IntoIterator<Item = InputEvent>) {
    let mut editor = Editor::open(path).expect("open file");
    let mut terminal = ScriptedTerminal::new(keys);
    assert!(
        editor.run(&mut terminal).is_err(),
        "a simulated crash must not take the clean-quit path"
    );
}

#[test]
fn clean_open_and_quit_removes_the_swap() {
    let directory = scratch("clean");
    let path = directory.join("note.txt");
    fs::write(&path, "seed\n").expect("seed file");

    let mut editor = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new(command("q"));
    editor.run(&mut terminal).expect("clean quit");

    assert_eq!(
        fs::read_to_string(&path).expect("read file"),
        "seed\n",
        "a clean quit must not modify the target"
    );
    assert!(
        swap_files(&directory).is_empty(),
        "a clean quit must remove the swap"
    );
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn crash_leaves_unsaved_text_and_recovery_saves_it_without_touching_the_original() {
    let directory = scratch("recover");
    let path = directory.join("note.txt");
    fs::write(&path, "seed\n").expect("seed file");

    crash_while_typing(&path, [InputEvent::Char('i'), InputEvent::Char('X')]);

    let swaps = swap_files(&directory);
    assert_eq!(swaps.len(), 1, "the crash must leave one swap");
    let swap_text = fs::read_to_string(&swaps[0]).expect("read swap");
    assert!(
        swap_text.contains("Xseed"),
        "the swap must contain the unsaved edit: {swap_text:?}"
    );
    assert_eq!(
        fs::read_to_string(&path).expect("read original"),
        "seed\n",
        "the crash and the swap must not modify the original file"
    );

    let mut editor = Editor::open(&path).expect("reopen file");
    let mut events = vec![InputEvent::Char('r')];
    events.extend(command("wq"));
    let mut terminal = ScriptedTerminal::new(events);
    editor.run(&mut terminal).expect("recover and save");

    assert!(
        terminal.screen().contains("E325: ATTENTION"),
        "startup did not show the recovery prompt: {}",
        terminal.screen()
    );
    assert_eq!(
        fs::read_to_string(&path).expect("read recovered file"),
        "Xseed\n",
        "recovery must write the recovered text only after :w"
    );
    assert!(
        swap_files(&directory).is_empty(),
        "a successful save must remove the swap"
    );
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn edit_removes_the_swap_and_save_restores_the_clean_lifecycle() {
    let directory = scratch("save");
    let path = directory.join("note.txt");
    fs::write(&path, "seed\n").expect("seed file");

    let mut editor = Editor::open(&path).expect("open file");
    let mut events = vec![
        InputEvent::Char('i'),
        InputEvent::Char('X'),
        InputEvent::Esc,
    ];
    events.extend(command("w"));
    events.extend(command("q"));
    let mut terminal = ScriptedTerminal::new(events);
    editor.run(&mut terminal).expect("edit, save and quit");

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "Xseed\n"
    );
    assert!(
        swap_files(&directory).is_empty(),
        "a clean save must remove the swap"
    );
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn open_original_keeps_the_swap_and_edit_anyway_replaces_it() {
    let directory = scratch("decisions");
    let path = directory.join("note.txt");
    fs::write(&path, "seed\n").expect("seed file");

    crash_while_typing(&path, [InputEvent::Char('i'), InputEvent::Char('X')]);
    assert_eq!(swap_files(&directory).len(), 1);

    let mut editor = Editor::open(&path).expect("open for original");
    let mut events = vec![
        InputEvent::Char('o'),
        InputEvent::Char('i'),
        InputEvent::Char('Z'),
        InputEvent::Esc,
    ];
    events.extend(command("q!"));
    let mut terminal = ScriptedTerminal::new(events);
    editor
        .run(&mut terminal)
        .expect("open original, edit without swapping, and quit");

    let swaps = swap_files(&directory);
    assert_eq!(
        swaps.len(),
        1,
        "opening the original must preserve the swap"
    );
    let preserved = fs::read_to_string(&swaps[0]).expect("read preserved swap");
    assert!(
        preserved.contains("Xseed"),
        "the preserved swap must still contain the unsaved edit"
    );
    assert!(
        !preserved.contains("Zseed"),
        "edits after choosing open-original must not replace the preserved swap"
    );
    assert_eq!(fs::read_to_string(&path).expect("read original"), "seed\n");

    // `E` explicitly overwrites the stale recovery state. Crash again so the
    // replacement swap can be inspected without the clean-quit removal.
    let mut editor = Editor::open(&path).expect("open for edit-anyway");
    let mut terminal = ScriptedTerminal::new([
        InputEvent::Char('e'),
        InputEvent::Char('i'),
        InputEvent::Char('Y'),
    ]);
    assert!(editor.run(&mut terminal).is_err());

    let swaps = swap_files(&directory);
    assert_eq!(swaps.len(), 1);
    let swap_text = fs::read_to_string(&swaps[0]).expect("read replaced swap");
    assert!(
        swap_text.contains("Yseed"),
        "edit-anyway must base the new swap on the original: {swap_text:?}"
    );
    assert!(
        !swap_text.contains("Xseed"),
        "edit-anyway must replace the old recovery text"
    );
    assert_eq!(
        fs::read_to_string(&path).expect("read original"),
        "seed\n",
        "recovery decisions must never write the target"
    );

    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn abort_at_the_prompt_leaves_the_swap_untouched() {
    let directory = scratch("abort");
    let path = directory.join("note.txt");
    fs::write(&path, "seed\n").expect("seed file");

    crash_while_typing(&path, [InputEvent::Char('i'), InputEvent::Char('X')]);
    let before = fs::read(swap_files(&directory)[0].clone()).expect("read swap");

    let mut editor = Editor::open(&path).expect("open for abort");
    let mut terminal = ScriptedTerminal::new([InputEvent::Char('q')]);
    editor
        .run(&mut terminal)
        .expect("abort the recovery prompt");

    assert_eq!(fs::read_to_string(&path).expect("read original"), "seed\n");
    let swaps = swap_files(&directory);
    assert_eq!(swaps.len(), 1, "abort must leave the swap on disk");
    assert_eq!(fs::read(&swaps[0]).expect("read preserved swap"), before);
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn recovery_marks_the_buffer_modified_so_plain_quit_is_refused() {
    let directory = scratch("dirty-recovery");
    let path = directory.join("note.txt");
    fs::write(&path, "seed\n").expect("seed file");

    crash_while_typing(&path, [InputEvent::Char('i'), InputEvent::Char('X')]);

    let mut editor = Editor::open(&path).expect("open for dirty recovery");
    let mut events = vec![InputEvent::Char('r')];
    events.extend(command("q"));
    let mut terminal = ScriptedTerminal::new(events);
    assert!(
        editor.run(&mut terminal).is_err(),
        "a refused quit must keep the editor waiting for input"
    );

    assert!(
        terminal
            .screen()
            .contains("E37: No write since last change"),
        "recovery must mark the buffer modified: {}",
        terminal.screen()
    );
    assert_eq!(
        fs::read_to_string(&path).expect("read original"),
        "seed\n",
        "plain quit must not write the recovered text"
    );
    assert_eq!(
        swap_files(&directory).len(),
        1,
        "a refused quit must keep the recovery swap"
    );
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn delete_swap_removes_the_recovery_file_and_leaves_the_original() {
    let directory = scratch("delete");
    let path = directory.join("note.txt");
    fs::write(&path, "seed\n").expect("seed file");

    crash_while_typing(&path, [InputEvent::Char('i'), InputEvent::Char('X')]);
    assert_eq!(swap_files(&directory).len(), 1);

    let mut editor = Editor::open(&path).expect("open for delete");
    let mut events = vec![InputEvent::Char('d')];
    events.extend(command("q"));
    let mut terminal = ScriptedTerminal::new(events);
    editor.run(&mut terminal).expect("delete swap and quit");

    assert_eq!(fs::read_to_string(&path).expect("read original"), "seed\n");
    assert!(
        swap_files(&directory).is_empty(),
        "delete must remove the recovery swap and the clean replacement"
    );
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn preserve_forces_a_swap_flush_and_reports_it() {
    let directory = scratch("preserve");
    let path = directory.join("note.txt");
    fs::write(&path, "seed\n").expect("seed file");

    let mut editor = Editor::open(&path).expect("open file");
    let mut events = vec![
        InputEvent::Char('i'),
        InputEvent::Char('X'),
        InputEvent::Esc,
    ];
    events.extend(command("preserve"));
    let mut terminal = ScriptedTerminal::new(events);
    assert!(editor.run(&mut terminal).is_err());

    assert!(
        terminal.screen().contains("swap file preserved"),
        "the preserve command did not report success: {}",
        terminal.screen()
    );
    let swaps = swap_files(&directory);
    assert_eq!(swaps.len(), 1);
    assert!(
        fs::read_to_string(&swaps[0])
            .expect("read preserved swap")
            .contains("Xseed")
    );
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn stale_swap_is_reported_and_can_be_recovered() {
    let directory = scratch("stale");
    let path = directory.join("note.txt");
    fs::write(&path, "seed\n").expect("seed file");

    crash_while_typing(&path, [InputEvent::Char('i'), InputEvent::Char('X')]);
    let swaps = swap_files(&directory);
    assert_eq!(swaps.len(), 1);
    let swap_text = fs::read_to_string(&swaps[0]).expect("read swap");
    let current = format!("pid={}", std::process::id());
    assert!(swap_text.contains(&current));
    let stale = swap_text.replace(&current, "pid=999999");
    fs::write(&swaps[0], stale).expect("make swap stale");

    let mut editor = Editor::open(&path).expect("open stale swap");
    let mut events = vec![InputEvent::Char('r')];
    events.extend(command("wq"));
    let mut terminal = ScriptedTerminal::new(events);
    editor.run(&mut terminal).expect("recover stale swap");

    let screen = terminal.screen();
    assert!(
        screen.contains("no longer running"),
        "the prompt must report the dead writer: {screen}"
    );
    assert_eq!(
        fs::read_to_string(&path).expect("read recovered file"),
        "Xseed\n"
    );
    assert!(swap_files(&directory).is_empty());
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn mismatched_swap_is_refused_and_can_only_be_deleted() {
    let directory = scratch("mismatch");
    let first = directory.join("a.txt");
    let second = directory.join("b.txt");
    fs::write(&first, "first\n").expect("seed first");
    fs::write(&second, "second\n").expect("seed second");

    crash_while_typing(&first, [InputEvent::Char('i'), InputEvent::Char('A')]);
    crash_while_typing(&second, [InputEvent::Char('i'), InputEvent::Char('B')]);

    let swaps = swap_files(&directory);
    let first_swap = swaps
        .iter()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().contains("a.txt"))
        })
        .expect("first swap");
    let second_swap = swaps
        .iter()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().contains("b.txt"))
        })
        .expect("second swap");
    fs::copy(first_swap, second_swap).expect("replace second swap with first");

    let mut editor = Editor::open(&second).expect("open mismatched swap");
    let mut events = vec![InputEvent::Char('r')];
    events.extend([InputEvent::Char('d')]);
    events.extend(command("q"));
    let mut terminal = ScriptedTerminal::new(events);
    editor
        .run(&mut terminal)
        .expect("refuse recovery, delete and quit");

    let screen = terminal.screen();
    assert!(
        screen.contains("does not match"),
        "the prompt must identify the mismatch: {screen}"
    );
    assert!(
        screen.contains("E325:"),
        "a refused recovery must report E325: {screen}"
    );
    assert_eq!(
        fs::read_to_string(&second).expect("read second"),
        "second\n",
        "the mismatched original must stay untouched"
    );
    assert_eq!(
        fs::read_to_string(&first).expect("read first"),
        "first\n",
        "the source swap's original must stay untouched"
    );
    assert!(
        !second_swap.exists(),
        "delete must remove the mismatched swap"
    );
    fs::remove_dir_all(directory).expect("clean scratch directory");
}
