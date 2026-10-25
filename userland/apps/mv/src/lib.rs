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
//! A **move engine**, not a data source. For one parsed [`Command`] it renames
//! each source onto its destination through the directories that hold them,
//! opened from the working directory through the shared tree seam
//! ([`Roots`]). A rename within one filesystem is atomic and is the whole
//! operation. A rename that would cross a filesystem boundary is refused
//! with `CrossVolume`, and the engine moves the source across so that it
//! still replaces the destination in one step: it builds the copy owner-only
//! under a staging name beside the destination, fills it through its own
//! descriptor, puts it on the medium, renames it into place only while that
//! name still holds it, and removes last only what it copied, each entry only
//! while it is unchanged. It keeps the rename's own rules there too — a
//! directory replaces only an empty directory, nothing else replaces a
//! directory, and no directory moves into its own subtree. The operations
//! that touch the outside world are the injected seams:
//!
//! * [`Roots`] — the working directory the operands are opened from, and the
//!   held directories and files the move goes through.
//! * [`Staging`] — the fresh name a copy across volumes is built under.
//! * [`Prompt`] — the `-i` question.
//! * [`Output`] — write the usage banner and the `-v` reports.
//!
//! The binary that ships as `mv` wires the running system's seams; tests
//! wire the shared in-memory fixture, whose attacker hook swaps names between
//! two calls, so every routing, fallback and race decision is testable
//! without a kernel.
//!
//! # Fail closed
//!
//! An option `getopt` refuses ([`MvError::Option`]), and a missing operand
//! or more than one source under `-T` ([`MvError::Usage`]), move nothing.
//! Every other failure names the paths it concerns, quoted, and keeps the
//! source: a move across volumes that fails before its rename leaves the
//! destination as it was and removes its staged copy while its name still
//! holds it, and after the rename a copied entry that cannot be removed is
//! [`MvError::Remove`] and one that changed meanwhile [`MvError::Changed`].
//! Each is reported and the next operand is still moved, the run exiting 1;
//! an operand's own move is all or nothing. There is no panic.
//!
//! # Module map
//!
//! * [`error`] — [`MvError`], the outcomes of [`run`].
//! * [`command`] — the [`Command`] shape and its [`parse`]r.
//! * [`io`] — the [`Staging`], [`Prompt`] and [`Output`] seams beside the
//!   shared tree seam.
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
pub use io::{Dir, Kind, Node, Output, Prompt, Roots, Staging};
