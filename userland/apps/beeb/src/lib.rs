//! `beeb.app`: a BBC Micro Model B, cycle-exact, and the clean-room ROMs it
//! runs (`plans/BEEB.md`).
#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod cpu;
pub mod machine;
pub mod media;
#[cfg(not(freestanding))]
pub mod rom;
pub mod snapshot;
