//! Sideways RAM, which holds the register where a free bank is found. A bank
//! pages in at &8000, where the editor runs, so what reaches one runs from
//! [`page::RAM_CODE`], copied there at each entry: `copy`, the forward copy
//! the text's moves share; `bank_copy`, the same with a bank paged in; and
//! `claim`, which takes a free bank. A bank is free where the MOS found no
//! ROM in it, it holds no ROM image since and it takes a write; a claimed
//! bank's header marks it as no ROM, so no BREAK registers it.

use alloc::vec::Vec;

use super::ops::{add16, cmp16, sub16};
use super::ws::{page, zp as z};
use crate::machine::bbc::sheila;
use crate::machine::{OS_BASE, SIDEWAYS_BASE};
use crate::rom::asm::{abs, byte, hi, imm, lo, zp, Asm, AsmError, RTS};
use crate::rom::os::{entry, header, osbyte, zp as os, CR, ROM_TYPES};

/// What a claimed bank's copyright offset holds: an offset to itself, not a
/// zero, so the MOS finds no copyright there.
pub(super) const UNSIGNED: u8 = 7;
const _: () = assert!(header::COPYRIGHT - SIDEWAYS_BASE == UNSIGNED as u16);
/// The register's first byte in a claimed bank, past the header the claim
/// marks, and the most it holds there.
pub(super) const REGISTER: u16 = header::COPYRIGHT + 1;
const REGISTER_ROOM: u16 = OS_BASE - REGISTER;
/// What OSBYTE &81 returns in X for X 0, Y &FF on an Electron, which pages
/// its ROMs elsewhere.
const ELECTRON: u8 = 1;

/// The routines assembled for [`page::RAM_CODE`], and where each starts.
pub(super) struct Ram {
    bytes: Vec<u8>,
    /// N1 bytes from P1 to P2, first byte first, so the destination lies
    /// below the source or apart from it; A, X, Y and the pointers' high
    /// bytes changed.
    pub copy: u16,
    /// The same with bank X paged in for it, the editor's own bank paged
    /// for none.
    pub bank_copy: u16,
    /// Carry set where bank X is taken for the register, X kept.
    claim: u16,
    /// A CR, which `reg_cr` copies to end a register's last line.
    cr: u16,
}

/// The routines for [`page::RAM_CODE`].
pub(super) fn assemble() -> Result<Ram, AsmError> {
    let room = usize::from(page::MARK - page::RAM_CODE);
    let mut a = Asm::new(page::RAM_CODE, room);
    a.here("copy");
    a.ldy(imm(0));
    a.ldx(zp(z::N1 + 1));
    let part = a.local();
    a.beq(part);
    let pages = a.here_local();
    a.lda(zp(z::P1).ind_y());
    a.sta(zp(z::P2).ind_y());
    a.iny();
    a.bne(pages);
    a.inc(zp(z::P1 + 1));
    a.inc(zp(z::P2 + 1));
    a.dex();
    a.bne(pages);
    a.bind(part);
    a.ldx(zp(z::N1));
    a.beq(RTS);
    let bytes = a.here_local();
    a.lda(zp(z::P1).ind_y());
    a.sta(zp(z::P2).ind_y());
    a.iny();
    a.dex();
    a.bne(bytes);
    a.rts();

    a.here("bank_copy");
    a.cpx(zp(os::ROM));
    a.beq("copy");
    page_in(&mut a);
    a.jsr("copy");
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

    a.here("cr");
    a.byte(CR);
    a.here("ram_top");

    let image = a.finish()?;
    let at = |name: &'static str| image.symbol(name).ok_or(AsmError::Unbound { name });
    let top = at("ram_top")?;
    Ok(Ram {
        bytes: image.bytes[..usize::from(top - page::RAM_CODE)].to_vec(),
        copy: at("copy")?,
        bank_copy: at("bank_copy")?,
        claim: at("claim")?,
        cr: at("cr")?,
    })
}

/// Bank X paged in, the bank it replaces pushed.
fn page_in(a: &mut Asm) {
    a.lda(zp(os::ROM));
    a.pha();
    a.stx(zp(os::ROM));
    a.stx(abs(sheila::ROMSEL));
}

pub(super) fn emit(a: &mut Asm, ram: &Ram) {
    installing(a, ram);
    finding(a, ram);
    register(a, ram);
}

/// `install`: the routines copied to [`page::RAM_CODE`]. `reclaim`: the same,
/// after anything else has run, and the register's bank taken again, the
/// register emptied and another found where it is no longer free.
fn installing(a: &mut Asm, ram: &Ram) {
    a.here("install");
    a.ldx(imm(byte(ram.bytes.len() - 1)));
    let copy = a.here_local();
    a.lda(abs("ram_code").x());
    a.sta(abs(page::RAM_CODE).x());
    a.dex();
    a.bpl(copy);
    a.rts();
    let bytes = ram.bytes.clone();
    a.later(move |a| {
        a.here("ram_code");
        a.bytes(&bytes);
    });

    a.here("reclaim");
    a.jsr("install");
    a.ldx(abs(page::REG_BANK));
    a.cpx(zp(os::ROM));
    a.beq(RTS);
    a.jsr(ram.claim);
    a.bcs(RTS);
    a.lda(imm(0));
    a.sta(abs(page::REG_LEN));
    a.sta(abs(page::REG_LEN + 1));
    a.fall_into("find_ram");
}

