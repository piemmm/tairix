//! The five channels: OSFIND, OSBGET, OSBPUT, OSARGS, OSGBPB's transfers
//! and FSCV's end-of-file test.
//!
//! Each channel buffers one sector of its file. A file opened for output is
//! given sixty-four sectors, recorded as its length until it closes; a file
//! that outgrows its sectors grows to the next multiple of 256 as far as
//! the next file allows, a step at a time as DFS 1.20 grows it, or cannot
//! extend. A file may be open for input on
//! several channels, but for update or output on one alone. As DFS 1.20
//! does, a read at the end returns &FE with C set once, and the next is an
//! EOF error.

use super::far;
use super::ws::{catalogue as cat, channel as ch, page, zp as z};
use crate::rom::asm::{abs, imm, lo, zp, Acc, Asm, Operand, RTS};
use crate::rom::os::{filing_system, zp as os};

/// A channel block's field, by its offset from the block at X.
fn field(offset: u8) -> Operand {
    abs(ch::CHANNELS + u16::from(offset)).x()
}

/// A catalogue entry's field in the second sector, from the entry at Y.
fn info(offset: u8) -> Operand {
    abs(cat::INFO + u16::from(offset)).y()
}

/// OSFIND's modes.
const INPUT: u8 = 0x40;
const OUTPUT: u8 = 0x80;
const UPDATE: u8 = 0xC0;
/// What a read past the end returns.
const PAST_END: u8 = 0xFE;
/// OSARGS's reasons.
const ARGS_PTR: u8 = 0;
const ARGS_SET_PTR: u8 = 1;
const ARGS_EXT: u8 = 2;
const ARGS_ENSURE: u8 = 0xFF;
/// OSGBPB's block, by offset, and its first reason that is not a transfer.
mod gbpb {
    pub const HANDLE: u8 = 0;
    pub const ADDRESS: u8 = 1;
    pub const COUNT: u8 = 5;
    pub const PTR: u8 = 9;
    pub const TITLE: u8 = 5;
    pub const FIRST_GET: u8 = 3;
}

pub(super) fn emit(a: &mut Asm) {
    find(a);
    opening(a);
    closing(a);
    checks(a);
    bytes(a);
    buffers(a);
    growth(a);
    args(a);
    gbpb_data(a);
}

/// For each open channel, X its block: `body` runs with CHANNEL set, and X
/// is reloaded after it.
pub(super) fn each_open(a: &mut Asm, body: impl FnOnce(&mut Asm)) {
    a.ldx(imm(ch::SIZE));
    let next = a.here_local();
    a.lda(field(ch::FLAGS));
    let closed = a.local();
    a.bpl(closed);
    a.stx(zp(z::CHANNEL));
    body(a);
    a.ldx(zp(z::CHANNEL));
    a.bind(closed);
    a.txa();
    a.clc();
    a.adc(imm(ch::SIZE));
    a.tax();
    a.cmp(imm(ch::END));
    a.bne(next);
}

/// OSFIND: A zero closes handle Y, or every file for Y zero; otherwise the
/// file named at X and Y opens, its handle in A, or zero if it is not there
/// to read or update.
fn find(a: &mut Asm) {
    a.here("dfs_find");
    a.sta(zp(z::SAVE_A));
    a.stx(zp(z::SAVE_X));
    a.sty(zp(z::SAVE_Y));
    a.jsr("claim_workspace");
    a.lda(zp(z::SAVE_A));
    let open = a.local();
    a.bne(open);
    a.ldy(zp(z::SAVE_Y));
    let all = a.local();
    a.beq(all);
    a.jsr("channel_check");
    a.jsr("close_channel");
    let restore = a.local();
    a.jmp(restore);
    a.bind(all);
    a.jsr("close_all");
    a.bind(restore);
    a.lda(zp(z::SAVE_A));
    a.jmp("restore_xy");

    a.bind(open);
    a.and(imm(UPDATE));
    a.sta(zp(z::MODE));
    a.lda(zp(z::SAVE_X));
    a.sta(zp(os::TEXT));
    a.lda(zp(z::SAVE_Y));
    a.sta(zp(os::TEXT + 1));
    a.ldy(imm(0));
    a.jsr("parse_name");
    a.jsr("name_catalogue");
    a.lda(zp(z::MODE));
    a.cmp(imm(OUTPUT));
    far(a, Asm::bne, "open_output");
    // As DFS 1.20 does, the file is checked before a channel is sought.
    a.jsr("find_first");
    let missing = a.local();
    a.bcs(missing);
    a.lda(zp(z::MODE));
    a.cmp(imm(INPUT));
    let update = a.local();
    a.bne(update);
    a.jsr("check_unwritten");
    let checked = a.local();
    a.jmp(checked);
    a.bind(update);
    a.jsr("check_closed");
    a.bind(checked);
    a.jsr("free_channel");
    a.jmp("opened");
    a.bind(missing);
    a.lda(imm(0));
    // X and Y as they were, N and Z set by A, C kept, as DFS 1.20 leaves
    // them.
    a.here("restore_xy");
    a.ldx(zp(z::SAVE_X));
    a.ldy(zp(z::SAVE_Y));
    a.ora(imm(0));
    a.rts();
}

