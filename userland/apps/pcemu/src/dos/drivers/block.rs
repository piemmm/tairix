//! The built-in block driver: a unit per floppy drive — two on a machine with
//! one, which take turns at it as DOS's do — and per DOS partition of each
//! fixed disk: the first primary one, then every logical drive of its
//! extended partition. Every sector moves through INT 13h, a track's run at
//! a time, never across a 64 KiB boundary, each call tried again after a
//! reset. A drive with a change line tells DOS a reinserted volume from a
//! new one by the serial, label and BPB its boot sector declares.

use super::{local, unknown, Driver};
use crate::asm::{Reg16, Reg8};
use crate::bios::disk_status;
use crate::bios::layout::diskette;
use crate::cpu::flag;
use crate::dos::data::{driver, unit, MAX_UNITS, SEGMENT, UNIT, UNITS};
use crate::dos::device::{command, error, field};
use crate::dos::engine::Far;
use crate::dos::formats::{bpb_at, is_dos, partition, total_of, Diskette};
use crate::dos::rom;
use tairix_fat::boot::{at as boot_at, declared, DOS_BPB};
use tairix_fat::format::FIXED_DRIVE;
use tairix_partition::mbr;

/// Bytes of a sector on every medium the machine has.
const SECTOR: u16 = 512;
/// Bytes of DOS's BPB, as the driver addresses them.
#[allow(clippy::cast_possible_truncation, reason = "a BPB is 25 bytes")]
pub const BPB_LEN: u16 = DOS_BPB as u16;
/// Where the BPB lies in a boot sector.
const BPB_AT: usize = boot_at::BYTES_PER_SECTOR;
/// The label a volume that declares none goes by.
const NO_NAME: [u8; 12] = *b"NO NAME    \0";
/// Ticks within which a floppy without a change line counts as unchanged.
const SETTLED: u32 = 37;
/// Tries at a transfer call before its error stands.
const TRIES: u8 = 3;
/// EBRs a disk's chain may hold before it counts as looping.
const CHAIN: u8 = MAX_UNITS;

/// Phases.
const CALL: u8 = 1;
const VERIFY: u8 = 3;
const PROMPT: u8 = 4;
const PROMPT_KEY: u8 = 5;
const CHANGE: u8 = 6;
const BOOT_SECTOR: u8 = 7;
const FAT_SECTOR: u8 = 8;
const FLOPPY_TYPE: u8 = 9;
const FLOPPY_LINE: u8 = 10;
const FIXED_GEOMETRY: u8 = 11;
const FIXED_MBR: u8 = 12;
const FIXED_BOOT: u8 = 13;
const EBR: u8 = 14;
const LOGICAL_BOOT: u8 = 15;
const IDENTIFY: u8 = 16;

/// The transfer call machinery's phases, past every command's own.
const TRYING: u8 = 0x70;
const RETRYING: u8 = 0x71;
const STANDING: u8 = 0x72;

/// Scratch, as offsets from SS:SP.
const SECTOR_AT: u16 = local::SCRATCH;
const INDEX: u16 = local::SCRATCH + 4;
const ENTRY: u16 = local::SCRATCH + 5;
const HOPS: u16 = local::SCRATCH + 6;
const BOUNCED: u16 = local::SCRATCH + 7;
const BASE: u16 = local::SCRATCH + 8;
/// The transfer call in flight: its AX, made again after a reset; the
/// phase the request resumes in; what a changed medium does to it; and the
/// status a failure stands with, kept across its reset.
const CALL_AX: u16 = local::SCRATCH + 12;
const CALL_PHASE: u16 = local::SCRATCH + 14;
const CALL_CHANGE: u16 = local::SCRATCH + 15;
const CALL_STATUS: u16 = local::SCRATCH + 16;
const _: () = assert!(CALL_STATUS < rom::DRIVER_LOCALS as u16);
/// A generic IOCTL request's: the sector or track it has reached, and the
/// INT 1Eh vector a format replaced, clear of a swap's and a call's.
pub(super) const PROGRESS: u16 = local::SCRATCH;
pub(super) const REPLACED_TABLE: u16 = local::SCRATCH + 8;

/// Where INT 1Eh's vector lies.
pub(super) const DISKETTE_PARAMETERS: u16 = diskette::VECTOR as u16 * 4;

/// What a transfer call does when the medium has changed under it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum OnChange {
    /// It is made again: the call learns, identifies or rewrites the medium,
    /// so the new one is what it is for.
    Retry,
    /// Its error stands: the call moves data of the volume DOS holds, which
    /// another medium must never take in its place.
    Stand,
}

/// A unit, as the driver reads it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Unit {
    /// Its BIOS drive.
    pub drive: u8,
    /// The flags [`unit`](mod@unit) names.
    pub flags: u8,
    /// Heads the drive is addressed by.
    pub heads: u8,
    /// Sectors a track.
    pub sectors: u8,
    /// Where the volume starts on the drive.
    pub start: u32,
    /// The volume's sectors.
    pub total: u32,
}

pub(super) const fn unit_at(n: u8) -> u16 {
    UNITS + n as u16 * UNIT
}

/// Where unit `n`'s BPB lies in the kernel's segment.
#[must_use]
pub const fn unit_bpb(n: u8) -> u16 {
    unit_at(n) + unit::BPB
}

/// The units the driver found.
pub fn units_of(g: &mut crate::bios::guest::Guest<'_>) -> u8 {
    g.read8(SEGMENT, driver::UNIT_COUNT).min(MAX_UNITS)
}

