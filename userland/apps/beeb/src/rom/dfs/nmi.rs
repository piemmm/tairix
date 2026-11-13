//! The NMI routine the 8271 interrupts through, assembled for &0D00 and
//! copied there while the DFS holds the NMI: each interrupt moves one byte
//! to or from memory, or, once the command ends, takes its result.

use super::ws::zp as z;
use crate::machine::fdc::status;
use crate::machine::sheila;
use crate::rom::asm::{abs, hi, imm, lo, sym, zp, Asm, AsmError};
use crate::rom::os::page_d;

/// The 8271's status, data and result registers.
pub(super) const STATUS: u16 = sheila::FDC;
pub(super) const RESULT: u16 = sheila::FDC + 1;
pub(super) const DATA: u16 = sheila::FDC + 4;

/// The room the NMI routine has, up to the extended vectors.
const ROOM: usize = (page_d::EXTENDED_VECTORS - page_d::NMI) as usize;

/// The assembled routine and the places the DFS sets before each command.
pub(super) struct Nmi {
    /// The bytes to copy to &0D00.
    pub bytes: alloc::vec::Vec<u8>,
    /// The entry's jump operand, which picks the routine below.
    pub route: u16,
    /// The three routines.
    pub read: u16,
    pub write: u16,
    pub discard: u16,
    /// The memory operands a read stores through and a write fetches from.
    pub store: u16,
    pub fetch: u16,
}

/// Assemble the routine.
///
/// # Errors
///
/// The builder's first [`AsmError`], or [`AsmError::Unbound`] for a place
/// the DFS sets that the routine lacks.
pub(super) fn build() -> Result<Nmi, AsmError> {
    let mut a = Asm::new(page_d::NMI, ROOM);
    a.here("nmi");
    a.jmp(abs("nmi_read"));

    // A read stores NMI_COUNT bytes, then discards the rest of its sectors.
    a.here("nmi_read");
    a.pha();
    a.lda(abs(STATUS));
    a.and(imm(status::DATA_REQUEST));
    a.beq("nmi_end");
    a.lda(abs(DATA));
    a.here("nmi_store");
    a.sta(abs(0));
    a.inc(abs(sym("nmi_store") + 1));
    let stored = a.local();
    a.bne(stored);
    a.inc(abs(sym("nmi_store") + 2));
    a.bind(stored);
    a.lda(zp(z::NMI_COUNT));
    let low = a.local();
    a.bne(low);
    a.dec(zp(z::NMI_COUNT + 1));
    a.bind(low);
    a.dec(zp(z::NMI_COUNT));
    let out = a.local();
    a.bne(out);
    a.lda(zp(z::NMI_COUNT + 1));
    a.bne(out);
    a.lda(imm(lo("nmi_discard")));
    a.sta(abs(sym("nmi") + 1));
    a.lda(imm(hi("nmi_discard")));
    a.sta(abs(sym("nmi") + 2));
    a.bind(out);
    a.pla();
    a.rti();

    a.here("nmi_discard");
    a.pha();
    a.lda(abs(STATUS));
    a.and(imm(status::DATA_REQUEST));
    a.beq("nmi_end");
    a.lda(abs(DATA));
    a.pla();
    a.rti();

    a.here("nmi_write");
    a.pha();
    a.lda(abs(STATUS));
    a.and(imm(status::DATA_REQUEST));
    a.beq("nmi_end");
    a.here("nmi_fetch");
    a.lda(abs(0));
    a.sta(abs(DATA));
    a.inc(abs(sym("nmi_fetch") + 1));
    let fetched = a.local();
    a.bne(fetched);
    a.inc(abs(sym("nmi_fetch") + 2));
    a.bind(fetched);
    a.pla();
    a.rti();

    // The command has ended: reading its result lets INT fall.
    a.here("nmi_end");
    a.lda(abs(RESULT));
    a.sta(zp(z::NMI_RESULT));
    a.pla();
    a.rti();
    a.here("nmi_top");

    let image = a.finish()?;
    let at = |name: &'static str| image.symbol(name).ok_or(AsmError::Unbound { name });
    let len = usize::from(at("nmi_top")? - page_d::NMI);
    Ok(Nmi {
        route: at("nmi")? + 1,
        read: at("nmi_read")?,
        write: at("nmi_write")?,
        discard: at("nmi_discard")?,
        store: at("nmi_store")? + 1,
        fetch: at("nmi_fetch")? + 1,
        bytes: image.bytes[..len].to_vec(),
    })
}
