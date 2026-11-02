//! Noninteractive command-line behavior of the `vanta` binary.

use std::process::Command;

#[test]
fn help_and_version_print_without_entering_the_editor() {
    for (argument, expected) in [
        ("--help", "usage: vanta [file]"),
        ("-h", "usage: vanta [file]"),
        ("--version", env!("CARGO_PKG_VERSION")),
        ("-V", env!("CARGO_PKG_VERSION")),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_vanta"))
            .arg(argument)
            .output()
            .expect("run the vanta binary");

        assert!(
            output.status.success(),
            "{argument} must exit successfully, stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(expected),
            "{argument} output must contain {expected:?}, got {stdout:?}"
        );
        assert!(
            !stdout.contains("\u{1b}[?1049h"),
            "{argument} must not enter the alternate screen"
        );
    }
}