/// `find_ram`: the register's bank: the highest free one claimed, or the
/// editor's own. An Electron's ROMs page elsewhere, so it keeps its own.
fn finding(a: &mut Asm, ram: &Ram) {
    a.here("find_ram");
    a.lda(zp(os::ROM));
    a.sta(abs(page::REG_BANK));
    a.lda(imm(osbyte::INKEY));
    a.ldx(imm(0));
    a.ldy(imm(0xFF));
    a.jsr(entry::OSBYTE);
    a.cpx(imm(ELECTRON));
    a.beq(RTS);
    a.ldx(imm(15));
    let scan = a.here_local();
    let next = a.local();
    a.lda(abs(ROM_TYPES).x());
    a.bne(next);
    a.jsr(ram.claim);
    let found = a.local();
    a.bcs(found);
    a.bind(next);
    a.dex();
    a.bpl(scan);
    a.rts();
    a.bind(found);
    a.stx(abs(page::REG_BANK));
    a.rts();
}

/// `reg_room`: carry set where the register cannot be made N1 long, N1
/// kept. `reg_source`: N1 its length, P3 its first byte, and X and
/// [`page::SRC_BANK`] its bank. `reg_cr`: a CR put at the register's N0th
/// byte.
fn register(a: &mut Asm, ram: &Ram) {
    a.here("reg_room");
    a.lda(abs(page::REG_BANK));
    a.cmp(zp(os::ROM));
    let banked = a.local();
    a.bne(banked);
    a.lda(zp(z::N1));
    a.pha();
    a.lda(zp(z::N1 + 1));
    a.pha();
    a.jsr("resize_reg");
    a.pla();
    a.sta(zp(z::N1 + 1));
    a.pla();
    a.sta(zp(z::N1));
    a.rts();
    a.bind(banked);
    a.lda(zp(z::N1));
    a.cmp(imm(lo(REGISTER_ROOM + 1)));
    a.lda(zp(z::N1 + 1));
    a.sbc(imm(hi(REGISTER_ROOM + 1)));
    a.rts();

    a.here("reg_source");
    a.lda(abs(page::REG_LEN));
    a.sta(zp(z::N1));
    a.lda(abs(page::REG_LEN + 1));
    a.sta(zp(z::N1 + 1));
    a.ldx(abs(page::REG_BANK));
    a.stx(abs(page::SRC_BANK));
    a.lda(zp(z::REG));
    a.ldy(zp(z::REG + 1));
    a.cpx(zp(os::ROM));
    let at = a.local();
    a.beq(at);
    a.lda(imm(lo(REGISTER)));
    a.ldy(imm(hi(REGISTER)));
    a.bind(at);
    a.sta(zp(z::P3));
    a.sty(zp(z::P3 + 1));
    a.rts();

    // `cut_room`: the command abandoned where the free memory cannot take
    // N1 bytes into the undo record and, below HIMEM, the register too, a CR
    // more for whole lines (A non-zero); N0 and N1 kept.
    a.here("cut_room");
    a.cmp(imm(1));
    a.lda(zp(z::N1));
    a.adc(imm(0));
    a.sta(zp(z::N2));
    a.lda(zp(z::N1 + 1));
    a.adc(imm(0));
    a.sta(zp(z::N2 + 1));
    let refuse = a.local();
    let below = a.local();
    a.lda(abs(page::REG_BANK));
    a.cmp(zp(os::ROM));
    a.beq(below);
    a.lda(zp(z::N2));
    a.cmp(imm(lo(REGISTER_ROOM + 1)));
    a.lda(zp(z::N2 + 1));
    a.sbc(imm(hi(REGISTER_ROOM + 1)));
    a.bcs(refuse);
    a.lda(imm(0));
    a.sta(zp(z::N2));
    a.sta(zp(z::N2 + 1));
    a.bind(below);
    add16(a, z::N2, z::N2, z::N1);
    sub16(a, z::P0, z::LIMIT, z::BUF);
    sub16(a, z::P0, z::P0, z::LEN);
    cmp16(a, z::P0, z::N2);
    a.bcs(RTS);
    a.bind(refuse);
    a.jmp("no_room");

    a.here("reg_cr");
    a.jsr("reg_source");
    a.clc();
    a.lda(zp(z::P3));
    a.adc(zp(z::N0));
    a.sta(zp(z::P2));
    a.lda(zp(z::P3 + 1));
    a.adc(zp(z::N0 + 1));
    a.sta(zp(z::P2 + 1));
    a.lda(imm(lo(ram.cr)));
    a.sta(zp(z::P1));
    a.lda(imm(hi(ram.cr)));
    a.sta(zp(z::P1 + 1));
    a.lda(imm(1));
    a.sta(zp(z::N1));
    a.lda(imm(0));
    a.sta(zp(z::N1 + 1));
    a.jmp(abs(ram.bank_copy));
}
