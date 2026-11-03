//! The file manager's command line: the starting location a caller may name,
//! and the reserved short-help switches.
//!
//! # Why this is its own module
//!
//! The `Run` binary around it is a freestanding program — it only exists when
//! the crate is built for a bare-metal target — so nothing inside it can be
//! reached by a host test. Deciding what a command line means is worth
//! testing: which spellings open a folder, which are turned down and with what
//! reason, and which are a usage error the program refuses outright. All of
//! that is a pure function of the argument vector, so it lives here, compiles
//! on the host, and is covered by the tests beside it.
//!
//! # The grammar
//!
//! `files --desktop [--mount] [path]`, with the argument conventions every
//! other command app follows: `-h`/`-?`/`--help` win immediately, `--` ends
//! option parsing, and at most one operand is accepted. With `--mount` the
//! operand is a disk image or archive to attach and show rather than a place
//! to open at.
//!
//! # The desktop's own file manager
//!
//! This program is a **component of the desktop**: it starts with the
//! session, holds a permanent icon-bar slot offering the user's places and
//! whatever is mounted, and never quits. `--desktop` is the session saying so,
//! and a command line without it is refused, so it never runs as an ordinary
//! application; the session hands what it opens to the instance it started. At bring-up the session names
//! nothing and no window opens; when it starts the file manager again to show
//! a folder or an image, the operand names it.
//!
//! # Untrusted input, and the two different refusals
//!
//! The operand comes from whoever launched the program — the desktop opening
//! a folder, a shell, or something hostile — so it is validated before it is
//! used, through the one shared path grammar rather than a second parser here.
//! There are deliberately two outcomes:
//!
//! * A **malformed or out-of-bounds location** ([`Start::refused`]) is a
//!   refusal the program states and recovers from: the window still opens,
//!   among the launching user's own files. A bad argument never leaves the
//!   user with no window.
//! * A **command line the program cannot act on at all** ([`UsageError`]) — an
//!   unrecognised option, or a second operand — is refused outright, as in
//!   every other command app: guessing which of two operands was meant would
//!   be worse than saying so.
//!
//! Nothing here opens, lists, or writes anything: whether the accepted
//! location can actually be listed is the filesystem's decision, made under
//! the launching user's own identity by the program around this module, which
//! walks the [`Ladder`] of places to fall back to until one lists.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use tairix_abi::fs::FS_PATH_MAX;
use tairix_abi::Errno;
use tairix_browse::vfs::{components_from_absolute_path, spell_absolute_path, spell_shown_path};
use tairix_browse::{BrowseError, Unnamable};
use tairix_util::getopt::{Arg, Opt, Order, Refusal, Scan, Takes};
use tairix_util::quote::quote;

use crate::operation::device_refusal;
use tairix_browse::mount::MOUNT_SWITCH;

/// The usage banner a usage error is reported with, and the fallback the
/// short-help switches print when `files`'s own Help tree is unavailable.
pub const USAGE: &str = "usage: files --desktop [--mount] [path] [-h | -?]";

/// What a `files` command line asks the program to do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Run the file manager, as [`Start`] describes.
    Open(Start),
    /// Render `files`'s own short help (`-h`/`-?`/`--help`): the `NAME`,
    /// `SYNOPSIS`, and compact `OPTIONS` of its Help document, through the
    /// same engine as any other command's short help.
    Help,
}

/// What the file manager shows as it starts, and what the command line asked
/// for that could not be honoured.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Start {
    /// The accepted place to show as root-first path components, or `None`
    /// when the command line named none or the one it named was refused.
    pub location: Option<Vec<String>>,
    /// The disk image or archive to attach and show, as an absolute path,
    /// where `--mount` named one that was accepted: the window opens on its
    /// volume once it is attached, and on none before.
    pub image: Option<String>,
    /// The reason a named location was turned down, ready to be stated on the
    /// error stream, or `None` when nothing was refused.
    ///
    /// Carried out rather than written here so the program states it through
    /// the single fail-loud reporting path it already uses for every other
    /// refusal, and so a test can read exactly what a user would be told.
    pub refused: Option<String>,
}

