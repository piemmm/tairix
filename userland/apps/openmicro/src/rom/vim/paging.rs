//! Sideways RAM, which the text's memory runs on into once main memory is
//! full ([`super::vmem`]). A bank pages in at &8000, where the editor runs,
//! so what reaches one runs from [`page::RAM_CODE`], copied there at each
//! entry. A bank is free where the MOS found no ROM in it, it holds no ROM
//! image since and it takes a write; a claimed bank's header marks it as no
//! ROM, so no BREAK registers it.

use alloc::vec::Vec;

use super::ops::{mov16, sub16};
use super::vmem::BANK_PAGES;
use super::ws::{page, show, zp as z};
use crate::machine::bbc::sheila;
use crate::machine::SIDEWAYS_BASE;
use crate::rom::asm::{abs, byte, hi, imm, lo, zp, Asm, AsmError, RTS};
use crate::rom::os::{entry, header, osbyte, page2, zp as os, CR};

/// What a claimed bank's copyright offset holds: an offset to itself, not a
/// zero, so the MOS finds no copyright there.
pub(super) const UNSIGNED: u8 = 7;
const _: () = assert!(header::COPYRIGHT - SIDEWAYS_BASE == UNSIGNED as u16);
/// What OSBYTE &81 returns in X for X 0, Y &FF on an Electron, which pages
/// its ROMs elsewhere.
const ELECTRON: u8 = 1;
/// The highest bank number.
const LAST_BANK: u8 = 15;

/// The routines assembled for [`page::RAM_CODE`], and where each starts.
pub(super) struct Ram {
    bytes: Vec<u8>,
    /// [`page::VA`] the byte at VP, Y with bank X paged in; Y kept.
    pub get: u16,
    /// [`page::VA`] written at VP, Y with bank X paged in; Y kept.
    pub put: u16,
    /// [`page::XC`] bytes, 0 for 256, from VP to P3 with bank X paged in,
    /// first byte first; `down` last byte first.
    pub up: u16,
    pub down: u16,
    /// The same from bank [`page::SB`] to bank [`page::DB`].
    pub up2: u16,
    pub down2: u16,
    /// Carry set where bank X is taken for the text, X kept.
    claim: u16,
    /// A bank X's type from the ROM type table, as `lda` leaves the flags;
    /// `install` sets the table's address the OS gives.
    pub rom_type: u16,
}

/// The routines for [`page::RAM_CODE`].
pub(super) fn assemble() -> Result<Ram, AsmError> {
    let room = usize::from(page::MARK - page::RAM_CODE);
    let mut a = Asm::new(page::RAM_CODE, room);
    a.here("page_back");
    a.pla();
    a.sta(zp(os::ROM));
    a.sta(abs(sheila::ROMSEL));
    a.rts();

    // A ROM image starts its copyright "\0(" where the offset points; a bank
    // without one is RAM if the offset's byte takes a write.
    a.here("claim");
    page_in(&mut a);
    a.ldy(abs(header::COPYRIGHT));
    a.lda(abs(SIDEWAYS_BASE).y());
    let unsigned = a.local();
    let refused = a.local();
    a.bne(unsigned);
    a.lda(abs(SIDEWAYS_BASE + 1).y());
    a.cmp(imm(b'('));
    a.beq(refused);
    a.bind(unsigned);
    a.lda(abs(header::COPYRIGHT));
    a.eor(imm(0xFF));
    a.sta(abs(header::COPYRIGHT));
    a.cmp(abs(header::COPYRIGHT));
    a.bne(refused);
    a.lda(imm(UNSIGNED));
    a.sta(abs(header::COPYRIGHT));
    a.sec();
    a.bcs("page_back");
    a.bind(refused);
    a.clc();
    a.bcc("page_back");

    a.here("get");
    page_in(&mut a);
    a.lda(zp(z::VP).ind_y());
    a.sta(abs(page::VA));
    a.jmp("page_back");

    a.here("put");
    page_in(&mut a);
    a.lda(abs(page::VA));
    a.sta(zp(z::VP).ind_y());
    a.jmp("page_back");

    a.here("up");
    page_in(&mut a);
    a.ldy(imm(0));
    let up = a.here_local();
    a.lda(zp(z::VP).ind_y());
    a.sta(zp(z::P3).ind_y());
    a.iny();
    a.cpy(abs(page::XC));
    a.bne(up);
    a.jmp("page_back");

    a.here("down");
    page_in(&mut a);
    a.ldy(abs(page::XC));
    let down = a.here_local();
    a.dey();
    a.lda(zp(z::VP).ind_y());
    a.sta(zp(z::P3).ind_y());
    a.tya();
    a.bne(down);
    a.jmp("page_back");

    a.here("up2");
    a.lda(zp(os::ROM));
    a.pha();
    a.ldy(imm(0));
    let up2 = a.here_local();
    across(&mut a);
    a.iny();
    a.cpy(abs(page::XC));
    a.bne(up2);
    a.jmp("page_back");

    a.here("down2");
    a.lda(zp(os::ROM));
    a.pha();
    a.ldy(abs(page::XC));
    let down2 = a.here_local();
    a.dey();
    across(&mut a);
    a.tya();
    a.bne(down2);
    a.jmp("page_back");

    a.here("rom_type");
    a.lda(abs(page2::MODEL_B.rom_types).x());
    a.rts();

    a.here("ram_top");

    let image = a.finish()?;
    let at = |name: &'static str| image.symbol(name).ok_or(AsmError::Unbound { name });
    let top = at("ram_top")?;
    Ok(Ram {
        bytes: image.bytes[..usize::from(top - page::RAM_CODE)].to_vec(),
        get: at("get")?,
        put: at("put")?,
        up: at("up")?,
        down: at("down")?,
        up2: at("up2")?,
        down2: at("down2")?,
        claim: at("claim")?,
        rom_type: at("rom_type")?,
    })
}

