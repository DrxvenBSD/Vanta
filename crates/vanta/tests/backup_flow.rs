//! Headless editor regressions for the BUF-005 save transaction.
//!
//! These drive the real `Editor::run` loop through the scripted terminal, so
//! the `:set` options, save failure handling, backup file and dirty flag are
//! exercised together rather than as isolated filesystem calls.

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
        Ok(Size {
            width: 200,
            height: 24,
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

fn scratch(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after the Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "vanta-backup-flow-{}-{}-{nanos}-{label}",
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

fn run(path: &Path, events: impl IntoIterator<Item = InputEvent>) -> String {
    let mut editor = Editor::open(path).expect("open file");
    let mut terminal = ScriptedTerminal::new(events);
    editor.run(&mut terminal).expect("run editor");
    terminal.screen()
}

fn backup_path(path: &Path) -> PathBuf {
    path.with_file_name("note.txt~")
}

#[test]
fn default_writebackup_is_transient_on_success() {
    let directory = scratch("transient");
    let path = directory.join("note.txt");
    fs::write(&path, "seed\n").expect("seed file");

    let mut events = vec![
        InputEvent::Char('i'),
        InputEvent::Char('X'),
        InputEvent::Esc,
    ];
    events.extend(command("w"));
    events.extend(command("q"));
    let screen = run(&path, events);

    assert_eq!(
        fs::read_to_string(&path).expect("read target"),
        "Xseed\n",
        "the save must write the edited buffer"
    );
    assert!(
        !backup_path(&path).exists(),
        "default writebackup must remove the transient backup after success"
    );
    assert!(screen.contains("written"), "screen: {screen}");

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn backup_option_keeps_the_previous_contents() {
    let directory = scratch("kept");
    let path = directory.join("note.txt");
    fs::write(&path, "seed\n").expect("seed file");

    let mut events = command("set backup");
    events.extend([
        InputEvent::Char('i'),
        InputEvent::Char('X'),
        InputEvent::Esc,
    ]);
    events.extend(command("w"));
    events.extend(command("q"));
    let screen = run(&path, events);

    assert_eq!(fs::read_to_string(&path).expect("read target"), "Xseed\n");
    assert_eq!(
        fs::read_to_string(backup_path(&path)).expect("read kept backup"),
        "seed\n",
        "backup on must keep the previous file contents"
    );
    assert!(screen.contains("backup:"), "screen: {screen}");

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn backupdir_and_backupext_are_honored_by_the_editor() {
    let directory = scratch("options");
    let backup_dir = directory.join("backup dir");
    fs::create_dir(&backup_dir).expect("create backup directory");
    let path = directory.join("note.txt");
    fs::write(&path, "seed\n").expect("seed file");

    let mut events = command("set backup");
    events.extend(command(&format!(
        "set backupdir=\"{}\"",
        backup_dir.display()
    )));
    events.extend(command("set backupext=.bak"));
    events.extend([
        InputEvent::Char('i'),
        InputEvent::Char('X'),
        InputEvent::Esc,
    ]);
    events.extend(command("w"));
    events.extend(command("q"));
    run(&path, events);

    assert_eq!(fs::read_to_string(&path).expect("read target"), "Xseed\n");
    assert_eq!(
        fs::read_to_string(backup_dir.join("note.txt.bak")).expect("read configured backup"),
        "seed\n"
    );
    assert!(
        !backup_path(&path).exists(),
        "the configured backup directory must be used instead of the target directory"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn failed_backup_aborts_and_keeps_the_buffer_dirty() {
    let directory = scratch("failed-backup");
    let path = directory.join("note.txt");
    fs::write(&path, "seed\n").expect("seed file");
    let blocker = directory.join("not-a-directory");
    fs::write(&blocker, "blocker").expect("create blocker");

    let mut events = command("set backup");
    events.extend(command(&format!("set backupdir={}", blocker.display())));
    events.extend([
        InputEvent::Char('i'),
        InputEvent::Char('X'),
        InputEvent::Esc,
    ]);
    events.extend(command("w"));
    events.extend(command("q!"));
    let screen = run(&path, events);

    assert_eq!(
        fs::read_to_string(&path).expect("read untouched target"),
        "seed\n",
        "a failed backup must abort before the target is written"
    );
    assert!(screen.contains("save failed"), "screen: {screen}");
    assert!(
        screen.contains("[+]"),
        "the failed save must keep the buffer dirty"
    );
    assert!(!backup_path(&path).exists());

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn read_only_target_aborts_and_leaves_a_usable_backup() {
    let directory = scratch("read-only-target");
    let path = directory.join("note.txt");
    fs::write(&path, "seed\n").expect("seed file");
    let original_permissions = fs::metadata(&path).expect("target metadata").permissions();
    let mut read_only = original_permissions.clone();
    read_only.set_readonly(true);
    fs::set_permissions(&path, read_only).expect("mark target read-only");

    let mut events = vec![
        InputEvent::Char('i'),
        InputEvent::Char('X'),
        InputEvent::Esc,
    ];
    events.extend(command("w"));
    events.extend(command("q!"));
    let screen = run(&path, events);

    assert_eq!(
        fs::read_to_string(&path).expect("read read-only target"),
        "seed\n",
        "a failed read-only save must not change the target"
    );
    assert_eq!(
        fs::read_to_string(backup_path(&path)).expect("read aborted backup"),
        "seed\n",
        "a failed save must leave the previous contents usable"
    );
    assert!(screen.contains("save failed"), "screen: {screen}");
    assert!(
        screen.contains("[+]"),
        "the failed save must keep the buffer dirty"
    );

    fs::set_permissions(&path, original_permissions).expect("restore target permissions");
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn disabling_both_backup_modes_leaves_no_backup() {
    let directory = scratch("disabled");
    let path = directory.join("note.txt");
    fs::write(&path, "seed\n").expect("seed file");

    let mut events = command("set nobackup");
    events.extend(command("set nowritebackup"));
    events.extend([
        InputEvent::Char('i'),
        InputEvent::Char('X'),
        InputEvent::Esc,
    ]);
    events.extend(command("w"));
    events.extend(command("q"));
    run(&path, events);

    assert_eq!(fs::read_to_string(&path).expect("read target"), "Xseed\n");
    assert!(
        !backup_path(&path).exists(),
        "both backup modes off must leave no backup"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}
