//! Headless regressions for untitled-buffer swaps and explicit recovery.
//!
//! A crash is simulated by exhausting the scripted terminal, so `Editor::run`
//! returns before `close_session`. The swap directory is redirected with
//! `VANTA_STATE_DIR`; tests in this binary serialize that override.

use std::collections::VecDeque;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use vanta::editor::Editor;
use vanta::terminal::{Frame, InputEvent, Size, Terminal};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Serializes access to the process-wide state-directory override.
static STATE_LOCK: Mutex<()> = Mutex::new(());

/// A scratch state directory that holds the lock until the test ends.
struct Isolated {
    root: PathBuf,
    state: PathBuf,
    _guard: MutexGuard<'static, ()>,
}

fn isolate(label: &str) -> Isolated {
    let guard = STATE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "vanta-untitled-{}-{}-{nanos}-{label}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&root).expect("create scratch directory");
    let state = root.join("state");
    // SAFETY: every test in this binary holds STATE_LOCK before changing or
    // reading VANTA_STATE_DIR, and the variable is not read by another thread.
    unsafe { std::env::set_var("VANTA_STATE_DIR", &state) };
    Isolated {
        root,
        state,
        _guard: guard,
    }
}

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

fn command(text: &str) -> Vec<InputEvent> {
    let mut events = vec![InputEvent::Char(':')];
    events.extend(text.chars().map(InputEvent::Char));
    events.push(InputEvent::Enter);
    events
}

fn typed(text: &str) -> Vec<InputEvent> {
    let mut events = vec![InputEvent::Char('i')];
    events.extend(text.chars().map(InputEvent::Char));
    events.push(InputEvent::Esc);
    events
}

/// Returns every swap file in the isolated state directory.
fn state_swaps(state: &Path) -> Vec<PathBuf> {
    swap_files(&state.join("swap"))
}

/// Returns every `.swp` file directly inside `directory`.
fn swap_files(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with('.') && name.ends_with(".swp"))
        })
        .collect()
}

/// Rewrites the writer PID so a same-process simulated crash looks stale.
fn make_swaps_stale(state: &Path) {
    let current = format!("pid={}", std::process::id());
    for path in state_swaps(state) {
        let text = fs::read_to_string(&path).expect("read state swap");
        assert!(text.contains(&current), "swap must carry this process PID");
        fs::write(path, text.replace(&current, "pid=999999")).expect("make swap stale");
    }
}

#[test]
fn untitled_swap_flushes_at_200_bytes_and_recovers_only_to_an_explicit_path() {
    let isolated = isolate("lifecycle");
    let mut editor = Editor::untitled();

    let mut terminal = ScriptedTerminal::new(typed(&"a".repeat(199)));
    assert!(editor.run(&mut terminal).is_err(), "simulate a crash");
    assert!(
        state_swaps(&isolated.state).is_empty(),
        "an untitled swap under 200 changed bytes must stay deferred"
    );

    let mut terminal = ScriptedTerminal::new([InputEvent::Char('i'), InputEvent::Char('a')]);
    assert!(editor.run(&mut terminal).is_err(), "simulate a crash");
    let swaps = state_swaps(&isolated.state);
    assert_eq!(
        swaps.len(),
        1,
        "the 200th changed byte must flush the untitled swap"
    );
    let swap_text = fs::read_to_string(&swaps[0]).expect("read untitled swap");
    assert!(
        swap_text.contains(&"a".repeat(200)),
        "the untitled swap must contain the typed text"
    );

    make_swaps_stale(&isolated.state);
    let saved = isolated.root.join("recovered.txt");
    let mut recovered = Editor::recover_untitled().expect("recover untitled swap");
    let mut terminal = ScriptedTerminal::new(
        [
            command("q"),
            command("w"),
            command(&format!("w \"{}\"", saved.display())),
            command("q"),
        ]
        .concat(),
    );
    recovered.run(&mut terminal).expect("save recovered text");

    let screen = terminal.screen();
    assert!(
        screen.contains(&"a".repeat(40)),
        "the recovered text was not drawn: {screen}"
    );
    assert!(
        screen.contains("E37: No write since last change"),
        "a recovered untitled buffer must refuse plain :q: {screen}"
    );
    assert!(
        screen.contains("E32: No file name"),
        "a recovered untitled buffer must refuse bare :w: {screen}"
    );
    assert_eq!(
        fs::read_to_string(&saved).expect("read saved recovered text"),
        "a".repeat(200),
        "the recovered text must be written to the explicit path"
    );
    assert!(
        state_swaps(&isolated.state).is_empty(),
        "a successful save must remove the untitled swap"
    );
    assert!(
        !isolated.root.join("recovered.txt~").exists(),
        "a successful save must not leave a stray backup"
    );

    fs::remove_dir_all(isolated.root).expect("clean scratch directory");
}

