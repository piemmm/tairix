//! What a copy is made of: the source disk's format found, the target
//! checked against it, and the target formatted to it where it differs.

use super::{
    at, flag, has, ioctl, past, set_flag, set_parameters, soft, text, track_request, DiskCopyPress,
};
use crate::dos::device::failed;
use crate::dos::device::generic::{self, parameters, track};
use crate::dos::formats::{bpb_at, total_of, word_at, Diskette};
use crate::dos::message::decimal;
use crate::dos::program::{say, Flow, Frame, Id, Program, Routine};
use crate::frames::{locals, take, Far, Fault, Outcome};
use tairix_fat::boot::DOS_BPB;

/// The most sectors and cylinders a disk without a BPB is taken to have:
/// the 15 of a high-density 5¼-inch disk's 80 tracks, or the 8 or 9 of a
/// double-density one's 40.
const HIGH_DENSITY: (u8, u8) = (15, 79);
const DOUBLE_DENSITY: u8 = 39;

/// The BPB the parameter block at `at` holds.
fn bpb_of(p: &mut Program<'_, '_>, at: u16) -> [u8; DOS_BPB] {
    let mut bpb = [0u8; DOS_BPB];
    p.g.read_bytes(p.own, at + parameters::BPB, &mut bpb);
    bpb
}

/// A disk's geometry, as the BPB at `at` gives it: sectors a track, heads,
/// cylinders — none if the BPB names no sectors or heads — and bytes a
/// sector.
fn geometry(p: &mut Program<'_, '_>, at: u16) -> Option<(u16, u16, u32, u16)> {
    let bpb = bpb_of(p, at);
    let (sectors, heads) = (word_at(&bpb, bpb_at::SECTORS), word_at(&bpb, bpb_at::HEADS));
    if sectors == 0 || heads == 0 {
        return None;
    }
    let cylinders = total_of(&bpb) / (u32::from(sectors) * u32::from(heads));
    Some((
        sectors,
        heads,
        cylinders,
        word_at(&bpb, bpb_at::BYTES_PER_SECTOR),
    ))
}

/// The target drive's own cylinders, sectors a track and heads.
fn target_drive(p: &mut Program<'_, '_>) -> (u16, u16, u16) {
    let bpb = bpb_of(p, at::TARGET_DRIVE);
    (
        p.get16(at::TARGET_DRIVE + parameters::CYLINDERS),
        word_at(&bpb, bpb_at::SECTORS),
        word_at(&bpb, bpb_at::HEADS),
    )
}

/// Lays a track layout of the copy's sectors, numbered from 1, into the
/// parameter block at `at`.
fn lay_out(p: &mut Program<'_, '_>, at: u16) {
    let (sectors, bytes) = (u16::from(p.get8(at::SECTORS)), p.get16(at::SECTOR_BYTES));
    p.set16(at + parameters::LAYOUT_ENTRIES, sectors);
    for n in 0..sectors.min(parameters::LAYOUT_MAX) {
        let entry = at + parameters::LAYOUT + n * 4;
        p.set16(entry, n + 1);
        p.set16(entry + 2, bytes);
    }
}

/// The copy can go on no further: `words` say why.
fn fatal(p: &mut Program<'_, '_>, phase: &mut u8, said: u8, words: &[u8]) -> Flow {
    set_flag(p, flag::FATAL, true);
    *phase = said;
    Flow::Call(say(p, words))
}

locals! {
    /// The source's format, as DOS 4.0's `CHECK_SOURCE` finds it: its BPB
    /// asked of the drive, or for a disk without one its sectors and sides
    /// found by the sectors it holds; checked against the target drive, set
    /// on the source drive, and told.
    pub struct DiskCopySource {}
}

mod source {
    pub const ASKED: u8 = 1;
    pub const PRESSED: u8 = 2;
    pub const EIGHTH: u8 = 3;
    pub const NINTH: u8 = 4;
    pub const FIFTEENTH: u8 = 5;
    pub const SECOND_SIDE: u8 = 6;
    pub const SET: u8 = 7;
    pub const SAID: u8 = 8;
}

