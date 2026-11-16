//! The BAM in its buffer: blocks found for files as the original finds
//! them, marked used and free, chains freed and marked, and the BAM
//! written back. A new file starts on the track nearest the directory's,
//! below before above, at its lowest free sector. Each block after steps
//! ten sectors on, one less past the track's end, to the first free from
//! there; a full track moves the file on to the next track away from the
//! directory, and the disk's edge to the other side's nearest, stepping as
//! from sector 0.

use super::errors::number;
use super::ws::{zp, BAM, BAM_BUFFER, TOUCHED_BYTES};
use crate::media::c1541::d64;
use crate::media::c1541::layout::{bam, BAM_SECTOR, DIRECTORY_TRACK, TRACKS};
use crate::rom::asm::{abs, imm, zp as z, Acc, Asm, RTS};

/// The sectors a file's next block steps past, and a directory sector's.
pub const FILE_INTERLEAVE: u8 = 10;
pub const DIRECTORY_INTERLEAVE: u8 = 3;
/// Where the original reports a disk with no block for a new file.
const NO_TRACK: u8 = TRACKS + 1;
const NO_TRACK_SECTOR: u8 = 1;
/// `ALLOC_STEP`'s way away from the directory: down the disk and up it.
const DOWN: u8 = 0xFF;
const UP: u8 = 0x01;

const _: () = assert!(
    DOWN ^ UP == 0xFE,
    "the way is turned by flipping all but its low bit"
);

pub fn emit(a: &mut Asm) {
    finding(a);
    searching(a);
    marking(a);
    writing_back(a);
    chains(a);
    a.later(|a| {
        a.here("track_sectors");
        a.byte(0);
        for track in 1..=TRACKS {
            a.byte(d64::sectors(track));
        }
    });
}

/// `allocate_first`: a new file's first block, used, its track in A and
/// sector in Y; ILLEGAL TRACK OR SECTOR as a system block's, at 36, 1, and
/// C set where the disk has none, as the original reports it.
/// `allocate_next`: the block after track A, sector Y, used, in A and Y;
/// DISK FULL and C set where there is none. Each keeps X.
fn finding(a: &mut Asm) {
    a.here("allocate_first");
    let (step, found, none) = (a.local(), a.local(), a.local());
    a.stx(z(zp::ALLOC_X));
    a.lda(imm(1));
    a.sta(z(zp::ALLOC_STEP));
    a.bind(step);
    a.lda(imm(DIRECTORY_TRACK));
    a.sec();
    a.sbc(z(zp::ALLOC_STEP));
    a.jsr("track_has_free");
    a.bne(found);
    a.lda(imm(DIRECTORY_TRACK));
    a.clc();
    a.adc(z(zp::ALLOC_STEP));
    a.jsr("track_has_free");
    a.bne(found);
    a.inc(z(zp::ALLOC_STEP));
    a.lda(z(zp::ALLOC_STEP));
    a.cmp(imm(DIRECTORY_TRACK));
    a.bcc(step);
    a.bind(none);
    a.lda(imm(number::ILLEGAL_SYSTEM_BLOCK));
    a.ldx(imm(NO_TRACK));
    a.ldy(imm(NO_TRACK_SECTOR));
    a.jsr("set_error");
    a.ldx(z(zp::ALLOC_X));
    a.sec();
    a.rts();
    a.bind(found);
    a.lda(imm(0));
    a.sta(z(zp::ALLOC_SECTOR));
    a.jmp("take_found");

    a.here("allocate_next");
    let (other_track, way, move_on, edge, full) =
        (a.local(), a.local(), a.local(), a.local(), a.local());
    a.stx(z(zp::ALLOC_X));
    a.sty(z(zp::ALLOC_SECTOR));
    a.ldy(imm(0));
    a.sty(z(zp::ALLOC_SWITCHED));
    a.jsr("track_has_free");
    a.beq(other_track);
    a.lda(imm(FILE_INTERLEAVE));
    a.jsr("step_sector");
    a.jmp("take_found");
    a.bind(other_track);
    a.lda(z(zp::ALLOC_TRACK));
    a.cmp(imm(DIRECTORY_TRACK));
    a.lda(imm(UP));
    a.bcs(way);
    a.lda(imm(DOWN));
    a.bind(way);
    a.sta(z(zp::ALLOC_STEP));
    a.bind(move_on);
    a.lda(z(zp::ALLOC_TRACK));
    a.clc();
    a.adc(z(zp::ALLOC_STEP));
    a.beq(edge);
    a.cmp(imm(TRACKS + 1));
    a.beq(edge);
    a.jsr("track_has_free");
    a.beq(move_on);
    a.lda(imm(FILE_INTERLEAVE));
    a.jsr("step_sector");
    a.jmp("take_found");
    a.bind(edge);
    a.lda(z(zp::ALLOC_SWITCHED));
    a.bne(full);
    a.inc(z(zp::ALLOC_SWITCHED));
    a.lda(z(zp::ALLOC_STEP));
    a.eor(imm(DOWN ^ UP));
    a.sta(z(zp::ALLOC_STEP));
    a.lda(imm(DIRECTORY_TRACK));
    a.sta(z(zp::ALLOC_TRACK));
    a.lda(imm(0));
    a.sta(z(zp::ALLOC_SECTOR));
    a.beq(move_on);
    a.bind(full);
    a.lda(imm(number::DISK_FULL));
    a.jsr("error_at_zero");
    a.ldx(z(zp::ALLOC_X));
    a.rts();
}

