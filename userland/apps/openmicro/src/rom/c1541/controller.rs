//! The controller, from VIA2's timer A: the motor started and let run on a
//! while after the last job, then each job in the queue done against the
//! head in turn. A job's track is reached a half-track at a time, the
//! track a header names correcting where the head was thought to be; a
//! sector is found by its header, compared with the job's track and sector
//! and the disk's ID. A read decodes the data block after it, a write
//! replaces it, a verify compares it.

use super::ws::{zp, BUFFERS, OVERFLOW, OVERFLOW_BYTES};
use crate::machine::c64::drive::{mechanism, via2};
use crate::machine::via::{irq, reg};
use crate::media::c1541::recording::{
    job, GAP, HEADER_GCR, HEADER_MARK, HEADER_PADDING, SYNC_BYTES,
};
use crate::media::c1541::{d64, HALF_TRACKS};
use crate::rom::asm::{abs, hi, imm, zp as z, Acc, Asm, Label, RTS};

/// The jobs, as their codes' top nybble.
pub mod code {
    pub const READ: u8 = 0x80;
    pub const WRITE: u8 = 0x90;
    pub const VERIFY: u8 = 0xA0;
    pub const SEEK: u8 = 0xB0;
    pub const BUMP: u8 = 0xC0;
    pub const JUMP: u8 = 0xD0;
    pub const EXECUTE: u8 = 0xE0;
}

/// The controller's tick, ten milliseconds of the drive's cycles less the
/// timer's two of reloading.
pub const TICK: u16 = 10_000 - 2;
/// Ticks the motor takes to come up to speed, and an idle one runs on.
const SPIN_UP_TICKS: u8 = 50;
const SPIN_DOWN_TICKS: u8 = 250;
/// Half-tracks from a step's start to the head's resting: passes of the
/// delay loop for a step and for the settling after the last.
const STEP_PASSES: u8 = 12;
const SETTLE_PASSES: u8 = 60;
/// Half-tracks stepped out on a bump, more than the stop is from the last.
const BUMP_STEPS: u8 = 90;
/// The half-track the head rests on at power-on, track 18's.
pub const HOME_HALF_TRACK: u8 = 34;
/// Headers looked at for a sector: two revolutions of the longest track.
const HEADER_TRIES: u8 = 2 * 21;
/// Passes of 256 of the SYNC watch, about twenty milliseconds.
const SYNC_PASSES: u8 = 8;
/// The gap's bytes written after a data block.
const TRAILING_GAP_BYTES: u8 = 2;
/// A header's GCR bytes holding its first five bytes, and the top bits of
/// the next, which hold the rest of the ID before the padding.
const MEANINGFUL_GCR: u8 = 7;
const LAST_MEANINGFUL_BITS: u8 = 0xF0;
/// VIA2's PCR reading and writing: CA2 high, letting BYTE READY reach SO;
/// CB2 high to read, low to write.
pub const READING: u8 = 0xEE;
pub(super) const WRITING: u8 = 0xCE;

pub(super) const MECHANISM: u16 = via2(reg::ORB);
pub(super) const DATA: u16 = via2(reg::ORA);

pub fn emit(a: &mut Asm) {
    interrupt(a);
    tick(a);
    head(a);
    sectors(a);
    headers(a);
    reading_block(a);
    jobs(a);
    sought(a);
    writing_job(a);
    a.later(|a| {
        a.here("track_zones");
        for track in 0..=HALF_TRACKS / 2 {
            let track = u8::try_from(track).unwrap_or(u8::MAX);
            a.byte(d64::zone(track) << mechanism::ZONE.trailing_zeros());
        }
    });
}

/// The IRQ: VIA2's timer A ticks the controller; the registers are kept.
fn interrupt(a: &mut Asm) {
    a.here("irq");
    let done = a.local();
    a.pha();
    a.txa();
    a.pha();
    a.tya();
    a.pha();
    a.lda(abs(via2(reg::IFR)));
    a.and(imm(irq::T1));
    a.beq(done);
    a.lda(abs(via2(reg::T1C_L)));
    a.jsr("controller");
    a.bind(done);
    a.pla();
    a.tay();
    a.pla();
    a.tax();
    a.pla();
    a.rti();
}

