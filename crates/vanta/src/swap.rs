//! Crash-recovery swap files.
//!
//! A swap file contains the current text, cursor and modified flag of a
//! buffer. A named buffer's swap lives next to the target file and uses the
//! canonical path hash in its name; an untitled buffer's swap lives in the
//! state directory's `swap/` subdirectory because it has no target. A named
//! swap falls back to that same state directory when the target directory is
//! not writable. Every swap records the writing process's PID, host and start
//! time so a later session can distinguish a live session from a crashed one.
//!
//! The file format is deliberately small and self-checking:
//!
//! ```text
//! VANTA-SWAP 1
//! [kind=untitled]
//! path-hash=<hex>
//! pid=<number>
//! host=<name>
//! started=<unix-seconds>
//! cursor-line=<number>
//! cursor-col=<number>
//! modified=<0|1>
//! text-bytes=<number>
//! text-checksum=<hex>
//!
//! <UTF-8 buffer text>
//! ```
//!
//! Writes go through [`vanta_fs::save_atomic`], so a crash during a swap
//! update leaves the previous swap version intact. The file is restricted to
//! `0600` on Unix and the swap directory to `0700`. Recovery never writes to
//! the target file; the caller decides whether to save recovered text
//! afterward.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
// Linux reads `/proc` instead of spawning a process, so the import is only
// needed where the `tasklist` or `ps` probe is compiled in.
#[cfg(any(windows, all(unix, not(target_os = "linux"))))]
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use vanta_text::{ByteCol, LineCol, LineNumber};

