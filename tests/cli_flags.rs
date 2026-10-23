//! `--version` and `--help`, tested by running the real binary.
//!
//! These exist for the *downloaded* binary (Phase 10, ADR-096): the first thing
//! anyone does with an unknown executable is ask it what it is, and until this
//! was added Sandesh answered by starting a mail server and trying to bind port
//! 8080. From a source checkout that hardly showed; from a release archive it is
//! the first impression.
//!
//! Driven as a subprocess rather than by calling `run_cli`, because the
//! behaviour under test is precisely what the process does with its argv — and
//! because the function blocks forever on every path that is not an early exit,
//! so a unit test that got the argument handling wrong would hang rather than
//! fail.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;
use std::process::Command;

/// The binary this test was built alongside.
///
/// This test lives in the workspace root package — which owns the `sandesh`
/// binary — precisely so that `CARGO_BIN_EXE_` is set. Cargo then *guarantees*
/// the binary is built before the test runs. The obvious alternative, walking
/// up from `current_exe()` to find `target/debug/sandesh`, silently tests a
/// stale binary when one is lying around and fails confusingly when none is:
/// both were observed while writing this.
fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sandesh"))
}

/// Runs the binary with one argument, returning (exit code, stdout, stderr).
///
/// The environment is scrubbed of `SANDESH_*` so a developer's own settings
/// cannot change the result — and so that a regression which *starts the
/// server* fails here rather than inheriting a usable configuration and
/// blocking the suite.
fn run(argument: &str) -> (Option<i32>, String, String) {
    let mut command = Command::new(binary());
    for (key, _) in std::env::vars() {
        if key.starts_with("SANDESH_") {
            command.env_remove(key);
        }
    }

    let output = command
        .arg(argument)
        .output()
        .expect("the sandesh binary should be runnable");

    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn version_flag_prints_the_version_and_exits() {
    for flag in ["--version", "-V"] {
        let (code, stdout, _) = run(flag);
        assert_eq!(code, Some(0), "{flag} should succeed");
        assert_eq!(
            stdout.trim(),
            format!("sandesh {}", env!("CARGO_PKG_VERSION")),
            "{flag} should print exactly the name and version"
        );
    }
}

#[test]
fn help_flag_describes_how_sandesh_is_configured() {
    for flag in ["--help", "-h"] {
        let (code, stdout, _) = run(flag);
        assert_eq!(code, Some(0), "{flag} should succeed");

        // The usage line, and the fact that configuration is environmental —
        // the one thing a user cannot guess from a binary that takes no flags.
        assert!(
            stdout.contains("Usage:"),
            "{flag} should print a usage line"
        );
        assert!(
            stdout.contains("SANDESH_DATA_DIR"),
            "{flag} should name the data directory variable"
        );
        assert!(
            stdout.contains("SANDESH_SMTP_ADDR"),
            "{flag} should name the SMTP listener variable"
        );
    }
}

#[test]
fn an_unrecognised_argument_fails_rather_than_starting_the_server() {
    // Silently ignoring an argument is the worse failure: someone who mistypes
    // a variable name as a flag would get a running server that is not
    // configured the way they asked, and no indication of it.
    let (code, stdout, stderr) = run("--nonsense");

    assert_eq!(code, Some(1), "an unknown argument should exit non-zero");
    assert!(
        stderr.contains("--nonsense"),
        "the error should name the offending argument, got: {stderr}"
    );
    assert!(
        !stdout.contains("SMTP"),
        "nothing should have started; stdout was: {stdout}"
    );
}

#[test]
fn a_bad_argument_is_reported_even_beside_a_good_one() {
    // Regression: the first version of this returned from inside the argument
    // loop, so `--version --nonsense` printed the version and exited 0 without
    // ever looking at the second argument. Clippy's `never_loop` is what
    // surfaced it — the lint was pointing at a real defect, not a style nit.
    let mut command = Command::new(binary());
    for (key, _) in std::env::vars() {
        if key.starts_with("SANDESH_") {
            command.env_remove(key);
        }
    }

    let output = command
        .args(["--version", "--nonsense"])
        .output()
        .expect("the sandesh binary should be runnable");

    assert_eq!(
        output.status.code(),
        Some(1),
        "a bad argument must fail even when a valid one precedes it"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--nonsense"),
        "the error should name the offending argument"
    );
}

#[test]
fn the_early_exit_paths_touch_no_state() {
    // `--version` must not create a database, a blob directory or a listener.
    // This is what makes it safe to run against an installed binary to find out
    // what it is — including as root, or in a directory the user did not intend
    // to fill with mail.
    let scratch = std::env::temp_dir().join(format!("sandesh-cli-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("scratch directory");

    let output = Command::new(binary())
        .arg("--version")
        .current_dir(&scratch)
        .output()
        .expect("the sandesh binary should be runnable");
    assert_eq!(output.status.code(), Some(0));

    let leftovers: Vec<_> = std::fs::read_dir(&scratch)
        .expect("scratch is readable")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();

    let _ = std::fs::remove_dir_all(&scratch);

    assert!(
        leftovers.is_empty(),
        "--version must not create anything, found: {leftovers:?}"
    );
}
