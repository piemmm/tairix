//! Relative files, as the original's: records of one length laid end to
//! end through data blocks their side sectors list, reached by `P` and
//! read and written a record at a time. One is open at a time, through two
//! buffers: its channel's, holding one data block, written back when
//! another is wanted, and one holding a side sector. A record past the
//! file extends it a block at a time, in the order the original takes
//! them, where the disk has room for it.

use super::errors::number;
use super::files::{mode, named_block, EOI, LAST_OFFSET};
use super::ws::{page, zp, NO_FILE};
use crate::media::c1541::layout::{entry, kind, side, DATA_BYTES, LINK_BYTES};
use crate::rom::asm::{abs, hi, imm, lo, zp as z, Acc, Asm, RTS};

/// A buffer's block where it holds none: a data block's index's high byte,
/// a side sector's number.
const NOTHING: u8 = 0xFF;
/// `REL_DIRTY`: the data block and the side sector to be written back, and
/// the file changed since it opened, which has its entry counted at the
/// close.
const DATA_DIRTY: u8 = 0x01;
const SIDE_DIRTY: u8 = 0x02;
const CHANGED: u8 = 0x80;
/// `REL_SINGLE` while an extension adds a single block.
const SINGLE: u8 = 0x80;
/// `REL_STATE`: the byte to send held, and it the RETURN past the file;
/// the cursor at the record and position. Never read through V, which
/// BYTE READY sets through SO whenever the disk turns.
const READY: u8 = 0x80;
const PAST: u8 = 0x40;
const CURSOR: u8 = 0x01;
/// An empty record's first byte.
const EMPTY: u8 = 0xFF;
/// `P`'s bytes after its letter: the channel, the record low byte first,
/// and the position, each counted from 1.
const P_CHANNEL: u16 = 1;
const P_RECORD: u16 = 2;
const P_POSITION: u16 = 4;
/// One past the data blocks six side sectors list.
const TOO_MANY_BLOCKS: u16 = side::MOST as u16 * side::LISTED as u16 + 1;

const _: () = assert!(
    READY == 0x80 && CHANGED == 0x80,
    "N reads the byte held and the change"
);

pub fn emit(a: &mut Asm) {
    arithmetic(a);
    counting(a);
    blocks(a);
    sides(a);
    cursor(a);
    reading(a);
    writing(a);
    extending(a);
    adding(a);
    opening_sides(a);
    opening(a);
    counted(a);
    creating(a);
    closing(a);
    positioning(a);
}

/// Page zero's pair `pair` put at `pointer`'s offset Y and on, Y left at
/// its second byte.
fn put_pair(a: &mut Asm, pointer: u16, pair: u16) {
    a.lda(z(pair));
    a.sta(z(pointer).ind_y());
    a.iny();
    a.lda(z(pair + 1));
    a.sta(z(pointer).ind_y());
}

/// The block named at `pointer`'s offset Y taken into page zero's pair
/// `pair`.
fn take_pair(a: &mut Asm, pointer: u16, pair: u16) {
    a.lda(z(pointer).ind_y());
    a.sta(z(pair));
    a.iny();
    a.lda(z(pointer).ind_y());
    a.sta(z(pair + 1));
}

/// `multiply`: `REL_WORK` `REL_FACTOR` times A, 24 bits, the factor spent.
/// `divide`: `REL_WORK` divided by A, the remainder in A. `rel_locate`: the
/// cursor at the record and position. Each keeps X.
fn arithmetic(a: &mut Asm) {
    a.here("multiply");
    let (bit, unset) = (a.local(), a.local());
    a.sta(z(zp::REL_OPERAND));
    a.lda(imm(0));
    for byte in 0..3 {
        a.sta(z(zp::REL_WORK + byte));
    }
    a.ldy(imm(8));
    a.bind(bit);
    a.lsr(z(zp::REL_OPERAND));
    a.bcc(unset);
    a.clc();
    for byte in 0..3 {
        a.lda(z(zp::REL_WORK + byte));
        a.adc(z(zp::REL_FACTOR + byte));
        a.sta(z(zp::REL_WORK + byte));
    }
    a.bind(unset);
    a.asl(z(zp::REL_FACTOR));
    a.rol(z(zp::REL_FACTOR + 1));
    a.rol(z(zp::REL_FACTOR + 2));
    a.dey();
    a.bne(bit);
    a.rts();

    a.here("divide");
    let (step, subtract, kept) = (a.local(), a.local(), a.local());
    a.sta(z(zp::REL_OPERAND));
    a.lda(imm(0));
    a.sta(z(zp::REL_REMAINDER));
    a.ldy(imm(24));
    a.bind(step);
    a.asl(z(zp::REL_WORK));
    a.rol(z(zp::REL_WORK + 1));
    a.rol(z(zp::REL_WORK + 2));
    a.rol(z(zp::REL_REMAINDER));
    a.lda(z(zp::REL_REMAINDER));
    // A bit carried out of the remainder puts it past any divisor.
    a.bcs(subtract);
    a.cmp(z(zp::REL_OPERAND));
    a.bcc(kept);
    a.bind(subtract);
    a.sbc(z(zp::REL_OPERAND));
    a.sta(z(zp::REL_REMAINDER));
    a.inc(z(zp::REL_WORK));
    a.bind(kept);
    a.dey();
    a.bne(step);
    a.lda(z(zp::REL_REMAINDER));
    a.rts();

    a.here("rel_locate");
    let carried = a.local();
    a.lda(abs(page::REL_RECORD));
    a.sta(z(zp::REL_FACTOR));
    a.lda(abs(page::REL_RECORD + 1));
    a.sta(z(zp::REL_FACTOR + 1));
    a.lda(imm(0));
    a.sta(z(zp::REL_FACTOR + 2));
    a.lda(abs(page::REL_LENGTH));
    a.jsr("multiply");
    a.lda(z(zp::REL_WORK));
    a.clc();
    a.adc(abs(page::REL_POSITION));
    a.sta(z(zp::REL_WORK));
    a.bcc(carried);
    a.inc(z(zp::REL_WORK + 1));
    a.bne(carried);
    a.inc(z(zp::REL_WORK + 2));
    a.bind(carried);
    a.lda(imm(DATA_BYTES));
    a.jsr("divide");
    a.sta(abs(page::REL_OFFSET));
    a.lda(z(zp::REL_WORK));
    a.sta(abs(page::REL_INDEX));
    a.lda(z(zp::REL_WORK + 1));
    a.sta(abs(page::REL_INDEX + 1));
    a.lda(abs(page::REL_STATE));
    a.ora(imm(CURSOR));
    a.sta(abs(page::REL_STATE));
    a.rts();
}

