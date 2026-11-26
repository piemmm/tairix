//! TAIRiX Bridge's `SharedCLibrary` (`plans/RISCOS-EMULATOR.md` §10): the
//! library kernel and the C library as host calls over guest memory, their
//! interface held to what RISC OS 5.30's 6.23 was recorded doing.
//!
//! - [`MODULES`]: the built-in modules this crate provides.
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

mod atomic;
mod c;
mod c99low;
mod chunk;
mod client;
mod kernel;
mod lap;
mod libinit;
mod module;
mod stack_chunk;

use tairix_riscos_kernel::module::Builtin;

/// The built-in modules this crate provides, for the session's
/// `Config::modules`.
pub static MODULES: [&Builtin; 1] = [&module::SHARED_C_LIBRARY];
