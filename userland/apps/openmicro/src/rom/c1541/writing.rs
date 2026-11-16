//! Writing: a file created, or one named after `@` replaced once the new
//! one closes, or appended to. A block is linked and written once its
//! successor is found, so a disk that fills leaves the last full block
//! unwritten and the file unclosed, as the original's does. A new file's
//! entry is written unclosed when it opens, after the BAM on a data
//! channel and alone on the SAVE channel, as the original orders them; it
//! closes into its entry with its length, the BAM written after. Off the
//! SAVE channel, each track a file begins to fill is written to the BAM as
//! wholly used, and the entry a replace will take over is marked while it
//! runs, so a file never closed leaves its blocks safe until a validate.
//! The bytes past a last block's end are zero, where the original leaves
//! what its buffer held before. The original finds a file's next block as
//! soon as one fills, so a file of whole blocks leaves one allocated past
//! its end, and a disk with none left loses the file's last block; this DOS
//! finds a block only for a byte that needs one.

use super::bam::DIRECTORY_INTERLEAVE;
use super::errors::number;
use super::files::{mode, KIND_BITS};
use super::ws::{page, zp};
use crate::machine::c64::serial::channel;
use crate::media::c1541::layout::{entry, kind, DIRECTORY_TRACK, LINK_BYTES, NAME_BYTES, PADDING};
use crate::rom::asm::{abs, imm, zp as z, Asm, Label, RTS};

/// The byte an empty file holds.
const CR: u8 = b'\r';

const _: () = assert!(
    entry::TRACK == entry::TYPE + 1 && entry::SECTOR == entry::TRACK + 1,
    "an entry's type, first track and sector follow one another"
);
/// A block's first data byte's offset.
const FIRST_DATA: u8 = LINK_BYTES;
/// The character before a name that asks it be replaced.
const REPLACE: u8 = b'@';
/// A directory sector's link where it is the last.
const LAST_LINK_SECTOR: u8 = 0xFF;

pub fn emit(a: &mut Asm) {
    opening(a);
    creating(a);
    appending(a);
    writing(a);
    blocks(a);
    closing(a);
    entries(a);
    entry_writes(a);
}

/// `open_write`: the channel's file X written: of the name's type, else a
/// program on the SAVE channel and sequential elsewhere; C set where it
/// cannot be, the channel left with no file. X kept.
fn opening(a: &mut Asm) {
    a.here("open_write");
    let typed = a.local();
    a.lda(z(zp::NAME_TYPE));
    a.bne(typed);
    a.lda(imm(kind::SEQ));
    a.ldy(z(zp::CHANNEL));
    a.cpy(imm(channel::SAVE));
    a.bne(typed);
    a.lda(imm(kind::PRG));
    a.bind(typed);
    a.sta(abs(page::FILE_KIND).x());
    a.jsr("create_file");
    a.bcc(RTS);
    a.jmp("open_failed");
}