/// A command line the program cannot act on, and so refuses outright.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UsageError {
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
    /// A second operand; carries it. The window shows one directory, so two
    /// locations have no meaning and the second is never silently dropped.
    ExtraOperand(String),
    /// No desktop-role switch: the file manager is the desktop's own and runs
    /// only as the session starts it.
    NotByDesktop,
    /// `--mount` naming no image.
    NoImage,
    /// The argument vector was not valid UTF-8 and so could not be read at
    /// all.
    NotUtf8,
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UsageError::Option(refusal) => write!(f, "{refusal}"),
            UsageError::ExtraOperand(arg) => write!(f, "extra operand {}", quote(arg)),
            UsageError::NotByDesktop => {
                f.write_str("the file manager starts with the desktop and does not run on its own")
            }
            UsageError::NoImage => write!(f, "{MOUNT_SWITCH} names no disk image"),
            UsageError::NotUtf8 => f.write_str("argument vector is not valid UTF-8"),
        }
    }
}

/// What each of `files`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Desktop,
    Help,
    Mount,
}

/// [`tairix_window::DESKTOP_ROLE_SWITCH`] as a long option's name, its `--`
/// taken off.
const DESKTOP_ROLE: &str = tairix_window::DESKTOP_ROLE_SWITCH.split_at(2).1;

/// [`MOUNT_SWITCH`] as a long option's name, its `--` taken off.
const MOUNT: &str = MOUNT_SWITCH.split_at(2).1;

/// `files`'s options, and the reserved help switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::long(&[DESKTOP_ROLE], Takes::Nothing, Flag::Desktop),
    Opt::long(&[MOUNT], Takes::Nothing, Flag::Mount),
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
];

/// Parse `args` (the program's arguments, excluding the program name) into a
/// [`Command`].
///
/// The grammar is `files --desktop [--mount] [path]`, read as GNU's
/// `getopt_long` reads it ([`Scan`]): options anywhere among the operands, a
/// long one by any unambiguous prefix, `--` ending them. The help switches
/// (`-h`, `-?`, `--help`) answer at once; `--desktop` is the session starting
/// its file manager. The one optional operand is the folder to show, or a file
/// to show selected in the folder holding it — or, with `--mount`, the disk
/// image or archive to attach and show. It is validated here and, when it does
/// not survive, reported through [`Start::refused`] with a window still
/// opening among the launching user's own files.
///
/// # Errors
///
/// * [`UsageError::Option`] — an option `getopt` refuses, in its words.
/// * [`UsageError::ExtraOperand`] — a second operand.
/// * [`UsageError::NotByDesktop`] — no `--desktop`.
/// * [`UsageError::NoImage`] — `--mount` with no operand.
pub fn parse(args: &[&str]) -> Result<Command, UsageError> {
    let mut operand: Option<&str> = None;
    let mut desktop = false;
    let mut mount = false;
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(UsageError::Option)? {
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            Arg::Opt(Flag::Desktop, _) => desktop = true,
            Arg::Opt(Flag::Mount, _) => mount = true,
            Arg::Operand(spelling) => {
                if operand.is_some() {
                    return Err(UsageError::ExtraOperand(String::from(spelling)));
                }
                operand = Some(spelling);
            }
        }
    }
    if !desktop {
        return Err(UsageError::NotByDesktop);
    }
    if mount {
        let spelling = operand.ok_or(UsageError::NoImage)?;
        return Ok(Command::Open(match image_path(spelling) {
            Ok(image) => Start {
                image: Some(image),
                ..Start::default()
            },
            Err(refused) => Start {
                refused: Some(format!("{refused}; {FALLBACK}")),
                ..Start::default()
            },
        }));
    }
    Ok(Command::Open(start_at(operand)))
}

impl Start {
    /// Whether a window opens as the file manager starts: the command line
    /// named a place to show, or one that was refused, which opens among the
    /// user's own files instead. An image opens its window once attached.
    #[must_use]
    pub const fn shows_at_start(&self) -> bool {
        self.image.is_none() && (self.location.is_some() || self.refused.is_some())
    }
}

/// The starting location `operand` names, or none with the reason it was
/// refused.
///
/// Separate from [`parse`] so the validation of the location — the part that
/// treats the argument as hostile — is one function with one job.
fn start_at(operand: Option<&str>) -> Start {
    let Some(spelling) = operand else {
        return Start::default();
    };
    match location_components(spelling) {
        Ok(location) => Start {
            location: Some(location),
            ..Start::default()
        },
        Err(refused) => Start {
            refused: Some(refused),
            ..Start::default()
        },
    }
}