impl Driver<'_, '_> {
    pub(super) fn unit(&mut self, n: u8) -> Unit {
        let at = unit_at(n);
        let g = &mut *self.g;
        let word32 = |g: &mut crate::bios::guest::Guest<'_>, field: u16| {
            u32::from(g.read16(SEGMENT, at + field))
                | u32::from(g.read16(SEGMENT, at + field + 2)) << 16
        };
        Unit {
            drive: g.read8(SEGMENT, at + unit::DRIVE),
            flags: g.read8(SEGMENT, at + unit::FLAGS),
            heads: g.read8(SEGMENT, at + unit::HEADS).max(1),
            sectors: g.read8(SEGMENT, at + unit::SECTORS).max(1),
            start: word32(g, unit::START),
            total: word32(g, unit::TOTAL),
        }
    }

    fn set_unit(&mut self, n: u8, u: &Unit) {
        let at = unit_at(n);
        self.g.write8(SEGMENT, at + unit::DRIVE, u.drive);
        self.g.write8(SEGMENT, at + unit::FLAGS, u.flags);
        self.g.write8(SEGMENT, at + unit::HEADS, u.heads);
        self.g.write8(SEGMENT, at + unit::SECTORS, u.sectors);
        self.g
            .write_bytes(SEGMENT, at + unit::START, &u.start.to_le_bytes());
        self.g
            .write_bytes(SEGMENT, at + unit::TOTAL, &u.total.to_le_bytes());
    }

    fn unit_count(&mut self) -> u8 {
        units_of(self.g)
    }

    /// The request's unit, if the driver has it.
    pub(super) fn request_unit(&mut self) -> Option<u8> {
        let n = self.get8(field::UNIT);
        (n < self.unit_count()).then_some(n)
    }

    fn ticks(&mut self) -> u32 {
        let low = self.g.bda16(crate::bios::bda::TICKS);
        u32::from(low) | u32::from(self.g.bda16(crate::bios::bda::TICKS + 2)) << 16
    }

    pub(super) fn touch(&mut self, n: u8) {
        let now = self.ticks();
        self.g
            .write_bytes(SEGMENT, unit_at(n) + unit::TOUCHED, &now.to_le_bytes());
    }

    /// Sets CX and DH for `sector` of the track at `cylinder` and `head`, DL
    /// for the drive; and for a floppy the sectors a track the BIOS reads by,
    /// the medium's or a sector past them asked for, as a transfer of several
    /// reaching the track's end would otherwise go on to the other head's at
    /// the BIOS table's ninth.
    pub(super) fn address_sector(&mut self, u: &Unit, cylinder: u16, head: u8, sector: u8) {
        if u.flags & unit::FLOPPY != 0 {
            self.g.write8(
                SEGMENT,
                driver::DISKETTE_TABLE + diskette::LAST_SECTOR,
                u.sectors.max(sector),
            );
        }
        let [cyl_low, cyl_high] = cylinder.to_le_bytes();
        self.g.set_r16(
            Reg16::Cx,
            u16::from_le_bytes([sector | (cyl_high & 3) << 6, cyl_low]),
        );
        self.g
            .set_r16(Reg16::Dx, u16::from_le_bytes([u.drive, head]));
    }

    /// The same for the drive's sector `lba`.
    pub(super) fn address(&mut self, u: &Unit, lba: u32) {
        let per_cylinder = u32::from(u.heads) * u32::from(u.sectors);
        let cylinder = u16::try_from(lba / per_cylinder).unwrap_or(u16::MAX);
        let head = u8::try_from(lba / u32::from(u.sectors) % u32::from(u.heads)).unwrap_or(0);
        let sector = u8::try_from(lba % u32::from(u.sectors) + 1).unwrap_or(0);
        self.address_sector(u, cylinder, head, sector);
    }

    /// Reads the drive's sector `lba` into the bounce buffer.
    pub(super) fn read_bounce(&mut self, u: &Unit, lba: u32, phase: u8) {
        self.address(u, lba);
        self.g.set_r16(Reg16::Ax, 0x0201);
        self.g.regs_mut().set_sreg(crate::cpu::Seg::Es, SEGMENT);
        self.g.set_r16(Reg16::Bx, driver::BOUNCE);
        self.disk_call(phase, OnChange::Retry);
    }

    /// Makes INT 13h transfer call AX — a read, write, verify or format, its
    /// other registers set — and resumes the request in `phase` once it
    /// succeeds or its failure stands. Only AX is kept to make it again:
    /// INT 13h's transfers and its reset leave every other register as
    /// they found it.
    pub(super) fn disk_call(&mut self, phase: u8, on_change: OnChange) {
        let ax = self.g.r16(Reg16::Ax);
        self.set_local16(CALL_AX, ax);
        self.set_local8(CALL_PHASE, phase);
        self.set_local8(CALL_CHANGE, u8::from(on_change == OnChange::Stand));
        self.set_local8(local::TRIES, TRIES);
        self.set_phase(TRYING);
        self.bios(0x13);
    }

    pub(super) fn bounce(&mut self) -> [u8; 512] {
        let mut sector = [0u8; 512];
        self.g.read_bytes(SEGMENT, driver::BOUNCE, &mut sector);
        sector
    }

    pub(super) fn failed(&mut self) -> bool {
        self.g.regs().flag(flag::CF)
    }
}

/// A request to the block driver.
pub fn serve(d: &mut Driver<'_, '_>) {
    match d.phase() {
        TRYING => return attempted(d),
        RETRYING => return retried(d),
        STANDING => return stood(d),
        _ => {}
    }
    match d.get8(field::COMMAND) {
        command::INIT => init(d),
        command::MEDIA_CHECK => media_check(d),
        command::BUILD_BPB => build_bpb(d),
        command::INPUT => transfer(d, 0x02, false),
        command::OUTPUT => transfer(d, 0x03, false),
        command::OUTPUT_VERIFY => transfer(d, 0x03, true),
        command::OPEN | command::CLOSE => d.finish(None),
        command::REMOVABLE => match d.request_unit() {
            Some(n) => {
                let fixed = d.unit(n).flags & unit::FLOPPY == 0;
                d.finish_busy(fixed);
            }
            None => d.finish(Some(error::UNKNOWN_UNIT)),
        },
        command::GET_LOGICAL => get_logical(d),
        command::SET_LOGICAL => set_logical(d),
        command::GENERIC_IOCTL => super::block_ioctl::serve(d),
        _ => unknown(d),
    }
}

/// The driver error a BIOS status stands for.
pub(super) const fn driver_error(status: u8) -> u8 {
    match status {
        disk_status::INVALID => error::UNKNOWN_COMMAND,
        disk_status::WRITE_PROTECTED => error::WRITE_PROTECT,
        disk_status::NOT_FOUND => error::NOT_FOUND,
        disk_status::CHANGED => error::DISK_CHANGE,
        disk_status::CRC => error::CRC,
        disk_status::SEEK => error::SEEK,
        disk_status::TIMEOUT | disk_status::NOT_READY => error::NOT_READY,
        disk_status::WRITE_FAULT => error::WRITE_FAULT,
        _ => error::GENERAL,
    }
}

