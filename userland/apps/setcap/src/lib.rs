//! TAIRiX `setcap` — set or clear a file's required-capability gate (Stage 6 `userland/apps/`).
//!
//! Every inode in the TAIRiX permission model may carry an **optional
//! capability requirement**: a capability the caller must hold to reach the
//! node at all, on top of the mode/ACL checks. `setcap`
//! changes that gate. The capability operand is either a canonical `CAP_*`
//! name (e.g. `CAP_AUDIT_READ`), which sets the gate to that capability, or
//! the literal `-`, which clears the gate so the node has none. With `-R` a
//! directory operand is changed and then its contents are changed
//! recursively. This is the companion to [`getcap`](../getcap/index.html) and
//! a building block of the filesystem permission model.
//!
//! `setcap` is the policy-*writing* sibling of `chmod`/`chown`: it stores the
//! gate but makes no permission decision itself (the VFS
//! is the policy point). Setting a gate is itself a privileged operation; the
//! filesystem seam refuses an attempt the caller is not authorised to make
//! (it surfaces as [`SetcapError::Apply`]).
//!
//! # What this crate is
//!
//! A **capability-gate-setting engine**, not a data source. For one parsed
//! [`Command`] it opens each operand through its final link, sets the new
//! gate through that descriptor, and walks each directory `-R` must descend.
//! The operations that touch the outside world are the injected seams:
//!
//! * [`Roots`](tairix_fscopy::tree::Roots) — the shared tree seam: each
//!   operand opened through its final link, each tree walked through the
//!   directories the run holds open and never through a link.
//! * [`Gates`] — set the gate of a node the run holds.
//! * [`Output`] — write the usage banner to the terminal.
//!
//! No binary ships it yet: the kernel has no call that sets a node's gate
//! (plans/APPS.md §12.1), so the engine runs against `tairix_fscopy`'s
//! in-memory tree in its tests, every parsing, cap-spec, and recursion
//! decision testable without a kernel.
//!
//! # Fail closed
//!
//! An option `getopt` refuses ([`SetcapError::Option`]) or a missing
//! operand ([`SetcapError::Usage`]) changes nothing; a capability operand
//! that is neither a known `CAP_*` name
//! nor `-` is a [`SetcapError::BadCapability`]; an operand that cannot be
//! opened surfaces the underlying [`Errno`](tairix_abi::Errno) as
//! [`SetcapError::Stat`], and one that is a link naming nothing is
//! [`SetcapError::Dangling`]; a gate that cannot be applied is
//! [`SetcapError::Apply`]; a directory that cannot be opened or listed
//! during a recursive descent is [`SetcapError::Read`]. A file that cannot
//! be opened, changed or listed is reported and the rest are still changed,
//! the rest of its directory too, and the run exits 1; a link met during a
//! recursive descent is left alone. There is no panic.
//!
//! # Module map
//!
//! * [`error`] — [`SetcapError`], the outcomes of [`run`].
//! * [`command`] — the [`Command`] shape and the [`parse`]r (incl.
//!   [`parse_capability`]).
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
pub use command::{parse, parse_capability, Command};
pub use error::SetcapError;
pub use io::{Gates, Output};