/// A tick: the first job waiting is done once the motor is up to speed;
/// with none, an idle motor counts down to stopping. A disk going in or out
/// blocks the write-protect sensor a moment: any change to it leaves the
/// disk to be made ready again.
fn tick(a: &mut Asm) {
    a.here("controller");
    let (steady, find, job_waiting, running, up, stop) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.lda(abs(MECHANISM));
    a.and(imm(mechanism::WRITABLE));
    a.cmp(z(zp::SENSOR));
    a.beq(steady);
    a.sta(z(zp::SENSOR));
    a.lda(imm(0));
    a.sta(z(zp::DISK_READY));
    a.bind(steady);
    a.ldx(imm(5));
    a.bind(find);
    a.lda(z(zp::JOBS).x());
    a.bmi(job_waiting);
    a.dex();
    a.bpl(find);
    a.lda(z(zp::SPIN_DOWN));
    a.beq(RTS);
    a.dec(z(zp::SPIN_DOWN));
    a.beq(stop);
    a.rts();
    a.bind(stop);
    a.lda(abs(MECHANISM));
    a.and(imm(!(mechanism::MOTOR | mechanism::LED)));
    a.sta(abs(MECHANISM));
    a.rts();

    a.bind(job_waiting);
    a.lda(abs(MECHANISM));
    a.and(imm(mechanism::MOTOR));
    a.bne(running);
    a.lda(abs(MECHANISM));
    a.ora(imm(mechanism::MOTOR | mechanism::LED));
    a.sta(abs(MECHANISM));
    a.lda(imm(SPIN_UP_TICKS));
    a.sta(z(zp::SPIN_UP));
    a.bind(running);
    a.lda(z(zp::SPIN_UP));
    a.beq(up);
    a.dec(z(zp::SPIN_UP));
    a.rts();
    a.bind(up);
    a.lda(imm(SPIN_DOWN_TICKS));
    a.sta(z(zp::SPIN_DOWN));
    a.stx(z(zp::JOB_BUFFER));
    a.txa();
    a.asl(Acc);
    a.tay();
    a.lda(abs(zp::HEADERS).y());
    a.sta(z(zp::JOB_TRACK));
    a.lda(abs(zp::HEADERS + 1).y());
    a.sta(z(zp::JOB_SECTOR));
    a.lda(imm(0));
    a.sta(z(zp::BUFFER));
    a.txa();
    a.clc();
    a.adc(imm(hi(BUFFERS)));
    a.sta(z(zp::BUFFER + 1));
    a.jsr("do_job");
    a.ldx(z(zp::JOB_BUFFER));
    a.sta(z(zp::JOBS).x());
    a.rts();
}

/// `seek_track`: the head stepped a half-track at a time to the job's
/// track, then let settle, and the track's zone selected. `bump`: the head
/// stepped out against its stop, track 1's half-track. Each phase is the
/// half-track's low two bits, so one step round moves the head one way.
fn head(a: &mut Asm) {
    a.here("seek_track");
    let (step, inward, set, there, unmoved) =
        (a.local(), a.local(), a.local(), a.local(), a.local());
    a.lda(z(zp::JOB_TRACK));
    a.sec();
    a.sbc(imm(1));
    a.asl(Acc);
    a.cmp(z(zp::HALF_TRACK));
    a.beq(unmoved);
    a.bind(step);
    a.cmp(z(zp::HALF_TRACK));
    a.beq(there);
    a.pha();
    a.bcs(inward);
    a.dec(z(zp::HALF_TRACK));
    a.skip_over(|a| {
        a.bind(inward);
        a.inc(z(zp::HALF_TRACK));
    });
    a.bind(set);
    a.jsr("set_phase");
    a.ldx(imm(STEP_PASSES));
    a.jsr("delay");
    a.pla();
    a.jmp(step);
    a.bind(there);
    a.ldx(imm(SETTLE_PASSES));
    a.jsr("delay");
    a.bind(unmoved);
    a.ldx(z(zp::JOB_TRACK));
    a.lda(abs(MECHANISM));
    a.and(imm(!mechanism::ZONE));
    a.ora(abs("track_zones").x());
    a.sta(abs(MECHANISM));
    a.rts();

    a.here("bump");
    let out = a.local();
    a.ldy(imm(BUMP_STEPS));
    a.bind(out);
    a.dec(z(zp::HALF_TRACK));
    a.jsr("set_phase");
    a.ldx(imm(STEP_PASSES));
    a.jsr("delay");
    a.dey();
    a.bne(out);
    a.lda(imm(0));
    a.sta(z(zp::HALF_TRACK));
    a.lda(imm(job::OK));
    a.rts();

    a.here("set_phase");
    a.lda(z(zp::HALF_TRACK));
    a.and(imm(mechanism::PHASE));
    a.sta(z(zp::INDEX));
    a.lda(abs(MECHANISM));
    a.and(imm(!mechanism::PHASE));
    a.ora(z(zp::INDEX));
    a.sta(abs(MECHANISM));
    a.rts();

    // X passes of about a thousand cycles; A and Y kept.
    a.here("delay");
    let (outer, inner) = (a.local(), a.local());
    a.txa();
    a.pha();
    a.bind(outer);
    a.ldx(imm(200));
    a.bind(inner);
    a.dex();
    a.bne(inner);
    a.pla();
    a.tax();
    a.dex();
    a.txa();
    a.pha();
    a.bne(outer);
    a.pla();
    a.rts();
}