/// A transfer call returned. A success resumes the request; a failure is
/// followed by a reset, as INT 13h asks — a call that timed out can leave
/// the controller mid-command — and the call is made again while tries
/// remain, unless no try could mend it: a write-protected medium, an empty
/// drive whose every try would wait out the timeout, or a change the call
/// must not carry on across.
fn attempted(d: &mut Driver<'_, '_>) {
    if !d.failed() {
        rest_table(d);
        let phase = d.local8(CALL_PHASE);
        d.set_phase(phase);
        return serve(d);
    }
    let status = d.g.r8(Reg8::Ah);
    let tries = d.local8(local::TRIES).saturating_sub(1);
    d.set_local8(local::TRIES, tries);
    let lasting = match status {
        disk_status::WRITE_PROTECTED | disk_status::TIMEOUT => true,
        disk_status::CHANGED => d.local8(CALL_CHANGE) != 0,
        _ => false,
    };
    d.set_local8(CALL_STATUS, status);
    d.g.set_r16(Reg16::Ax, 0x0000);
    d.set_phase(if tries == 0 || lasting {
        STANDING
    } else {
        RETRYING
    });
    d.bios(0x13);
}

/// The drive is reset: the call is made again.
fn retried(d: &mut Driver<'_, '_>) {
    let ax = d.local16(CALL_AX);
    d.g.set_r16(Reg16::Ax, ax);
    d.set_phase(TRYING);
    d.bios(0x13);
}

/// The drive is reset after a failure that stands: the request resumes
/// with it, as if the call had just returned it.
fn stood(d: &mut Driver<'_, '_>) {
    rest_table(d);
    let status = d.local8(CALL_STATUS);
    d.g.set_r8(Reg8::Ah, status);
    d.g.regs_mut().set_flag(flag::CF, true);
    let phase = d.local8(CALL_PHASE);
    d.set_phase(phase);
    serve(d);
}

/// INT 13h as DOS hooks it: a track's format marks every unit on its drive
/// as maybe changed, whoever asks for it, as DOS's driver does; then the
/// BIOS's handler takes the call.
pub fn int13(g: &mut crate::bios::guest::Guest<'_>) {
    if g.r8(Reg8::Ah) == 0x05 {
        let drive = g.r8(Reg8::Dl);
        for n in 0..units_of(g) {
            let at = unit_at(n);
            if g.read8(SEGMENT, at + unit::DRIVE) == drive {
                let flags = g.read8(SEGMENT, at + unit::FLAGS);
                g.write8(
                    SEGMENT,
                    at + unit::FLAGS,
                    flags | unit::FORMATTED | unit::CHANGED,
                );
            }
        }
    }
    let offset = g.read16(SEGMENT, driver::INT13);
    let segment = g.read16(SEGMENT, driver::INT13 + 2);
    g.jump(segment, offset);
}

/// Gives the driver's diskette parameters back the sectors a track they
/// were copied with, which programs calling INT 13h themselves expect.
fn rest_table(d: &mut Driver<'_, '_>) {
    let rest = d.g.read8(SEGMENT, driver::DISKETTE_EOT);
    d.g.write8(
        SEGMENT,
        driver::DISKETTE_TABLE + diskette::LAST_SECTOR,
        rest,
    );
}

/// The units sharing unit `n`'s floppy drive, if any other does.
fn shares_drive(d: &mut Driver<'_, '_>, n: u8) -> bool {
    let u = d.unit(n);
    u.flags & unit::FLOPPY != 0
        && (0..d.unit_count()).any(|m| {
            m != n && {
                let other = d.unit(m);
                other.flags & unit::FLOPPY != 0 && other.drive == u.drive
            }
        })
}

/// Whether unit `n` must be swapped in before its drive is used: another
/// unit used the drive last.
pub(super) fn must_swap(d: &mut Driver<'_, '_>, n: u8) -> bool {
    if !shares_drive(d, n) {
        return false;
    }
    let drive = d.unit(n).drive;
    d.g.read8(SEGMENT, driver::OWNERS + u16::from(drive & 3)) != n
}

/// What a drive two units share asks for when the other unit's turn comes.
const SWAP_TEXT: &[u8] = b"\r\nInsert diskette for drive ?: and press any key when ready\r\n\r\n";
/// Where the drive's letter goes in it.
const SWAP_LETTER: usize = 28;

/// Asks for the disk of unit `n`, a character a BIOS call, then waits for a
/// key; `true` once the unit has the drive.
pub(super) fn swap(d: &mut Driver<'_, '_>, n: u8) -> bool {
    match d.phase() {
        PROMPT => {
            let index = usize::from(d.local8(INDEX));
            if let Some(&byte) = SWAP_TEXT.get(index) {
                let byte = if index == SWAP_LETTER { b'A' + n } else { byte };
                let next = d.local8(INDEX) + 1;
                d.set_local8(INDEX, next);
                d.g.set_r16(Reg16::Ax, u16::from_le_bytes([byte, 0x0E]));
                d.g.set_r16(Reg16::Bx, 0x0007);
                d.bios(0x10);
                return false;
            }
            d.g.set_r8(Reg8::Ah, 0x00);
            d.set_phase(PROMPT_KEY);
            d.bios(0x16);
            false
        }
        PROMPT_KEY => {
            let drive = d.unit(n).drive;
            d.g.write8(SEGMENT, driver::OWNERS + u16::from(drive & 3), n);
            d.set_phase(0);
            true
        }
        _ => {
            d.set_local8(INDEX, 0);
            d.set_phase(PROMPT);
            swap(d, n)
        }
    }
}

