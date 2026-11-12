//! One disk formatted: its parameters set, the user asked for it, its
//! tracks formatted and their bad clusters marked, its volume and system
//! written, its label made and its report given — and the questions
//! `FORMAT` asks, as DOS 4.0's asks them.

use super::{
    at, bad_track, device_error, exit, flag, has, ioctl, over_progress, replan, set_flag,
    set_parameters, switch, switched, text, volume_of, FormatBad, FormatSystem, FormatTracks,
    FormatWrite, PROGRESS_WIDTH,
};
use crate::dos::device::failed;
use crate::dos::device::generic::parameters::kind;
use crate::dos::device::generic::{self, parameters, track};
use crate::dos::message;
use crate::dos::program::{say, say_error, Flow, Frame, Id, Int, Line, Program, Returned, Routine};
use crate::dos::shell::letter;
use crate::dos::utility::{figure_line, label, place, space};
use crate::frames::{locals, take, Fault, Outcome};
use alloc::vec::Vec;
use tairix_fat::boot::{at as boot_at, Bpb, Kind, BOOT_SECTOR, SIGNATURE};
use tairix_fat::table;
use tairix_partition::mbr;

/// What a disk's format came to.
pub mod outcome {
    /// Formatted, and reported.
    pub const DONE: u32 = 0;
    /// Not formatted, and said so: another may be.
    pub const FAILED: u32 = 1;
    /// `FORMAT` ends: its code is set.
    pub const FATAL: u32 = 2;
}

/// What a yes-or-no question was answered.
pub mod answer {
    pub use crate::dos::program::answer::{NEITHER, NO, YES};
    pub use crate::dos::program::ENDED;
}

/// The line [`FormatAsk`] read.
pub(crate) fn typed(p: &mut Program<'_, '_>) -> Vec<u8> {
    let len = p.get8(at::LINE + 1).min(at::LINE_ROOM);
    let mut out = alloc::vec![0u8; usize::from(len)];
    p.g.read_bytes(p.own, at::LINE + 2, &mut out);
    out
}

/// The track request for the track holding the volume's first sector,
/// read into the buffer: the boot record, whatever volume, if any, the
/// partition holds, reached without DOS's media check.
pub(crate) fn read_boot(p: &mut Program<'_, '_>, phase: &mut u8, then: u8) -> Flow {
    let bpb = volume_of(p);
    let (hidden, sectors, heads) = bpb.map_or((0, 1, 1), |b| {
        (
            b.hidden_sectors,
            u32::from(b.sectors_per_track.max(1)),
            u32::from(b.heads.max(1)),
        )
    });
    let track = hidden / sectors;
    let cylinder = u16::try_from(track / heads).unwrap_or(u16::MAX);
    let head = u16::try_from(track % heads).unwrap_or(0);
    let first = u16::try_from(hidden % sectors).unwrap_or(0);
    track_request(p, head, cylinder, first, 1);
    *phase = then;
    Flow::Call(ioctl(p, generic::READ_TRACK, at::PACKET))
}

/// Lays a track read or write request for `count` sectors from `first` of
/// the track at `cylinder` and `head` into the buffer.
pub(crate) fn track_request(
    p: &mut Program<'_, '_>,
    head: u16,
    cylinder: u16,
    first: u16,
    count: u16,
) {
    p.set8(at::PACKET + track::SPECIAL, 0);
    p.set16(at::PACKET + track::HEAD, head);
    p.set16(at::PACKET + track::CYLINDER, cylinder);
    p.set16(at::PACKET + track::FIRST, first);
    p.set16(at::PACKET + track::COUNT, count);
    p.set_far(
        at::PACKET + track::BUFFER,
        crate::frames::Far::new(p.own, at::BUFFER),
    );
}

/// Whether the boot record the read `read` brought is one: its signature
/// is there.
pub(crate) fn boot_valid(p: &mut Program<'_, '_>, read: Outcome) -> bool {
    read.is_ok() && signed(p)
}

/// Whether the sector in the buffer ends with the boot signature.
fn signed(p: &mut Program<'_, '_>) -> bool {
    let mut sector = [0u8; BOOT_SECTOR];
    p.g.read_bytes(p.own, at::BUFFER, &mut sector);
    sector[boot_at::SIGNATURE..] == SIGNATURE
}