/// A new file for output, any old one of the name deleted first unless
/// locked or open; then the channel at CHANNEL set up from the entry at
/// ENTRY and its handle returned.
fn opening(a: &mut Asm) {
    a.here("open_output");
    a.jsr("find_first");
    let create = a.local();
    a.bcs(create);
    a.jsr("check_unlocked");
    a.jsr("check_closed");
    a.jsr("free_channel");
    a.jsr("remove_entry");
    let made = a.local();
    a.jmp(made);
    a.bind(create);
    a.jsr("free_channel");
    a.bind(made);
    // Load and execution addresses zero, the length the room it is given.
    a.ldx(imm(11));
    a.lda(imm(0));
    let clear = a.here_local();
    a.sta(abs(page::INFO_LOAD).x());
    a.dex();
    a.bpl(clear);
    a.sta(zp(z::T5));
    a.lda(imm(ch::OUTPUT_ROOM));
    a.sta(zp(z::T4));
    a.sta(abs(page::INFO_LENGTH + 1));
    a.jsr("allocate");
    a.lda(zp(z::T6));
    a.sta(abs(page::INFO_START));
    a.lda(zp(z::T7));
    a.sta(abs(page::INFO_START + 1));
    a.jsr("make_room");
    a.jsr("set_entry");
    a.jsr("write_catalogue");

    a.here("opened");
    a.ldx(zp(z::CHANNEL));
    a.ldy(zp(z::ENTRY));
    for k in 0..=cat::NAME_LEN {
        a.lda(abs(cat::NAMES + u16::from(k)).y());
        a.sta(field(ch::NAME + k));
    }
    a.lda(abs(page::CAT_DRIVE));
    a.sta(field(ch::DRIVE));
    a.jsr("entry_info");
    a.jsr("length_sectors");
    a.ldx(zp(z::CHANNEL));
    for (from, to) in [
        (page::INFO_START, ch::START),
        (page::INFO_START + 1, ch::START + 1),
    ] {
        a.lda(abs(from));
        a.sta(field(to));
    }
    a.lda(zp(z::T4));
    a.sta(field(ch::ROOM));
    a.lda(zp(z::T5));
    a.sta(field(ch::ROOM + 1));
    // PTR zero; EXT the length, but nothing for output.
    a.ldy(zp(z::MODE));
    for k in 0..3 {
        a.lda(imm(0));
        a.sta(field(ch::PTR + k));
        a.cpy(imm(OUTPUT));
        let empty = a.local();
        a.beq(empty);
        a.lda(abs(page::INFO_LENGTH + u16::from(k)));
        a.bind(empty);
        a.sta(field(ch::EXT + k));
    }
    // The flags for the mode: output's length is to be recorded at close.
    a.lda(imm(ch::OPEN));
    let set = a.local();
    a.cpy(imm(INPUT));
    a.beq(set);
    a.ora(imm(ch::WRITABLE));
    a.cpy(imm(UPDATE));
    a.beq(set);
    a.ora(imm(ch::EXTENDED));
    a.bind(set);
    a.sta(field(ch::FLAGS));
    // The handle numbers the block from the first, the buffer page with it.
    a.txa();
    for _ in 0..5 {
        a.lsr(Acc);
    }
    a.clc();
    a.adc(imm(ch::FIRST_HANDLE - 1));
    a.pha();
    a.adc(imm(ch::FIRST_BUFFER - ch::FIRST_HANDLE));
    a.sta(field(ch::BUFFER));
    a.pla();
    a.jmp("restore_xy");
}

