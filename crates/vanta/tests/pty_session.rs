//! Runs the real `vanta` binary inside a pseudo-terminal.
//!
//! The headless loop test proves the editor logic; these tests prove that the
//! binary enters raw mode and the alternate screen, draws the buffer, accepts
//! real terminal input, saves the file and restores the terminal. A debug-only
//! panic injection proves the panic hook restores the terminal too.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

const ENTER_ALTERNATE_SCREEN: &str = "\u{1b}[?1049h";
const LEAVE_ALTERNATE_SCREEN: &str = "\u{1b}[?1049l";

struct Session {
    child: Box<dyn Child + Send + Sync>,
    master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    output: Arc<Mutex<Vec<u8>>>,
    reader: Option<JoinHandle<()>>,
}

impl Session {
    fn spawn(path: &Path, extra_env: Option<(&str, &str)>) -> Self {
        Self::spawn_session(Some(path), extra_env)
    }

    fn spawn_untitled(extra_env: Option<(&str, &str)>) -> Self {
        Self::spawn_session(None, extra_env)
    }

    fn spawn_session(path: Option<&Path>, extra_env: Option<(&str, &str)>) -> Self {
        let args: Vec<std::ffi::OsString> = path
            .map(|path| vec![path.as_os_str().to_owned()])
            .unwrap_or_default();
        let env: Vec<(String, String)> = extra_env
            .map(|(key, value)| vec![(key.to_string(), value.to_string())])
            .unwrap_or_default();
        Self::spawn_command(&args, None, &env)
    }

    /// Spawns the binary with explicit arguments, working directory and env.
    fn spawn_command(
        args: &[std::ffi::OsString],
        cwd: Option<&Path>,
        extra_env: &[(String, String)],
    ) -> Self {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open pseudo-terminal");

        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_vanta"));
        for argument in args {
            command.arg(argument);
        }
        if let Some(cwd) = cwd {
            command.cwd(cwd);
        }
        for (key, value) in extra_env {
            command.env(key, value);
        }

        let child = pair
            .slave
            .spawn_command(command)
            .expect("spawn the vanta binary");
        drop(pair.slave);

        let master = pair.master;
        let mut reader = master.try_clone_reader().expect("clone PTY reader");
        let writer = Arc::new(Mutex::new(master.take_writer().expect("take PTY writer")));

        let output = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&output);
        let responses = Arc::clone(&writer);
        let reader_thread = std::thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            let mut answered = 0usize;
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        let queries = {
                            let mut captured = captured.lock().expect("PTY output lock");
                            captured.extend_from_slice(&buffer[..count]);
                            captured
                                .windows(4)
                                .filter(|window| *window == b"\x1b[6n")
                                .count()
                        };
                        // ConPTY asks the host terminal for the cursor
                        // position; answer as a real terminal would.
                        while answered < queries {
                            let mut writer = responses.lock().expect("PTY writer lock");
                            if writer
                                .write_all(b"\x1b[1;1R")
                                .and_then(|()| writer.flush())
                                .is_err()
                            {
                                break;
                            }
                            drop(writer);
                            answered += 1;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            child,
            master,
            writer,
            output,
            reader: Some(reader_thread),
        }
    }

    fn wait_for(&self, needle: &[u8], timeout: Duration) -> bool {
        self.wait_for_from(needle, 0, timeout)
    }

    fn wait_for_from(&self, needle: &[u8], offset: usize, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let output = self.output.lock().expect("PTY output lock");
            let tail = output.get(offset..).unwrap_or_default();
            if tail.windows(needle.len()).any(|window| window == needle) {
                return true;
            }
            drop(output);
            if !needle.contains(&0x1b)
                && self.screen_contains(&String::from_utf8_lossy(needle), offset)
            {
                // The screen can match mid-frame. Wait for the output length
                // to settle so a caller never marks a half-written frame.
                let first = self.output.lock().expect("PTY output lock").len();
                std::thread::sleep(Duration::from_millis(20));
                let second = self.output.lock().expect("PTY output lock").len();
                if first == second {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    fn mark(&self) -> usize {
        self.output.lock().expect("PTY output lock").len()
    }

    fn contains_from(&self, needle: &[u8], offset: usize) -> bool {
        let output = self.output.lock().expect("PTY output lock");
        let tail = output.get(offset..).unwrap_or_default();
        tail.windows(needle.len()).any(|window| window == needle)
    }

    fn screen_contains(&self, needle: &str, offset: usize) -> bool {
        let output = self.output.lock().expect("PTY output lock");
        let split = offset.min(output.len());
        let mut screen = TestScreen::default();
        screen.apply(&output[..split]);
        screen.apply(&output[split..]);
        screen.render().contains(needle)
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut writer = self.writer.lock().expect("PTY writer lock");
        writer.write_all(bytes).expect("write to the PTY");
        writer.flush().expect("flush the PTY");
    }

    fn snapshot(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().expect("PTY output lock")).into_owned()
    }

    fn kill(&mut self) {
        self.child.kill().expect("kill the vanta process");
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(None) => {
                    self.child.kill().expect("kill the hung vanta process");
                    return false;
                }
                Err(error) => panic!("failed to wait for vanta: {error}"),
            }
        }
    }

    fn finish(mut self) -> String {
        drop(self.writer);
        drop(self.master);
        if let Some(reader) = self.reader.take() {
            reader.join().expect("join the PTY reader");
        }
        String::from_utf8_lossy(&self.output.lock().expect("PTY output lock")).into_owned()
    }
}