/// `wait_sync`: SYNC waited for about twenty milliseconds, C set and A
/// `NO_SYNC` where none comes; V cleared for the first byte after it.
/// `find_header`: the job's sector's header, read from each SYNC on the
/// track until it comes; a header naming another track moves the head
/// there and starts again. `find_expected`: the same, `EXPECTED` already
/// holding the header's GCR.
fn sectors(a: &mut Asm) {
    a.here("wait_sync");
    let (watch, found) = (a.local(), a.local());
    a.ldx(imm(0));
    a.ldy(imm(SYNC_PASSES));
    a.bind(watch);
    a.bit(abs(MECHANISM));
    a.bpl(found);
    a.dex();
    a.bne(watch);
    a.dey();
    a.bne(watch);
    a.lda(imm(job::NO_SYNC));
    a.sec();
    a.rts();
    a.bind(found);
    a.lda(abs(DATA));
    a.clv();
    a.clc();
    a.rts();

    a.here("find_header");
    let (next, ours, not_wanted, wrong_track, mismatch, header_bad, compare) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.jsr("expect_header");
    a.here("find_expected");
    a.lda(imm(HEADER_TRIES));
    a.sta(z(zp::TRIES));
    a.bind(next);
    a.jsr("read_raw_header");
    a.bcs(RTS);
    // The GCR that holds the mark, checksum, sector, track and both of the
    // ID's characters, compared as it came: the data block follows too
    // soon to decode a header first.
    a.ldx(imm(MEANINGFUL_GCR - 1));
    a.bind(compare);
    a.lda(z(zp::RAW).x());
    a.cmp(z(zp::EXPECTED).x());
    a.bne(ours);
    a.dex();
    a.bpl(compare);
    a.lda(z(zp::RAW + u16::from(MEANINGFUL_GCR)));
    a.eor(z(zp::EXPECTED + u16::from(MEANINGFUL_GCR)));
    a.and(imm(LAST_MEANINGFUL_BITS));
    a.bne(ours);
    a.clc();
    a.rts();
    // Not the header wanted: decoded, to tell another sector from another
    // track, another disk or a header gone bad.
    a.bind(ours);
    a.jsr("decode_raw_header");
    a.bcs(not_wanted);
    a.lda(z(zp::HEADER + 3));
    a.cmp(z(zp::JOB_TRACK));
    a.bne(wrong_track);
    a.lda(z(zp::HEADER + 2));
    a.cmp(z(zp::JOB_SECTOR));
    a.bne(not_wanted);
    a.eor(z(zp::HEADER + 3));
    a.eor(z(zp::HEADER + 4));
    a.eor(z(zp::HEADER + 5));
    a.cmp(z(zp::HEADER + 1));
    a.bne(header_bad);
    a.lda(z(zp::HEADER + 5));
    a.cmp(z(zp::DISK_ID));
    a.bne(mismatch);
    a.lda(z(zp::HEADER + 4));
    a.cmp(z(zp::DISK_ID + 1));
    a.bne(mismatch);
    a.bind(not_wanted);
    a.dec(z(zp::TRIES));
    a.bne(next);
    a.lda(imm(job::NO_HEADER));
    a.sec();
    a.rts();
    a.bind(header_bad);
    a.lda(imm(job::HEADER_CHECKSUM));
    a.sec();
    a.rts();
    a.bind(mismatch);
    a.lda(imm(job::ID_MISMATCH));
    a.sec();
    a.rts();
    // The head was on the header's track: step from there, the tries
    // still counting so a disk whose headers lie cannot keep it stepping.
    a.bind(wrong_track);
    a.sec();
    a.sbc(imm(1));
    a.asl(Acc);
    a.sta(z(zp::HALF_TRACK));
    a.jsr("seek_track");
    a.jmp(not_wanted);
}

