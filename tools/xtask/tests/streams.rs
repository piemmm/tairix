//! xtask writing to a stream no one reads: an error or a lost line, never a
//! panic that ends it early.

#[path = "support/xtask.rs"]
mod support;

use std::net::Shutdown;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::process::{Command, Output, Stdio};

use support::timeout_env::TIMEOUT_ENV_VAR;
use support::xtask;
use tairix_fuzzseed::scratch::Scratch;

/// A stream whose reader has shut its end, so every write to it fails as one
/// to a closed pipe does, whatever copies of either end a concurrent spawn
/// holds.
fn closed_pipe() -> Stdio {
    let (reader, writer) = UnixStream::pair().expect("a socket pair");
    reader
        .shutdown(Shutdown::Read)
        .expect("its reading end shut");
    Stdio::from(OwnedFd::from(writer))
}

/// `cmd` run with its stdout no one reads and its stderr captured.
fn shown_nothing(mut cmd: Command) -> Output {
    cmd.stdout(closed_pipe())
        .stderr(Stdio::piped())
        .output()
        .expect("xtask runs")
}

#[test]
fn xtask_starts_without_the_operators_budget_override() {
    assert!(xtask(&[])
        .get_envs()
        .any(|(key, value)| key == TIMEOUT_ENV_VAR && value.is_none()));
}

#[test]
fn output_to_a_closed_pipe_fails_the_command_and_says_why() {
    for args in [&["help"][..], &["ci-long", "--dry-run"]] {
        let run = shown_nothing(xtask(args));
        assert_eq!(run.status.code(), Some(1), "{args:?}: {run:?}");
        assert!(
            String::from_utf8_lossy(&run.stderr).contains("cannot write to stdout"),
            "{args:?}: {run:?}"
        );
    }
}

#[test]
fn a_diagnostic_to_a_closed_pipe_is_lost_and_xtask_goes_on() {
    let run = xtask(&[])
        .stdout(Stdio::null())
        .stderr(closed_pipe())
        .output()
        .expect("xtask runs");
    assert_eq!(run.status.code(), Some(2), "its usage refusal: {run:?}");
}

#[test]
fn a_key_set_made_is_reported_made_whatever_stdout_takes() {
    // Outside the source tree, where a key set is refused.
    let home = Scratch::under(&std::env::temp_dir(), "xtask-streams-keys");
    let keys = home.path().join("signing-keys");
    let init = || {
        let mut cmd = xtask(&["keys", "init"]);
        cmd.env("TAIRIX_SIGNING_KEYS", &keys);
        shown_nothing(cmd)
    };
    let made = init();
    let kept = init();
    assert_eq!(made.status.code(), Some(0), "{made:?}");
    assert!(
        String::from_utf8_lossy(&made.stderr).contains("back it up"),
        "{made:?}"
    );
    assert_eq!(kept.status.code(), Some(0), "{kept:?}");
    assert!(
        String::from_utf8_lossy(&kept.stderr).contains("kept the signing key set"),
        "{kept:?}"
    );
}
