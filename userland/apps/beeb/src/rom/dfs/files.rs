//! OSFILE: a whole file saved, loaded or deleted, or its catalogue entry
//! read or written, through the block at X and Y.
//!
//! As DFS 1.20 does, each call returns A 0 when no file matches and 1 when
//! one does, and leaves X and Y as they were; a locked file reads as not
//! deletable, and either not writable or not deletable locks one.

use super::far;
use super::ws::{catalogue as cat, page, zp as z};
use crate::rom::asm::{abs, imm, zp, Acc, Asm, RTS};
use crate::rom::os::zp as os;

/// The block's fields, by offset.
mod block {
    pub const NAME: u8 = 0;
    pub const LOAD: u8 = 2;
    /// The execution address; its low byte zero asks a load for the
    /// block's address.
    pub const EXEC: u8 = 6;
    /// The start of the data to save, or the length read.
    pub const START: u8 = 10;
    /// The end of the data to save, or the attributes.
    pub const END: u8 = 14;
}

/// OSFILE's reasons.
const SAVE: u8 = 0;
const WRITE_ALL: u8 = 1;
const WRITE_LOAD: u8 = 2;
const WRITE_EXEC: u8 = 3;
const WRITE_ATTRIBUTES: u8 = 4;
const READ: u8 = 5;
const DELETE: u8 = 6;
const LOAD: u8 = 0xFF;
/// The attributes that lock a file, either of them: not writable, not
/// deletable.
const LOCKING: u8 = 0x0A;
/// A locked file's attributes as read: not deletable.
const LOCKED: u8 = 0x08;
/// A file found.
const FILE: u8 = 1;
/// The directory byte's lock bit.
const DIR_BYTE: u16 = cat::NAMES + cat::ENTRY_DIR as u16;

pub(super) fn emit(a: &mut Asm) {
    osfile(a);
    save(a);
    load(a);
    entries(a);
}

fn osfile(a: &mut Asm) {
    a.here("dfs_file");
    a.stx(zp(z::BLOCK));
    a.sty(zp(z::BLOCK + 1));
    a.pha();
    a.jsr("claim_workspace");
    a.ldy(imm(block::NAME));
    a.lda(zp(z::BLOCK).ind_y());
    a.sta(zp(os::TEXT));
    a.iny();
    a.lda(zp(z::BLOCK).ind_y());
    a.sta(zp(os::TEXT + 1));
    a.ldy(imm(0));
    a.jsr("parse_name");
    a.jsr("name_catalogue");
    a.pla();
    let found = a.local();
    a.cmp(imm(LOAD));
    let not_load = a.local();
    a.bne(not_load);
    a.jsr("file_load");
    a.jmp(found);
    a.bind(not_load);
    a.cmp(imm(SAVE));
    let not_save = a.local();
    a.bne(not_save);
    a.jsr("file_save");
    a.jmp(found);
    a.bind(not_save);
    a.cmp(imm(DELETE + 1));
    let none = a.local();
    a.bcs(none);
    a.pha();
    a.jsr("find_first");
    let missing = a.local();
    a.bcs(missing);
    a.pla();
    a.jsr("file_entry");
    a.bind(found);
    a.lda(imm(FILE));
    let back = a.local();
    a.bne(back);
    a.bind(missing);
    a.pla();
    a.bind(none);
    a.lda(imm(0));
    a.bind(back);
    a.ldx(zp(z::BLOCK));
    a.ldy(zp(z::BLOCK + 1));
    a.ora(imm(0));
    a.rts();
}

