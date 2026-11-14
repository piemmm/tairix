//! *BACKUP, *COPY and *COMPACT, which move sectors through the memory from
//! OSHWM to HIMEM, so a program there is lost, as with DFS 1.20; and
//! *DESTROY.
//!
//! Copying to the drive copied from asks for each disc as it is next
//! needed, and only then: the source's catalogue and the first memoryful of
//! a file are read before the destination is asked for.

use super::channels::each_open;
use super::commands::index_of;
use super::far;
use super::ws::{catalogue as cat, channel as ch, page, zp as z};
use crate::rom::asm::{abs, imm, zp, Asm, RTS};
use crate::rom::os::{entry, osbyte};

/// Which disc is in the drive when both are one: neither yet, the source,
/// the destination.
const NEITHER: u8 = 0xFF;
const SOURCE_IN: u8 = 0;
const TARGET_IN: u8 = 1;

/// The copy in hand, in the utility zero page and the command scratch.
mod c {
    use super::{page, z};
    /// The drives copied from and to.
    pub const FROM: u16 = z::UTILITY;
    pub const TO: u16 = z::UTILITY + 1;
    /// The memory's first page and its pages.
    pub const BASE: u16 = z::UTILITY + 2;
    pub const PAGES: u16 = z::UTILITY + 3;
    /// Sectors still to move, and those of the pass in hand.
    pub const LEFT: u16 = z::UTILITY + 4;
    pub const PASS: u16 = z::UTILITY + 6;
    /// Non-zero when the two discs share a drive, and which is in it.
    pub const SWAPS: u16 = z::UTILITY + 7;
    pub const IN: u16 = page::WORK;
    /// The next sector to read and to write.
    pub const SOURCE: u16 = page::WORK + 1;
    pub const TARGET: u16 = page::WORK + 3;
    /// The pattern *COPY looks for, and where its search has got to.
    pub const PATTERN: u16 = page::WORK + 5;
    pub const AT: u16 = page::WORK + 14;
    /// Non-zero once *COMPACT has moved a file.
    pub const MOVED: u16 = page::WORK + 15;
}

pub(super) fn emit(a: &mut Asm) {
    common(a);
    passes(a);
    backup(a);
    copy(a);
    compact(a);
    destroy(a);
}

/// `two_drives`: the two drive arguments into FROM and TO, a Syntax error
/// for command X without them, and "Copying from :F to :T". `memory`: the
/// pages from OSHWM to HIMEM, and neither disc in.
fn common(a: &mut Asm) {
    a.here("two_drives");
    a.stx(zp(z::T4));
    for to in [c::FROM, c::TO] {
        a.jsr("drive_argument");
        let given = a.local();
        a.bcc(given);
        a.ldx(zp(z::T4));
        a.jmp("syntax");
        a.bind(given);
        a.sta(zp(to));
    }
    a.ldx(imm(0));
    a.cmp(zp(c::FROM));
    let apart = a.local();
    a.bne(apart);
    a.inx();
    a.bind(apart);
    a.stx(zp(c::SWAPS));
    a.call_with_text("print_inline", &["Copying from :"]);
    a.lda(zp(c::FROM));
    a.jsr("print_digit");
    a.call_with_text("print_inline", &[" to :"]);
    a.lda(zp(c::TO));
    a.jsr("print_digit");
    a.jmp(abs(entry::OSNEWL));

    a.here("memory");
    for (call, at) in [(osbyte::OSHWM, c::BASE), (osbyte::HIMEM, c::PAGES)] {
        a.lda(imm(call));
        a.jsr(entry::OSBYTE);
        a.sty(zp(at));
    }
    a.lda(zp(c::PAGES));
    a.sec();
    a.sbc(zp(c::BASE));
    a.sta(zp(c::PAGES));
    a.lda(imm(NEITHER));
    a.sta(abs(c::IN));
    a.rts();

    // The disc to copy from, or to, asked for if it is not in.
    for (routine, wanted, prompt) in [
        ("source_disc", SOURCE_IN, "Insert source disk"),
        ("target_disc", TARGET_IN, "Insert destination disk"),
    ] {
        a.here(routine);
        a.lda(zp(c::SWAPS));
        a.beq(RTS);
        a.lda(imm(wanted));
        a.cmp(abs(c::IN));
        a.beq(RTS);
        a.sta(abs(c::IN));
        a.call_with_text("print_inline", &[prompt]);
        a.jmp("ask_key");
    }
    a.here("ask_key");
    a.call_with_text("print_inline", &[" and hit a key"]);
    a.jsr("read_key");
    a.jmp(abs(entry::OSNEWL));
}