/// Bank X paged in, the bank it replaces pushed.
fn page_in(a: &mut Asm) {
    a.lda(zp(os::ROM));
    a.pha();
    a.stx(zp(os::ROM));
    a.stx(abs(sheila::ROMSEL));
}

/// The byte at VP, Y read from bank [`page::SB`] and written at P3, Y in
/// bank [`page::DB`].
fn across(a: &mut Asm) {
    for (bank, at, read) in [(page::SB, z::VP, true), (page::DB, z::P3, false)] {
        a.ldx(abs(bank));
        a.stx(zp(os::ROM));
        a.stx(abs(sheila::ROMSEL));
        if read {
            a.lda(zp(at).ind_y());
        } else {
            a.sta(zp(at).ind_y());
        }
    }
}

pub(super) fn emit(a: &mut Asm, ram: &Ram) {
    installing(a, ram);
    growing(a, ram);
}

/// `install`: the routines copied to [`page::RAM_CODE`]. `reclaim`: the same
/// after anything else has run, and the text given up, and said so, where a
/// bank it runs on into is no longer the editor's.
fn installing(a: &mut Asm, ram: &Ram) {
    // Counting up, since the routines pass 128 bytes.
    a.here("install");
    a.ldx(imm(0));
    let copy = a.here_local();
    a.lda(abs("ram_code").x());
    a.sta(abs(page::RAM_CODE).x());
    a.inx();
    a.cpx(imm(byte(ram.bytes.len())));
    a.bne(copy);
    a.lda(imm(osbyte::ROM_TABLE));
    a.ldx(imm(0));
    a.ldy(imm(0xFF));
    a.jsr(entry::OSBYTE);
    a.stx(abs(ram.rom_type + 1));
    a.sty(abs(ram.rom_type + 2));
    a.rts();
    let bytes = ram.bytes.clone();
    a.later(move |a| {
        a.here("ram_code");
        a.bytes(&bytes);
    });

    a.here("reclaim");
    a.jsr("install");
    a.ldx(abs(page::SEGMENTS));
    let each = a.here_local();
    a.dex();
    a.bmi(RTS);
    a.stx(zp(z::T0));
    a.lda(abs(page::BANKS).x());
    a.tax();
    a.jsr(ram.rom_type);
    let lost = a.local();
    a.bne(lost);
    a.lda(imm(lo(header::COPYRIGHT)));
    a.sta(zp(z::VP));
    a.lda(imm(hi(header::COPYRIGHT)));
    a.sta(zp(z::VP + 1));
    a.ldy(imm(0));
    a.jsr(ram.get);
    a.lda(abs(page::VA));
    a.cmp(imm(UNSIGNED));
    a.bne(lost);
    a.ldx(zp(z::T0));
    a.jmp(each);
    // Another program took it: the text, the undo record and the register
    // lost with it, and no name left to write the empty text over.
    a.bind(lost);
    a.lda(imm(0));
    a.sta(abs(page::SEGMENTS));
    a.sta(zp(z::FILE));
    a.jsr("set_area");
    a.jsr("buf_reset");
    a.lda(imm(CR));
    a.sta(abs(page::NAME));
    a.call_with_text("say", &["Text lost"]);
    a.lda(imm(show::ALL));
    a.jmp("owe");
}

/// `grow`: a free bank taken onto the end of the text's memory, what lies
/// past the gap moved up into it; carry set where none can be, nothing
/// changed. N0, N1, N2 and P3 kept.
fn growing(a: &mut Asm, ram: &Ram) {
    a.here("grow");
    a.lda(abs(page::SEGMENTS));
    a.cmp(imm(page::MOST_BANKS));
    let refused = a.local();
    a.bcs(refused);
    a.lda(imm(osbyte::INKEY));
    a.ldx(imm(0));
    a.ldy(imm(0xFF));
    a.jsr(entry::OSBYTE);
    a.cpx(imm(ELECTRON));
    a.beq(refused);
    // A free bank that is neither the editor's nor already the text's.
    a.ldx(imm(LAST_BANK));
    let scan = a.here_local();
    let next = a.local();
    a.jsr(ram.rom_type);
    a.bne(next);
    a.cpx(zp(os::ROM));
    a.beq(next);
    a.txa();
    a.ldy(abs(page::SEGMENTS));
    let held = a.here_local();
    let free = a.local();
    a.dey();
    a.bmi(free);
    a.cmp(abs(page::BANKS).y());
    a.bne(held);
    a.beq(next);
    a.bind(free);
    a.jsr(ram.claim);
    let found = a.local();
    a.bcs(found);
    a.bind(next);
    a.dex();
    a.bpl(scan);
    a.bind(refused);
    a.sec();
    a.rts();
    a.bind(found);
    a.txa();
    a.ldy(abs(page::SEGMENTS));
    a.sta(abs(page::BANKS).y());
    a.inc(abs(page::SEGMENTS));
    for v in [z::N1, z::N1 + 1, z::P3, z::P3 + 1] {
        a.lda(zp(v));
        a.pha();
    }
    a.jsr("p1_gend");
    mov16(a, z::P2, z::GEND);
    a.lda(zp(z::P2 + 1));
    a.clc();
    a.adc(imm(BANK_PAGES));
    a.sta(zp(z::P2 + 1));
    sub16(a, z::N1, z::LIMIT, z::GEND);
    a.jsr("memmove");
    for v in [z::P3 + 1, z::P3, z::N1 + 1, z::N1] {
        a.pla();
        a.sta(zp(v));
    }
    a.lda(imm(BANK_PAGES));
    a.jsr("past_gap_by");
    a.clc();
    a.rts();
}