/// `create_file`: file X of kind `FILE_KIND` created with the name parsed:
/// FILE EXISTS where the name is taken but for `@`, which replaces that
/// file when this one closes, FILE TYPE MISMATCH where it is of another
/// kind, the replaced entry marked meanwhile where the channel is not the
/// SAVE channel. A name with a wildcard creates nothing: SYNTAX ERROR, or
/// FILE TYPE MISMATCH where a replace's pattern finds nothing, as the
/// original reports them. C set with the error, the disk left to be read
/// again for the BAM's sake. X kept.
fn creating(a: &mut Asm) {
    a.here("create_file");
    let (exists, wild, mismatch, refused, replacing, place, failed_pulled, failed) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.lda(imm(0));
    a.sta(abs(page::FILE_OLD_TRACK).x());
    a.sta(abs(page::FILE_BLOCKS).x());
    a.sta(abs(page::FILE_BLOCKS_HIGH).x());
    a.sta(abs(page::FILE_FILL).x());
    a.jsr("ensure_ready");
    a.bcs(failed);
    a.txa();
    a.pha();
    a.jsr("name_as_pattern");
    a.jsr("find_entry");
    a.bcc(exists);
    a.lda(z(zp::ERROR));
    a.bne(failed_pulled);
    a.lda(z(zp::NAME_END));
    a.sta(z(zp::COUNT));
    a.ldx(z(zp::NAME_START));
    a.jsr("wildcard_in");
    a.bcs(wild);
    a.jsr("free_entry");
    a.bcs(failed_pulled);
    a.pla();
    a.tax();
    a.pha();
    a.jsr("keep_entry");
    a.jmp(place);
    // A new file named by a pattern is refused; one a replace's pattern
    // found nothing for is a type mismatch, as the original's is.
    a.bind(wild);
    a.jsr("first_character");
    a.cmp(imm(REPLACE));
    a.beq(mismatch);
    a.lda(imm(number::INVALID_NAME));
    a.jmp(refused);
    a.bind(mismatch);
    a.lda(imm(number::TYPE_MISMATCH));
    a.bind(refused);
    a.jsr("error_at_zero");
    a.bind(failed_pulled);
    a.pla();
    a.tax();
    a.bind(failed);
    a.lda(imm(0));
    a.sta(z(zp::DISK_READY));
    a.sec();
    a.rts();
    a.bind(exists);
    a.jsr("first_character");
    a.cmp(imm(REPLACE));
    a.beq(replacing);
    a.lda(imm(number::FILE_EXISTS));
    a.jmp(refused);
    a.bind(replacing);
    a.pla();
    a.tax();
    a.pha();
    a.ldy(imm(entry::TYPE));
    a.lda(z(zp::DIRECTORY_ENTRY).ind_y());
    a.and(imm(KIND_BITS));
    a.cmp(abs(page::FILE_KIND).x());
    a.bne(mismatch);
    a.jsr("keep_entry");
    a.ldy(imm(entry::TRACK));
    a.lda(z(zp::DIRECTORY_ENTRY).ind_y());
    a.sta(abs(page::FILE_OLD_TRACK).x());
    a.ldy(imm(entry::SECTOR));
    a.lda(z(zp::DIRECTORY_ENTRY).ind_y());
    a.sta(abs(page::FILE_OLD_SECTOR).x());
    placing(a, place, failed);
}

/// `create_file` run on at `place`, its slot found and file X pulled: the
/// first block found and the entry written or marked, the file then open to
/// write; to `failed` where that cannot be.
fn placing(a: &mut Asm, place: Label, failed: Label) {
    let (replaced, started) = (a.local(), a.local());
    a.bind(place);
    a.pla();
    a.jsr("allocate_first");
    a.bcs(failed);
    a.sta(abs(page::FILE_FIRST_TRACK).x());
    a.sta(abs(page::FILE_TRACK).x());
    a.tya();
    a.sta(abs(page::FILE_FIRST_SECTOR).x());
    a.sta(abs(page::FILE_SECTOR).x());
    a.lda(abs(page::FILE_TRACK).x());
    a.jsr("fill_track");
    a.bcs(failed);
    a.lda(abs(page::FILE_OLD_TRACK).x());
    a.bne(replaced);
    a.jsr("write_new_entry");
    a.bcs(failed);
    a.bcc(started);
    a.bind(replaced);
    a.lda(z(zp::CHANNEL));
    a.cmp(imm(channel::SAVE));
    a.beq(started);
    a.jsr("mark_replaced");
    a.bcs(failed);
    a.bind(started);
    a.jsr("start_block");
    a.lda(imm(mode::WRITE));
    a.sta(abs(page::FILE_MODE).x());
    a.clc();
    a.rts();
}

