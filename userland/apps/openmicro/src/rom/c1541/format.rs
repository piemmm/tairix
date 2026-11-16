//! Format: `N` with a disk's name and an ID lays every track out anew and
//! writes an empty directory; with the name alone it writes the BAM and
//! the directory afresh on a disk already formatted, keeping its ID. Every
//! open file is given up first. The tracks are laid out as the original
//! lays them out at this drive's speed: each sector a SYNC, its header, a
//! gap of nine bytes, a SYNC and its data block, then the zone's gap, the
//! first sector's SYNC a byte longer and the last gap the rest of the
//! revolution; each data block `$4B` then `$01`s, but track 1's, which opens
//! with `$00`. The format runs as the original's does, as code an execute
//! job's buffer jumps to.

use super::controller::{code, DATA, MECHANISM, READING, WRITING};
use super::errors::number;
use super::ws::{buffer, page, zp, BAM, OVERFLOW, OVERFLOW_BYTES};
use crate::cpu::isa::JMP_ABSOLUTE;
use crate::machine::c64::drive::{mechanism, via2};
use crate::machine::via::reg;
use crate::media::c1541::d64;
use crate::media::c1541::layout::{
    bam, DIRECTORY_TRACK, FIRST_DIRECTORY_SECTOR, NAME_BYTES, PADDING, TRACKS,
};
use crate::media::c1541::recording::{
    job, FILL, FILL_FIRST, FILL_FIRST_TRACK_1, GAP, HEADER_GAP, HEADER_GCR, SECTOR_RECORDING, SYNC,
    SYNC_BYTES,
};
use crate::rom::asm::{abs, hi, imm, lo, zp as z, Asm, RTS};

/// The buffers a format uses: the job's, whose code jumps to the format;
/// the track's headers in GCR; and the data block in GCR, run on into the
/// overflow.
const JOB_BUFFER: u8 = 2;
const HEADER_TABLE: u16 = buffer(0);
const BLOCK_BUFFER: u8 = 1;
/// Tries at laying a track out before the format fails.
const TRACK_TRIES: u8 = 3;
/// The gap after each data block, by zone, as the original's at this
/// drive's speed.
const SECTOR_GAPS: [u8; 4] = [8, 11, 18, 8];
/// The BAM header's bytes after the name's field.
const HEADER_PADDING: u8 = bam::HEADER_END - bam::NAME - NAME_BYTES;

/// The gap after track `track`'s last sector: the rest of a revolution.
const fn tail(track: u8) -> usize {
    let sectors = d64::sectors(track) as usize;
    let gap = SECTOR_GAPS[d64::zone(track) as usize] as usize;
    d64::revolution_bytes(d64::zone(track)) - sectors * SECTOR_RECORDING - 1 - (sectors - 1) * gap
}

const _: () = {
    let mut track = 1;
    while track <= TRACKS {
        assert!(
            tail(track) > 0 && tail(track) < 0x100,
            "a track's tail is a byte's count"
        );
        track += 1;
    }
    assert!(
        d64::sectors(1) as usize * HEADER_GCR as usize <= 0x100,
        "a track's headers fit a buffer"
    );
};

pub fn emit(a: &mut Asm) {
    command(a);
    fresh(a);
    disk(a);
    track(a);
    writing(a);
    a.later(|a| {
        a.here("format_gaps");
        a.byte(0);
        for track in 1..=TRACKS {
            a.byte(SECTOR_GAPS[usize::from(d64::zone(track))]);
        }
        a.here("format_tails");
        a.byte(0);
        for track in 1..=TRACKS {
            a.byte(u8::try_from(tail(track)).unwrap_or(u8::MAX));
        }
    });
}

