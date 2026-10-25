//! The parsed shape of a `mount` command line.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::MountFlags;
use tairix_util::getopt::{Arg, Opt, Order, Scan};

use crate::error::MountError;

/// One thing the `mount` tool can do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// List the current mount table (no operands). Issues the ungated
    /// `sysinfo-v1` `MOUNT_LIST` query.
    List,
    /// Attach the described filesystem (two operands).
    Mount(MountRequest),
    /// Print the usage banner (`-h`/`--help`).
    Help,
}

/// A parsed `mount` attach request.
///
/// The privileged *act* of mounting is the kernel's decision, gated on
/// `CAP_FS_MOUNT`; this struct only describes what the
/// user asked for. The driver `fstype` is optional — a kernel that probes
/// the superblock can identify it — and `flags` is the parsed mount policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MountRequest {
    /// The backing source (a `/Storage` volume or device identifier).
    pub source: String,
    /// The mount-point path.
    pub target: String,
    /// The driver filesystem type (`-t`), or [`None`] to let the kernel
    /// probe.
    pub fstype: Option<String>,
    /// The parsed mount-policy flags (`-r` and `-o`).
    pub flags: MountFlags,
}

/// What each of `mount`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    ReadOnly,
    Types,
    Options,
    Help,
}

/// `mount`'s options and the reserved help switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::flag('r', &["read-only"], Flag::ReadOnly),
    Opt::valued('t', &["types"], Flag::Types),
    Opt::valued('o', &["options"], Flag::Options),
    Opt::flag('h', &["help"], Flag::Help),
];

/// Parse `args` (the tool's arguments, excluding the program name) into a
/// [`Command`].
///
/// The grammar is `mount [-r] [-t TYPE] [-o OPTIONS] [--] [SOURCE TARGET]`,
/// read as GNU's `getopt_long` reads it ([`Scan`]): options anywhere among
/// the operands, clustered, `-t`'s and `-o`'s value attached (`-text4`,
/// `--types=ext4`) or the next argument, a long option by any unambiguous
/// prefix, `--` ending them.
///
/// * no operands — list the current mount table.
/// * `SOURCE TARGET` — attach `SOURCE` at `TARGET`.
/// * `-r` / `--read-only` — mount read-only (shorthand for `-o ro`).
/// * `-t` / `--types` TYPE — the driver filesystem type.
/// * `-o` / `--options` LIST — a comma-separated subset of `ro,rw,noexec`.
/// * `-h` / `--help` — print the usage banner (wins immediately).
///
/// # Errors
///
/// * [`MountError::Option`] — an option `getopt` refuses, in its words.
/// * [`MountError::Usage`] — a number of operands other than zero or two,
///   or mount options given without operands (there is nothing to mount).
/// * [`MountError::BadOption`] — an unknown or empty `-o`/`-t` value.
pub fn parse(args: &[&str]) -> Result<Command, MountError> {
    let mut flags = MountFlags::default();
    let mut fstype: Option<String> = None;
    let mut operands: Vec<String> = Vec::new();
    let mut saw_option = false;

    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        let arg = arg.map_err(MountError::Option)?;
        saw_option |= matches!(arg, Arg::Opt(..));
        match arg {
            Arg::Operand(operand) => operands.push(String::from(operand)),
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            Arg::Opt(Flag::ReadOnly, _) => flags = flags.union(MountFlags::READ_ONLY),
            Arg::Opt(Flag::Types, value) => fstype = value.map(String::from),
            Arg::Opt(Flag::Options, value) => {
                flags = flags.union(parse_options(value.unwrap_or_default())?);
            }
        }
    }

    match operands.len() {
        0 if saw_option => Err(MountError::Usage),
        0 => Ok(Command::List),
        2 => {
            let mut operands = operands.into_iter();
            let source = operands.next().unwrap_or_default();
            let target = operands.next().unwrap_or_default();
            Ok(Command::Mount(MountRequest {
                source,
                target,
                fstype,
                flags,
            }))
        }
        _ => Err(MountError::Usage),
    }
}

/// Parse a comma-separated `-o` option list into the [`MountFlags`] it sets.
///
/// Accepts the policy names TAIRiX recognises: `ro`/`rw` (read-only vs the
/// read-write default) and the `noexec` restriction. `rw` clears nothing on
/// its own — it is the default — but is accepted so a user can write it
/// explicitly. There are no setuid programs or device files to restrict, so
/// `nosuid` and `nodev` are refused rather than accepted as no-ops.
///
/// # Errors
///
/// [`MountError::BadOption`] for an empty list, an empty element, or an
/// unrecognised option name.
fn parse_options(text: &str) -> Result<MountFlags, MountError> {
    if text.is_empty() {
        return Err(MountError::BadOption);
    }
    let mut flags = MountFlags::default();
    for element in text.split(',') {
        let flag = match element {
            "rw" => MountFlags::default(),
            "ro" => MountFlags::READ_ONLY,
            "noexec" => MountFlags::NOEXEC,
            _ => return Err(MountError::BadOption),
        };
        flags = flags.union(flag);
    }
    Ok(flags)
}

#[cfg(test)]
mod tests {
    use super::{parse, Command, MountRequest};
    use crate::error::MountError;
    use alloc::string::{String, ToString};
    use tairix_abi::driver::filesystem::MountFlags;