/// `open_append`: the named file opened on channel file X to write on
/// after its last byte, its entry unclosed meanwhile: FILE NOT FOUND where
/// absent, WRITE FILE OPEN where being written, FILE TYPE MISMATCH where
/// of another type than asked or relative, whose records a byte stream
/// would break. Its length comes to the blocks before its last, one at
/// least, and every block the append writes, as the original's counts it.
fn appending(a: &mut Asm) {
    a.here("open_append");
    let (missing, found, walk, failed, streamed) =
        (a.local(), a.local(), a.local(), a.local(), a.local());
    a.lda(z(zp::NAME_END));
    a.cmp(z(zp::NAME_START));
    a.beq(missing);
    a.jsr("ensure_ready");
    a.bcs(failed);
    a.jsr("name_as_pattern");
    a.jsr("find_entry");
    a.bcc(found);
    a.lda(z(zp::ERROR));
    a.bne(failed);
    a.bind(missing);
    a.lda(imm(number::FILE_NOT_FOUND));
    a.jmp("refuse_open");
    a.bind(failed);
    a.jmp("open_failed");
    a.bind(found);
    a.jsr("found_checked");
    a.bcs(RTS);
    a.ldy(imm(entry::TYPE));
    a.lda(z(zp::DIRECTORY_ENTRY).ind_y());
    a.and(imm(KIND_BITS));
    a.cmp(imm(kind::REL));
    a.bne(streamed);
    a.lda(imm(number::TYPE_MISMATCH));
    a.jmp("refuse_open");
    a.bind(streamed);
    a.jsr("channel_file");
    a.jsr("keep_entry");
    a.lda(imm(0));
    a.sta(abs(page::FILE_OLD_TRACK).x());
    a.sta(abs(page::FILE_FILL).x());
    a.ldy(imm(entry::TYPE));
    a.lda(z(zp::DIRECTORY_ENTRY).ind_y());
    a.and(imm(!kind::CLOSED));
    a.sta(abs(page::FILE_KIND).x());
    a.sta(z(zp::DIRECTORY_ENTRY).ind_y());
    a.ldy(imm(entry::TRACK));
    a.lda(z(zp::DIRECTORY_ENTRY).ind_y());
    a.sta(abs(page::FILE_FIRST_TRACK).x());
    a.sta(abs(page::FILE_TRACK).x());
    a.ldy(imm(entry::SECTOR));
    a.lda(z(zp::DIRECTORY_ENTRY).ind_y());
    a.sta(abs(page::FILE_FIRST_SECTOR).x());
    a.sta(abs(page::FILE_SECTOR).x());
    a.txa();
    a.pha();
    a.jsr("write_directory");
    a.pla();
    a.tax();
    a.bcs(failed);
    a.lda(imm(0));
    a.sta(abs(page::FILE_BLOCKS).x());
    a.sta(abs(page::FILE_BLOCKS_HIGH).x());
    walking(a, walk, failed);
}

/// `open_append` run on at `walk` with file X at its first block: its
/// blocks walked to the last and counted, writing set to go on after the
/// last byte; to `failed` where a block cannot be read.
fn walking(a: &mut Asm, walk: Label, failed: Label) {
    let (last, fewer, low, counted, at_data) =
        (a.local(), a.local(), a.local(), a.local(), a.local());
    a.bind(walk);
    a.lda(abs(page::FILE_TRACK).x());
    a.ldy(abs(page::FILE_SECTOR).x());
    a.jsr("read_block_at");
    a.bcs(failed);
    a.jsr("count_block");
    a.jsr("point_at_file");
    a.ldy(imm(0));
    a.lda(z(zp::FILE_POINTER).ind_y());
    a.beq(last);
    a.sta(abs(page::FILE_TRACK).x());
    a.iny();
    a.lda(z(zp::FILE_POINTER).ind_y());
    a.sta(abs(page::FILE_SECTOR).x());
    a.jmp(walk);
    a.bind(last);
    a.lda(abs(page::FILE_BLOCKS_HIGH).x());
    a.bne(fewer);
    a.lda(abs(page::FILE_BLOCKS).x());
    a.cmp(imm(2));
    a.bcc(counted);
    a.bind(fewer);
    a.lda(abs(page::FILE_BLOCKS).x());
    a.bne(low);
    a.dec(abs(page::FILE_BLOCKS_HIGH).x());
    a.bind(low);
    a.dec(abs(page::FILE_BLOCKS).x());
    a.bind(counted);
    // Writing goes on after the last byte; a block whose link names none
    // is written from its start.
    a.ldy(imm(1));
    a.lda(z(zp::FILE_POINTER).ind_y());
    a.cmp(imm(FIRST_DATA));
    a.bcs(at_data);
    a.lda(imm(FIRST_DATA - 1));
    a.bind(at_data);
    a.clc();
    a.adc(imm(1));
    a.sta(abs(page::FILE_AT).x());
    a.lda(imm(mode::WRITE));
    a.sta(abs(page::FILE_MODE).x());
    a.clc();
    a.rts();
}

