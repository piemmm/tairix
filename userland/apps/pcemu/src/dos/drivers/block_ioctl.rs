//! The block driver's generic IOCTL, category `08h`: device parameters, a
//! track read, written, verified or formatted, the medium's ID, and the
//! access flag — what `FORMAT` and `DISKCOPY` drive a disk through.

use super::block::{
    driver_error, identify, learn, must_swap, read_medium, swap, unit_at, OnChange, BPB_LEN,
    DISKETTE_PARAMETERS, PROGRESS, REPLACED_TABLE,
};
use super::Driver;
use crate::asm::{Reg16, Reg8};
use crate::bios::disk_status;
use crate::bios::layout::diskette;
use crate::cpu::Seg;
use crate::dos::data::{driver, unit, SEGMENT};
use crate::dos::device::generic::media::{FILE_SYSTEM, LABEL, SERIAL};
use crate::dos::device::generic::{access, parameters, track};
use crate::dos::device::generic::{
    DISK, FORMAT_TRACK, GET_ACCESS, GET_MEDIA_ID, GET_PARAMETERS, READ_TRACK, SET_ACCESS,
    SET_MEDIA_ID, SET_PARAMETERS, VERIFY_TRACK, WRITE_TRACK,
};
use crate::dos::device::{error, field};
use crate::dos::engine::Far;
use crate::dos::formats::{bpb_at, word_at, Diskette};
use core::ops::Range;
use tairix_fat::boot::{at as boot_at, DOS_BPB, EXTENDED};
use tairix_partition::mbr;

/// The most sectors one verify call reaches.
const VERIFY_MAX: u32 = 0xFF;
/// Each, and where the boot sector keeps it.
const ID_FIELDS: [(u16, Range<usize>); 3] = [
    (SERIAL, boot_at::SERIAL..boot_at::LABEL),
    (LABEL, boot_at::LABEL..boot_at::TYPE),
    (FILE_SYSTEM, boot_at::TYPE..boot_at::TYPE + 8),
];

/// Phases, past those of the driver's own a request may pass through: a
/// drive two units share is swapped, or a medium's BPB read.
const CALL: u8 = 1;
const VERIFY: u8 = 3;
const MEDIA_SET: u8 = 0x20;
const BOOT_READ: u8 = 0x21;
const BOOT_WRITE: u8 = 0x22;
const STATUS_SET: u8 = 0x23;

/// A generic IOCTL request.
pub fn serve(d: &mut Driver<'_, '_>) {
    let Some(n) = d.request_unit() else {
        return d.finish(Some(error::UNKNOWN_UNIT));
    };
    if d.get8(field::CATEGORY) != DISK {
        return d.finish(Some(error::UNKNOWN_COMMAND));
    }
    let packet = d.get_far(field::PACKET);
    match d.get8(field::FUNCTION) {
        GET_PARAMETERS => get_parameters(d, n, packet),
        SET_PARAMETERS => set_parameters(d, n, packet),
        GET_ACCESS => {
            let readable = d.unit(n).flags & unit::UNFORMATTED == 0;
            let at = packet.add(access::FLAG);
            d.g.write8(at.segment, at.offset, u8::from(readable));
            d.finish(None);
        }
        SET_ACCESS => {
            let at = unit_at(n) + unit::FLAGS;
            let flags = d.g.read8(SEGMENT, at) & !unit::UNFORMATTED;
            let blocked = packet8(d, packet, access::FLAG) == 0;
            d.g.write8(
                SEGMENT,
                at,
                if blocked {
                    flags | unit::UNFORMATTED
                } else {
                    flags
                },
            );
            d.finish(None);
        }
        READ_TRACK => sectors(d, n, packet, 0x02),
        WRITE_TRACK => sectors(d, n, packet, 0x03),
        VERIFY_TRACK => tracks(d, n, packet, false),
        FORMAT_TRACK => tracks(d, n, packet, true),
        GET_MEDIA_ID => media_id(d, n, packet, false),
        SET_MEDIA_ID => media_id(d, n, packet, true),
        _ => d.finish(Some(error::UNKNOWN_COMMAND)),
    }
}

fn packet8(d: &mut Driver<'_, '_>, packet: Far, field: u16) -> u8 {
    let at = packet.add(field);
    d.g.read8(at.segment, at.offset)
}

