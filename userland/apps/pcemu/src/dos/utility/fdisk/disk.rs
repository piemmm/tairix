//! The fixed disks as `FDISK` reaches them, through INT 13h as DOS 4.0's
//! does: each disk's shape and tables read in, its extended partition's
//! chain followed, each logical drive's label and file system asked of DOS;
//! the changes written back; and the free run a partition is made in found,
//! the tracks its system area takes verified.
//!
//! DOS 4.0's `FDISK` tests a run by writing over its tracks; verifying them
//! finds the same bad tracks and writes nothing before the user has asked
//! for anything.

use super::table::{
    system_tracks, Disk, Extent, Logical, Shape, LABEL_LEN, MAX_DISKS, SYSTEM_LEN, UNFORMATTED,
};
use super::views::lettered;
use super::{at, disks, keep, load, set_run};
use crate::dos::device::generic::{self, access, media};
use crate::dos::formats::{is_dos, FAT12_MOST};
use crate::dos::ioctl::drive_bits;
use crate::dos::program::{Flow, Frame, Id, Int, Program, Returned, Routine};
use crate::dos::shell::found_label;
use crate::dos::utility::{carried as failed, disk_ioctl};
use crate::frames::{locals, Outcome};
use alloc::vec::Vec;
use tairix_fat::boot::Kind;
use tairix_fat::format::FIXED_DRIVE;
use tairix_partition::mbr::MBR_SECTOR_LEN;

/// The INT 13h functions used.
mod bios {
    pub const READ: u8 = 0x02;
    pub const WRITE: u8 = 0x03;
    pub const VERIFY: u8 = 0x04;
    pub const PARAMETERS: u16 = 0x0800;
}

/// Tries at verifying a track before it counts as bad, as DOS 4.0's.
const VERIFY_TRIES: u8 = 5;
/// What a drive on the network is listed with.
const REMOTE_LABEL: &[u8; LABEL_LEN] = b"* Remote * ";

/// The INT 13h call making `function` of `count` sectors of disk `disk`
/// from sector `lba`, through [`at::SECTOR`].
fn sector_call(
    p: &Program<'_, '_>,
    function: u8,
    count: u8,
    disk: u8,
    shape: Shape,
    lba: u32,
) -> Option<Frame> {
    let [head, sector, cylinder] = shape.geometry()?.chs(lba);
    Some(Frame::of(&Int {
        bx: at::SECTOR,
        cx: u16::from_le_bytes([sector, cylinder]),
        dx: u16::from_le_bytes([FIXED_DRIVE | disk, head]),
        ..Int::bios(p.own, 0x13, u16::from_le_bytes([count, function]))
    }))
}

/// The sector [`at::SECTOR`] holds.
fn sector(p: &mut Program<'_, '_>) -> [u8; MBR_SECTOR_LEN] {
    let mut out = [0u8; MBR_SECTOR_LEN];
    p.g.read_bytes(p.own, at::SECTOR, &mut out);
    out
}

locals! {
    /// Every fixed disk's shape and tables read into the tables' memory,
    /// their count into [`at::DISKS`], none where function `08h` finds no
    /// disk. A disk whose master boot record or chain cannot be read is
    /// left unreadable. Returns the disks.
    pub struct FdiskRead {
        disk: u8,
        count: u8,
        next: u32,
        drive: u8,
    }
}

mod reading {
    pub const COUNTED: u8 = 1;
    pub const SHAPED: u8 = 2;
    pub const RECORD_READ: u8 = 3;
    pub const CHAIN_READ: u8 = 4;
    pub const ACCESS: u8 = 5;
    pub const DTA_SET: u8 = 6;
    pub const FOUND: u8 = 7;
    pub const MEDIA: u8 = 8;
    pub const REMOTE: u8 = 9;
}

impl FdiskRead {
    /// The next disk's shape asked for, or every disk's drives' labels.
    fn next_disk(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if self.disk >= self.count {
            self.disk = 0;
            self.drive = 0;
            return self.next_drive(p, phase);
        }
        *phase = reading::SHAPED;
        Flow::Call(Frame::of(&Int {
            dx: u16::from(FIXED_DRIVE | self.disk),
            ..Int::bios(p.own, 0x13, bios::PARAMETERS)
        }))
    }

    fn done_with_disk(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        self.disk += 1;
        self.next_disk(p, phase)
    }

    /// The next record of the chain read.
    fn read_record(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, shape: Shape) -> Flow {
        *phase = reading::CHAIN_READ;
        match sector_call(p, bios::READ, 1, self.disk, shape, self.next) {
            Some(call) => Flow::Call(call),
            None => self.done_with_disk(p, phase),
        }
    }

