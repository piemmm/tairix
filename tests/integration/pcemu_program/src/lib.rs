//! The `pcemucheck` fixture of the PC emulator's QEMU verticals
//! (`plans/PCEMU.md` PC21). Run from the scripted root shell on every native
//! target, it holds the emulator to what no host test can show: the reference
//! run ([`tairix_pcemu::digest`]) computes the digest every target asserts
//! under this target's compiler, and the default machine boots DOS in its
//! sandboxed worker and runs a program from a diskette: its hot loop is
//! translated in a compiler sandbox, installed into the worker with
//! `code_install` and run natively, and the sum it prints is right.
//!
//! A pass prints [`PASS_MARKER`] and exits `0`. A failure prints its reason
//! and parks rather than exiting: the vertical ends on this program's audited
//! exit, so a failed check times the run out with the reason in the
//! transcript.
//!
//! The program, its diskette and what a screen must show are pure and
//! host-tested here; `src/run.rs` runs the machine.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

#[cfg(feature = "checks")]
extern crate alloc;

#[cfg(feature = "checks")]
mod checks;

#[cfg(feature = "checks")]
pub use checks::{answered, at_prompt, diskette, pass_line, sum_line, Unmade, TYPED};

/// The command word: the bundle's name, the word the vertical's script
/// types, and the `comm` the kernel attests on the fixture's audited calls.
pub const COMMAND: &str = "pcemucheck";

/// The line the fixture prints once every check passed.
pub const PASS_MARKER: &str = "PCEMU PASS";

/// Bytes at the end of the machine's share, past its layout, where the
/// fixture's worker says how its translated code ran: a byte set once a block
/// has run natively, then how many have, little-endian.
pub const TAIL: usize = 1 + 8;
