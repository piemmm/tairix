//! TAIRiX `chmod` — change file mode bits (Stage 6
//! `userland/apps/`).
//!
//! `chmod` applies a mode to each of its file operands. The mode is either an
//! absolute octal value (`644`, `0755`, …) that replaces the permission bits
//! outright, or a comma-separated list of symbolic clauses (`g+w`, `o-rx`,
//! `a=rx`, `u+s`, `g=u`, `=644`) that transform the file's current bits, read
//! by the one grammar `tairix_util::mode` holds; a clause naming no field
//! spares the bits the process's umask holds. With `-R` a directory operand
//! is changed and then its contents are changed recursively. This is the
//! POSIX model.
//!
//! # What this crate is
//!
//! A **mode-changing engine**, not a data source. For one parsed [`Command`]
//! it opens each operand through its final link, reads its kind and current
//! mode, computes the new mode and sets it through that same descriptor, and
//! walks each directory `-R` must descend. The operations that touch the
//! outside world are the injected seams:
//!
//! * [`Roots`](tairix_fscopy::tree::Roots) — the shared tree seam: each
//!   operand opened through its final link, each tree walked through the
//!   directories the run holds open, each entry opened by name from its own
//!   directory and never through a link.
//! * [`Output`](tairix_util::output::Output) — write the short help and
//!   `-v`/`-c` reports to the terminal.
//! * `HelpSource` (from `lib/help`) — the tool's own bundle `Help/` tree,
//!   rendered by the `-h`/`-?`/`--help` switches through the one shared
//!   engine (never embedded help text).
//!
//! The process's umask, which a clause naming no field spares, is handed to
//! [`run`] as a value. The binary that ships as `chmod` wires the running
//! system's tree seam and the console; tests wire `tairix_fscopy`'s
//! in-memory tree, so every parsing, mode-algebra, and recursion decision is
//! testable without a kernel.
//!
//! # Fail closed
//!
//! An option `getopt` refuses ([`ChmodError::Option`]) or a missing operand
//! ([`ChmodError::Usage`]) changes nothing; a mode operand that is neither
//! octal nor symbolic is a [`ChmodError::BadMode`] naming it; an operand that
//! cannot be opened or read surfaces the underlying
//! [`Errno`](tairix_abi::Errno) as [`ChmodError::Stat`]; a mode that cannot
//! be applied is [`ChmodError::Apply`]; a directory that cannot be opened or
//! listed during a recursive descent is [`ChmodError::Read`]; a link operand
//! naming nothing is [`ChmodError::Dangling`]. Each is reported and the rest
//! are still changed, the rest of its directory too, and the run exits 1. A
//! link operand changes what it names; a link met during a recursive descent
//! — listed as one, or swapped in since — is left alone with what it names.
//! There is no panic.
//!
//! # Module map
//!
//! * [`error`] — [`ChmodError`], the outcomes of [`run`].
//! * [`command`] — the [`Command`] shape and its [`parse`]r.
//! * [`client`] — the [`run`] entry point and the recursive engine.
//!
//! # Layering & safety
//!
//! `no_std` (with `alloc`); the dependencies are `lib/*` crates — `lib/abi`,
//! the shared `lib/fscopy` tree seam, the shared `lib/help` engine and
//! `lib/util` — so this userland tool never links a kernel or driver crate.
//! No `unsafe`, and no `unwrap`/`expect`/`panic!` in production paths.

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

extern crate alloc;

pub mod client;
pub mod command;
pub mod error;

pub use client::{run, USAGE};
pub use command::{parse, Command, Options, Verbosity};
pub use error::ChmodError;