/// Validate one starting-location spelling into root-first path components.
///
/// Fail closed, and bounded before anything is allocated from it: the raw
/// argument must be within the kernel's own path bound, must be an absolute
/// view path (an alias-rooted or relative spelling names a place this window
/// cannot address), and every component must survive the one shared filename
/// rule — which is what rejects `.`, `..`, a control character, and an
/// over-long name.
///
/// The one rule for every path this program is *given* — the command line's
/// starting location, and an open target the desktop hands a running
/// instance — so a launch and a relaunch accept exactly the same spellings.
///
/// # Errors
///
/// The reason to state on the error stream, phrased for a user and naming the
/// spelling only when it is short enough to be worth echoing.
pub fn location_components(spelling: &str) -> Result<Vec<String>, String> {
    if spelling.len() > FS_PATH_MAX {
        return Err(format!(
            "starting location refused (longer than {FS_PATH_MAX} bytes); {FALLBACK}"
        ));
    }
    if !spelling.starts_with('/') {
        return Err(refused(spelling, "not an absolute path"));
    }
    components_from_absolute_path(spelling).map_err(|errno| match errno {
        Errno::LengthOutOfRange => refused(spelling, "path exceeds the maximum length"),
        _ => refused(spelling, &malformed_detail(spelling)),
    })
}

/// Validate one disk image's or archive's spelling into the absolute path an
/// attach names: [`location_components`]'s rule, for an image this program is
/// given — by `--mount`, or as an open target the desktop hands a running
/// instance.
///
/// # Errors
///
/// The reason to state on the error stream.
pub fn image_path(spelling: &str) -> Result<String, String> {
    if spelling.len() > FS_PATH_MAX {
        return Err(format!(
            "disk image refused (longer than {FS_PATH_MAX} bytes)"
        ));
    }
    if !spelling.starts_with('/') {
        return Err(image_refused(spelling, "not an absolute path"));
    }
    components_from_absolute_path(spelling)
        .map(|components| spell_absolute_path(&components))
        .map_err(|errno| match errno {
            Errno::LengthOutOfRange => image_refused(spelling, "path exceeds the maximum length"),
            _ => image_refused(spelling, &malformed_detail(spelling)),
        })
}

/// One refusal of an image named `spelling`, for `detail`, escaped as a
/// location's is.
fn image_refused(spelling: &str, detail: &str) -> String {
    format!("disk image {spelling:?} refused ({detail})")
}

/// Which rule the spelling broke, taken from the one shared filename rule so
/// this module never carries a second copy of it.
///
/// Walked only on the refusal path, and only to phrase the diagnosis: the
/// acceptance decision was already made by the shared path parser.
fn malformed_detail(spelling: &str) -> String {
    for segment in spelling.split('/').filter(|segment| !segment.is_empty()) {
        if let Err(err) = tairix_path::validate_file_name(segment) {
            return format!("{segment:?}: {err}");
        }
    }
    String::from("not a valid path")
}

/// One refusal sentence: what was named, why it was turned down, and where the
/// window opens instead.
///
/// The spelling is written through its debug form so a control character or an
/// escape sequence in a hostile argument is escaped rather than replayed at
/// whatever terminal reads the error stream.
fn refused(spelling: &str, detail: &str) -> String {
    format!("starting location {spelling:?} refused ({detail}); {FALLBACK}")
}

/// Where a window whose named location was refused opens: where a bare open
/// would ([`tairix_browse::bare_open_places`]).
const FALLBACK: &str = "opening your own files instead";

/// The sentence to state when a location a window was to open at turns out
/// not to be listable, saying `why` and naming the place tried `instead`.
///
/// It lives here, with the rest of the command line's vocabulary, so the
/// wording of every "we are opening somewhere else" message is decided in one
/// place and can be read by a test.
#[must_use]
pub fn unlistable_reason(location: &[String], why: &str, instead: &[String]) -> String {
    format!(
        "could not list {} ({why}); opening {} instead",
        spell_shown_path(location),
        spell_shown_path(instead)
    )
}