fn media_check(d: &mut Driver<'_, '_>) {
    let Some(n) = d.request_unit() else {
        return d.finish(Some(error::UNKNOWN_UNIT));
    };
    let u = d.unit(n);
    if matches!(d.phase(), 0 | PROMPT | PROMPT_KEY) && must_swap(d, n) {
        if swap(d, n) {
            answer_media(d, n, -1);
        }
        return;
    }
    let formatted = d.phase() == 0 && u.flags & unit::FORMATTED != 0;
    if formatted {
        d.g.write8(
            SEGMENT,
            unit_at(n) + unit::FLAGS,
            u.flags & !unit::FORMATTED,
        );
    }
    if u.flags & unit::FLOPPY == 0 {
        return answer_media(d, n, if formatted { -1 } else { 1 });
    }
    // A medium being formatted may hold nothing readable, and one formatted
    // without a change line may have been swapped at any time.
    let unknowable = u.flags & (unit::FAKE_BPB | unit::CHANGE_LINE) != unit::CHANGE_LINE;
    if formatted && unknowable {
        return answer_media(d, n, 0);
    }
    if u.flags & unit::CHANGE_LINE == 0 {
        let mut touched = [0u8; 4];
        d.g.read_bytes(SEGMENT, unit_at(n) + unit::TOUCHED, &mut touched);
        let since = d.ticks().wrapping_sub(u32::from_le_bytes(touched));
        return answer_media(d, n, i8::from(since < SETTLED));
    }
    match d.phase() {
        // The line answers in the carry; a drive that cannot say is asked
        // what it holds, as a changed one is.
        CHANGE if !d.failed() && u.flags & unit::CHANGED == 0 => answer_media(d, n, 1),
        CHANGE => d.read_bounce(&u, u.start, IDENTIFY),
        IDENTIFY if d.failed() => {
            let status = d.g.r8(Reg8::Ah);
            d.finish(Some(driver_error(status)));
        }
        IDENTIFY => {
            let same = holds_volume(d, n);
            answer_media(d, n, if same { 1 } else { -1 });
        }
        _ => {
            d.g.set_r16(Reg16::Ax, 0x1600);
            d.g.set_r16(Reg16::Dx, u16::from(u.drive));
            d.set_phase(CHANGE);
            d.bios(0x13);
        }
    }
}

/// Whether the boot sector in the bounce buffer is of the volume unit `n`
/// last learned, which the unit's mark of a pending change then follows. A
/// volume that declares no serial cannot be told from another of its
/// format, so it is never the same one.
fn holds_volume(d: &mut Driver<'_, '_>, n: u8) -> bool {
    let boot = d.bounce();
    let at = unit_at(n);
    let flags = d.g.read8(SEGMENT, at + unit::FLAGS);
    let mut bpb = [0u8; DOS_BPB];
    d.g.read_bytes(SEGMENT, at + unit::BPB, &mut bpb);
    let mut serial = [0u8; 4];
    d.g.read_bytes(SEGMENT, at + unit::SERIAL, &mut serial);
    let mut label = NO_NAME;
    d.g.read_bytes(SEGMENT, at + unit::LABEL, &mut label);
    let (read_serial, read_label) = declared(&boot, boot_at::EXTENDED);
    let same = flags & unit::IDENTIFIED != 0
        && read_serial == Some(u32::from_le_bytes(serial))
        && label_of(read_label) == label
        && boot[BPB_AT..BPB_AT + DOS_BPB] == bpb;
    let flags = if same {
        flags & !unit::CHANGED
    } else {
        flags | unit::CHANGED
    };
    d.g.write8(SEGMENT, at + unit::FLAGS, flags);
    same
}

/// Answers a media check: `-1` changed, `0` unknown, `1` unchanged; a change
/// hands back the label the old medium had.
fn answer_media(d: &mut Driver<'_, '_>, n: u8, answer: i8) {
    d.set8(field::ADDRESS, answer.to_le_bytes()[0]);
    if answer < 0 {
        let label = Far::new(SEGMENT, unit_at(n) + unit::LABEL);
        d.set_far(field::ADDRESS + 1, label);
    }
    d.finish(None);
}

/// The BPB a boot sector records, if it is one of a FAT12 or FAT16 volume
/// DOS can use.
fn boot_bpb(sector: &[u8; 512]) -> Option<[u8; DOS_BPB]> {
    let bpb = tairix_fat::boot::Bpb::parse(sector)?;
    if bpb.kind == tairix_fat::boot::Kind::Fat32 || bpb.bytes_per_sector != SECTOR {
        return None;
    }
    let mut out = [0u8; DOS_BPB];
    out.copy_from_slice(&sector[BPB_AT..BPB_AT + DOS_BPB]);
    Some(out)
}

/// Records `bpb` as unit `n`'s, and what it says of the medium's geometry
/// and size, with the identity the boot sector `boot` declares: the medium
/// DOS is told of, so no change is pending.
pub(super) fn learn(d: &mut Driver<'_, '_>, n: u8, bpb: &[u8; DOS_BPB], boot: Option<&[u8; 512]>) {
    d.g.write_bytes(SEGMENT, unit_at(n) + unit::BPB, bpb);
    let mut u = d.unit(n);
    if u.flags & unit::FLOPPY != 0 {
        u.sectors = bpb[bpb_at::SECTORS].max(1);
        u.heads = bpb[bpb_at::HEADS].max(1);
        u.total = total_of(bpb);
    }
    u.flags &= !(unit::CHANGED | unit::UNFORMATTED);
    d.set_unit(n, &u);
    identify(d, n, boot);
}

/// Records the serial and label boot sector `boot` declares as unit `n`'s
/// volume's.
pub(super) fn identify(d: &mut Driver<'_, '_>, n: u8, boot: Option<&[u8; 512]>) {
    let at = unit_at(n);
    let (serial, label) = boot.map_or((None, None), |boot| declared(boot, boot_at::EXTENDED));
    let flags = d.g.read8(SEGMENT, at + unit::FLAGS) & !unit::IDENTIFIED;
    let flags = if serial.is_some() {
        flags | unit::IDENTIFIED
    } else {
        flags
    };
    d.g.write8(SEGMENT, at + unit::FLAGS, flags);
    d.g.write_bytes(
        SEGMENT,
        at + unit::SERIAL,
        &serial.unwrap_or(0).to_le_bytes(),
    );
    d.g.write_bytes(SEGMENT, at + unit::LABEL, &label_of(label));
}