/// `move_sectors`: LEFT sectors from SOURCE on FROM to TARGET on TO, a
/// memoryful at a time; `read_pass` and `write_pass` are one memoryful's
/// halves.
fn passes(a: &mut Asm) {
    a.here("move_sectors");
    a.lda(zp(c::LEFT));
    a.ora(zp(c::LEFT + 1));
    a.beq(RTS);
    a.jsr("read_pass");
    a.jsr("write_pass");
    a.jmp("move_sectors");

    a.here("read_pass");
    a.lda(zp(c::PAGES));
    a.ldx(zp(c::LEFT + 1));
    let counted = a.local();
    a.bne(counted);
    a.cmp(zp(c::LEFT));
    a.bcc(counted);
    a.lda(zp(c::LEFT));
    a.bind(counted);
    a.sta(zp(c::PASS));
    a.jsr("source_disc");
    a.lda(zp(c::FROM));
    a.ldx(imm(0));
    a.ldy(imm(0));
    a.jmp("pass_transfer");

    a.here("write_pass");
    a.jsr("target_disc");
    a.lda(zp(c::TO));
    a.ldx(imm(1));
    a.ldy(imm(2));
    a.jsr("pass_transfer");
    a.lda(zp(c::LEFT));
    a.sec();
    a.sbc(zp(c::PASS));
    a.sta(zp(c::LEFT));
    let same = a.local();
    a.bcs(same);
    a.dec(zp(c::LEFT + 1));
    a.bind(same);
    for at in [c::SOURCE, c::TARGET] {
        a.lda(abs(at));
        a.clc();
        a.adc(zp(c::PASS));
        a.sta(abs(at));
        let carried = a.local();
        a.bcc(carried);
        a.inc(abs(at + 1));
        a.bind(carried);
    }
    a.rts();

    // PASS sectors between memory and drive A: X 0 reading from SOURCE, 1
    // writing to TARGET, Y the offset of the sector number used.
    a.here("pass_transfer");
    a.sta(abs(page::OP_DRIVE));
    a.stx(abs(page::OP_WRITE));
    a.lda(abs(c::SOURCE).y());
    a.sta(abs(page::OP_SECTOR));
    a.lda(abs(c::SOURCE + 1).y());
    a.sta(abs(page::OP_SECTOR + 1));
    a.lda(zp(c::PASS));
    a.sta(abs(page::OP_SECTORS));
    a.sta(abs(page::OP_STORE + 1));
    a.lda(imm(0));
    a.sta(abs(page::OP_SECTORS + 1));
    a.sta(abs(page::OP_STORE));
    a.sta(abs(page::OP_STORE + 2));
    a.sta(abs(page::OP_ADDRESS));
    a.lda(zp(c::BASE));
    a.sta(abs(page::OP_ADDRESS + 1));
    a.jmp("transfer");
}

/// *BACKUP: every sector the source disc's catalogue counts, its catalogue
/// among them, onto a destination that counts at least as many, else Disk
/// full before anything is written.
fn backup(a: &mut Asm) {
    a.here("star_backup");
    a.ldx(imm(index_of("BACKUP")));
    a.jsr("two_drives");
    a.jsr("confirm");
    a.jsr("memory");
    for (disc, drive) in [("source_disc", c::FROM), ("target_disc", c::TO)] {
        a.jsr(disc);
        a.lda(zp(drive));
        a.sta(abs(page::OP_DRIVE));
        a.jsr("read_catalogue");
        if drive == c::FROM {
            a.lda(abs(cat::SECTORS));
            a.sta(zp(c::LEFT));
            a.lda(abs(cat::OPTION));
            a.and(imm(3));
            a.sta(zp(c::LEFT + 1));
        }
    }
    a.lda(abs(cat::SECTORS));
    a.cmp(zp(c::LEFT));
    a.lda(abs(cat::OPTION));
    a.and(imm(3));
    a.sbc(zp(c::LEFT + 1));
    far(a, Asm::bcs, "err_disk_full");
    a.lda(imm(0));
    for at in [c::SOURCE, c::TARGET] {
        a.sta(abs(at));
        a.sta(abs(at + 1));
    }
    a.jsr("move_sectors");
    // The destination's catalogue was written beneath the one held.
    a.lda(imm(super::disc::NO_CATALOGUE));
    a.sta(abs(page::CAT_DRIVE));
    a.rts();
}