/// `expect_header`: `EXPECTED` the job's sector's header encoded, its
/// checksum from the track, sector and the disk's ID. `read_raw_header`:
/// the next header's GCR into `RAW`, C set and A `NO_SYNC` where no SYNC
/// comes. `decode_raw_header`: `RAW` into `HEADER`, C set and A
/// `NO_HEADER` where it is not a header. `read_header`: both.
fn headers(a: &mut Asm) {
    a.here("expect_header");
    a.lda(imm(HEADER_MARK));
    a.sta(z(zp::PLAIN));
    a.lda(z(zp::JOB_SECTOR));
    a.sta(z(zp::PLAIN + 2));
    a.eor(z(zp::JOB_TRACK));
    a.eor(z(zp::DISK_ID));
    a.eor(z(zp::DISK_ID + 1));
    a.sta(z(zp::PLAIN + 1));
    a.lda(z(zp::JOB_TRACK));
    a.sta(z(zp::PLAIN + 3));
    a.jsr("encode_group");
    for offset in 0..5u16 {
        a.lda(z(zp::GCR + offset));
        a.sta(z(zp::EXPECTED + offset));
    }
    a.lda(z(zp::DISK_ID + 1));
    a.sta(z(zp::PLAIN));
    a.lda(z(zp::DISK_ID));
    a.sta(z(zp::PLAIN + 1));
    a.lda(imm(HEADER_PADDING));
    a.sta(z(zp::PLAIN + 2));
    a.sta(z(zp::PLAIN + 3));
    a.jsr("encode_group");
    for offset in 0..5u16 {
        a.lda(z(zp::GCR + offset));
        a.sta(z(zp::EXPECTED + 5 + offset));
    }
    a.rts();

    a.here("read_header");
    a.jsr("read_raw_header");
    a.bcs(RTS);
    a.jmp("decode_raw_header");

    a.here("read_raw_header");
    let raw = a.local();
    a.jsr("wait_sync");
    a.bcs(RTS);
    a.ldy(imm(0));
    a.bind(raw);
    a.bvc(raw);
    a.clv();
    a.lda(abs(DATA));
    a.sta(abs(zp::RAW).y());
    a.iny();
    a.cpy(imm(HEADER_GCR));
    a.bne(raw);
    a.clc();
    a.rts();

    a.here("decode_raw_header");
    let not_header = a.local();
    for half in 0..2u16 {
        let (copy, place) = (a.local(), a.local());
        a.ldx(imm(4));
        a.bind(copy);
        a.lda(z(zp::RAW + 5 * half).x());
        a.sta(z(zp::GCR).x());
        a.dex();
        a.bpl(copy);
        a.jsr("decode_group");
        a.bcs(not_header);
        a.ldx(imm(3));
        a.bind(place);
        a.lda(z(zp::PLAIN).x());
        a.sta(z(zp::HEADER + 4 * half).x());
        a.dex();
        a.bpl(place);
    }
    a.lda(z(zp::HEADER));
    a.cmp(imm(HEADER_MARK));
    a.bne(not_header);
    a.clc();
    a.rts();
    a.bind(not_header);
    a.lda(imm(job::NO_HEADER));
    a.sec();
    a.rts();
}

