//! TAIRiX `chown` — change file owner and group, with GNU's command line.
//!
//! Each file operand is given the owner and group an `[OWNER][:[GROUP]]`
//! operand names, or those of a `--reference` file. Names are read against
//! the account and group directories, a number taken only when no name
//! matches. Every change is made through the descriptor of the node its
//! current owner was read from, and `-R` walks a tree through the directories
//! it holds (`tairix_fscopy::tree`), so no entry swapped for a link meanwhile
//! can carry a change out of the tree.
//!
//! # Module map
//!
//! * [`command`] — the [`Command`] a command line asks for, and its [`parse`]r.
//! * [`owner`] — the owner operand's grammar and its [`resolve`]r.
//! * [`io`] — the [`Accounts`] seam names are read through.
//! * [`client`] — [`run`]: operands opened, trees walked, changes reported.
//! * [`error`] — [`ChownError`].
//!
//! `no_std` with `alloc`, no `unsafe`, and no `unwrap`/`expect`/`panic!` in
//! production paths.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod client;
pub mod command;
pub mod error;
pub mod io;
pub mod owner;

pub use client::{run, USAGE};
pub use command::{parse, Command, Links, Options, Target, Traverse, Verbosity};
pub use error::ChownError;
pub use io::{Account, Accounts, Output};
pub use owner::{resolve, Owner, Resolved};