/// `count_records`: the records the file's blocks hold whole, as many as a
/// record number names at most. `last_sector`: a file's last block's link
/// sector, the offset of the last byte of the last record its blocks hold
/// whole, in A. `rel_present`: C set where the record is past the file.
/// `data_bytes`: `REL_WORK` the bytes the file's blocks hold. Each keeps X.
fn counting(a: &mut Asm) {
    a.here("count_records");
    let fits = a.local();
    a.jsr("data_bytes");
    a.lda(abs(page::REL_LENGTH));
    a.jsr("divide");
    a.lda(z(zp::REL_WORK + 2));
    a.beq(fits);
    a.lda(imm(0xFF));
    a.sta(z(zp::REL_WORK));
    a.sta(z(zp::REL_WORK + 1));
    a.bind(fits);
    a.lda(z(zp::REL_WORK));
    a.sta(abs(page::REL_RECORDS));
    a.lda(z(zp::REL_WORK + 1));
    a.sta(abs(page::REL_RECORDS + 1));
    a.rts();

    // The block's last offset less the bytes past the last whole record.
    a.here("last_sector");
    a.jsr("data_bytes");
    a.lda(abs(page::REL_LENGTH));
    a.jsr("divide");
    a.eor(imm(LAST_OFFSET));
    a.rts();

    a.here("rel_present");
    a.lda(abs(page::REL_RECORD + 1));
    a.cmp(abs(page::REL_RECORDS + 1));
    a.bne(RTS);
    a.lda(abs(page::REL_RECORD));
    a.cmp(abs(page::REL_RECORDS));
    a.rts();

    a.here("data_bytes");
    a.lda(abs(page::REL_BLOCKS));
    a.sta(z(zp::REL_FACTOR));
    a.lda(abs(page::REL_BLOCKS + 1));
    a.sta(z(zp::REL_FACTOR + 1));
    a.lda(imm(0));
    a.sta(z(zp::REL_FACTOR + 2));
    a.lda(imm(DATA_BYTES));
    a.jmp("multiply");
}

/// The data block's buffer. `rel_block`: data block `REL_WANT` held, read
/// where it is not, the one there written back first where dirty;
/// `FILE_POINTER` at it, X its buffer. `rel_claim`: the buffer cleared to
/// make block `REL_WANT` at track A, sector Y, the one there written back
/// first. `rel_write_back`: the block there written back where dirty. Each
/// sets C with the error where a block cannot be read or written.
fn blocks(a: &mut Asm) {
    a.here("rel_block");
    let (miss, held, lost) = (a.local(), a.local(), a.local());
    a.lda(z(zp::REL_WANT));
    a.cmp(abs(page::REL_HELD));
    a.bne(miss);
    a.lda(z(zp::REL_WANT + 1));
    a.cmp(abs(page::REL_HELD + 1));
    a.beq(held);
    a.bind(miss);
    a.jsr("rel_write_back");
    a.bcs(RTS);
    a.jsr("rel_pointer");
    a.bcs(RTS);
    a.ldx(abs(page::REL_FILE));
    a.sta(abs(page::FILE_TRACK).x());
    a.tya();
    a.sta(abs(page::FILE_SECTOR).x());
    a.lda(abs(page::FILE_TRACK).x());
    a.jsr("read_block_at");
    a.bcs(lost);
    a.bind(held);
    a.lda(z(zp::REL_WANT));
    a.sta(abs(page::REL_HELD));
    a.lda(z(zp::REL_WANT + 1));
    a.sta(abs(page::REL_HELD + 1));
    a.ldx(abs(page::REL_FILE));
    a.jsr("point_at_file");
    a.clc();
    a.rts();
    a.bind(lost);
    a.lda(imm(NOTHING));
    a.sta(abs(page::REL_HELD + 1));
    a.rts();

    a.here("rel_claim");
    let clear = a.local();
    a.pha();
    a.tya();
    a.pha();
    a.jsr("rel_write_back");
    a.pla();
    a.tay();
    a.pla();
    a.bcs(RTS);
    a.ldx(abs(page::REL_FILE));
    a.sta(abs(page::FILE_TRACK).x());
    a.tya();
    a.sta(abs(page::FILE_SECTOR).x());
    a.jsr("point_at_file");
    a.lda(imm(0));
    a.tay();
    a.bind(clear);
    a.sta(z(zp::FILE_POINTER).ind_y());
    a.iny();
    a.bne(clear);
    a.lda(abs(page::REL_DIRTY));
    a.ora(imm(DATA_DIRTY));
    a.sta(abs(page::REL_DIRTY));
    a.jmp(held);

    a.here("rel_write_back");
    let kept = a.local();
    a.lda(abs(page::REL_DIRTY));
    a.and(imm(DATA_DIRTY));
    a.beq(kept);
    a.ldx(abs(page::REL_FILE));
    a.ldy(abs(page::FILE_SECTOR).x());
    a.lda(abs(page::FILE_TRACK).x());
    a.jsr("write_block_at");
    a.bcs(RTS);
    a.lda(abs(page::REL_DIRTY));
    a.and(imm(!DATA_DIRTY));
    a.sta(abs(page::REL_DIRTY));
    a.bind(kept);
    a.clc();
    a.rts();
}

/// `rel_pointer`: the track in A and sector in Y of data block `REL_WANT`,
/// from its side sector. `side_entry`: `REL_WANT`'s side sector in A and its
/// place in that one's list in `REL_COUNT`. `rel_side`: side sector A
/// held, `FILE_POINTER` at it, X its buffer: read through the table of the
/// one there, written back first where dirty, or through the first where
/// none is. `side_write_back`: the side sector there written back where
/// dirty. `side_at`: Y at side sector A's block in the table, `FILE_POINTER`
/// at the side sector's buffer, X that buffer. `side_dirty`: the side
/// sector to be written back, C clear. Each sets C with the error where a
/// block cannot be read or written.
fn sides(a: &mut Asm) {
    a.here("rel_pointer");
    a.jsr("side_entry");
    a.jsr("rel_side");
    a.bcs(RTS);
    a.lda(z(zp::REL_COUNT));
    a.asl(Acc);
    // A place in a list is below 128, so the shift clears C.
    a.adc(imm(side::POINTERS));
    a.tay();
    named_block(a, zp::FILE_POINTER);
    a.clc();
    a.rts();

    a.here("side_entry");
    let (each, subtract, borrowed, done) = (a.local(), a.local(), a.local(), a.local());
    a.lda(z(zp::REL_WANT));
    a.sta(z(zp::REL_COUNT));
    a.ldy(z(zp::REL_WANT + 1));
    a.ldx(imm(0));
    a.bind(each);
    a.cpy(imm(0));
    a.bne(subtract);
    a.lda(z(zp::REL_COUNT));
    a.cmp(imm(side::LISTED));
    a.bcc(done);
    a.bind(subtract);
    a.lda(z(zp::REL_COUNT));
    a.sec();
    a.sbc(imm(side::LISTED));
    a.sta(z(zp::REL_COUNT));
    a.bcs(borrowed);
    a.dey();
    a.bind(borrowed);
    a.inx();
    a.bne(each);
    a.bind(done);
    a.txa();
    a.rts();

    a.here("rel_side");
    let (pointed, through_table, pulled, lost) = (a.local(), a.local(), a.local(), a.local());
    a.cmp(abs(page::REL_SIDE_HELD));
    a.beq(pointed);
    a.pha();
    a.jsr("side_write_back");
    a.bcs(pulled);
    a.lda(abs(page::REL_SIDE_HELD));
    a.cmp(imm(NOTHING));
    a.bne(through_table);
    a.ldx(abs(page::REL_SIDE));
    a.lda(z(zp::REL_FIRST_SIDE));
    a.ldy(z(zp::REL_FIRST_SIDE + 1));
    a.jsr("read_block_at");
    a.bcs(pulled);
    a.lda(imm(0));
    a.sta(abs(page::REL_SIDE_HELD));
    a.pla();
    a.jmp("rel_side");
    a.bind(through_table);
    a.pla();
    a.pha();
    a.jsr("side_at");
    named_block(a, zp::FILE_POINTER);
    a.jsr("read_block_at");
    a.pla();
    a.bcs(lost);
    a.sta(abs(page::REL_SIDE_HELD));
    a.bind(pointed);
    a.ldx(abs(page::REL_SIDE));
    a.jsr("point_at_file");
    a.clc();
    a.rts();
    a.bind(lost);
    a.lda(imm(NOTHING));
    a.sta(abs(page::REL_SIDE_HELD));
    a.rts();
    a.bind(pulled);
    a.pla();
    a.rts();

    side_writes(a);
}