/// *COPY: each file the pattern matches on the source drive copied to the
/// destination, replacing a file of its name there unless locked; each is
/// shown as *INFO shows it.
fn copy(a: &mut Asm) {
    a.here("star_copy");
    a.ldx(imm(index_of("COPY")));
    a.jsr("two_drives");
    a.jsr("next_word");
    let named = a.local();
    a.bne(named);
    a.ldx(imm(index_of("COPY")));
    a.jmp("syntax");
    a.bind(named);
    a.jsr("parse_name");
    a.lda(zp(c::FROM));
    a.sta(zp(z::DRIVE));
    for k in 0..9 {
        a.lda(zp(z::NAME + k));
        a.sta(abs(c::PATTERN + k));
    }
    a.jsr("memory");
    a.lda(imm(0));
    a.sta(abs(c::AT));
    a.jsr("copy_search");
    far(a, Asm::bcc, "err_not_found");

    a.here("copy_file");
    a.jsr("print_info");
    a.jsr("entry_info");
    a.jsr("length_sectors");
    a.lda(zp(z::T4));
    a.sta(zp(c::LEFT));
    a.lda(zp(z::T5));
    a.sta(zp(c::LEFT + 1));
    a.lda(abs(page::INFO_START));
    a.sta(abs(c::SOURCE));
    a.lda(abs(page::INFO_START + 1));
    a.sta(abs(c::SOURCE + 1));
    // The entry's own name, to make on the destination, and where the
    // search goes on from.
    a.ldy(zp(z::ENTRY));
    a.sty(abs(c::AT));
    for k in 0..u16::from(cat::NAME_LEN + 1) {
        a.lda(abs(cat::NAMES + k).y());
        if k == u16::from(cat::NAME_LEN) {
            a.and(imm(!cat::LOCKED));
        }
        a.sta(zp(z::NAME + k));
    }
    // The first memoryful, while the source is in.
    a.lda(zp(c::LEFT));
    a.ora(zp(c::LEFT + 1));
    let empty = a.local();
    a.beq(empty);
    a.jsr("read_pass");
    a.bind(empty);
    a.jsr("target_disc");
    a.lda(zp(c::TO));
    a.sta(zp(z::DRIVE));
    a.jsr("name_catalogue");
    a.jsr("find_first");
    let new = a.local();
    a.bcs(new);
    a.jsr("check_unlocked");
    a.jsr("check_closed");
    a.jsr("remove_entry");
    a.bind(new);
    a.jsr("length_sectors");
    a.jsr("allocate");
    a.lda(zp(z::T6));
    a.sta(abs(page::INFO_START));
    a.sta(abs(c::TARGET));
    a.lda(zp(z::T7));
    a.sta(abs(page::INFO_START + 1));
    a.sta(abs(c::TARGET + 1));
    a.jsr("make_room");
    a.jsr("set_entry");
    a.jsr("write_catalogue");
    a.lda(zp(c::LEFT));
    a.ora(zp(c::LEFT + 1));
    let written = a.local();
    a.beq(written);
    a.jsr("write_pass");
    a.jsr("move_sectors");
    a.bind(written);
    a.jsr("copy_search");
    far(a, Asm::bcs, "copy_file");
    a.rts();

    // The source catalogue read and the pattern's next match after AT.
    a.here("copy_search");
    a.jsr("source_disc");
    for k in 0..9 {
        a.lda(abs(c::PATTERN + k));
        a.sta(zp(z::NAME + k));
    }
    a.jsr("name_catalogue");
    a.lda(imm(0xFF));
    a.sta(abs(page::PATTERN));
    a.lda(abs(c::AT));
    a.sta(zp(z::ENTRY));
    a.jmp("find_next");
}

/// *COMPACT: every file moved down over the gaps below it, each shown as it
/// lands, then the free sectors left in hex; the catalogue is written only
/// if a file moved, as DFS 1.20 writes it. Open files stay open, as with
/// DFS 1.20: their buffers are written back first, and a channel follows
/// its file when it moves.
fn compact(a: &mut Asm) {
    a.here("star_compact");
    a.jsr("drive_argument");
    let given = a.local();
    a.bcc(given);
    a.lda(abs(page::DRIVE));
    a.bind(given);
    a.sta(zp(c::FROM));
    a.sta(zp(c::TO));
    each_open(a, |a| a.jsr("flush"));
    a.call_with_text("print_inline", &["Compacting :"]);
    a.lda(zp(c::FROM));
    a.jsr("print_digit");
    a.jsr(entry::OSNEWL);
    a.jsr("memory");
    a.lda(imm(0));
    a.sta(zp(c::SWAPS));
    a.sta(abs(c::MOVED));
    a.lda(zp(c::FROM));
    a.sta(abs(page::OP_DRIVE));
    a.jsr("read_catalogue");
    // The next free sector, from the catalogue up, each file in turn from
    // the lowest.
    a.lda(imm(cat::OWN_SECTORS));
    a.sta(abs(c::TARGET));
    a.lda(imm(0));
    a.sta(abs(c::TARGET + 1));
    a.lda(abs(cat::END));
    a.sta(zp(z::ENTRY));
    compact_files(a);
}