/// `read_block`: the data block after the header as GCR, the buffer then
/// the overflow filled.
fn reading_block(a: &mut Asm) {
    a.here("read_block");
    let (into_buffer, into_overflow) = (a.local(), a.local());
    a.jsr("wait_sync");
    a.bcs(RTS);
    a.ldy(imm(0));
    a.bind(into_buffer);
    a.bvc(into_buffer);
    a.clv();
    a.lda(abs(DATA));
    a.sta(z(zp::BUFFER).ind_y());
    a.iny();
    a.bne(into_buffer);
    a.bind(into_overflow);
    a.bvc(into_overflow);
    a.clv();
    a.lda(abs(DATA));
    a.sta(abs(OVERFLOW).y());
    a.iny();
    a.cpy(imm(OVERFLOW_BYTES));
    a.bne(into_overflow);
    a.clc();
    a.rts();
}

/// `do_job`: the job's code in the queue done, its return code in A.
/// `jump_to_buffer`: the job's buffer run as code.
fn jobs(a: &mut Asm) {
    a.here("do_job");
    let (
        check_jump,
        check_seek,
        check_execute,
        check_read,
        check_write,
        check_verify,
        failed,
        unknown,
    ) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.ldx(z(zp::JOB_BUFFER));
    a.lda(z(zp::JOBS).x());
    a.and(imm(0xF0));
    a.cmp(imm(code::BUMP));
    a.bne(check_jump);
    a.jmp("bump");
    a.bind(check_jump);
    a.cmp(imm(code::JUMP));
    a.bne(check_seek);
    a.jmp("jump_to_buffer");
    a.bind(check_seek);
    a.pha();
    a.jsr("seek_track");
    a.pla();
    a.cmp(imm(code::SEEK));
    a.bne(check_execute);
    a.jmp("seek_job");
    a.bind(check_execute);
    a.cmp(imm(code::EXECUTE));
    a.bne(check_read);
    a.jmp("jump_to_buffer");
    a.bind(check_read);
    a.cmp(imm(code::READ));
    a.bne(check_write);
    a.jsr("find_header");
    a.bcs(failed);
    a.jsr("read_block");
    a.bcs(failed);
    a.jsr("decode_block");
    a.bcs(failed);
    a.lda(imm(job::OK));
    a.bind(failed);
    a.rts();
    a.bind(check_write);
    a.cmp(imm(code::WRITE));
    a.bne(check_verify);
    a.jmp("write_job");
    a.bind(check_verify);
    a.cmp(imm(code::VERIFY));
    a.bne(unknown);
    a.jmp("verify_job");
    // A code no job has is done at once.
    a.bind(unknown);
    a.lda(imm(job::OK));
    a.rts();

    a.here("jump_to_buffer");
    a.jsr("enter_buffer");
    a.lda(imm(job::OK));
    a.rts();
    a.here("enter_buffer");
    a.jmp(abs(zp::BUFFER).ind());
}

/// `seek_job`: the first header on the track gives the disk's ID.
fn sought(a: &mut Asm) {
    a.here("seek_job");
    let (next, found) = (a.local(), a.local());
    a.lda(imm(HEADER_TRIES));
    a.sta(z(zp::TRIES));
    a.bind(next);
    a.jsr("read_header");
    a.bcc(found);
    a.cmp(imm(job::NO_SYNC));
    a.beq(RTS);
    a.dec(z(zp::TRIES));
    a.bne(next);
    a.lda(imm(job::NO_HEADER));
    a.rts();
    a.bind(found);
    a.lda(z(zp::HEADER + 5));
    a.sta(z(zp::DISK_ID));
    a.lda(z(zp::HEADER + 4));
    a.sta(z(zp::DISK_ID + 1));
    a.lda(imm(job::OK));
    a.rts();
}