/// `take_found`: the first free sector on `ALLOC_TRACK` from
/// `ALLOC_SECTOR` used and returned, X from `ALLOC_X`; a track whose bits
/// deny its count is full. `track_has_free`: `ALLOC_TRACK` track A and
/// `ALLOC_SECTORS` its sectors, A its free sectors' count, Z set where it
/// has none. `step_sector`: `ALLOC_SECTOR` stepped A sectors on, wrapping
/// one short past the track's end. `first_free_from`: `ALLOC_SECTOR` the
/// first free sector from it, wrapping, C set where the track has none.
fn searching(a: &mut Asm) {
    a.here("take_found");
    let taken = a.local();
    a.jsr("first_free_from");
    a.bcc(taken);
    a.lda(imm(number::DISK_FULL));
    a.jsr("error_at_zero");
    a.ldx(z(zp::ALLOC_X));
    a.rts();
    a.bind(taken);
    a.jsr("use_found");
    a.ldx(z(zp::ALLOC_X));
    a.clc();
    a.rts();

    a.here("track_has_free");
    a.sta(z(zp::ALLOC_TRACK));
    a.tay();
    a.lda(abs("track_sectors").y());
    a.sta(z(zp::ALLOC_SECTORS));
    a.lda(z(zp::ALLOC_TRACK));
    a.asl(Acc);
    a.asl(Acc);
    a.tay();
    a.lda(abs(BAM).y());
    a.rts();

    a.here("step_sector");
    let set = a.local();
    a.clc();
    a.adc(z(zp::ALLOC_SECTOR));
    a.cmp(z(zp::ALLOC_SECTORS));
    a.bcc(set);
    a.sbc(z(zp::ALLOC_SECTORS));
    a.beq(set);
    a.sec();
    a.sbc(imm(1));
    a.bind(set);
    a.sta(z(zp::ALLOC_SECTOR));
    a.rts();

    a.here("first_free_from");
    let (try_sector, next, found_free) = (a.local(), a.local(), a.local());
    a.lda(z(zp::ALLOC_SECTORS));
    a.sta(z(zp::COUNT));
    a.bind(try_sector);
    a.jsr("bam_bit");
    a.bne(found_free);
    a.inc(z(zp::ALLOC_SECTOR));
    a.lda(z(zp::ALLOC_SECTOR));
    a.cmp(z(zp::ALLOC_SECTORS));
    a.bcc(next);
    a.lda(imm(0));
    a.sta(z(zp::ALLOC_SECTOR));
    a.bind(next);
    a.dec(z(zp::COUNT));
    a.bne(try_sector);
    a.sec();
    a.rts();
    a.bind(found_free);
    a.clc();
    a.rts();
}