/// `file_write`: A, sent to the channel, written to its file where it
/// writes. `write_file_byte`: A written to file X, the full block before
/// it linked and written first; C set where the file can take no more.
/// `block_full`: file X's full block given its successor and written, a
/// new block started; ENDED and C set where it cannot be, a data
/// channel's BAM written as it runs out, as the original's is. Each keeps
/// X.
fn writing(a: &mut Asm) {
    a.here("file_write");
    let (dropped, not_direct, written) = (a.local(), a.local(), a.local());
    a.pha();
    a.jsr("channel_file");
    a.bcs(dropped);
    a.lda(abs(page::FILE_MODE).x());
    a.cmp(imm(mode::DIRECT));
    a.bne(not_direct);
    a.pla();
    a.jmp("direct_write");
    a.bind(not_direct);
    a.cmp(imm(mode::REL));
    a.bne(written);
    a.pla();
    a.jmp("rel_write");
    a.bind(written);
    a.cmp(imm(mode::WRITE));
    a.bne(dropped);
    a.pla();
    a.here("write_file_byte");
    let room = a.local();
    a.pha();
    a.lda(abs(page::FILE_AT).x());
    a.bne(room);
    a.jsr("block_full");
    a.bcs(dropped);
    a.bind(room);
    a.jsr("point_at_file");
    a.pla();
    a.ldy(abs(page::FILE_AT).x());
    a.sta(z(zp::FILE_POINTER).ind_y());
    a.inc(abs(page::FILE_AT).x());
    a.clc();
    a.rts();
    a.bind(dropped);
    a.pla();
    a.sec();
    a.rts();

    a.here("block_full");
    let (full, ended) = (a.local(), a.local());
    a.lda(abs(page::FILE_TRACK).x());
    a.ldy(abs(page::FILE_SECTOR).x());
    a.jsr("allocate_next");
    a.bcs(full);
    a.pha();
    a.tya();
    a.pha();
    a.jsr("point_at_file");
    a.ldy(imm(1));
    a.pla();
    a.sta(z(zp::FILE_POINTER).ind_y());
    a.dey();
    a.pla();
    a.sta(z(zp::FILE_POINTER).ind_y());
    a.lda(abs(page::FILE_TRACK).x());
    a.ldy(abs(page::FILE_SECTOR).x());
    a.jsr("write_block_at");
    a.bcs(ended);
    a.ldy(imm(0));
    a.lda(z(zp::FILE_POINTER).ind_y());
    a.sta(abs(page::FILE_TRACK).x());
    a.iny();
    a.lda(z(zp::FILE_POINTER).ind_y());
    a.sta(abs(page::FILE_SECTOR).x());
    // The block written, its buffer is free for the BAM's copy.
    a.lda(abs(page::FILE_TRACK).x());
    a.jsr("fill_track");
    a.bcs(ended);
    a.jsr("count_block");
    a.jsr("start_block");
    a.clc();
    a.rts();
    a.bind(full);
    a.lda(z(zp::CHANNEL));
    a.cmp(imm(channel::SAVE));
    a.beq(ended);
    a.jsr("write_bam");
    a.bind(ended);
    a.lda(imm(mode::ENDED));
    a.sta(abs(page::FILE_MODE).x());
    a.sec();
    a.rts();
}

