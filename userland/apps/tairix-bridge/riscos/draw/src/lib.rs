//! TAIRiX Bridge's Draw and DrawFile modules (`plans/RISCOS-EMULATOR.md`
//! §8): paths processed, filled and stroked as RISC OS 5.30's Draw module
//! does each, held to what it was recorded doing.
//!
//! - [`MODULES`]: the built-in modules this crate provides.
//! - [`pixels`]: Draw's fill of a path another module holds, its pixels
//!   handed back.
//! - `fixture` (test support): Draw files made to order.
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

mod dash;
mod drawfile;
mod fill;
#[cfg(any(test, feature = "test-support"))]
pub mod fixture;
mod flatten;
mod geometry;
mod module;
mod path;
pub mod pixels;
mod process;
mod raster;
mod read;
mod sink;
mod stage;
mod thicken;

use tairix_riscos_kernel::module::Builtin;

/// The built-in modules this crate provides, for the session's
/// `Config::modules`.
pub static MODULES: [&Builtin; 2] = [&module::DRAW, &drawfile::DRAW_FILE];