/// `close_channel` closes the channel at X: its buffer written back, and
/// its extent recorded as its length if it has changed. `close_all` closes
/// every open channel.
fn closing(a: &mut Asm) {
    a.here("close_all");
    each_open(a, |a| a.jsr("close_channel"));
    a.rts();

    a.here("close_channel");
    a.jsr("flush");
    a.lda(field(ch::FLAGS));
    a.and(imm(ch::EXTENDED));
    let recorded = a.local();
    a.beq(recorded);
    a.jsr("channel_entry");
    a.ldy(zp(z::ENTRY));
    a.lda(field(ch::EXT));
    a.sta(info(cat::LENGTH));
    a.lda(field(ch::EXT + 1));
    a.sta(info(cat::LENGTH + 1));
    a.lda(field(ch::EXT + 2));
    a.jsr("set_length_top");
    a.jsr("write_catalogue");
    a.ldx(zp(z::CHANNEL));
    a.bind(recorded);
    a.lda(imm(0));
    a.sta(field(ch::FLAGS));
    a.rts();

    // The top two bits of the length of the entry at Y set from A's two.
    a.here("set_length_top");
    a.and(imm(3));
    for _ in 0..cat::LENGTH_SHIFT {
        a.asl(Acc);
    }
    a.sta(zp(z::T0));
    a.lda(info(cat::MIXED));
    a.and(imm(!(3 << cat::LENGTH_SHIFT)));
    a.ora(zp(z::T0));
    a.sta(info(cat::MIXED));
    a.rts();
}

/// `channel_check` finds handle Y's block, at X and CHANNEL, or is a
/// Channel error; `free_channel` finds a closed one or is Too many open;
/// `check_closed` and `check_unwritten` are an Open error if the entry at
/// ENTRY is open at all, or open to write; `channel_entry` finds the channel
/// at X's entry in its drive's catalogue.
fn checks(a: &mut Asm) {
    a.here("channel_check");
    a.cpy(imm(ch::FIRST_HANDLE));
    far(a, Asm::bcs, "err_channel");
    a.cpy(imm(ch::LAST_HANDLE + 1));
    far(a, Asm::bcc, "err_channel");
    a.tya();
    a.sec();
    a.sbc(imm(ch::FIRST_HANDLE - 1));
    for _ in 0..5 {
        a.asl(Acc);
    }
    a.tax();
    a.lda(field(ch::FLAGS));
    far(a, Asm::bmi, "err_channel");
    a.stx(zp(z::CHANNEL));
    a.rts();

    a.here("free_channel");
    a.ldx(imm(ch::SIZE));
    let next = a.here_local();
    a.lda(field(ch::FLAGS));
    let free = a.local();
    a.bpl(free);
    a.txa();
    a.clc();
    a.adc(imm(ch::SIZE));
    a.tax();
    a.cmp(imm(ch::END));
    a.bne(next);
    a.jmp("err_too_many_open");
    a.bind(free);
    a.stx(zp(z::CHANNEL));
    a.rts();

    open_checks(a);
}