/// A save: any old file of the name, unless locked or open, makes way for
/// the new one, which takes the first room that holds it.
fn save(a: &mut Asm) {
    a.here("file_save");
    a.jsr("find_first");
    let new = a.local();
    a.bcs(new);
    a.jsr("check_unlocked");
    a.jsr("check_closed");
    a.jsr("remove_entry");
    a.bind(new);
    // The addresses as given, then the length the span between start and
    // end.
    a.ldy(imm(block::LOAD));
    a.ldx(imm(0));
    let addresses = a.here_local();
    a.lda(zp(z::BLOCK).ind_y());
    a.sta(abs(page::INFO_LOAD).x());
    a.iny();
    a.inx();
    a.cpx(imm(8));
    a.bne(addresses);
    a.sec();
    for k in 0..4 {
        a.ldy(imm(block::END + k));
        a.lda(zp(z::BLOCK).ind_y());
        a.ldy(imm(block::START + k));
        a.sbc(zp(z::BLOCK).ind_y());
        a.sta(abs(page::INFO_LENGTH + u16::from(k)));
    }
    a.jsr("length_sectors");
    a.jsr("allocate");
    a.lda(zp(z::T6));
    a.sta(abs(page::INFO_START));
    a.lda(zp(z::T7));
    a.sta(abs(page::INFO_START + 1));
    // The catalogue first, then the data straight on from it, as DFS 1.20
    // writes them.
    a.jsr("make_room");
    a.jsr("set_entry");
    a.jsr("write_catalogue");
    a.ldy(imm(block::START));
    a.lda(zp(z::BLOCK).ind_y());
    a.sta(abs(page::OP_ADDRESS));
    a.iny();
    a.lda(zp(z::BLOCK).ind_y());
    a.sta(abs(page::OP_ADDRESS + 1));
    a.jsr("sectors_of_file");
    a.lda(imm(1));
    a.sta(abs(page::OP_WRITE));
    a.jsr("transfer");
    a.jmp("message");

    // T4 and T5: the sectors INFO_LENGTH fills.
    a.here("length_sectors");
    a.lda(abs(page::INFO_LENGTH + 1));
    a.sta(zp(z::T4));
    a.lda(abs(page::INFO_LENGTH + 2));
    a.and(imm(3));
    a.sta(zp(z::T5));
    a.lda(abs(page::INFO_LENGTH));
    let whole = a.local();
    a.beq(whole);
    a.inc(zp(z::T4));
    a.bne(whole);
    a.inc(zp(z::T5));
    a.bind(whole);
    a.rts();

    // The transfer's sectors and bytes: those of the file the INFO record
    // describes.
    a.here("sectors_of_file");
    a.jsr("length_sectors");
    a.lda(abs(page::INFO_START));
    a.sta(abs(page::OP_SECTOR));
    a.lda(abs(page::INFO_START + 1));
    a.sta(abs(page::OP_SECTOR + 1));
    a.lda(zp(z::T4));
    a.sta(abs(page::OP_SECTORS));
    a.lda(zp(z::T5));
    a.sta(abs(page::OP_SECTORS + 1));
    for k in 0..3 {
        a.lda(abs(page::INFO_LENGTH + k));
        a.sta(abs(page::OP_STORE + k));
    }
    a.rts();
}

/// A load: the first file that matches, to the block's address unless its
/// execution address's low byte asks for the file's own, the catalogue
/// entry then written into the block.
fn load(a: &mut Asm) {
    a.here("file_load");
    a.jsr("find_first");
    far(a, Asm::bcc, "err_not_found");
    a.jsr("entry_info");
    a.ldy(imm(block::EXEC));
    a.lda(zp(z::BLOCK).ind_y());
    let given = a.local();
    a.beq(given);
    a.lda(abs(page::INFO_LOAD));
    a.sta(abs(page::OP_ADDRESS));
    a.lda(abs(page::INFO_LOAD + 1));
    a.sta(abs(page::OP_ADDRESS + 1));
    let go = a.local();
    a.jmp(go);
    a.bind(given);
    a.ldy(imm(block::LOAD));
    a.lda(zp(z::BLOCK).ind_y());
    a.sta(abs(page::OP_ADDRESS));
    a.iny();
    a.lda(zp(z::BLOCK).ind_y());
    a.sta(abs(page::OP_ADDRESS + 1));
    a.bind(go);
    a.jsr("load_data");
    a.jsr("message");
    a.jmp("entry_to_block");

    // The file the INFO record describes read to OP_ADDRESS.
    a.here("load_data");
    a.jsr("sectors_of_file");
    a.lda(imm(0));
    a.sta(abs(page::OP_WRITE));
    a.jmp("transfer");

    // With messages on, the entry at ENTRY as *INFO shows it.
    a.here("message");
    a.lda(abs(page::MESSAGES));
    a.bne(RTS);
    a.jmp("print_info");
}

