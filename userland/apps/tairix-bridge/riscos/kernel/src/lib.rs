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
pub mod byte;
mod callbacks;
mod cli;
pub use cli::table::parameters as command_parameters;
mod clock;
pub mod cmos;
mod convert;
pub mod corpus;
mod dictionary;
mod dynamic;
mod env;
pub use env::{hex, hex8, Hex};
pub mod error;
mod events;
mod extension;
pub mod filing;
pub mod heap;
mod host;
mod info;
pub mod input;
mod international;
mod intkey;
mod keyboard;
mod kheap;
pub mod memory;
pub mod module;
pub mod mouse;
pub mod msgtrans;
pub mod os;
pub mod output;
pub mod ramdisc;
pub mod release;
mod resourcefs;
mod resources;
pub mod service;
pub mod swi;
mod system;
mod territory;
mod text;
mod traps;
mod vars;
pub mod vectors;
pub mod word;
mod zeropage;

#[cfg(test)]
mod testkit;