    /// The letter of the logical drive being asked about, if one is left:
    /// the disks' DOS drives in the order DOS letters them.
    fn letter(&mut self, p: &mut Program<'_, '_>) -> Option<u8> {
        let disks = disks(p);
        while usize::from(self.disk) < disks.len() {
            let listed = lettered(&disks, usize::from(self.disk));
            if let Some(&(letter, _)) = listed.get(usize::from(self.drive)) {
                return Some(letter).filter(|&letter| letter != b' ');
            }
            self.disk += 1;
            self.drive = 0;
        }
        None
    }

    /// Where the drive being asked about lies in its disk's drives.
    fn drive_index(&mut self, p: &mut Program<'_, '_>) -> Option<usize> {
        let disks = disks(p);
        let disk = disks.get(usize::from(self.disk))?;
        disk.logical
            .iter()
            .enumerate()
            .filter(|(_, drive)| !drive.deleted && is_dos(drive.type_byte()))
            .nth(usize::from(self.drive))
            .map(|(at, _)| at)
    }

    /// `change` made to the drive being asked about.
    fn update(&mut self, p: &mut Program<'_, '_>, change: impl FnOnce(&mut Logical, u32)) {
        let Some(index) = self.drive_index(p) else {
            return;
        };
        let mut disk = load(p, self.disk);
        let per_cylinder = disk.shape.per_cylinder();
        if let Some(drive) = disk.logical.get_mut(index) {
            let sectors = u32::from(drive.extent().cylinders()) * per_cylinder;
            change(drive, sectors);
        }
        keep(p, self.disk, &disk);
    }

    /// The next logical drive asked whether it is formatted, or, with none
    /// left, the reading done.
    fn next_drive(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let Some(letter) = self.letter(p) else {
            return Flow::Return(Ok(u32::from(self.count)));
        };
        p.g.write_bytes(p.own, at::PACKET, &[0; access::LEN as usize]);
        *phase = reading::ACCESS;
        Flow::Call(disk_ioctl(
            p,
            letter - b'A',
            generic::GET_ACCESS,
            at::PACKET,
        ))
    }

    /// The drive asked whether it is on the network.
    fn remote(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let letter = self.letter(p).unwrap_or(b'C');
        *phase = reading::REMOTE;
        Flow::Call(Frame::of(&Int {
            bx: u16::from(letter - b'A' + 1),
            ..Int::dos(p.own, 0x4409)
        }))
    }
}