locals! {
    /// A line typed in answer, the console's input flushed before and
    /// after, as DOS 4.0's `FORMAT` reads one: the bytes typed, or with
    /// `yes_no` the [`answer`] the country's yes and no make of them; either
    /// way [`answer::ENDED`] from an input with none left, whose line is
    /// empty.
    pub struct FormatAsk {
        yes_no: bool,
        ended: bool,
    }
}

impl FormatAsk {
    #[must_use]
    pub const fn line() -> Self {
        Self {
            yes_no: false,
            ended: false,
        }
    }

    #[must_use]
    pub const fn yes_no() -> Self {
        Self {
            yes_no: true,
            ended: false,
        }
    }
}

mod asking {
    pub const FLUSHED: u8 = 1;
    pub const TYPED: u8 = 2;
    pub const FLUSHED_AFTER: u8 = 3;
    pub const ANSWERED: u8 = 4;
}

impl Routine for FormatAsk {
    const ID: Id = Id::FormatAsk;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            asking::FLUSHED => {
                take!(child);
                p.set8(at::LINE, at::LINE_ROOM);
                p.set8(at::LINE + 1, 0);
                *phase = asking::TYPED;
                Flow::Call(Frame::of(&Line::at(at::LINE)))
            }
            asking::TYPED => {
                self.ended = take!(child) == answer::ENDED;
                *phase = asking::FLUSHED_AFTER;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x0C00)))
            }
            asking::FLUSHED_AFTER => {
                take!(child);
                if self.ended {
                    return Flow::Return(Ok(answer::ENDED));
                }
                let count = p.get8(at::LINE + 1);
                if !self.yes_no {
                    return Flow::Return(Ok(u32::from(count)));
                }
                if count == 0 {
                    return Flow::Return(Ok(answer::NEITHER));
                }
                let first = p.get8(at::LINE + 2);
                *phase = asking::ANSWERED;
                Flow::Call(Frame::of(&Int {
                    dx: u16::from(first),
                    ..Int::dos(p.own, 0x6523)
                }))
            }
            asking::ANSWERED => Flow::Return(Ok(match child {
                Ok(answer::YES) => answer::YES,
                Ok(answer::NO) => answer::NO,
                _ => answer::NEITHER,
            })),
            _ => {
                *phase = asking::FLUSHED;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x0C00)))
            }
        }
    }
}

locals! {
    /// One disk formatted, as DOS 4.0's `FORMAT` formats each: an
    /// [`outcome`].
    pub struct FormatDisk {
        /// A bad track was met and passed: the format is past its start.
        passed: bool,
    }
}

mod phase {
    pub const FAKE_SET: u8 = 1;
    pub const WARNED: u8 = 2;
    pub const WARNING_ANSWERED: u8 = 3;
    pub const ACCESSED: u8 = 4;
    pub const ASKED: u8 = 5;
    pub const TYPED: u8 = 6;
    pub const PROMPTED: u8 = 7;
    pub const STATUS: u8 = 8;
    pub const FORMATTED: u8 = 9;
    pub const MARKED: u8 = 10;
    pub const WRITTEN: u8 = 11;
    pub const DPB: u8 = 12;
    pub const SYSTEM: u8 = 13;
    pub const SYSTEM_SAID: u8 = 14;
    pub const SYSTEM_ACCESSED: u8 = 27;
    pub const LINE_SAID: u8 = 15;
    pub const RESET: u8 = 16;
    pub const TABLE_READ: u8 = 17;
    pub const TABLE_WRITTEN: u8 = 18;
    pub const LABELLED: u8 = 19;
    pub const SPACE: u8 = 20;
    pub const REPORTED: u8 = 21;
    pub const FAILED: u8 = 23;
    pub const TERMINATED: u8 = 24;
    pub const FATAL: u8 = 25;
    pub const DONE: u8 = 26;
}

impl FormatDisk {
    /// Says the disk could not be formatted: it is not, and another may be.
    fn failed(p: &mut Program<'_, '_>, phase: &mut u8, words: &[u8], error: bool) -> Flow {
        *phase = phase::FAILED;
        Flow::Call(if error {
            say_error(p, words)
        } else {
            say(p, words)
        })
    }

    /// Says why `FORMAT` must end, and ends it.
    fn fatal(p: &mut Program<'_, '_>, phase: &mut u8, words: &[u8], error: bool) -> Flow {
        p.set8(at::STATUS, exit::FATAL);
        *phase = phase::FATAL;
        Flow::Call(if error {
            say_error(p, words)
        } else {
            say(p, words)
        })
    }

