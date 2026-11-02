//! Developer task runner for the Vanta workspace.
//!
//! Run tasks as `cargo xtask <command>`; the alias lives in
//! `.cargo/config.toml`. Commands mirror `AGENTS.md` §5.4 so that a local run
//! and CI execute the same checks.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const HELP: &str = "\
Vanta developer tasks

Usage: cargo xtask <command>

Commands:
  check   Format check, Clippy with warnings denied, and the workspace tests
  test    Run the workspace test suite
  perf    Generate a 1 GiB fixture and measure the G1 large-file budgets
  fmt     Format the workspace
  help    Show this message
";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        eprintln!("error: a task is required\n\n{HELP}");
        return ExitCode::FAILURE;
    };
    if args.next().is_some() {
        eprintln!("error: `cargo xtask` takes exactly one task\n\n{HELP}");
        return ExitCode::FAILURE;
    }

    match command.as_str() {
        "check" => run_check(),
        "test" => finish(run(&["test", "--workspace"])),
        "perf" => run_perf(),
        "fmt" => finish(run(&["fmt", "--all"])),
        "help" | "-h" | "--help" => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("error: unknown task `{other}`\n\n{HELP}");
            ExitCode::FAILURE
        }
    }
}

/// Runs the checks from AGENTS §5.4 in order, stopping at the first failure.
fn run_check() -> ExitCode {
    let steps: &[&[&str]] = &[
        &["fmt", "--all", "--check"],
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ],
        &["test", "--workspace"],
    ];

    for step in steps {
        if let Err(code) = run(step) {
            return code;
        }
    }
    ExitCode::SUCCESS
}

/// Builds the release binary, then runs the ignored 1 GiB G1 harness.
///
/// The harness generates its fixture at run time. It is deliberately outside
/// `check`: a 1 GiB write, open and edit run is release evidence, not a fast
/// edit loop check.
fn run_perf() -> ExitCode {
    if let Err(code) = run(&["build", "--release", "-p", "vanta"]) {
        return code;
    }
    finish(run(&[
        "test",
        "--release",
        "-p",
        "vanta",
        "--test",
        "perf_g1",
        "--",
        "--ignored",
        "--nocapture",
    ]))
}

/// Runs one Cargo command in the workspace root.
fn run(args: &[&str]) -> Result<(), ExitCode> {
    let root = workspace_root();
    eprintln!("$ cargo {}", args.join(" "));
    let status = Command::new("cargo")
        .args(args)
        .current_dir(&root)
        .status()
        .map_err(|error| {
            eprintln!("error: failed to start cargo: {error}");
            ExitCode::FAILURE
        })?;

    if status.success() {
        return Ok(());
    }
    Err(ExitCode::from(
        u8::try_from(status.code().unwrap_or(1)).unwrap_or(1),
    ))
}

fn finish(result: Result<(), ExitCode>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => code,
    }
}

/// Returns the workspace root, which is the parent of this crate.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask is a workspace member and therefore has a parent directory")
        .to_path_buf()
}