impl FdiskRead {
    /// The disks' shapes and tables read.
    fn read_tables(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            reading::COUNTED => {
                let count = p.result(Returned::Dx).to_le_bytes()[0];
                self.count = if child.is_err() || failed(p) {
                    0
                } else {
                    count.min(MAX_DISKS)
                };
                p.set8(at::DISKS, self.count);
                for disk in 0..self.count {
                    keep(p, disk, &Disk::default());
                }
                if self.count == 0 {
                    return Flow::Return(Ok(0));
                }
                self.next_disk(p, phase)
            }
            reading::SHAPED => {
                let shape = Shape::reported(
                    p.result(Returned::Cx),
                    p.result(Returned::Dx).to_le_bytes()[1],
                );
                if child.is_err() || failed(p) || !shape.usable() {
                    return self.done_with_disk(p, phase);
                }
                keep(
                    p,
                    self.disk,
                    &Disk {
                        shape,
                        ..Disk::default()
                    },
                );
                *phase = reading::RECORD_READ;
                match sector_call(p, bios::READ, 1, self.disk, shape, 0) {
                    Some(call) => Flow::Call(call),
                    None => self.done_with_disk(p, phase),
                }
            }
            reading::RECORD_READ => {
                let shape = load(p, self.disk).shape;
                if child.is_err() || failed(p) {
                    return self.done_with_disk(p, phase);
                }
                let base = crate::dos::boot::master_boot_record().unwrap_or([0; MBR_SECTOR_LEN]);
                let disk = Disk::read(shape, &sector(p), &base);
                let extended = disk.extended();
                keep(p, self.disk, &disk);
                match extended {
                    Some(extended) => {
                        self.next = shape.sector_of(extended.start);
                        self.read_record(p, phase, shape)
                    }
                    None => self.done_with_disk(p, phase),
                }
            }
            reading::CHAIN_READ => {
                let mut disk = load(p, self.disk);
                if child.is_err() || failed(p) {
                    disk.readable = false;
                    disk.logical.clear();
                    keep(p, self.disk, &disk);
                    return self.done_with_disk(p, phase);
                }
                let next = disk.take_record(self.next, &sector(p));
                let shape = disk.shape;
                keep(p, self.disk, &disk);
                match next {
                    Some(next) => {
                        self.next = next;
                        self.read_record(p, phase, shape)
                    }
                    None => self.done_with_disk(p, phase),
                }
            }
            _ => {
                *phase = reading::COUNTED;
                Flow::Call(Frame::of(&Int {
                    dx: u16::from(FIXED_DRIVE),
                    ..Int::bios(p.own, 0x13, bios::PARAMETERS)
                }))
            }
        }
    }

    /// The logical drives asked of DOS: formatted or not, labelled how, of
    /// which file system, on the network or not.
    fn ask_drives(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            reading::ACCESS => {
                let formatted = child.is_ok() && p.get8(at::PACKET + access::FLAG) != 0;
                if !formatted {
                    self.update(p, |drive, _| {
                        drive.label = [0; LABEL_LEN];
                        drive.system = UNFORMATTED;
                    });
                    return self.remote(p, phase);
                }
                *phase = reading::DTA_SET;
                Flow::Call(Frame::of(&Int {
                    dx: at::DTA,
                    ..Int::dos(p.own, 0x1A00)
                }))
            }
            reading::DTA_SET => {
                let letter = self.letter(p).unwrap_or(b'C');
                p.g.write_bytes(p.own, at::PATTERN, &[letter, b':', b'\\']);
                p.g.write_bytes(p.own, at::PATTERN + 3, b"????????.???\0");
                *phase = reading::FOUND;
                Flow::Call(Frame::of(&Int {
                    cx: u16::from(tairix_fat::dir::attr::VOLUME_ID),
                    dx: at::PATTERN,
                    ..Int::dos(p.own, 0x4E00)
                }))
            }
            reading::FOUND => {
                let found = if child.is_ok() {
                    found_label(p, at::DTA)
                } else {
                    Vec::new()
                };
                self.update(p, |drive, _| {
                    drive.label = [0; LABEL_LEN];
                    for (slot, &byte) in drive.label.iter_mut().zip(&found) {
                        *slot = byte;
                    }
                });
                p.g.write_bytes(p.own, at::PACKET, &[0; media::LEN as usize]);
                *phase = reading::MEDIA;
                let letter = self.letter(p).unwrap_or(b'C');
                Flow::Call(disk_ioctl(
                    p,
                    letter - b'A',
                    generic::GET_MEDIA_ID,
                    at::PACKET,
                ))
            }
            reading::MEDIA => {
                let mut named = [0u8; SYSTEM_LEN];
                let read = child.is_ok();
                if read {
                    p.g.read_bytes(p.own, at::PACKET + media::FILE_SYSTEM, &mut named);
                }
                self.update(p, |drive, sectors| {
                    drive.system = if read {
                        named
                    } else if sectors > FAT12_MOST {
                        *Kind::Fat16.type_name()
                    } else {
                        *Kind::Fat12.type_name()
                    };
                });
                self.remote(p, phase)
            }
            _ => {
                if child.is_ok() && p.result(Returned::Dx) & drive_bits::REMOTE != 0 {
                    self.update(p, |drive, _| drive.label = *REMOTE_LABEL);
                }
                self.drive += 1;
                self.next_drive(p, phase)
            }
        }
    }
}

impl Routine for FdiskRead {
    const ID: Id = Id::FdiskRead;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if *phase >= reading::ACCESS {
            self.ask_drives(p, phase, child)
        } else {
            self.read_tables(p, phase, child)
        }
    }
}

locals! {
    /// Every disk's changes written: its master boot record, its chain and
    /// the first sector of each partition made. Returns `0`, or `1` once a
    /// write failed, leaving the rest unwritten.
    pub struct FdiskWrite {
        disk: u8,
        index: u8,
    }
}

mod writing {
    pub const WRITTEN: u8 = 1;
}

impl Routine for FdiskWrite {
    const ID: Id = Id::FdiskWrite;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if *phase == writing::WRITTEN {
            if child.is_err() || failed(p) {
                return Flow::Return(Ok(1));
            }
            self.index += 1;
        }
        let count = p.get8(at::DISKS);
        while self.disk < count {
            let disk = load(p, self.disk);
            let writes = if disk.readable {
                disk.writes()
            } else {
                Vec::new()
            };
            if let Some(&(lba, bytes)) = writes.get(usize::from(self.index)) {
                p.g.write_bytes(p.own, at::SECTOR, &bytes);
                *phase = writing::WRITTEN;
                return match sector_call(p, bios::WRITE, 1, self.disk, disk.shape, lba) {
                    Some(call) => Flow::Call(call),
                    None => Flow::Return(Ok(1)),
                };
            }
            self.disk += 1;
            self.index = 0;
        }
        Flow::Return(Ok(0))
    }
}