impl DiskCopySource {
    /// The medium's format, from its BPB.
    fn known(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let Some((sectors, heads, cylinders, bytes)) =
            geometry(p, at::SOURCE_MEDIUM).filter(|g| g.2 != 0)
        else {
            return Self::old(p, phase);
        };
        let (target_cylinders, target_sectors, target_heads) = target_drive(p);
        let one_side = has(p, flag::ONE_SIDE);
        let fits = cylinders <= u32::from(target_cylinders)
            && sectors <= target_sectors
            && (one_side || heads <= target_heads);
        let (Ok(last), Ok(sectors), Ok(last_side), true) = (
            u8::try_from(cylinders - 1),
            u8::try_from(sectors),
            u8::try_from(heads - 1),
            fits,
        ) else {
            return fatal(p, phase, source::SAID, text::INCOMPATIBLE);
        };
        p.set8(at::LAST_TRACK, last);
        p.set8(at::SECTORS, sectors);
        p.set16(at::SECTOR_BYTES, bytes);
        p.set8(at::LAST_SIDE, if one_side { 0 } else { last_side });
        Self::table(p, phase)
    }

    /// A disk with no BPB to read, as DOS 2's and earlier: its sectors
    /// found by which of the eighth, ninth and fifteenth it holds.
    fn old(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        p.set16(at::SECTOR_BYTES, 512);
        *phase = source::EIGHTH;
        Flow::Call(Frame::of(&DiskCopyProbe::of(at::SOURCE, 0, 8)))
    }

    /// The old disk's sectors and cylinders known: its sides next.
    fn sided(p: &mut Program<'_, '_>, phase: &mut u8, sectors: u8, last: u8) -> Flow {
        p.set8(at::SECTORS, sectors);
        p.set8(at::LAST_TRACK, last);
        p.set8(at::LAST_SIDE, 0);
        if has(p, flag::ONE_SIDE) {
            return Self::old_table(p, phase);
        }
        *phase = source::SECOND_SIDE;
        Flow::Call(Frame::of(&DiskCopyProbe::of(at::SOURCE, 1, sectors)))
    }

    /// The old disk's parameters, as DOS's own standard disk of its
    /// sectors and sides has them.
    fn old_table(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let diskette = match (p.get8(at::SECTORS), p.get8(at::LAST_SIDE)) {
            (8, 0) => Diskette::K160,
            (8, _) => Diskette::K320,
            (9, 0) => Diskette::K180,
            (9, _) => Diskette::K360,
            _ => Diskette::K1200,
        };
        p.g.write_bytes(p.own, at::SOURCE_MEDIUM + parameters::BPB, &diskette.bpb());
        Self::table(p, phase)
    }

    /// The format known: laid out for both disks, set on the source drive.
    fn table(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        lay_out(p, at::SOURCE_MEDIUM);
        lay_out(p, at::TARGET_MEDIUM);
        set_flag(p, flag::SOURCE_SET, true);
        *phase = source::SET;
        Flow::Call(set_parameters(
            p,
            at::SOURCE,
            at::SOURCE_MEDIUM,
            parameters::GOOD_LAYOUT,
        ))
    }
}

impl Routine for DiskCopySource {
    const ID: Id = Id::DiskCopySource;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            source::ASKED => match child {
                Ok(_) => Self::known(p, phase),
                Err(Fault::Error(code)) => match soft(code) {
                    Some(what) => {
                        *phase = source::PRESSED;
                        Flow::Call(Frame::of(&DiskCopyPress::of(what, at::SOURCE)))
                    }
                    None => Self::old(p, phase),
                },
                Err(fault) => Flow::Return(Err(fault)),
            },
            source::EIGHTH => {
                if take!(child) == 0 {
                    return fatal(p, phase, source::SAID, text::BAD_SOURCE);
                }
                *phase = source::NINTH;
                Flow::Call(Frame::of(&DiskCopyProbe::of(at::SOURCE, 0, 9)))
            }
            source::NINTH => {
                if take!(child) == 0 {
                    return Self::sided(p, phase, 8, DOUBLE_DENSITY);
                }
                *phase = source::FIFTEENTH;
                Flow::Call(Frame::of(&DiskCopyProbe::of(at::SOURCE, 0, HIGH_DENSITY.0)))
            }
            source::FIFTEENTH => match take!(child) {
                0 => Self::sided(p, phase, 9, DOUBLE_DENSITY),
                _ => Self::sided(p, phase, HIGH_DENSITY.0, HIGH_DENSITY.1),
            },
            source::SECOND_SIDE => {
                if take!(child) == 0 {
                    return Self::old_table(p, phase);
                }
                p.set8(at::LAST_SIDE, 1);
                if target_drive(p).2 != 2 {
                    return fatal(p, phase, source::SAID, text::INCOMPATIBLE);
                }
                Self::old_table(p, phase)
            }
            source::SET => {
                if let Err(fault) = past(child) {
                    return Flow::Return(Err(fault));
                }
                let tracks = decimal(u32::from(p.get8(at::LAST_TRACK)) + 1);
                let sectors = decimal(u32::from(p.get8(at::SECTORS)));
                let sides = decimal(u32::from(p.get8(at::LAST_SIDE)) + 1);
                let words = [
                    &b"\r\nCopying "[..],
                    &tracks,
                    b" tracks\r\n",
                    &sectors,
                    b" Sectors/Track, ",
                    &sides,
                    b" Side(s)\r\n",
                ]
                .concat();
                *phase = source::SAID;
                Flow::Call(say(p, &words))
            }
            source::SAID => Flow::Return(child.map(|_| 0)),
            _ => {
                if *phase == source::PRESSED {
                    take!(child);
                }
                p.set8(
                    at::SOURCE_MEDIUM + parameters::SPECIAL,
                    parameters::MEDIUM_BPB,
                );
                *phase = source::ASKED;
                Flow::Call(ioctl(
                    p,
                    at::SOURCE,
                    generic::GET_PARAMETERS,
                    at::SOURCE_MEDIUM,
                ))
            }
        }
    }
}