fn packet16(d: &mut Driver<'_, '_>, packet: Far, field: u16) -> u16 {
    let at = packet.add(field);
    d.g.read16(at.segment, at.offset)
}

/// The unit's device type, as function `60h` names it.
fn device_type(d: &mut Driver<'_, '_>, n: u8) -> u8 {
    match d.g.read8(SEGMENT, unit_at(n) + unit::KIND) {
        0 => parameters::kind::FIXED,
        1 => parameters::kind::DD525,
        2 => parameters::kind::HD525,
        3 => parameters::kind::DD35,
        _ => parameters::kind::OTHER,
    }
}

/// The drive's parameters, with its recommended BPB, or with bit 0 the
/// medium's, read from it.
fn get_parameters(d: &mut Driver<'_, '_>, n: u8, packet: Far) {
    let medium = packet8(d, packet, parameters::SPECIAL) & parameters::MEDIUM_BPB != 0;
    let u = d.unit(n);
    let floppy = u.flags & unit::FLOPPY != 0;
    if medium && floppy {
        match read_medium(d, n) {
            None => return,
            Some(Err(code)) => return d.finish(Some(code)),
            Some(Ok(())) => {}
        }
    }
    let kind = device_type(d, n);
    let mut attributes = if floppy { 0 } else { parameters::NON_REMOVABLE };
    if u.flags & unit::CHANGE_LINE != 0 {
        attributes |= parameters::CHANGE_LINE;
    }
    let cylinders = if floppy {
        Diskette::of_drive(d.g.read8(SEGMENT, unit_at(n) + unit::KIND)).cylinders()
    } else {
        let per_cylinder = u32::from(u.heads) * u32::from(u.sectors);
        u16::try_from(u.total.div_ceil(per_cylinder)).unwrap_or(u16::MAX)
    };
    let mut bpb = [0u8; DOS_BPB];
    let field = if medium { unit::BPB } else { unit::RECOMMENDED };
    d.g.read_bytes(SEGMENT, unit_at(n) + field, &mut bpb);
    let at = |field: u16| packet.add(field);
    let kind_at = at(parameters::KIND);
    d.g.write8(kind_at.segment, kind_at.offset, kind);
    let attributes_at = at(parameters::ATTRIBUTES);
    d.g.write16(attributes_at.segment, attributes_at.offset, attributes);
    let cylinders_at = at(parameters::CYLINDERS);
    d.g.write16(cylinders_at.segment, cylinders_at.offset, cylinders);
    let media = at(parameters::MEDIA_TYPE);
    d.g.write8(media.segment, media.offset, 0);
    let parameters = at(parameters::BPB);
    d.g.write_bytes(parameters.segment, parameters.offset, &bpb);
    d.g.write_bytes(
        parameters.segment,
        parameters.offset.wrapping_add(BPB_LEN),
        &[0; 6],
    );
    d.finish(None);
}

/// Sets the drive's recommended BPB, or with bit 0 the BPB its medium is
/// taken to have until it is set again; either way the medium is taken to
/// be being formatted. The device type, attributes and cylinders describe
/// the drive itself and are kept, and a track layout is always a run of
/// 512-byte sectors numbered from 1, as these drives' are.
fn set_parameters(d: &mut Driver<'_, '_>, n: u8, packet: Far) {
    let special = packet8(d, packet, parameters::SPECIAL);
    if packet16(d, packet, parameters::LAYOUT_ENTRIES) > parameters::LAYOUT_MAX {
        return d.finish(Some(error::GENERAL));
    }
    let mut fake = d.unit(n).flags & unit::FAKE_BPB;
    if special & parameters::LAYOUT_ONLY == 0 {
        let mut bpb = [0u8; DOS_BPB];
        let at = packet.add(parameters::BPB);
        d.g.read_bytes(at.segment, at.offset, &mut bpb);
        if special & parameters::FAKE_BPB == 0 {
            d.g.write_bytes(SEGMENT, unit_at(n) + unit::RECOMMENDED, &bpb);
            fake = 0;
        } else if drives(&bpb) {
            learn(d, n, &bpb, None);
            fake = unit::FAKE_BPB;
        } else {
            return d.finish(Some(error::GENERAL));
        }
    }
    let at = unit_at(n) + unit::FLAGS;
    let flags = d.g.read8(SEGMENT, at) & !unit::FAKE_BPB;
    d.g.write8(SEGMENT, at, flags | fake | unit::FORMATTED | unit::CHANGED);
    d.finish(None);
}