/// The label a unit keeps for a volume, ASCIZ: the one it declares, else
/// `NO NAME`.
fn label_of(declared: Option<[u8; 11]>) -> [u8; 12] {
    let mut label = NO_NAME;
    if let Some(declared) = declared {
        label[..11].copy_from_slice(&declared);
    }
    label
}

fn build_bpb(d: &mut Driver<'_, '_>) {
    let Some(n) = d.request_unit() else {
        return d.finish(Some(error::UNKNOWN_UNIT));
    };
    match read_medium(d, n) {
        None => {}
        Some(Ok(())) => {
            d.set_far(field::COUNT, Far::new(SEGMENT, unit_bpb(n)));
            d.finish(None);
        }
        Some(Err(code)) => d.finish(Some(code)),
    }
}

/// Learns the BPB of the medium in unit `n`'s drive: its boot sector's, or
/// a DOS 1 diskette's by the media byte its FAT begins with; `None` while
/// the reads are under way. A BPB set for formatting stands, read from
/// nothing, as a program laying a new format has the drive's only word on
/// what the medium holds.
pub(super) fn read_medium(d: &mut Driver<'_, '_>, n: u8) -> Option<Result<(), u8>> {
    let u = d.unit(n);
    match d.phase() {
        BOOT_SECTOR | FAT_SECTOR if d.failed() => Some(Err(driver_error(d.g.r8(Reg8::Ah)))),
        BOOT_SECTOR => {
            let boot = d.bounce();
            if let Some(bpb) = boot_bpb(&boot) {
                learn(d, n, &bpb, Some(&boot));
                return Some(Ok(()));
            }
            if u.flags & unit::FLOPPY == 0 {
                return Some(Err(error::UNKNOWN_MEDIA));
            }
            d.read_bounce(&u, u.start + 1, FAT_SECTOR);
            None
        }
        FAT_SECTOR => {
            let fat = d.bounce();
            let Some(diskette) = Diskette::of_media(fat[0]) else {
                return Some(Err(error::UNKNOWN_MEDIA));
            };
            learn(d, n, &diskette.bpb(), None);
            Some(Ok(()))
        }
        _ if u.flags & unit::FAKE_BPB != 0 => {
            d.g.write8(SEGMENT, unit_at(n) + unit::FLAGS, u.flags & !unit::CHANGED);
            d.touch(n);
            Some(Ok(()))
        }
        _ => {
            if must_swap(d, n) && !swap(d, n) {
                return None;
            }
            d.touch(n);
            d.read_bounce(&u, u.start, BOOT_SECTOR);
            None
        }
    }
}

/// Reads or writes the request's sectors: INT 13h function `function`,
/// each written run verified when `verify`.
fn transfer(d: &mut Driver<'_, '_>, function: u8, verify: bool) {
    let Some(n) = d.request_unit() else {
        return d.finish(Some(error::UNKNOWN_UNIT));
    };
    let u = d.unit(n);
    let count = d.get16(field::COUNT);
    let first = match d.get16(field::SECTOR) {
        0xFFFF => {
            let low = d.get16(field::SECTOR32);
            u32::from(low) | u32::from(d.get16(field::SECTOR32 + 2)) << 16
        }
        sector => u32::from(sector),
    };
    let buffer = d.get_far(field::ADDRESS);
    match d.phase() {
        0 | PROMPT | PROMPT_KEY => {
            if must_swap(d, n) && !swap(d, n) {
                return;
            }
            if u64::from(first) + u64::from(count) > u64::from(u.total) {
                d.set16(field::COUNT, 0);
                return d.finish(Some(error::NOT_FOUND));
            }
            d.set_local16(local::DONE, 0);
            if u.flags & unit::CHANGED != 0 {
                return d.read_bounce(&u, u.start, IDENTIFY);
            }
        }
        IDENTIFY => {
            let refused = if d.failed() {
                Some(driver_error(d.g.r8(Reg8::Ah)))
            } else {
                (!holds_volume(d, n)).then_some(error::DISK_CHANGE)
            };
            if let Some(code) = refused {
                d.set16(field::COUNT, 0);
                return d.finish(Some(code));
            }
        }
        CALL | VERIFY if d.failed() => return chunk_failed(d, n),
        CALL => {
            if !chunk_moved(d, &u, function, verify, buffer) {
                return;
            }
        }
        VERIFY => {
            let done = d.local16(local::DONE) + d.local16(local::CHUNK);
            d.set_local16(local::DONE, done);
        }
        _ => {}
    }
    let done = d.local16(local::DONE);
    if done >= count {
        d.touch(n);
        return d.finish(None);
    }
    next_chunk(
        d,
        &u,
        function,
        u.start + first + u32::from(done),
        count - done,
        buffer,
    );
}

/// A chunk's call failed for good: the count says what moved, and a change
/// of medium it met is kept for DOS's next media check, the line that told
/// of it being clear again. A verify's failure is the write's, unless the
/// medium went or changed.
fn chunk_failed(d: &mut Driver<'_, '_>, n: u8) {
    let status = d.g.r8(Reg8::Ah);
    let done = d.local16(local::DONE);
    d.set16(field::COUNT, done);
    if status == disk_status::CHANGED {
        let at = unit_at(n) + unit::FLAGS;
        let flags = d.g.read8(SEGMENT, at);
        d.g.write8(SEGMENT, at, flags | unit::CHANGED);
    }
    let code = match driver_error(status) {
        code @ (error::NOT_READY | error::DISK_CHANGE) => code,
        _ if d.phase() == VERIFY => error::WRITE_FAULT,
        code => code,
    };
    d.finish(Some(code));
}

/// A chunk moved: a bounced read copied out, a verify begun, or the count
/// moved on; `false` while a verify is under way.
fn chunk_moved(d: &mut Driver<'_, '_>, u: &Unit, function: u8, verify: bool, buffer: Far) -> bool {
    let chunk = d.local16(local::CHUNK);
    if function == 0x02 && d.local8(BOUNCED) != 0 {
        let done = d.local16(local::DONE);
        let to = Far::new(
            buffer.segment,
            buffer.offset.wrapping_add(done.wrapping_mul(SECTOR)),
        );
        let sector = d.bounce();
        d.g.write_bytes(to.segment, to.offset, &sector);
    }
    if verify {
        let at = d.local32(SECTOR_AT);
        d.address(u, at);
        d.g.set_r16(
            Reg16::Ax,
            u16::from_le_bytes([chunk.to_le_bytes()[0], 0x04]),
        );
        d.disk_call(VERIFY, OnChange::Stand);
        return false;
    }
    let done = d.local16(local::DONE) + chunk;
    d.set_local16(local::DONE, done);
    true
}

