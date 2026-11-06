//! TAIRiX `label` — show or change a volume's title, the label its
//! filesystem records.
//!
//! `label NAME` prints the title of the volume `NAME` names; `label NAME
//! TITLE` asks the kernel to set it, an empty `TITLE` clearing it. `NAME` is
//! read as `unmount` reads one, by the one resolution both share
//! ([`tairix_procinfo::named_volume`]).
//!
//! The tool resolves and presents; it holds no authority. The kernel's
//! `volume_retitle` path decides who may retitle the volume — a disk image's
//! owner, the mount authority over a device's volume, the user at the seat
//! over a removable one whose root they may write — and audits every
//! decision; this tool hands it the volume's key and the title through the
//! injected [`Retitler`] seam and reports its answer.
//!
//! # Fail closed
//!
//! An option `getopt` refuses, or a number of operands other than one or two,
//! is a usage error. A title is checked before anything is asked, a name
//! naming no volume is a typed error, and a refusal surfaces the kernel's own
//! [`Errno`](tairix_abi::Errno). There is no panic.
//!
//! # Module map
//!
//! * [`error`] — [`LabelError`], the outcomes of [`run`].
//! * [`command`] — the [`Command`] shape and the [`parse`]r.
//! * [`io`] — the [`Retitler`] and [`Output`] seams.
//! * [`client`] — the [`run`] engine.

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

extern crate alloc;

pub mod client;
pub mod command;
pub mod error;
pub mod io;

pub use client::run;
pub use command::{parse, Command};
pub use error::LabelError;
pub use io::{Output, Retitler};