    /// The FAT laid out in memory, every cluster free, and the root as the
    /// disk's directory begins; the volume's numbers taken.
    fn begin(p: &mut Program<'_, '_>) -> Option<Bpb> {
        let bpb = volume_of(p)?;
        let fat = p.get16(at::FAT);
        let zeros = [0u8; 512];
        for n in 0..bpb.fat_sectors {
            let (segment, offset) = place(fat, n * 512);
            p.g.write_bytes(segment, offset, &zeros);
        }
        let head = table::head(bpb.kind, bpb.media);
        let mut first = [0u8; 4];
        table::set(bpb.kind, &mut first, 0, head[0]);
        table::set(bpb.kind, &mut first, 1, head[1]);
        p.g.write_bytes(
            fat,
            0,
            &first[..if bpb.kind == Kind::Fat12 { 3 } else { 4 }],
        );
        let root = p.get16(at::ROOT);
        let mut sector = [0u8; 512];
        if has(p, flag::OLD_DIR) {
            for entry in sector.chunks_mut(32) {
                entry[0] = 0xE5;
            }
        }
        for n in 0..bpb.root_sectors() {
            let (segment, offset) = place(root, n * 512);
            p.g.write_bytes(segment, offset, &sector);
        }
        p.set32(
            at::START,
            u32::try_from(bpb.first_data_sector()).unwrap_or(u32::MAX),
        );
        p.set16(
            at::MAX_CLUSTER,
            u16::try_from(bpb.max_cluster()).unwrap_or(u16::MAX),
        );
        set_flag(p, flag::BIG_FAT, bpb.kind != Kind::Fat12);
        Some(bpb)
    }

    /// The tracks' formatting begun: from the first whole track of the
    /// volume, every track it holds.
    fn first_track(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let Some(bpb) = volume_of(p) else {
            return Self::fatal(p, phase, text::DEVICE_PARAMETERS, false);
        };
        let sectors = u32::from(bpb.sectors_per_track.max(1));
        let heads = u32::from(bpb.heads.max(1));
        let track = bpb.hidden_sectors.div_ceil(sectors);
        p.set16(
            at::FIRST_CYLINDER,
            u16::try_from(track / heads).unwrap_or(u16::MAX),
        );
        p.set16(at::FIRST_HEAD, u16::try_from(track % heads).unwrap_or(0));
        let count = bpb.total_sectors / sectors;
        p.set32(at::TRACK_COUNT, count);
        p.set32(at::LEFT, count);
        p.set16(
            at::CYLINDER,
            u16::try_from(track / heads).unwrap_or(u16::MAX),
        );
        p.set16(at::HEAD, u16::try_from(track % heads).unwrap_or(0));
        p.set32(at::DONE, 0);
        p.set16(at::PERCENT, 0);
        *phase = phase::FORMATTED;
        Flow::Call(Frame::of(&FormatTracks::default()))
    }

    /// The track the format stopped at was bad: told why, or its clusters
    /// marked and the format gone on past it.
    fn stopped(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, code: u16) -> Flow {
        if matches!(code, failed::WRITE_PROTECT | failed::NOT_READY) {
            return Self::failed(p, phase, &device_error(code), true);
        }
        let (cylinder, head) = (p.get16(at::CYLINDER), p.get16(at::HEAD));
        if cylinder == 0 && head == 0 {
            return Self::failed(p, phase, text::UNUSABLE, false);
        }
        if !bad_track(code) {
            return Self::failed(p, phase, b"", false);
        }
        let single_sided = drive_kind(p) == kind::DD525 && cylinder == 0 && head == 1;
        if !self.passed && single_sided && !has(p, flag::LAST_CHANCE) {
            set_flag(p, flag::LAST_CHANCE, true);
            let switches = p.get16(at::ASKED) | switch::ONE;
            if replan(p, switches).is_ok() {
                *phase = 0;
                return self.step(p, phase, Ok(0));
            }
        }
        let first =
            (p.get32(at::TRACK_COUNT) - p.get32(at::LEFT)) * u32::from(sectors_per_track(p));
        if first < p.get32(at::START) {
            return Self::failed(p, phase, text::UNUSABLE, false);
        }
        self.passed = true;
        *phase = phase::MARKED;
        Flow::Call(Frame::of(&FormatBad::of(first)))
    }

