//! End-to-end encoding tests for the editor loop (`PROJECT_SPEC` BUF-007).
//!
//! Each test writes real bytes, drives the same `Editor::run` loop the binary
//! uses, and inspects the resulting file. A failed conversion must leave the
//! original bytes untouched and report the offending position.

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
                width: 200,
                height: 10,
            },
            events: events.into_iter().collect(),
            frames: Vec::new(),
        }
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
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "input script exhausted"))
    }
}

fn scratch() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "vanta-encoding-{}-{}-{nanos}",
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

fn run(path: &Path, events: Vec<InputEvent>) -> Vec<Frame> {
    let mut editor = Editor::open(path).expect("open file");
    let mut terminal = ScriptedTerminal::new(events);
    editor.run(&mut terminal).expect("run editor");
    terminal.frames
}

fn frames_contain(frames: &[Frame], needle: &str) -> bool {
    frames
        .iter()
        .any(|frame| frame.rows.iter().any(|row| row.contains(needle)))
}

fn utf16_le(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

fn utf16_be(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xFE, 0xFF];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    bytes
}

#[test]
fn preserves_utf8_bom_utf16le_and_utf16be_on_save() {
    let cases = [
        (
            "utf8-bom",
            b"\xEF\xBB\xBFa\n".to_vec(),
            b"\xEF\xBB\xBFXa\n".to_vec(),
        ),
        ("utf16le", utf16_le("a\n"), utf16_le("Xa\n")),
        ("utf16be", utf16_be("a\n"), utf16_be("Xa\n")),
    ];

    for (name, original, expected) in cases {
        let directory = scratch();
        let path = directory.join(format!("{name}.txt"));
        fs::write(&path, &original).expect("seed encoded file");

        let events = [
            vec![
                InputEvent::Char('i'),
                InputEvent::Char('X'),
                InputEvent::Esc,
            ],
            command("w"),
            command("q"),
        ]
        .concat();
        run(&path, events);

        assert_eq!(
            fs::read(&path).expect("read saved file"),
            expected,
            "{name} must keep its encoding and BOM after editing"
        );
        fs::remove_dir_all(directory).expect("clean scratch directory");
    }
}

#[test]
fn latin1_without_bom_round_trips_and_warns() {
    let directory = scratch();
    let path = directory.join("latin1.txt");
    let original = b"caf\xE9\n".to_vec();
    fs::write(&path, &original).expect("seed latin1 file");

    let frames = run(&path, [command("w"), command("q")].concat());

    assert_eq!(
        fs::read(&path).expect("read saved file"),
        original,
        "an unchanged latin1 file must write back byte-for-byte"
    );
    assert!(
        frames_contain(&frames, "latin1"),
        "the editor must warn that the file was opened as latin1"
    );
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn unrepresentable_latin1_character_aborts_the_save_without_touching_the_file() {
    let directory = scratch();
    let path = directory.join("latin1.txt");
    let original = b"caf\xE9\n".to_vec();
    fs::write(&path, &original).expect("seed latin1 file");

    let events = [
        vec![
            InputEvent::Char('i'),
            InputEvent::Char('€'),
            InputEvent::Esc,
        ],
        command("w"),
        command("q!"),
    ]
    .concat();
    let frames = run(&path, events);

    assert_eq!(
        fs::read(&path).expect("read original after failed conversion"),
        original,
        "a failed conversion must leave the target untouched"
    );
    assert!(
        frames_contain(&frames, "E513"),
        "the editor must report the unrepresentable character"
    );
    assert!(
        frames_contain(&frames, "byte 0"),
        "the error must name the offending byte offset"
    );
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn explicit_fileencoding_converts_latin1_to_utf8_on_write() {
    let directory = scratch();
    let path = directory.join("latin1.txt");
    fs::write(&path, b"caf\xE9\n").expect("seed latin1 file");

    let events = [
        command("set fileencoding=utf-8"),
        command("w"),
        command("q"),
    ]
    .concat();
    run(&path, events);

    assert_eq!(
        fs::read(&path).expect("read converted file"),
        "café\n".as_bytes(),
        ":set fileencoding=utf-8 must change the next write's encoding"
    );
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn bomb_option_controls_the_utf8_byte_order_mark() {
    let directory = scratch();
    let path = directory.join("bom.txt");
    fs::write(&path, "a\n").expect("seed file");

    let events = [command("set bomb"), command("w"), command("q")].concat();
    run(&path, events);

    assert_eq!(
        fs::read(&path).expect("read BOM file"),
        b"\xEF\xBB\xBFa\n",
        ":set bomb must write a UTF-8 BOM"
    );
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn invalid_utf8_opens_as_lossless_latin1() {
    let directory = scratch();
    let path = directory.join("binary.txt");
    let original = b"a\xFF\n".to_vec();
    fs::write(&path, &original).expect("seed invalid UTF-8 file");

    let frames = run(&path, [command("w"), command("q")].concat());

    assert_eq!(
        fs::read(&path).expect("read saved file"),
        original,
        "the lossless fallback must not rewrite undecodable bytes"
    );
    assert!(
        frames_contain(&frames, "latin1"),
        "the fallback must be visible in the status row"
    );
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn unknown_fileencoding_is_rejected() {
    let directory = scratch();
    let path = directory.join("note.txt");
    fs::write(&path, "a\n").expect("seed file");

    let frames = run(
        &path,
        [command("set fileencoding=utf-7"), command("q")].concat(),
    );

    assert!(
        frames_contain(&frames, "E474"),
        "an unsupported fileencoding must be rejected"
    );
    fs::remove_dir_all(directory).expect("clean scratch directory");
}