/// `sides` run on: `side_write_back`, `side_at` and `side_dirty`.
fn side_writes(a: &mut Asm) {
    a.here("side_write_back");
    let clean = a.local();
    a.lda(abs(page::REL_DIRTY));
    a.and(imm(SIDE_DIRTY));
    a.beq(clean);
    a.lda(abs(page::REL_SIDE_HELD));
    a.jsr("side_at");
    named_block(a, zp::FILE_POINTER);
    a.jsr("write_block_at");
    a.bcs(RTS);
    a.lda(abs(page::REL_DIRTY));
    a.and(imm(!SIDE_DIRTY));
    a.sta(abs(page::REL_DIRTY));
    a.bind(clean);
    a.clc();
    a.rts();

    a.here("side_at");
    a.asl(Acc);
    // A side sector's number is below 128, so the shift clears C.
    a.adc(imm(side::TABLE));
    a.tay();
    a.ldx(abs(page::REL_SIDE));
    a.jmp("point_at_file");

    a.here("side_dirty");
    a.lda(abs(page::REL_DIRTY));
    a.ora(imm(SIDE_DIRTY));
    a.sta(abs(page::REL_DIRTY));
    a.clc();
    a.rts();
}

/// The cursor. `rel_get`: the byte at it in A; `rel_put`: A put there, its
/// block dirty; each sets C where the block cannot be read. `rel_step`: the
/// cursor a byte on. `cursor_block`: the cursor's block held, Y at its byte.
fn cursor(a: &mut Asm) {
    a.here("rel_get");
    a.jsr("cursor_block");
    a.bcs(RTS);
    a.lda(z(zp::FILE_POINTER).ind_y());
    a.clc();
    a.rts();

    a.here("rel_put");
    a.pha();
    a.jsr("cursor_block");
    a.pla();
    a.bcs(RTS);
    a.sta(z(zp::FILE_POINTER).ind_y());
    a.lda(abs(page::REL_DIRTY));
    a.ora(imm(DATA_DIRTY | CHANGED));
    a.sta(abs(page::REL_DIRTY));
    a.clc();
    a.rts();

    a.here("cursor_block");
    a.lda(abs(page::REL_INDEX));
    a.sta(z(zp::REL_WANT));
    a.lda(abs(page::REL_INDEX + 1));
    a.sta(z(zp::REL_WANT + 1));
    a.jsr("rel_block");
    a.bcs(RTS);
    a.lda(abs(page::REL_OFFSET));
    a.clc();
    a.adc(imm(LINK_BYTES));
    a.tay();
    a.clc();
    a.rts();

    a.here("rel_step");
    a.inc(abs(page::REL_OFFSET));
    a.lda(abs(page::REL_OFFSET));
    a.cmp(imm(DATA_BYTES));
    a.bne(RTS);
    a.lda(imm(0));
    a.sta(abs(page::REL_OFFSET));
    a.inc(abs(page::REL_INDEX));
    a.bne(RTS);
    a.inc(abs(page::REL_INDEX + 1));
    a.rts();
}

/// Talking. `rel_ready`: the byte to send held unless it is: a record reads
/// from the position to its last byte not zero, EOI with it, an empty one
/// from its start its first byte, a position past that to its end; a
/// record used up reads the next one; past the file a RETURN with EOI,
/// RECORD NOT PRESENT. C set where a block cannot be read. `rel_taken`: the
/// next byte held, or after a record's last the next record ready to read.
/// `next_record`: the record after this one, from its start; the last a
/// record number names stays the last.
fn reading(a: &mut Asm) {
    a.here("rel_ready");
    let (prepare, unused, located, ended, scan, zero, failed) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.bit(abs(page::REL_STATE));
    a.bpl(prepare);
    a.clc();
    a.rts();
    a.bind(prepare);
    a.lda(abs(page::REL_POSITION));
    a.cmp(abs(page::REL_LENGTH));
    a.bcc(unused);
    a.jsr("next_record");
    a.bind(unused);
    a.jsr("rel_present");
    a.bcs("rel_past");
    a.lda(abs(page::REL_STATE));
    a.and(imm(CURSOR));
    a.bne(located);
    a.jsr("rel_locate");
    // With no byte not zero, a read from the record's start ends on it, one
    // from further on at the record's end.
    a.bind(located);
    a.lda(imm(0));
    a.ldx(abs(page::REL_POSITION));
    a.beq(ended);
    a.ldx(abs(page::REL_LENGTH));
    a.dex();
    a.txa();
    a.bind(ended);
    a.sta(abs(page::REL_END));
    // The position counts the scan, kept on the stack: loading a block
    // spends the other counts.
    a.lda(abs(page::REL_POSITION));
    a.pha();
    a.bind(scan);
    a.jsr("rel_get");
    a.bcs(failed);
    a.beq(zero);
    a.lda(abs(page::REL_POSITION));
    a.sta(abs(page::REL_END));
    a.bind(zero);
    a.jsr("rel_step");
    a.inc(abs(page::REL_POSITION));
    a.lda(abs(page::REL_POSITION));
    a.cmp(abs(page::REL_LENGTH));
    a.bcc(scan);
    a.pla();
    a.sta(abs(page::REL_POSITION));
    a.jsr("rel_locate");
    a.jsr("rel_hold");
    a.bcs(RTS);
    a.lda(abs(page::REL_STATE));
    a.ora(imm(READY));
    a.sta(abs(page::REL_STATE));
    a.clc();
    a.rts();
    // The scan stepped the cursor on: it is found again next time.
    a.bind(failed);
    a.pla();
    a.sta(abs(page::REL_POSITION));
    a.lda(imm(0));
    a.sta(abs(page::REL_STATE));
    a.rts();

    a.here("rel_past");
    a.ldx(abs(page::REL_FILE));
    a.lda(imm(b'\r'));
    a.sta(abs(page::FILE_BYTE).x());
    a.lda(imm(EOI));
    a.sta(abs(page::FILE_EOI).x());
    a.lda(abs(page::REL_STATE));
    a.ora(imm(READY | PAST));
    a.sta(abs(page::REL_STATE));
    a.lda(imm(number::RECORD_NOT_PRESENT));
    a.jsr("error_at_zero");
    a.clc();
    a.rts();
    taking(a);
}