/// Whether an entry is open, and which channel holds it.
fn open_checks(a: &mut Asm) {
    a.here("check_closed");
    a.lda(imm(ch::OPEN));
    let scan = a.local();
    a.bne(scan);
    a.here("check_unwritten");
    a.lda(imm(ch::WRITABLE));
    a.bind(scan);
    a.sta(zp(z::T3));
    a.ldx(imm(ch::SIZE));
    let channel = a.here_local();
    a.lda(field(ch::FLAGS));
    a.and(zp(z::T3));
    let other = a.local();
    a.beq(other);
    a.jsr("channel_is_entry");
    far(a, Asm::bne, "err_open");
    a.bind(other);
    a.txa();
    a.clc();
    a.adc(imm(ch::SIZE));
    a.tax();
    a.cmp(imm(ch::END));
    a.bne(channel);
    a.rts();

    // Z set if the channel at X holds the entry at ENTRY on CAT_DRIVE: the
    // same drive, first sector, directory and name. X is kept.
    a.here("channel_is_entry");
    a.lda(field(ch::DRIVE));
    a.cmp(abs(page::CAT_DRIVE));
    a.bne(RTS);
    a.ldy(zp(z::ENTRY));
    a.lda(field(ch::START));
    a.cmp(info(cat::START));
    a.bne(RTS);
    a.lda(info(cat::MIXED));
    a.and(imm(3));
    a.cmp(field(ch::START + 1));
    a.bne(RTS);
    a.stx(zp(z::T1));
    a.lda(imm(cat::NAME_LEN + 1));
    a.sta(zp(z::T2));
    let compare = a.here_local();
    a.lda(field(ch::NAME));
    a.eor(abs(cat::NAMES).y());
    a.and(imm(!cat::LOCKED));
    let differs = a.local();
    a.bne(differs);
    a.inx();
    a.iny();
    a.dec(zp(z::T2));
    a.bne(compare);
    a.bind(differs);
    a.php();
    a.ldx(zp(z::T1));
    a.plp();
    a.rts();

    a.here("channel_entry");
    a.lda(field(ch::DRIVE));
    a.sta(abs(page::OP_DRIVE));
    a.jsr("read_catalogue");
    a.ldx(zp(z::CHANNEL));
    a.lda(imm(0));
    a.sta(zp(z::ENTRY));
    let entry = a.here_local();
    a.lda(zp(z::ENTRY));
    a.clc();
    a.adc(imm(cat::FIRST));
    a.sta(zp(z::ENTRY));
    a.cmp(abs(cat::END));
    let looking = a.local();
    a.beq(looking);
    far(a, Asm::bcc, "err_disk_changed");
    a.bind(looking);
    a.jsr("channel_is_entry");
    a.bne(entry);
    a.rts();
}

/// OSBGET and OSBPUT on handle Y, X and Y kept; `bget_core` and
/// `bput_core` on the channel at X, OSGBPB's too.
fn bytes(a: &mut Asm) {
    a.here("dfs_bget");
    a.stx(zp(z::SAVE_X));
    a.sty(zp(z::SAVE_Y));
    a.jsr("claim_workspace");
    a.jsr("channel_check");
    a.jsr("bget_core");
    a.jmp("restore_xy");

    a.here("bget_core");
    a.jsr("at_end");
    let read = a.local();
    a.bcc(read);
    a.lda(field(ch::FLAGS));
    a.and(imm(ch::AT_END));
    far(a, Asm::beq, "err_eof");
    a.lda(field(ch::FLAGS));
    a.ora(imm(ch::AT_END));
    a.sta(field(ch::FLAGS));
    a.lda(imm(PAST_END));
    a.sec();
    a.rts();
    a.bind(read);
    a.jsr("load_buffer");
    a.ldy(field(ch::PTR));
    a.lda(zp(z::BUFFER).ind_y());
    a.pha();
    a.jsr("advance");
    a.pla();
    a.clc();
    a.rts();

    a.here("dfs_bput");
    a.sta(zp(z::SAVE_A));
    a.stx(zp(z::SAVE_X));
    a.sty(zp(z::SAVE_Y));
    a.jsr("claim_workspace");
    a.jsr("channel_check");
    a.lda(zp(z::SAVE_A));
    a.jsr("bput_core");
    a.lda(zp(z::SAVE_A));
    a.jmp("restore_xy");

    a.here("bput_core");
    a.pha();
    a.lda(field(ch::FLAGS));
    a.and(imm(ch::WRITABLE));
    far(a, Asm::bne, "err_channel_read_only");
    a.lda(field(ch::DIR));
    far(a, Asm::bpl, "err_locked");
    a.jsr("room_for_ptr");
    a.jsr("load_buffer");
    a.ldy(field(ch::PTR));
    a.pla();
    a.sta(zp(z::BUFFER).ind_y());
    a.lda(field(ch::FLAGS));
    a.ora(imm(ch::DIRTY));
    a.sta(field(ch::FLAGS));
    a.jsr("advance");
    a.jsr("at_end");
    a.bcc(RTS);
    a.fall_into("ptr_to_ext");

    // EXT moved out to PTR, to be recorded when the file closes.
    a.here("ptr_to_ext");
    for k in 0..3 {
        a.lda(field(ch::PTR + k));
        a.sta(field(ch::EXT + k));
    }
    a.lda(field(ch::FLAGS));
    a.ora(imm(ch::EXTENDED));
    a.sta(field(ch::FLAGS));
    a.rts();

    // PTR one on.
    a.here("advance");
    a.inc(field(ch::PTR));
    a.bne(RTS);
    a.inc(field(ch::PTR + 1));
    a.bne(RTS);
    a.inc(field(ch::PTR + 2));
    a.rts();

    // C set if PTR has reached EXT.
    a.here("at_end");
    a.lda(field(ch::PTR));
    a.cmp(field(ch::EXT));
    a.lda(field(ch::PTR + 1));
    a.sbc(field(ch::EXT + 1));
    a.lda(field(ch::PTR + 2));
    a.sbc(field(ch::EXT + 2));
    a.rts();

    // FSCV 1: X &FF if handle X is at its end, else 0.
    a.here("dfs_eof");
    a.txa();
    a.tay();
    a.jsr("channel_check");
    a.jsr("at_end");
    a.ldx(imm(0));
    a.bcc(RTS);
    a.dex();
    a.rts();
}