/// The first line of every swap file.
const MAGIC: &str = "VANTA-SWAP 1";
/// The delay after which a future-dated swap timestamp is treated as suspect.
const FUTURE_SLACK_SECS: u64 = 300;
/// Process-local sequence that keeps two untitled swaps in one second apart.
static UNTITLED_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A failed swap-file operation.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SwapError {
    /// The swap file could not be read.
    #[error("cannot read swap file {}: {source}", path.display())]
    Read {
        /// The swap path.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// The swap file could not be written.
    #[error("cannot write swap file {}: {source}", path.display())]
    Write {
        /// The swap path.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: vanta_fs::SaveError,
    },

    /// The swap file could not be removed.
    #[error("cannot remove swap file {}: {source}", path.display())]
    Remove {
        /// The swap path.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// The swap file could not be restricted to owner-only permissions.
    #[cfg(unix)]
    #[error("cannot restrict permissions on swap file {}: {source}", path.display())]
    Permissions {
        /// The swap path.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// The header is structurally invalid.
    #[error("invalid swap file: {0}")]
    InvalidFormat(String),

    /// The file ended before the declared text did.
    #[error("truncated swap file")]
    Truncated,

    /// The text does not match its recorded checksum.
    #[error("swap file checksum does not match its text")]
    ChecksumMismatch,

    /// The text is not valid UTF-8.
    #[error("swap text is not valid UTF-8: {0}")]
    TextEncoding(#[from] std::string::FromUtf8Error),
}

/// Which kind of buffer a swap belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SwapKind {
    /// The swap belongs to a file the user named.
    Named,
    /// The swap belongs to an untitled `[No Name]` buffer.
    Untitled,
}

/// One parsed swap file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SwapSnapshot {
    /// Whether the writer was editing a named file or an untitled buffer.
    pub kind: SwapKind,
    /// Hash of the canonical target path this swap belongs to, or of the
    /// untitled session identity.
    pub path_hash: u64,
    /// Process that wrote the latest swap update.
    pub pid: u32,
    /// Hostname reported by that process.
    pub host: String,
    /// Unix timestamp when that process's swap session started.
    pub started_at: u64,
    /// Cursor when the latest update was flushed.
    pub cursor: LineCol,
    /// Whether the buffer was modified when the update was flushed.
    pub modified: bool,
    /// The complete buffer text at that moment.
    pub text: String,
}

/// A swap file belonging to one buffer.
#[derive(Clone, Debug)]
pub(crate) struct SwapFile {
    /// Where the swap is written when the target directory is writable, or
    /// the only path for an untitled buffer.
    primary: PathBuf,
    /// The state-directory copy used when the primary cannot be written.
    fallback: PathBuf,
    /// The path currently holding this swap.
    path: PathBuf,
    hash: u64,
    started_at: u64,
    kind: SwapKind,
}

impl SwapFile {
    /// Creates the swap identity for `original`.
    ///
    /// Returns `None` when the path has no file name and therefore cannot
    /// form a swap name. The file is not created until [`SwapFile::write`].
    pub(crate) fn new(original: &Path) -> Option<Self> {
        let canonical = canonical_identity(original);
        let hash = hash_path(&canonical);
        let primary = swap_path(&canonical, hash)?;
        let fallback = state_swap_path()
            .and_then(|directory| primary.file_name().map(|name| directory.join(name)))
            .unwrap_or_else(|| primary.clone());
        Some(Self {
            path: primary.clone(),
            primary,
            fallback,
            hash,
            started_at: now_secs(),
            kind: SwapKind::Named,
        })
    }

    /// Creates a swap identity for an untitled buffer in the state directory.
    ///
    /// The name carries the same 16-digit hash the header records, so a later
    /// `-r` can validate what it finds. Returns `None` when no state
    /// directory is available or it cannot be created.
    pub(crate) fn untitled() -> Option<Self> {
        Some(Self::untitled_in(swap_directory()?))
    }

    /// Builds an untitled swap identity inside `directory` without writing.
    fn untitled_in(directory: PathBuf) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.subsec_nanos())
            .unwrap_or(0);
        let sequence = UNTITLED_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let identity = format!(
            "untitled:{}-{}-{nanos}-{sequence}",
            std::process::id(),
            now_secs()
        );
        let hash = checksum(identity.as_bytes());
        let path = directory.join(format!(".untitled-{hash:016x}.swp"));
        Self {
            path: path.clone(),
            primary: path.clone(),
            fallback: path,
            hash,
            started_at: now_secs(),
            kind: SwapKind::Untitled,
        }
    }

    /// Adopts an existing untitled swap found in the state directory.
    ///
    /// The header supplies the hash and session start, so this does not guess
    /// them from the name. A file whose name and header disagree, or that is
    /// not an untitled swap, is rejected.
    pub(crate) fn adopt_untitled(path: PathBuf) -> Result<Self, SwapError> {
        let bytes = fs::read(&path).map_err(|source| SwapError::Read {
            path: path.clone(),
            source,
        })?;
        let snapshot = parse(&bytes)?;
        if snapshot.kind != SwapKind::Untitled {
            return Err(SwapError::InvalidFormat(
                "not an untitled swap file".to_string(),
            ));
        }
        if swap_name_hash(&path) != Some(snapshot.path_hash) {
            return Err(SwapError::InvalidFormat(
                "swap name does not match its header".to_string(),
            ));
        }
        Ok(Self {
            path: path.clone(),
            primary: path.clone(),
            fallback: path,
            hash: snapshot.path_hash,
            started_at: snapshot.started_at,
            kind: SwapKind::Untitled,
        })
    }

    /// Returns the swap path.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the canonical-path hash used in the name and header.
    pub(crate) fn hash(&self) -> u64 {
        self.hash
    }

    /// Points the swap at an existing copy, preferring the target directory.
    ///
    /// Returns `true` when a swap file exists for this buffer. A named swap
    /// that could not be written beside its target is discovered from the
    /// state-directory fallback.
    pub(crate) fn discover(&mut self) -> bool {
        if self.primary.exists() {
            self.path = self.primary.clone();
            return true;
        }
        if self.fallback != self.primary && self.fallback.exists() {
            self.path = self.fallback.clone();
            return true;
        }
        false
    }

    /// Reads and validates the swap file.
    pub(crate) fn read(&self) -> Result<SwapSnapshot, SwapError> {
        let bytes = fs::read(&self.path).map_err(|source| SwapError::Read {
            path: self.path.clone(),
            source,
        })?;
        parse(&bytes)
    }

    /// Writes a complete swap update atomically.
    ///
    /// A named swap that cannot be written beside its target is retried once
    /// in the state-directory fallback; the swap stays there for the rest of
    /// the session and is discovered there next time.
    pub(crate) fn write(
        &mut self,
        text: &str,
        cursor: LineCol,
        modified: bool,
    ) -> Result<(), SwapError> {
        let bytes = encode(
            self.hash,
            self.started_at,
            self.kind,
            text,
            cursor,
            modified,
        );
        match write_atomic(&self.path, &bytes) {
            Ok(()) => Ok(()),
            Err(error) if self.path != self.fallback => {
                if !prepare_swap_directory(&self.fallback) {
                    return Err(error);
                }
                match write_atomic(&self.fallback, &bytes) {
                    Ok(()) => {
                        self.path = self.fallback.clone();
                        Ok(())
                    }
                    Err(_) => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    }

    /// Removes the swap file. A missing file is treated as already removed.
    ///
    /// Both the primary and the fallback copy are removed so a failover does
    /// not leave a stale claim behind.
    pub(crate) fn remove(&self) -> Result<(), SwapError> {
        remove_path(&self.primary)?;
        if self.fallback != self.primary {
            remove_path(&self.fallback)?;
        }
        Ok(())
    }

    /// Returns why this swap is stale, or `None` when its writer may still be
    /// running.
    ///
    /// A swap is stale when it came from another host, was written by a
    /// process that no longer exists, or carries a timestamp from the future.
    /// A PID reused within this process is also stale because the session
    /// start time no longer matches.
    pub(crate) fn stale_reason(&self, snapshot: &SwapSnapshot) -> Option<String> {
        if snapshot.host != current_host() {
            return Some(format!("swap was created on host `{}`", snapshot.host));
        }
        if snapshot.pid == std::process::id() && snapshot.started_at != self.started_at {
            return Some(format!("process {} has been reused", snapshot.pid));
        }
        if !pid_is_alive(snapshot.pid) {
            return Some(format!("process {} is no longer running", snapshot.pid));
        }
        if snapshot.started_at > now_secs().saturating_add(FUTURE_SLACK_SECS) {
            return Some("swap timestamp is in the future".to_string());
        }
        None
    }
}

/// Encodes one complete swap file.
fn encode(
    path_hash: u64,
    started_at: u64,
    kind: SwapKind,
    text: &str,
    cursor: LineCol,
    modified: bool,
) -> Vec<u8> {
    let kind = match kind {
        SwapKind::Named => "",
        SwapKind::Untitled => "kind=untitled\n",
    };
    let header = format!(
        "{MAGIC}\n\
         {kind}\
         path-hash={path_hash:016x}\n\
         pid={}\n\
         host={}\n\
         started={started_at}\n\
         cursor-line={}\n\
         cursor-col={}\n\
         modified={}\n\
         text-bytes={}\n\
         text-checksum={:016x}\n\n",
        std::process::id(),
        current_host(),
        cursor.line.get(),
        cursor.col.get(),
        u8::from(modified),
        text.len(),
        checksum(text.as_bytes()),
    );
    let mut bytes = header.into_bytes();
    bytes.extend_from_slice(text.as_bytes());
    bytes
}

/// Parses and validates a complete swap file.
fn parse(bytes: &[u8]) -> Result<SwapSnapshot, SwapError> {
    let split = bytes
        .windows(2)
        .position(|window| window == b"\n\n")
        .ok_or(SwapError::Truncated)?;
    let header = std::str::from_utf8(&bytes[..split])
        .map_err(|_| SwapError::InvalidFormat("header is not UTF-8".to_string()))?;
    let mut lines = header.lines();
    if lines.next() != Some(MAGIC) {
        return Err(SwapError::InvalidFormat("unknown magic line".to_string()));
    }

    let mut kind = None;
    let mut path_hash = None;
    let mut pid = None;
    let mut host = None;
    let mut started_at = None;
    let mut cursor_line = None;
    let mut cursor_col = None;
    let mut modified = None;
    let mut text_bytes = None;
    let mut text_checksum = None;

    for line in lines {
        let Some((key, value)) = line.split_once('=') else {
            return Err(SwapError::InvalidFormat(format!(
                "field `{line}` has no value"
            )));
        };
        match key {
            "kind" => {
                kind = Some(match value {
                    "named" => SwapKind::Named,
                    "untitled" => SwapKind::Untitled,
                    _ => {
                        return Err(SwapError::InvalidFormat(format!(
                            "invalid swap kind `{value}`"
                        )));
                    }
                });
            }
            "path-hash" => path_hash = Some(parse_hex(value, "path-hash")?),
            "pid" => {
                pid = Some(
                    value
                        .parse::<u32>()
                        .map_err(|_| SwapError::InvalidFormat(format!("invalid pid `{value}`")))?,
                )
            }
            "host" => host = Some(value.to_string()),
            "started" => {
                started_at = Some(value.parse::<u64>().map_err(|_| {
                    SwapError::InvalidFormat(format!("invalid start time `{value}`"))
                })?)
            }
            "cursor-line" => cursor_line = Some(parse_usize(value, "cursor-line")?),
            "cursor-col" => cursor_col = Some(parse_usize(value, "cursor-col")?),
            "modified" => {
                modified = Some(match value {
                    "0" => false,
                    "1" => true,
                    _ => {
                        return Err(SwapError::InvalidFormat(format!(
                            "invalid modified flag `{value}`"
                        )));
                    }
                });
            }
            "text-bytes" => text_bytes = Some(parse_usize(value, "text-bytes")?),
            "text-checksum" => text_checksum = Some(parse_hex(value, "text-checksum")?),
            other => {
                return Err(SwapError::InvalidFormat(format!("unknown field `{other}`")));
            }
        }
    }

    let text_bytes = text_bytes.ok_or_else(|| missing("text-bytes"))?;
    let text_checksum = text_checksum.ok_or_else(|| missing("text-checksum"))?;
    let body = &bytes[split + 2..];
    if body.len() != text_bytes {
        return Err(SwapError::Truncated);
    }
    if checksum(body) != text_checksum {
        return Err(SwapError::ChecksumMismatch);
    }
    let text = String::from_utf8(body.to_vec())?;

    Ok(SwapSnapshot {
        kind: kind.unwrap_or(SwapKind::Named),
        path_hash: path_hash.ok_or_else(|| missing("path-hash"))?,
        pid: pid.ok_or_else(|| missing("pid"))?,
        host: host.ok_or_else(|| missing("host"))?,
        started_at: started_at.ok_or_else(|| missing("started"))?,
        cursor: LineCol {
            line: LineNumber::new(cursor_line.ok_or_else(|| missing("cursor-line"))?),
            col: ByteCol::new(cursor_col.ok_or_else(|| missing("cursor-col"))?),
        },
        modified: modified.ok_or_else(|| missing("modified"))?,
        text,
    })
}

fn missing(field: &str) -> SwapError {
    SwapError::InvalidFormat(format!("missing field `{field}`"))
}

fn parse_usize(value: &str, field: &str) -> Result<usize, SwapError> {
    value
        .parse::<usize>()
        .map_err(|_| SwapError::InvalidFormat(format!("invalid {field} `{value}`")))
}

fn parse_hex(value: &str, field: &str) -> Result<u64, SwapError> {
    u64::from_str_radix(value.trim_start_matches("0x"), 16)
        .map_err(|_| SwapError::InvalidFormat(format!("invalid {field} `{value}`")))
}

/// Returns the stable FNV-1a hash of a byte slice.
fn checksum(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Hashes the canonical path for the swap name and header.
///
/// `as_encoded_bytes` preserves the platform's path encoding; hashing is not a
/// display conversion, so an undecodable path must not collapse to a lossy
/// replacement string.
fn hash_path(path: &Path) -> u64 {
    checksum(path.as_os_str().as_encoded_bytes())
}

/// Returns the canonical identity of `path`, including a not-yet-created file.
///
/// Canonicalizing the parent keeps a relative path and its absolute spelling
/// on the same swap file; falling back to the supplied path keeps the editor
/// usable when the parent cannot be resolved.
pub(crate) fn canonical_identity(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if let (Ok(parent), Some(name)) = (parent.canonicalize(), path.file_name()) {
        return parent.join(name);
    }
    path.to_path_buf()
}

/// Builds `.name.vanta-<hash>.swp` next to the target.
fn swap_path(original: &Path, hash: u64) -> Option<PathBuf> {
    let name = original.file_name()?;
    let parent = original.parent().unwrap_or_else(|| Path::new("."));
    let mut swap_name = std::ffi::OsString::from(".");
    swap_name.push(name);
    swap_name.push(format!(".vanta-{hash:016x}.swp"));
    Some(parent.join(swap_name))
}

/// Returns the hash encoded in an untitled swap's file name.
fn swap_name_hash(path: &Path) -> Option<u64> {
    let name = path.file_name()?.to_str()?;
    let hex = name.strip_prefix(".untitled-")?.strip_suffix(".swp")?;
    u64::from_str_radix(hex, 16).ok()
}

/// Writes one swap version to `path` and restricts its permissions.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), SwapError> {
    vanta_fs::save_atomic(path, bytes).map_err(|source| SwapError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    restrict_permissions(path)
}

/// Removes one swap path, treating a missing file as already removed.
fn remove_path(path: &Path) -> Result<(), SwapError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(SwapError::Remove {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Returns the state-directory root, honoring `VANTA_STATE_DIR`.
pub(crate) fn state_root() -> Option<PathBuf> {
    if let Some(directory) = env::var_os("VANTA_STATE_DIR") {
        return Some(PathBuf::from(directory));
    }
    state_root_default()
}

/// Returns the platform default state directory from spec §7.7.
#[cfg(windows)]
fn state_root_default() -> Option<PathBuf> {
    env::var_os("LOCALAPPDATA").map(|base| PathBuf::from(base).join("vanta").join("state"))
}

/// Returns the XDG state directory, matching Neovim's macOS convention.
#[cfg(unix)]
fn state_root_default() -> Option<PathBuf> {
    if let Some(directory) = env::var_os("XDG_STATE_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(directory).join("vanta"));
    }
    env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("vanta")
    })
}

/// Unknown platforms have no documented state directory.
#[cfg(not(any(unix, windows)))]
fn state_root_default() -> Option<PathBuf> {
    None
}

/// Returns `<state>/swap` without creating it.
fn state_swap_path() -> Option<PathBuf> {
    state_root().map(|root| root.join("swap"))
}

/// Creates and restricts the swap directory, returning it when usable.
fn swap_directory() -> Option<PathBuf> {
    let directory = state_swap_path()?;
    prepare_directory(&directory).then_some(directory)
}

/// Creates `directory` with owner-only permissions on Unix.
fn prepare_directory(directory: &Path) -> bool {
    if fs::create_dir_all(directory).is_err() {
        return false;
    }
    restrict_directory(directory).is_ok()
}

/// Creates the fallback's parent directory before writing into it.
fn prepare_swap_directory(path: &Path) -> bool {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => prepare_directory(parent),
        _ => false,
    }
}

/// Every untitled swap this process may recover, newest first.
///
/// A swap is offered when it is modified, has no live writer and validates
/// against its own name. Unreadable or foreign files are skipped rather than
/// reported: the state directory is shared by every Vanta session.
pub(crate) fn recoverable_untitled() -> io::Result<Vec<(SwapFile, SwapSnapshot)>> {
    match state_swap_path() {
        Some(directory) => recoverable_untitled_in(&directory),
        None => Ok(Vec::new()),
    }
}

/// Scans one swap directory for recoverable untitled swaps.
fn recoverable_untitled_in(directory: &Path) -> io::Result<Vec<(SwapFile, SwapSnapshot)>> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(source),
    };

    let mut swaps = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(".untitled-") || !name.ends_with(".swp") {
            continue;
        }
        let Ok(swap) = SwapFile::adopt_untitled(entry.path()) else {
            continue;
        };
        let Ok(snapshot) = swap.read() else {
            continue;
        };
        if !snapshot.modified || swap.stale_reason(&snapshot).is_none() {
            continue;
        }
        swaps.push((swap, snapshot));
    }
    swaps.sort_by(|(_, left), (_, right)| right.started_at.cmp(&left.started_at));
    Ok(swaps)
}

