//! The clean-room DFS: a disc filing system equivalent to Acorn DFS 1.20 at
//! its documented interfaces and workspace, over the 8271 (`plans/BEEB.md`
//! §3).

mod catalogue;
mod channels;
mod commands;
mod copy;
mod disc;
mod errors;
mod files;
mod nmi;
mod service;
mod text;
mod utils;
mod ws;

#[cfg(test)]
mod tests;

use crate::machine::{ROM_SIZE, SIDEWAYS_BASE};
use crate::rom::asm::{lo, Asm, AsmError, Image};

/// A service ROM of 6502 code: a service entry and no language.
const ROM_TYPE: u8 = 0x82;
/// The ROM's version byte.
const VERSION: u8 = 1;
/// The title, the version string, and the copyright, this ROM's own.
const TITLE: &str = "DFS";
const VERSION_TEXT: &str = "1.20";
const COPYRIGHT: &str = "(C)2026TAIRiX";

/// Build the DFS image.
///
/// # Errors
///
/// The builder's first [`AsmError`].
pub fn build() -> Result<Image, AsmError> {
    let nmi = nmi::build()?;
    let mut a = Asm::new(SIDEWAYS_BASE, ROM_SIZE);
    header(&mut a);
    service::emit(&mut a);
    commands::emit(&mut a);
    catalogue::emit(&mut a);
    files::emit(&mut a);
    channels::emit(&mut a);
    copy::emit(&mut a);
    utils::emit(&mut a);
    text::emit(&mut a);
    disc::emit(&mut a, &nmi);
    errors::emit(&mut a);
    a.finish()
}

/// A conditional jump to a target a branch cannot reach: the opposite
/// branch over a JMP.
pub(super) fn far(a: &mut Asm, unless: fn(&mut Asm, crate::rom::asm::Label), target: &'static str) {
    let skip = a.local();
    unless(a, skip);
    a.jmp(crate::rom::asm::abs(target));
    a.bind(skip);
}

/// The sideways ROM header: no language entry, the service entry, the
/// type, copyright offset and version, then the title, the version string
/// and the copyright.
fn header(a: &mut Asm) {
    a.bytes(&[0, 0, 0]);
    a.jmp(crate::rom::asm::abs("service"));
    a.byte(ROM_TYPE);
    a.byte(lo("copyright"));
    a.byte(VERSION);
    a.text(TITLE);
    a.byte(0);
    a.text(VERSION_TEXT);
    a.here("copyright");
    a.byte(0);
    a.text(COPYRIGHT);
    a.byte(0);
}