/// The channel at X's buffer: `load_buffer` makes it hold the sector PTR is
/// in, reading it if it starts before EXT and clearing it otherwise, with
/// BUFFER pointing at it; `flush` writes it back if it has changed, which
/// reads its drive's catalogue.
fn buffers(a: &mut Asm) {
    a.here("load_buffer");
    a.lda(field(ch::FLAGS));
    a.and(imm(ch::VALID));
    let fetch = a.local();
    a.beq(fetch);
    a.lda(field(ch::HELD));
    a.cmp(field(ch::PTR + 1));
    a.bne(fetch);
    a.lda(field(ch::HELD + 1));
    a.cmp(field(ch::PTR + 2));
    a.beq("buffer_pointer");
    a.bind(fetch);
    a.jsr("flush");
    a.lda(field(ch::PTR + 1));
    a.sta(field(ch::HELD));
    a.lda(field(ch::PTR + 2));
    a.sta(field(ch::HELD + 1));
    a.lda(field(ch::FLAGS));
    a.ora(imm(ch::VALID));
    a.and(imm(!ch::DIRTY));
    a.sta(field(ch::FLAGS));
    // The sectors EXT reaches into, against the one held.
    a.lda(field(ch::EXT + 1));
    a.sta(zp(z::T0));
    a.lda(field(ch::EXT + 2));
    a.sta(zp(z::T1));
    a.lda(field(ch::EXT));
    let rounded = a.local();
    a.beq(rounded);
    a.inc(zp(z::T0));
    a.bne(rounded);
    a.inc(zp(z::T1));
    a.bind(rounded);
    a.lda(field(ch::HELD));
    a.cmp(zp(z::T0));
    a.lda(field(ch::HELD + 1));
    a.sbc(zp(z::T1));
    let clear = a.local();
    a.bcs(clear);
    a.lda(imm(0));
    a.jsr("buffer_transfer");
    a.jmp("buffer_pointer");
    a.bind(clear);
    a.jsr("buffer_pointer");
    a.lda(imm(0));
    a.tay();
    let zero = a.here_local();
    a.sta(zp(z::BUFFER).ind_y());
    a.iny();
    a.bne(zero);
    a.rts();

    a.here("buffer_pointer");
    a.lda(imm(0));
    a.sta(zp(z::BUFFER));
    a.lda(field(ch::BUFFER));
    a.sta(zp(z::BUFFER + 1));
    a.rts();

    // Written only to the disc that holds the file: one changed is Disk
    // changed before a byte reaches it.
    a.here("flush");
    a.lda(field(ch::FLAGS));
    a.and(imm(ch::DIRTY));
    a.beq(RTS);
    a.jsr("channel_entry");
    a.lda(field(ch::FLAGS));
    a.and(imm(!ch::DIRTY));
    a.sta(field(ch::FLAGS));
    a.lda(imm(1));
    a.fall_into("buffer_transfer");

    // The buffer's sector read with A zero, or written; X reloaded.
    a.here("buffer_transfer");
    a.sta(abs(page::OP_WRITE));
    a.lda(field(ch::DRIVE));
    a.sta(abs(page::OP_DRIVE));
    a.lda(field(ch::START));
    a.clc();
    a.adc(field(ch::HELD));
    a.sta(abs(page::OP_SECTOR));
    a.lda(field(ch::START + 1));
    a.adc(field(ch::HELD + 1));
    a.sta(abs(page::OP_SECTOR + 1));
    a.lda(imm(1));
    a.sta(abs(page::OP_SECTORS));
    a.sta(abs(page::OP_STORE + 1));
    a.lda(imm(0));
    a.sta(abs(page::OP_SECTORS + 1));
    a.sta(abs(page::OP_STORE));
    a.sta(abs(page::OP_STORE + 2));
    a.sta(abs(page::OP_ADDRESS));
    a.lda(field(ch::BUFFER));
    a.sta(abs(page::OP_ADDRESS + 1));
    a.jsr("transfer");
    a.ldx(zp(z::CHANNEL));
    a.rts();
}