/// Whether a BPB set for a medium describes one these drives can reach:
/// 512-byte sectors, and a geometry INT 13h addresses.
fn drives(bpb: &[u8; DOS_BPB]) -> bool {
    let byte = |at: usize| u8::try_from(word_at(bpb, at)).ok();
    let addressed = byte(bpb_at::HEADS)
        .zip(byte(bpb_at::SECTORS))
        .and_then(|(heads, sectors)| mbr::Geometry::new(heads, sectors));
    word_at(bpb, bpb_at::BYTES_PER_SECTOR) == 512 && addressed.is_some()
}

/// Reads or writes a track's sectors one at a time through the bounce
/// buffer: INT 13h function `function`.
fn sectors(d: &mut Driver<'_, '_>, n: u8, packet: Far, function: u8) {
    let u = d.unit(n);
    let head = packet16(d, packet, track::HEAD);
    let cylinder = packet16(d, packet, track::CYLINDER);
    let first = packet16(d, packet, track::FIRST);
    let count = packet16(d, packet, track::COUNT);
    let buffer = {
        let at = packet.add(track::BUFFER);
        let offset = d.g.read16(at.segment, at.offset);
        Far::new(d.g.read16(at.segment, at.offset.wrapping_add(2)), offset)
    };
    match d.phase() {
        CALL if d.failed() => {
            let status = d.g.r8(Reg8::Ah);
            return d.finish(Some(driver_error(status)));
        }
        CALL => {
            let index = d.local16(PROGRESS);
            if function == 0x02 {
                let sector = d.bounce();
                let to = buffer.add(index.wrapping_mul(512));
                d.g.write_bytes(to.segment, to.offset, &sector);
            }
            d.set_local16(PROGRESS, index + 1);
        }
        _ => {
            if u32::from(first) + u32::from(count) > u32::from(parameters::LAYOUT_MAX)
                || u32::from(cylinder) > mbr::MAX_CYLINDER
                || head > 0xFF
            {
                return d.finish(Some(error::NOT_FOUND));
            }
            if !reached(d, n) {
                return;
            }
            d.set_local16(PROGRESS, 0);
        }
    }
    let index = d.local16(PROGRESS);
    if index >= count {
        return d.finish(None);
    }
    if function == 0x03 {
        let from = buffer.add(index.wrapping_mul(512));
        let mut sector = [0u8; 512];
        d.g.read_bytes(from.segment, from.offset, &mut sector);
        d.g.write_bytes(SEGMENT, driver::BOUNCE, &sector);
    }
    let sector = u8::try_from(first + index + 1).unwrap_or(u8::MAX);
    d.address_sector(&u, cylinder, u8::try_from(head).unwrap_or(u8::MAX), sector);
    d.g.regs_mut().set_sreg(Seg::Es, SEGMENT);
    d.g.set_r16(Reg16::Bx, driver::BOUNCE);
    d.g.set_r16(Reg16::Ax, u16::from_le_bytes([1, function]));
    d.disk_call(CALL, OnChange::Retry);
}

/// Whether unit `n` has its drive, a drive two units share being swapped
/// to it first; the medium is reached now.
fn reached(d: &mut Driver<'_, '_>, n: u8) -> bool {
    if must_swap(d, n) && !swap(d, n) {
        return false;
    }
    d.touch(n);
    true
}

/// The drive sector a track's first sector is, as the unit addresses it.
fn track_lba(u: &super::Unit, cylinder: u16, head: u16) -> u32 {
    (u32::from(cylinder) * u32::from(u.heads) + u32::from(head)) * u32::from(u.sectors)
}