/// `write_job`: the buffer encoded and its header found; at the old data
/// block's SYNC the head starts writing at once, continuing it with its own
/// before the block, then a gap; the buffer is decoded back. The SYNC is
/// the mark rather than a count of the gap's bytes, which the header's
/// comparison lets pass uncounted: a block rewritten starts where the last
/// did, a byte or so after the original.
fn writing_job(a: &mut Asm) {
    a.here("write_job");
    let (writable, sync, from_buffer, from_overflow, last, restore) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.lda(abs(MECHANISM));
    a.and(imm(mechanism::WRITABLE));
    a.bne(writable);
    a.lda(imm(job::WRITE_PROTECT));
    a.rts();
    a.bind(writable);
    a.jsr("encode_block");
    a.jsr("find_header");
    a.bcs(restore);
    a.jsr("wait_sync");
    a.bcs(restore);
    a.lda(imm(0xFF));
    a.sta(abs(via2(reg::DDRA)));
    a.sta(abs(DATA));
    a.lda(imm(WRITING));
    a.sta(abs(via2(reg::PCR)));
    a.ldx(imm(SYNC_BYTES));
    a.bind(sync);
    a.bvc(sync);
    a.clv();
    a.dex();
    a.bne(sync);
    a.ldy(imm(0));
    a.bind(from_buffer);
    a.lda(z(zp::BUFFER).ind_y());
    let wait_buffer = a.here_local();
    a.bvc(wait_buffer);
    a.clv();
    a.sta(abs(DATA));
    a.iny();
    a.bne(from_buffer);
    a.bind(from_overflow);
    a.lda(abs(OVERFLOW).y());
    let wait_overflow = a.here_local();
    a.bvc(wait_overflow);
    a.clv();
    a.sta(abs(DATA));
    a.iny();
    a.cpy(imm(OVERFLOW_BYTES));
    a.bne(from_overflow);
    // Gap after the block, so its last byte is wholly on the disk before
    // the head goes back to reading.
    a.ldx(imm(TRAILING_GAP_BYTES));
    a.bind(last);
    a.lda(imm(GAP));
    let wait_gap = a.here_local();
    a.bvc(wait_gap);
    a.clv();
    a.sta(abs(DATA));
    a.dex();
    a.bne(last);
    let drained = a.here_local();
    a.bvc(drained);
    a.clv();
    a.lda(imm(READING));
    a.sta(abs(via2(reg::PCR)));
    a.lda(imm(0));
    a.sta(abs(via2(reg::DDRA)));
    a.lda(imm(job::OK));
    a.bind(restore);
    a.pha();
    a.jsr("decode_block");
    a.pla();
    a.rts();
    verifying_job(a, restore);
}

/// `verify_job`: the block on the disk compared with the buffer encoded,
/// leaving through the write's `restore`. `compare_block`: the data block
/// after the SYNC just seen compared with the buffer's GCR and the
/// overflow's, A and `RESULT` OK or VERIFY.
fn verifying_job(a: &mut Asm, restore: Label) {
    a.here("verify_job");
    a.jsr("encode_block");
    a.jsr("find_header");
    a.bcs(restore);
    a.jsr("wait_sync");
    a.bcs(restore);
    a.jsr("compare_block");
    a.jmp(restore);

    a.here("compare_block");
    let (in_buffer, in_overflow, buffer_same, overflow_same) =
        (a.local(), a.local(), a.local(), a.local());
    a.lda(imm(job::OK));
    a.sta(z(zp::RESULT));
    a.ldy(imm(0));
    a.bind(in_buffer);
    a.bvc(in_buffer);
    a.clv();
    a.lda(abs(DATA));
    a.cmp(z(zp::BUFFER).ind_y());
    a.beq(buffer_same);
    a.ldx(imm(job::VERIFY));
    a.stx(z(zp::RESULT));
    a.bind(buffer_same);
    a.iny();
    a.bne(in_buffer);
    a.bind(in_overflow);
    a.bvc(in_overflow);
    a.clv();
    a.lda(abs(DATA));
    a.cmp(abs(OVERFLOW).y());
    a.beq(overflow_same);
    a.ldx(imm(job::VERIFY));
    a.stx(z(zp::RESULT));
    a.bind(overflow_same);
    a.iny();
    a.cpy(imm(OVERFLOW_BYTES));
    a.bne(in_overflow);
    a.lda(z(zp::RESULT));
    a.rts();
}
