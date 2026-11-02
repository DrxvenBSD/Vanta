//! Integration tests for PROJECT_SPEC §3D, BUF-002.
//!
//! These exercise the real filesystem: replacement of an existing file,
//! creation of a new one, cleanup of temporary files, and a failed replace
//! that must leave the target untouched.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use vanta_fs::SaveError;
use vanta_fs::save_atomic;

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn scratch(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "vanta-fs-{}-{}-{label}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).expect("create scratch directory");
    path
}

fn temporary_files(directory: &Path) -> Vec<PathBuf> {
    fs::read_dir(directory)
        .expect("read scratch directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains(".vanta-tmp-"))
        })
        .collect()
}

#[test]
fn replaces_an_existing_file_and_cleans_the_temporary() {
    let directory = scratch("replace");
    let target = directory.join("note.txt");
    fs::write(&target, b"old contents").expect("seed target");

    save_atomic(&target, b"new contents").expect("atomic save succeeds");

    assert_eq!(fs::read(&target).expect("read target"), b"new contents");
    assert!(
        temporary_files(&directory).is_empty(),
        "temporary files must be renamed, not left behind"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn creates_a_new_file() {
    let directory = scratch("create");
    let target = directory.join("new.txt");

    save_atomic(&target, b"created").expect("atomic save creates the file");

    assert_eq!(fs::read(&target).expect("read target"), b"created");
    assert!(temporary_files(&directory).is_empty());
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[test]
fn replace_failure_leaves_the_target_untouched() {
    let directory = scratch("failure");
    let target = directory.join("target");
    fs::create_dir(&target).expect("create target directory");
    fs::write(target.join("keep.txt"), b"keep").expect("seed target contents");

    let error = save_atomic(&target, b"replacement").expect_err("replacing a directory fails");

    assert!(
        matches!(error, SaveError::Replace { .. }),
        "unexpected error: {error}"
    );
    assert!(target.is_dir(), "the original target must survive");
    assert_eq!(
        fs::read(target.join("keep.txt")).expect("read original"),
        b"keep"
    );
    assert!(
        temporary_files(&directory).is_empty(),
        "a failed replace must remove its temporary file"
    );
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}

#[cfg(unix)]
#[test]
fn preserves_the_existing_unix_mode() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = scratch("mode");
    let target = directory.join("script.sh");
    fs::write(&target, b"#!/bin/sh\n").expect("seed target");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).expect("set mode");

    save_atomic(&target, b"#!/bin/sh\necho updated\n").expect("atomic save succeeds");

    let mode = fs::metadata(&target)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o640, "existing permissions must be preserved");
    fs::remove_dir_all(&directory).expect("clean scratch directory");
}