    /// The report, from function `36h`'s answer as the buffer keeps it: the
    /// disk's space, the system's, the bad sectors', what is left; then its
    /// allocation units and serial number.
    fn report(p: &mut Program<'_, '_>) -> (Vec<u8>, Vec<u8>) {
        let word = |p: &mut Program<'_, '_>, n: u16| u32::from(p.get16(at::BUFFER + n * 2));
        let (per_cluster, free, sector, clusters) =
            (word(p, 0), word(p, 1), word(p, 2), word(p, 3));
        let cluster = per_cluster * sector;
        let total = clusters.saturating_mul(cluster);
        let system = p.get32(at::SYSTEM_BYTES);
        let bad = clusters
            .saturating_sub(free)
            .saturating_mul(cluster)
            .saturating_sub(system);
        let mut bytes = text::NEW_LINE.to_vec();
        figure_line(&mut bytes, total, &[space::TOTAL]);
        if system != 0 {
            figure_line(&mut bytes, system, &[b" bytes used by system"]);
        }
        if bad != 0 {
            figure_line(&mut bytes, bad, &[space::BAD]);
        }
        figure_line(
            &mut bytes,
            total.saturating_sub(bad).saturating_sub(system),
            &[space::AVAILABLE],
        );
        let mut units = text::NEW_LINE.to_vec();
        figure_line(&mut units, cluster, &[space::UNIT]);
        figure_line(&mut units, free, &[b" allocation units available on disk"]);
        units.extend_from_slice(text::NEW_LINE);
        if !switched(p, switch::EIGHT) {
            let serial = p.get32(at::SERIAL);
            units.extend_from_slice(&message::serial_line(serial));
            units.extend_from_slice(message::NEW_LINE);
        }
        (bytes, units)
    }

    /// The partition's entry in the disk's master boot record given the type
    /// DOS gives a FAT volume like the new one, the record read into the
    /// buffer: `false` for a record holding no entry of the volume, which
    /// is then a logical drive's.
    fn retype(p: &mut Program<'_, '_>) -> bool {
        let Some(bpb) = volume_of(p) else {
            return false;
        };
        let mut record = [0u8; 512];
        p.g.read_bytes(p.own, at::BUFFER, &mut record);
        let Ok(entries) = mbr::entries(&record) else {
            return false;
        };
        let Some(slot) = entries.as_slice().iter().position(|entry| {
            entry.start_lba == bpb.hidden_sectors
                && matches!(
                    entry.type_byte,
                    mbr::PART_TYPE_FAT12 | mbr::PART_TYPE_FAT16_SMALL | mbr::PART_TYPE_FAT16_CHS
                )
        }) else {
            return false;
        };
        let geometry = mbr::Geometry::new(
            u8::try_from(bpb.heads).unwrap_or(0),
            u8::try_from(bpb.sectors_per_track).unwrap_or(0),
        );
        let kind = mbr::fat_type(bpb.kind, bpb.hidden_sectors, bpb.total_sectors, geometry);
        let at = mbr::PARTITION_TABLE_OFFSET + slot * mbr::PARTITION_ENTRY_LEN + 4;
        p.set8(at::BUFFER + u16::try_from(at).unwrap_or(0), kind);
        true
    }
}

/// The device type the drive's parameters name.
fn drive_kind(p: &mut Program<'_, '_>) -> u8 {
    p.get8(at::SAVED + parameters::KIND)
}

fn sectors_per_track(p: &mut Program<'_, '_>) -> u16 {
    volume_of(p).map_or(1, |bpb| bpb.sectors_per_track.max(1))
}

impl Routine for FormatDisk {
    const ID: Id = Id::FormatDisk;

