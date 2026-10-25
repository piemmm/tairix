//! TAIRiX `fstree` — the full-screen tree file manager
//! (`plans/APPS.md`).
//!
//! A persistent directory-tree pane plus a file pane over the storage
//! forest, drawn with the OS curses library. This crate delivers the S1
//! model core (the lazily populated tree, pane navigation, sorting, the
//! hidden-entries toggle, the status/message lines, the `?` help overlay),
//! the S2 file operations (copy, move, rename, delete, mkdir, and the
//! permission-bits editor, each planned and validated before any I/O and
//! carried out by the shared operation engine, `lib/fscopy`, whose per-file
//! overwrite questions run through the key loop), and the S3 tagging
//! surface: multi-file tags
//! (`t`, tag-by-glob, invert, clear), batch copy/move/delete over the
//! tagged set with a per-file continue-on-error report, the flattened
//! branch view, and disk-usage statistics — both fed by one bounded,
//! cancellable walker — and the S4 search surface: a live filename filter
//! per pane (`f`), a branch-wide filename search (`/`), and a streaming
//! file-content search (`F`) over the tagged set or the focused branch,
//! both searches feeding the same taggable, operable flattened list.
//! The S5 viewers open with Enter on a regular file: a streaming text
//! pager and an offset/hex/ASCII dump, auto-picked from a head sample,
//! each with paging, goto, and background literal search over the same
//! seams. The S9 disassembly viewer opens a recognised executable
//! container (or a signed manifest) into a summary page and
//! per-region paged disassembly, every decode running behind the parser
//! sandbox through the [`view_disasm::Decode`] seam; `o` force-picks a
//! viewer, and an unrecognised file falls back to hex with a notice.
//! The remaining stages are staged in `plans/APPS.md`.
//!
//! # What this crate is
//!
//! A fully host-testable session built from seams:
//!
//! * [`fs::Fs`] — the directory-listing and free-space channel, and where
//!   an operation is started; the `Run` binary implements it over the
//!   kernel-authorised `fs_*` syscalls and the working directory, the tests
//!   over in-memory trees.
//! * `Tty` (from `tairix-curses`) — the terminal byte channel; an
//!   in-memory channel makes the whole session drivable without a kernel.
//! * `tairix_appdata::Settings` — this application's own app-data store,
//!   where the confirmation preferences live. It is gated on the
//!   kernel-attested bundle identity, so no other application the user
//!   launches can read or rewrite them, and the manager spells no store
//!   path of its own; the tests drive the shared fake service over the real
//!   `appdata-v1` codec.
//! * [`model::Model`] — the I/O-free state machine that [`render::render`]
//!   draws and [`app::run`] drives.
//!
//! # Module map
//!
//! * [`fs`] — the [`fs::Fs`] seam and its listing vocabulary.
//! * [`model`] — the tree/pane/sort/prompt state machine.
//! * [`ops`] — the file-operation planner and the operations it starts.
//! * [`tag`] — the tag set and the batch-operation driver.
//! * [`walk`] — the bounded branch walker (flattened view, disk usage,
//!   and the searches through its [`walk::Sieve`]).
//! * [`search`] — the streaming file-content scanner behind `F`.
//! * [`settings`] — the closed preference registry over the app-data
//!   store's open key namespace.
//! * [`view_text`] / [`view_hex`] — the Enter viewers: the streaming
//!   text pager and the hex dump, paging and searching through the same
//!   [`fs::Fs`] seam in bounded windows.
//! * [`view_disasm`] — the disassembly viewer: container summaries and
//!   per-region instruction windows, decoded only by the sandboxed
//!   decode service through the [`view_disasm::Decode`] seam.
//! * [`mod@render`] — the curses frame (panes, status, message, overlays).
//! * [`app`] — the key grammar and the session loop (walk ticks run on a
//!   timed input bound; every wait still parks in the kernel).
//!
//! # Layering & safety
//!
//! `no_std` (with `alloc`). It links only `lib/*` crates — the audited
//! `tairix-abi`, the OS-provided `tairix-curses`/`tairix-termcap`/
//! `tairix-vt`/`tairix-help`/`tairix-appdata`, and the sandbox seam
//! `tairix-sandbox` (with `tairix-binfmt` solely for magic-prefix routing)
//! — never a kernel or driver crate. No `unsafe`, and no `unwrap`/`expect`/`panic!` in
//! production paths; a refused listing fails closed onto the message
//! line.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::String;
use core::fmt;

use tairix_util::getopt::{Arg, Opt, Order, Refusal, Scan, Takes};
use tairix_util::quote::quote_if_needed;

pub mod app;
pub mod fs;
pub mod info;
pub mod model;
pub mod ops;
pub mod render;
pub mod search;
pub mod settings;
pub mod tag;
pub mod view_disasm;
pub mod view_hex;
pub mod view_text;
pub mod walk;

#[cfg(test)]
mod tests;

pub use app::{handle_event, publish_settings, run, FstreeError};
pub use fs::{Fs, FsEntry, VolumeInfo};
pub use info::{Info, NullInfo};
pub use model::{Model, Pane, SettingsState, SortKey};
pub use render::render;
pub use search::{ContentScan, Needle};
pub use settings::{SettingKey, Settings};
pub use tag::{Batch, BatchProgress, TagEntry, TagRange, TagSet};
pub use view_disasm::{Decode, DisasmView};
pub use view_hex::{HexPattern, HexView};
pub use view_text::{JobOutcome, TextView};
pub use walk::{FlatEntry, Sieve, WalkPurpose, WalkState, Walker};

/// The command word the bundle is installed under: the one spelling the help
/// lookup, the app-data defaults layer and the advisory records' producer
/// share. The app-data store itself is keyed on the bundle identity the
/// kernel attests, never on this.
pub const OWN_WORD: &str = "fstree";

/// What `fstree`'s command line asks for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// The short-help switches (`-h`, `-?`).
    Help,
    /// A session starting in this directory: the operand, or the root view
    /// `/` when none is named.
    Open(String),
}

/// A command line `fstree` does not understand.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UsageError {
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
    /// A second operand: a session starts in one directory.
    ExtraOperand(String),
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::ExtraOperand(operand) => {
                write!(f, "unknown argument: {}", quote_if_needed(operand))
            }
        }
    }
}

/// What `fstree`'s one option means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Help,
}

/// `fstree`'s options: its help switches alone.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::letter('h', Takes::Nothing, Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
];

/// Parse `args` (the tool's arguments, excluding the program name), read as
/// GNU's `getopt_long` reads it ([`Scan`]): the help switches answer at once
/// wherever they stand, `--` ends the options, and a second operand is
/// refused where it stands.
///
/// # Errors
///
/// * [`UsageError::Option`] — an option `getopt` refuses, in its words.
/// * [`UsageError::ExtraOperand`] — a second directory operand.
pub fn parse(args: &[&str]) -> Result<Command, UsageError> {
    let mut root: Option<String> = None;
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(UsageError::Option)? {
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            Arg::Operand(extra) if root.is_some() => {
                return Err(UsageError::ExtraOperand(String::from(extra)));
            }
            Arg::Operand(directory) => root = Some(String::from(directory)),
        }
    }
    Ok(Command::Open(root.unwrap_or_else(|| String::from("/"))))
}
