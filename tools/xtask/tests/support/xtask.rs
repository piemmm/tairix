//! Running the xtask binary from its integration tests.

#[path = "../../src/timeout_env.rs"]
pub(crate) mod timeout_env;

use std::process::{Command, Stdio};

/// The xtask binary run with `args`, its stdin closed and without the
/// operator's budget override, as a malformed one stops every command but
/// `help`.
pub(crate) fn xtask(args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_xtask"));
    cmd.args(args)
        .stdin(Stdio::null())
        .env_remove(timeout_env::TIMEOUT_ENV_VAR);
    cmd
}
