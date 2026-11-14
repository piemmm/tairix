//! The top page: the extended vectors' entry points and their dispatcher,
//! the default vector table's descriptor, the OS entry points and the
//! processor's vectors, each at its documented address.

use super::ws::{os, page_d, vector};
use super::{page, Held};
use crate::machine::Model;
use crate::rom::asm::{abs, hi, imm, lo, sym, zp, Asm};
use crate::rom::os::{entry, EXTENDED_ENTRIES};

/// The default vector table's length, then its address.
pub const DEFAULTS_DESCRIPTOR: u16 = 0xFFB6;
/// The first OS entry point.
pub const ENTRY_POINTS: u16 = 0xFFB9;
// OSEVEN, GSINIT and GSREAD are the third to fifth of the jumps placed from
// there.
const _: () = assert!(
    ENTRY_POINTS + 6 == entry::OSEVEN
        && ENTRY_POINTS + 9 == entry::GSINIT
        && ENTRY_POINTS + 12 == entry::GSREAD
);

/// A vector's extended entry: the dispatcher learns which from where it was
/// called from.
pub fn emit(a: &mut Asm, model: Model) {
    a.org(EXTENDED_ENTRIES);
    for _ in 0..vector::COUNT {
        a.jsr("extended_vector");
    }
    extended_vector(a, model);
    a.place_later_before(DEFAULTS_DESCRIPTOR);
    a.org(DEFAULTS_DESCRIPTOR);
    a.byte(2 * vector::COUNT);
    a.word("vector_defaults");
    entry_points(a);
}

/// Page in the ROM the extended vector names and enter its routine with the
/// caller's registers and flags; the routine's `RTS` comes back through
/// `extended_return`, which restores the ROM paged in before.
///
/// The frame built under the caller's return address, top first: the flags,
/// the routine's address for `RTI`, `extended_return - 1` for the routine's
/// `RTS`, then the ROM to restore.
fn extended_vector(a: &mut Asm, model: Model) {
    a.here("extended_vector");
    a.pha();
    a.pha();
    a.pha();
    a.php();
    a.pha();
    a.txa();
    a.pha();
    a.tya();
    a.pha();
    a.tsx();
    // The return address's low byte is 3n + 2 for vector n.
    a.ldy(abs(0x0108).x());
    a.lda(abs(page_d::EXTENDED_VECTORS - 2).y());
    a.sta(abs(0x0105).x());
    a.lda(abs(page_d::EXTENDED_VECTORS - 1).y());
    a.sta(abs(0x0106).x());
    a.lda(imm(lo(sym("extended_return") + -1)));
    a.sta(abs(0x0107).x());
    a.lda(imm(hi(sym("extended_return") + -1)));
    a.sta(abs(0x0108).x());
    a.lda(zp(os::ROM));
    a.sta(abs(0x0109).x());
    a.lda(abs(page_d::EXTENDED_VECTORS).y());
    page(a, model, Held::A);
    a.pla();
    a.tay();
    a.pla();
    a.tax();
    a.pla();
    a.rti();

    a.here("extended_return");
    a.php();
    a.pha();
    a.txa();
    a.pha();
    a.tsx();
    a.lda(abs(0x0104).x());
    page(a, model, Held::A);
    // The flags over the ROM's byte, so popping them twice leaves the
    // caller's return address on top.
    a.lda(abs(0x0103).x());
    a.sta(abs(0x0104).x());
    a.pla();
    a.tax();
    a.pla();
    a.plp();
    a.plp();
    a.rts();
}

/// The documented OS entry points, each placed at its address.
fn entry_points(a: &mut Asm) {
    let routines = [
        ("osrdrm_entry", "osrdrm"),
        ("vduchr_entry", "vduchr"),
        ("oseven_entry", "oseven"),
        ("gsinit_entry", "gsinit"),
        ("gsread_entry", "gsread"),
        ("nvrdch_entry", "nvrdch"),
        ("nvwrch_entry", "nvwrch"),
    ];
    for (at, (placed, routine)) in (ENTRY_POINTS..).step_by(3).zip(routines) {
        a.org(at);
        a.here(placed);
        a.jmp(routine);
    }
    for (at, name, vector) in [
        (entry::OSFIND, "osfind", vector::FIND),
        (entry::OSGBPB, "osgbpb", vector::GBPB),
        (entry::OSBPUT, "osbput", vector::BPUT),
        (entry::OSBGET, "osbget", vector::BGET),
        (entry::OSARGS, "osargs", vector::ARGS),
        (entry::OSFILE, "osfile", vector::FILE),
        (entry::OSRDCH, "osrdch", vector::RDCH),
        (entry::OSWORD, "osword", vector::WORD),
        (entry::OSBYTE, "osbyte", vector::BYTE),
        (entry::OSCLI, "oscli", vector::CLI),
    ] {
        a.org(at);
        a.here(name);
        a.jmp(abs(vector).ind());
    }
    // OSASCI, OSNEWL, OSWRCR and OSWRCH fall into one another.
    a.org(entry::OSASCI);
    a.here("osasci");
    a.cmp(imm(0x0D));
    a.bne(sym("oswrch"));
    a.org(entry::OSNEWL);
    a.here("osnewl");
    a.lda(imm(0x0A));
    a.jsr("oswrch");
    a.org(entry::OSWRCR);
    a.lda(imm(0x0D));
    a.org(entry::OSWRCH);
    a.here("oswrch");
    a.jmp(abs(vector::WRCH).ind());
    a.org(0xFFFA);
    a.word(page_d::NMI);
    a.word("reset");
    a.word("irq_entry");
}
