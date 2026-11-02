//! Filesystem operations for Vanta.
//!
//! This crate owns the file lifecycle that must not lose user data. The first
//! operation is [`save_atomic`], which implements the atomic replacement
//! guarantee from `PROJECT_SPEC` BUF-002:
//!
//! * Contents are written to a new temporary file in the target's directory
//!   with `0600` permissions, flushed and fsynced.
//! * The target is replaced by renaming the temporary file over it, never by
//!   opening or truncating the target.
//! * A failure at any step leaves the previous target contents in place and
//!   removes the temporary file where possible.
//! * On Unix the parent directory is fsynced after the rename so the
//!   replacement survives a crash.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

mod backup;
pub use backup::{BackupOptions, SaveOutcome, save_with_backup};

mod encoding;
pub use encoding::{DecodedText, EncodeError, TextEncoding, decode_file, encode_text};

/// A temporary file sequence shared by all saves in this process.
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A failed atomic save.
///
/// Every variant names the target path and, where one exists, the underlying
/// I/O error. Callers can report the error to the user and leave the buffer
/// modified.
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    /// The target path has no final component to name a temporary file after.
    #[error("cannot save to {}: the path has no file name", path.display())]
    MissingFileName {
        /// The target path.
        path: PathBuf,
    },

    /// The temporary file could not be created in the target directory.
    #[error("cannot create a temporary file for {}: {source}", path.display())]
    CreateTemp {
        /// The target path.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// Writing, flushing or fsyncing the temporary file failed.
    #[error("cannot write the temporary file for {}: {source}", path.display())]
    WriteTemp {
        /// The target path.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// The target's existing permissions could not be applied to the temporary
    /// file.
    #[error("cannot preserve the permissions of {}: {source}", path.display())]
    PreservePermissions {
        /// The target path.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// The temporary file could not be renamed over the target.
    #[error("cannot replace {}: {source}", path.display())]
    Replace {
        /// The target path.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// The containing directory could not be fsynced after the replacement.
    #[error("cannot sync the directory {}: {source}", path.display())]
    SyncDirectory {
        /// The containing directory.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// The target exists but is read-only.
    #[error("cannot write read-only file {}", path.display())]
    ReadOnly {
        /// The target path.
        path: PathBuf,
    },

    /// The target could not be opened for writing even though it is not
    /// marked read-only, for example because of an ACL or sharing denial.
    #[error("cannot write {}: {source}", path.display())]
    Access {
        /// The target path.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// The target's metadata could not be inspected before the save.
    #[error("cannot inspect {}: {source}", path.display())]
    Metadata {
        /// The target path.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// The previous contents could not be read for a backup.
    #[error("cannot read {} for backup: {source}", path.display())]
    BackupRead {
        /// The target path.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// A backup file could not be written.
    #[error("cannot write backup {}: {source}", path.display())]
    BackupWrite {
        /// The backup path.
        path: PathBuf,
        /// The atomic save failure for the backup.
        #[source]
        source: Box<SaveError>,
    },

    /// The configured extension is empty or contains a path separator.
    #[error("invalid backup extension `{extension}`")]
    InvalidBackupExtension {
        /// The rejected extension.
        extension: String,
    },

    /// The backup path would overwrite the target itself.
    #[error("backup path for {} is the target itself", path.display())]
    BackupIsTarget {
        /// The target path.
        path: PathBuf,
    },
}

/// Writes `contents` to `path` without ever truncating the existing file.
///
/// The write goes to a unique temporary file in the same directory, is fsynced,
/// and is then renamed over `path`. If any step fails, the previous contents of
/// `path` remain untouched and the temporary file is removed where possible.
///
/// # Errors
///
/// Returns [`SaveError`] when the path cannot be named, the temporary file
/// cannot be created or written, the target cannot be replaced, or the parent
/// directory cannot be fsynced on Unix.
pub fn save_atomic(path: &Path, contents: &[u8]) -> Result<(), SaveError> {
    let file_name = path.file_name().ok_or_else(|| SaveError::MissingFileName {
        path: path.to_path_buf(),
    })?;
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };

    let (temp_path, mut temp) = create_temp(parent, file_name, path)?;
    let written = temp
        .write_all(contents)
        .and_then(|()| temp.flush())
        .and_then(|()| temp.sync_all());
    // Close the handle before replacing the target; Windows refuses to rename
    // over a file that is still open in some sharing modes.
    drop(temp);

    if let Err(source) = written {
        remove_temp(&temp_path);
        return Err(SaveError::WriteTemp {
            path: path.to_path_buf(),
            source,
        });
    }

    if let Err(error) = preserve_permissions(path, &temp_path) {
        remove_temp(&temp_path);
        return Err(error);
    }

    if let Err(source) = replace(&temp_path, path) {
        remove_temp(&temp_path);
        return Err(SaveError::Replace {
            path: path.to_path_buf(),
            source,
        });
    }

    sync_parent(parent).map_err(|source| SaveError::SyncDirectory {
        path: parent.to_path_buf(),
        source,
    })
}

/// Creates a uniquely named temporary file in `parent` with `0600` permissions
/// on Unix.
fn create_temp(
    parent: &Path,
    file_name: &OsStr,
    target: &Path,
) -> Result<(PathBuf, File), SaveError> {
    for _ in 0..32 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut name = OsString::from(".");
        name.push(file_name);
        name.push(format!(".vanta-tmp-{}-{sequence}", process::id()));
        let temp_path = parent.join(name);

        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }

        match options.open(&temp_path) {
            Ok(file) => return Ok((temp_path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(SaveError::CreateTemp {
                    path: target.to_path_buf(),
                    source,
                });
            }
        }
    }

    Err(SaveError::CreateTemp {
        path: target.to_path_buf(),
        source: io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not create a unique temporary file",
        ),
    })
}

/// Applies the target's existing Unix mode to the temporary file.
#[cfg(unix)]
fn preserve_permissions(target: &Path, temp: &Path) -> Result<(), SaveError> {
    use std::os::unix::fs::PermissionsExt as _;

    match fs::metadata(target) {
        Ok(metadata) => {
            let mode = metadata.permissions().mode();
            fs::set_permissions(temp, fs::Permissions::from_mode(mode)).map_err(|source| {
                SaveError::PreservePermissions {
                    path: target.to_path_buf(),
                    source,
                }
            })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(SaveError::PreservePermissions {
            path: target.to_path_buf(),
            source,
        }),
    }
}

/// Windows has no meaningful mode to copy; the temporary file keeps its
/// restrictive creation permissions.
#[cfg(not(unix))]
fn preserve_permissions(_target: &Path, _temp: &Path) -> Result<(), SaveError> {
    Ok(())
}

/// Replaces `target` with `temp`.
///
/// Windows can fail with a sharing violation while another process holds the
/// target open, so the replace is retried briefly. Unix rename is atomic and
/// needs no retry.
#[cfg(windows)]
fn replace(temp: &Path, target: &Path) -> io::Result<()> {
    const ATTEMPTS: u32 = 5;
    let mut last_error = None;

    for attempt in 0..ATTEMPTS {
        match fs::rename(temp, target) {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_error = Some(error);
                std::thread::sleep(std::time::Duration::from_millis(
                    10 * u64::from(attempt + 1),
                ));
            }
        }
    }

    Err(last_error.expect("the retry loop always attempts at least one rename"))
}

/// Replaces `target` with `temp` with a single atomic rename.
#[cfg(not(windows))]
fn replace(temp: &Path, target: &Path) -> io::Result<()> {
    fs::rename(temp, target)
}

/// Fsyncs the containing directory so the rename is durable across a crash.
#[cfg(unix)]
fn sync_parent(parent: &Path) -> io::Result<()> {
    File::open(parent)?.sync_all()
}

/// Directory fsync is not available on this platform; the file contents were
/// still fsynced before the rename.
#[cfg(not(unix))]
fn sync_parent(_parent: &Path) -> io::Result<()> {
    Ok(())
}

/// Removes a temporary file after a failed save.
///
/// The target is untouched and the temporary name is unique, so a cleanup
/// failure must not mask the error that caused the save to fail.
fn remove_temp(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => {}
    }
}
