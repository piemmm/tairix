//! The `Run` binary of the USB Audio Class driver; the program itself is
//! `program.rs`, built for bare-metal targets only.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program;

/// The host build's inert stand-in, so the workspace's host tooling covers
/// the crate.
#[cfg(not(freestanding))]
fn main() {}
