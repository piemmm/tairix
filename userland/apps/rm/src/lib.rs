//! TAIRiX `rm` — remove files and directories (Stage 6
//! `userland/apps/`).
//!
//! `rm` removes each of its operands in order. A non-directory operand — a
//! regular file, or a symbolic link as itself, never followed — is removed
//! directly. A directory operand is removed only with `-r`, which removes its
//! contents first, depth-first, and then the directory itself; without `-r`,
//! naming a directory is a [`RmError::IsDirectory`]. With `-f` a non-existent
//! operand is skipped rather than reported. This is the POSIX model.
//!
//! # What this crate is
//!
//! A **removal engine**, not a data source. For one parsed [`Command`] it
//! describes each operand, walks each directory it must descend, and removes
//! every reachable object. The operations that touch the outside world are
//! the injected seams:
//!
//! * [`Roots`](tairix_fscopy::tree::Roots) — the shared tree seam: each
//!   operand reached by name in the directory holding it, each tree walked
//!   through the directories the run holds open and never through a link,
//!   and each directory removed only while its name still holds the node
//!   the walk emptied.
//! * [`Prompt`] — ask a confirmation question, and say whether its replies
//!   come from a terminal.
//! * [`Output`] — write the usage banner and the `-v` reports.
//!
//! The binary that ships as `rm` wires the running system's tree seam and
//! the console; tests wire `tairix_fscopy`'s in-memory tree. This is the seam
//! discipline of the other userland crates, and it keeps every parsing,
//! recursion, and force decision testable without a kernel.
//!
//! # Fail closed
//!
//! An option `getopt` refuses ([`RmError::Option`]) and a missing operand
//! ([`RmError::Usage`]) remove nothing; a directory named without `-r` is a
//! [`RmError::IsDirectory`]; under `-r` or `-d` an operand whose last
//! component is `.` or `..` is [`RmError::Dot`], refused before anything is
//! looked up; an operand that cannot be inspected surfaces the underlying
//! [`Errno`](tairix_abi::Errno) as [`RmError::Stat`] (unless `-f` makes a
//! [`NotFound`](tairix_abi::Errno::NotFound) a no-op); a directory that
//! cannot be enumerated, and is not empty, is [`RmError::Read`]; a removal
//! that fails — a directory's included, once its name no longer holds the
//! node emptied — is [`RmError::Remove`]; a recursive removal of the view
//! root, however spelled, is [`RmError::PreserveRoot`]. Each is reported and
//! the rest are still removed, as GNU's are; a directory left holding
//! something is kept without a word, and the run exits 1. There is no panic.
//!
//! # Module map
//!
//! * [`error`] — [`RmError`], the outcomes of [`run`].
//! * [`command`] — the [`Command`] shape and its [`parse`]r.
//! * [`io`] — the [`Prompt`] seam and the [`Output`] stream.
//! * [`client`] — the [`run`] entry point and the recursive removal engine.
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

pub use client::{run, USAGE};
pub use command::{parse, Command, Interactive, Options};
pub use error::RmError;
pub use io::{Output, Prompt};
