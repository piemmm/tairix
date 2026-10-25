//! TAIRiX `cp` — copy files and directories (Stage 6
//! `userland/apps/`).
//!
//! `cp` copies each of its source operands to a destination. With a single
//! source and a destination that does not name a directory, the source is
//! copied to that exact path. When the destination names an existing
//! directory — and always when there is more than one source — each source is
//! copied *into* that directory under its own base name. A directory source
//! is copied only with `-r`, which reproduces the whole subtree; without `-r`
//! a directory operand is a [`CpError::IsDirectory`]. This is the POSIX
//! model.
//!
//! # Links, instead of bytes
//!
//! `-l` gives the destination a second *name* for the source's own node
//! rather than a copy of its bytes, so the two cannot diverge on a later
//! write; `-s` makes a symbolic link naming the source. They are
//! alternatives to copying rather than modifiers of it, so they are one
//! last-wins [`Contents`] state that cannot silently combine.
//!
//! `-P` reproduces a symbolic-link source as a link storing the same target,
//! verbatim — so a relative or dangling link survives the copy — instead of
//! following it. `--preserve=links` gives two sources naming one node two
//! names at the destination rather than two copies, keyed on the identity
//! every `fs_stat` and `fs_readdir` record carries; only a node whose name
//! count exceeds one is remembered, so the map holds the hard links a copy
//! meets rather than one entry per node on the volume. `-d` is exactly that
//! pair, spelled as the pair so its halves cannot drift.
//!
//! `-a` and the `--preserve` set other than `--preserve=links` are refused
//! ([`CpError::Unsupported`]): `-a` is `-dR --preserve=all`, and
//! `--preserve=all` includes timestamps no call can set here, so honouring
//! it would report a preservation that did not happen. `-dR` is the rest of
//! it and is available.
//!
//! # What this crate is
//!
//! A **copy engine**, not a data source. For one parsed [`Command`] it looks
//! at each operand, streams regular files from source to destination, and
//! walks each directory it must reproduce through the shared tree seam —
//! every entry reached by name from a source directory held open and copied
//! by name into a destination directory the copy made or opened, a directory
//! it made filled before it is given its mode. The operations that touch the
//! outside world are the injected seams:
//!
//! * [`FileSystem`] — the shared seam's [`Roots`] (operands opened from the
//!   working directory, and the directories and files held from them), and
//!   what `cp` needs beyond it: an operand read or written through its final
//!   link, a resolved spelling, and a link put in a name's place in one step.
//! * [`Prompt`] — the `-i` question.
//! * [`Output`] — write the usage banner to the terminal.
//!
//! The binary that ships as `cp` wires the running system's seams; tests wire
//! the shared in-memory tree, so every parsing, recursion, mode and force
//! decision is testable without a kernel.
//!
//! # Fail closed
//!
//! An option `getopt` refuses ([`CpError::Option`]), and a missing operand
//! or more than one source under `-T` ([`CpError::Usage`]), copy nothing; a
//! directory source named without `-r` is a [`CpError::IsDirectory`]; a
//! directory source whose destination already exists as a non-directory is a
//! [`CpError::Overwrite`]; a path that cannot be inspected is
//! [`CpError::Stat`], one that cannot be read [`CpError::Read`], a destination
//! that cannot be made [`CpError::Create`] and a failed write
//! [`CpError::Write`], each naming the path and the kernel's reason. Each is
//! reported and the rest are still copied, the rest of its directory too, and
//! the run exits 1. There is no panic.
//!
//! # Module map
//!
//! * [`error`] — [`CpError`], the outcomes of [`run`].
//! * [`command`] — the [`Command`] shape and its [`parse`]r.
//! * [`io`] — the [`FileSystem`], [`Prompt`] and [`Output`] seams and the
//!   [`Probe`] a look at a node carries.
//! * [`client`] — the [`run`] entry point and the recursive copy engine.
//!
//! # Layering & safety
//!
//! `no_std` (with `alloc`); it depends only on `lib/*` crates, so this
//! userland tool never links a kernel or driver crate. No `unsafe`, and no
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
pub use command::{parse, Clobber, Command, Contents, Dereference, Options, TargetMode};
pub use error::CpError;
pub use io::{Dir, FileSystem, FinalLink, Kind, Node, Output, Probe, Prompt, Roots};