    #[allow(
        clippy::too_many_lines,
        reason = "one arm a phase, in the order DOS's FORMAT formats a disk"
    )]
    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            phase::FAKE_SET => {
                if child.is_err() {
                    return Self::fatal(p, phase, text::BAD_PARAMETER, true);
                }
                if has(p, flag::LAST_CHANCE) {
                    *phase = phase::PROMPTED;
                    return self.step(p, phase, Ok(0));
                }
                if has(p, flag::FIXED) {
                    let drive = letter(p.get8(at::DRIVE));
                    *phase = phase::WARNED;
                    return Flow::Call(say(p, &[text::WARNING, &[drive], text::PROCEED].concat()));
                }
                *phase = phase::ACCESSED;
                let drive = p.get8(at::DRIVE);
                Flow::Call(Frame::of(&Int {
                    bx: u16::from(drive) + 1,
                    ..Int::dos(p.own, 0x440F)
                }))
            }
            phase::WARNED => {
                take!(child);
                *phase = phase::WARNING_ANSWERED;
                Flow::Call(Frame::of(&FormatAsk::yes_no()))
            }
            phase::WARNING_ANSWERED => {
                if take!(child) != answer::YES {
                    p.set8(at::STATUS, exit::DECLINED);
                    return Flow::Return(Ok(outcome::FATAL));
                }
                *phase = phase::PROMPTED;
                Flow::Call(say(p, b"\r\n\r\n"))
            }
            phase::ACCESSED => {
                take!(child);
                let drive = letter(p.get8(at::DRIVE));
                *phase = phase::ASKED;
                Flow::Call(say(
                    p,
                    &[text::INSERT, &[drive, b':'], b"\r\n", text::ENTER].concat(),
                ))
            }
            phase::ASKED => {
                take!(child);
                *phase = phase::TYPED;
                Flow::Call(Frame::of(&FormatAsk::line()))
            }
            phase::TYPED => {
                take!(child);
                *phase = phase::PROMPTED;
                Flow::Call(say(p, b"\r\n\r\n"))
            }
            phase::PROMPTED => {
                take!(child);
                if switched(p, switch::EIGHT) {
                    return Self::first_track(p, phase);
                }
                p.g.write_bytes(p.own, at::PACKET, &[track::STATUS, 0, 0, 0, 0, 0, 0]);
                *phase = phase::STATUS;
                Flow::Call(ioctl(p, generic::FORMAT_TRACK, at::PACKET))
            }
            phase::STATUS => {
                // A driver that cannot be asked is taken for a BIOS that cannot.
                let status = if child.is_ok() {
                    p.get8(at::PACKET + track::SPECIAL)
                } else {
                    track::status::NO_ROM
                };
                match status {
                    track::status::SUPPORTED => Self::first_track(p, phase),
                    track::status::UNKNOWN_MEDIUM => {
                        Self::fatal(p, phase, text::BAD_PARAMETER, true)
                    }
                    track::status::EMPTY => {
                        Self::failed(p, phase, &device_error(failed::NOT_READY), true)
                    }
                    _ if switched(p, switch::N | switch::T) => {
                        Self::fatal(p, phase, text::UNSUPPORTED, false)
                    }
                    _ => Self::first_track(p, phase),
                }
            }
            phase::FORMATTED => match u16::try_from(take!(child)).unwrap_or(u16::MAX) {
                0 => {
                    set_flag(p, flag::WRITING, true);
                    *phase = phase::WRITTEN;
                    Flow::Call(Frame::of(&FormatWrite::default()))
                }
                code => self.stopped(p, phase, code),
            },
            phase::MARKED => {
                take!(child);
                let (cylinder, head) = (p.get16(at::CYLINDER), p.get16(at::HEAD));
                let heads = volume_of(p).map_or(1, |bpb| bpb.heads.max(1));
                let (cylinder, head) = if head + 1 >= heads {
                    (cylinder.saturating_add(1), 0)
                } else {
                    (cylinder, head + 1)
                };
                p.set16(at::CYLINDER, cylinder);
                p.set16(at::HEAD, head);
                let left = p.get32(at::LEFT).saturating_sub(1);
                p.set32(at::LEFT, left);
                *phase = phase::FORMATTED;
                Flow::Call(Frame::of(&FormatTracks::default()))
            }
            phase::WRITTEN => {
                set_flag(p, flag::WRITING, false);
                match take!(child) {
                    super::write::written::OK => {
                        *phase = phase::DPB;
                        let drive = p.get8(at::DRIVE);
                        Flow::Call(Frame::of(&Int {
                            dx: u16::from(drive) + 1,
                            ..Int::dos(p.own, 0x3200)
                        }))
                    }
                    super::write::written::BOOT => Self::failed(p, phase, text::UNUSABLE, false),
                    _ => {
                        p.set8(at::STATUS, exit::FATAL);
                        Flow::Return(Ok(outcome::FATAL))
                    }
                }
            }
            phase::DPB => {
                if take!(child).to_le_bytes()[0] == 0xFF {
                    return Self::failed(p, phase, text::UNUSABLE, false);
                }
                p.set32(at::SYSTEM_BYTES, 0);
                if !switched(p, switch::S) {
                    *phase = phase::LINE_SAID;
                    return Flow::Call(say(p, text::NEW_LINE));
                }
                let drive = p.get8(at::DRIVE);
                *phase = phase::SYSTEM_ACCESSED;
                Flow::Call(Frame::of(&Int {
                    bx: u16::from(drive) + 1,
                    ..Int::dos(p.own, 0x440F)
                }))
            }
            phase::SYSTEM_ACCESSED => {
                take!(child);
                *phase = phase::SYSTEM;
                Flow::Call(Frame::of(&FormatSystem::default()))
            }
            phase::SYSTEM => match child {
                Ok(_) => {
                    *phase = phase::SYSTEM_SAID;
                    Flow::Call(say(p, text::TRANSFERRED))
                }
                Err(Fault::Break) => Flow::Return(Err(Fault::Break)),
                Err(_) => {
                    p.set32(at::SYSTEM_BYTES, 0);
                    *phase = phase::SYSTEM_SAID;
                    Flow::Call(say(p, text::NOT_SYSTEM))
                }
            },
            phase::SYSTEM_SAID => {
                take!(child);
                *phase = phase::LINE_SAID;
                Flow::Call(say(p, text::NEW_LINE))
            }
            phase::LINE_SAID => {
                take!(child);
                *phase = phase::RESET;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x0D00)))
            }
            phase::RESET => {
                take!(child);
                if has(p, flag::FIXED) {
                    track_request(p, 0, 0, 0, 1);
                    *phase = phase::TABLE_READ;
                    return Flow::Call(ioctl(p, generic::READ_TRACK, at::PACKET));
                }
                self.label(p, phase)
            }
            phase::TABLE_READ => {
                if child.is_err() {
                    return Self::failed(p, phase, text::TABLE_READ, false);
                }
                if !signed(p) {
                    return Self::failed(p, phase, text::BAD_TABLE, false);
                }
                if !Self::retype(p) {
                    return self.label(p, phase);
                }
                track_request(p, 0, 0, 0, 1);
                *phase = phase::TABLE_WRITTEN;
                Flow::Call(ioctl(p, generic::WRITE_TRACK, at::PACKET))
            }
            phase::TABLE_WRITTEN => {
                if child.is_err() {
                    return Self::failed(p, phase, text::TABLE_WRITE, false);
                }
                self.label(p, phase)
            }
            phase::LABELLED => {
                take!(child);
                *phase = phase::SPACE;
                let drive = p.get8(at::DRIVE);
                Flow::Call(Frame::of(&Int {
                    dx: u16::from(drive) + 1,
                    ..Int::dos(p.own, 0x3600)
                }))
            }
            phase::SPACE => {
                take!(child);
                for (n, register) in [Returned::Ax, Returned::Bx, Returned::Cx, Returned::Dx]
                    .into_iter()
                    .enumerate()
                {
                    let value = p.result(register);
                    p.set16(at::BUFFER + u16::try_from(n * 2).unwrap_or(0), value);
                }
                let (space, _) = Self::report(p);
                *phase = phase::REPORTED;
                Flow::Call(say(p, &space))
            }
            phase::REPORTED => {
                take!(child);
                let (_, units) = Self::report(p);
                *phase = phase::DONE;
                Flow::Call(say(p, &units))
            }
            phase::FAILED => {
                take!(child);
                *phase = phase::TERMINATED;
                Flow::Call(say(
                    p,
                    &over_progress(text::TERMINATED, PROGRESS_WIDTH, b"\r\n"),
                ))
            }
            phase::TERMINATED => {
                take!(child);
                Flow::Return(Ok(outcome::FAILED))
            }
            phase::FATAL => {
                take!(child);
                Flow::Return(Ok(outcome::FATAL))
            }
            phase::DONE => {
                take!(child);
                Flow::Return(Ok(outcome::DONE))
            }
            _ => {
                if Self::begin(p).is_none() {
                    return Self::fatal(p, phase, text::DEVICE_PARAMETERS, false);
                }
                *phase = phase::FAKE_SET;
                Flow::Call(set_parameters(
                    p,
                    at::PARAMS,
                    parameters::FAKE_BPB | parameters::GOOD_LAYOUT,
                ))
            }
        }
    }
}