/// `start_block`: file X's buffer cleared, its next byte the first data
/// byte. `count_block`: one more block in file X's length. `fill_track`:
/// track A, where file X has a block on it at last, recorded as the one it
/// fills, and off the SAVE channel the BAM written so through X's buffer,
/// which holds nothing yet; A and Y kept, C set where the write fails.
/// Each keeps X.
fn blocks(a: &mut Asm) {
    a.here("start_block");
    let clear = a.local();
    a.jsr("point_at_file");
    a.lda(imm(0));
    a.tay();
    a.bind(clear);
    a.sta(z(zp::FILE_POINTER).ind_y());
    a.iny();
    a.bne(clear);
    a.lda(imm(FIRST_DATA));
    a.sta(abs(page::FILE_AT).x());
    a.rts();

    a.here("count_block");
    a.inc(abs(page::FILE_BLOCKS).x());
    a.bne(RTS);
    a.inc(abs(page::FILE_BLOCKS_HIGH).x());
    a.rts();

    a.here("fill_track");
    let (saving, unchanged) = (a.local(), a.local());
    a.cmp(abs(page::FILE_FILL).x());
    a.beq(unchanged);
    a.sta(abs(page::FILE_FILL).x());
    a.pha();
    a.lda(z(zp::CHANNEL));
    a.cmp(imm(channel::SAVE));
    a.beq(saving);
    a.tya();
    a.pha();
    a.lda(abs(page::FILE_FILL).x());
    a.jsr("write_bam_filling");
    a.pla();
    a.tay();
    a.pla();
    a.rts();
    a.bind(saving);
    a.pla();
    a.bind(unchanged);
    a.clc();
    a.rts();
}

/// `close_write`: file X's last block linked as the last and written, its
/// entry closed with its length, a replaced file's blocks freed and the
/// entry given the new one's, its mark cleared, and the BAM written; an
/// empty file holds a RETURN, as the original's does. C set with the
/// error where a write fails. X kept.
fn closing(a: &mut Asm) {
    a.here("close_write");
    let (linked, entry_done, bam, failed_pulled) = (a.local(), a.local(), a.local(), a.local());
    a.lda(abs(page::FILE_AT).x());
    a.cmp(imm(FIRST_DATA));
    a.bne(linked);
    a.lda(abs(page::FILE_BLOCKS).x());
    a.ora(abs(page::FILE_BLOCKS_HIGH).x());
    a.bne(linked);
    a.lda(imm(CR));
    a.jsr("write_file_byte");
    a.bind(linked);
    a.jsr("point_at_file");
    a.ldy(imm(0));
    a.tya();
    a.sta(z(zp::FILE_POINTER).ind_y());
    a.lda(abs(page::FILE_AT).x());
    a.sec();
    a.sbc(imm(1));
    a.iny();
    a.sta(z(zp::FILE_POINTER).ind_y());
    a.lda(abs(page::FILE_TRACK).x());
    a.ldy(abs(page::FILE_SECTOR).x());
    a.jsr("write_block_at");
    a.bcs(RTS);
    a.jsr("count_block");
    a.txa();
    a.pha();
    a.lda(abs(page::FILE_ENTRY_TRACK).x());
    a.ldy(abs(page::FILE_ENTRY_SECTOR).x());
    a.jsr("entry_sector");
    a.pla();
    a.tax();
    a.pha();
    a.bcs(failed_pulled);
    a.lda(abs(page::FILE_ENTRY).x());
    a.sta(z(zp::DIRECTORY_ENTRY));
    a.ldy(imm(entry::TYPE));
    a.lda(abs(page::FILE_KIND).x());
    a.ora(imm(kind::CLOSED));
    a.sta(z(zp::DIRECTORY_ENTRY).ind_y());
    a.ldy(imm(entry::BLOCKS));
    a.lda(abs(page::FILE_BLOCKS).x());
    a.sta(z(zp::DIRECTORY_ENTRY).ind_y());
    a.iny();
    a.lda(abs(page::FILE_BLOCKS_HIGH).x());
    a.sta(z(zp::DIRECTORY_ENTRY).ind_y());
    a.lda(abs(page::FILE_OLD_TRACK).x());
    a.beq(entry_done);
    a.ldy(imm(entry::TRACK));
    a.lda(abs(page::FILE_FIRST_TRACK).x());
    a.sta(z(zp::DIRECTORY_ENTRY).ind_y());
    a.ldy(imm(entry::SECTOR));
    a.lda(abs(page::FILE_FIRST_SECTOR).x());
    a.sta(z(zp::DIRECTORY_ENTRY).ind_y());
    a.ldy(imm(entry::REPLACEMENT));
    a.lda(imm(0));
    a.sta(z(zp::DIRECTORY_ENTRY).ind_y());
    a.iny();
    a.sta(z(zp::DIRECTORY_ENTRY).ind_y());
    a.bind(entry_done);
    a.jsr("write_directory");
    a.pla();
    a.tax();
    a.pha();
    a.bcs(failed_pulled);
    a.lda(abs(page::FILE_OLD_TRACK).x());
    a.beq(bam);
    a.ldy(abs(page::FILE_OLD_SECTOR).x());
    a.jsr("free_chain");
    a.bcs(failed_pulled);
    a.bind(bam);
    a.pla();
    a.tax();
    a.jmp("write_bam");
    a.bind(failed_pulled);
    a.pla();
    a.tax();
    a.sec();
    a.rts();
}