/// OSFILE 1 to 6 on the entry at ENTRY, the reason in A.
fn entries(a: &mut Asm) {
    a.here("file_entry");
    a.cmp(imm(READ));
    far(a, Asm::bne, "entry_to_block");
    a.cmp(imm(DELETE));
    let write = a.local();
    a.bne(write);
    a.jsr("check_unlocked");
    a.jsr("check_closed");
    a.jsr("message");
    a.jsr("remove_entry");
    a.jmp("write_catalogue");

    a.bind(write);
    a.pha();
    a.jsr("entry_info");
    a.pla();
    a.pha();
    for (field, to, skip) in [
        (block::LOAD, page::INFO_LOAD, WRITE_EXEC),
        (block::EXEC, page::INFO_EXEC, WRITE_LOAD),
    ] {
        let kept = a.local();
        a.cmp(imm(skip));
        a.beq(kept);
        a.cmp(imm(WRITE_ATTRIBUTES));
        a.beq(kept);
        a.ldy(imm(field));
        a.ldx(imm(0));
        let copy = a.here_local();
        a.lda(zp(z::BLOCK).ind_y());
        a.sta(abs(to).x());
        a.iny();
        a.inx();
        a.cpx(imm(4));
        a.bne(copy);
        a.bind(kept);
        a.pla();
        a.pha();
    }
    a.jsr("set_fields");
    a.pla();
    a.cmp(imm(WRITE_ALL));
    let attributes = a.local();
    a.beq(attributes);
    a.cmp(imm(WRITE_ATTRIBUTES));
    let written = a.local();
    a.bne(written);
    a.bind(attributes);
    a.ldy(imm(block::END));
    a.lda(zp(z::BLOCK).ind_y());
    a.and(imm(LOCKING));
    a.cmp(imm(1));
    a.jsr("set_lock");
    a.bind(written);
    a.jmp("write_catalogue");

    // The entry's addresses and length, and its attributes, into the block.
    a.here("entry_to_block");
    a.jsr("entry_info");
    a.ldy(imm(block::LOAD));
    a.ldx(imm(0));
    let field = a.here_local();
    a.lda(abs(page::INFO_LOAD).x());
    a.sta(zp(z::BLOCK).ind_y());
    a.iny();
    a.inx();
    a.cpx(imm(12));
    a.bne(field);
    a.ldx(zp(z::ENTRY));
    a.lda(abs(DIR_BYTE).x());
    a.asl(Acc);
    a.lda(imm(0));
    let unlocked = a.local();
    a.bcc(unlocked);
    a.lda(imm(LOCKED));
    a.bind(unlocked);
    a.sta(zp(z::BLOCK).ind_y());
    a.lda(imm(0));
    for _ in 0..3 {
        a.iny();
        a.sta(zp(z::BLOCK).ind_y());
    }
    a.rts();

    // The entry at ENTRY locked with C set, or unlocked.
    a.here("set_lock");
    a.ldy(zp(z::ENTRY));
    a.lda(abs(DIR_BYTE).y());
    a.and(imm(!cat::LOCKED));
    let store = a.local();
    a.bcc(store);
    a.ora(imm(cat::LOCKED));
    a.bind(store);
    a.sta(abs(DIR_BYTE).y());
    a.rts();

    // Locked unless the entry at ENTRY is not.
    a.here("check_unlocked");
    a.ldy(zp(z::ENTRY));
    a.lda(abs(DIR_BYTE).y());
    far(a, Asm::bpl, "err_locked");
    a.rts();
}