/// `room_for_ptr`: the channel at X may hold a byte at PTR; `room_for`,
/// one in the sector in T6 and T7. A file without room grows to the next
/// multiple of 256 sectors, or to the next file's start or the disc's end
/// if nearer, recorded in the catalogue as its length while it is open,
/// until the sector fits; Can't extend when it can grow no further.
fn growth(a: &mut Asm) {
    a.here("room_for_ptr");
    a.lda(field(ch::PTR + 1));
    a.sta(zp(z::T6));
    a.lda(field(ch::PTR + 2));
    a.sta(zp(z::T7));
    a.here("room_for");
    a.lda(zp(z::T6));
    a.cmp(field(ch::ROOM));
    a.lda(zp(z::T7));
    a.sbc(field(ch::ROOM + 1));
    a.bcc(RTS);
    a.jsr("channel_entry");
    // The next file's first sector, or the disc's end.
    a.ldy(zp(z::ENTRY));
    a.cpy(imm(cat::FIRST));
    let last = a.local();
    a.beq(last);
    a.tya();
    a.sec();
    a.sbc(imm(cat::FIRST));
    a.tay();
    a.jsr("entry_start");
    let limit = a.local();
    a.jmp(limit);
    a.bind(last);
    a.lda(abs(cat::SECTORS));
    a.sta(zp(z::T2));
    a.lda(abs(cat::OPTION));
    a.and(imm(3));
    a.sta(zp(z::T3));
    // The limit, as sectors from the file's first.
    a.bind(limit);
    a.lda(zp(z::T2));
    a.sec();
    a.sbc(field(ch::START));
    a.sta(zp(z::T2));
    a.lda(zp(z::T3));
    a.sbc(field(ch::START + 1));
    a.sta(zp(z::T3));
    a.lda(field(ch::ROOM));
    a.cmp(zp(z::T2));
    a.lda(field(ch::ROOM + 1));
    a.sbc(zp(z::T3));
    far(a, Asm::bcc, "err_cant_extend");
    // The next multiple of 256 sectors, unless the limit comes first.
    a.ldy(field(ch::ROOM + 1));
    a.iny();
    a.lda(imm(0));
    a.cmp(zp(z::T2));
    a.tya();
    a.sbc(zp(z::T3));
    let limited = a.local();
    a.bcs(limited);
    a.sty(zp(z::T3));
    a.lda(imm(0));
    a.sta(zp(z::T2));
    a.bind(limited);
    a.lda(zp(z::T2));
    a.sta(field(ch::ROOM));
    a.lda(zp(z::T3));
    a.sta(field(ch::ROOM + 1));
    // Recorded as the length: ROOM whole sectors.
    a.ldy(zp(z::ENTRY));
    a.lda(imm(0));
    a.sta(info(cat::LENGTH));
    a.lda(zp(z::T2));
    a.sta(info(cat::LENGTH + 1));
    a.lda(zp(z::T3));
    a.jsr("set_length_top");
    a.jsr("write_catalogue");
    a.ldx(zp(z::CHANNEL));
    a.lda(field(ch::FLAGS));
    a.ora(imm(ch::EXTENDED));
    a.sta(field(ch::FLAGS));
    a.jmp("room_for");
}