#[derive(Clone)]
struct TestScreen {
    rows: Vec<Vec<char>>,
    row: usize,
    col: usize,
}

impl Default for TestScreen {
    fn default() -> Self {
        Self {
            rows: vec![vec![' '; 80]; 24],
            row: 0,
            col: 0,
        }
    }
}

impl TestScreen {
    fn apply(&mut self, bytes: &[u8]) {
        let chars: Vec<char> = String::from_utf8_lossy(bytes).chars().collect();
        let mut index = 0;
        while index < chars.len() {
            match chars[index] {
                '\x1b' => index = self.escape(&chars, index),
                '\r' => {
                    self.col = 0;
                    index += 1;
                }
                '\n' => {
                    self.row += 1;
                    if self.row >= self.rows.len() {
                        self.rows.remove(0);
                        self.rows.push(vec![' '; 80]);
                        self.row = self.rows.len() - 1;
                    }
                    index += 1;
                }
                '\u{8}' => {
                    self.col = self.col.saturating_sub(1);
                    index += 1;
                }
                character => {
                    if self.row < self.rows.len() && self.col < 80 {
                        self.rows[self.row][self.col] = character;
                    }
                    self.col = self.col.saturating_add(1).min(80);
                    index += 1;
                }
            }
        }
    }

    fn escape(&mut self, chars: &[char], index: usize) -> usize {
        let Some(next) = chars.get(index + 1).copied() else {
            return chars.len();
        };
        if next == '[' {
            let mut end = index + 2;
            while end < chars.len() && !('@'..='~').contains(&chars[end]) {
                end += 1;
            }
            if end >= chars.len() {
                return chars.len();
            }
            let params: String = chars[index + 2..end].iter().collect();
            self.csi(&params, chars[end]);
            return end + 1;
        }
        if next == ']' {
            let mut end = index + 2;
            while end < chars.len() && chars[end] != '\u{7}' {
                end += 1;
            }
            return (end + 1).min(chars.len());
        }
        (index + 2).min(chars.len())
    }

    fn csi(&mut self, params: &str, final_byte: char) {
        let numbers: Vec<usize> = params
            .split(';')
            .map(|part| part.trim_start_matches('?').parse::<usize>().unwrap_or(0))
            .collect();
        let first = numbers.first().copied().unwrap_or(0);
        match final_byte {
            'H' | 'f' => {
                self.row = first.saturating_sub(1).min(self.rows.len() - 1);
                self.col = numbers
                    .get(1)
                    .copied()
                    .unwrap_or(0)
                    .saturating_sub(1)
                    .min(80);
            }
            'A' => self.row = self.row.saturating_sub(first.max(1)),
            'B' => self.row = (self.row + first.max(1)).min(self.rows.len() - 1),
            'C' => self.col = (self.col + first.max(1)).min(80),
            'D' => self.col = self.col.saturating_sub(first.max(1)),
            'G' => self.col = first.saturating_sub(1).min(80),
            'd' => self.row = first.saturating_sub(1).min(self.rows.len() - 1),
            'K' => {
                if self.col < 80 {
                    self.rows[self.row][self.col..].fill(' ');
                }
            }
            'J' => {
                if first == 2 {
                    for row in &mut self.rows {
                        row.fill(' ');
                    }
                } else if self.col < 80 {
                    self.rows[self.row][self.col..].fill(' ');
                    for row in &mut self.rows[self.row + 1..] {
                        row.fill(' ');
                    }
                }
            }
            'X' => {
                if self.col < 80 {
                    let end = (self.col + first.max(1)).min(80);
                    self.rows[self.row][self.col..end].fill(' ');
                }
            }
            'S' => {
                self.rows.remove(0);
                self.rows.push(vec![' '; 80]);
            }
            'T' => {
                self.rows.pop();
                self.rows.insert(0, vec![' '; 80]);
            }
            _ => {}
        }
    }