/// `rel_ready` run on: `rel_hold`: the byte at the cursor held, EOI with
/// it at the record's last to send; `rel_taken` and `next_record`.
fn taking(a: &mut Asm) {
    a.here("rel_hold");
    let (last, held) = (a.local(), a.local());
    a.jsr("rel_get");
    a.bcs(RTS);
    a.ldx(abs(page::REL_FILE));
    a.sta(abs(page::FILE_BYTE).x());
    a.lda(abs(page::REL_POSITION));
    a.cmp(abs(page::REL_END));
    a.beq(last);
    a.lda(imm(0));
    a.beq(held);
    a.bind(last);
    a.lda(imm(EOI));
    a.bind(held);
    a.sta(abs(page::FILE_EOI).x());
    a.clc();
    a.rts();

    a.here("rel_taken");
    let record_end = a.local();
    a.lda(abs(page::REL_STATE));
    a.bpl(RTS);
    a.and(imm(PAST));
    a.bne(RTS);
    a.ldx(abs(page::REL_FILE));
    a.lda(abs(page::FILE_EOI).x());
    a.bne(record_end);
    a.inc(abs(page::REL_POSITION));
    a.jsr("rel_step");
    a.jsr("rel_hold");
    a.bcc(RTS);
    // Nothing is held: the next byte asked for is read again.
    a.lda(imm(0));
    a.sta(abs(page::REL_STATE));
    a.rts();
    a.bind(record_end);
    a.fall_into("next_record");

    a.here("next_record");
    a.lda(imm(0));
    a.sta(abs(page::REL_POSITION));
    a.sta(abs(page::REL_STATE));
    a.inc(abs(page::REL_RECORD));
    a.bne(RTS);
    a.inc(abs(page::REL_RECORD + 1));
    a.bne(RTS);
    a.dec(abs(page::REL_RECORD));
    a.dec(abs(page::REL_RECORD + 1));
    a.rts();
}

/// Listening. `rel_write`: A written at the cursor, the file extended first
/// where the record is past it; at the record's end OVERFLOW IN RECORD and
/// the byte dropped, as is one the file cannot be extended for. A byte with
/// EOI ends the record, the rest of it zeroed, the next record next.
fn writing(a: &mut Asm) {
    a.here("rel_write");
    let (present, located, fill, overflow, dropped, ended, next) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.pha();
    a.lda(abs(page::REL_STATE));
    a.and(imm(CURSOR));
    a.sta(abs(page::REL_STATE));
    a.lda(abs(page::REL_POSITION));
    a.cmp(abs(page::REL_LENGTH));
    a.bcs(overflow);
    a.jsr("rel_present");
    a.bcc(present);
    a.jsr("rel_extend");
    a.bcs(dropped);
    a.bind(present);
    a.lda(abs(page::REL_STATE));
    a.and(imm(CURSOR));
    a.bne(located);
    a.jsr("rel_locate");
    a.bind(located);
    a.pla();
    a.jsr("rel_put");
    a.bcs(ended);
    a.inc(abs(page::REL_POSITION));
    a.jsr("rel_step");
    a.bit(z(zp::EOI));
    a.bpl(RTS);
    a.bind(fill);
    a.lda(abs(page::REL_POSITION));
    a.cmp(abs(page::REL_LENGTH));
    a.bcs(next);
    a.lda(imm(0));
    a.jsr("rel_put");
    a.bcs(next);
    a.inc(abs(page::REL_POSITION));
    a.jsr("rel_step");
    a.jmp(fill);
    a.bind(overflow);
    a.lda(imm(number::RECORD_OVERFLOW));
    a.jsr("error_at_zero");
    a.bind(dropped);
    a.pla();
    a.bind(ended);
    a.bit(z(zp::EOI));
    a.bpl(RTS);
    a.bind(next);
    a.jmp("next_record");
}

/// `rel_extend`: the file extended to hold the record: FILE TOO LARGE, C
/// set and nothing taken, where it needs more blocks than six side sectors
/// list or than the disk has free, as the original's manual has it. Each
/// block is added as `rel_add` adds it; the last, linked to no spare, is
/// then linked as a file's last, and it, the side sector and the BAM
/// written. One that fails part-way gives the file up, the blocks it took
/// kept used. `too_large`: FILE TOO LARGE, C set.
fn extending(a: &mut Asm) {
    a.here("rel_extend");
    let (rounded, more, linked) = (a.local(), a.local(), a.local());
    a.lda(abs(page::REL_RECORD));
    a.clc();
    a.adc(imm(1));
    a.sta(z(zp::REL_FACTOR));
    a.lda(abs(page::REL_RECORD + 1));
    a.adc(imm(0));
    a.sta(z(zp::REL_FACTOR + 1));
    a.bcs("too_large");
    a.lda(imm(0));
    a.sta(z(zp::REL_FACTOR + 2));
    a.lda(abs(page::REL_LENGTH));
    a.jsr("multiply");
    a.lda(z(zp::REL_WORK));
    a.clc();
    a.adc(imm(DATA_BYTES - 1));
    a.sta(z(zp::REL_WORK));
    a.bcc(rounded);
    a.inc(z(zp::REL_WORK + 1));
    a.bne(rounded);
    a.inc(z(zp::REL_WORK + 2));
    a.bind(rounded);
    a.lda(imm(DATA_BYTES));
    a.jsr("divide");
    a.lda(z(zp::REL_WORK + 2));
    a.bne("too_large");
    a.lda(z(zp::REL_WORK));
    a.sta(z(zp::REL_NEEDED));
    a.cmp(imm(lo(TOO_MANY_BLOCKS)));
    a.lda(z(zp::REL_WORK + 1));
    a.sta(z(zp::REL_NEEDED + 1));
    a.sbc(imm(hi(TOO_MANY_BLOCKS)));
    a.bcs("too_large");
    a.jsr("rel_room");
    a.bcs(RTS);
    a.lda(abs(page::REL_DIRTY));
    a.ora(imm(CHANGED));
    a.sta(abs(page::REL_DIRTY));
    a.bind(more);
    a.jsr("rel_add");
    a.bcs("rel_given_up");
    a.lda(abs(page::REL_BLOCKS));
    a.cmp(z(zp::REL_NEEDED));
    a.lda(abs(page::REL_BLOCKS + 1));
    a.sbc(z(zp::REL_NEEDED + 1));
    a.bcc(more);
    a.ldy(imm(0));
    a.lda(z(zp::FILE_POINTER).ind_y());
    a.bne(linked);
    a.jsr("last_sector");
    a.ldy(imm(1));
    a.sta(z(zp::FILE_POINTER).ind_y());
    a.bind(linked);
    a.jsr("rel_write_back");
    a.bcs("rel_given_up");
    a.jsr("side_write_back");
    a.bcs("rel_given_up");
    a.jsr("write_bam");
    a.bcs("rel_given_up");
    a.jsr("count_records");
    a.lda(imm(0));
    a.sta(abs(page::REL_STATE));
    a.clc();
    a.rts();

    a.here("too_large");
    a.lda(imm(number::FILE_TOO_LARGE));
    a.jmp("error_at_zero");

    a.here("rel_given_up");
    a.jsr("write_bam");
    a.ldx(abs(page::REL_FILE));
    a.lda(imm(mode::ENDED));
    a.sta(abs(page::FILE_MODE).x());
    a.jsr("rel_release");
    a.sec();
    a.rts();
    room(a);
}