/// What a free run is looked for to hold.
pub mod run_for {
    pub const PRIMARY: u8 = 0;
    pub const EXTENDED: u8 = 1;
    pub const LOGICAL: u8 = 2;
}

locals! {
    /// The free run a partition of kind `kind` is made in on the current
    /// disk found, as DOS 4.0's search finds it: the largest, its system
    /// area's tracks verified, a run met by a track that will not verify
    /// taken past it and the largest looked for again. Leaves it in
    /// [`at::RUN`], none where no run will do.
    pub struct FdiskSpace {
        kind: u8,
        disk: u8,
        start: u16,
        end: u16,
        track: u16,
        tracks: u16,
        tries: u8,
    }
}

impl FdiskSpace {
    #[must_use]
    pub const fn of(kind: u8, disk: u8) -> Self {
        Self {
            kind,
            disk,
            start: 0,
            end: 0,
            track: 0,
            tracks: 0,
            tries: 0,
        }
    }
}

mod searching {
    pub const VERIFIED: u8 = 1;
}

impl FdiskSpace {
    /// The cylinders found bad so far.
    fn bad(p: &mut Program<'_, '_>) -> Vec<Extent> {
        (0..p.get8(at::BAD_COUNT).min(at::BAD_MAX))
            .map(|n| {
                let at = at::BAD + u16::from(n) * 4;
                Extent {
                    start: p.get16(at),
                    end: p.get16(at + 2),
                }
            })
            .collect()
    }

    /// The largest run found and its verifying begun, or none left.
    fn search(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let disk = load(p, self.disk);
        let bad = Self::bad(p);
        let run = match self.kind {
            run_for::PRIMARY => disk.free_space(false, &bad),
            run_for::EXTENDED => disk.free_space(true, &bad),
            _ => disk.logical_space(&bad),
        };
        let Some(run) = run else {
            set_run(p, None);
            return Flow::Return(Ok(0));
        };
        (self.start, self.end) = (run.start, run.end);
        let tracks = system_tracks(
            disk.shape,
            run,
            self.kind == run_for::PRIMARY && self.disk == 0,
        );
        self.tracks = u16::try_from(tracks).unwrap_or(u16::MAX);
        self.track = 0;
        self.tries = 0;
        self.verify(p, phase, disk.shape)
    }

    /// The next track verified, or the run found good.
    fn verify(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, shape: Shape) -> Flow {
        if self.track >= self.tracks {
            set_run(
                p,
                Some(Extent {
                    start: self.start,
                    end: self.end,
                }),
            );
            return Flow::Return(Ok(1));
        }
        let heads = u32::from(shape.heads.max(1));
        let first_head = u32::from(self.start == 0);
        let track = u32::from(self.start) * heads + first_head + u32::from(self.track);
        let cylinder = u16::try_from(track / heads).unwrap_or(u16::MAX);
        if cylinder > self.end {
            return self.left_out(p, phase, self.end);
        }
        let lba = track * u32::from(shape.sectors);
        *phase = searching::VERIFIED;
        match sector_call(p, bios::VERIFY, shape.sectors, self.disk, shape, lba) {
            Some(call) => Flow::Call(call),
            None => self.left_out(p, phase, self.end),
        }
    }

    /// The run from its start to cylinder `last` left out, and the largest
    /// looked for again; a search that has left out as much as it keeps
    /// finds nothing more.
    fn left_out(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, last: u16) -> Flow {
        let count = p.get8(at::BAD_COUNT);
        if count >= at::BAD_MAX {
            set_run(p, None);
            return Flow::Return(Ok(0));
        }
        let at = at::BAD + u16::from(count) * 4;
        p.set16(at, self.start);
        p.set16(at + 2, last);
        p.set8(at::BAD_COUNT, count + 1);
        self.search(p, phase)
    }
}

impl Routine for FdiskSpace {
    const ID: Id = Id::FdiskSpace;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if *phase != searching::VERIFIED {
            p.set8(at::BAD_COUNT, 0);
            return self.search(p, phase);
        }
        let shape = load(p, self.disk).shape;
        if child.is_ok() && !failed(p) {
            self.track += 1;
            self.tries = 0;
            return self.verify(p, phase, shape);
        }
        self.tries += 1;
        if self.tries < VERIFY_TRIES {
            return self.verify(p, phase, shape);
        }
        let heads = u32::from(shape.heads.max(1));
        let track =
            u32::from(self.start) * heads + u32::from(self.start == 0) + u32::from(self.track);
        let cylinder = u16::try_from(track / heads).unwrap_or(u16::MAX);
        self.left_out(p, phase, cylinder)
    }
}
