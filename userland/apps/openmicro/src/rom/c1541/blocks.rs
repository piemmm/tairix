//! Direct access, as the original's: a channel opened with `#` holds a
//! buffer of its own, read and written a byte at a time through its
//! pointer, and the block commands move blocks between such a buffer and
//! the disk. `U1` and `B-R` read, `U2` and `B-W` write, `B-P` points, `B-E`
//! reads and runs, and `B-A` and `B-F` allocate and free in the BAM.
//!
//! A channel's pointer is the offset of the byte it holds ready to send;
//! once that byte goes the pointer moves on, past the buffer's last byte
//! back to 1, and a byte written goes at the pointer.

use super::commands::table;
use super::errors::number;
use super::files::{mode, EOI, LAST_OFFSET};
use super::ws::{page, zp, BLOCK_NUMBERS, BUFFERS, CHANNEL_BUFFERS, NO_FILE};
use crate::media::c1541::layout::TRACKS;
use crate::rom::asm::{abs, hi, imm, lo, zp as z, Acc, Asm, RTS};

/// The block commands by the letter after `B-`.
const BLOCK_COMMANDS: [(u8, &str); 6] = [
    (b'R', "block_read"),
    (b'W', "block_write"),
    (b'P', "block_pointer"),
    (b'A', "block_allocate"),
    (b'F', "block_free"),
    (b'E', "block_execute"),
];
/// The offset a `B-` command's numbers start at where it has no colon.
const BLOCK_NUMBERS_AT: u8 = 3;
/// The separators between a command's numbers.
const SEPARATORS: [u8; 2] = *b" ,";
/// Where `B-R` reads from: the byte after the one holding the last
/// byte's offset.
const COUNTED_FROM: u8 = 1;
/// A direct channel's first byte to send, which its buffer's number
/// stands in for, and the offset its writes start at.
const DIRECT_FIRST: u8 = 1;
/// Where a pointer past its buffer's last byte comes back to.
const WRAPPED: u8 = 1;

pub fn emit(a: &mut Asm) {
    direct(a);
    numbers(a);
    commands(a);
    reading(a);
    allocation(a);
    a.later(|a| table(a, "block_commands", &BLOCK_COMMANDS));
}

/// `open_direct`: the channel's OPEN with a name of `#`, the highest free
/// buffer, or `#` and a number, that buffer where it is free; else NO
/// CHANNEL. Its first byte to send is the buffer's number. `direct_point`:
/// file X's pointer at offset A and the byte there held, EOI with it where
/// it is the last. `direct_taken`: file X's byte taken, its pointer on.
/// `direct_write`: A put at file X's pointer, the pointer on.
fn direct(a: &mut Asm) {
    a.here("open_direct");
    let (any, opened, refused) = (a.local(), a.local(), a.local());
    a.ldx(imm(1));
    a.cpx(z(zp::COMMAND_LENGTH));
    a.bcs(any);
    a.jsr("skip_spaces");
    a.jsr("decimal_at");
    a.bcs(refused);
    a.cmp(imm(CHANNEL_BUFFERS));
    a.bcs(refused);
    a.tax();
    a.lda(abs(page::FILE_MODE).x());
    a.beq(opened);
    a.bind(refused);
    a.lda(imm(number::NO_CHANNEL));
    a.jmp("error_at_zero");
    a.bind(any);
    a.jsr("hold_buffer");
    a.bcs(RTS);
    a.bind(opened);
    a.lda(imm(mode::DIRECT));
    a.sta(abs(page::FILE_MODE).x());
    a.txa();
    a.ldy(z(zp::CHANNEL));
    a.sta(abs(page::CHANNEL_FILE).y());
    a.sta(abs(page::FILE_BYTE).x());
    a.lda(imm(LAST_OFFSET));
    a.sta(abs(page::FILE_LAST).x());
    a.lda(imm(DIRECT_FIRST));
    a.sta(abs(page::FILE_AT).x());
    a.lda(imm(0));
    a.sta(abs(page::FILE_EOI).x());
    a.clc();
    a.rts();

    a.here("direct_point");
    let (last, held) = (a.local(), a.local());
    a.sta(abs(page::FILE_AT).x());
    a.tay();
    a.jsr("point_at_file");
    a.lda(z(zp::FILE_POINTER).ind_y());
    a.sta(abs(page::FILE_BYTE).x());
    a.tya();
    a.cmp(abs(page::FILE_LAST).x());
    a.beq(last);
    a.lda(imm(0));
    a.beq(held);
    a.bind(last);
    a.lda(imm(EOI));
    a.bind(held);
    a.sta(abs(page::FILE_EOI).x());
    a.clc();
    a.rts();

    a.here("direct_taken");
    let on = a.local();
    a.lda(abs(page::FILE_AT).x());
    a.cmp(abs(page::FILE_LAST).x());
    a.bne(on);
    a.lda(imm(WRAPPED - 1));
    a.bind(on);
    a.clc();
    a.adc(imm(1));
    a.jmp("direct_point");

    a.here("direct_write");
    a.pha();
    a.jsr("point_at_file");
    a.pla();
    a.ldy(abs(page::FILE_AT).x());
    a.sta(z(zp::FILE_POINTER).ind_y());
    a.inc(abs(page::FILE_AT).x());
    a.clc();
    a.rts();
}