    fn mount(source: &str, target: &str, fstype: Option<&str>, flags: MountFlags) -> Command {
        Command::Mount(MountRequest {
            source: String::from(source),
            target: String::from(target),
            fstype: fstype.map(String::from),
            flags,
        })
    }

    #[test]
    fn no_arguments_lists_the_mount_table() {
        assert_eq!(parse(&[]), Ok(Command::List));
        assert_eq!(parse(&["--"]), Ok(Command::List));
    }

    #[test]
    fn two_operands_describe_a_mount() {
        assert_eq!(
            parse(&["/Storage/data", "/Storage/data"]),
            Ok(mount(
                "/Storage/data",
                "/Storage/data",
                None,
                MountFlags::default()
            ))
        );
    }

    #[test]
    fn every_option_parses() {
        assert_eq!(
            parse(&[
                "-r",
                "-t",
                "arxfs",
                "-o",
                "rw,noexec",
                "vol",
                "/Storage/vol",
            ]),
            Ok(mount(
                "vol",
                "/Storage/vol",
                Some("arxfs"),
                MountFlags::READ_ONLY.union(MountFlags::NOEXEC),
            ))
        );
    }

    #[test]
    fn long_options_with_space_or_equals() {
        assert_eq!(
            parse(&["--types=ext4", "--options", "ro", "dev", "/Storage/d"]),
            Ok(mount(
                "dev",
                "/Storage/d",
                Some("ext4"),
                MountFlags::READ_ONLY
            ))
        );
    }

    #[test]
    fn attached_short_values_parse() {
        assert_eq!(
            parse(&["-tarxfs", "-onoexec", "v", "/Storage/v"]),
            Ok(mount("v", "/Storage/v", Some("arxfs"), MountFlags::NOEXEC))
        );
    }

    #[test]
    fn read_only_shorthand_sets_the_flag() {
        let Command::Mount(req) = parse(&["-r", "v", "/Storage/v"]).expect("valid") else {
            panic!("expected mount");
        };
        assert!(req.flags.contains(MountFlags::READ_ONLY));
    }

    #[test]
    fn rw_option_is_the_read_write_default() {
        assert_eq!(
            parse(&["-o", "rw", "v", "/Storage/v"]),
            Ok(mount("v", "/Storage/v", None, MountFlags::default()))
        );
    }

    #[test]
    fn help_wins_immediately() {
        assert_eq!(parse(&["-h"]), Ok(Command::Help));
        assert_eq!(parse(&["--help"]), Ok(Command::Help));
        assert_eq!(parse(&["-r", "--help", "a", "b"]), Ok(Command::Help));
    }

    #[test]
    fn options_without_operands_is_usage() {
        // There is nothing to mount, and options never apply to a listing.
        assert_eq!(parse(&["-r"]), Err(MountError::Usage));
        assert_eq!(parse(&["-t", "ext4"]), Err(MountError::Usage));
    }

    #[test]
    fn wrong_operand_count_is_usage() {
        assert_eq!(parse(&["only-one"]), Err(MountError::Usage));
        assert_eq!(parse(&["a", "b", "c"]), Err(MountError::Usage));
    }

    /// GNU 9.4's words for each refusal.
    fn refused(args: &[&str]) -> String {
        parse(args).expect_err("refused").to_string()
    }

    #[test]
    fn a_refused_option_is_worded_as_getopt_words_it() {
        assert_eq!(refused(&["-x", "a", "b"]), "invalid option -- 'x'");
        assert_eq!(
            refused(&["--frob", "a", "b"]),
            "unrecognized option '--frob'"
        );
        assert_eq!(refused(&["-t"]), "option requires an argument -- 't'");
        assert_eq!(
            refused(&["a", "b", "-o"]),
            "option requires an argument -- 'o'"
        );
        assert_eq!(
            refused(&["--types"]),
            "option '--types' requires an argument"
        );
        assert_eq!(
            refused(&["--read-only=1", "a", "b"]),
            "option '--read-only' doesn't allow an argument"
        );
    }

    /// `getopt_long` reads `mount --read --ty=ext4` as
    /// `mount --read-only --types=ext4`.
    #[test]
    fn a_long_option_may_be_abbreviated() {
        assert_eq!(
            parse(&["--read", "--ty=ext4", "dev", "/Storage/d"]),
            Ok(mount(
                "dev",
                "/Storage/d",
                Some("ext4"),
                MountFlags::READ_ONLY
            ))
        );
        assert_eq!(parse(&["--he"]), Ok(Command::Help));
    }

    #[test]
    fn bad_option_value_is_bad_option() {
        assert_eq!(
            parse(&["-o", "weird", "a", "b"]),
            Err(MountError::BadOption)
        );
        assert_eq!(parse(&["-o", "", "a", "b"]), Err(MountError::BadOption));
        assert_eq!(
            parse(&["-o", "ro,,noexec", "a", "b"]),
            Err(MountError::BadOption)
        );
        for meaningless in ["nosuid", "nodev"] {
            assert_eq!(
                parse(&["-o", meaningless, "a", "b"]),
                Err(MountError::BadOption),
                "{meaningless} restricts nothing here"
            );
        }
    }

    #[test]
    fn double_dash_protects_a_dash_named_operand() {
        assert_eq!(
            parse(&["--", "-weird", "/Storage/x"]),
            Ok(mount("-weird", "/Storage/x", None, MountFlags::default()))
        );
    }
}