/// `format_command`: `N`. A colon is wanted, SYNTAX ERROR for want of a
/// name; drive 1 is READ ERROR at no block, as the original reports it. The
/// ID is the two characters after the name's comma, a missing one zero;
/// with no comma, or nothing after it, the format is quick, the disk read
/// again for its own. A format that fails is READ ERROR at no block, the
/// disk to be read again.
fn command(a: &mut Asm) {
    a.here("format_command");
    let (colon, after, id, put, quick, fresh, failed) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.ldx(imm(0));
    a.jsr("find_colon");
    a.bcc(colon);
    a.lda(imm(number::NO_NAME));
    a.jmp("error_at_zero");
    a.bind(colon);
    a.jsr("second_drive");
    a.bne(after);
    a.lda(imm(number::NO_SYNC));
    a.jmp("error_at_zero");
    a.bind(after);
    a.txa();
    a.pha();
    a.jsr("drop_files");
    a.pla();
    a.tax();
    a.inx();
    a.jsr("scan_name");
    a.inx();
    a.cpx(z(zp::COMMAND_LENGTH));
    a.bcs(quick);
    a.ldy(imm(0));
    a.bind(id);
    a.lda(imm(0));
    a.cpx(z(zp::COMMAND_LENGTH));
    a.bcs(put);
    a.lda(abs(page::COMMAND).x());
    a.bind(put);
    a.sta(abs(zp::DISK_ID).y());
    a.inx();
    a.iny();
    a.cpy(imm(2));
    a.bne(id);
    a.lda(imm(JMP_ABSOLUTE));
    a.sta(abs(buffer(JOB_BUFFER)));
    a.lda(imm(lo("format_disk")));
    a.sta(abs(buffer(JOB_BUFFER) + 1));
    a.lda(imm(hi("format_disk")));
    a.sta(abs(buffer(JOB_BUFFER) + 2));
    a.ldx(imm(JOB_BUFFER));
    a.lda(imm(1));
    a.ldy(imm(0));
    a.jsr("set_header");
    a.lda(imm(code::EXECUTE));
    a.jsr("run_job");
    a.lda(z(zp::FORMAT_RESULT));
    a.cmp(imm(job::OK));
    a.beq(fresh);
    a.lda(imm(number::NO_SYNC));
    a.jsr("error_at_zero");
    a.jmp("forget_disk");
    a.bind(quick);
    a.jsr("initialize");
    a.bcs(RTS);
    a.bind(fresh);
    a.jsr("fresh_bam");
    a.jsr("write_bam");
    a.bcs(failed);
    a.jsr("empty_directory");
    a.lda(imm(DIRECTORY_TRACK));
    a.sta(z(zp::DIRECTORY_TRACK));
    a.lda(imm(FIRST_DIRECTORY_SECTOR));
    a.sta(z(zp::DIRECTORY_SECTOR));
    a.jsr("write_directory");
    a.bcs(failed);
    a.lda(imm(1));
    a.sta(z(zp::DISK_READY));
    a.rts();
    a.bind(failed);
    a.jmp("forget_disk");
}

/// `fresh_bam`: a new BAM in its buffer: linked to the directory, the
/// version, the name parsed, the disk's ID and the DOS type with their
/// padding, and every block free but the BAM's and the directory's first.
fn fresh(a: &mut Asm) {
    a.here("fresh_bam");
    let (clear, padding) = (a.local(), a.local());
    a.lda(imm(0));
    a.tay();
    a.bind(clear);
    a.sta(abs(BAM).y());
    a.iny();
    a.bne(clear);
    a.lda(imm(DIRECTORY_TRACK));
    a.sta(abs(BAM));
    a.lda(imm(FIRST_DIRECTORY_SECTOR));
    a.sta(abs(BAM + 1));
    a.lda(imm(bam::VERSION));
    a.sta(abs(BAM + u16::from(bam::VERSION_AT)));
    a.lda(imm(lo(BAM)));
    a.sta(z(zp::POINTER));
    a.lda(imm(hi(BAM)));
    a.sta(z(zp::POINTER + 1));
    a.ldy(imm(bam::NAME));
    a.jsr("put_name_at");
    // The header's padding runs on from wherever the name ended, the ID and
    // the DOS type over it.
    a.ldx(imm(HEADER_PADDING));
    a.lda(imm(PADDING));
    a.bind(padding);
    a.sta(abs(BAM).y());
    a.iny();
    a.dex();
    a.bne(padding);
    a.lda(z(zp::DISK_ID));
    a.sta(abs(BAM + u16::from(bam::ID)));
    a.lda(z(zp::DISK_ID + 1));
    a.sta(abs(BAM + u16::from(bam::ID) + 1));
    a.lda(imm(bam::DOS_TYPE[0]));
    a.sta(abs(BAM + u16::from(bam::DOS_TYPE_AT)));
    a.lda(imm(bam::DOS_TYPE[1]));
    a.sta(abs(BAM + u16::from(bam::DOS_TYPE_AT) + 1));
    a.jsr("clear_bam");
    a.lda(imm(DIRECTORY_TRACK));
    a.ldy(imm(FIRST_DIRECTORY_SECTOR));
    a.jmp("use_block");
}