/// `block_numbers` and `user_numbers`: a block command's numbers, from
/// after its colon where it has one, else from after its letters for a
/// `B-` command and its first separator for a `U` one; up to four in
/// `NUMBERS` as `numbers_from` reads them.
fn numbers(a: &mut Asm) {
    a.here("block_numbers");
    let plain = a.local();
    a.ldx(imm(0));
    a.jsr("find_colon");
    a.bcs(plain);
    a.inx();
    a.jmp("numbers_from");
    a.bind(plain);
    a.ldx(imm(BLOCK_NUMBERS_AT));
    a.jmp("numbers_from");

    a.here("user_numbers");
    let (plain, scan, separated) = (a.local(), a.local(), a.local());
    a.ldx(imm(0));
    a.jsr("find_colon");
    a.bcs(plain);
    a.inx();
    a.jmp("numbers_from");
    a.bind(plain);
    a.ldx(imm(0));
    a.bind(scan);
    a.inx();
    a.cpx(z(zp::COMMAND_LENGTH));
    a.bcs(separated);
    a.lda(abs(page::COMMAND).x());
    a.jsr("is_separator");
    a.bne(scan);
    a.bind(separated);
    a.fall_into("numbers_from");
    parsing(a);
}

/// `numbers_from`: the numbers from offset X, a missing one 0, a byte's
/// worth of each kept; anything but digits and separators among them is
/// SYNTAX ERROR, C set. `decimal_at`: the digits from offset X as a byte
/// in A, X past them, C set where they make more than a byte holds.
/// `skip_spaces`: X past any spaces.
fn parsing(a: &mut Asm) {
    a.here("numbers_from");
    let (clear, next, number, kept, done) = (a.local(), a.local(), a.local(), a.local(), a.local());
    a.lda(imm(0));
    a.ldy(imm(BLOCK_NUMBERS - 1));
    a.bind(clear);
    a.sta(abs(zp::NUMBERS).y());
    a.dey();
    a.bpl(clear);
    a.iny();
    a.bind(next);
    a.cpx(z(zp::COMMAND_LENGTH));
    a.bcs(done);
    a.lda(abs(page::COMMAND).x());
    a.jsr("is_separator");
    a.bne(number);
    a.inx();
    a.bne(next);
    a.bind(number);
    a.jsr("decimal_at");
    a.cpy(imm(BLOCK_NUMBERS));
    a.bcs(kept);
    a.sta(abs(zp::NUMBERS).y());
    a.bind(kept);
    a.iny();
    a.cpx(z(zp::COMMAND_LENGTH));
    a.bcs(done);
    a.lda(abs(page::COMMAND).x());
    // A number ends at a separator or the command's end, anything else
    // among them included.
    a.jsr("is_separator");
    a.beq(next);
    a.lda(imm(number::SYNTAX));
    a.jmp("error_at_zero");
    a.bind(done);
    a.clc();
    a.rts();

    a.here("is_separator");
    a.cmp(imm(SEPARATORS[0]));
    a.beq(RTS);
    a.cmp(imm(SEPARATORS[1]));
    a.rts();

    a.here("decimal_at");
    let (digit, over, ended) = (a.local(), a.local(), a.local());
    a.lda(imm(0));
    a.sta(z(zp::VALUE));
    a.sta(z(zp::OVERFLOW));
    a.bind(digit);
    a.cpx(z(zp::COMMAND_LENGTH));
    a.bcs(ended);
    a.lda(abs(page::COMMAND).x());
    a.sec();
    a.sbc(imm(b'0'));
    a.cmp(imm(10));
    a.bcs(ended);
    a.inx();
    a.pha();
    a.lda(z(zp::VALUE));
    a.cmp(imm(26));
    a.bcs(over);
    a.asl(Acc);
    a.asl(Acc);
    a.adc(z(zp::VALUE));
    a.asl(Acc);
    a.sta(z(zp::VALUE));
    a.pla();
    a.adc(z(zp::VALUE));
    a.sta(z(zp::VALUE));
    a.bcc(digit);
    a.pha();
    a.bind(over);
    a.pla();
    a.sec();
    a.ror(z(zp::OVERFLOW));
    a.bne(digit);
    a.bind(ended);
    a.asl(z(zp::OVERFLOW));
    a.lda(z(zp::VALUE));
    a.rts();

    a.here("skip_spaces");
    let skip = a.local();
    a.bind(skip);
    a.cpx(z(zp::COMMAND_LENGTH));
    a.bcs(RTS);
    a.lda(abs(page::COMMAND).x());
    a.cmp(imm(b' '));
    a.bne(RTS);
    a.inx();
    a.bne(skip);
}