/// `rel_room`, from `rel_extend` with `REL_NEEDED` found: the file's last
/// block held and the spare it links to, if any, in `REL_SPARE`;
/// `REL_SINGLE` set where the extension adds a single block; FILE TOO
/// LARGE where the disk has fewer blocks free than the extension takes:
/// the data blocks, a side sector for each list begun, and the spare a last
/// block opening a list leaves where the extension adds more than it, less
/// the spare there now.
fn room(a: &mut Asm) {
    a.here("rel_room");
    let (several, dangles, ends_list, begun, no_spare, low, roomy) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.lda(abs(page::REL_BLOCKS));
    a.sec();
    a.sbc(imm(1));
    a.sta(z(zp::REL_WANT));
    a.lda(abs(page::REL_BLOCKS + 1));
    a.sbc(imm(0));
    a.sta(z(zp::REL_WANT + 1));
    a.jsr("rel_block");
    a.bcs(RTS);
    a.ldy(imm(0));
    take_pair(a, zp::FILE_POINTER, zp::REL_SPARE);
    a.ldx(imm(0));
    a.lda(z(zp::REL_NEEDED));
    a.sec();
    a.sbc(abs(page::REL_BLOCKS));
    a.sta(z(zp::REL_WORK));
    a.lda(z(zp::REL_NEEDED + 1));
    a.sbc(abs(page::REL_BLOCKS + 1));
    a.sta(z(zp::REL_WORK + 1));
    a.bne(several);
    a.lda(z(zp::REL_WORK));
    a.cmp(imm(1));
    a.bne(several);
    a.ldx(imm(SINGLE));
    a.bind(several);
    a.stx(z(zp::REL_SINGLE));
    a.lda(z(zp::REL_NEEDED));
    a.sec();
    a.sbc(imm(1));
    a.sta(z(zp::REL_WANT));
    a.lda(z(zp::REL_NEEDED + 1));
    a.sbc(imm(0));
    a.sta(z(zp::REL_WANT + 1));
    a.jsr("side_entry");
    a.pha();
    // A spare is left where the last block begins a list and the extension
    // adds more than it.
    a.lda(z(zp::REL_COUNT));
    a.bne(ends_list);
    a.lda(z(zp::REL_WORK + 1));
    a.bne(dangles);
    a.lda(z(zp::REL_WORK));
    a.cmp(imm(2));
    a.bcc(ends_list);
    a.bind(dangles);
    a.inc(z(zp::REL_WORK));
    a.bne(ends_list);
    a.inc(z(zp::REL_WORK + 1));
    a.bind(ends_list);
    // The last block's side sector, one on, less those there now.
    a.pla();
    a.clc();
    a.adc(imm(1));
    a.sec();
    a.sbc(abs(page::REL_SIDES));
    a.clc();
    a.adc(z(zp::REL_WORK));
    a.sta(z(zp::REL_WORK));
    a.bcc(begun);
    a.inc(z(zp::REL_WORK + 1));
    a.bind(begun);
    a.lda(z(zp::REL_SPARE));
    a.beq(no_spare);
    a.lda(z(zp::REL_WORK));
    a.bne(low);
    a.dec(z(zp::REL_WORK + 1));
    a.bind(low);
    a.dec(z(zp::REL_WORK));
    a.bind(no_spare);
    a.jsr("count_free");
    a.lda(z(zp::FREE));
    a.cmp(z(zp::REL_WORK));
    a.lda(z(zp::FREE + 1));
    a.sbc(z(zp::REL_WORK + 1));
    a.bcs(roomy);
    a.jmp("too_large");
    a.bind(roomy);
    a.clc();
    a.rts();
}

/// `rel_add`: data block `REL_BLOCKS` added after the last, which is held:
/// the spare that one links to where it opened a side sector, else the next
/// from it; linked from it, and listed, first in a new side sector where it
/// begins a list. It is then held cleared, its records empty, linked to the
/// spare it opened a side sector with, else as a file's last until the next
/// block is added. C set
/// where a block cannot be found, read or written. `rel_list`: `REL_NEW`
/// listed in side sector A at place `REL_COUNT`, the link's sector at it.
fn adding(a: &mut Asm) {
    a.here("rel_add");
    let (fresh, placed, opens, listed, counted) =
        (a.local(), a.local(), a.local(), a.local(), a.local());
    a.lda(z(zp::REL_SPARE));
    a.beq(fresh);
    a.sta(z(zp::REL_NEW));
    a.lda(z(zp::REL_SPARE + 1));
    a.sta(z(zp::REL_NEW + 1));
    a.lda(imm(0));
    a.sta(z(zp::REL_SPARE));
    a.beq(placed);
    a.bind(fresh);
    a.ldx(abs(page::REL_FILE));
    a.ldy(abs(page::FILE_SECTOR).x());
    a.lda(abs(page::FILE_TRACK).x());
    a.jsr("allocate_next");
    a.bcs(RTS);
    a.sta(z(zp::REL_NEW));
    a.sty(z(zp::REL_NEW + 1));
    a.bind(placed);
    a.ldx(abs(page::REL_FILE));
    a.jsr("point_at_file");
    a.ldy(imm(0));
    put_pair(a, zp::FILE_POINTER, zp::REL_NEW);
    a.lda(abs(page::REL_DIRTY));
    a.ora(imm(DATA_DIRTY));
    a.sta(abs(page::REL_DIRTY));
    a.lda(abs(page::REL_BLOCKS));
    a.sta(z(zp::REL_WANT));
    a.lda(abs(page::REL_BLOCKS + 1));
    a.sta(z(zp::REL_WANT + 1));
    a.jsr("side_entry");
    a.ldx(z(zp::REL_COUNT));
    a.beq(opens);
    a.jsr("rel_list");
    a.jmp(listed);
    a.bind(opens);
    a.jsr("rel_open_side");
    a.bind(listed);
    a.bcs(RTS);
    a.lda(z(zp::REL_NEW));
    a.ldy(z(zp::REL_NEW + 1));
    a.jsr("rel_claim");
    a.bcs(RTS);
    a.ldy(imm(0));
    put_pair(a, zp::FILE_POINTER, zp::REL_SPARE);
    a.jsr("rel_marks");
    a.inc(abs(page::REL_BLOCKS));
    a.bne(counted);
    a.inc(abs(page::REL_BLOCKS + 1));
    a.bind(counted);
    a.clc();
    a.rts();

    a.here("rel_list");
    a.jsr("rel_side");
    a.bcs(RTS);
    a.lda(z(zp::REL_COUNT));
    a.asl(Acc);
    a.adc(imm(side::POINTERS));
    a.tay();
    put_pair(a, zp::FILE_POINTER, zp::REL_NEW);
    a.tya();
    a.ldy(imm(1));
    a.sta(z(zp::FILE_POINTER).ind_y());
    a.jmp("side_dirty");
    marks(a);
}

