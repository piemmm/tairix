//! The parsed shape of an `unmount` command line.

use alloc::string::String;

use tairix_util::getopt::{Arg, Opt, Order, Scan};

use crate::error::UnmountError;

/// One thing the `unmount` tool can do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Detach the volume mounted under `name`. `force` selects the
    /// audited force-unmount that discards retained uncommitted data
    /// when a clean commit is impossible.
    Unmount {
        /// The volume's catalog name (`usb1`) or its mount-point path
        /// (`/Storage/usb1`), matched against the mount listing.
        name: String,
        /// `true` for `-f`/`--force`.
        force: bool,
    },
    /// Render the tool's own short help (`-?`/`--help`).
    Help,
}

/// What each of `unmount`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Force,
    Help,
}

/// `unmount`'s option, as `umount` spells it, and the reserved help switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::flag('f', &["force"], Flag::Force),
    Opt::flag('?', &["help"], Flag::Help),
];

/// Parse `args` (the tool's arguments, excluding the program name) into a
/// [`Command`].
///
/// The grammar is `unmount [-f] [--] NAME`, following the established
/// `umount` surface, read as GNU's `getopt_long` reads it ([`Scan`]):
/// options anywhere among the operands, a long one by any unambiguous
/// prefix, `--` ending them.
///
/// * `NAME` — the volume to detach, by catalog name or mount-point path.
/// * `-f` / `--force` — force-unmount: retract the volume even when its
///   uncommitted data cannot be committed, deliberately discarding the
///   retained set (an audited, capability-gated kernel decision).
/// * `-?` / `--help` — the tool's own short help (wins immediately).
///
/// # Errors
///
/// * [`UnmountError::Option`] — an option `getopt` refuses, in its words.
/// * [`UnmountError::Usage`] — a number of operands other than exactly one.
pub fn parse(args: &[&str]) -> Result<Command, UnmountError> {
    let mut force = false;
    let mut name: Option<String> = None;
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(UnmountError::Option)? {
            Arg::Operand(_) if name.is_some() => return Err(UnmountError::Usage),
            Arg::Operand(operand) => name = Some(String::from(operand)),
            Arg::Opt(Flag::Force, _) => force = true,
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
        }
    }

    match name {
        Some(name) => Ok(Command::Unmount { name, force }),
        None => Err(UnmountError::Usage),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse, Command};
    use crate::error::UnmountError;
    use alloc::string::{String, ToString};

    fn unmount(name: &str, force: bool) -> Command {
        Command::Unmount {
            name: String::from(name),
            force,
        }
    }

    #[test]
    fn one_operand_is_a_plain_unmount() {
        assert_eq!(parse(&["usb1"]), Ok(unmount("usb1", false)));
        assert_eq!(
            parse(&["/Storage/usb1"]),
            Ok(unmount("/Storage/usb1", false))
        );
    }

    #[test]
    fn force_parses_short_and_long() {
        assert_eq!(parse(&["-f", "usb1"]), Ok(unmount("usb1", true)));
        assert_eq!(parse(&["--force", "usb1"]), Ok(unmount("usb1", true)));
        assert_eq!(parse(&["usb1", "-f"]), Ok(unmount("usb1", true)));
    }

    #[test]
    fn help_switches_win_immediately() {
        assert_eq!(parse(&["-?"]), Ok(Command::Help));
        assert_eq!(parse(&["--help"]), Ok(Command::Help));
        assert_eq!(parse(&["-f", "--help", "usb1"]), Ok(Command::Help));
    }

    #[test]
    fn double_dash_protects_a_dash_named_operand() {
        assert_eq!(parse(&["--", "-f"]), Ok(unmount("-f", false)));
        assert_eq!(
            parse(&["-f", "--", "--force"]),
            Ok(unmount("--force", true))
        );
    }

    #[test]
    fn wrong_operand_counts_are_usage() {
        assert_eq!(parse(&[]), Err(UnmountError::Usage));
        assert_eq!(parse(&["-f"]), Err(UnmountError::Usage));
        assert_eq!(parse(&["a", "b"]), Err(UnmountError::Usage));
    }

    /// GNU 9.4's words for each refusal.
    fn refused(args: &[&str]) -> String {
        parse(args).expect_err("refused").to_string()
    }

    #[test]
    fn a_refused_option_is_worded_as_getopt_words_it() {
        assert_eq!(refused(&["-x", "usb1"]), "invalid option -- 'x'");
        assert_eq!(refused(&["--frob", "usb1"]), "unrecognized option '--frob'");
        assert_eq!(refused(&["-fx", "usb1"]), "invalid option -- 'x'");
        assert_eq!(
            refused(&["--force=1", "usb1"]),
            "option '--force' doesn't allow an argument"
        );
    }

    /// `getopt_long` reads `unmount --for usb1` as `unmount --force usb1`.
    #[test]
    fn a_long_option_may_be_abbreviated() {
        assert_eq!(parse(&["--for", "usb1"]), Ok(unmount("usb1", true)));
        assert_eq!(parse(&["--he"]), Ok(Command::Help));
    }
}
