//! Generated-fixture tests for large-file mode (`PROJECT_SPEC` BUF-012).
//!
//! The fixtures are created at test time at the 20 MiB threshold, so no large
//! blob is committed. The assertions cover activation, the visible indicator,
//! prompt opening, a UTF-16 streaming round-trip and a below-threshold file.

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use vanta::editor::Editor;
use vanta::terminal::{Frame, InputEvent, Size, Terminal};

const THRESHOLD: u64 = 20 * 1024 * 1024;
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
        "vanta-large-{}-{}-{nanos}",
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

fn frames_contain(frames: &[Frame], needle: &str) -> bool {
    frames
        .iter()
        .any(|frame| frame.rows.iter().any(|row| row.contains(needle)))
}

fn run(path: &Path, events: Vec<InputEvent>) -> Vec<Frame> {
    let mut editor = Editor::open(path).expect("open generated fixture");
    let mut terminal = ScriptedTerminal::new(events);
    editor.run(&mut terminal).expect("run editor");
    terminal.frames
}

fn ascii_chunk() -> Vec<u8> {
    let line = b"0123456789abcdefghijklmnopqrstuvwxyz\n";
    let mut chunk = Vec::with_capacity(64 * 1024);
    while chunk.len() < 64 * 1024 {
        chunk.extend_from_slice(line);
    }
    chunk.truncate(64 * 1024);
    chunk
}

fn write_utf8_fixture(path: &Path, size: u64) {
    let chunk = ascii_chunk();
    let mut file = BufWriter::new(File::create(path).expect("create UTF-8 fixture"));
    let mut remaining = size;
    while remaining > 0 {
        let take = remaining.min(chunk.len() as u64) as usize;
        file.write_all(&chunk[..take]).expect("write UTF-8 fixture");
        remaining -= take as u64;
    }
    file.flush().expect("flush UTF-8 fixture");
}

fn write_utf16le_fixture(path: &Path, size: u64) {
    assert!(size >= 2 && size.is_multiple_of(2));
    let pattern = "0123456789abcdef\n".repeat(4096);
    let mut chunk = Vec::with_capacity(64 * 1024);
    for unit in pattern.encode_utf16() {
        chunk.extend_from_slice(&unit.to_le_bytes());
    }
    chunk.truncate(64 * 1024);

    let mut file = BufWriter::new(File::create(path).expect("create UTF-16 fixture"));
    file.write_all(&[0xFF, 0xFE]).expect("write UTF-16 BOM");
    let mut remaining = size - 2;
    while remaining > 0 {
        let take = remaining.min(chunk.len() as u64) as usize & !1;
        assert!(take > 0, "the UTF-16 chunk must contain a whole unit");
        file.write_all(&chunk[..take])
            .expect("write UTF-16 fixture");
        remaining -= take as u64;
    }
    file.flush().expect("flush UTF-16 fixture");
}

fn write_latin1_fixture(path: &Path, size: u64) {
    let line = b"latin\xFF1\n";
    let mut chunk = Vec::with_capacity(64 * 1024);
    while chunk.len() < 64 * 1024 {
        chunk.extend_from_slice(line);
    }
    chunk.truncate(64 * 1024);

    let mut file = BufWriter::new(File::create(path).expect("create latin-1 fixture"));
    let mut remaining = size;
    while remaining > 0 {
        let take = remaining.min(chunk.len() as u64) as usize;
        file.write_all(&chunk[..take])
            .expect("write latin-1 fixture");
        remaining -= take as u64;
    }
    file.flush().expect("flush latin-1 fixture");
}

#[test]
fn large_file_activates_is_visible_and_saves_unchanged_bytes() {
    let directory = scratch();
    let path = directory.join("large.txt");
    write_utf8_fixture(&path, THRESHOLD);
    let before = fs::read(&path).expect("read generated fixture");

    let start = Instant::now();
    let mut editor = Editor::open(&path).expect("open large UTF-8 file");
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "opening a 20 MiB file took {elapsed:?}, expected under 5s"
    );

    let mut terminal = ScriptedTerminal::new([command("w"), command("q")].concat());
    editor.run(&mut terminal).expect("save and quit large file");

    assert!(
        frames_contain(&terminal.frames, "[LARGE]"),
        "the status row must visibly report large-file mode"
    );
    assert_eq!(
        fs::read(&path).expect("read saved large file"),
        before,
        "an unchanged save must reproduce the source bytes exactly"
    );
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn large_utf16_file_round_trips_through_the_streaming_decoder() {
    let directory = scratch();
    let path = directory.join("large-utf16.txt");
    write_utf16le_fixture(&path, THRESHOLD);
    let before = fs::read(&path).expect("read generated UTF-16 fixture");

    let frames = run(&path, [command("w"), command("q")].concat());

    assert!(
        frames_contain(&frames, "[LARGE]"),
        "a large UTF-16 file must also report large-file mode"
    );
    assert_eq!(
        fs::read(&path).expect("read saved large UTF-16 file"),
        before,
        "streaming UTF-16 decoding must not alter the encoded bytes"
    );
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn just_below_the_threshold_does_not_activate_large_file_mode() {
    let directory = scratch();
    let path = directory.join("under.txt");
    write_utf8_fixture(&path, THRESHOLD - 1);

    let frames = run(&path, command("q"));

    assert!(
        !frames_contain(&frames, "[LARGE]"),
        "a file below 20 MiB must not activate large-file mode"
    );
    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn large_invalid_utf8_file_round_trips_as_lossless_latin1() {
    let directory = scratch();
    let path = directory.join("large-latin1.txt");
    write_latin1_fixture(&path, THRESHOLD + 1);
    let before = fs::read(&path).expect("read generated latin-1 fixture");

    let frames = run(&path, [command("w"), command("q")].concat());

    assert!(
        frames_contain(&frames, "[LARGE]"),
        "the large fallback file must report large-file mode"
    );
    assert!(
        frames_contain(&frames, "latin1"),
        "the fallback must remain visible in the status row"
    );
    assert_eq!(
        fs::read(&path).expect("read saved large latin-1 file"),
        before,
        "a large latin-1 fallback must not silently re-encode the source"
    );
    fs::remove_dir_all(directory).expect("clean scratch directory");
}