/// `block_command`: `B-` and the letter after it; INVALID COMMAND for
/// another. `numbered_channel`: the file of the channel `NUMBERS` names
/// in X, NO CHANNEL where it has none. `block_at`: the drive, track and
/// sector `NUMBERS` holds from offset X checked, the block in
/// `ALLOC_TRACK` and `ALLOC_SECTOR`, A and Y: drive 1 is DRIVE NOT READY,
/// a block no disk has ILLEGAL TRACK OR SECTOR, each at the block named,
/// C set.
fn commands(a: &mut Asm) {
    a.here("block_command");
    let invalid = a.local();
    a.lda(abs(page::COMMAND + 1));
    a.cmp(imm(b'-'));
    a.bne(invalid);
    a.lda(imm(lo("block_commands")));
    a.sta(z(zp::POINTER));
    a.lda(imm(hi("block_commands")));
    a.sta(z(zp::POINTER + 1));
    a.lda(abs(page::COMMAND + 2));
    a.jmp("dispatch");
    a.bind(invalid);
    a.lda(imm(number::INVALID_COMMAND));
    a.jmp("error_at_zero");

    a.here("numbered_channel");
    let none = a.local();
    a.ldy(z(zp::NUMBERS));
    a.cpy(imm(page::DATA_CHANNELS));
    a.bcs(none);
    a.ldx(abs(page::CHANNEL_FILE).y());
    a.cpx(imm(NO_FILE));
    a.beq(none);
    a.clc();
    a.rts();
    a.bind(none);
    a.lda(imm(number::NO_CHANNEL));
    a.jmp("error_at_zero");

    a.here("block_at");
    let (ready, illegal, refused) = (a.local(), a.local(), a.local());
    a.lda(abs(zp::NUMBERS + 1).x());
    a.sta(z(zp::ALLOC_TRACK));
    a.lda(abs(zp::NUMBERS + 2).x());
    a.sta(z(zp::ALLOC_SECTOR));
    a.lda(abs(zp::NUMBERS).x());
    a.beq(ready);
    a.lda(imm(number::NOT_READY));
    a.bne(refused);
    a.bind(ready);
    a.ldx(z(zp::ALLOC_TRACK));
    a.beq(illegal);
    a.cpx(imm(TRACKS + 1));
    a.bcs(illegal);
    a.lda(z(zp::ALLOC_SECTOR));
    a.cmp(abs("track_sectors").x());
    a.bcs(illegal);
    a.txa();
    a.ldy(z(zp::ALLOC_SECTOR));
    a.clc();
    a.rts();
    a.bind(illegal);
    a.lda(imm(number::ILLEGAL_BLOCK));
    a.bind(refused);
    a.ldx(z(zp::ALLOC_TRACK));
    a.ldy(z(zp::ALLOC_SECTOR));
    a.jsr("set_error");
    a.sec();
    a.rts();
}