/// Returns the current hostname with field-delimiter characters replaced.
fn current_host() -> String {
    let raw = env::var("COMPUTERNAME")
        .or_else(|_| env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".to_string());
    let host: String = raw
        .chars()
        .map(|character| {
            if character.is_ascii_graphic() && character != '=' {
                character
            } else {
                '_'
            }
        })
        .collect();
    if host.is_empty() {
        "unknown".to_string()
    } else {
        host
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Returns whether `pid` currently names a running process.
///
/// A best-effort false negative is preferable to claiming a live process is
/// stale; if the platform cannot answer, the swap is treated as possibly live.
#[cfg(target_os = "linux")]
fn pid_is_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

/// Returns whether `pid` currently names a running process.
#[cfg(all(unix, not(target_os = "linux")))]
fn pid_is_alive(pid: u32) -> bool {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pid="])
        .output();
    match output {
        Ok(output) if output.status.success() => !output.stdout.is_empty(),
        Ok(_) => false,
        Err(_) => true,
    }
}

/// Returns whether `pid` currently names a running process.
#[cfg(windows)]
fn pid_is_alive(pid: u32) -> bool {
    let filter = format!("PID eq {pid}");
    let output = Command::new("tasklist")
        .args(["/FI", &filter, "/NH", "/FO", "CSV"])
        .output();
    let output = match output {
        Ok(output) if output.status.success() => output,
        _ => return true,
    };
    let expected = pid.to_string();
    String::from_utf8_lossy(&output.stdout)
        .split(',')
        .any(|field| field.trim().trim_matches('"') == expected)
}

/// Unknown platforms cannot check liveness without a platform API.
#[cfg(not(any(unix, windows)))]
fn pid_is_alive(_pid: u32) -> bool {
    true
}

/// Restricts the swap directory to its owner on Unix.
#[cfg(unix)]
fn restrict_directory(path: &Path) -> Result<(), io::Error> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

/// Windows has no Unix mode to restrict; the default ACL applies.
#[cfg(not(unix))]
fn restrict_directory(_path: &Path) -> Result<(), io::Error> {
    Ok(())
}

/// Restricts a swap file to its owner on Unix.
#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<(), SwapError> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| {
        SwapError::Permissions {
            path: path.to_path_buf(),
            source,
        }
    })
}