/// `bam_bit`: `ALLOC_TRACK`, `ALLOC_SECTOR`'s bit in the BAM, its byte's
/// offset in Y and its mask in `ALLOC_MASK`, A the bit, Z set where the
/// block is used. `use_found`: that block used, its track noted as touched,
/// its track in A and sector in Y. `touch_track`: `ALLOC_TRACK` noted as
/// touched. `use_block` and `free_block`: track A,
/// sector Y used or freed, its track's count kept true, a block already so
/// left alone. `track_count_at`: Y at `ALLOC_TRACK`'s count in the BAM, A
/// the count. These keep X. `count_free`: `FREE` the blocks the tracks'
/// counts give free, but the directory track's, which no file takes; X
/// changed, Y kept.
fn marking(a: &mut Asm) {
    a.here("count_free");
    let (sum, counted) = (a.local(), a.local());
    let entry_of = |track: u8| bam::TRACK_ENTRY_BYTES * track;
    a.lda(imm(0));
    a.sta(z(zp::FREE));
    a.sta(z(zp::FREE + 1));
    a.ldx(imm(entry_of(1)));
    a.bind(sum);
    a.cpx(imm(entry_of(DIRECTORY_TRACK)));
    a.beq(counted);
    a.lda(abs(BAM).x());
    a.clc();
    a.adc(z(zp::FREE));
    a.sta(z(zp::FREE));
    a.bcc(counted);
    a.inc(z(zp::FREE + 1));
    a.bind(counted);
    a.txa();
    a.clc();
    a.adc(imm(bam::TRACK_ENTRY_BYTES));
    a.tax();
    a.cpx(imm(entry_of(TRACKS + 1)));
    a.bne(sum);
    a.rts();

    a.here("bam_bit");
    a.lda(z(zp::ALLOC_SECTOR));
    a.and(imm(0x07));
    a.tay();
    a.lda(abs("bit_of").y());
    a.sta(z(zp::ALLOC_MASK));
    a.lda(z(zp::ALLOC_SECTOR));
    for _ in 0..3 {
        a.lsr(Acc);
    }
    a.sta(z(zp::ALLOC_BYTE));
    a.lda(z(zp::ALLOC_TRACK));
    a.asl(Acc);
    a.asl(Acc);
    // The bitmap follows the count.
    a.sec();
    a.adc(z(zp::ALLOC_BYTE));
    a.tay();
    a.lda(abs(BAM).y());
    a.and(z(zp::ALLOC_MASK));
    a.rts();

    a.here("use_block");
    a.sta(z(zp::ALLOC_TRACK));
    a.sty(z(zp::ALLOC_SECTOR));
    a.jsr("bam_bit");
    a.beq(RTS);
    a.here("use_found");
    a.jsr("bam_bit");
    a.lda(z(zp::ALLOC_MASK));
    a.eor(imm(0xFF));
    a.and(abs(BAM).y());
    a.sta(abs(BAM).y());
    a.jsr("track_count_at");
    a.sec();
    a.sbc(imm(1));
    a.sta(abs(BAM).y());
    a.jsr("touch_track");
    a.lda(z(zp::ALLOC_TRACK));
    a.ldy(z(zp::ALLOC_SECTOR));
    a.rts();

    a.here("touch_track");
    a.lda(z(zp::ALLOC_TRACK));
    a.and(imm(0x07));
    a.tay();
    a.lda(abs("bit_of").y());
    a.pha();
    a.lda(z(zp::ALLOC_TRACK));
    for _ in 0..3 {
        a.lsr(Acc);
    }
    a.tay();
    a.pla();
    a.ora(abs(zp::TOUCHED).y());
    a.sta(abs(zp::TOUCHED).y());
    a.rts();

    a.here("free_block");
    a.sta(z(zp::ALLOC_TRACK));
    a.sty(z(zp::ALLOC_SECTOR));
    a.jsr("bam_bit");
    a.bne(RTS);
    a.lda(z(zp::ALLOC_MASK));
    a.ora(abs(BAM).y());
    a.sta(abs(BAM).y());
    a.jsr("track_count_at");
    a.clc();
    a.adc(imm(1));
    a.sta(abs(BAM).y());
    a.rts();

    a.here("track_count_at");
    a.lda(z(zp::ALLOC_TRACK));
    a.asl(Acc);
    a.asl(Acc);
    a.tay();
    a.lda(abs(BAM).y());
    a.rts();
}

