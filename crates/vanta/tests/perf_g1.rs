//! G1 large-file performance evidence (`PERF-006`, `PERF-007`).
//!
//! This test is intentionally ignored by the normal gate because it writes a
//! generated 1 GiB fixture and opens it in the release binary. Run it with:
//!
//! ```text
//! cargo xtask perf
//! ```
//!
//! The fixture is created under the system temporary directory and removed on
//! completion. The harness measures the real binary through a ConPTY/PTY,
//! performs 1,000 Insert-mode edits, scrolls to force a viewport move, samples
//! resident memory, and fails when the specification budgets are exceeded.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

const GIB: u64 = 1024 * 1024 * 1024;
const OPEN_BUDGET: Duration = Duration::from_secs(8);
const SCROLL_BYTE_BUDGET: usize = 2048;
const EDIT_COUNT: usize = 1000;
const PTY_ROWS: u16 = 24;
const PTY_COLS: u16 = 80;

struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after the Unix epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("vanta-g1-perf-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&path).expect("create perf scratch directory");
        Self { path }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct Session {
    child: Box<dyn Child + Send + Sync>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    output: Arc<Mutex<Vec<u8>>>,
    reader: Option<JoinHandle<()>>,
    master: Box<dyn MasterPty + Send>,
}

impl Session {
    fn spawn(path: &Path, state: &Path) -> Self {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: PTY_ROWS,
                cols: PTY_COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open pseudo-terminal");

        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_vanta"));
        command.arg(path);
        command.env("VANTA_STATE_DIR", state);
        let child = pair
            .slave
            .spawn_command(command)
            .expect("spawn the release vanta binary");
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
            writer,
            output,
            reader: Some(reader_thread),
            master,
        }
    }

    fn mark(&self) -> usize {
        self.output.lock().expect("PTY output lock").len()
    }

    fn contains_from(&self, needle: &[u8], offset: usize) -> bool {
        let output = self.output.lock().expect("PTY output lock");
        output
            .get(offset..)
            .unwrap_or_default()
            .windows(needle.len())
            .any(|window| window == needle)
    }

    fn wait_for_from(&self, needle: &[u8], offset: usize, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.contains_from(needle, offset) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    fn wait_for(&self, needle: &[u8], timeout: Duration) -> bool {
        self.wait_for_from(needle, 0, timeout)
    }

    fn bytes_since(&self, offset: usize) -> usize {
        self.output
            .lock()
            .expect("PTY output lock")
            .get(offset..)
            .map_or(0, <[u8]>::len)
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut writer = self.writer.lock().expect("PTY writer lock");
        writer.write_all(bytes).expect("write to the PTY");
        writer.flush().expect("flush the PTY");
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => {
                    let _ = self.child.kill();
                    return false;
                }
                Err(error) => panic!("failed to wait for vanta: {error}"),
            }
        }
    }

    fn finish(mut self) {
        drop(self.writer);
        drop(self.master);
        if let Some(reader) = self.reader.take() {
            reader.join().expect("join the PTY reader");
        }
    }
}

fn write_fixture(path: &Path) {
    let line = [b'x'; 79];
    let mut block = Vec::with_capacity(1024 * 1024);
    while block.len() + 80 <= 1024 * 1024 {
        block.extend_from_slice(&line);
        block.push(b'\n');
    }

    let mut file = BufWriter::new(File::create(path).expect("create 1 GiB fixture"));
    let mut remaining = GIB;
    while remaining > block.len() as u64 {
        file.write_all(&block).expect("write fixture block");
        remaining -= block.len() as u64;
    }
    if remaining > 0 {
        let tail = usize::try_from(remaining).expect("bounded tail");
        file.write_all(&block[..tail]).expect("write fixture tail");
    }
    file.flush().expect("flush fixture");
}

fn json_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn host_label() -> String {
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown-host".to_string());
    let cpu = std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_else(|_| "unknown-cpu".to_string());
    format!("{host} / {cpu}")
}

fn terminal_label() -> String {
    if std::env::var_os("WT_SESSION").is_some() {
        "Windows Terminal / ConPTY".to_string()
    } else if let Ok(program) = std::env::var("TERM_PROGRAM") {
        format!("{program} / PTY")
    } else if let Ok(term) = std::env::var("TERM") {
        format!("{term} / PTY")
    } else {
        "ConPTY / no TERM".to_string()
    }
}

#[cfg(windows)]
fn resident_bytes(pid: u32) -> Option<u64> {
    let script = format!("(Get-Process -Id {pid}).WorkingSet64");
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

#[cfg(target_os = "linux")]
fn resident_bytes(pid: u32) -> Option<u64> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

#[cfg(target_os = "macos")]
fn resident_bytes(pid: u32) -> Option<u64> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let kib: u64 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .ok()?;
    Some(kib * 1024)
}

