//! The parsed shape of a `usermod` command line.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use tairix_util::getopt::{Arg, Opt, Order, Refusal, Scan, Takes};

/// Why the command line was not understood. Nothing is modified.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UsageError {
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
    /// A non-decimal id, a contradictory lock request, an operand count
    /// other than one, or a line that asks for no change.
    Usage,
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::Usage => f.write_str("invalid usage"),
        }
    }
}

/// Which lock state a command line asked for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Lock {
    /// `-L`: bar the account from logging in.
    Locked,
    /// `-U`: let it log in again.
    Unlocked,
}

/// What a `usermod` command line asks to change.
///
/// Every field is optional, and an absent one is left exactly as the
/// account holds it — the tool never rewrites a field nobody named.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Changes {
    /// `-c`: the account comment / display name.
    pub comment: Option<String>,
    /// `-d`: the home directory.
    pub home: Option<String>,
    /// `-s`: the login shell.
    pub shell: Option<String>,
    /// `-g`: the primary group id.
    pub primary_gid: Option<u32>,
    /// `-G`: the whole supplementary group set, replacing the current one.
    pub supplementary_gids: Option<Vec<u32>>,
    /// `--grants`: the whole capability grant ceiling, replacing the
    /// current one.
    pub grants: Option<String>,
    /// `-L` / `-U`: the lock state.
    pub lock: Option<Lock>,
}

impl Changes {
    /// Whether any identity field was named, which is what decides
    /// whether a whole-record replacement is sent at all.
    #[must_use]
    pub const fn touches_identity(&self) -> bool {
        self.comment.is_some()
            || self.home.is_some()
            || self.shell.is_some()
            || self.primary_gid.is_some()
            || self.supplementary_gids.is_some()
    }

    /// Whether the line asked for nothing at all.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        !self.touches_identity() && self.grants.is_none() && self.lock.is_none()
    }
}

/// One thing the `usermod` tool can do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Apply `changes` to the named account.
    Modify {
        /// The account to modify.
        name: String,
        /// What to change about it.
        changes: Changes,
    },
    /// Render this command's own short help (`-h`/`-?`/`--help`).
    Help,
}

/// What each of `usermod`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Comment,
    Home,
    Shell,
    Gid,
    Groups,
    Grants,
    Lock,
    Unlock,
    Help,
}

/// `usermod`'s options and the reserved help switches. `--grants`, the
/// capability ceiling, is a TAIRiX concept with no shadow-utils counterpart,
/// so it is long-only and cannot collide with a shadow-utils switch.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::valued('c', &["comment"], Flag::Comment),
    Opt::valued('d', &["home"], Flag::Home),
    Opt::valued('s', &["shell"], Flag::Shell),
    Opt::valued('g', &["gid"], Flag::Gid),
    Opt::valued('G', &["groups"], Flag::Groups),
    Opt::long(&["grants"], Takes::Value, Flag::Grants),
    Opt::flag('L', &["lock"], Flag::Lock),
    Opt::flag('U', &["unlock"], Flag::Unlock),
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
];

/// Parse `args` (the tool's arguments, excluding the program name).
///
/// The grammar is
/// `usermod [-c COMMENT] [-d HOME] [-s SHELL] [-g GID] [-G LIST] [-L | -U]
/// [--grants LIST] [--] NAME`, read as GNU's `getopt_long` reads it
/// ([`Scan`]): options anywhere among the operands, clustered, a value
/// attached (`-g0`, `--gid=0`) or following, a long one by any unambiguous
/// prefix, `--` ending them. The help switches (`-h`, `-?`, `--help`) answer
/// at once.
///
/// `-G` **replaces** the supplementary set: GNU's `-a` append is
/// deliberately absent, because the syscall takes whole sets and an append
/// that silently read a stale listing would be worse than an explicit
/// replacement. `-L` with `-U` is refused rather than guessed at. Exactly
/// one operand — the account name — is required, and a line that asks for
/// no change at all is refused rather than run as a no-op.
///
/// # Errors
///
/// [`UsageError::Option`] for an option `getopt` refuses, and
/// [`UsageError::Usage`] for a non-decimal id, both lock switches, an
/// operand count other than one, or an empty change set.
pub fn parse(args: &[&str]) -> Result<Command, UsageError> {
    let mut changes = Changes::default();
    let mut operands: Vec<&str> = Vec::new();
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(UsageError::Option)? {
            Arg::Operand(operand) => operands.push(operand),
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            Arg::Opt(Flag::Comment, value) => changes.comment = value.map(String::from),
            Arg::Opt(Flag::Home, value) => changes.home = value.map(String::from),
            Arg::Opt(Flag::Shell, value) => changes.shell = value.map(String::from),
            Arg::Opt(Flag::Gid, value) => changes.primary_gid = value.map(id).transpose()?,
            Arg::Opt(Flag::Groups, value) => {
                changes.supplementary_gids = value.map(id_list).transpose()?;
            }
            Arg::Opt(Flag::Grants, value) => changes.grants = value.map(String::from),
            Arg::Opt(Flag::Lock, _) => set_lock(&mut changes, Lock::Locked)?,
            Arg::Opt(Flag::Unlock, _) => set_lock(&mut changes, Lock::Unlocked)?,
        }
    }
    let [name] = operands.as_slice() else {
        return Err(UsageError::Usage);
    };
    if changes.is_empty() {
        return Err(UsageError::Usage);
    }
    Ok(Command::Modify {
        name: String::from(*name),
        changes,
    })
}

/// Record a lock request, refusing a line that asks for both states.
fn set_lock(changes: &mut Changes, lock: Lock) -> Result<(), UsageError> {
    match changes.lock {
        Some(held) if held != lock => Err(UsageError::Usage),
        _ => {
            changes.lock = Some(lock);
            Ok(())
        }
    }
}

/// An option's value as a decimal id.
fn id(value: &str) -> Result<u32, UsageError> {
    value.parse::<u32>().map_err(|_| UsageError::Usage)
}

/// An option's value as a comma-separated decimal id list.
///
/// An empty string is the empty set — how a caller clears a supplementary
/// membership — while an empty *element* (`1,,2`) is malformed.
fn id_list(value: &str) -> Result<Vec<u32>, UsageError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    value.split(',').map(id).collect()
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
