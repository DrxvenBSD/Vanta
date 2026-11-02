//! Backup files and the write-abort transaction.
//!
//! `PROJECT_SPEC` BUF-005 preserves Vim's defaults: `backup` is off,
//! `writebackup` is on, `backupdir` locates the backup, and `backupext` is `~`.
//! A backup is written before the target, and a backup failure aborts the save
//! while the target is still intact. A transient `writebackup` is removed only
//! after the new target contents are durable; a `backup` file is kept.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use crate::{SaveError, save_atomic};

/// Options for the backup-aware save transaction.
///
/// The defaults match Vim: `backup` off, `writebackup` on, backup in the
/// target's directory, and the `~` extension. An empty `backup_dirs` list means
/// "the target's directory", not "no backup directory".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackupOptions {
    /// Keep a backup file after a successful write (`backup`).
    pub backup: bool,
    /// Create a transient backup before a write (`writebackup`).
    pub write_backup: bool,
    /// Directories tried in order for the backup file (`backupdir`).
    pub backup_dirs: Vec<PathBuf>,
    /// Suffix appended to the target file name (`backupext`).
    pub extension: String,
}

impl Default for BackupOptions {
    fn default() -> Self {
        Self {
            backup: false,
            write_backup: true,
            backup_dirs: Vec::new(),
            extension: "~".to_string(),
        }
    }
}

impl BackupOptions {
    /// Returns whether either backup mode asks for a backup before writing.
    pub fn should_backup(&self) -> bool {
        self.backup || self.write_backup
    }

    /// Returns whether a successful write keeps the backup file.
    pub fn keeps_backup(&self) -> bool {
        self.backup
    }
}

/// What happened to the backup after a successful write.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SaveOutcome {
    /// A backup left on disk after the save.
    pub backup: Option<PathBuf>,
    /// A non-fatal cleanup warning, set when a transient backup could not be
    /// removed after the new target was durable.
    pub warning: Option<String>,
}

/// Writes `contents` to `path` with the configured backup transaction.
///
/// The order is deliberate:
///
/// 1. If the target exists and either backup mode is on, copy the previous
///    bytes into a backup. A backup failure returns an error before the target
///    is touched.
/// 2. Reject a read-only or permission-denied target before replacing it.
/// 3. Replace the target with [`save_atomic`].
/// 4. Keep a `backup` file, or remove a transient `writebackup` only after the
///    replacement succeeded.
///
/// If step 3 fails, the backup is left in place for manual recovery. If a
/// transient backup cannot be removed in step 4, the save still succeeds and
/// [`SaveOutcome::warning`] explains the leftover file.
///
/// # Errors
///
/// Returns [`SaveError`] when the backup cannot be created, the target is
/// read-only, metadata cannot be inspected, or the atomic replacement fails.
pub fn save_with_backup(
    path: &Path,
    contents: &[u8],
    options: &BackupOptions,
) -> Result<SaveOutcome, SaveError> {
    validate_extension(&options.extension)?;

    let target_exists = match fs::metadata(path) {
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(source) => {
            return Err(SaveError::Metadata {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    let backup = if target_exists && options.should_backup() {
        let original = fs::read(path).map_err(|source| SaveError::BackupRead {
            path: path.to_path_buf(),
            source,
        })?;
        Some(write_backup(path, &original, options)?)
    } else {
        None
    };

    // This happens after the backup on purpose: a read-only target must still
    // leave the previous contents recoverable in the backup left by a failed
    // write attempt.
    ensure_target_writable(path)?;

    save_atomic(path, contents)?;

    let mut outcome = SaveOutcome::default();
    if let Some(backup) = backup {
        if options.keeps_backup() {
            outcome.backup = Some(backup);
        } else {
            match fs::remove_file(&backup) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    outcome.warning = Some(format!(
                        "cannot remove backup {}: {error}",
                        backup.display()
                    ));
                    outcome.backup = Some(backup);
                }
            }
        }
    }
    Ok(outcome)
}

/// Writes the previous contents to the first usable backup candidate.
fn write_backup(
    target: &Path,
    original: &[u8],
    options: &BackupOptions,
) -> Result<PathBuf, SaveError> {
    let candidates = backup_candidates(target, options)?;
    let mut last_error = None;

    for candidate in candidates {
        if same_file_path(&candidate, target) {
            last_error = Some(SaveError::BackupIsTarget {
                path: target.to_path_buf(),
            });
            continue;
        }
        match save_atomic(&candidate, original) {
            Ok(()) => return Ok(candidate),
            Err(source) => {
                last_error = Some(SaveError::BackupWrite {
                    path: candidate,
                    source: Box::new(source),
                });
            }
        }
    }

    Err(last_error.expect("backup_candidates always returns at least one path"))
}

/// Builds the backup paths in configured-directory order.
fn backup_candidates(target: &Path, options: &BackupOptions) -> Result<Vec<PathBuf>, SaveError> {
    let file_name = target
        .file_name()
        .ok_or_else(|| SaveError::MissingFileName {
            path: target.to_path_buf(),
        })?;
    let mut backup_name = OsString::from(file_name);
    backup_name.push(&options.extension);

    let directories = if options.backup_dirs.is_empty() {
        vec![
            target
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf(),
        ]
    } else {
        options.backup_dirs.clone()
    };

    Ok(directories
        .into_iter()
        .map(|directory| directory.join(&backup_name))
        .collect())
}

/// Rejects an empty extension or one that would escape the backup directory.
fn validate_extension(extension: &str) -> Result<(), SaveError> {
    if extension.is_empty() || extension.contains('/') || extension.contains('\\') {
        return Err(SaveError::InvalidBackupExtension {
            extension: extension.to_string(),
        });
    }
    Ok(())
}

/// Returns whether two paths name the same existing file.
fn same_file_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// Rejects a target that is read-only or cannot be opened for writing.
///
/// The open probe catches permission checks that `Permissions::readonly` does
/// not represent, such as an ACL denial. It does not truncate the target.
fn ensure_target_writable(path: &Path) -> Result<(), SaveError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(SaveError::Metadata {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    if metadata.permissions().readonly() {
        return Err(SaveError::ReadOnly {
            path: path.to_path_buf(),
        });
    }

    if metadata.is_file() {
        match OpenOptions::new().write(true).open(path) {
            Ok(file) => drop(file),
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                return Err(SaveError::ReadOnly {
                    path: path.to_path_buf(),
                });
            }
            Err(source) => {
                return Err(SaveError::Access {
                    path: path.to_path_buf(),
                    source,
                });
            }
        }
    }

    Ok(())
}