#[test]
#[ignore = "cargo xtask perf; generates and opens a 1 GiB fixture"]
fn perf_006_and_007_on_a_generated_1_gib_fixture() {
    let scratch = Scratch::new();
    let fixture = scratch.path.join("large.log");
    let state = scratch.path.join("state");
    fs::create_dir_all(&state).expect("create state directory");
    write_fixture(&fixture);
    let fixture_bytes = fs::metadata(&fixture).expect("fixture metadata").len();
    assert_eq!(fixture_bytes, GIB);

    let started = Instant::now();
    let mut session = Session::spawn(&fixture, &state);
    assert!(
        session.wait_for(b"[LARGE]", Duration::from_secs(30)),
        "the first editable frame did not appear within 30s"
    );
    let open_elapsed = started.elapsed();
    let pid = session.child.process_id().expect("vanta process id");

    let after_open = resident_bytes(pid).expect("sample RSS after open");
    let edit_started = Instant::now();
    let before_edit = session.mark();
    let mut edits = Vec::with_capacity(EDIT_COUNT + 2);
    edits.push(b'i');
    edits.extend(std::iter::repeat_n(b'x', EDIT_COUNT));
    edits.push(0x1b);
    session.write(&edits);
    assert!(
        session.wait_for_from(b"[+]", before_edit, Duration::from_secs(30)),
        "the 1,000 Insert-mode edits did not reach the modified frame"
    );
    assert!(
        session.wait_for_from(b"NORMAL", before_edit, Duration::from_secs(10)),
        "the edits did not return to Normal mode"
    );
    let edit_elapsed = edit_started.elapsed();
    std::thread::sleep(Duration::from_millis(100));
    let after_edit = resident_bytes(pid).expect("sample RSS after edits");

    // Move to the last visible text row, then make exactly one scroll frame.
    session.write(b"0");
    session.write(&[b'j'; 22]);
    assert!(
        session.wait_for(b"\x1b[23;1H", Duration::from_secs(10)),
        "the cursor did not reach the last visible text row"
    );
    std::thread::sleep(Duration::from_millis(100));
    let before_scroll = session.mark();
    let scroll_started = Instant::now();
    session.write(b"j");
    assert!(
        session.wait_for_from(b"\x1b[23;1H", before_scroll, Duration::from_secs(10)),
        "the one-line scroll frame did not appear"
    );
    let scroll_detect_elapsed = scroll_started.elapsed();
    std::thread::sleep(Duration::from_millis(100));
    let scroll_bytes = session.bytes_since(before_scroll);
    let after_scroll = resident_bytes(pid).expect("sample RSS after scroll");
    let peak_rss = after_open.max(after_edit).max(after_scroll);
    let rss_ratio = peak_rss as f64 / fixture_bytes as f64;

    session.write(b":q!\r");
    assert!(
        session.wait_for_exit(Duration::from_secs(10)),
        "vanta did not exit after the perf run"
    );
    session.finish();

    println!(
        "{{\"perf\":\"G1\",\"os\":\"{}\",\"arch\":\"{}\",\"host\":\"{}\",\"terminal\":\"{}\",\"fixture_bytes\":{},\"open_ms\":{:.3},\"edit_ms\":{:.3},\"edits\":{},\"scroll_detect_ms\":{:.3},\"scroll_bytes\":{},\"rss_bytes\":{},\"rss_ratio\":{:.3},\"open_budget_ms\":8000,\"rss_budget_bytes\":{},\"scroll_budget_bytes\":{}}}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        json_escape(&host_label()),
        json_escape(&terminal_label()),
        fixture_bytes,
        open_elapsed.as_secs_f64() * 1000.0,
        edit_elapsed.as_secs_f64() * 1000.0,
        EDIT_COUNT,
        scroll_detect_elapsed.as_secs_f64() * 1000.0,
        scroll_bytes,
        peak_rss,
        rss_ratio,
        (fixture_bytes as f64 * 2.5) as u64,
        SCROLL_BYTE_BUDGET
    );

    assert!(
        open_elapsed <= OPEN_BUDGET,
        "PERF-006 failed: 1 GiB first editable frame took {open_elapsed:?}, budget {OPEN_BUDGET:?}"
    );
    assert!(
        peak_rss <= (fixture_bytes as f64 * 2.5) as u64,
        "PERF-007 failed: {peak_rss} RSS bytes is {rss_ratio:.3}x the 1 GiB file, budget 2.5x"
    );
    assert!(
        scroll_bytes <= SCROLL_BYTE_BUDGET,
        "TERM-004 scroll budget failed: one line emitted {scroll_bytes} bytes, budget {SCROLL_BYTE_BUDGET}"
    );
}