/// Verifies a track, formatting it first when `format` and the drive is a
/// floppy's; a fixed disk's track is only verified, as DOS's `FORMAT` of
/// one does. Bit 1 asks for several tracks, from the one named on through
/// the heads and then the cylinders: verified, never formatted, as a
/// floppy is formatted a track at a time.
fn tracks(d: &mut Driver<'_, '_>, n: u8, packet: Far, format: bool) {
    let u = d.unit(n);
    let special = packet8(d, packet, track::SPECIAL);
    if format && special & track::STATUS != 0 {
        return status(d, n, packet);
    }
    let floppy = u.flags & unit::FLOPPY != 0;
    let several = special & track::SEVERAL != 0;
    let count = if several {
        packet16(d, packet, track::TRACKS)
    } else {
        1
    };
    if several && (format && floppy || u32::from(count) * u32::from(u.sectors) > VERIFY_MAX) {
        return d.finish(Some(error::UNKNOWN_COMMAND));
    }
    match d.phase() {
        MEDIA_SET if d.failed() && d.g.r8(Reg8::Ah) == disk_status::TIMEOUT => {
            return d.finish(Some(error::NOT_READY));
        }
        MEDIA_SET => {
            // The BIOS formats as many sectors as the parameters INT 1Eh
            // points at say: the medium's, from the BIOS where it has them.
            let table = if d.failed() {
                Far::new(
                    d.g.read16(0, DISKETTE_PARAMETERS + 2),
                    d.g.read16(0, DISKETTE_PARAMETERS),
                )
            } else {
                Far::new(d.g.seg(Seg::Es), d.g.r16(Reg16::Di))
            };
            replace_table(d, table, u.sectors);
            return format_track(d, n, packet);
        }
        CALL => {
            restore_table(d);
            if d.failed() {
                let status = d.g.r8(Reg8::Ah);
                return d.finish(Some(driver_error(status)));
            }
        }
        VERIFY if d.failed() => {
            let status = d.g.r8(Reg8::Ah);
            return d.finish(Some(driver_error(status)));
        }
        VERIFY => {
            let done = d.local16(PROGRESS) + 1;
            d.set_local16(PROGRESS, done);
        }
        _ => {
            if !reached(d, n) {
                return;
            }
            d.set_local16(PROGRESS, 0);
            if format && floppy {
                return set_media(d, &u, MEDIA_SET);
            }
        }
    }
    let done = d.local16(PROGRESS);
    if done >= count {
        return d.finish(None);
    }
    let track = u32::from(packet16(d, packet, track::HEAD)) + u32::from(done);
    let heads = u32::from(u.heads);
    let cylinder = u32::from(packet16(d, packet, track::CYLINDER)) + track / heads;
    let lba = (cylinder * heads + track % heads) * u32::from(u.sectors);
    d.address(&u, lba);
    d.g.regs_mut().set_sreg(Seg::Es, SEGMENT);
    d.g.set_r16(Reg16::Bx, driver::BOUNCE);
    d.g.set_r16(Reg16::Ax, u16::from_le_bytes([u.sectors, 0x04]));
    d.disk_call(VERIFY, OnChange::Retry);
}

/// Points INT 1Eh at a copy of the parameters at `table`, its sectors a
/// track `sectors`, keeping the vector it held.
fn replace_table(d: &mut Driver<'_, '_>, table: Far, sectors: u8) {
    let mut parameters = [0u8; diskette::LEN as usize];
    d.g.read_bytes(table.segment, table.offset, &mut parameters);
    parameters[usize::from(diskette::LAST_SECTOR)] = sectors;
    d.g.write_bytes(SEGMENT, driver::FORMAT_TABLE, &parameters);
    let offset = d.g.read16(0, DISKETTE_PARAMETERS);
    let segment = d.g.read16(0, DISKETTE_PARAMETERS + 2);
    d.set_local16(REPLACED_TABLE, offset);
    d.set_local16(REPLACED_TABLE + 2, segment);
    d.g.write16(0, DISKETTE_PARAMETERS, driver::FORMAT_TABLE);
    d.g.write16(0, DISKETTE_PARAMETERS + 2, SEGMENT);
}

/// Points INT 1Eh back where [`replace_table`] found it.
fn restore_table(d: &mut Driver<'_, '_>) {
    let offset = d.local16(REPLACED_TABLE);
    let segment = d.local16(REPLACED_TABLE + 2);
    d.g.write16(0, DISKETTE_PARAMETERS, offset);
    d.g.write16(0, DISKETTE_PARAMETERS + 2, segment);
}