/// `write_bam`: the BAM written back whole. `untouch_tracks`: no track
/// touched, as the BAM is read or written whole; C kept.
/// `write_bam_filling`: a copy of the BAM written through buffer X with
/// every touched track recorded wholly used, as the original's records the
/// tracks files are filling, so a file never closed leaves no block of it
/// free; the BAM in memory stays as it is, C set where the write fails.
/// Each keeps X.
fn writing_back(a: &mut Asm) {
    a.here("write_bam");
    let written = a.local();
    a.txa();
    a.pha();
    a.ldx(imm(BAM_BUFFER));
    a.lda(imm(DIRECTORY_TRACK));
    a.ldy(imm(BAM_SECTOR));
    a.jsr("write_block_at");
    a.bcs(written);
    a.jsr("untouch_tracks");
    a.bind(written);
    a.pla();
    a.tax();
    a.rts();

    a.here("untouch_tracks");
    let each = a.local();
    a.lda(imm(0));
    a.ldy(imm(TOUCHED_BYTES - 1));
    a.bind(each);
    a.sta(abs(zp::TOUCHED).y());
    a.dey();
    a.bpl(each);
    a.rts();

    a.here("write_bam_filling");
    let (copy, each, kept) = (a.local(), a.local(), a.local());
    a.jsr("point_at_file");
    a.ldy(imm(0));
    a.bind(copy);
    a.lda(abs(BAM).y());
    a.sta(z(zp::FILE_POINTER).ind_y());
    a.iny();
    a.bne(copy);
    a.txa();
    a.pha();
    // X the track, from 0, whose bit is never set.
    a.ldx(imm(0));
    a.bind(each);
    a.txa();
    for _ in 0..3 {
        a.lsr(Acc);
    }
    a.tay();
    a.lda(abs(zp::TOUCHED).y());
    a.sta(z(zp::COUNT));
    a.txa();
    a.and(imm(0x07));
    a.tay();
    a.lda(abs("bit_of").y());
    a.and(z(zp::COUNT));
    a.beq(kept);
    a.txa();
    a.asl(Acc);
    a.asl(Acc);
    a.tay();
    a.lda(imm(0));
    for _ in 0..bam::TRACK_ENTRY_BYTES {
        a.sta(z(zp::FILE_POINTER).ind_y());
        a.iny();
    }
    a.bind(kept);
    a.inx();
    a.cpx(imm(TRACKS + 1));
    a.bne(each);
    a.pla();
    a.tax();
    a.lda(imm(DIRECTORY_TRACK));
    a.ldy(imm(BAM_SECTOR));
    a.jmp("write_block_at");
}

/// `free_chain` and `use_chain`: every block of the chain from track A,
/// sector Y freed or used, each read through buffer X for its link; C set
/// with the error where one cannot be read. Each keeps X. `clear_bam`:
/// every block free but the BAM's own.
fn chains(a: &mut Asm) {
    a.here("free_chain");
    let (walk, block, done) = (a.local(), a.local(), a.local());
    a.pha();
    a.lda(imm(0));
    a.beq(walk);
    a.here("use_chain");
    a.pha();
    a.lda(imm(1));
    a.bind(walk);
    a.sta(z(zp::CHAIN_USES));
    a.pla();
    a.bind(block);
    a.sta(z(zp::CHAIN_TRACK));
    a.sty(z(zp::CHAIN_SECTOR));
    let (using, marked) = (a.local(), a.local());
    a.lda(z(zp::CHAIN_USES));
    a.bne(using);
    a.lda(z(zp::CHAIN_TRACK));
    a.jsr("free_block");
    a.jmp(marked);
    a.bind(using);
    a.lda(z(zp::CHAIN_TRACK));
    a.jsr("use_block");
    a.bind(marked);
    a.lda(z(zp::CHAIN_TRACK));
    a.ldy(z(zp::CHAIN_SECTOR));
    a.jsr("read_block_at");
    a.bcs(RTS);
    a.jsr("point_at_file");
    a.ldy(imm(0));
    a.lda(z(zp::FILE_POINTER).ind_y());
    a.beq(done);
    a.pha();
    a.iny();
    a.lda(z(zp::FILE_POINTER).ind_y());
    a.tay();
    a.pla();
    a.jmp(block);
    a.bind(done);
    a.clc();
    a.rts();

    a.here("clear_bam");
    let (track, bits) = (a.local(), a.local());
    a.txa();
    a.pha();
    a.ldx(imm(bam::TRACK_ENTRY_BYTES));
    a.ldy(imm(1));
    a.bind(track);
    a.lda(abs("track_sectors").y());
    a.sta(abs(BAM).x());
    // The track's first sixteen sectors' bits, then those it has past them.
    a.sec();
    a.sbc(imm(16));
    a.sta(z(zp::COUNT));
    a.lda(imm(0xFF));
    a.sta(abs(BAM + 1).x());
    a.sta(abs(BAM + 2).x());
    a.lda(imm(0));
    a.bind(bits);
    a.sec();
    a.rol(Acc);
    a.dec(z(zp::COUNT));
    a.bne(bits);
    a.sta(abs(BAM + 3).x());
    a.txa();
    a.clc();
    a.adc(imm(bam::TRACK_ENTRY_BYTES));
    a.tax();
    a.iny();
    a.cpy(imm(TRACKS + 1));
    a.bne(track);
    a.lda(imm(DIRECTORY_TRACK));
    a.ldy(imm(BAM_SECTOR));
    a.jsr("use_block");
    a.pla();
    a.tax();
    a.rts();
}
