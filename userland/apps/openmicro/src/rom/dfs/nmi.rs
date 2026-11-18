//! The NMI routine the 8271 interrupts through, assembled for &0D00 and
//! copied there while the DFS holds the NMI: each interrupt moves one byte
//! to or from memory, or, once the command ends, takes its result. What a
//! transfer runs on is held after it, the one place a read cannot land.

use super::ws::zp as z;
use crate::machine::bbc::fdc::status;
use crate::machine::bbc::sheila;
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
    /// What a transfer runs on.
    pub held: Held,
}

/// What a transfer runs on while the DFS holds the NMI, from the claim to
/// its release: a read may land anywhere from &0E00 up, this workspace
/// included.
pub(super) struct Held {
    /// The ROM that had the NMI before the DFS claimed it.
    pub previous: u16,
    /// The drive's select bits, and the direction, a write non-zero.
    pub select: u16,
    pub write: u16,
    /// The next command's track and sector on it.
    pub track: u16,
    pub sector: u16,
    /// Sectors still to move, the address the next command starts at, and
    /// the bytes a read has still to store, three of them: one run, as the
    /// transfer is asked for them.
    pub sectors: u16,
    pub address: u16,
    pub wanted: u16,
    /// The sectors the command in hand moves, and the bytes of them a read
    /// stores.
    pub count: u16,
    pub bytes: u16,
    /// Attempts the command in hand has left.
    pub attempts: u16,
}

impl Held {
    /// The state laid out from `base`, and the address past it.
    const fn from(base: u16) -> (Self, u16) {
        let previous = base;
        let select = previous + 1;
        let write = select + 1;
        let track = write + 1;
        let sector = track + 1;
        let sectors = sector + 1;
        let address = sectors + 2;
        let wanted = address + 2;
        let count = wanted + 3;
        let bytes = count + 1;
        let attempts = bytes + 2;
        (
            Self {
                previous,
                select,
                write,
                track,
                sector,
                sectors,
                address,
                wanted,
                count,
                bytes,
                attempts,
            },
            attempts + 1,
        )
    }
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
    // Room for the held state, which each transfer sets rather than copies.
    let (_, held_len) = Held::from(0);
    a.fill(usize::from(held_len), 0);

    let image = a.finish()?;
    let at = |name: &'static str| image.symbol(name).ok_or(AsmError::Unbound { name });
    let top = at("nmi_top")?;
    Ok(Nmi {
        route: at("nmi")? + 1,
        read: at("nmi_read")?,
        write: at("nmi_write")?,
        discard: at("nmi_discard")?,
        store: at("nmi_store")? + 1,
        fetch: at("nmi_fetch")? + 1,
        held: Held::from(top).0,
        bytes: image.bytes[..usize::from(top - page_d::NMI)].to_vec(),
    })
}