/// Each file from the lowest, moved down to the next free sector, then the
/// free sectors past the last.
fn compact_files(a: &mut Asm) {
    let file = a.here_local();
    let done = a.local();
    a.lda(zp(z::ENTRY));
    let more = a.local();
    a.bne(more);
    a.jmp(done);
    a.bind(more);
    a.tay();
    a.jsr("entry_start");
    a.jsr("entry_sectors");
    a.lda(zp(z::T0));
    a.sta(zp(c::LEFT));
    a.lda(zp(z::T1));
    a.sta(zp(c::LEFT + 1));
    a.lda(zp(z::T2));
    a.sta(abs(c::SOURCE));
    a.lda(zp(z::T3));
    a.sta(abs(c::SOURCE + 1));
    each_open(a, |a| {
        a.jsr("channel_is_entry");
        let other = a.local();
        a.bne(other);
        a.lda(abs(c::TARGET));
        a.sta(abs(ch::CHANNELS + u16::from(ch::START)).x());
        a.lda(abs(c::TARGET + 1));
        a.sta(abs(ch::CHANNELS + u16::from(ch::START) + 1).x());
        a.bind(other);
    });
    a.ldy(zp(z::ENTRY));
    a.lda(abs(c::TARGET));
    a.sta(abs(cat::INFO + u16::from(cat::START)).y());
    a.lda(abs(cat::INFO + u16::from(cat::MIXED)).y());
    a.and(imm(!3));
    a.ora(abs(c::TARGET + 1));
    a.sta(abs(cat::INFO + u16::from(cat::MIXED)).y());
    // A file already in place stays; any other moves down.
    let placed = a.local();
    a.lda(abs(c::SOURCE));
    a.cmp(abs(c::TARGET));
    let moves = a.local();
    a.bne(moves);
    a.lda(abs(c::SOURCE + 1));
    a.cmp(abs(c::TARGET + 1));
    a.bne(moves);
    // In place: TARGET past it without moving a sector.
    a.lda(abs(c::TARGET));
    a.clc();
    a.adc(zp(c::LEFT));
    a.sta(abs(c::TARGET));
    a.lda(abs(c::TARGET + 1));
    a.adc(zp(c::LEFT + 1));
    a.sta(abs(c::TARGET + 1));
    a.jmp(placed);
    a.bind(moves);
    a.jsr("move_sectors");
    a.lda(imm(0xFF));
    a.sta(abs(c::MOVED));
    a.bind(placed);
    a.jsr("print_info");
    a.lda(zp(z::ENTRY));
    a.sec();
    a.sbc(imm(cat::FIRST));
    a.sta(zp(z::ENTRY));
    a.jmp(file);
    a.bind(done);
    a.lda(abs(c::MOVED));
    let unmoved = a.local();
    a.beq(unmoved);
    a.jsr("write_catalogue");
    a.bind(unmoved);
    // The sectors past the last file.
    a.lda(abs(cat::SECTORS));
    a.sec();
    a.sbc(abs(c::TARGET));
    a.tax();
    a.lda(abs(cat::OPTION));
    a.and(imm(3));
    a.sbc(abs(c::TARGET + 1));
    a.jsr("print_digit");
    a.txa();
    a.jsr("print_hex");
    a.call_with_text("print_inline", &[" free sectors\r"]);
    a.rts();
}

/// *DESTROY: the unlocked files the pattern matches listed, then deleted
/// if the answer is Y.
fn destroy(a: &mut Asm) {
    a.here("star_destroy");
    a.jsr("confirm");
    a.jsr("first_of_pattern");
    let list = a.here_local();
    a.ldy(zp(z::ENTRY));
    a.lda(abs(cat::NAMES + u16::from(cat::ENTRY_DIR)).y());
    let skip = a.local();
    a.bmi(skip);
    a.jsr(entry::OSASCI);
    a.lda(imm(b'.'));
    a.jsr(entry::OSASCI);
    a.jsr("print_entry_name");
    a.jsr(entry::OSNEWL);
    a.bind(skip);
    a.jsr("find_next");
    a.bcc(list);
    a.jsr("go");
    // Each unlocked match removed, the search resumed where it was.
    a.jsr("find_pattern");
    let each = a.here_local();
    let finished = a.local();
    a.bcs(finished);
    a.ldy(zp(z::ENTRY));
    a.lda(abs(cat::NAMES + u16::from(cat::ENTRY_DIR)).y());
    let next = a.local();
    a.bmi(next);
    a.jsr("check_closed");
    a.jsr("remove_entry");
    a.lda(zp(z::ENTRY));
    a.sec();
    a.sbc(imm(cat::FIRST));
    a.sta(zp(z::ENTRY));
    a.bind(next);
    a.jsr("find_next");
    a.jmp(each);
    a.bind(finished);
    a.jsr("write_catalogue");
    a.call_with_text("print_inline", &["Deleted\r"]);
    a.rts();
}
