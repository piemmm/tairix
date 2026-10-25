//! TAIRiX `getcap` — report a file's required-capability gate (Stage 6 `userland/apps/`).
//!
//! Every inode in the TAIRiX permission model may carry an **optional
//! capability requirement**: a capability the caller must hold to reach the
//! node at all, on top of the mode/ACL checks. `getcap`
//! reports that gate. For each file operand it prints one line —
//! `path CAP_NAME` — when the file carries a gate, and prints nothing for a
//! file that has none (so a clean tree is silent, the model `getcap` follows
//! on POSIX systems). With `-R` a directory operand is reported and then its
//! contents are reported recursively.
//!
//! # What this crate is
//!
//! A **capability-gate reporter**, not a policy point. For one parsed
//! [`Command`] it opens each operand through its final link, reads its gate
//! through that descriptor, renders the gated files, and walks each
//! directory `-R` must descend. The driver only *reports* the stored gate;
//! `getcap` makes no permission decision (the VFS is the policy point). The
//! operations that touch the outside world are the injected seams:
//!
//! * [`Roots`](tairix_fscopy::tree::Roots) — the shared tree seam: each
//!   operand opened through its final link, each tree walked through the
//!   directories the run holds open and never through a link.
//! * [`Gates`] — read the gate of a node the run holds.
//! * [`Output`] — write the report (and the usage banner) to the terminal.
//!
//! No binary ships it yet: the kernel has no call that reads a node's gate
//! (plans/APPS.md §12.1), so the engine runs against `tairix_fscopy`'s
//! in-memory tree in its tests, every parsing, rendering, and recursion
//! decision testable without a kernel.
//!
//! # Fail closed
//!
//! An option `getopt` refuses ([`GetcapError::Option`]) or a missing
//! operand ([`GetcapError::Usage`]) reports nothing; an operand that cannot
//! be opened surfaces the underlying [`Errno`](tairix_abi::Errno) as
//! [`GetcapError::Stat`], and one that is a link naming nothing is
//! [`GetcapError::Dangling`]; a gate that cannot be read is
//! [`GetcapError::Query`]; a directory that cannot be opened or listed
//! during a recursive descent is [`GetcapError::Read`]; a failed write is
//! [`GetcapError::Output`]. A file that cannot be opened, queried or listed
//! is reported and the rest are still reported, the rest of its directory
//! too, and the run exits 1; a link met during a recursive descent is left
//! alone. There is no panic.
//!
//! # Module map
//!
//! * [`error`] — [`GetcapError`], the outcomes of [`run`].
//! * [`command`] — the [`Command`] shape and the [`parse`]r.
//! * [`io`] — the [`Gates`] seam and the [`Output`] stream.
//! * [`client`] — the [`run`] entry point and the recursive engine.
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
pub use command::{parse, Command};
pub use error::GetcapError;
pub use io::{Gates, Output};
