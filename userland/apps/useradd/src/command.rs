//! The parsed shape of a `useradd` command line, the account it describes,
//! and the login-name validator.

use alloc::string::String;
use alloc::vec::Vec;

use crate::error::UseraddError;
use tairix_util::getopt::{Arg, Opt, Order, Scan, Takes};

/// Maximum length of a login name, in bytes.
///
/// Matches the long-standing Unix `LOGIN_NAME_MAX`/`login.defs` ceiling. The
/// bound keeps a hostile command line from forcing an unbounded name into the
/// database; raising it is a reviewed change here, not a per-call workaround.
pub const MAX_NAME_LEN: usize = 32;

/// One thing the `useradd` tool can do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Create the described account.
    Create(NewUser),
    /// Print the usage banner (`-h`/`--help`).
    Help,
}

/// A parsed `useradd` account specification.
///
/// All ids are **decimal** (TAIRiX has no name-to-id seam in this tool, so a
/// name would be interface creep). The primary group is
/// always present — `useradd` requires `-g` rather than guessing a default
/// group.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewUser {
    /// The login name, validated against [`validate_name`].
    pub name: String,
    /// The requested numeric user id (`-u`), or [`None`] to let the database
    /// allocate the next free one.
    pub uid: Option<u32>,
    /// The numeric primary group id (`-g`).
    pub primary_gid: u32,
    /// The numeric supplementary group ids (`-G`), in operand order. Empty
    /// when `-G` was not given.
    pub supplementary_gids: Vec<u32>,
    /// The optional account comment / full name (`-c`).
    pub comment: Option<String>,
    /// The optional home directory (`-d`).
    pub home: Option<String>,
}

/// What each of `useradd`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Uid,
    Gid,
    Groups,
    Comment,
    Home,
    Help,
}

/// `useradd`'s options and the reserved help switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::valued('u', &["uid"], Flag::Uid),
    Opt::valued('g', &["gid"], Flag::Gid),
    Opt::valued('G', &["groups"], Flag::Groups),
    Opt::valued('c', &["comment"], Flag::Comment),
    Opt::valued('d', &["home"], Flag::Home),
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
];

/// Parse `args` (the tool's arguments, excluding the program name) into a
/// [`Command`].
///
/// The grammar is
/// `useradd [-u UID] -g GID [-G LIST] [-c COMMENT] [-d HOME] [--] NAME`, read
/// as GNU's `getopt_long` reads it ([`Scan`]): options anywhere among the
/// operands, clustered, a value attached (`-u0`, `--uid=0`) or following
/// (`-u 0`), a long one by any unambiguous prefix, `--` ending them. The help
/// switches (`-h`, `-?`, `--help`) answer at once. `-g` is required, and so
/// is exactly one operand: the login name.
///
/// # Errors
///
/// [`UseraddError::Option`] for an option `getopt` refuses.
/// [`UseraddError::Usage`] for a missing `-g` or a number of operands other
/// than one. [`UseraddError::BadId`] when a `-u`/`-g`/`-G` value is not a
/// decimal id (or a `-G` element is empty). [`UseraddError::BadName`] when
/// the operand is not a valid login name.
pub fn parse(args: &[&str]) -> Result<Command, UseraddError> {
    let mut uid: Option<u32> = None;
    let mut primary_gid: Option<u32> = None;
    let mut supplementary_gids: Vec<u32> = Vec::new();
    let mut comment: Option<String> = None;
    let mut home: Option<String> = None;
    let mut operands: Vec<String> = Vec::new();
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(UseraddError::Option)? {
            Arg::Operand(operand) => operands.push(String::from(operand)),
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            Arg::Opt(Flag::Uid, value) => uid = value.map(parse_id).transpose()?,
            Arg::Opt(Flag::Gid, value) => primary_gid = value.map(parse_id).transpose()?,
            Arg::Opt(Flag::Groups, value) => {
                supplementary_gids = parse_id_list(value.unwrap_or_default())?;
            }
            Arg::Opt(Flag::Comment, value) => comment = value.map(String::from),
            Arg::Opt(Flag::Home, value) => home = value.map(String::from),
        }
    }

    if operands.len() != 1 {
        return Err(UseraddError::Usage);
    }
    let name = operands.remove(0);
    if !validate_name(&name) {
        return Err(UseraddError::BadName);
    }
    let primary_gid = primary_gid.ok_or(UseraddError::Usage)?;
    Ok(Command::Create(NewUser {
        name,
        uid,
        primary_gid,
        supplementary_gids,
        comment,
        home,
    }))
}