/// Formats the packet's track of 512-byte sectors numbered from 1.
fn format_track(d: &mut Driver<'_, '_>, n: u8, packet: Far) {
    let u = d.unit(n);
    let [cylinder, ..] = packet16(d, packet, track::CYLINDER).to_le_bytes();
    let [head, ..] = packet16(d, packet, track::HEAD).to_le_bytes();
    let mut fields = [0u8; 512];
    for (s, chunk) in (1..=u.sectors).zip(fields.as_chunks_mut::<4>().0) {
        chunk.copy_from_slice(&[cylinder, head, s, 2]);
    }
    d.g.write_bytes(SEGMENT, driver::BOUNCE, &fields);
    let lba = track_lba(&u, u16::from(cylinder), u16::from(head));
    d.address(&u, lba);
    d.g.regs_mut().set_sreg(Seg::Es, SEGMENT);
    d.g.set_r16(Reg16::Bx, driver::BOUNCE);
    d.g.set_r16(Reg16::Ax, u16::from_le_bytes([u.sectors, 0x05]));
    d.disk_call(CALL, OnChange::Retry);
}

/// Asks the BIOS to set the drive up for formatting the medium the unit
/// now has, as INT 13h AH 18h does.
fn set_media(d: &mut Driver<'_, '_>, u: &super::Unit, phase: u8) {
    let per_cylinder = u32::from(u.heads) * u32::from(u.sectors);
    let last =
        u16::try_from((u.start + u.total).div_ceil(per_cylinder).saturating_sub(1)).unwrap_or(0);
    let [low, high] = last.to_le_bytes();
    d.g.set_r16(Reg16::Ax, 0x1800);
    d.g.set_r16(
        Reg16::Cx,
        u16::from_le_bytes([u.sectors | (high & 3) << 6, low]),
    );
    d.g.set_r16(Reg16::Dx, u16::from(u.drive));
    d.set_phase(phase);
    d.bios(0x13);
}

/// A format status call: whether the BIOS formats the medium the unit was
/// set to, answered in the special function byte. A fixed disk takes no
/// media type.
fn status(d: &mut Driver<'_, '_>, n: u8, packet: Far) {
    let u = d.unit(n);
    let answer = match d.phase() {
        _ if u.flags & unit::FLOPPY == 0 => track::status::NO_ROM,
        STATUS_SET if !d.failed() => track::status::SUPPORTED,
        STATUS_SET => match d.g.r8(Reg8::Ah) {
            disk_status::BAD_MEDIA => track::status::UNKNOWN_MEDIUM,
            disk_status::TIMEOUT => track::status::EMPTY,
            _ => track::status::NO_ROM,
        },
        _ => return set_media(d, &u, STATUS_SET),
    };
    let at = packet.add(track::SPECIAL);
    d.g.write8(at.segment, at.offset, answer);
    d.finish(None);
}

/// Reads the medium's ID from its boot sector, or writes the packet's into
/// it when `set`.
fn media_id(d: &mut Driver<'_, '_>, n: u8, packet: Far, set: bool) {
    let u = d.unit(n);
    match d.phase() {
        BOOT_READ | BOOT_WRITE if d.failed() => {
            let status = d.g.r8(Reg8::Ah);
            d.finish(Some(driver_error(status)));
        }
        BOOT_READ => {
            let mut boot = d.bounce();
            if boot[boot_at::EXTENDED] != EXTENDED {
                return d.finish(Some(error::UNKNOWN_MEDIA));
            }
            for (field, bytes) in ID_FIELDS {
                let at = packet.add(field);
                if set {
                    d.g.read_bytes(at.segment, at.offset, &mut boot[bytes]);
                } else {
                    d.g.write_bytes(at.segment, at.offset, &boot[bytes]);
                }
            }
            if !set {
                return d.finish(None);
            }
            d.g.write_bytes(SEGMENT, driver::BOUNCE, &boot);
            d.address(&u, u.start);
            d.g.regs_mut().set_sreg(Seg::Es, SEGMENT);
            d.g.set_r16(Reg16::Bx, driver::BOUNCE);
            d.g.set_r16(Reg16::Ax, 0x0301);
            d.disk_call(BOOT_WRITE, OnChange::Retry);
        }
        // Only the volume's identity has changed.
        BOOT_WRITE => {
            let boot = d.bounce();
            identify(d, n, Some(&boot));
            d.finish(None);
        }
        _ => {
            if reached(d, n) {
                d.read_bounce(&u, u.start, BOOT_READ);
            }
        }
    }
}

#[cfg(test)]
#[path = "block_ioctl_tests.rs"]
mod tests;
