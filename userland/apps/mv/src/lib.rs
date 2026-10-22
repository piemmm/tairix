//! TAIRiX `mv` — move (rename) files and directories (Stage 6 `userland/apps/`).
//!
//! `mv` relocates each of its source operands to a destination. With a single
//! source and a destination that does not name a directory, the source is
//! moved to that exact path. When the destination names an existing
//! directory — and always when there is more than one source — each source is
//! moved *into* that directory under its own base name. Unlike `cp`, a
//! directory source needs no flag: a directory is moved like any other
//! operand. This is the POSIX model.
//!
//! # What this crate is
//!
//! A **move engine**, not a data source. For one parsed [`Command`] it asks
//! the injected filesystem seam to rename each source onto its destination.
//! A rename within one filesystem is atomic and is the whole operation. A
//! rename that would cross a filesystem boundary reports
//! [`RenameOutcome::CrossDevice`], and the engine moves the source across so
//! that it still replaces the destination in one step: it builds the copy
//! under a staging name beside the destination, puts it on the medium,
//! renames it into place, and removes the source last. It keeps the rename's
//! own rules there too — a directory replaces only an empty directory,
//! nothing else replaces a directory, and no directory moves into its own
//! subtree. The operations that touch the outside world are the injected
//! seams:
//!
//! * [`FileSystem`] — learn a path's kind, owner and canonical spelling,
//!   rename a path, read a file's bytes and a directory's entries, build and
//!   commit a replacement, and remove the source.
//! * [`Output`] — write the usage banner and the `-v` reports.
//!
//! The binary that ships as `mv` wires the real syscall-backed filesystem and
//! console output; tests wire in-memory fixtures. This is the seam discipline
//! of the other userland crates (`init`'s `Spawner`/`Reaper`, `login`'s
//! `Prompt`, `sysinfo`'s `Transport`, `cat`'s `FileSource`, `ls`'s `Listing`,
//! `rm`'s `Removal`, `cp`'s `FileSystem`), and it keeps every parsing,
//! routing, and fallback decision testable without a kernel.
//!
//! # Fail closed
//!
//! An unknown option, a missing operand, or more than one source aimed at a
//! non-directory destination is a [`MvError::Usage`] that moves nothing.
//! Every other failure names the paths it concerns, quoted, and keeps the
//! source: a move across volumes that fails before its rename leaves the
//! destination as it was and removes its staged copy, and only a source that
//! cannot be removed after its move is [`MvError::Remove`]. The first failure
//! stops the run before any later operand, and there is no panic.
//!
//! # Module map
//!
//! * [`error`] — [`MvError`], the outcomes of [`run`].
//! * [`command`] — the [`Command`] shape and its [`parse`]r.
//! * [`io`] — the [`FileSystem`] and [`Output`] seams and the
//!   [`Entry`]/[`EntryKind`]/[`Owner`]/[`RenameOutcome`] data they carry.
//! * [`client`] — the [`run`] entry point and the move engine.
//!
//! # Layering & safety
//!
//! `no_std` (with `alloc`), built on `lib/*` crates alone, so this userland
//! tool never links a kernel or driver crate. No `unsafe`, and no
//! `unwrap`/`expect`/`panic!` in production paths.

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

extern crate alloc;

pub mod client;
pub mod command;
pub mod error;
pub mod io;

pub use client::{run, Seams, USAGE};
pub use command::{parse, Clobber, Command, Options, TargetMode};
pub use error::MvError;
pub use io::{
    Ancestry, AttributeStore, Entry, EntryKind, FileSystem, Output, Owner, PathFiles, Prompt,
    RenameOutcome,
};
