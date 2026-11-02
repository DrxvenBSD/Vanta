//! The Vanta editor binary.
//!
//! Usage: `vanta [file]` or `vanta -r [file]`. With no file it starts an
//! empty untitled buffer and the editor draws its startup hint. `-r` recovers
//! the newest retained untitled swap, or the swap of the named file, and
//! marks the recovered buffer modified so it is only written by an explicit
//! save. The terminal is restored on the normal path, on early errors, and by
//! the panic hook.

use std::ffi::OsStr;
use std::fmt;
use std::path::PathBuf;
use std::process::ExitCode;

use vanta::editor::Editor;
use vanta::terminal::{CrosstermTerminal, install_panic_hook};

const USAGE: &str = "usage: vanta [-r] [file]";
const HELP: &str = "\
Vanta - a modal terminal editor

usage: vanta [file]
       vanta -r [file]

With no file, Vanta opens an empty [No Name] buffer and shows a startup hint.
With -r, Vanta recovers the newest retained untitled swap, or the swap of the
named file, and marks the recovered buffer modified.

options:
  -h, --help     print this help
  -V, --version  print the version
  -r, --recover  recover a swap (untitled when no file is given)";

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let editor = match args.as_slice() {
        [] => Editor::untitled(),
        [argument] if is_flag(argument, "-h", "--help") => {
            println!("{HELP}");
            return ExitCode::SUCCESS;
        }
        [argument] if is_flag(argument, "-V", "--version") => {
            println!("vanta {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        [argument] if is_flag(argument, "-r", "--recover") => match Editor::recover_untitled() {
            Ok(editor) => editor,
            Err(error) => return fail(&error),
        },
        [flag, path] if is_flag(flag, "-r", "--recover") => {
            match Editor::recover(PathBuf::from(path)) {
                Ok(editor) => editor,
                Err(error) => return fail(&error),
            }
        }
        [path] => match Editor::open(PathBuf::from(path)) {
            Ok(editor) => editor,
            Err(error) => return fail(&error),
        },
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    run(editor)
}

/// Reports a startup error without entering the editor.
fn fail(error: &dyn fmt::Display) -> ExitCode {
    eprintln!("vanta: {error}");
    ExitCode::FAILURE
}

/// Returns whether `argument` is either spelling of a flag.
fn is_flag(argument: &OsStr, short: &str, long: &str) -> bool {
    argument == OsStr::new(short) || argument == OsStr::new(long)
}

/// Runs the editor loop and restores the terminal on every normal path.
fn run(mut editor: Editor) -> ExitCode {
    install_panic_hook();
    let mut terminal = CrosstermTerminal::new();
    if let Err(error) = terminal.enter() {
        eprintln!("vanta: cannot start the terminal: {error}");
        return ExitCode::FAILURE;
    }

    maybe_inject_panic();

    let run_result = editor.run(&mut terminal);
    let restore_result = terminal.restore();
    if let Err(error) = &restore_result {
        eprintln!("vanta: cannot restore the terminal: {error}");
    }

    match run_result {
        Ok(()) if restore_result.is_ok() => ExitCode::SUCCESS,
        Ok(()) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("vanta: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Panics after entering the alternate screen when the debug binary is given
/// `VANTA_TEST_PANIC_AFTER_INIT`.
///
/// This exists only in debug builds so the PTY test can prove that the panic
/// hook restores the terminal (`PROJECT_SPEC` OBS-004). Release builds compile
/// it out.
#[cfg(debug_assertions)]
fn maybe_inject_panic() {
    if std::env::var_os("VANTA_TEST_PANIC_AFTER_INIT").is_some() {
        panic!("injected panic for terminal-restoration testing");
    }
}

#[cfg(not(debug_assertions))]
fn maybe_inject_panic() {}