locals! {
    /// The target checked against the source's format, as DOS 4.0's
    /// `CHECK_TARGET` checks it: its own BPB, or for a disk without one the
    /// sectors it holds; set on the target drive where they agree, the
    /// target formatted to the source's where they do not.
    pub struct DiskCopyTarget {}
}

mod target {
    pub const ASKED: u8 = 1;
    pub const PRESSED: u8 = 2;
    pub const EIGHTH: u8 = 3;
    pub const NINTH: u8 = 4;
    pub const FIFTEENTH: u8 = 5;
    pub const FORMAT_SAID: u8 = 6;
    pub const DONE: u8 = 7;
}

impl DiskCopyTarget {
    /// The target's BPB against the source's format.
    fn known(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let Some((sectors, heads, cylinders, bytes)) = geometry(p, at::TARGET_MEDIUM) else {
            return Self::old(phase);
        };
        let last = cylinders.checked_sub(1);
        if last != Some(u32::from(p.get8(at::LAST_TRACK))) {
            if sectors == u16::from(HIGH_DENSITY.0) && p.get8(at::LAST_TRACK) == DOUBLE_DENSITY {
                return fatal(p, phase, target::DONE, text::INCOMPATIBLE);
            }
            return Self::format(p, phase);
        }
        let same = sectors == u16::from(p.get8(at::SECTORS))
            && bytes == p.get16(at::SECTOR_BYTES)
            && heads.saturating_sub(1) >= u16::from(p.get8(at::LAST_SIDE));
        if same {
            Self::set(p, phase)
        } else {
            Self::format(p, phase)
        }
    }

    /// A target with no BPB to read: formatted unless the sectors it holds
    /// are the source's, a high-density source's never written over a
    /// double-density disk or the other way round.
    fn old(phase: &mut u8) -> Flow {
        *phase = target::EIGHTH;
        Flow::Call(Frame::of(&DiskCopyProbe::of(at::TARGET, 0, 8)))
    }

    /// The target's parameters set to the source's, for writing.
    fn set(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        set_flag(p, flag::TARGET_SET, true);
        let cylinders = u16::from(p.get8(at::LAST_TRACK)) + 1;
        p.set16(at::TARGET_MEDIUM + parameters::CYLINDERS, cylinders);
        *phase = target::DONE;
        Flow::Call(set_parameters(
            p,
            at::TARGET,
            at::TARGET_MEDIUM,
            parameters::GOOD_LAYOUT,
        ))
    }

    /// The old target agrees with the source: the source's BPB is its own.
    fn as_source(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let mut bpb = [0u8; DOS_BPB];
        p.g.read_bytes(p.own, at::SOURCE_MEDIUM + parameters::BPB, &mut bpb);
        p.g.write_bytes(p.own, at::TARGET_MEDIUM + parameters::BPB, &bpb);
        Self::set(p, phase)
    }

    fn format(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        set_flag(p, flag::FORMATTING, true);
        *phase = target::FORMAT_SAID;
        Flow::Call(say(p, text::FORMATTING))
    }
}