impl FormatDisk {
    /// The label made, but for a DOS 1 disk, which has none.
    fn label(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        *phase = phase::LABELLED;
        if switched(p, switch::EIGHT) {
            return self.step(p, phase, Ok(0));
        }
        Flow::Call(Frame::of(&FormatLabel {}))
    }
}

locals! {
    /// The new volume's label: the one `/V` gave, or one typed, made as
    /// its root's label entry once any other is deleted. None typed, none
    /// is made.
    pub struct FormatLabel {}
}

mod labelling {
    pub const ASKED: u8 = 1;
    pub const TYPED: u8 = 2;
    pub const REFUSED: u8 = 3;
    pub const SAID: u8 = 4;
    pub const DELETED: u8 = 5;
    pub const CREATED: u8 = 6;
    pub const CLOSED: u8 = 7;
}

impl FormatLabel {
    fn ask(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        *phase = labelling::ASKED;
        Flow::Call(say(p, text::ASK_LABEL))
    }

    /// The label's FCB: an extended one of the label attribute, on the
    /// drive formatted, named `name`.
    fn fcb(p: &mut Program<'_, '_>, name: &[u8; 11]) {
        let mut fcb = [0u8; 0x2C];
        fcb[0] = 0xFF;
        fcb[6] = tairix_fat::dir::attr::VOLUME_ID;
        fcb[7] = p.get8(at::DRIVE) + 1;
        fcb[8..19].copy_from_slice(name);
        p.g.write_bytes(p.own, at::FCB, &fcb);
    }

