//! The RISC OS emulator's ARM processor (`plans/RISCOS-EMULATOR.md` §2).
//!
//! - [`cpu`]: the processor, stepping through a [`bus::Bus`] under a
//!   [`profile::Profile`].
//! - [`a32`]: the one A32 encoding table, and the decoder, encoder and
//!   assembler's text derived from it.
//! - [`isa`]: the architecture features an encoding needs.
//! - [`psr`], [`reg`] and [`cond`]: the programmer's model's vocabulary.
//! - [`vectors`]: the conformance vectors' encoding and the runner that
//!   holds a processor to them.
//!
//! The crate knows nothing of RISC OS: it is a processor, and the session
//! above it decides what its memory, its SWIs and its host calls mean.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod a32;
pub mod bus;
pub mod cond;
mod cp15;
pub mod cpu;
pub mod isa;
pub mod profile;
pub mod psr;
pub mod reg;
pub mod vectors;