    fn render(&self) -> String {
        self.rows
            .iter()
            .map(|row| row.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn scratch(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "vanta-pty-{}-{}-{nanos}-{label}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).expect("create scratch directory");
    path
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

/// Waits until one swap in `directory` contains `needle`.
fn wait_for_swap_text(directory: &Path, needle: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(entries) = fs::read_dir(directory) {
            for entry in entries.flatten() {
                if let Ok(text) = fs::read_to_string(entry.path())
                    && text.contains(needle)
                {
                    return true;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn real_binary_opens_edits_saves_and_restores() {
    let directory = scratch("edit");
    let path = directory.join("note.txt");
    fs::write(&path, "hello\nworld\n").expect("seed file");

    let mut session = Session::spawn(&path, None);
    let drawn = session.wait_for(b"hello", Duration::from_secs(10));
    assert!(
        drawn,
        "the initial frame was not drawn; output: {:?}",
        session.snapshot()
    );

    session.write(b"iX");
    assert!(
        session.wait_for(b"Xhello", Duration::from_secs(10)),
        "the edited frame was not drawn; output: {:?}",
        session.snapshot()
    );
    session.write(b"\x1b");
    session.write(b":w\r");
    assert!(
        session.wait_for(b"written", Duration::from_secs(10)),
        "the save result was not drawn; output: {:?}",
        session.snapshot()
    );
    session.write(b":q\r");

    assert!(
        session.wait_for_exit(Duration::from_secs(10)),
        "vanta did not exit after :w and :q"
    );
    let output = session.finish();

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "Xhello\nworld\n"
    );
    assert!(
        output.contains(ENTER_ALTERNATE_SCREEN),
        "the alternate screen was never entered: {output:?}"
    );
    assert!(
        output.contains(LEAVE_ALTERNATE_SCREEN),
        "the alternate screen was not restored: {output:?}"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn real_binary_undoes_redoes_and_saves_the_redone_text() {
    let directory = scratch("undo");
    let path = directory.join("note.txt");
    fs::write(&path, "seed\n").expect("seed file");

    let mut session = Session::spawn(&path, None);
    assert!(
        session.wait_for(b"seed", Duration::from_secs(10)),
        "the initial frame was not drawn; output: {:?}",
        session.snapshot()
    );

    session.write(b"iXY");
    assert!(
        session.wait_for(b"XYseed", Duration::from_secs(10)),
        "the typed text was not drawn; output: {:?}",
        session.snapshot()
    );
    session.write(b"\x1b");
    assert!(
        session.wait_for(b"[+]", Duration::from_secs(10)),
        "leaving Insert did not leave the cursor after the typed text; output: {:?}",
        session.snapshot()
    );

    let before_undo = session.mark();
    session.write(b"u");
    assert!(
        session.wait_for_from(b"\x1b[H", before_undo, Duration::from_secs(10)),
        "undo did not restore the clean buffer and its cursor; output: {:?}",
        session.snapshot()
    );
    assert!(
        !session.contains_from(b"[+]", before_undo),
        "undo did not clear the modified flag; output: {:?}",
        session.snapshot()
    );

    let before_redo = session.mark();
    session.write(b"\x12");
    assert!(
        session.wait_for_from(b"XYseed", before_redo, Duration::from_secs(10)),
        "redo did not restore the typed text; output: {:?}",
        session.snapshot()
    );
    assert!(
        session.wait_for_from(b"[+]", before_redo, Duration::from_secs(10)),
        "redo did not restore the cursor and the modified flag; output: {:?}",
        session.snapshot()
    );

    let before_save = session.mark();
    session.write(b":w\r");
    assert!(
        session.wait_for_from(b"written", before_save, Duration::from_secs(10)),
        "the save result was not drawn; output: {:?}",
        session.snapshot()
    );
    assert!(
        !session.contains_from(b"[+]", before_save),
        "a successful save did not clear the modified flag; output: {:?}",
        session.snapshot()
    );
    session.write(b":q\r");
    assert!(
        session.wait_for_exit(Duration::from_secs(10)),
        "vanta did not exit after :w and :q"
    );
    let output = session.finish();

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "XYseed\n",
        "the saved file must contain the redone text"
    );
    assert!(
        output.contains(LEAVE_ALTERNATE_SCREEN),
        "the alternate screen was not restored: {output:?}"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn real_binary_deletes_yanks_and_puts_a_whole_line() {
    let directory = scratch("linewise");
    let path = directory.join("note.txt");
    fs::write(&path, "one\ntwo\nthree\n").expect("seed file");

    let mut session = Session::spawn(&path, None);
    assert!(
        session.wait_for(b"one", Duration::from_secs(10)),
        "the initial frame was not drawn; output: {:?}",
        session.snapshot()
    );

    let before_delete = session.mark();
    session.write(b"dd");
    assert!(
        session.wait_for_from(b"[+]", before_delete, Duration::from_secs(10)),
        "dd did not delete the first line and mark the buffer modified; output: {:?}",
        session.snapshot()
    );

    let before_put = session.mark();
    session.write(b"yy");
    session.write(b"p");
    assert!(
        session.wait_for_from(b"two", before_put, Duration::from_secs(10)),
        "yy p did not insert a line below the current one; output: {:?}",
        session.snapshot()
    );

    session.write(b":w\r");
    assert!(
        session.wait_for(b"written", Duration::from_secs(10)),
        "the save result was not drawn; output: {:?}",
        session.snapshot()
    );
    session.write(b":q\r");
    assert!(
        session.wait_for_exit(Duration::from_secs(10)),
        "vanta did not exit after :w and :q"
    );
    let output = session.finish();

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "two\ntwo\nthree\n",
        "the saved file must contain the linewise dd/yy/p result"
    );
    assert!(
        output.contains(LEAVE_ALTERNATE_SCREEN),
        "the alternate screen was not restored: {output:?}"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn real_binary_visually_selects_across_lines_and_deletes() {
    let directory = scratch("visual");
    let path = directory.join("note.txt");
    fs::write(&path, "abc\ndef\n").expect("seed file");

    let mut session = Session::spawn(&path, None);
    assert!(
        session.wait_for(b"abc", Duration::from_secs(10)),
        "the initial frame was not drawn; output: {:?}",
        session.snapshot()
    );

    let before_selection = session.mark();
    session.write(b"vj");
    assert!(
        session.wait_for_from(b"VISU", before_selection, Duration::from_secs(10)),
        "the characterwise selection was not drawn; output: {:?}",
        session.snapshot()
    );
    assert!(
        session.contains_from(b"\x1b[7mabc", before_selection)
            || session.contains_from(b"\x1b[7ma", before_selection),
        "the selected abc cells were not drawn with reverse video; output: {:?}",
        session.snapshot()
    );
    assert!(
        session.contains_from(b"\x1b[7md", before_selection)
            || session.contains_from(b"\x1b[7m\r\nd", before_selection)
            || session.contains_from(b"\x1b[7m\x1b[1Cbc", before_selection)
            || session.contains_from(b"\x1b[7mabc \r\nd", before_selection),
        "the selected d cell on the next row was not drawn with reverse video; output: {:?}",
        session.snapshot()
    );

    let before_delete = session.mark();
    session.write(b"d");
    assert!(
        session.wait_for_from(b"[+]", before_delete, Duration::from_secs(10)),
        "the Visual delete did not return to Normal with a modified buffer; output: {:?}",
        session.snapshot()
    );

    session.write(b":w\r");
    assert!(
        session.wait_for(b"written", Duration::from_secs(10)),
        "the save result was not drawn; output: {:?}",
        session.snapshot()
    );
    session.write(b":q\r");
    assert!(
        session.wait_for_exit(Duration::from_secs(10)),
        "vanta did not exit after the Visual save"
    );
    let output = session.finish();

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "ef\n",
        "the saved file must contain the bytes outside the cross-line selection"
    );
    assert!(
        output.contains(LEAVE_ALTERNATE_SCREEN),
        "the alternate screen was not restored: {output:?}"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn real_binary_searches_and_moves_the_cursor() {
    let directory = scratch("search");
    let path = directory.join("note.txt");
    fs::write(&path, "alpha\nbeta\ngamma\n").expect("seed file");

    let mut session = Session::spawn(&path, None);
    assert!(
        session.wait_for(b"alpha", Duration::from_secs(10)),
        "the initial frame was not drawn; output: {:?}",
        session.snapshot()
    );

    let before_prompt = session.mark();
    session.write(b"/beta");
    assert!(
        session.wait_for_from(b"/beta", before_prompt, Duration::from_secs(10)),
        "the search prompt was not drawn; output: {:?}",
        session.snapshot()
    );

    let before_search = session.mark();
    session.write(b"\r");
    assert!(
        session.wait_for_from(
            b"NORMAL  2:1  note.txt",
            before_search,
            Duration::from_secs(10)
        ),
        "the search did not move to line 2 col 1; output: {:?}",
        session.snapshot()
    );
    assert!(
        session.contains_from(b"\x1b[2;1H", before_search),
        "the terminal cursor was not moved to the match row; output: {:?}",
        session.snapshot()
    );
    assert!(
        session.contains_from(b"\x1b[7mbeta", before_search)
            || session.contains_from(b"\x1b[7m\r\nbeta", before_search)
            || session.contains_from(b"\x1b[7m\x1b[2;1Hbeta", before_search),
        "the current search match was not drawn in reverse video; output: {:?}",
        session.snapshot()
    );

    session.write(b":q\r");
    assert!(
        session.wait_for_exit(Duration::from_secs(10)),
        "vanta did not exit after a search-only run"
    );
    let output = session.finish();

    assert_eq!(
        fs::read_to_string(&path).expect("read untouched file"),
        "alpha\nbeta\ngamma\n",
        "search must not modify the file"
    );
    assert!(
        output.contains(LEAVE_ALTERNATE_SCREEN),
        "the alternate screen was not restored: {output:?}"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[cfg(debug_assertions)]
#[test]
fn panic_hook_restores_the_terminal() {
    let directory = scratch("panic");
    let path = directory.join("note.txt");
    fs::write(&path, "hello\n").expect("seed file");

    let mut session = Session::spawn(&path, Some(("VANTA_TEST_PANIC_AFTER_INIT", "1")));
    let exited = session.wait_for_exit(Duration::from_secs(10));
    assert!(
        exited,
        "the injected panic did not end the process; output: {:?}",
        session.snapshot()
    );
    let output = session.finish();

    assert!(
        output.contains(ENTER_ALTERNATE_SCREEN),
        "the alternate screen was never entered: {output:?}"
    );
    assert!(
        output.contains(LEAVE_ALTERNATE_SCREEN),
        "the panic hook did not leave the alternate screen: {output:?}"
    );
    assert!(
        output.contains("injected panic"),
        "the panic message is missing from the session: {output:?}"
    );
    assert_eq!(
        fs::read_to_string(&path).expect("read untouched file"),
        "hello\n",
        "a panic must not save the buffer"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn real_binary_kill_leaves_a_recoverable_swap_and_recovery_saves_it() {
    let directory = scratch("kill-recover");
    let path = directory.join("note.txt");
    fs::write(&path, "seed\n").expect("seed file");

    let mut session = Session::spawn(&path, None);
    assert!(
        session.wait_for(b"seed", Duration::from_secs(10)),
        "the initial frame was not drawn; output: {:?}",
        session.snapshot()
    );

    session.write(b"iRECOVERED");
    assert!(
        session.wait_for(b"RECOVEREDseed", Duration::from_secs(10)),
        "the unsaved edit was not drawn; output: {:?}",
        session.snapshot()
    );

    session.kill();
    assert!(
        session.wait_for_exit(Duration::from_secs(10)),
        "the killed vanta process did not exit"
    );
    let killed_output = session.finish();
    assert!(
        killed_output.contains(ENTER_ALTERNATE_SCREEN),
        "the killed process never entered the editor screen: {killed_output:?}"
    );

    assert_eq!(
        fs::read_to_string(&path).expect("read original after kill"),
        "seed\n",
        "killing the editor must not modify the target"
    );
    let swaps = swap_files(&directory);
    assert_eq!(swaps.len(), 1, "the kill must leave exactly one swap");
    let swap_text = fs::read_to_string(&swaps[0]).expect("read swap after kill");
    assert!(
        swap_text.contains("RECOVEREDseed"),
        "the swap must contain the unsaved text: {swap_text:?}"
    );

    let mut recovery = Session::spawn(&path, None);
    assert!(
        recovery.wait_for(b"E325: ATTENTION", Duration::from_secs(10)),
        "the recovery prompt was not drawn; output: {:?}",
        recovery.snapshot()
    );
    assert!(
        recovery.wait_for(b"no longer running", Duration::from_secs(10)),
        "the prompt did not identify the dead writer; output: {:?}",
        recovery.snapshot()
    );

    recovery.write(b"r");
    assert!(
        recovery.wait_for(b"RECOVEREDseed", Duration::from_secs(10)),
        "the recovered text was not drawn; output: {:?}",
        recovery.snapshot()
    );
    recovery.write(b":wq\r");
    assert!(
        recovery.wait_for_exit(Duration::from_secs(10)),
        "the recovery session did not exit after :wq"
    );
    let recovery_output = recovery.finish();

    assert_eq!(
        fs::read_to_string(&path).expect("read recovered file"),
        "RECOVEREDseed\n",
        "recovery plus :wq must write the unsaved text"
    );
    assert!(
        swap_files(&directory).is_empty(),
        "a successful recovery save must remove the swap"
    );
    assert!(
        recovery_output.contains(LEAVE_ALTERNATE_SCREEN),
        "the recovery session did not restore the alternate screen: {recovery_output:?}"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn real_binary_failed_save_keeps_the_original_and_a_backup() {
    let directory = scratch("backup-failure");
    let path = directory.join("note.txt");
    fs::write(&path, "seed\n").expect("seed file");
    let backup = path.with_file_name("note.txt~");
    let original_permissions = fs::metadata(&path).expect("target metadata").permissions();
    let mut read_only = original_permissions.clone();
    read_only.set_readonly(true);
    fs::set_permissions(&path, read_only).expect("mark target read-only");

    let mut session = Session::spawn(&path, None);
    assert!(
        session.wait_for(b"seed", Duration::from_secs(10)),
        "the initial frame was not drawn; output: {:?}",
        session.snapshot()
    );

    session.write(b"iX\x1b");
    assert!(
        session.wait_for(b"Xseed", Duration::from_secs(10)),
        "the edit was not drawn; output: {:?}",
        session.snapshot()
    );

    session.write(b":w\r");
    assert!(
        session.wait_for(b"save failed", Duration::from_secs(10)),
        "the read-only save did not report a failure; output: {:?}",
        session.snapshot()
    );
    assert_eq!(
        fs::read_to_string(&path).expect("read original after failed save"),
        "seed\n",
        "a failed save must leave the target byte-for-byte intact"
    );
    assert_eq!(
        fs::read_to_string(&backup).expect("read aborted-save backup"),
        "seed\n",
        "a failed save must leave a usable previous-contents backup"
    );

    fs::set_permissions(&path, original_permissions).expect("restore target permissions");

    session.write(b":w\r");
    assert!(
        session.wait_for(b"written", Duration::from_secs(10)),
        "the retry after restoring permissions was not written; output: {:?}",
        session.snapshot()
    );
    session.write(b":q\r");
    assert!(
        session.wait_for_exit(Duration::from_secs(10)),
        "vanta did not exit after the successful retry"
    );
    let output = session.finish();

    assert_eq!(
        fs::read_to_string(&path).expect("read retried save"),
        "Xseed\n"
    );
    assert!(
        !backup.exists(),
        "a successful default writebackup must remove the transient backup"
    );
    assert!(
        swap_files(&directory).is_empty(),
        "a clean save must remove the swap"
    );
    assert!(
        output.contains(LEAVE_ALTERNATE_SCREEN),
        "the failed-then-successful save did not restore the alternate screen: {output:?}"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn real_binary_scrolls_a_long_line_into_view() {
    let directory = scratch("long-line");
    let path = directory.join("note.txt");
    let line = format!(
        "START{}VISIBLE_AFTER_SCROLL{}\n",
        "x".repeat(200),
        "y".repeat(200)
    );
    fs::write(&path, &line).expect("seed file");

    let mut session = Session::spawn(&path, None);
    assert!(
        session.wait_for(b"START", Duration::from_secs(10)),
        "the initial frame was not drawn; output: {:?}",
        session.snapshot()
    );

    let before_scroll = session.mark();
    assert!(
        !session.contains_from(b"VISIBLE_AFTER_SCROLL", before_scroll),
        "the marker should start beyond the 80-column viewport"
    );

    session.write(&vec![b'l'; 230]);
    assert!(
        session.wait_for_from(
            b"VISIBLE_AFTER_SCROLL",
            before_scroll,
            Duration::from_secs(10)
        ),
        "the line was not scrolled far enough to reveal the marker; output: {:?}",
        session.snapshot()
    );
    assert!(
        session.wait_for_from(b"231", before_scroll, Duration::from_secs(10)),
        "the cursor did not reach column 231; output: {:?}",
        session.snapshot()
    );

    session.write(b":q\r");
    assert!(
        session.wait_for_exit(Duration::from_secs(10)),
        "vanta did not exit after scrolling"
    );
    let output = session.finish();

    assert_eq!(
        fs::read_to_string(&path).expect("read untouched file"),
        line,
        "scrolling must not modify the file"
    );
    assert!(
        output.contains(LEAVE_ALTERNATE_SCREEN),
        "the alternate screen was not restored: {output:?}"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn real_binary_starts_untitled_and_saves_with_write_as() {
    let directory = scratch("startup");
    let path = directory.join("from-startup.txt");

    let mut session = Session::spawn_untitled(None);
    assert!(
        session.wait_for(b"a modal terminal editor", Duration::from_secs(10)),
        "the startup page was not drawn; output: {:?}",
        session.snapshot()
    );
    assert!(
        session.wait_for(b"[No Name]", Duration::from_secs(10)),
        "the untitled status was not drawn; output: {:?}",
        session.snapshot()
    );
    assert!(
        session.wait_for("\u{f07c}".as_bytes(), Duration::from_secs(10)),
        "the startup menu icon was not drawn; output: {:?}",
        session.snapshot()
    );
    assert!(
        session.wait_for("██".as_bytes(), Duration::from_secs(10)),
        "the large block title was not drawn; output: {:?}",
        session.snapshot()
    );

    session.write(b"ihello");
    assert!(
        session.wait_for(b"hello", Duration::from_secs(10)),
        "inserted text was not drawn; output: {:?}",
        session.snapshot()
    );
    session.write(b"\x1b");
    session.write(format!(":w \"{}\"\r", path.display()).as_bytes());
    assert!(
        session.wait_for(b"written", Duration::from_secs(10)),
        "the save-as command did not complete; output: {:?}",
        session.snapshot()
    );
    session.write(b":q\r");
    assert!(
        session.wait_for_exit(Duration::from_secs(10)),
        "vanta did not exit after saving an untitled buffer"
    );
    let output = session.finish();

    assert_eq!(
        fs::read_to_string(&path).expect("read saved file"),
        "hello",
        "the startup buffer must save exactly what was typed"
    );
    assert!(
        output.contains(ENTER_ALTERNATE_SCREEN),
        "the alternate screen was never entered: {output:?}"
    );
    assert!(
        output.contains(LEAVE_ALTERNATE_SCREEN),
        "the alternate screen was not restored: {output:?}"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn real_binary_kill_recovers_an_untitled_buffer_to_an_explicit_path() {
    let directory = scratch("untitled-kill");
    let state = directory.join("state");
    let cwd = directory.join("cwd");
    fs::create_dir_all(&state).expect("create state directory");
    fs::create_dir_all(&cwd).expect("create working directory");
    let env = [(
        "VANTA_STATE_DIR".to_string(),
        state.to_string_lossy().into_owned(),
    )];
    let saved = directory.join("recovered.txt");

    let mut session = Session::spawn_command(&[], Some(&cwd), &env);
    assert!(
        session.wait_for(b"[No Name]", Duration::from_secs(10)),
        "the untitled status was not drawn; output: {:?}",
        session.snapshot()
    );
    session.write(b"iRECOVERED");
    assert!(
        session.wait_for(b"RECOVERED", Duration::from_secs(10)),
        "the typed text was not drawn; output: {:?}",
        session.snapshot()
    );
    session.write(b"\x1b");
    assert!(
        wait_for_swap_text(&state.join("swap"), "RECOVERED", Duration::from_secs(20)),
        "the four-second idle flush did not write the untitled swap; state: {:?}",
        fs::read_dir(state.join("swap")).map(|entries| entries
            .flatten()
            .map(|entry| entry.path())
            .collect::<Vec<_>>())
    );

    session.kill();
    assert!(
        session.wait_for_exit(Duration::from_secs(10)),
        "the killed vanta process did not exit"
    );
    let _ = session.finish();
    assert!(
        fs::read_dir(&cwd)
            .expect("read working directory")
            .next()
            .is_none(),
        "an untitled session must leave no file in the working directory"
    );

    let mut recovery = Session::spawn_command(&[std::ffi::OsString::from("-r")], Some(&cwd), &env);
    assert!(
        recovery.wait_for(b"RECOVERED", Duration::from_secs(10)),
        "vanta -r did not draw the recovered untitled text; output: {:?}",
        recovery.snapshot()
    );
    recovery.write(format!(":w \"{}\"\r", saved.display()).as_bytes());
    assert!(
        recovery.wait_for(b"written", Duration::from_secs(10)),
        "the recovered buffer did not save to the explicit path; output: {:?}",
        recovery.snapshot()
    );
    recovery.write(b":q\r");
    assert!(
        recovery.wait_for_exit(Duration::from_secs(10)),
        "vanta did not exit after saving the recovered untitled buffer"
    );
    let output = recovery.finish();

    assert_eq!(
        fs::read_to_string(&saved).expect("read recovered file"),
        "RECOVERED",
        "vanta -r must save exactly the recovered text at the explicit path"
    );
    assert!(
        swap_files(&state.join("swap")).is_empty(),
        "the save must remove the untitled swap"
    );
    assert!(
        fs::read_dir(&cwd)
            .expect("read working directory")
            .next()
            .is_none(),
        "the recovered session must leave no stray files in the working directory"
    );
    assert!(
        output.contains(LEAVE_ALTERNATE_SCREEN),
        "the alternate screen was not restored: {output:?}"
    );

    fs::remove_dir_all(directory).expect("clean scratch directory");
}

#[test]
fn real_binary_one_line_scroll_stays_within_cell_byte_budget() {
    let directory = scratch("scroll-budget");
    let path = directory.join("note.txt");
    let content: String = (0..120)
        .map(|index| format!("line-{index:03}-{}\n", "x".repeat(70)))
        .collect();
    fs::write(&path, content).expect("seed large buffer");

    let mut session = Session::spawn(&path, None);
    assert!(
        session.wait_for(b"line-000", Duration::from_secs(10)),
        "the initial frame was not drawn; output: {:?}",
        session.snapshot()
    );

    // 23 text rows fit above the status row. Move to the last one, then make
    // one scroll frame with a single `j`.
    session.write(&[b'j'; 22]);
    assert!(
        session.wait_for(b"\x1b[23;1H", Duration::from_secs(10)),
        "the cursor did not reach the last visible text row; output: {:?}",
        session.snapshot()
    );

    let before = session.mark();
    session.write(b"j");
    assert!(
        session.wait_for_from(b"\x1b[23;1H", before, Duration::from_secs(10)),
        "the one-line scroll frame was not drawn; output: {:?}",
        session.snapshot()
    );
    std::thread::sleep(Duration::from_millis(100));

    let emitted = {
        let output = session.output.lock().expect("PTY output lock");
        output.get(before..).map_or(0, <[u8]>::len)
    };
    // Windows ConPTY measured 1,871 bytes for this frame. The 2,048 ceiling
    // leaves 208 bytes (~11%) of host re-render headroom. Vanta's direct
    // Screen output is smaller (scroll region plus one new row), but ConPTY
    // repaints the 23-row text area, so this is not a cell-minimal byte count.
    assert!(
        emitted <= 2048,
        "one-line scroll emitted {emitted} bytes, above the 2048-byte cell budget; output: {:?}",
        session.snapshot()
    );
    assert!(
        !session.contains_from(b"\x1b[2J", before),
        "one-line scroll cleared the whole screen; output: {:?}",
        session.snapshot()
    );

    session.write(b":q\r");
    assert!(
        session.wait_for_exit(Duration::from_secs(10)),
        "vanta did not exit after the scroll measurement"
    );
    let output = session.finish();
    assert!(
        output.contains(LEAVE_ALTERNATE_SCREEN),
        "the alternate screen was not restored: {output:?}"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn real_binary_cursor_only_frame_does_not_repaint_buffer_rows() {
    let directory = scratch("damage");
    let path = directory.join("note.txt");
    fs::write(&path, "alpha\nbeta\ngamma\n").expect("seed file");

    let mut session = Session::spawn(&path, None);
    assert!(
        session.wait_for(b"NORMAL  1:1  note.txt", Duration::from_secs(10)),
        "the initial frame was not drawn; output: {:?}",
        session.snapshot()
    );

    let before = session.mark();
    session.write(b"l");
    assert!(
        session.wait_for_from(b"\x1b[1;2H", before, Duration::from_secs(10)),
        "the cursor-only move was not drawn; output: {:?}",
        session.snapshot()
    );
    assert!(
        !session.contains_from(b"alpha", before),
        "a cursor-only move repainted the first buffer row; output: {:?}",
        session.snapshot()
    );
    assert!(
        !session.contains_from(b"beta", before),
        "a cursor-only move repainted the second buffer row; output: {:?}",
        session.snapshot()
    );
    assert!(
        !session.contains_from(b"\x1b[2J", before),
        "a cursor-only move cleared the whole screen; output: {:?}",
        session.snapshot()
    );

    session.write(b":q\r");
    assert!(
        session.wait_for_exit(Duration::from_secs(10)),
        "vanta did not exit after a clean :q"
    );
    let output = session.finish();
    assert!(
        output.contains(LEAVE_ALTERNATE_SCREEN),
        "the alternate screen was not restored: {output:?}"
    );

    fs::remove_dir_all(&directory).expect("clean scratch directory");
}