/// `format_disk`, entered from the execute job: the head bumped against
/// its stop, then every track laid out in turn, `FORMAT_RESULT` the
/// outcome; a protected disk is refused before anything is written.
fn disk(a: &mut Asm) {
    a.here("format_disk");
    let (writable, each_track, again, next) = (a.local(), a.local(), a.local(), a.local());
    a.lda(abs(MECHANISM));
    a.and(imm(mechanism::WRITABLE));
    a.bne(writable);
    a.lda(imm(job::WRITE_PROTECT));
    a.sta(z(zp::FORMAT_RESULT));
    a.rts();
    a.bind(writable);
    a.jsr("bump");
    a.lda(imm(1));
    a.sta(z(zp::JOB_TRACK));
    a.bind(each_track);
    a.jsr("seek_track");
    a.lda(imm(TRACK_TRIES));
    a.sta(z(zp::FORMAT_TRIES));
    a.bind(again);
    a.jsr("format_track");
    a.bcc(next);
    a.dec(z(zp::FORMAT_TRIES));
    a.bne(again);
    a.sta(z(zp::FORMAT_RESULT));
    a.rts();
    a.bind(next);
    a.inc(z(zp::JOB_TRACK));
    a.lda(z(zp::JOB_TRACK));
    a.cmp(imm(TRACKS + 1));
    a.bne(each_track);
    a.lda(imm(job::OK));
    a.sta(z(zp::FORMAT_RESULT));
    a.rts();
}