/// Validate a login name against `[a-z_][a-z0-9_-]*` within
/// [`MAX_NAME_LEN`].
///
/// The first character must be a lowercase ASCII letter or `_`; the rest may
/// also be ASCII digits or `-`. An empty or over-long name, an upper-case
/// letter, a leading digit or `-`, or any other byte is rejected. This is the
/// portable Unix login-name shape; it deliberately admits no name that could
/// be confused for a numeric id or an option.
#[must_use]
pub fn validate_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return false;
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap_or('\0');
    if !(first.is_ascii_lowercase() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Parse a comma-separated list of decimal ids into a vector.
///
/// # Errors
///
/// [`UseraddError::BadId`] for an empty list, an empty element, or an element
/// that is not a decimal [`u32`].
fn parse_id_list(text: &str) -> Result<Vec<u32>, UseraddError> {
    if text.is_empty() {
        return Err(UseraddError::BadId);
    }
    let mut ids = Vec::new();
    for element in text.split(',') {
        ids.push(parse_id(element)?);
    }
    Ok(ids)
}

/// Parse one non-empty run of decimal digits into a [`u32`].
///
/// # Errors
///
/// [`UseraddError::BadId`] for an empty string, a non-digit, or an overflow.
fn parse_id(text: &str) -> Result<u32, UseraddError> {
    if text.is_empty() {
        return Err(UseraddError::BadId);
    }
    let mut value: u32 = 0;
    for c in text.chars() {
        let digit = c.to_digit(10).ok_or(UseraddError::BadId)?;
        value = value
            .checked_mul(10)
            .and_then(|v| v.checked_add(digit))
            .ok_or(UseraddError::BadId)?;
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::{parse, validate_name, Command, NewUser, MAX_NAME_LEN};
    use crate::error::UseraddError;
    use alloc::string::{String, ToString};
    use alloc::vec;
    use alloc::vec::Vec;

    fn create(
        name: &str,
        uid: Option<u32>,
        primary_gid: u32,
        supplementary_gids: &[u32],
        comment: Option<&str>,
        home: Option<&str>,
    ) -> Command {
        Command::Create(NewUser {
            name: name.to_string(),
            uid,
            primary_gid,
            supplementary_gids: supplementary_gids.to_vec(),
            comment: comment.map(String::from),
            home: home.map(String::from),
        })
    }

    // ----- command-line parsing -------------------------------------------

    #[test]
    fn a_name_and_a_group_parses() {
        assert_eq!(
            parse(&["-g", "100", "alice"]),
            Ok(create("alice", None, 100, &[], None, None))
        );
    }

    #[test]
    fn every_option_parses() {
        assert_eq!(
            parse(&[
                "-u",
                "1000",
                "-g",
                "100",
                "-G",
                "10,20,30",
                "-c",
                "Alice A",
                "-d",
                "/Users/alice",
                "alice",
            ]),
            Ok(create(
                "alice",
                Some(1000),
                100,
                &[10, 20, 30],
                Some("Alice A"),
                Some("/Users/alice"),
            ))
        );
    }

    #[test]
    fn long_options_parse_with_space_or_equals() {
        assert_eq!(
            parse(&["--uid", "7", "--gid=100", "--groups", "1,2", "bob"]),
            Ok(create("bob", Some(7), 100, &[1, 2], None, None))
        );
    }

    #[test]
    fn attached_short_values_parse() {
        assert_eq!(
            parse(&["-u0", "-g0", "root_svc"]),
            Ok(create("root_svc", Some(0), 0, &[], None, None))
        );
    }

    #[test]
    fn question_mark_is_short_help() {
        assert_eq!(parse(&["-?"]), Ok(Command::Help));
    }

    #[test]
    fn help_wins_immediately() {
        assert_eq!(parse(&["-h"]), Ok(Command::Help));
        assert_eq!(parse(&["--help"]), Ok(Command::Help));
        assert_eq!(parse(&["-g", "100", "--help", "alice"]), Ok(Command::Help));
    }

    #[test]
    fn missing_primary_group_is_usage() {
        assert_eq!(parse(&["alice"]), Err(UseraddError::Usage));
        assert_eq!(parse(&["-u", "1000", "alice"]), Err(UseraddError::Usage));
    }

    #[test]
    fn wrong_operand_count_is_usage() {
        assert_eq!(parse(&["-g", "100"]), Err(UseraddError::Usage));
        assert_eq!(
            parse(&["-g", "100", "alice", "bob"]),
            Err(UseraddError::Usage)
        );
    }

    /// `getopt`'s words for each refusal.
    fn refused(args: &[&str]) -> String {
        parse(args).expect_err("refused").to_string()
    }

    #[test]
    fn a_refused_option_is_worded_as_getopt_words_it() {
        assert_eq!(refused(&["-x", "-g", "100", "a"]), "invalid option -- 'x'");
        assert_eq!(
            refused(&["--frob", "-g", "100", "a"]),
            "unrecognized option '--frob'"
        );
        assert_eq!(
            refused(&["-g", "100", "-u"]),
            "option requires an argument -- 'u'"
        );
        assert_eq!(refused(&["-g"]), "option requires an argument -- 'g'");
        assert_eq!(
            refused(&["-g", "100", "a", "--home"]),
            "option '--home' requires an argument"
        );
        assert_eq!(
            refused(&["--help=x", "-g", "100", "a"]),
            "option '--help' doesn't allow an argument"
        );
    }

    /// `getopt_long` reads `--gi` as `--gid` and `--com` as `--comment`, and
    /// refuses a prefix two long names share.
    #[test]
    fn a_long_option_may_be_abbreviated() {
        assert_eq!(
            parse(&["--gi=100", "--com", "Alice A", "alice"]),
            Ok(create("alice", None, 100, &[], Some("Alice A"), None))
        );
        assert_eq!(
            refused(&["--g", "100", "alice"]),
            "option '--g' is ambiguous; possibilities: '--gid' '--groups'"
        );
        assert_eq!(
            refused(&["--h"]),
            "option '--h' is ambiguous; possibilities: '--home' '--help'"
        );
    }

    #[test]
    fn double_dash_ends_options_so_a_dash_named_operand_is_taken() {
        // `-weird` after `--` is the (invalid) name operand, not an option.
        assert_eq!(
            parse(&["-g", "100", "--", "-weird"]),
            Err(UseraddError::BadName)
        );
    }

    #[test]
    fn a_non_decimal_id_is_bad_id() {
        assert_eq!(parse(&["-g", "wheel", "a"]), Err(UseraddError::BadId));
        assert_eq!(
            parse(&["-u", "0x10", "-g", "1", "a"]),
            Err(UseraddError::BadId)
        );
        assert_eq!(
            parse(&["-g", "1", "-G", "1,,2", "a"]),
            Err(UseraddError::BadId)
        );
        assert_eq!(parse(&["-g", "1", "-G", "", "a"]), Err(UseraddError::BadId));
        // u32::MAX + 1 overflows.
        assert_eq!(parse(&["-g", "4294967296", "a"]), Err(UseraddError::BadId));
    }

    #[test]
    fn an_invalid_name_is_bad_name() {
        assert_eq!(parse(&["-g", "1", "Alice"]), Err(UseraddError::BadName));
        assert_eq!(parse(&["-g", "1", "1abc"]), Err(UseraddError::BadName));
        assert_eq!(parse(&["-g", "1", "a$b"]), Err(UseraddError::BadName));
    }

    // ----- login-name validation -----------------------------------------

    #[test]
    fn valid_names_are_accepted() {
        assert!(validate_name("alice"));
        assert!(validate_name("_svc"));
        assert!(validate_name("user-1"));
        assert!(validate_name("a"));
        let max = "a".repeat(MAX_NAME_LEN);
        assert!(validate_name(&max));
    }

    #[test]
    fn invalid_names_are_rejected() {
        assert!(!validate_name(""));
        assert!(!validate_name("Alice")); // upper-case
        assert!(!validate_name("1abc")); // leading digit
        assert!(!validate_name("-abc")); // leading dash
        assert!(!validate_name("a b")); // space
        assert!(!validate_name("café")); // non-ASCII
        let too_long = "a".repeat(MAX_NAME_LEN + 1);
        assert!(!validate_name(&too_long));
    }

    /// Every locale's `OPTIONS` section documents exactly the switches this
    /// parser accepts (`plans/APPS.md` §3.1): the flag tokens are
    /// language-neutral, so each translated document must carry the same
    /// keys as the canonical one. The documents are read from the bundle's
    /// own on-disk `Help/` tree — the single source the image builder plants
    /// — never a copy embedded in this crate.
    #[test]
    fn help_documents_the_parser_switches() {
        extern crate std;
        use alloc::format;
        use std::fs;

        let help_root = format!("{}/Help", env!("CARGO_MANIFEST_DIR"));
        let locales = tairix_help::REQUIRED_LOCALES;
        for locale in locales {
            let path = format!("{help_root}/{locale}/useradd.md");
            let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
            for switch in [
                "`-u, --uid UID`",
                "`-g, --gid GID`",
                "`-G, --groups LIST`",
                "`-c, --comment TEXT`",
                "`-d, --home PATH`",
                "`-h, -?, --help`",
            ] {
                assert!(
                    text.contains(switch),
                    "{locale}/useradd.md must document {switch}"
                );
            }
        }
    }

    #[test]
    fn the_empty_supplementary_default_is_an_empty_vec() {
        let parsed = parse(&["-g", "1", "alice"]).expect("valid");
        let Command::Create(user) = parsed else {
            panic!("expected create");
        };
        assert_eq!(user.supplementary_gids, Vec::<u32>::new());
    }

    #[test]
    fn a_single_supplementary_group_parses() {
        assert_eq!(
            parse(&["-g", "1", "-G", "42", "alice"]),
            Ok(create("alice", None, 1, &[42], None, None))
        );
        // And the vec form is what we expect.
        assert_eq!(
            parse(&["-g", "1", "-G", "42", "alice"]),
            Ok(create("alice", None, 1, &vec![42][..], None, None))
        );
    }
}