/// Starts the next BIOS call from `lba`, `left` sectors still to move: as
/// many as the track and the DMA page allow, a sector through the bounce
/// buffer where the page has no room for one.
fn next_chunk(d: &mut Driver<'_, '_>, u: &Unit, function: u8, lba: u32, left: u16, buffer: Far) {
    let done = d.local16(local::DONE);
    let in_track = u32::from(u.sectors) - lba % u32::from(u.sectors);
    let linear = buffer.linear() + u32::from(done) * u32::from(SECTOR);
    let room = (0x1_0000 - (linear & 0xFFFF)) / u32::from(SECTOR);
    let bounced = room == 0;
    let chunk = if bounced {
        1
    } else {
        u16::try_from(in_track.min(u32::from(left)).min(room)).unwrap_or(1)
    };
    d.set_local16(local::CHUNK, chunk);
    d.set_local32(SECTOR_AT, lba);
    d.set_local8(BOUNCED, u8::from(bounced));
    d.address(u, lba);
    if bounced {
        if function == 0x03 {
            let mut sector = [0u8; 512];
            let from = Far::new(
                buffer.segment,
                buffer.offset.wrapping_add(done.wrapping_mul(SECTOR)),
            );
            d.g.read_bytes(from.segment, from.offset, &mut sector);
            d.g.write_bytes(SEGMENT, driver::BOUNCE, &sector);
        }
        d.g.regs_mut().set_sreg(crate::cpu::Seg::Es, SEGMENT);
        d.g.set_r16(Reg16::Bx, driver::BOUNCE);
    } else {
        let [low, ..] = linear.to_le_bytes();
        d.g.regs_mut()
            .set_sreg(crate::cpu::Seg::Es, u16::try_from(linear >> 4).unwrap_or(0));
        d.g.set_r16(Reg16::Bx, u16::from(low & 0x0F));
    }
    let [count_byte, ..] = chunk.to_le_bytes();
    d.g.set_r16(Reg16::Ax, u16::from_le_bytes([count_byte, function]));
    d.disk_call(CALL, OnChange::Stand);
}

fn get_logical(d: &mut Driver<'_, '_>) {
    let Some(n) = d.request_unit() else {
        return d.finish(Some(error::UNKNOWN_UNIT));
    };
    let answer = if shares_drive(d, n) {
        let drive = d.unit(n).drive;
        d.g.read8(SEGMENT, driver::OWNERS + u16::from(drive & 3)) + 1
    } else {
        0
    };
    d.set8(field::UNIT, answer);
    d.finish(None);
}

fn set_logical(d: &mut Driver<'_, '_>) {
    let Some(n) = d.request_unit() else {
        return d.finish(Some(error::UNKNOWN_UNIT));
    };
    if shares_drive(d, n) {
        let drive = d.unit(n).drive;
        d.g.write8(SEGMENT, driver::OWNERS + u16::from(drive & 3), n);
        d.set8(field::UNIT, n + 1);
    } else {
        d.set8(field::UNIT, 0);
    }
    d.finish(None);
}

/// Adds a unit, if the driver has room; its number.
fn add_unit(d: &mut Driver<'_, '_>, u: &Unit) -> Option<u8> {
    let n = d.unit_count();
    if n >= MAX_UNITS {
        return None;
    }
    d.g.write8(SEGMENT, driver::UNIT_COUNT, n + 1);
    d.set_unit(n, u);
    let at = unit_at(n);
    d.g.write_bytes(SEGMENT, at + unit::TOUCHED, &[0; 4]);
    Some(n)
}

/// The partition table's entry `entry`, as [`driver::SCAN`] keeps it.
fn scanned(d: &mut Driver<'_, '_>, entry: u8) -> (u8, u32, u32) {
    let at = driver::SCAN + u16::from(entry) * driver::SCAN_ENTRY;
    let mut raw = [0u8; 9];
    d.g.read_bytes(SEGMENT, at, &mut raw);
    (
        raw[0],
        u32::from_le_bytes([raw[1], raw[2], raw[3], raw[4]]),
        u32::from_le_bytes([raw[5], raw[6], raw[7], raw[8]]),
    )
}

/// The fixed disk `disk`'s geometry, as the partition scan keeps it.
fn disk_geometry(d: &mut Driver<'_, '_>, disk: u8) -> (u8, u8) {
    let at = driver::EXTENDED + u16::from(disk & 1) * 6 + 4;
    (
        d.g.read8(SEGMENT, at).max(1),
        d.g.read8(SEGMENT, at + 1).max(1),
    )
}

fn extended_of(d: &mut Driver<'_, '_>, disk: u8) -> u32 {
    let at = driver::EXTENDED + u16::from(disk & 1) * 6;
    let mut raw = [0u8; 4];
    d.g.read_bytes(SEGMENT, at, &mut raw);
    u32::from_le_bytes(raw)
}