/// Windows has no Unix mode to restrict; the default ACL applies.
#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<(), SwapError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn scratch(label: &str) -> PathBuf {
        let path = env::temp_dir().join(format!(
            "vanta-swap-{}-{}-{label}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("create scratch directory");
        path
    }

    fn sample_swap() -> (PathBuf, PathBuf, SwapFile) {
        let directory = scratch("sample");
        let target = directory.join("note.txt");
        fs::write(&target, "seed").expect("seed target");
        let swap = SwapFile::new(&target).expect("swap identity");
        (directory, target, swap)
    }

    #[test]
    fn swap_name_stays_next_to_the_target_and_encodes_the_hash() {
        let (directory, target, swap) = sample_swap();
        let canonical_target = target.canonicalize().expect("canonical target");
        assert_eq!(swap.path().parent(), canonical_target.parent());
        let name = swap
            .path()
            .file_name()
            .expect("swap file name")
            .to_string_lossy();
        assert!(name.starts_with(".note.txt.vanta-"));
        assert!(name.ends_with(".swp"));
        assert!(name.contains(&format!("{:016x}", swap.hash())));

        let relative = Path::new("note.txt");
        let absolute = env::current_dir()
            .expect("current directory")
            .join(relative);
        assert_eq!(
            SwapFile::new(relative).expect("relative identity").hash(),
            SwapFile::new(&absolute).expect("absolute identity").hash(),
            "relative and absolute paths must resolve to the same swap"
        );
        fs::remove_dir_all(directory).expect("clean scratch directory");
    }

    #[test]
    fn untitled_swaps_use_the_state_directory_and_the_header_marks_them() {
        let directory = scratch("untitled");
        let swap_directory = directory.join("swap");
        fs::create_dir_all(&swap_directory).expect("create swap directory");
        let mut swap = SwapFile::untitled_in(swap_directory.clone());
        assert_eq!(swap.path().parent(), Some(swap_directory.as_path()));
        let name = swap
            .path()
            .file_name()
            .expect("untitled swap name")
            .to_string_lossy()
            .into_owned();
        assert!(name.starts_with(".untitled-") && name.ends_with(".swp"));

        swap.write("draft", LineCol::default(), true)
            .expect("write untitled swap");
        let snapshot = swap.read().expect("read untitled swap");
        assert_eq!(snapshot.kind, SwapKind::Untitled);
        assert_eq!(snapshot.text, "draft");

        // This test process is the live writer, so make the recorded PID stale
        // the way a killed session would be before scanning.
        let text = fs::read_to_string(swap.path()).expect("read raw untitled swap");
        let current = format!("pid={}", std::process::id());
        assert!(text.contains(&current));
        fs::write(swap.path(), text.replace(&current, "pid=999999"))
            .expect("mark untitled swap stale");

        let found = recoverable_untitled_in(&swap_directory).expect("scan state swaps");
        assert_eq!(found.len(), 1, "the modified untitled swap must be offered");
        assert_eq!(found[0].1.text, "draft");
        swap.remove().expect("remove untitled swap");
        assert!(!swap.path().exists());
        fs::remove_dir_all(directory).expect("clean scratch directory");
    }

    #[test]
    fn an_unmodified_untitled_swap_is_not_offered_for_recovery() {
        let directory = scratch("untitled-clean");
        let swap_directory = directory.join("swap");
        fs::create_dir_all(&swap_directory).expect("create swap directory");
        let mut swap = SwapFile::untitled_in(swap_directory.clone());
        swap.write("", LineCol::default(), false)
            .expect("write clean untitled swap");
        let found = recoverable_untitled_in(&swap_directory).expect("scan state swaps");
        assert!(found.is_empty(), "a clean swap must not be recovered");
        swap.remove().expect("remove untitled swap");
        fs::remove_dir_all(directory).expect("clean scratch directory");
    }

    #[test]
    fn write_and_read_round_trip_text_cursor_and_flags() {
        let (directory, _target, mut swap) = sample_swap();
        let cursor = LineCol {
            line: LineNumber::new(1),
            col: ByteCol::new(2),
        };
        swap.write("one\r\ntwo", cursor, true).expect("write swap");

        let snapshot = swap.read().expect("read swap");
        assert_eq!(snapshot.text, "one\r\ntwo");
        assert_eq!(snapshot.cursor, cursor);
        assert!(snapshot.modified);
        assert_eq!(snapshot.path_hash, swap.hash());
        assert_eq!(snapshot.pid, std::process::id());

        swap.remove().expect("remove swap");
        assert!(!swap.path().exists());
        swap.remove().expect("removing a missing swap is harmless");
        fs::remove_dir_all(directory).expect("clean scratch directory");
    }

    #[cfg(unix)]
    #[test]
    fn swap_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let (directory, _target, mut swap) = sample_swap();
        swap.write("secret", LineCol::default(), true)
            .expect("write swap");
        let mode = fs::metadata(swap.path())
            .expect("swap metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        fs::remove_dir_all(directory).expect("clean scratch directory");
    }

    #[test]
    fn truncated_and_corrupt_text_is_rejected() {
        let (directory, _target, mut swap) = sample_swap();
        swap.write("hello", LineCol::default(), true)
            .expect("write swap");
        let bytes = fs::read(swap.path()).expect("read raw swap");
        let mut truncated = bytes.clone();
        truncated.pop();
        assert!(matches!(parse(&truncated), Err(SwapError::Truncated)));

        let mut corrupt = bytes;
        *corrupt.last_mut().expect("swap has text") = b'!';
        assert!(matches!(parse(&corrupt), Err(SwapError::ChecksumMismatch)));
        fs::remove_dir_all(directory).expect("clean scratch directory");
    }

    #[test]
    fn stale_detection_reports_dead_pids_and_foreign_hosts() {
        let (directory, _target, mut swap) = sample_swap();
        swap.write("text", LineCol::default(), true)
            .expect("write swap");
        let mut snapshot = swap.read().expect("read swap");
        assert_eq!(snapshot.kind, SwapKind::Named);

        snapshot.pid = 999_999;
        assert!(
            swap.stale_reason(&snapshot)
                .is_some_and(|reason| reason.contains("no longer running")),
            "a dead PID must be stale"
        );

        snapshot.host = "another-host".to_string();
        assert!(
            swap.stale_reason(&snapshot)
                .is_some_and(|reason| reason.contains("another-host")),
            "a foreign host must be stale"
        );
        fs::remove_dir_all(directory).expect("clean scratch directory");
    }
}