/// `free_entry`: an empty slot at `DIRECTORY_TRACK`, `DIRECTORY_SECTOR`
/// and `DIRECTORY_ENTRY`, its sector in the directory buffer; every slot
/// used, a new directory sector three on from the last, chained after it,
/// DISK FULL where the directory's track has none; C set with the error,
/// X changed. `keep_entry`: the slot kept as file X's entry.
/// `mark_replaced`: the entry found marked as being replaced by file X, its
/// first block noted, and written.
fn entries(a: &mut Asm) {
    a.here("free_entry");
    let (check, full, found) = (a.local(), a.local(), a.local());
    a.jsr("first_entry");
    a.bcs(RTS);
    a.bind(check);
    a.ldy(imm(entry::TYPE));
    a.lda(z(zp::DIRECTORY_ENTRY).ind_y());
    a.beq(found);
    a.jsr("next_entry");
    a.bcc(check);
    a.lda(z(zp::ERROR));
    a.bne(RTS);
    a.lda(imm(DIRECTORY_TRACK));
    a.jsr("track_has_free");
    a.beq(full);
    a.lda(z(zp::DIRECTORY_SECTOR));
    a.sta(z(zp::ALLOC_SECTOR));
    a.lda(imm(DIRECTORY_INTERLEAVE));
    a.jsr("step_sector");
    a.jsr("first_free_from");
    a.bcs(full);
    a.jsr("use_found");
    a.tya();
    a.pha();
    a.ldy(imm(1));
    a.sta(z(zp::DIRECTORY_AT).ind_y());
    a.dey();
    a.lda(imm(DIRECTORY_TRACK));
    a.sta(z(zp::DIRECTORY_AT).ind_y());
    a.jsr("write_directory");
    a.pla();
    a.bcs(RTS);
    a.sta(z(zp::DIRECTORY_SECTOR));
    a.jsr("empty_directory");
    a.lda(imm(0));
    a.sta(z(zp::DIRECTORY_ENTRY));
    a.bind(found);
    a.clc();
    a.rts();
    a.bind(full);
    a.lda(imm(number::DISK_FULL));
    a.jmp("error_at_zero");

    a.here("keep_entry");
    a.lda(z(zp::DIRECTORY_TRACK));
    a.sta(abs(page::FILE_ENTRY_TRACK).x());
    a.lda(z(zp::DIRECTORY_SECTOR));
    a.sta(abs(page::FILE_ENTRY_SECTOR).x());
    a.lda(z(zp::DIRECTORY_ENTRY));
    a.sta(abs(page::FILE_ENTRY).x());
    a.rts();

    a.here("mark_replaced");
    a.ldy(imm(entry::TYPE));
    a.lda(z(zp::DIRECTORY_ENTRY).ind_y());
    a.ora(imm(kind::REPLACING));
    a.sta(z(zp::DIRECTORY_ENTRY).ind_y());
    a.ldy(imm(entry::REPLACEMENT));
    a.lda(abs(page::FILE_FIRST_TRACK).x());
    a.sta(z(zp::DIRECTORY_ENTRY).ind_y());
    a.iny();
    a.lda(abs(page::FILE_FIRST_SECTOR).x());
    a.sta(z(zp::DIRECTORY_ENTRY).ind_y());
    a.txa();
    a.pha();
    a.jsr("write_directory");
    a.pla();
    a.tax();
    a.rts();
}