/// `INIT`: finds the units, then hands DOS their count and BPBs.
fn init(d: &mut Driver<'_, '_>) {
    let floppies = {
        let equipment = d.g.bda16(crate::bios::bda::EQUIPMENT);
        if equipment & 1 != 0 {
            u8::try_from((equipment >> 6 & 3) + 1).unwrap_or(1)
        } else {
            0
        }
    };
    let disks = d.g.bda8(crate::bios::bda::FIXED_COUNT).min(2);
    match d.phase() {
        0 => {
            install_table(d);
            d.g.write8(SEGMENT, driver::UNIT_COUNT, 0);
            d.g.write_bytes(SEGMENT, driver::OWNERS, &[0; 4]);
            d.g.write_bytes(SEGMENT, driver::EXTENDED, &[0; 12]);
            d.set_local8(INDEX, 0);
            if floppies > 0 {
                return floppy_type(d, 0);
            }
            fixed_next(d, 0, disks);
        }
        FLOPPY_TYPE => floppy_typed(d),
        FLOPPY_LINE => floppy_lined(d, floppies, disks),
        FIXED_GEOMETRY => fixed_geometry(d, disks),
        FIXED_MBR => fixed_mbr(d, disks),
        FIXED_BOOT | LOGICAL_BOOT => {
            let disk = d.local8(INDEX);
            let logical = d.phase() == LOGICAL_BOOT;
            if !d.failed() {
                let entry = if logical { 0 } else { d.local8(ENTRY) };
                let (_, start, total) = scanned(d, entry);
                add_partition(d, disk, start, total);
            }
            if logical {
                follow_chain(d, disk, disks);
            } else {
                fixed_next(d, disk + 1, disks);
            }
        }
        EBR => extended_record(d, disks),
        _ => d.finish(Some(error::GENERAL)),
    }
}

/// Copies the diskette parameters INT 1Eh points at into the driver's own,
/// where each transfer can give them its medium's sectors a track, and
/// points INT 1Eh there, as DOS's driver does.
fn install_table(d: &mut Driver<'_, '_>) {
    let offset = d.g.read16(0, DISKETTE_PARAMETERS);
    let segment = d.g.read16(0, DISKETTE_PARAMETERS + 2);
    let mut table = [0u8; diskette::LEN as usize];
    d.g.read_bytes(segment, offset, &mut table);
    d.g.write_bytes(SEGMENT, driver::DISKETTE_TABLE, &table);
    d.g.write8(
        SEGMENT,
        driver::DISKETTE_EOT,
        table[usize::from(diskette::LAST_SECTOR)],
    );
    d.g.write16(0, DISKETTE_PARAMETERS, driver::DISKETTE_TABLE);
    d.g.write16(0, DISKETTE_PARAMETERS + 2, SEGMENT);
}

/// A floppy drive's type is known: its unit, with the BPB of its type's
/// standard medium until a diskette is read, then a look at its change line.
fn floppy_typed(d: &mut Driver<'_, '_>) {
    let drive = d.local8(INDEX);
    let kind = if d.failed() { 1 } else { d.g.r8(Reg8::Bl) };
    let bpb = Diskette::of_drive(kind).bpb();
    let u = Unit {
        drive,
        flags: unit::FLOPPY,
        heads: bpb[bpb_at::HEADS],
        sectors: bpb[bpb_at::SECTORS],
        start: 0,
        total: total_of(&bpb),
    };
    if let Some(n) = add_unit(d, &u) {
        d.g.write_bytes(SEGMENT, unit_at(n) + unit::BPB, &bpb);
        d.g.write_bytes(SEGMENT, unit_at(n) + unit::RECOMMENDED, &bpb);
        d.g.write_bytes(SEGMENT, unit_at(n) + unit::LABEL, &NO_NAME);
        d.g.write8(SEGMENT, unit_at(n) + unit::KIND, kind);
    }
    d.g.set_r16(Reg16::Ax, 0x1500);
    d.g.set_r16(Reg16::Dx, u16::from(drive));
    d.set_phase(FLOPPY_LINE);
    d.bios(0x13);
}

/// A floppy drive's change line is known; then the next drive, a lone
/// drive's second unit, or the fixed disks.
fn floppy_lined(d: &mut Driver<'_, '_>, floppies: u8, disks: u8) {
    let drive = d.local8(INDEX);
    if !d.failed() && d.g.r8(Reg8::Ah) == 0x02 {
        let n = d.unit_count().saturating_sub(1);
        let mut u = d.unit(n);
        u.flags |= unit::CHANGE_LINE;
        d.set_unit(n, &u);
    }
    if drive + 1 < floppies {
        d.set_local8(INDEX, drive + 1);
        return floppy_type(d, drive + 1);
    }
    if floppies == 1 {
        let first = d.unit(0);
        let kind = d.g.read8(SEGMENT, unit_at(0) + unit::KIND);
        let bpb = Diskette::of_drive(kind).bpb();
        if let Some(n) = add_unit(d, &first) {
            d.g.write_bytes(SEGMENT, unit_bpb(n), &bpb);
            d.g.write_bytes(SEGMENT, unit_at(n) + unit::RECOMMENDED, &bpb);
            d.g.write_bytes(SEGMENT, unit_at(n) + unit::LABEL, &NO_NAME);
            d.g.write8(SEGMENT, unit_at(n) + unit::KIND, kind);
        }
    }
    fixed_next(d, 0, disks);
}

/// A unit to read fixed disk `disk`'s tables through, before any partition
/// of it is one.
fn probe(d: &mut Driver<'_, '_>, disk: u8) -> Unit {
    let (heads, sectors) = disk_geometry(d, disk);
    Unit {
        drive: FIXED_DRIVE | disk,
        flags: 0,
        heads,
        sectors,
        start: 0,
        total: 1,
    }
}

/// A fixed disk's geometry is known: its master boot record is read.
fn fixed_geometry(d: &mut Driver<'_, '_>, disks: u8) {
    let disk = d.local8(INDEX);
    if d.failed() {
        return fixed_next(d, disk + 1, disks);
    }
    let heads = d.g.r8(Reg8::Dh).saturating_add(1);
    let sectors = d.g.r8(Reg8::Cl) & 0x3F;
    let at = driver::EXTENDED + u16::from(disk) * 6 + 4;
    d.g.write8(SEGMENT, at, heads);
    d.g.write8(SEGMENT, at + 1, sectors);
    let probe = probe(d, disk);
    d.read_bounce(&probe, 0, FIXED_MBR);
}