/// OSARGS: with Y zero, A 0 names the filing system, 1 gives the address
/// of a *RUN command's tail at X, and &FF writes every buffer back; with a
/// handle, A 0 and 2 read PTR and EXT to X, 1 sets PTR from X and &FF
/// writes the file's buffer back. A returns zero but for naming the
/// filing system or an unknown reason.
fn args(a: &mut Asm) {
    a.here("dfs_args");
    a.sta(zp(z::SAVE_A));
    a.stx(zp(z::SAVE_X));
    a.sty(zp(z::SAVE_Y));
    a.jsr("claim_workspace");
    a.ldy(zp(z::SAVE_Y));
    let file = a.local();
    a.bne(file);
    let zero = a.local();
    let out = a.local();
    a.lda(zp(z::SAVE_A));
    let not_fs = a.local();
    a.bne(not_fs);
    a.lda(imm(filing_system::DFS));
    a.jmp(out);
    a.bind(not_fs);
    a.ldx(zp(z::SAVE_X));
    a.cmp(imm(ARGS_SET_PTR));
    let not_tail = a.local();
    a.bne(not_tail);
    a.lda(abs(page::TAIL));
    a.sta(zp(0).x());
    a.lda(abs(page::TAIL + 1));
    a.sta(zp(1).x());
    a.lda(imm(0xFF));
    a.sta(zp(2).x());
    a.sta(zp(3).x());
    a.jmp(zero);
    a.bind(not_tail);
    a.cmp(imm(ARGS_ENSURE));
    let unknown = a.local();
    a.bne(unknown);
    each_open(a, |a| a.jsr("flush"));
    a.jmp(zero);
    a.bind(unknown);
    a.jmp(out);

    a.bind(file);
    a.jsr("channel_check");
    a.ldy(zp(z::SAVE_X));
    a.lda(zp(z::SAVE_A));
    for (reason, from) in [(ARGS_PTR, ch::PTR), (ARGS_EXT, ch::EXT)] {
        let other = a.local();
        a.cmp(imm(reason));
        a.bne(other);
        for k in 0..3 {
            a.lda(field(from + k));
            a.sta(abs(u16::from(k)).y());
        }
        a.lda(imm(0));
        a.sta(abs(3).y());
        a.jmp(zero);
        a.bind(other);
    }
    a.cmp(imm(ARGS_SET_PTR));
    let not_set = a.local();
    a.bne(not_set);
    a.jsr("set_ptr");
    a.jmp(zero);
    a.bind(not_set);
    a.cmp(imm(ARGS_ENSURE));
    a.bne(zero);
    a.jsr("flush");
    a.bind(zero);
    a.lda(imm(0));
    a.bind(out);
    a.jmp("restore_xy");

    // PTR from the four bytes at zero page Y for the channel at X; a file
    // being written grows to reach it.
    a.here("set_ptr");
    for k in 0..3 {
        a.lda(abs(u16::from(k)).y());
        a.sta(field(ch::PTR + k));
    }
    a.lda(field(ch::FLAGS));
    a.and(imm(!ch::AT_END));
    a.sta(field(ch::FLAGS));
    // Past EXT, a file being written grows to hold the byte before PTR.
    a.lda(field(ch::EXT));
    a.cmp(field(ch::PTR));
    a.lda(field(ch::EXT + 1));
    a.sbc(field(ch::PTR + 1));
    a.lda(field(ch::EXT + 2));
    a.sbc(field(ch::PTR + 2));
    let within = a.local();
    a.bcs(within);
    a.lda(field(ch::FLAGS));
    a.and(imm(ch::WRITABLE));
    a.beq(within);
    a.lda(field(ch::PTR));
    a.cmp(imm(1));
    a.lda(field(ch::PTR + 1));
    a.sbc(imm(0));
    a.sta(zp(z::T6));
    a.lda(field(ch::PTR + 2));
    a.sbc(imm(0));
    a.sta(zp(z::T7));
    a.jsr("room_for");
    a.jmp("ptr_to_ext");
    a.bind(within);
    a.rts();
}