/// `format_track`: `JOB_TRACK`, the head on it, laid out and each sector
/// read back; C set and A the job's code where one does not match. The data
/// block is encoded at tracks 1 and 2 only, every later track's being the
/// same as track 2's.
fn track(a: &mut Asm) {
    a.here("format_track");
    let (fill, first, encoded, headers, copy, verify, expected, failed) = (
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
    a.sta(z(zp::BUFFER));
    a.lda(imm(hi(buffer(BLOCK_BUFFER))));
    a.sta(z(zp::BUFFER + 1));
    a.lda(z(zp::JOB_TRACK));
    a.cmp(imm(3));
    a.bcs(encoded);
    a.lda(imm(FILL));
    a.ldy(imm(0));
    a.bind(fill);
    a.sta(z(zp::BUFFER).ind_y());
    a.iny();
    a.bne(fill);
    a.lda(imm(FILL_FIRST_TRACK_1));
    a.ldx(z(zp::JOB_TRACK));
    a.dex();
    a.beq(first);
    a.lda(imm(FILL_FIRST));
    a.bind(first);
    a.sta(z(zp::BUFFER).ind_y());
    a.jsr("encode_block");
    a.bind(encoded);
    a.lda(imm(0));
    a.sta(z(zp::JOB_SECTOR));
    a.sta(z(zp::FORMAT_HEADER));
    a.bind(headers);
    a.jsr("expect_header");
    a.ldx(imm(0));
    a.ldy(z(zp::FORMAT_HEADER));
    a.bind(copy);
    a.lda(z(zp::EXPECTED).x());
    a.sta(abs(HEADER_TABLE).y());
    a.iny();
    a.inx();
    a.cpx(imm(HEADER_GCR));
    a.bne(copy);
    a.sty(z(zp::FORMAT_HEADER));
    a.inc(z(zp::JOB_SECTOR));
    a.ldx(z(zp::JOB_TRACK));
    a.lda(z(zp::JOB_SECTOR));
    a.cmp(abs("track_sectors").x());
    a.bne(headers);
    a.jsr("write_track");
    a.lda(imm(0));
    a.sta(z(zp::JOB_SECTOR));
    a.sta(z(zp::FORMAT_HEADER));
    // Each header's GCR taken from the table, so the next sector's comes
    // in time.
    a.bind(verify);
    a.ldx(imm(0));
    a.ldy(z(zp::FORMAT_HEADER));
    a.bind(expected);
    a.lda(abs(HEADER_TABLE).y());
    a.sta(z(zp::EXPECTED).x());
    a.iny();
    a.inx();
    a.cpx(imm(HEADER_GCR));
    a.bne(expected);
    a.sty(z(zp::FORMAT_HEADER));
    a.jsr("find_expected");
    a.bcs(failed);
    a.jsr("wait_sync");
    a.bcs(failed);
    a.jsr("compare_block");
    a.cmp(imm(job::OK));
    a.bne(failed);
    a.inc(z(zp::JOB_SECTOR));
    a.ldx(z(zp::JOB_TRACK));
    a.lda(z(zp::JOB_SECTOR));
    a.cmp(abs("track_sectors").x());
    a.bne(verify);
    a.clc();
    a.rts();
    a.bind(failed);
    a.sec();
    a.rts();
}

/// `write_track`: the whole of `JOB_TRACK` written in one revolution from
/// wherever the head is: each sector from the header table and the data
/// block's GCR, as the module's notes lay it out. Each byte goes to the
/// head as BYTE READY asks for the next.
fn writing(a: &mut Asm) {
    a.here("write_track");
    a.ldx(z(zp::JOB_TRACK));
    a.lda(abs("format_gaps").x());
    a.sta(z(zp::FORMAT_GAP));
    a.lda(abs("format_tails").x());
    a.sta(z(zp::FORMAT_TAIL));
    a.lda(abs("track_sectors").x());
    a.sta(z(zp::FORMAT_SECTORS));
    a.lda(imm(0));
    a.sta(z(zp::FORMAT_HEADER));
    a.lda(imm(0xFF));
    a.sta(abs(via2(reg::DDRA)));
    // The first sector's SYNC a byte longer than the rest.
    a.lda(imm(SYNC));
    a.sta(abs(DATA));
    a.lda(imm(WRITING));
    a.sta(abs(via2(reg::PCR)));
    // A byte read before writing began must not count as the first asked
    // for.
    a.clv();
    sectors_written(a);
}

/// `write_track` run on, writing begun: each sector, its gap and at last
/// the tail, then reading again once the last byte is on its way.
fn sectors_written(a: &mut Asm) {
    let (sector, sync, header, header_gap, data_sync, block, overflow, gap, last, tail, drained) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.bind(sector);
    a.lda(imm(SYNC));
    a.ldx(imm(SYNC_BYTES));
    a.bind(sync);
    a.bvc(sync);
    a.clv();
    a.sta(abs(DATA));
    a.dex();
    a.bne(sync);
    a.ldy(z(zp::FORMAT_HEADER));
    a.ldx(imm(HEADER_GCR));
    a.bind(header);
    a.lda(abs(HEADER_TABLE).y());
    let header_ready = a.here_local();
    a.bvc(header_ready);
    a.clv();
    a.sta(abs(DATA));
    a.iny();
    a.dex();
    a.bne(header);
    a.sty(z(zp::FORMAT_HEADER));
    a.lda(imm(GAP));
    a.ldx(imm(HEADER_GAP));
    a.bind(header_gap);
    a.bvc(header_gap);
    a.clv();
    a.sta(abs(DATA));
    a.dex();
    a.bne(header_gap);
    a.lda(imm(SYNC));
    a.ldx(imm(SYNC_BYTES));
    a.bind(data_sync);
    a.bvc(data_sync);
    a.clv();
    a.sta(abs(DATA));
    a.dex();
    a.bne(data_sync);
    a.ldy(imm(0));
    a.bind(block);
    a.lda(z(zp::BUFFER).ind_y());
    let block_ready = a.here_local();
    a.bvc(block_ready);
    a.clv();
    a.sta(abs(DATA));
    a.iny();
    a.bne(block);
    a.bind(overflow);
    a.lda(abs(OVERFLOW).y());
    let overflow_ready = a.here_local();
    a.bvc(overflow_ready);
    a.clv();
    a.sta(abs(DATA));
    a.iny();
    a.cpy(imm(OVERFLOW_BYTES));
    a.bne(overflow);
    a.dec(z(zp::FORMAT_SECTORS));
    a.beq(last);
    a.ldx(z(zp::FORMAT_GAP));
    a.lda(imm(GAP));
    a.bind(gap);
    a.bvc(gap);
    a.clv();
    a.sta(abs(DATA));
    a.dex();
    a.bne(gap);
    a.jmp(sector);
    a.bind(last);
    a.ldx(z(zp::FORMAT_TAIL));
    a.lda(imm(GAP));
    a.bind(tail);
    a.bvc(tail);
    a.clv();
    a.sta(abs(DATA));
    a.dex();
    a.bne(tail);
    a.bind(drained);
    a.bvc(drained);
    a.clv();
    a.lda(imm(READING));
    a.sta(abs(via2(reg::PCR)));
    a.lda(imm(0));
    a.sta(abs(via2(reg::DDRA)));
    a.rts();
}