/// `write_new_entry`: file X's entry written unclosed: as `fill_new_entry`
/// fills it, its kind, first block and name, the rest zero; X kept.
/// `put_name`: the name parsed into the entry, padded, sixteen characters
/// at most, Y past it; `put_name_at` the same at `POINTER`'s offset Y; X
/// changed. A name ending where it starts is written as the original
/// writes it: the character there, a zero past the command's end, then a
/// whole field's padding, so one inside the command runs a byte past the
/// field. `empty_directory`: the directory buffer emptied, linked as the
/// directory's last sector.
fn entry_writes(a: &mut Asm) {
    a.here("write_new_entry");
    a.jsr("fill_new_entry");
    a.txa();
    a.pha();
    a.jsr("write_directory");
    a.pla();
    a.tax();
    a.rts();

    a.here("fill_new_entry");
    let rest = a.local();
    a.ldy(imm(entry::TYPE));
    a.lda(abs(page::FILE_KIND).x());
    a.sta(z(zp::DIRECTORY_ENTRY).ind_y());
    a.iny();
    a.lda(abs(page::FILE_FIRST_TRACK).x());
    a.sta(z(zp::DIRECTORY_ENTRY).ind_y());
    a.iny();
    a.lda(abs(page::FILE_FIRST_SECTOR).x());
    a.sta(z(zp::DIRECTORY_ENTRY).ind_y());
    a.txa();
    a.pha();
    a.jsr("put_name");
    // The side sectors, record length, replacement and length, zero to the
    // entry's end, over any byte the name ran on into.
    a.lda(z(zp::DIRECTORY_ENTRY));
    a.clc();
    a.adc(imm(entry::NAME + NAME_BYTES));
    a.tay();
    a.adc(imm(entry::BYTES - entry::NAME - NAME_BYTES));
    a.sta(z(zp::COUNT));
    a.lda(imm(0));
    a.bind(rest);
    a.sta(z(zp::DIRECTORY_AT).ind_y());
    a.iny();
    a.cpy(z(zp::COUNT));
    a.bne(rest);
    a.pla();
    a.tax();
    a.rts();

    a.here("put_name");
    let (each, pad) = (a.local(), a.local());
    a.lda(z(zp::DIRECTORY_AT));
    a.sta(z(zp::POINTER));
    a.lda(z(zp::DIRECTORY_AT + 1));
    a.sta(z(zp::POINTER + 1));
    a.lda(z(zp::DIRECTORY_ENTRY));
    a.clc();
    a.adc(imm(entry::NAME));
    a.tay();
    a.here("put_name_at");
    let (separator, ended) = (a.local(), a.local());
    a.lda(imm(NAME_BYTES));
    a.sta(z(zp::COUNT));
    a.ldx(z(zp::NAME_START));
    a.cpx(z(zp::COMMAND_LENGTH));
    a.bcs(ended);
    a.lda(abs(page::COMMAND).x());
    // A comma first is the name and the comma ending it at once.
    a.cmp(imm(b','));
    a.beq(separator);
    a.cpx(z(zp::NAME_END));
    a.bne(each);
    a.bind(separator);
    a.sta(z(zp::POINTER).ind_y());
    a.iny();
    a.inx();
    a.bne(each);
    a.bind(ended);
    a.lda(imm(0));
    a.sta(z(zp::POINTER).ind_y());
    a.iny();
    a.dec(z(zp::COUNT));
    a.bind(each);
    a.lda(imm(PADDING));
    a.cpx(z(zp::NAME_END));
    a.bcs(pad);
    a.lda(abs(page::COMMAND).x());
    a.inx();
    a.bind(pad);
    a.sta(z(zp::POINTER).ind_y());
    a.iny();
    a.dec(z(zp::COUNT));
    a.bne(each);
    a.rts();

    a.here("empty_directory");
    let clear = a.local();
    a.jsr("directory_buffer");
    a.bcs(RTS);
    a.lda(imm(0));
    a.tay();
    a.bind(clear);
    a.sta(z(zp::DIRECTORY_AT).ind_y());
    a.iny();
    a.bne(clear);
    a.lda(imm(LAST_LINK_SECTOR));
    a.iny();
    a.sta(z(zp::DIRECTORY_AT).ind_y());
    a.rts();
}