/// OSGBPB 1 to 4: bytes between the open file and memory, from the block's
/// PTR for 1 and 3; the block left with the address and PTR moved on and
/// the bytes not moved, C set if a read met the end first. 5 to 8 read the
/// catalogue, in `gbpb_info`.
fn gbpb_data(a: &mut Asm) {
    a.here("dfs_gbpb");
    a.sta(zp(z::SAVE_A));
    a.stx(zp(z::BLOCK));
    a.sty(zp(z::BLOCK + 1));
    a.jsr("claim_workspace");
    a.lda(zp(z::SAVE_A));
    a.cmp(imm(gbpb::TITLE));
    far(a, Asm::bcc, "gbpb_info");
    a.cmp(imm(1));
    far(a, Asm::bcs, "gbpb_return");
    a.ldy(imm(gbpb::HANDLE));
    a.lda(zp(z::BLOCK).ind_y());
    a.tay();
    a.jsr("channel_check");
    a.lda(zp(z::SAVE_A));
    a.lsr(Acc);
    let pointed = a.local();
    a.bcc(pointed);
    // The block's PTR, through zero page as OSARGS takes it.
    for k in 0..4 {
        a.ldy(imm(gbpb::PTR + k));
        a.lda(zp(z::BLOCK).ind_y());
        a.sta(zp(z::T4 + u16::from(k)));
    }
    a.ldy(imm(lo(z::T4)));
    a.jsr("set_ptr");
    a.bind(pointed);

    let each = a.here_local();
    a.ldy(imm(gbpb::COUNT));
    a.lda(zp(z::BLOCK).ind_y());
    for k in 1..4 {
        a.ldy(imm(gbpb::COUNT + k));
        a.ora(zp(z::BLOCK).ind_y());
    }
    let finished = a.local();
    a.beq(finished);
    a.ldy(imm(gbpb::ADDRESS));
    a.lda(zp(z::BLOCK).ind_y());
    a.sta(zp(z::POINTER));
    a.iny();
    a.lda(zp(z::BLOCK).ind_y());
    a.sta(zp(z::POINTER + 1));
    a.ldx(zp(z::CHANNEL));
    a.lda(zp(z::SAVE_A));
    a.cmp(imm(gbpb::FIRST_GET));
    let put = a.local();
    a.bcc(put);
    a.jsr("at_end");
    let short = a.local();
    a.bcs(short);
    a.jsr("bget_core");
    a.ldy(imm(0));
    a.sta(zp(z::POINTER).ind_y());
    let moved = a.local();
    a.jmp(moved);
    a.bind(put);
    a.ldy(imm(0));
    a.lda(zp(z::POINTER).ind_y());
    a.jsr("bput_core");
    a.bind(moved);
    // The address one on, the count one less.
    a.ldy(imm(gbpb::ADDRESS));
    a.sec();
    for _ in 0..4 {
        a.lda(zp(z::BLOCK).ind_y());
        a.adc(imm(0));
        a.sta(zp(z::BLOCK).ind_y());
        a.iny();
    }
    a.ldy(imm(gbpb::COUNT));
    a.clc();
    for _ in 0..4 {
        a.lda(zp(z::BLOCK).ind_y());
        a.sbc(imm(0));
        a.sta(zp(z::BLOCK).ind_y());
        a.iny();
    }
    a.jmp(each);
    a.bind(finished);
    a.clc();
    a.bind(short);
    a.php();
    a.ldx(zp(z::CHANNEL));
    a.ldy(imm(gbpb::PTR));
    for k in 0..3 {
        a.lda(field(ch::PTR + k));
        a.sta(zp(z::BLOCK).ind_y());
        a.iny();
    }
    a.lda(imm(0));
    a.sta(zp(z::BLOCK).ind_y());
    a.plp();
    a.here("gbpb_return");
    a.ldx(zp(z::BLOCK));
    a.ldy(zp(z::BLOCK + 1));
    a.lda(imm(0));
    a.rts();
}
