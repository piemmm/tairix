//! The `gpucheck --present` vertical's shared contract (`plans/GPU.md`
//! GP6.5).
//!
//! The freestanding guest kernel (`src/main.rs`) and the host runner's
//! enrolment (`tools/xtask/src/commands/qemu_tests.rs`) both read these
//! definitions, so what the host types and what the guest latches cannot
//! drift apart.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

// Macros rather than consts so `COMMAND_LINE` can be composed by `concat!`.
macro_rules! check_command {
    () => {
        "gpucheck"
    };
}

macro_rules! then_command {
    () => {
        "true"
    };
}

/// Bare name of the command under test, in the system command store.
pub const CHECK_COMMAND: &str = check_command!();

/// Bare name of the command the shell runs once the check has left.
///
/// [`COMMAND_LINE`] joins the two with `&&`, so the shell loads it only after
/// the check exited with status zero — every frame it presented was shown —
/// and its load is the guest's last witness.
pub const THEN_COMMAND: &str = then_command!();

/// The line typed at the terminal's shell.
pub const COMMAND_LINE: &str = concat!(check_command!(), " --present && ", then_command!(), "\n");

#[cfg(test)]
mod tests {
    use super::{CHECK_COMMAND, COMMAND_LINE, THEN_COMMAND};

    /// The line presents the scene first and runs the witness command only
    /// after it, and only on a clean exit.
    #[test]
    fn the_witness_command_runs_only_after_the_check_left_cleanly() {
        let (check, then) = COMMAND_LINE
            .trim_end_matches('\n')
            .split_once(" && ")
            .expect("the line joins two commands with &&");
        assert_eq!(check.split_whitespace().next(), Some(CHECK_COMMAND));
        assert!(check.ends_with(" --present"), "{check}");
        assert_eq!(then, THEN_COMMAND);
        assert!(COMMAND_LINE.ends_with('\n'), "the line is entered");
    }
}