/// `rel_marks`: each record that starts in data block `REL_WANT`, held,
/// marked empty, the one it ends with whole or not.
fn marks(a: &mut Asm) {
    a.here("rel_marks");
    let (first, each) = (a.local(), a.local());
    a.lda(z(zp::REL_WANT));
    a.sta(z(zp::REL_FACTOR));
    a.lda(z(zp::REL_WANT + 1));
    a.sta(z(zp::REL_FACTOR + 1));
    a.lda(imm(0));
    a.sta(z(zp::REL_FACTOR + 2));
    a.lda(imm(DATA_BYTES));
    a.jsr("multiply");
    a.lda(abs(page::REL_LENGTH));
    a.jsr("divide");
    // The bytes before the block of the record it starts in, or none.
    a.beq(first);
    a.sta(z(zp::REL_COUNT));
    a.lda(abs(page::REL_LENGTH));
    a.sec();
    a.sbc(z(zp::REL_COUNT));
    a.bind(first);
    a.sta(z(zp::REL_COUNT));
    a.bind(each);
    a.lda(z(zp::REL_COUNT));
    a.cmp(imm(DATA_BYTES));
    a.bcs(RTS);
    a.adc(imm(LINK_BYTES));
    a.tay();
    a.lda(imm(EMPTY));
    a.sta(z(zp::FILE_POINTER).ind_y());
    a.lda(z(zp::REL_COUNT));
    a.clc();
    a.adc(abs(page::REL_LENGTH));
    a.sta(z(zp::REL_COUNT));
    a.bcc(each);
    a.rts();
}

/// `rel_open_side`: data block `REL_NEW` begins side sector `REL_SIDES`'s
/// list: a spare taken next from it unless its extension adds it alone,
/// then the side sector next from it, as the original takes them; each side
/// sector's table naming the new one, the last so far linked to it; then
/// that one, written back, made the new one in its
/// buffer: one pointer, `REL_NEW`, its number, the length and table kept.
/// C set where a block cannot be found, read or written.
fn opening_sides(a: &mut Asm) {
    a.here("rel_open_side");
    let (side_only, each, clear) = (a.local(), a.local(), a.local());
    a.bit(z(zp::REL_SINGLE));
    a.bmi(side_only);
    for pair in [zp::REL_SPARE, zp::REL_SIDE_NEW] {
        if pair == zp::REL_SIDE_NEW {
            a.bind(side_only);
        }
        a.lda(z(zp::REL_NEW));
        a.ldy(z(zp::REL_NEW + 1));
        a.jsr("allocate_next");
        a.bcs(RTS);
        a.sta(z(pair));
        a.sty(z(pair + 1));
    }
    a.lda(imm(0));
    a.sta(z(zp::REL_COUNT));
    a.bind(each);
    a.lda(z(zp::REL_COUNT));
    a.jsr("rel_side");
    a.bcs(RTS);
    a.lda(abs(page::REL_SIDES));
    a.jsr("side_at");
    put_pair(a, zp::FILE_POINTER, zp::REL_SIDE_NEW);
    a.jsr("side_dirty");
    a.inc(z(zp::REL_COUNT));
    a.lda(z(zp::REL_COUNT));
    a.cmp(abs(page::REL_SIDES));
    a.bcc(each);
    a.ldy(imm(0));
    put_pair(a, zp::FILE_POINTER, zp::REL_SIDE_NEW);
    a.jsr("side_write_back");
    a.bcs(RTS);
    a.ldy(imm(0));
    a.tya();
    a.sta(z(zp::FILE_POINTER).ind_y());
    a.iny();
    a.lda(imm(side::POINTERS + 1));
    a.sta(z(zp::FILE_POINTER).ind_y());
    a.ldy(imm(side::NUMBER));
    a.lda(abs(page::REL_SIDES));
    a.sta(abs(page::REL_SIDE_HELD));
    a.sta(z(zp::FILE_POINTER).ind_y());
    a.ldy(imm(side::POINTERS));
    put_pair(a, zp::FILE_POINTER, zp::REL_NEW);
    a.lda(imm(0));
    a.bind(clear);
    a.iny();
    a.sta(z(zp::FILE_POINTER).ind_y());
    a.cpy(imm(LAST_OFFSET));
    a.bne(clear);
    a.inc(abs(page::REL_SIDES));
    a.jmp("side_dirty");
}