impl Routine for DiskCopyTarget {
    const ID: Id = Id::DiskCopyTarget;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let high = p.get8(at::SECTORS) == HIGH_DENSITY.0;
        match *phase {
            target::ASKED => match child {
                Ok(_) => Self::known(p, phase),
                Err(Fault::Error(code)) => match soft(code) {
                    Some(what) => {
                        *phase = target::PRESSED;
                        Flow::Call(Frame::of(&DiskCopyPress::of(what, at::TARGET)))
                    }
                    None if code == failed::UNKNOWN_MEDIA => Self::format(p, phase),
                    None => Self::old(phase),
                },
                Err(fault) => Flow::Return(Err(fault)),
            },
            target::EIGHTH => {
                if take!(child) == 0 {
                    return Self::format(p, phase);
                }
                *phase = target::NINTH;
                Flow::Call(Frame::of(&DiskCopyProbe::of(at::TARGET, 0, 9)))
            }
            target::NINTH => match take!(child) {
                0 if high => fatal(p, phase, target::DONE, text::INCOMPATIBLE),
                0 if p.get8(at::SECTORS) == 9 => Self::format(p, phase),
                0 => Self::as_source(p, phase),
                _ => {
                    *phase = target::FIFTEENTH;
                    Flow::Call(Frame::of(&DiskCopyProbe::of(at::TARGET, 0, HIGH_DENSITY.0)))
                }
            },
            target::FIFTEENTH => match (take!(child), high) {
                (0, false) | (1.., true) => Self::as_source(p, phase),
                _ => fatal(p, phase, target::DONE, text::INCOMPATIBLE),
            },
            target::FORMAT_SAID => {
                take!(child);
                *phase = target::DONE;
                Flow::Call(Frame::of(&DiskCopyFormat::default()))
            }
            target::DONE => Flow::Return(child.map(|_| 0)),
            _ => {
                if *phase == target::PRESSED {
                    take!(child);
                }
                p.set8(
                    at::TARGET_MEDIUM + parameters::SPECIAL,
                    parameters::MEDIUM_BPB,
                );
                *phase = target::ASKED;
                Flow::Call(ioctl(
                    p,
                    at::TARGET,
                    generic::GET_PARAMETERS,
                    at::TARGET_MEDIUM,
                ))
            }
        }
    }
}

locals! {
    /// Sector `sector` of the track at head `head` of cylinder 0 read from
    /// the drive at `drive` in the state, as DOS 4.0's `READ_A_SECTOR`
    /// reads one to tell a disk's format: `1` read, `0` not there; a drive
    /// the user can put right asked about and the sector tried again.
    pub struct DiskCopyProbe {
        drive: u16,
        head: u8,
        sector: u8,
    }
}

impl DiskCopyProbe {
    #[must_use]
    pub const fn of(drive: u16, head: u8, sector: u8) -> Self {
        Self {
            drive,
            head,
            sector,
        }
    }
}

mod probing {
    pub const READ: u8 = 1;
    pub const PRESSED: u8 = 2;
}

impl Routine for DiskCopyProbe {
    const ID: Id = Id::DiskCopyProbe;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if *phase == probing::READ {
            return match child {
                Ok(_) => Flow::Return(Ok(1)),
                Err(Fault::Error(code)) => match soft(code) {
                    Some(what) => {
                        *phase = probing::PRESSED;
                        Flow::Call(Frame::of(&DiskCopyPress::of(what, self.drive)))
                    }
                    None => Flow::Return(Ok(0)),
                },
                Err(fault) => Flow::Return(Err(fault)),
            };
        }
        if *phase == probing::PRESSED {
            take!(child);
        }
        track_request(
            p,
            self.head,
            0,
            u16::from(self.sector.saturating_sub(1)),
            1,
            Far::new(p.own, at::SECTOR),
        );
        *phase = probing::READ;
        Flow::Call(ioctl(p, self.drive, generic::READ_TRACK, at::PACKET))
    }
}

locals! {
    /// The target formatted to the source's format from the track the copy
    /// has reached to its end, as DOS 4.0's `FORMAT_ALL` formats it: the
    /// source's BPB set as the target's medium's, the BIOS asked whether it
    /// formats that, each track formatted, and the parameters set back for
    /// writing. A second side that cannot be formatted at the first
    /// cylinder is a target with one; any other track, a bad target.
    pub struct DiskCopyFormat {}
}

mod formatting {
    pub const SET: u8 = 1;
    pub const STATUS: u8 = 2;
    pub const FORMATTED: u8 = 3;
    pub const PRESSED: u8 = 4;
    pub const SAID: u8 = 5;
    pub const DONE: u8 = 6;
}

