//! TAIRiX Bridge's VDU drivers (`plans/RISCOS-EMULATOR.md` §8): RISC OS
//! 5's screen modes and screen memory, the VDU stream, `OS_Plot`, colours
//! and palettes, the pointer and the `ScreenModes` module, held to what
//! genuine RISC OS 5.30 was recorded doing.
//!
//! - [`DRIVERS`]: the drivers, as the kernel reaches them.
//! - [`MODULES`]: the built-in modules this crate provides.
//! - [`display`]: what a front end shows of a screen.
//! - [`corpus`]: the recorded screen programs and their answers.
//!
//! The drivers keep one screen's state, the session's, as a kernel
//! extension. What software reads in zero page — the colour blocks, the ECF
//! offsets and the changed box — is kept there, where it reads it.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

mod arc;
mod byte;
mod canvas;
mod changed;
mod colour;
pub mod corpus;
pub mod display;
mod font;
mod frames;
mod hline;
mod mode;
mod modestring;
mod palette;
mod plot;
mod pointer;
mod raster;
mod screen;
mod screenmodes;
mod select;
mod state;
mod stream;
mod swis;
mod teletext;
mod text;
mod vars;
mod word;

use tairix_riscos_kernel::module::{Builtin, VectorDefault};
use tairix_riscos_kernel::output::VduDrivers;

/// The VDU drivers, for the session's `Config::vdu`.
pub const DRIVERS: VduDrivers = VduDrivers {
    start: select::start,
    write: stream::write,
    queued: stream::queued,
    swi: Some(swis::perform),
    byte: Some(byte::reason),
    word: Some(word::reason),
    frames: frames::frames,
    deadline: frames::deadline,
    edit,
    monitor: select::monitor,
    set_monitor: select::set_monitor,
    origin,
    routines: &[hline::routine, screen::area_handler],
    vectors: &[VectorDefault {
        vector: palette::PALETTE_V,
        code: palette::vector,
    }],
};

/// The routines' places among [`VduDrivers::routines`].
mod routine {
    /// `HLineAddr`'s routine.
    pub(crate) const HLINE: u32 = 0;
    /// The screen area's handler.
    pub(crate) const SCREEN_AREA: u32 = 1;
}

/// The built-in modules this crate provides, for the session's
/// `Config::modules`.
pub static MODULES: [&Builtin; 1] = [&screenmodes::SCREEN_MODES];

/// A copy-cursor editing key.
fn edit(
    os: &mut tairix_riscos_kernel::os::Os<'_>,
    key: u8,
) -> Result<Option<u8>, tairix_riscos_kernel::os::Fail> {
    state::with(os, |vdu, os| vdu.edit_key(os, key))
}

/// The graphics origin, in OS units from the screen's bottom left.
fn origin(os: &mut tairix_riscos_kernel::os::Os<'_>) -> (i32, i32) {
    state::read(os, |vdu, _| vdu.graphics.origin).unwrap_or((0, 0))
}
