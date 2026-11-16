//! The clean-room 1541 DOS (`plans/C64.md` §5): the interface on the bus,
//! the command and error channels, the files and directory on the disk,
//! and the controller beneath them.

mod bam;
mod blocks;
mod bus;
mod channels;
mod commands;
mod controller;
mod errors;
mod files;
mod format;
pub(crate) mod gcr;
mod listing;
mod memory;
mod rel;
mod reset;
mod writing;
pub(crate) mod ws;

#[cfg(test)]
mod tests;

use crate::cpu::NMI_VECTOR;
use crate::machine::c64::drive::{self, DOS_BASE, DOS_BYTES};
use crate::rom::asm::{Asm, AsmError, Image};

/// The DOS ROM.
///
/// # Errors
///
/// The builder's first [`AsmError`].
pub fn build() -> Result<Image, AsmError> {
    let mut a = Asm::new(DOS_BASE, DOS_BYTES).with_quiet_pages(drive::quiet_read_page);
    // NMI, which the drive never raises, then reset and IRQ.
    a.pin(NMI_VECTOR, |a| {
        a.word("reset");
        a.word("reset");
        a.word("irq");
    });
    reset::emit(&mut a);
    bus::emit(&mut a);
    channels::emit(&mut a);
    commands::emit(&mut a);
    controller::emit(&mut a);
    errors::emit(&mut a);
    files::emit(&mut a);
    listing::emit(&mut a);
    writing::emit(&mut a);
    bam::emit(&mut a);
    blocks::emit(&mut a);
    memory::emit(&mut a);
    rel::emit(&mut a);
    format::emit(&mut a);
    gcr::emit(&mut a);
    a.place_later();
    a.finish()
}