/// `user_read` and `block_read`: `U1` and `B-R`, the block read into the
/// channel's buffer: the first sends it from offset 0 to its end, the
/// second from offset 1 to the offset its first byte gives.
/// `block_execute`: `B-E`, the block read as `U1` reads it, then its code
/// run from its start. `user_write` and `block_write`: `U2` and `B-W`, the
/// buffer written to the block, the second with the offset of the last
/// byte written put in its first byte. `block_pointer`: `B-P`, the
/// channel's pointer moved. Each makes the disk ready first, as the
/// original's do.
fn reading(a: &mut Asm) {
    a.here("user_read");
    a.jsr("user_numbers");
    a.bcs(RTS);
    a.jsr("read_numbered");
    a.bcs(RTS);
    a.here("whole_block");
    a.lda(imm(LAST_OFFSET));
    a.sta(abs(page::FILE_LAST).x());
    a.lda(imm(0));
    a.jmp("direct_point");

    a.here("block_read");
    a.jsr("block_numbers");
    a.bcs(RTS);
    a.jsr("read_numbered");
    a.bcs(RTS);
    a.jsr("point_at_file");
    a.ldy(imm(0));
    a.lda(z(zp::FILE_POINTER).ind_y());
    a.sta(abs(page::FILE_LAST).x());
    a.lda(imm(COUNTED_FROM));
    a.jmp("direct_point");

    a.here("block_execute");
    a.jsr("block_numbers");
    a.bcs(RTS);
    a.jsr("read_numbered");
    a.bcs(RTS);
    a.jsr("whole_block");
    a.lda(imm(0));
    a.sta(z(zp::MEMORY_AT));
    a.txa();
    a.clc();
    a.adc(imm(hi(BUFFERS)));
    a.sta(z(zp::MEMORY_AT + 1));
    a.jmp("call_memory");

    a.here("user_write");
    a.jsr("user_numbers");
    a.bcs(RTS);
    a.jsr("numbered_block");
    a.bcs(RTS);
    a.jmp("write_numbered");

    a.here("block_write");
    a.jsr("block_numbers");
    a.bcs(RTS);
    a.jsr("numbered_block");
    a.bcs(RTS);
    a.jsr("point_at_file");
    a.ldy(abs(page::FILE_AT).x());
    a.dey();
    a.tya();
    a.ldy(imm(0));
    a.sta(z(zp::FILE_POINTER).ind_y());
    a.here("write_numbered");
    a.lda(abs(zp::NUMBERS + 2));
    a.ldy(abs(zp::NUMBERS + 3));
    a.jmp("write_block_at");

    a.here("block_pointer");
    a.jsr("block_numbers");
    a.bcs(RTS);
    a.jsr("numbered_channel");
    a.bcs(RTS);
    a.lda(abs(zp::NUMBERS + 1));
    a.jmp("direct_point");

    // The channel's file in X and the block checked, the disk made ready.
    a.here("numbered_block");
    let refused = a.local();
    a.jsr("numbered_channel");
    a.bcs(refused);
    a.txa();
    a.pha();
    a.ldx(imm(1));
    a.jsr("block_at");
    a.pla();
    a.tax();
    a.bcs(refused);
    a.jmp("ensure_ready");
    a.bind(refused);
    a.rts();

    a.here("read_numbered");
    a.jsr("numbered_block");
    a.bcs(RTS);
    a.lda(abs(zp::NUMBERS + 2));
    a.ldy(abs(zp::NUMBERS + 3));
    a.jmp("read_block_at");
}

/// `block_allocate` and `block_free`: `B-A` and `B-F`, the block used or
/// freed in the BAM as it stands, without making the disk ready, its track
/// touched; a block already used is NO BLOCK at the next free one after
/// it up the disk, that track touched too, or at none. The BAM is then
/// written as the SAVE channel writes it, every touched track recorded
/// wholly used, its error standing over NO BLOCK.
fn allocation(a: &mut Asm) {
    a.here("block_allocate");
    let (search, check, none) = (a.local(), a.local(), a.local());
    a.jsr("allocation_block");
    a.bcs(RTS);
    a.jsr("bam_bit");
    a.beq(search);
    a.jsr("use_found");
    a.jmp("bam_written");
    a.bind(search);
    a.inc(z(zp::ALLOC_SECTOR));
    a.ldy(z(zp::ALLOC_TRACK));
    a.lda(z(zp::ALLOC_SECTOR));
    a.cmp(abs("track_sectors").y());
    a.bcc(check);
    a.lda(imm(0));
    a.sta(z(zp::ALLOC_SECTOR));
    a.inc(z(zp::ALLOC_TRACK));
    a.lda(z(zp::ALLOC_TRACK));
    a.cmp(imm(TRACKS + 1));
    a.bcs(none);
    a.bind(check);
    a.jsr("bam_bit");
    a.beq(search);
    a.jsr("touch_track");
    a.lda(imm(number::NO_BLOCK));
    a.ldx(z(zp::ALLOC_TRACK));
    a.ldy(z(zp::ALLOC_SECTOR));
    a.jsr("set_error");
    a.jmp("bam_written");
    a.bind(none);
    a.lda(imm(number::NO_BLOCK));
    a.jsr("error_at_zero");
    a.jmp("bam_written");

    a.here("block_free");
    a.jsr("allocation_block");
    a.bcs(RTS);
    a.lda(z(zp::ALLOC_TRACK));
    a.ldy(z(zp::ALLOC_SECTOR));
    a.jsr("free_block");
    a.here("bam_written");
    a.jsr("hold_buffer");
    a.bcs(RTS);
    a.jmp("write_bam_filling");

    // The block `NUMBERS` names from its drive checked, the allocation
    // search at it, its track touched.
    a.here("allocation_block");
    a.jsr("block_numbers");
    a.bcs(RTS);
    a.ldx(imm(0));
    a.jsr("block_at");
    a.bcs(RTS);
    a.jsr("touch_track");
    a.clc();
    a.rts();
}