impl DiskCopyFormat {
    fn next(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if p.get8(at::FORMAT_SIDE) > p.get8(at::LAST_SIDE) {
            p.set8(at::FORMAT_SIDE, 0);
            let track = p.get8(at::FORMAT_TRACK).saturating_add(1);
            p.set8(at::FORMAT_TRACK, track);
        }
        if p.get8(at::FORMAT_TRACK) > p.get8(at::LAST_TRACK) {
            return Self::done(p, phase);
        }
        let (side, cylinder) = (p.get8(at::FORMAT_SIDE), p.get8(at::FORMAT_TRACK));
        track_request(p, side, cylinder, 0, 0, Far::new(0, 0));
        *phase = formatting::FORMATTED;
        Flow::Call(ioctl(p, at::TARGET, generic::FORMAT_TRACK, at::PACKET))
    }

    /// The target's parameters set back for writing what was read.
    fn done(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        set_flag(p, flag::TARGET_SET, true);
        *phase = formatting::DONE;
        Flow::Call(set_parameters(
            p,
            at::TARGET,
            at::TARGET_MEDIUM,
            parameters::GOOD_LAYOUT,
        ))
    }

    fn failed(p: &mut Program<'_, '_>, phase: &mut u8, words: &[u8]) -> Flow {
        set_flag(p, flag::FATAL, true);
        *phase = formatting::SAID;
        Flow::Call(say(p, words))
    }
}

impl Routine for DiskCopyFormat {
    const ID: Id = Id::DiskCopyFormat;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            formatting::SET => {
                match child {
                    Ok(_) => {}
                    Err(Fault::Error(_)) => return Self::failed(p, phase, text::INCOMPATIBLE),
                    Err(fault) => return Flow::Return(Err(fault)),
                }
                p.g.write_bytes(p.own, at::PACKET, &[track::STATUS, 0, 0, 0, 0]);
                *phase = formatting::STATUS;
                Flow::Call(ioctl(p, at::TARGET, generic::FORMAT_TRACK, at::PACKET))
            }
            formatting::STATUS => {
                if let Err(fault) = past(child) {
                    return Flow::Return(Err(fault));
                }
                if child.is_ok()
                    && p.get8(at::PACKET + track::SPECIAL) == track::status::UNKNOWN_MEDIUM
                {
                    return Self::failed(p, phase, text::INCOMPATIBLE);
                }
                let (track, side) = (p.get8(at::TO_WRITE), p.get8(at::SIDE));
                p.set8(at::FORMAT_TRACK, track);
                p.set8(at::FORMAT_SIDE, side);
                Self::next(p, phase)
            }
            formatting::FORMATTED => match child {
                Ok(_) => {
                    let side = p.get8(at::FORMAT_SIDE) + 1;
                    p.set8(at::FORMAT_SIDE, side);
                    Self::next(p, phase)
                }
                Err(Fault::Error(code)) => {
                    if let Some(what) = soft(code) {
                        *phase = formatting::PRESSED;
                        return Flow::Call(Frame::of(&DiskCopyPress::of(what, at::TARGET)));
                    }
                    let one_sided = p.get8(at::FORMAT_SIDE) == 1 && p.get8(at::FORMAT_TRACK) == 0;
                    Self::failed(
                        p,
                        phase,
                        if one_sided {
                            text::INCOMPATIBLE
                        } else {
                            text::BAD_TARGET
                        },
                    )
                }
                Err(fault) => Flow::Return(Err(fault)),
            },
            formatting::PRESSED => {
                take!(child);
                Self::next(p, phase)
            }
            formatting::SAID => {
                take!(child);
                Self::done(p, phase)
            }
            formatting::DONE => Flow::Return(child.map(|_| 0)),
            _ => {
                let mut bpb = [0u8; DOS_BPB];
                p.g.read_bytes(p.own, at::SOURCE_MEDIUM + parameters::BPB, &mut bpb);
                p.g.write_bytes(p.own, at::TARGET_MEDIUM + parameters::BPB, &bpb);
                let (drive_cylinders, drive_sectors, _) = target_drive(p);
                let lesser = u16::from(p.get8(at::LAST_TRACK)) < drive_cylinders.saturating_sub(1)
                    || u16::from(p.get8(at::SECTORS)) < drive_sectors;
                p.set8(at::TARGET_MEDIUM + parameters::MEDIA_TYPE, u8::from(lesser));
                let cylinders = u16::from(p.get8(at::LAST_TRACK)) + 1;
                p.set16(at::TARGET_MEDIUM + parameters::CYLINDERS, cylinders);
                *phase = formatting::SET;
                Flow::Call(set_parameters(
                    p,
                    at::TARGET,
                    at::TARGET_MEDIUM,
                    parameters::FAKE_BPB | parameters::GOOD_LAYOUT,
                ))
            }
        }
    }
}
