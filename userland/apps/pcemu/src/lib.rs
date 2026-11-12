//! The engine of `PCEmulator.app`: an IBM PC/XT-class machine built around
//! the 8088, 8086, NEC V20 and V30, and Intel 80188 and 80186
//! (`plans/PCEMU.md`).
//!
//! Everything here is pure computation over guest state, so the whole
//! machine is tested on the host. Guest code and guest data are untrusted:
//! every path through the engine is total over them. The one `unsafe` is
//! the entry into translated code (`cpu::jit::native`).

#![no_std]
#![deny(unsafe_code)]
#![forbid(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

extern crate alloc;

// The unit tests refuse the heap at chosen requests.
#[cfg(test)]
#[global_allocator]
static ALLOCATOR: tairix_fuzzseed::meter::Metered = tairix_fuzzseed::meter::Metered;

pub mod app;
pub mod asm;
pub mod bios;
pub mod bits;
pub mod board;
pub mod chips;
pub mod cpu;
pub mod debug;
pub mod digest;
pub mod dos;
pub mod ems;
pub mod folder;
pub mod fpu;
pub(crate) mod frames;
pub mod front;
pub mod link;
pub mod machine;
pub mod ports;
#[cfg(freestanding)]
pub mod roles;
pub mod snapshot;
pub mod sound;
pub mod storage;
pub mod time;
#[cfg(any(test, feature = "vectors"))]
pub mod vectors;
pub mod video;
pub mod worker;
