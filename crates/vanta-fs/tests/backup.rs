//! Real-filesystem regressions for PROJECT_SPEC BUF-005.
//!
//! The tests exercise the backup transaction rather than the lower-level
//! atomic replacement: a transient `writebackup`, a retained `backup`,
//! `backupdir`/`backupext`, a failed backup, a read-only target and the
//! read-only backup-directory acceptance case.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use vanta_fs::{BackupOptions, SaveError, save_with_backup};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn scratch(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "vanta-backup-{}-{}-{label}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).expect("create scratch directory");
    path
}

#[test]
fn default_writebackup_is_removed_after_a_successful_save() {
    let directory = scratch("transient");
    let target = directory.join("note.txt");
    fs::write(&target, b"old contents").expect("seed target");

    let outcome = save_with_backup(&target, b"new contents", &BackupOptions::default())
        .expect("save with transient backup");

    assert_eq!(fs::read(&target).expect("read target"), b"new contents");
    assert_eq!(outcome, vanta_fs::SaveOutcome::default());
    assert!(
        !target.with_file_name("note.txt~").exists(),
        "Vim's default writebackup must not leave a backup after success"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn backup_option_keeps_the_previous_bytes() {
    let directory = scratch("kept");
    let target = directory.join("note.txt");
    fs::write(&target, b"old contents").expect("seed target");

    let options = BackupOptions {
        backup: true,
        ..BackupOptions::default()
    };
    let outcome = save_with_backup(&target, b"new contents", &options).expect("save with backup");

    assert_eq!(fs::read(&target).expect("read target"), b"new contents");
    let backup = target.with_file_name("note.txt~");
    assert_eq!(
        fs::read(&backup).expect("read kept backup"),
        b"old contents",
        "backup on must retain the previous target bytes"
    );
    assert_eq!(outcome.backup.as_deref(), Some(backup.as_path()));
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn backupdir_and_backupext_are_honored() {
    let directory = scratch("options");
    let backup_dir = directory.join("backups");
    fs::create_dir(&backup_dir).expect("create backup directory");
    let target = directory.join("note.txt");
    fs::write(&target, b"old contents").expect("seed target");

    let options = BackupOptions {
        backup: true,
        backup_dirs: vec![backup_dir.clone()],
        extension: ".bak".to_string(),
        ..BackupOptions::default()
    };
    let outcome = save_with_backup(&target, b"new contents", &options).expect("save with options");

    assert_eq!(fs::read(&target).expect("read target"), b"new contents");
    let backup = backup_dir.join("note.txt.bak");
    assert_eq!(
        fs::read(&backup).expect("read configured backup"),
        b"old contents"
    );
    assert!(!target.with_file_name("note.txt~").exists());
    assert_eq!(outcome.backup.as_deref(), Some(backup.as_path()));
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn failed_backup_aborts_before_the_target_is_touched() {
    let directory = scratch("backup-failure");
    let target = directory.join("note.txt");
    fs::write(&target, b"old contents").expect("seed target");
    let blocker = directory.join("not-a-directory");
    fs::write(&blocker, b"blocker").expect("create blocker file");

    let options = BackupOptions {
        backup: true,
        backup_dirs: vec![blocker],
        ..BackupOptions::default()
    };
    let error = save_with_backup(&target, b"new contents", &options)
        .expect_err("an unusable backup directory must abort");

    assert!(
        matches!(error, SaveError::BackupWrite { .. }),
        "unexpected error: {error}"
    );
    assert_eq!(
        fs::read(&target).expect("read untouched target"),
        b"old contents",
        "a failed backup must leave the original intact"
    );
    assert!(!target.with_file_name("note.txt~").exists());
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn read_only_target_fails_and_leaves_a_backup() {
    let directory = scratch("read-only-target");
    let target = directory.join("note.txt");
    fs::write(&target, b"old contents").expect("seed target");
    let original_permissions = fs::metadata(&target)
        .expect("target metadata")
        .permissions();
    let mut read_only = original_permissions.clone();
    read_only.set_readonly(true);
    fs::set_permissions(&target, read_only).expect("mark target read-only");

    let error = save_with_backup(&target, b"new contents", &BackupOptions::default())
        .expect_err("a read-only target must fail");

    assert!(
        matches!(error, SaveError::ReadOnly { .. }),
        "unexpected error: {error}"
    );
    assert_eq!(
        fs::read(&target).expect("read read-only target"),
        b"old contents",
        "the read-only target must be byte-for-byte intact"
    );
    assert_eq!(
        fs::read(target.with_file_name("note.txt~")).expect("read aborted backup"),
        b"old contents",
        "a failed write must leave a usable previous-contents backup"
    );

    fs::set_permissions(&target, original_permissions).expect("restore target permissions");
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn disabling_both_backup_modes_leaves_no_backup() {
    let directory = scratch("disabled");
    let target = directory.join("note.txt");
    fs::write(&target, b"old contents").expect("seed target");

    let options = BackupOptions {
        backup: false,
        write_backup: false,
        ..BackupOptions::default()
    };
    let outcome =
        save_with_backup(&target, b"new contents", &options).expect("save without backup");

    assert_eq!(fs::read(&target).expect("read target"), b"new contents");
    assert_eq!(outcome, vanta_fs::SaveOutcome::default());
    assert!(!target.with_file_name("note.txt~").exists());
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[cfg(unix)]
#[test]
fn read_only_backup_directory_aborts_the_save() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = scratch("read-only-backup-directory");
    let backup_dir = directory.join("backups");
    fs::create_dir(&backup_dir).expect("create backup directory");
    let target = directory.join("note.txt");
    fs::write(&target, b"old contents").expect("seed target");
    fs::set_permissions(&backup_dir, fs::Permissions::from_mode(0o500))
        .expect("mark backup directory read-only");

    let options = BackupOptions {
        backup: true,
        write_backup: false,
        backup_dirs: vec![backup_dir.clone()],
        ..BackupOptions::default()
    };
    let error = save_with_backup(&target, b"new contents", &options)
        .expect_err("an unwritable backup directory must abort");

    assert!(
        matches!(error, SaveError::BackupWrite { .. }),
        "unexpected error: {error}"
    );
    assert_eq!(
        fs::read(&target).expect("read untouched target"),
        b"old contents"
    );
    assert!(!backup_dir.join("note.txt~").exists());

    fs::set_permissions(&backup_dir, fs::Permissions::from_mode(0o700))
        .expect("restore backup directory permissions");
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}