/// `open_rel`: the channel's file X opened as the relative file the name
/// names, of the length its `L` field asks: one found of that length, or of
/// any where none is asked, else made where one is. FILE NOT FOUND for no
/// name, or a new one asking no length; RECORD NOT PRESENT for another
/// length than the file's; FILE TYPE MISMATCH for a file of another kind.
/// `rel_busy`: NO CHANNEL where a relative file is open already, C set and
/// the channel left with no file. `rel_buffers`: the side sector's buffer
/// held for the relative file X; NO CHANNEL and C set where none is free.
/// `rel_release`: the side sector's buffer given back, no relative file
/// open.
fn opening(a: &mut Asm) {
    a.here("open_rel");
    let (absent, failed, found) = (a.local(), a.local(), a.local());
    a.jsr("rel_busy");
    a.bcs(RTS);
    a.lda(z(zp::NAME_END));
    a.cmp(z(zp::NAME_START));
    a.beq(absent);
    a.jsr("ensure_ready");
    a.bcs(failed);
    a.jsr("name_as_pattern");
    a.jsr("find_entry");
    a.bcc(found);
    a.lda(z(zp::ERROR));
    a.bne(failed);
    a.lda(z(zp::NAME_RECORD));
    a.beq(absent);
    a.jmp("create_rel");
    a.bind(absent);
    a.lda(imm(number::FILE_NOT_FOUND));
    a.jmp("refuse_open");
    a.bind(failed);
    a.jmp("open_failed");
    a.bind(found);
    a.jsr("found_checked");
    a.bcs(RTS);
    a.jsr("channel_file");
    a.lda(z(zp::NAME_RECORD));
    a.beq("open_found_rel");
    a.ldy(imm(entry::RECORD_LENGTH));
    a.cmp(z(zp::DIRECTORY_ENTRY).ind_y());
    a.beq("open_found_rel");
    a.lda(imm(number::RECORD_NOT_PRESENT));
    a.jmp("refuse_open");

    a.here("rel_busy");
    let free = a.local();
    a.lda(abs(page::REL_FILE));
    a.bmi(free);
    a.lda(imm(number::NO_CHANNEL));
    a.jmp("refuse_open");
    a.bind(free);
    a.clc();
    a.rts();

    a.here("rel_buffers");
    let pulled = a.local();
    a.txa();
    a.pha();
    a.jsr("hold_buffer");
    a.bcs(pulled);
    a.lda(imm(mode::SIDE));
    a.sta(abs(page::FILE_MODE).x());
    a.stx(abs(page::REL_SIDE));
    a.pla();
    a.tax();
    a.stx(abs(page::REL_FILE));
    a.lda(imm(NOTHING));
    a.sta(abs(page::REL_SIDE_HELD));
    a.sta(abs(page::REL_HELD + 1));
    a.lda(imm(0));
    a.sta(abs(page::REL_DIRTY));
    a.clc();
    a.rts();
    a.bind(pulled);
    a.pla();
    a.tax();
    a.rts();

    a.here("rel_release");
    a.ldx(abs(page::REL_SIDE));
    a.lda(imm(mode::FREE));
    a.sta(abs(page::FILE_MODE).x());
    a.lda(imm(NO_FILE));
    a.sta(abs(page::REL_FILE));
    a.rts();
}

/// `open_found_rel`: the relative file at `DIRECTORY_ENTRY` opened on file
/// X, the directory's buffer given back first for the side sector's: its
/// side sectors those the first's table names, its data blocks those they
/// list, the last's list as long as its link's sector says, at least one;
/// RECORD NOT PRESENT for a record length of 0. `rel_opened`: the
/// relative file at its first record, its records counted.
fn counted(a: &mut Asm) {
    a.here("open_found_rel");
    let (no_length, broken, refused, count, tabled) =
        (a.local(), a.local(), a.local(), a.local(), a.local());
    let (some, whole, least, listed, lists) =
        (a.local(), a.local(), a.local(), a.local(), a.local());
    a.jsr("rel_busy");
    a.bcs(RTS);
    a.jsr("keep_entry");
    a.ldy(imm(entry::RECORD_LENGTH));
    a.lda(z(zp::DIRECTORY_ENTRY).ind_y());
    a.beq(no_length);
    a.sta(abs(page::REL_LENGTH));
    a.ldy(imm(entry::SIDE));
    take_pair(a, zp::DIRECTORY_ENTRY, zp::REL_FIRST_SIDE);
    a.jsr("release_held");
    a.jsr("rel_buffers");
    a.bcs(refused);
    a.lda(imm(0));
    a.jsr("rel_side");
    a.bcs(broken);
    a.ldy(imm(side::TABLE));
    a.ldx(imm(0));
    a.bind(count);
    a.lda(z(zp::FILE_POINTER).ind_y());
    a.beq(tabled);
    a.iny();
    a.iny();
    a.inx();
    a.cpx(imm(side::MOST));
    a.bcc(count);
    a.bind(tabled);
    a.txa();
    a.bne(some);
    a.inx();
    a.bind(some);
    a.stx(abs(page::REL_SIDES));
    a.dex();
    a.txa();
    a.jsr("rel_side");
    a.bcs(broken);
    a.ldy(imm(0));
    a.lda(z(zp::FILE_POINTER).ind_y());
    a.bne(whole);
    a.iny();
    a.lda(z(zp::FILE_POINTER).ind_y());
    a.sec();
    a.sbc(imm(side::POINTERS - 1));
    a.bcc(least);
    a.lsr(Acc);
    a.bne(listed);
    a.bind(least);
    a.lda(imm(1));
    a.bne(listed);
    a.bind(whole);
    a.lda(imm(side::LISTED));
    a.bind(listed);
    a.sta(abs(page::REL_BLOCKS));
    a.lda(imm(0));
    a.sta(abs(page::REL_BLOCKS + 1));
    a.ldx(abs(page::REL_SIDES));
    a.bind(lists);
    a.dex();
    a.beq("rel_opened");
    a.lda(abs(page::REL_BLOCKS));
    a.clc();
    a.adc(imm(side::LISTED));
    a.sta(abs(page::REL_BLOCKS));
    a.bcc(lists);
    a.inc(abs(page::REL_BLOCKS + 1));
    a.bne(lists);
    a.bind(no_length);
    a.lda(imm(number::RECORD_NOT_PRESENT));
    a.jmp("refuse_open");
    a.bind(broken);
    a.jsr("rel_release");
    a.bind(refused);
    a.jmp("open_failed");

    a.here("rel_opened");
    a.jsr("count_records");
    a.lda(imm(0));
    a.sta(abs(page::REL_RECORD));
    a.sta(abs(page::REL_RECORD + 1));
    a.sta(abs(page::REL_POSITION));
    a.sta(abs(page::REL_STATE));
    a.ldx(abs(page::REL_FILE));
    a.lda(imm(mode::REL));
    a.sta(abs(page::FILE_MODE).x());
    a.clc();
    a.rts();
}

/// `create_rel`: the relative file named made on file X, of the length
/// asked: its slot found, its first data block found as any file's and its
/// side sector the next from it, then each written as `rel_make` writes
/// them. A name with a wildcard makes nothing: SYNTAX ERROR. A failure
/// leaves the disk to be read again.
fn creating(a: &mut Asm) {
    a.here("create_rel");
    let (wild, failed, broken) = (a.local(), a.local(), a.local());
    a.lda(z(zp::NAME_END));
    a.sta(z(zp::COUNT));
    a.ldx(z(zp::NAME_START));
    a.jsr("wildcard_in");
    a.bcs(wild);
    a.jsr("free_entry");
    a.bcs(failed);
    a.jsr("channel_file");
    a.jsr("keep_entry");
    a.jsr("rel_buffers");
    a.bcs(failed);
    a.lda(z(zp::NAME_RECORD));
    a.sta(abs(page::REL_LENGTH));
    a.jsr("allocate_first");
    a.bcs(broken);
    a.sta(z(zp::REL_NEW));
    a.sty(z(zp::REL_NEW + 1));
    a.jsr("allocate_next");
    a.bcs(broken);
    a.sta(z(zp::REL_FIRST_SIDE));
    a.sty(z(zp::REL_FIRST_SIDE + 1));
    a.jsr("rel_make");
    a.bcs(broken);
    a.jmp("rel_opened");
    a.bind(wild);
    a.lda(imm(number::INVALID_NAME));
    a.jmp("refuse_open");
    a.bind(broken);
    a.jsr("rel_release");
    a.jsr("forget_disk");
    a.bind(failed);
    a.jmp("open_failed");
    making(a);
}

