//! The RISC OS emulator's ARM processor (`plans/RISCOS-EMULATOR.md` §2).
//!
//! - [`cpu`]: the processor, stepping through a [`bus::Bus`] under a
//!   [`profile::Profile`].
//! - [`instruction`]: the vocabulary every instruction set decodes to, and
//!   the assembler's text for it.
//! - [`a32`] and [`t32`]: each instruction set's one encoding table, and
//!   the decoder and encoder derived from it.
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
mod encoding;
pub mod instruction;
pub mod isa;
pub mod profile;
pub mod psr;
pub mod reg;
pub mod t32;
pub mod vectors;