/// Why a folder could not be listed, from the listing's refusal.
#[must_use]
pub const fn listing_refusal(err: BrowseError) -> &'static str {
    match err {
        BrowseError::Source(Errno::PermissionDenied) => "you may not read it",
        BrowseError::Source(Errno::NotFound) | BrowseError::NoSuchEntry => "it does not exist",
        BrowseError::Source(Errno::NotADirectory) | BrowseError::NotADirectory => {
            "it is not a folder"
        }
        BrowseError::OutOfMemory => "there was not enough memory to list it",
        BrowseError::Unreachable => "it cannot be reached now",
        BrowseError::Unnamable(Unnamable::Name) => "its name holds a character no path may carry",
        BrowseError::Unnamable(Unnamable::Length) => "its path is too long",
        BrowseError::Source(errno) => device_refusal(errno, "it could not be read"),
    }
}

/// The places a window may open at, tried in order until one lists: the one
/// named, then the one a window asked for no folder opens at, then the
/// bare-open places beneath home ([`tairix_browse::bare_open_places`]), then
/// the root view, which the namespace always has.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ladder {
    /// The places not yet refused, the one being tried first.
    places: VecDeque<Vec<String>>,
    /// Where the window was asked to open: the first place tried.
    asked: Vec<String>,
    /// Why that place was refused, once it has been.
    asked_why: Option<&'static str>,
    /// The folder the place asked for turned out to be a file in, and that
    /// file's name, to be shown selected there.
    shows: Option<(Vec<String>, String)>,
}

impl Ladder {
    /// The ladder from `named` (a bare open when `None`) for a user whose home
    /// is `home`, a window asked for no folder opening at `start`. A place
    /// reached twice — `named` *is* home, say — is tried once: trying it again
    /// after its refusal could only be refused again, and would say so twice.
    #[must_use]
    pub fn new(
        named: Option<Vec<String>>,
        home: Option<&[String]>,
        start: Option<Vec<String>>,
    ) -> Self {
        let beneath_home = home
            .map(tairix_browse::bare_open_places)
            .into_iter()
            .flatten();
        let mut places: VecDeque<Vec<String>> = VecDeque::new();
        for place in named
            .into_iter()
            .chain(start)
            .chain(beneath_home)
            .chain([Vec::new()])
        {
            if !places.contains(&place) {
                places.push_back(place);
            }
        }
        let asked = places.front().cloned().unwrap_or_default();
        Self {
            places,
            asked,
            asked_why: None,
            shows: None,
        }
    }

    /// The place asked for turned out, as `err` says, to be a file rather than
    /// a folder: try the folder holding it next, to show the file selected
    /// there. Answers whether that is what the ladder now tries — never once
    /// it has moved past the place asked for.
    pub fn show_instead(&mut self, err: BrowseError) -> bool {
        let a_file = matches!(
            err,
            BrowseError::Source(Errno::NotADirectory) | BrowseError::NotADirectory
        );
        if !a_file || self.shows.is_some() || self.places.front() != Some(&self.asked) {
            return false;
        }
        let Some((name, folder)) = self.asked.split_last() else {
            return false;
        };
        let (name, folder) = (name.clone(), folder.to_vec());
        self.places.pop_front();
        self.places.retain(|place| *place != folder);
        self.places.push_front(folder.clone());
        self.asked.clone_from(&folder);
        self.shows = Some((folder, name));
        true
    }

    /// The entry a window at `place` shows selected: the file the place asked
    /// for turned out to be, when `place` is the folder holding it.
    #[must_use]
    pub fn shows_in(&self, place: &[String]) -> Option<&str> {
        self.shows
            .as_ref()
            .filter(|(folder, _)| folder.as_slice() == place)
            .map(|(_, name)| name.as_str())
    }

    /// The place being tried, or `None` once every place has been refused.
    #[must_use]
    pub fn place(&self) -> Option<&[String]> {
        self.places.front().map(Vec::as_slice)
    }

    /// The place being tried was refused, for `why`: move on to the next,
    /// answering the sentence that states the move, or `None` when no place is
    /// left.
    pub fn step(&mut self, why: &'static str) -> Option<String> {
        let refused = self.places.pop_front()?;
        self.asked_why.get_or_insert(why);
        let next = self.places.front()?;
        Some(unlistable_reason(&refused, why, next))
    }

    /// Why the window is not opening where it was asked — the place asked for,
    /// why it was refused, and the one it opens at instead — or `None` while it
    /// still is.
    #[must_use]
    pub fn moved(&self) -> Option<String> {
        let why = self.asked_why?;
        let place = self.places.front()?;
        Some(unlistable_reason(&self.asked, why, place))
    }
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