    fn make(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let mut name = [b' '; 11];
        p.g.read_bytes(p.own, at::LABEL, &mut name);
        if name == [b' '; 11] {
            return Flow::Return(Ok(0));
        }
        Self::fcb(p, &[b'?'; 11]);
        *phase = labelling::DELETED;
        Flow::Call(Frame::of(&Int {
            dx: at::FCB,
            ..Int::dos(p.own, 0x1300)
        }))
    }
}

impl Routine for FormatLabel {
    const ID: Id = Id::FormatLabel;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            labelling::ASKED => {
                take!(child);
                *phase = labelling::TYPED;
                Flow::Call(Frame::of(&FormatAsk::line()))
            }
            labelling::TYPED => {
                take!(child);
                let typed = typed(p);
                let start = typed
                    .iter()
                    .position(|&b| !matches!(b, b' ' | b'\t'))
                    .unwrap_or(typed.len());
                let given = &typed[start..];
                let field = if given.is_empty() {
                    Some([b' '; 11])
                } else {
                    label::field(&given.to_ascii_uppercase())
                };
                let Some(field) = field else {
                    *phase = labelling::REFUSED;
                    return Flow::Call(say(
                        p,
                        &[
                            text::NEW_LINE,
                            &over_progress(text::BAD_LABEL, 44, b"\r\n")[..],
                        ]
                        .concat(),
                    ));
                };
                p.g.write_bytes(p.own, at::LABEL, &field);
                *phase = labelling::SAID;
                Flow::Call(say(p, text::NEW_LINE))
            }
            labelling::REFUSED => {
                take!(child);
                Self::ask(p, phase)
            }
            labelling::SAID => {
                take!(child);
                Self::make(p, phase)
            }
            labelling::DELETED => {
                let mut name = [b' '; 11];
                p.g.read_bytes(p.own, at::LABEL, &mut name);
                Self::fcb(p, &name);
                *phase = labelling::CREATED;
                Flow::Call(Frame::of(&Int {
                    dx: at::FCB,
                    ..Int::dos(p.own, 0x1600)
                }))
            }
            labelling::CREATED => {
                if take!(child).to_le_bytes()[0] != 0 {
                    return Flow::Return(Ok(0));
                }
                *phase = labelling::CLOSED;
                Flow::Call(Frame::of(&Int {
                    dx: at::FCB,
                    ..Int::dos(p.own, 0x1000)
                }))
            }
            labelling::CLOSED => {
                take!(child);
                Flow::Return(Ok(0))
            }
            _ => {
                if has(p, flag::LABELLED) {
                    return Self::make(p, phase);
                }
                Self::ask(p, phase)
            }
        }
    }
}
