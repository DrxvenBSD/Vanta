//! G2 acceptance tests for the shared Normal-mode grammar.
//!
//! These drive the real editor loop and save/reload a file, so a parser unit
//! test cannot pass while execution ignores a count, register or text object.

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
}

impl ScriptedTerminal {
    fn new(events: Vec<InputEvent>) -> Self {
        Self {
            events: events.into(),
        }
    }
}

impl Terminal for ScriptedTerminal {
    fn size(&mut self) -> io::Result<Size> {
        Ok(Size {
            width: 120,
            height: 24,
        })
    }

    fn draw(&mut self, _frame: &Frame) -> io::Result<()> {
        Ok(())
    }

    fn read_event(&mut self) -> io::Result<InputEvent> {
        self.events
            .pop_front()
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "input exhausted"))
    }
}

fn scratch() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "vanta-g2-{}-{}-{nanos}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).expect("scratch directory");
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

fn run(seed: &str, events: Vec<InputEvent>) -> String {
    let directory = scratch();
    let path: &Path = &directory.join("buffer.txt");
    fs::write(path, seed).expect("seed file");
    let mut script = events;
    script.extend(command("wq"));
    let mut editor = Editor::open(path).expect("open");
    let mut terminal = ScriptedTerminal::new(script);
    editor.run(&mut terminal).expect("run editor");
    let output = fs::read_to_string(path).expect("read result");
    fs::remove_dir_all(directory).expect("clean scratch");
    output
}

#[test]
fn counted_operator_composition_deletes_the_combined_motion() {
    assert_eq!(
        run("one two three four five six seven\n", chars("3d2w")),
        "seven\n"
    );
}

#[test]
fn a_named_linewise_register_can_be_put_back() {
    assert_eq!(
        run(
            "one\ntwo\nthree\nfour\n",
            [chars("\"a3dd"), chars("G"), chars("\"ap"),].concat(),
        ),
        "four\none\ntwo\nthree\n"
    );
}

#[test]
fn change_inside_quotes_replaces_only_the_contents() {
    assert_eq!(
        run(
            "say \"hello\" now\n",
            [
                chars("f\""),
                chars("ci\""),
                chars("X"),
                vec![InputEvent::Esc],
            ]
            .concat(),
        ),
        "say \"X\" now\n"
    );
}

#[test]
fn delete_around_parentheses_removes_the_pair_and_contents() {
    assert_eq!(
        run("f(a, b)\n", [chars("f("), chars("da(")].concat(),),
        "f\n"
    );
}

#[test]
fn change_word_does_not_swallow_the_following_space() {
    assert_eq!(
        run(
            "hello world\n",
            [chars("cw"), chars("X"), vec![InputEvent::Esc]].concat(),
        ),
        "X world\n"
    );
}

#[test]
fn dot_repeats_the_last_mutating_command() {
    assert_eq!(run("one two three\n", chars("dw.")), "three\n");
}

#[test]
fn a_recorded_macro_replays_through_the_same_parser() {
    assert_eq!(
        run("abcdef\n", [chars("qaxq"), chars("@a")].concat(),),
        "cdef\n"
    );
}

#[test]
fn normal_runs_commands_through_the_shared_parser() {
    assert_eq!(run("abc\n", command("normal A!")), "abc!\n");
}

#[test]
fn counts_reach_simple_actions() {
    assert_eq!(run("abcdef\n", chars("3x")), "def\n");
}