#[test]
fn untitled_swap_is_preserved_on_demand_and_recovers_dirty() {
    let isolated = isolate("preserve");
    let mut editor = Editor::untitled();
    let mut terminal = ScriptedTerminal::new([typed("X"), command("preserve")].concat());
    assert!(editor.run(&mut terminal).is_err(), "simulate a crash");

    assert!(
        terminal.screen().contains("swap file preserved"),
        "the preserve command did not report success: {}",
        terminal.screen()
    );
    let swaps = state_swaps(&isolated.state);
    assert_eq!(swaps.len(), 1, ":preserve must create the untitled swap");
    assert!(
        fs::read_to_string(&swaps[0])
            .expect("read preserved swap")
            .contains("X")
    );

    make_swaps_stale(&isolated.state);
    let mut recovered = Editor::recover_untitled().expect("recover untitled swap");
    let mut terminal = ScriptedTerminal::new(command("q"));
    assert!(
        recovered.run(&mut terminal).is_err(),
        "a refused quit must keep waiting for input"
    );
    assert!(
        terminal
            .screen()
            .contains("E37: No write since last change"),
        "recovery must mark the untitled buffer modified: {}",
        terminal.screen()
    );
    assert_eq!(
        state_swaps(&isolated.state).len(),
        1,
        "a refused quit must keep the recovered untitled swap"
    );

    fs::remove_dir_all(isolated.root).expect("clean scratch directory");
}

#[test]
fn recover_command_loads_a_named_swap_without_writing_until_save() {
    let isolated = isolate("named");
    let path = isolated.root.join("note.txt");
    fs::write(&path, "seed\n").expect("seed file");

    let mut crashed = Editor::open(&path).expect("open file");
    let mut terminal = ScriptedTerminal::new(typed("X"));
    assert!(crashed.run(&mut terminal).is_err(), "simulate a crash");
    assert_eq!(swap_files(&isolated.root).len(), 1);

    let mut editor = Editor::untitled();
    let mut terminal = ScriptedTerminal::new(
        [
            command(&format!("recover \"{}\"", path.display())),
            command("q"),
        ]
        .concat(),
    );
    assert!(editor.run(&mut terminal).is_err(), "simulate a crash");

    let screen = terminal.screen();
    assert!(
        screen.contains("recovered swap file"),
        ":recover did not report the recovered swap: {screen}"
    );
    assert!(
        screen.contains("E37: No write since last change"),
        "the recovered named buffer must refuse plain :q: {screen}"
    );
    assert_eq!(
        fs::read_to_string(&path).expect("read original"),
        "seed\n",
        ":recover must not write the original"
    );
    assert_eq!(
        swap_files(&isolated.root).len(),
        1,
        ":recover must keep the retained swap until a save"
    );

    let mut recovered = Editor::recover(&path).expect("recover the named swap directly");
    let mut terminal = ScriptedTerminal::new([command("w"), command("q")].concat());
    recovered.run(&mut terminal).expect("save recovered file");

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "Xseed\n"
    );
    assert!(
        swap_files(&isolated.root).is_empty(),
        "a successful save must remove the named swap"
    );

    fs::remove_dir_all(isolated.root).expect("clean scratch directory");
}

#[test]
fn recover_without_a_swap_reports_e305() {
    let isolated = isolate("no-swap");
    let path = isolated.root.join("clean.txt");
    fs::write(&path, "seed\n").expect("seed file");

    let mut editor = Editor::open(&path).expect("open clean file");
    let mut terminal = ScriptedTerminal::new([command("recover"), command("q")].concat());
    editor.run(&mut terminal).expect("clean quit");

    assert!(
        terminal
            .screen()
            .contains("E305: No swap file found for [No Name]"),
        ":recover must report E305 when no untitled swap exists: {}",
        terminal.screen()
    );
    assert_eq!(fs::read_to_string(&path).expect("read file"), "seed\n");

    fs::remove_dir_all(isolated.root).expect("clean scratch directory");
}

#[test]
fn recover_refuses_to_replace_a_modified_buffer() {
    let isolated = isolate("modified");
    let mut editor = Editor::untitled();
    let mut terminal =
        ScriptedTerminal::new([typed("X"), command("recover"), command("q!")].concat());
    editor.run(&mut terminal).expect("forced quit");

    assert!(
        terminal
            .screen()
            .contains("E37: No write since last change"),
        ":recover must not discard a modified buffer: {}",
        terminal.screen()
    );

    fs::remove_dir_all(isolated.root).expect("clean scratch directory");
}
