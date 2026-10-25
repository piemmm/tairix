//! TAIRiX `seq` — print a sequence of numbers (`plans/APPS.md` §12.1
//! Stage C).
//!
//! The GNU coreutils `seq`: print the numbers from FIRST to LAST in steps
//! of INCREMENT (both defaulting to 1), with the GNU option surface —
//! `-f`/`--format` (a printf-style floating-point format), `-s`/
//! `--separator`, and `-w`/`--equal-width` — and GNU 9.4's output rules,
//! established by running it: the default precision and `-w` width come
//! from how the operands are spelt, and a value rounding carried just past
//! LAST still prints when it reads back as LAST.
//!
//! # What this crate is
//!
//! The pure, host-testable core of the tool:
//!
//! * [`parse`] — the [`Command`] a command line names, with GNU `seq`'s
//!   option rules: no permutation (the first operand ends option
//!   scanning), a leading negative number is an operand, and the operand
//!   count, `-f` format, and `-f`/`-w` conflict are validated in the GNU
//!   diagnostic order.
//! * [`run`] — the engine: each operand's value and [`Layout`]
//!   ([`scan_operand`]), a sequence of integers computed exactly at any
//!   size ([`exact`]), any other through `f64` and the [`Format`]
//!   renderer (`%e`/`%f`/`%g`/`%a`, C-locale semantics), all written
//!   through the injected [`Output`].
//!
//! Three deliberate divergences from GNU. Its floating-point path computes
//! in C `long double`, TAIRiX in `f64`, the `double` the format contract
//! names: only a non-integer sequence needing more than 53 bits prints
//! differently, and `%a` spells the C `double` form (`0x1.8p+0`) where
//! glibc's `%La` writes `0xcp-3`. Its integers are exact only on a narrow
//! path (non-negative decimal spellings, small steps, no `-w`) and
//! otherwise printed from `long double`, wrongly past its 64 bits; TAIRiX
//! computes every sequence of integers exactly. And GNU lays out an
//! uppercase hexadecimal exponent (`0X1P-1`) as an integer, so
//! `seq 0X1P-1 3` prints `0 1 2 3` and `seq 0 0X1P-1 5` never ends; TAIRiX
//! reads it as C does, as it reads `0x1p-1`.
//!
//! # Layering & safety
//!
//! `no_std` (with `alloc`); it depends only on `lib/help`, for the
//! short-help switches, and `lib/util`'s C-locale number grammar and
//! rendering. No `unsafe`, and no
//! `unwrap`/`expect`/`panic!` in production paths; nothing writes to fd 3
//! (`stdinfo` — a full sequence is never an omission or summary).
//!
//! The bundle's `Help/` documents are **not** embedded in this crate:
//! they are authored once in the bundle's on-disk `Help/` tree, planted
//! onto `/System` by the image builder from that source (`tools/syshelp`),
//! and read back at runtime through the injected [`tairix_help::HelpSource`]
//! seam. Help is never hardcoded into the program (`plans/APPS.md`).

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod client;
pub mod command;
pub mod error;
pub mod exact;
pub mod format;
pub mod number;

pub use client::{run, Output, USAGE};
pub use command::{parse, Command, Job};
pub use error::SeqError;
pub use format::Format;
pub use number::{scan_operand, Layout, Operand};