/// `rel_make`, from `create_rel` with its blocks found: the side sector
/// written, its first pointer's offset as its link's sector, number 0, the
/// length, itself in the table and the data block listed; then the data
/// block holding its records empty; then the BAM; then the entry, closed and
/// counting no blocks yet, as the original's. C set where one cannot be.
fn making(a: &mut Asm) {
    a.here("rel_make");
    let (clear, entered) = (a.local(), a.local());
    a.ldx(abs(page::REL_SIDE));
    a.jsr("point_at_file");
    a.lda(imm(0));
    a.tay();
    a.bind(clear);
    a.sta(z(zp::FILE_POINTER).ind_y());
    a.iny();
    a.bne(clear);
    a.iny();
    a.lda(imm(side::POINTERS + 1));
    a.sta(z(zp::FILE_POINTER).ind_y());
    a.ldy(imm(side::RECORD_LENGTH));
    a.lda(abs(page::REL_LENGTH));
    a.sta(z(zp::FILE_POINTER).ind_y());
    a.iny();
    put_pair(a, zp::FILE_POINTER, zp::REL_FIRST_SIDE);
    a.ldy(imm(side::POINTERS));
    put_pair(a, zp::FILE_POINTER, zp::REL_NEW);
    a.lda(z(zp::REL_FIRST_SIDE));
    a.ldy(z(zp::REL_FIRST_SIDE + 1));
    a.jsr("write_block_at");
    a.bcs(RTS);
    a.lda(imm(0));
    a.sta(abs(page::REL_SIDE_HELD));
    a.sta(abs(page::REL_BLOCKS + 1));
    a.sta(z(zp::REL_WANT));
    a.sta(z(zp::REL_WANT + 1));
    a.lda(imm(1));
    a.sta(abs(page::REL_BLOCKS));
    a.sta(abs(page::REL_SIDES));
    a.lda(z(zp::REL_NEW));
    a.ldy(z(zp::REL_NEW + 1));
    a.jsr("rel_claim");
    a.jsr("rel_marks");
    a.jsr("last_sector");
    a.ldy(imm(1));
    a.sta(z(zp::FILE_POINTER).ind_y());
    a.jsr("rel_write_back");
    a.bcs(RTS);
    a.jsr("write_bam");
    a.bcc(entered);
    a.rts();
    a.bind(entered);
    a.ldx(abs(page::REL_FILE));
    a.lda(imm(kind::REL | kind::CLOSED));
    a.sta(abs(page::FILE_KIND).x());
    a.lda(z(zp::REL_NEW));
    a.sta(abs(page::FILE_FIRST_TRACK).x());
    a.lda(z(zp::REL_NEW + 1));
    a.sta(abs(page::FILE_FIRST_SECTOR).x());
    a.jsr("fill_new_entry");
    a.ldy(imm(entry::SIDE));
    put_pair(a, zp::DIRECTORY_ENTRY, zp::REL_FIRST_SIDE);
    a.iny();
    a.lda(abs(page::REL_LENGTH));
    a.sta(z(zp::DIRECTORY_ENTRY).ind_y());
    a.jmp("write_directory");
}

/// `close_rel`: the relative file's data block and side sector written
/// back and the side sector's buffer given back; where the file changed,
/// its entry then counts its data blocks and side sectors, as the
/// original's does only then.
fn closing(a: &mut Asm) {
    a.here("close_rel");
    a.jsr("rel_write_back");
    a.jsr("side_write_back");
    a.jsr("rel_release");
    a.bit(abs(page::REL_DIRTY));
    a.bpl(RTS);
    a.jsr("channel_file");
    a.lda(abs(page::FILE_ENTRY_TRACK).x());
    a.ldy(abs(page::FILE_ENTRY_SECTOR).x());
    a.jsr("entry_sector");
    a.bcs(RTS);
    a.jsr("channel_file");
    a.lda(abs(page::FILE_ENTRY).x());
    a.sta(z(zp::DIRECTORY_ENTRY));
    a.ldy(imm(entry::BLOCKS));
    a.lda(abs(page::REL_BLOCKS));
    a.clc();
    a.adc(abs(page::REL_SIDES));
    a.sta(z(zp::DIRECTORY_ENTRY).ind_y());
    a.iny();
    a.lda(abs(page::REL_BLOCKS + 1));
    a.adc(imm(0));
    a.sta(z(zp::DIRECTORY_ENTRY).ind_y());
    a.jmp("write_directory");
}

/// `position_command`: `P`: the relative file on the channel the command
/// names at the record and position it gives, taken from the command
/// buffer's bytes as they stand; a record or position of 0 is the first.
/// No file is NO CHANNEL, another kind FILE TYPE MISMATCH, a position past
/// the record OVERFLOW IN RECORD at its first, a record past the file
/// RECORD NOT PRESENT.
fn positioning(a: &mut Asm) {
    a.here("position_command");
    let (none, relative, numbered, first, placed, absent) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.lda(abs(page::COMMAND + P_CHANNEL));
    a.and(imm(0x0F));
    a.cmp(imm(page::DATA_CHANNELS));
    a.bcs(none);
    a.tay();
    a.ldx(abs(page::CHANNEL_FILE).y());
    a.cpx(imm(NO_FILE));
    a.beq(none);
    a.lda(abs(page::FILE_MODE).x());
    a.cmp(imm(mode::REL));
    a.beq(relative);
    a.lda(imm(number::TYPE_MISMATCH));
    a.jmp("error_at_zero");
    a.bind(none);
    a.lda(imm(number::NO_CHANNEL));
    a.jmp("error_at_zero");
    a.bind(relative);
    a.lda(abs(page::COMMAND + P_RECORD));
    a.sec();
    a.sbc(imm(1));
    a.sta(abs(page::REL_RECORD));
    a.lda(abs(page::COMMAND + P_RECORD + 1));
    a.sbc(imm(0));
    a.sta(abs(page::REL_RECORD + 1));
    a.bcs(numbered);
    a.lda(imm(0));
    a.sta(abs(page::REL_RECORD));
    a.sta(abs(page::REL_RECORD + 1));
    a.bind(numbered);
    a.lda(imm(0));
    a.sta(abs(page::REL_STATE));
    a.lda(abs(page::COMMAND + P_POSITION));
    a.beq(first);
    a.sec();
    a.sbc(imm(1));
    a.cmp(abs(page::REL_LENGTH));
    a.bcc(placed);
    a.lda(imm(number::RECORD_OVERFLOW));
    a.jsr("error_at_zero");
    a.bind(first);
    a.lda(imm(0));
    a.bind(placed);
    a.sta(abs(page::REL_POSITION));
    a.jsr("rel_present");
    a.bcs(absent);
    a.rts();
    a.bind(absent);
    a.lda(imm(number::RECORD_NOT_PRESENT));
    a.jmp("error_at_zero");
}