/// A fixed disk's master boot record is read: its entries kept, its
/// extended partition noted, and its first DOS partition's boot record read.
fn fixed_mbr(d: &mut Driver<'_, '_>, disks: u8) {
    let disk = d.local8(INDEX);
    if d.failed() {
        return fixed_next(d, disk + 1, disks);
    }
    let sector = d.bounce();
    let Ok(entries) = mbr::entries(&sector) else {
        return fixed_next(d, disk + 1, disks);
    };
    d.g.write_bytes(SEGMENT, driver::SCAN, &[0; 36]);
    for (slot, entry) in (0u16..).zip(entries.as_slice()) {
        let at = driver::SCAN + slot * driver::SCAN_ENTRY;
        d.g.write8(SEGMENT, at, entry.type_byte);
        d.g.write_bytes(SEGMENT, at + 1, &entry.start_lba.to_le_bytes());
        d.g.write_bytes(SEGMENT, at + 5, &entry.sectors.to_le_bytes());
        if mbr::is_extended(entry.type_byte) && extended_of(d, disk) == 0 {
            let ext = driver::EXTENDED + u16::from(disk) * 6;
            d.g.write_bytes(SEGMENT, ext, &entry.start_lba.to_le_bytes());
        }
    }
    let Some(e) = (0..4).find(|&e| is_dos(scanned(d, e).0)) else {
        return fixed_next(d, disk + 1, disks);
    };
    d.set_local8(ENTRY, e);
    let probe = probe(d, disk);
    let (_, start, _) = scanned(d, e);
    d.read_bounce(&probe, start, FIXED_BOOT);
}

/// An extended boot record is read: its logical drive, if a DOS one, has
/// its boot record read; the chain goes on either way.
fn extended_record(d: &mut Driver<'_, '_>, disks: u8) {
    let disk = d.local8(INDEX);
    if d.failed() {
        return logical_next(d, disk + 1, disks);
    }
    let sector = d.bounce();
    let Ok(record) = mbr::ebr(&sector) else {
        return logical_next(d, disk + 1, disks);
    };
    let here = d.local32(SECTOR_AT);
    let ext = extended_of(d, disk);
    let next = record.next.map_or(0, |offset| ext.saturating_add(offset));
    d.set_local32(BASE, next);
    let Some(entry) = record.logical.filter(|entry| is_dos(entry.type_byte)) else {
        return follow_chain(d, disk, disks);
    };
    let start = here.saturating_add(entry.start_lba);
    let at = driver::SCAN;
    d.g.write8(SEGMENT, at, entry.type_byte);
    d.g.write_bytes(SEGMENT, at + 1, &start.to_le_bytes());
    d.g.write_bytes(SEGMENT, at + 5, &entry.sectors.to_le_bytes());
    let probe = probe(d, disk);
    d.read_bounce(&probe, start, LOGICAL_BOOT);
}

fn floppy_type(d: &mut Driver<'_, '_>, drive: u8) {
    d.g.set_r16(Reg16::Ax, 0x0800);
    d.g.set_r16(Reg16::Dx, u16::from(drive));
    d.set_phase(FLOPPY_TYPE);
    d.bios(0x13);
}

/// Reads fixed disk `disk`'s geometry, or once every disk's primary
/// partition is found, starts on their logical drives.
fn fixed_next(d: &mut Driver<'_, '_>, disk: u8, disks: u8) {
    if disk >= disks {
        return logical_next(d, 0, disks);
    }
    d.set_local8(INDEX, disk);
    d.g.set_r16(Reg16::Ax, 0x0800);
    d.g.set_r16(Reg16::Dx, u16::from(FIXED_DRIVE | disk));
    d.set_phase(FIXED_GEOMETRY);
    d.bios(0x13);
}

/// Starts on fixed disk `disk`'s extended partition, or once every disk's
/// is read, answers DOS.
fn logical_next(d: &mut Driver<'_, '_>, disk: u8, disks: u8) {
    let mut disk = disk;
    while disk < disks && extended_of(d, disk) == 0 {
        disk += 1;
    }
    if disk >= disks {
        return init_done(d);
    }
    d.set_local8(INDEX, disk);
    d.set_local8(HOPS, 0);
    let ext = extended_of(d, disk);
    read_ebr(d, disk, ext);
}

fn read_ebr(d: &mut Driver<'_, '_>, disk: u8, at: u32) {
    let (heads, sectors) = disk_geometry(d, disk);
    let probe = Unit {
        drive: FIXED_DRIVE | disk,
        flags: 0,
        heads,
        sectors,
        start: 0,
        total: 1,
    };
    d.set_local32(SECTOR_AT, at);
    d.read_bounce(&probe, at, EBR);
}

/// Follows the EBR chain to the next record, or the next disk.
fn follow_chain(d: &mut Driver<'_, '_>, disk: u8, disks: u8) {
    let next = d.local32(BASE);
    let hops = d.local8(HOPS) + 1;
    d.set_local8(HOPS, hops);
    if next == 0 || hops >= CHAIN {
        return logical_next(d, disk + 1, disks);
    }
    read_ebr(d, disk, next);
}

/// Adds the partition of fixed disk `disk` whose boot sector the bounce
/// buffer holds: with the BPB it records, or, formatted for no volume DOS
/// can read, the one it would be formatted with.
fn add_partition(d: &mut Driver<'_, '_>, disk: u8, start: u32, total: u32) {
    let boot = d.bounce();
    let (heads, sectors) = disk_geometry(d, disk);
    let read = boot_bpb(&boot);
    let Some(bpb) = read.or_else(|| partition(start, total, heads, sectors)) else {
        return;
    };
    let u = Unit {
        drive: FIXED_DRIVE | disk,
        flags: 0,
        heads,
        sectors,
        start,
        total,
    };
    if let Some(n) = add_unit(d, &u) {
        learn(d, n, &bpb, read.is_some().then_some(&boot));
        d.g.write_bytes(SEGMENT, unit_at(n) + unit::RECOMMENDED, &bpb);
        if read.is_none() {
            let at = unit_at(n) + unit::FLAGS;
            let flags = d.g.read8(SEGMENT, at);
            d.g.write8(SEGMENT, at, flags | unit::UNFORMATTED);
        }
    }
}

fn init_done(d: &mut Driver<'_, '_>) {
    let count = d.unit_count();
    for n in 0..count {
        d.g.write16(SEGMENT, driver::BPB_ARRAY + u16::from(n) * 2, unit_bpb(n));
    }
    d.set8(field::MEDIA, count);
    d.set_far(field::COUNT, Far::new(SEGMENT, driver::BPB_ARRAY));
    d.finish(None);
}
