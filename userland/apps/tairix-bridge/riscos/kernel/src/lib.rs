//! TAIRiX Bridge's kernel (`plans/RISCOS-EMULATOR.md` §3, §4).
//!
//! - [`os`]: the core — the processors, guest calls, the run, and [`os::Os`],
//!   the view every SWI and module works through.
//! - [`memory`]: the guest address space: its two layouts, the memory
//!   policy, task slots and the dynamic-area manager, over the processor's
//!   MMU.
//! - [`swi`]: SWI numbers, names and dispatch.
//! - [`module`]: modules: the seam built-in modules join through, the
//!   inventory, and the module chain.
//! - [`context`]: the session's contexts, each a program's own processors,
//!   memory, input and screen, and the switches between them.
//! - `cli`: the command line interpreter and the kernel's `*` commands.
//! - [`corpus`]: the recorded observation corpus's scenarios and fixtures.
//!
//! The rest are the kernel's subsystems, each a part of [`os::Os`]:
//! vectors, services, events, callbacks, the clock, heaps, system variables,
//! the environment, character input and output, `OS_Byte` and `OS_Word`,
//! CMOS, the information and memory SWIs, conversions, text and exceptions.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

mod amb;
pub mod buffers;
pub mod byte;
mod callaswi;
mod callbacks;
mod cli;
pub use cli::table::parameters as command_parameters;
mod clock;
pub mod cmos;
pub mod context;
mod convert;
pub mod desktop;
pub use convert::Decimal;
pub mod corpus;
mod dictionary;
mod dynamic;
mod env;
pub use env::{hex, hex8, ExceptionKind, Hex, Registers};
pub mod error;
mod events;
mod extension;
pub mod filing;
mod fpemulator;
pub use fpemulator::FRESH_FPSR;
pub mod heap;
mod host;
mod info;
pub mod input;
mod international;
pub use international::{character as alphabet_character, ALPHABETS, UTF8 as UTF8_ALPHABET};
mod intkey;
mod keyboard;
mod kheap;
pub mod listing;
pub mod memory;
pub mod module;
pub mod mouse;
pub mod msgtrans;
pub mod os;
pub mod output;
pub mod ramdisc;
pub mod relay;
pub mod release;
pub mod resourcefs;
mod resources;
pub mod service;
mod squash;
pub mod swi;
mod system;
mod territory;
mod text;
mod traps;
mod vars;
pub mod vectors;
mod vfpsupport;
pub mod word;
mod zeropage;

/// Every request the unit tests make, metered per thread, so a test can
/// refuse a call the room it asks for.
#[cfg(test)]
#[global_allocator]
static ALLOC: tairix_fuzzseed::meter::Metered = tairix_fuzzseed::meter::Metered;

#[cfg(test)]
mod testkit;
