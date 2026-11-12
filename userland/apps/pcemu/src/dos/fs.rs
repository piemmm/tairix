//! The block layer the file system stands on: each drive's DPB, the disk
//! buffers, transfers through a drive's driver with DOS's INT 24h handling
//! of a failed one, the media check before a removable drive is used, and
//! the FAT's entries read and written through the buffers.

use super::data::{
    buffer, cds, dpb, driver, level, BUFFER, CDS, CDS_ENTRY, DPBS, DPB_SIZE, DRIVES, SECTOR_MAX,
    SEGMENT,
};
use super::device::{self, command, field, status, Device};
use super::engine::{locals, take, Far, Fault, Flow, Frame, Id, Kernel, Outcome, Routine};
use crate::asm::{Reg16, Reg8};
use tairix_fat::boot::{Kind, DOS_BPB};
use tairix_fat::name::{label_field, NO_LABEL};
use tairix_fat::table::{self, Link};

/// Extended errors the file system raises itself.
pub mod error {
    #![allow(missing_docs, reason = "the errors' own names")]
    pub const INVALID_FUNCTION: u16 = 0x01;
    pub const FILE_NOT_FOUND: u16 = 0x02;
    pub const PATH_NOT_FOUND: u16 = 0x03;
    pub const TOO_MANY_OPEN: u16 = 0x04;
    pub const ACCESS_DENIED: u16 = 0x05;
    pub const INVALID_HANDLE: u16 = 0x06;
    pub const ARENA_TRASHED: u16 = 0x07;
    pub const NO_MEMORY: u16 = 0x08;
    pub const BAD_BLOCK: u16 = 0x09;
    pub const BAD_ENVIRONMENT: u16 = 0x0A;
    pub const BAD_FORMAT: u16 = 0x0B;
    pub const INVALID_ACCESS: u16 = 0x0C;
    pub const INVALID_DATA: u16 = 0x0D;
    pub const INVALID_DRIVE: u16 = 0x0F;
    pub const CURRENT_DIRECTORY: u16 = 0x10;
    pub const NOT_SAME_DEVICE: u16 = 0x11;
    pub const NO_MORE_FILES: u16 = 0x12;
    pub const DISK_CHANGE: u16 = 0x22;
    /// A shared folder's server failed.
    pub const NETWORK_ERROR: u16 = 0x3B;
    /// The shared folder a drive or a file was is no longer shared.
    pub const NAME_DELETED: u16 = 0x40;
    pub const FILE_EXISTS: u16 = 0x50;
    pub const CANNOT_MAKE: u16 = 0x52;
    pub const FAIL_ON_24: u16 = 0x53;
    pub const INVALID_PARAMETER: u16 = 0x57;
}

/// A drive's parameters, as its DPB holds them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dpb {
    /// Where it lies in the kernel's segment.
    pub at: u16,
    /// The drive, `0` for A:.
    pub drive: u8,
    /// Its unit in its driver.
    pub unit: u8,
    /// Bytes per sector.
    pub sector_bytes: u16,
    /// Sectors per cluster, less one.
    pub cluster_mask: u8,
    /// The shift from clusters to sectors.
    pub cluster_shift: u8,
    /// Reserved sectors.
    pub reserved: u16,
    /// Copies of the FAT.
    pub fats: u8,
    /// Root directory entries.
    pub root_entries: u16,
    /// The first data sector.
    pub data: u16,
    /// The highest cluster.
    pub max_cluster: u16,
    /// Sectors of one FAT.
    pub fat_sectors: u16,
    /// The root directory's first sector.
    pub root: u16,
    /// The driver's header.
    pub driver: Far,
    /// The media descriptor.
    pub media: u8,
}

impl Dpb {
    /// The FAT's kind, by how many clusters it has.
    #[must_use]
    pub fn kind(&self) -> Kind {
        kind_of(self.max_cluster)
    }

    /// Sectors a cluster.
    #[must_use]
    pub fn cluster_sectors(&self) -> u16 {
        u16::from(self.cluster_mask) + 1
    }

    /// Bytes a cluster.
    #[must_use]
    pub fn cluster_bytes(&self) -> u32 {
        u32::from(self.cluster_sectors()) * u32::from(self.sector_bytes)
    }

    /// Whether `cluster` is one of the volume's data clusters.
    #[must_use]
    pub fn in_volume(&self, cluster: u16) -> bool {
        (2..=self.max_cluster).contains(&cluster)
    }

    /// The first sector of data cluster `cluster`.
    #[must_use]
    pub fn cluster_sector(&self, cluster: u16) -> u32 {
        u32::from(self.data) + (u32::from(cluster.saturating_sub(2)) << self.cluster_shift)
    }

    /// What an entry `value` read from the FAT says of its cluster.
    #[must_use]
    pub fn link(&self, value: u32) -> Link {
        table::classify(self.kind(), value, u32::from(self.max_cluster))
    }

    /// The value a chain's last entry is written with.
    #[must_use]
    pub fn end(&self) -> u32 {
        table::end(self.kind())
    }
}

/// The DPB a FAT12 or FAT16 volume's BPB `bpb` gives, its drive, unit,
/// driver and link left zero: the medium not yet checked, its free count
/// unknown.
#[must_use]
pub fn dpb_fields(bpb: &[u8; DOS_BPB]) -> Option<[u8; DPB_SIZE as usize]> {
    let volume = tairix_fat::boot::Bpb::from_dos(bpb)?;
    if volume.kind == Kind::Fat32 {
        return None;
    }
    let word = |value: u64| u16::try_from(value).unwrap_or(u16::MAX).to_le_bytes();
    let mut out = [0u8; DPB_SIZE as usize];
    let mut put = |field: u16, bytes: &[u8]| {
        let at = usize::from(field);
        out[at..at + bytes.len()].copy_from_slice(bytes);
    };
    put(dpb::SECTOR, &volume.bytes_per_sector.to_le_bytes());
    put(
        dpb::CLUSTER_MASK,
        &[volume.sectors_per_cluster.wrapping_sub(1)],
    );
    put(
        dpb::CLUSTER_SHIFT,
        &[u8::try_from(volume.sectors_per_cluster.trailing_zeros()).unwrap_or(0)],
    );
    put(dpb::RESERVED, &volume.reserved_sectors.to_le_bytes());
    put(dpb::FATS, &[volume.fats]);
    put(dpb::ROOT_ENTRIES, &volume.root_entries.to_le_bytes());
    put(dpb::DATA, &word(volume.first_data_sector()));
    put(dpb::MAX_CLUSTER, &word(u64::from(volume.max_cluster())));
    put(dpb::FAT_SECTORS, &word(u64::from(volume.fat_sectors)));
    put(
        dpb::ROOT,
        &word(volume.root_start() / volume.sector_bytes()),
    );
    put(dpb::MEDIA, &[volume.media]);
    put(dpb::ACCESSED, &[0]);
    put(dpb::FREE_HINT, &2u16.to_le_bytes());
    put(dpb::FREE, &0xFFFFu16.to_le_bytes());
    Some(out)
}

/// Where drive `drive`'s CDS entry lies.
#[must_use]
pub const fn cds_at(drive: u8) -> u16 {
    CDS + drive as u16 * CDS_ENTRY
}

/// The FAT a volume whose highest cluster is `max_cluster` has, as DOS
/// tells it from the clusters alone.
#[must_use]
pub fn kind_of(max_cluster: u16) -> Kind {
    if u32::from(max_cluster.saturating_sub(1)) < tairix_fat::boot::FAT16_CLUSTERS {
        Kind::Fat12
    } else {
        Kind::Fat16
    }
}

/// Where drive `drive`'s DPB lies.
#[must_use]
pub const fn dpb_at(drive: u8) -> u16 {
    DPBS + drive as u16 * DPB_SIZE
}

impl Kernel<'_, '_> {
    /// Drive `drive`'s parameters, if it is a physical drive with a DPB
    /// whose geometry the buffers can hold. A program may have written over
    /// the DPB, so its sector and cluster shape are checked before anything
    /// is sized by them.
    pub fn dpb(&mut self, drive: u8) -> Option<Dpb> {
        if drive >= DRIVES || self.get16(cds_at(drive) + cds::FLAGS) & cds::PHYSICAL == 0 {
            return None;
        }
        let at = dpb_at(drive);
        let sector_bytes = self.get16(at + dpb::SECTOR);
        let cluster_mask = self.get8(at + dpb::CLUSTER_MASK);
        let cluster_shift = self.get8(at + dpb::CLUSTER_SHIFT);
        let max_cluster = self.get16(at + dpb::MAX_CLUSTER);
        let fat_sectors = self.get16(at + dpb::FAT_SECTORS);
        let shaped = sector_bytes.is_power_of_two()
            && (32..=SECTOR_MAX).contains(&sector_bytes)
            && cluster_shift <= 7
            && u16::from(cluster_mask) + 1 == 1 << cluster_shift
            && (2..=tairix_fat::boot::FAT32_CLUSTERS).contains(&u32::from(max_cluster));
        if !shaped {
            return None;
        }
        let d = Dpb {
            at,
            drive,
            unit: self.get8(at + dpb::UNIT),
            sector_bytes,
            cluster_mask,
            cluster_shift,
            reserved: self.get16(at + dpb::RESERVED),
            fats: self.get8(at + dpb::FATS),
            root_entries: self.get16(at + dpb::ROOT_ENTRIES),
            data: self.get16(at + dpb::DATA),
            max_cluster,
            fat_sectors,
            root: self.get16(at + dpb::ROOT),
            driver: self.get_far(at + dpb::DRIVER),
            media: self.get8(at + dpb::MEDIA),
        };
        // Every cluster's entry must lie in the first FAT, or a FAT write
        // would land in whatever follows it.
        let (offset, width) = table::span(d.kind(), u32::from(max_cluster));
        let fat_bytes = u64::from(fat_sectors) * u64::from(sector_bytes);
        (offset + width as u64 <= fat_bytes).then_some(d)
    }

    /// Builds drive `drive`'s DPB from `bpb`, the volume's BPB its driver
    /// handed over, for unit `unit` of the driver at `driver`, its link to
    /// the next DPB kept; `false` when the BPB is none of a FAT12 or FAT16
    /// volume.
    pub fn build_dpb(&mut self, drive: u8, unit: u8, driver: Far, bpb: &[u8; DOS_BPB]) -> bool {
        let Some(mut fields) = dpb_fields(bpb) else {
            return false;
        };
        let at = dpb_at(drive);
        let mut next = [0u8; 4];
        self.g.read_bytes(SEGMENT, at + dpb::NEXT, &mut next);
        fields[usize::from(dpb::DRIVE)] = drive;
        fields[usize::from(dpb::UNIT)] = unit;
        let link = usize::from(dpb::DRIVER);
        fields[link..link + 2].copy_from_slice(&driver.offset.to_le_bytes());
        fields[link + 2..link + 4].copy_from_slice(&driver.segment.to_le_bytes());
        let chain = usize::from(dpb::NEXT);
        fields[chain..chain + 4].copy_from_slice(&next);
        self.g.write_bytes(SEGMENT, at, &fields);
        true
    }

    /// The disk buffers: the first one's segment, and how many there are.
    pub fn buffers(&mut self) -> (u16, u16) {
        (self.get16(driver::BUFFERS), self.get16(driver::BUFFERS + 2))
    }

    fn buffer_drive(&mut self, segment: u16) -> u8 {
        self.g.read8(segment, buffer::DRIVE)
    }

    fn buffer_sector(&mut self, segment: u16) -> u32 {
        let low = self.g.read16(segment, buffer::SECTOR);
        u32::from(low) | u32::from(self.g.read16(segment, buffer::SECTOR + 2)) << 16
    }

    /// The buffer holding `drive`'s `sector`, if one does.
    pub fn find_buffer(&mut self, drive: u8, sector: u32) -> Option<u16> {
        let (first, count) = self.buffers();
        (0..count).map(|n| buffer_at(first, n)).find(|&segment| {
            self.buffer_drive(segment) == drive && self.buffer_sector(segment) == sector
        })
    }

    /// Stamps the buffer at `segment` used now.
    pub fn touch_buffer(&mut self, segment: u16) {
        // A run of uses of one buffer costs one tick of the clock, so the
        // clock wraps no faster than the buffers trade places.
        if self.get16(driver::BUFFER_LAST) == segment {
            return;
        }
        let clock = self.get16(driver::BUFFER_CLOCK).wrapping_add(1);
        self.set16(driver::BUFFER_CLOCK, clock);
        self.set16(driver::BUFFER_LAST, segment);
        self.g.write16(segment, buffer::USED, clock);
    }

    /// The buffer to reuse for a new sector: a free one, else the one used
    /// longest ago that no call is reading into.
    fn victim(&mut self) -> Option<u16> {
        let (first, count) = self.buffers();
        let clock = self.get16(driver::BUFFER_CLOCK);
        let mut best: Option<(u16, u16)> = None;
        for n in 0..count {
            let segment = buffer_at(first, n);
            match self.buffer_drive(segment) {
                FREE => return Some(segment),
                BUSY => continue,
                _ => {}
            }
            let age = clock.wrapping_sub(self.g.read16(segment, buffer::USED));
            if best.is_none_or(|(_, oldest)| age > oldest) {
                best = Some((segment, age));
            }
        }
        best.map(|(segment, _)| segment)
    }

    /// Marks the buffer at `segment` changed, to be written back, with the
    /// FAT copies a FAT buffer writes.
    pub fn dirty(&mut self, segment: u16) {
        let flags = self.g.read8(segment, buffer::FLAGS);
        self.g.write8(segment, buffer::FLAGS, flags | buffer::DIRTY);
    }

    /// Forgets every buffer of `drive` — [`FREE`] for every drive — without
    /// writing it back: its medium has gone, or a reset asks for it.
    pub fn discard_buffers(&mut self, drive: u8) {
        let (first, count) = self.buffers();
        for n in 0..count {
            let segment = buffer_at(first, n);
            let held = self.buffer_drive(segment);
            if held < BUSY && (drive == FREE || held == drive) {
                self.g.write8(segment, buffer::DRIVE, FREE);
                self.g.write8(segment, buffer::FLAGS, 0);
            }
        }
    }

    /// Whether `drive` has a buffer still to be written back.
    pub fn has_dirty(&mut self, drive: u8) -> bool {
        let (first, count) = self.buffers();
        (0..count).any(|n| {
            let segment = buffer_at(first, n);
            self.buffer_drive(segment) == drive
                && self.g.read8(segment, buffer::FLAGS) & buffer::DIRTY != 0
        })
    }

    /// Frees every buffer a call was reading into when it was abandoned.
    pub fn release_busy_buffers(&mut self) {
        let (first, count) = self.buffers();
        for n in 0..count {
            let segment = buffer_at(first, n);
            if self.buffer_drive(segment) == BUSY {
                self.g.write8(segment, buffer::DRIVE, FREE);
            }
        }
    }

    /// Where each byte of `cluster`'s FAT entry lies — a buffer's segment and
    /// the offset in it — if every byte is in a buffer, each buffer touched.
    fn fat_entry(&mut self, d: &Dpb, cluster: u16) -> Option<([(u16, u16); 4], usize)> {
        if !d.in_volume(cluster) {
            return None;
        }
        let (offset, width) = table::span(d.kind(), u32::from(cluster));
        let mut places = [(0u16, 0u16); 4];
        let mut held: Option<(u32, u16)> = None;
        for (at, place) in (offset..).zip(places.iter_mut().take(width)) {
            let (sector, within) = fat_place(d, at);
            let segment = match held {
                Some((last, segment)) if last == sector => segment,
                _ => {
                    let segment = self.find_buffer(d.drive, sector)?;
                    self.touch_buffer(segment);
                    held = Some((sector, segment));
                    segment
                }
            };
            *place = (segment, buffer::DATA + within);
        }
        Some((places, width))
    }

    /// The FAT entry for `cluster`, if every byte of it is in a buffer.
    pub fn fat_peek(&mut self, d: &Dpb, cluster: u16) -> Option<u32> {
        let (places, width) = self.fat_entry(d, cluster)?;
        let mut bytes = [0u8; 4];
        for (byte, &(segment, at)) in bytes.iter_mut().zip(&places[..width]) {
            *byte = self.g.read8(segment, at);
        }
        table::read(d.kind(), u32::from(cluster), &bytes[..width])
    }

    /// Sets `cluster`'s FAT entry to `value` if every byte of it is in a
    /// buffer, keeping the bits a FAT12 entry shares with its neighbour:
    /// `false` when a sector must be read in first.
    pub fn fat_poke(&mut self, d: &Dpb, cluster: u16, value: u16) -> bool {
        let Some((places, width)) = self.fat_entry(d, cluster) else {
            return false;
        };
        let mut bytes = [0u8; 4];
        for (byte, &(segment, at)) in bytes.iter_mut().zip(&places[..width]) {
            *byte = self.g.read8(segment, at);
        }
        table::write(
            d.kind(),
            u32::from(cluster),
            &mut bytes[..width],
            u32::from(value),
        );
        for (&byte, &(segment, at)) in bytes.iter().zip(&places[..width]) {
            self.g.write8(segment, at, byte);
            self.dirty(segment);
        }
        true
    }

    /// Whether drive `drive`'s media must be checked by the running call
    /// before it is used: a removable drive not checked yet in this call.
    pub fn must_check(&mut self, drive: u8) -> bool {
        let at = self.scratch(level::CHECKED);
        let checked = self.get32(at);
        checked & 1 << (drive & 31) == 0
    }

    fn mark_checked(&mut self, drive: u8) {
        let at = self.scratch(level::CHECKED);
        let checked = self.get32(at);
        self.set32(at, checked | 1 << (drive & 31));
    }
}

/// The segment of buffer `n` of those from `first`.
const fn buffer_at(first: u16, n: u16) -> u16 {
    first.wrapping_add(n.wrapping_mul(BUFFER / 16))
}

/// The sector and byte FAT byte `at` lies at.
fn fat_place(d: &Dpb, at: u64) -> (u32, u16) {
    let bytes = u64::from(d.sector_bytes);
    let sector = u32::try_from(at / bytes).unwrap_or(u32::MAX);
    let within = u16::try_from(at % bytes).unwrap_or(0);
    (u32::from(d.reserved).saturating_add(sector), within)
}

/// A buffer's drive while it holds nothing.
pub const FREE: u8 = 0xFF;
/// A buffer's drive while a call reads into it.
pub const BUSY: u8 = 0xFE;

locals! {
    /// Moves `count` sectors from `sector` of drive `drive` to or from
    /// `address` through its driver; a failure is raised as INT 24h with the
    /// area `area` names, and the handler's answer honoured: retried, failed,
    /// ignored as done, or the program aborted.
    pub struct BlockIo {
        drive: u8,
        sector: u32,
        count: u16,
        address: Far,
        write: bool,
        area: u8,
        status: u16,
    }
}

/// The areas of a disk INT 24h names.
pub mod area {
    /// The boot area.
    pub const SYSTEM: u8 = 0;
    /// The FAT.
    pub const FAT: u8 = 1;
    /// The root directory.
    pub const ROOT: u8 = 2;
    /// The data area.
    pub const DATA: u8 = 3;
    /// A transfer INT 25h or 26h makes, whose failure its caller reports,
    /// not INT 24h.
    pub const ABSOLUTE: u8 = 0xFF;
}

const CALLED: u8 = 1;
const ANSWERED: u8 = 2;

impl Routine for BlockIo {
    const ID: Id = Id::BlockIo;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let Some(d) = k.dpb(self.drive) else {
            return Flow::Return(Err(Fault::Error(error::INVALID_DRIVE)));
        };
        match *phase {
            CALLED => {
                let status = u16::try_from(take!(child)).unwrap_or(0);
                if status & status::ERROR == 0 {
                    return Flow::Return(Ok(u32::from(self.count)));
                }
                if self.area == area::ABSOLUTE {
                    return Flow::Return(Err(Fault::Error(device::extended(status))));
                }
                self.status = status;
                let writing = if self.write { critical::WRITING } else { 0 };
                let flags =
                    critical::RETRY | critical::IGNORE | critical::FAIL | self.area << 1 | writing;
                *phase = ANSWERED;
                Flow::Call(Frame::of(&Critical {
                    flags,
                    drive: self.drive,
                    code: status & 0xFF,
                    header: d.driver,
                    action: 0,
                }))
            }
            ANSWERED => match u8::try_from(take!(child)) {
                Ok(critical::ACTION_RETRY) => {
                    *phase = 0;
                    self.step(k, phase, Ok(0))
                }
                Ok(critical::ACTION_IGNORE) => Flow::Return(Ok(u32::from(self.count))),
                Ok(critical::ACTION_FAIL) => {
                    Flow::Return(Err(Fault::Error(device::extended(self.status))))
                }
                _ => Flow::Return(Err(Fault::Abort)),
            },
            _ => {
                let verify = self.write && k.sda8(super::data::sda::VERIFY) != 0;
                let command = match (self.write, verify) {
                    (false, _) => command::INPUT,
                    (true, false) => command::OUTPUT,
                    (true, true) => command::OUTPUT_VERIFY,
                };
                k.request(d.unit, command);
                k.set_request8(field::MEDIA, d.media);
                k.set_request_far(field::ADDRESS, self.address);
                k.set_request16(field::COUNT, self.count);
                match u16::try_from(self.sector) {
                    Ok(sector) if sector != 0xFFFF => k.set_request16(field::SECTOR, sector),
                    _ => {
                        k.set_request16(field::SECTOR, 0xFFFF);
                        k.set_request32(field::SECTOR32, self.sector);
                    }
                }
                *phase = CALLED;
                Flow::Call(Frame::of(&Device { header: d.driver }))
            }
        }
    }
}

/// INT 24h's protocol.
pub mod critical {
    /// AH: the error was met writing.
    pub const WRITING: u8 = 0x01;
    /// AH: the error is a character device's, or a damaged FAT.
    pub const NOT_DISK: u8 = 0x80;
    /// AH: the handler may ignore it.
    pub const IGNORE: u8 = 0x20;
    /// AH: it may retry.
    pub const RETRY: u8 = 0x10;
    /// AH: it may fail the call.
    pub const FAIL: u8 = 0x08;
    /// AL's answers.
    pub const ACTION_IGNORE: u8 = 0;
    /// Retry.
    pub const ACTION_RETRY: u8 = 1;
    /// Abort the program.
    pub const ACTION_ABORT: u8 = 2;
    /// Fail the call.
    pub const ACTION_FAIL: u8 = 3;
}

locals! {
    /// Raises INT 24h for an error `code` on `drive` with AH `flags`, the
    /// device's header at `header`; returns the answer it may take. A
    /// critical error inside a handler fails at once, as DOS fails it.
    pub struct Critical {
        flags: u8,
        drive: u8,
        code: u16,
        header: Far,
        action: u8,
    }
}

impl Routine for Critical {
    const ID: Id = Id::Critical;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, _child: Outcome) -> Flow {
        use super::data::sda;
        if *phase == 0 {
            if k.sda8(sda::ERROR_MODE) != 0 {
                return Flow::Return(Ok(u32::from(Self::failing(k))));
            }
            k.set_sda8(sda::ERROR_MODE, 1);
            k.set_sda8(sda::ERROR_DRIVE, self.drive);
            k.call_int_on_caller(0x24);
            let g = &mut *k.g;
            g.set_r16(Reg16::Ax, u16::from_le_bytes([self.drive, self.flags]));
            g.set_r16(Reg16::Di, self.code);
            g.set_r16(Reg16::Bp, self.header.segment);
            g.set_r16(Reg16::Si, self.header.offset);
            *phase = 1;
            return Flow::Wait;
        }
        k.set_sda8(sda::ERROR_MODE, 0);
        k.set_sda8(sda::ERROR_DRIVE, 0xFF);
        let answer = k.g.r8(Reg8::Al);
        let allowed = |bit: u8| self.flags & bit != 0;
        let action = match answer {
            critical::ACTION_IGNORE if allowed(critical::IGNORE) => critical::ACTION_IGNORE,
            critical::ACTION_RETRY if allowed(critical::RETRY) => critical::ACTION_RETRY,
            critical::ACTION_FAIL | critical::ACTION_IGNORE | critical::ACTION_RETRY
                if allowed(critical::FAIL) =>
            {
                Self::failing(k)
            }
            _ => critical::ACTION_ABORT,
        };
        Flow::Return(Ok(u32::from(action)))
    }
}

impl Critical {
    /// The call is to be failed: it says so in the last error it records.
    fn failing(k: &mut Kernel<'_, '_>) -> u8 {
        let at = k.scratch(level::FAILED);
        k.set8(at, 1);
        critical::ACTION_FAIL
    }
}

locals! {
    /// Brings `drive`'s `sector` into a buffer and returns the buffer's
    /// segment: read from the disk unless `fresh`, when the caller is to
    /// fill it whole. A buffer `kind` of [`buffer::FAT`] writes its FAT
    /// copies back.
    pub struct Buffer {
        drive: u8,
        sector: u32,
        kind: u8,
        fresh: bool,
        victim: u16,
    }
}

const FLUSHED: u8 = 1;
const READ: u8 = 2;

impl Routine for Buffer {
    const ID: Id = Id::Buffer;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            FLUSHED => {
                take!(child);
                *phase = 0;
                self.step(k, phase, Ok(0))
            }
            READ => {
                let segment = self.victim;
                if let Err(fault) = child {
                    k.g.write8(segment, buffer::DRIVE, FREE);
                    return Flow::Return(Err(fault));
                }
                self.claim(k, segment);
                Flow::Return(Ok(u32::from(segment)))
            }
            _ => {
                if let Some(segment) = k.find_buffer(self.drive, self.sector) {
                    k.touch_buffer(segment);
                    return Flow::Return(Ok(u32::from(segment)));
                }
                let Some(segment) = k.victim() else {
                    return Flow::Return(Err(Fault::Error(super::engine::GENERAL_FAILURE)));
                };
                if k.g.read8(segment, buffer::FLAGS) & buffer::DIRTY != 0 {
                    *phase = FLUSHED;
                    return Flow::Call(Frame::of(&Flush {
                        segment,
                        drive: FREE,
                        copy: 0,
                    }));
                }
                if self.fresh {
                    self.claim(k, segment);
                    return Flow::Return(Ok(u32::from(segment)));
                }
                let Some(d) = k.dpb(self.drive) else {
                    return Flow::Return(Err(Fault::Error(error::INVALID_DRIVE)));
                };
                self.victim = segment;
                k.g.write8(segment, buffer::DRIVE, BUSY);
                *phase = READ;
                Flow::Call(Frame::of(&BlockIo {
                    drive: self.drive,
                    sector: self.sector,
                    count: 1,
                    address: Far::new(segment, buffer::DATA),
                    write: false,
                    area: area_of(&d, self.sector),
                    status: 0,
                }))
            }
        }
    }
}

impl Buffer {
    /// Records the buffer at `segment` as holding the sector, clean.
    fn claim(&self, k: &mut Kernel<'_, '_>, segment: u16) {
        let (copies, stride) = match k.dpb(self.drive) {
            Some(d) if self.kind == buffer::FAT => (d.fats, d.fat_sectors),
            _ => (1, 0),
        };
        k.g.write8(segment, buffer::DRIVE, self.drive);
        k.g.write8(segment, buffer::FLAGS, self.kind);
        k.g.write_bytes(segment, buffer::SECTOR, &self.sector.to_le_bytes());
        k.g.write8(segment, buffer::COPIES, copies);
        k.g.write16(segment, buffer::STRIDE, stride);
        k.touch_buffer(segment);
    }
}

/// The area of the disk `sector` lies in, as INT 24h names it.
#[must_use]
pub fn area_of(d: &Dpb, sector: u32) -> u8 {
    if sector < u32::from(d.reserved) {
        area::SYSTEM
    } else if sector < u32::from(d.root) {
        area::FAT
    } else if sector < u32::from(d.data) {
        area::ROOT
    } else {
        area::DATA
    }
}

/// Writes back every changed buffer of `drive` ([`FREE`] for all): how a
/// call that changed a directory ends, and how a process ends, so what it
/// changed is on the disk when it returns rather than only in the buffers.
pub fn written_back(drive: u8) -> Flow {
    Flow::Call(Frame::of(&Flush {
        segment: 0,
        drive,
        copy: 0,
    }))
}

locals! {
    /// Writes back the buffer at `segment`, every FAT copy of it; or, with
    /// no segment, every changed buffer of `drive` ([`FREE`] for all).
    pub struct Flush {
        segment: u16,
        drive: u8,
        copy: u8,
    }
}

impl Routine for Flush {
    const ID: Id = Id::Flush;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if *phase == CALLED {
            take!(child);
            self.copy += 1;
            *phase = 0;
        }
        loop {
            let segment = if self.segment != 0 {
                self.segment
            } else {
                let (first, count) = k.buffers();
                let Some(segment) = (0..count).map(|n| buffer_at(first, n)).find(|&s| {
                    let drive = k.buffer_drive(s);
                    drive < BUSY
                        && (self.drive == FREE || drive == self.drive)
                        && k.g.read8(s, buffer::FLAGS) & buffer::DIRTY != 0
                }) else {
                    return Flow::Return(Ok(0));
                };
                segment
            };
            let copies = k.g.read8(segment, buffer::COPIES).max(1);
            if k.g.read8(segment, buffer::FLAGS) & buffer::DIRTY == 0 || self.copy >= copies {
                let flags = k.g.read8(segment, buffer::FLAGS);
                k.g.write8(segment, buffer::FLAGS, flags & !buffer::DIRTY);
                self.copy = 0;
                if self.segment != 0 {
                    return Flow::Return(Ok(0));
                }
                continue;
            }
            let drive = k.buffer_drive(segment);
            let stride = u32::from(k.g.read16(segment, buffer::STRIDE));
            let sector = k.buffer_sector(segment) + u32::from(self.copy) * stride;
            let Some(d) = k.dpb(drive) else {
                k.g.write8(segment, buffer::FLAGS, 0);
                continue;
            };
            *phase = CALLED;
            return Flow::Call(Frame::of(&BlockIo {
                drive,
                sector,
                count: 1,
                address: Far::new(segment, buffer::DATA),
                write: true,
                area: area_of(&d, sector),
                status: 0,
            }));
        }
    }
}

locals! {
    /// Puts `drive`'s label in its boot record — the entry just made's
    /// name, or none once one is `deleted` — through its driver's media-ID
    /// requests, as DOS does when a label entry is made or deleted, so the
    /// volume and the driver's knowledge of it agree. A driver or volume
    /// that keeps no media ID has its label in its directory alone.
    pub struct MediaLabel {
        drive: u8,
        deleted: bool,
    }
}

const ID_READ: u8 = 1;
const ID_WRITTEN: u8 = 2;

impl MediaLabel {
    #[must_use]
    pub const fn of(drive: u8, deleted: bool) -> Self {
        Self { drive, deleted }
    }

    fn request(k: &mut Kernel<'_, '_>, d: &Dpb, function: u8) -> Flow {
        k.request(d.unit, command::GENERIC_IOCTL);
        k.set_request8(field::CATEGORY, device::generic::DISK);
        k.set_request8(field::FUNCTION, function);
        let record = k.scratch(level::MEDIA_ID);
        k.set_request_far(field::PACKET, Far::new(SEGMENT, record));
        Flow::Call(Frame::of(&Device { header: d.driver }))
    }
}

impl Routine for MediaLabel {
    const ID: Id = Id::MediaLabel;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let Some(d) = k.dpb(self.drive) else {
            return Flow::Return(Ok(0));
        };
        let record = k.scratch(level::MEDIA_ID);
        match *phase {
            0 => {
                if k.attributes(d.driver) & device::attribute::GENERIC == 0 {
                    return Flow::Return(Ok(0));
                }
                k.g.write_bytes(SEGMENT, record, &[0; device::generic::media::LEN as usize]);
                *phase = ID_READ;
                Self::request(k, &d, device::generic::GET_MEDIA_ID)
            }
            ID_READ => {
                let status = u16::try_from(take!(child)).unwrap_or(0);
                if status & status::ERROR != 0 {
                    return Flow::Return(Ok(0));
                }
                let label = if self.deleted {
                    label_field(NO_LABEL)
                } else {
                    k.found_entry().name
                };
                k.g.write_bytes(SEGMENT, record + device::generic::media::LABEL, &label);
                *phase = ID_WRITTEN;
                Self::request(k, &d, device::generic::SET_MEDIA_ID)
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}

locals! {
    /// Checks `drive`'s medium before the running call uses it: a changed
    /// one has its buffers dropped, its DPB built again from the BPB its
    /// driver reads, and the drive's current directory left to be looked up
    /// again; a change with buffers still to be written raises the
    /// invalid-disk-change critical error, and a check or read the driver
    /// fails — an empty drive's — raises its own.
    pub struct Media {
        drive: u8,
        status: u16,
    }
}

const CHECKED: u8 = 1;
const BUILT: u8 = 2;
const ASKED: u8 = 3;
const FAILED: u8 = 4;

impl Routine for Media {
    const ID: Id = Id::Media;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let Some(d) = k.dpb(self.drive) else {
            return Flow::Return(Err(Fault::Error(error::INVALID_DRIVE)));
        };
        match *phase {
            CHECKED => {
                let status = u16::try_from(take!(child)).unwrap_or(0);
                if status & status::ERROR != 0 {
                    return self.failed(phase, &d, status);
                }
                match i8::from_le_bytes([k.request8(field::ADDRESS)]) {
                    1 => self.done(k),
                    0 if k.has_dirty(self.drive) => self.done(k),
                    0 => self.rebuild(k, phase, &d),
                    _ if k.has_dirty(self.drive) => {
                        *phase = ASKED;
                        Flow::Call(Frame::of(&Critical {
                            flags: critical::RETRY | critical::IGNORE | critical::FAIL,
                            drive: self.drive,
                            code: u16::from(device::error::DISK_CHANGE),
                            header: d.driver,
                            action: 0,
                        }))
                    }
                    _ => self.rebuild(k, phase, &d),
                }
            }
            ASKED => match u8::try_from(take!(child)) {
                Ok(critical::ACTION_RETRY) => {
                    *phase = 0;
                    self.step(k, phase, Ok(0))
                }
                Ok(critical::ACTION_IGNORE) => self.rebuild(k, phase, &d),
                Ok(critical::ACTION_FAIL) => Flow::Return(Err(Fault::Error(error::DISK_CHANGE))),
                _ => Flow::Return(Err(Fault::Abort)),
            },
            FAILED => match u8::try_from(take!(child)) {
                Ok(critical::ACTION_RETRY) => {
                    *phase = 0;
                    self.step(k, phase, Ok(0))
                }
                Ok(critical::ACTION_FAIL) => {
                    Flow::Return(Err(Fault::Error(device::extended(self.status))))
                }
                _ => Flow::Return(Err(Fault::Abort)),
            },
            BUILT => {
                let status = u16::try_from(take!(child)).unwrap_or(0);
                if status & status::ERROR != 0 {
                    return self.failed(phase, &d, status);
                }
                let pointer = k.request_far(field::COUNT);
                let mut bpb = [0u8; DOS_BPB];
                k.g.read_bytes(pointer.segment, pointer.offset, &mut bpb);
                if !k.build_dpb(self.drive, d.unit, d.driver, &bpb) {
                    return Flow::Return(Err(Fault::Error(0x1A)));
                }
                // The new medium may hold the same directory: it is looked
                // up again by its path when the drive's directory is asked.
                let at = cds_at(self.drive);
                if k.get16(at + cds::CLUSTER) != 0 {
                    k.set16(at + cds::CLUSTER, cds::UNKNOWN);
                }
                self.done(k)
            }
            _ => {
                if !k.must_check(self.drive) {
                    return Flow::Return(Ok(0));
                }
                if k.get8(d.at + dpb::ACCESSED) == 0xFF {
                    return self.rebuild(k, phase, &d);
                }
                k.request(d.unit, command::MEDIA_CHECK);
                k.set_request8(field::MEDIA, d.media);
                *phase = CHECKED;
                Flow::Call(Frame::of(&Device { header: d.driver }))
            }
        }
    }
}

impl Media {
    /// The driver failed the check or the read with `status`: INT 24h is
    /// raised for it, retry and fail allowed.
    fn failed(&mut self, phase: &mut u8, d: &Dpb, status: u16) -> Flow {
        self.status = status;
        *phase = FAILED;
        Flow::Call(Frame::of(&Critical {
            flags: critical::RETRY | critical::FAIL | area::SYSTEM << 1,
            drive: self.drive,
            code: status & 0xFF,
            header: d.driver,
            action: 0,
        }))
    }

    fn done(self, k: &mut Kernel<'_, '_>) -> Flow {
        k.mark_checked(self.drive);
        Flow::Return(Ok(0))
    }

    fn rebuild(self, k: &mut Kernel<'_, '_>, phase: &mut u8, d: &Dpb) -> Flow {
        k.discard_buffers(self.drive);
        k.request(d.unit, command::BUILD_BPB);
        k.set_request8(field::MEDIA, d.media);
        let scratch = k.scratch(level::BPB);
        k.set_request_far(field::ADDRESS, Far::new(SEGMENT, scratch));
        *phase = BUILT;
        Flow::Call(Frame::of(&Device { header: d.driver }))
    }
}

locals! {
    /// Reads `cluster`'s FAT entry on `drive`, a buffer at a time; FAT12's
    /// entries may span two sectors.
    pub struct FatGet {
        drive: u8,
        cluster: u16,
        got: u8,
        bytes: [u8; 4],
    }
}

impl Routine for FatGet {
    const ID: Id = Id::FatGet;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if *phase == CALLED {
            take!(child);
            *phase = 0;
        }
        let Some(d) = k.dpb(self.drive) else {
            return Flow::Return(Err(Fault::Error(error::INVALID_DRIVE)));
        };
        if !d.in_volume(self.cluster) {
            return Flow::Return(Err(Fault::Error(error::INVALID_DATA)));
        }
        if self.got == 0 {
            if let Some(value) = k.fat_peek(&d, self.cluster) {
                return Flow::Return(Ok(value));
            }
        }
        let (offset, width) = table::span(d.kind(), u32::from(self.cluster));
        while usize::from(self.got) < width {
            let (sector, within) = fat_place(&d, offset + u64::from(self.got));
            let Some(segment) = k.find_buffer(d.drive, sector) else {
                *phase = CALLED;
                return Flow::Call(Frame::of(&Buffer {
                    drive: d.drive,
                    sector,
                    kind: buffer::FAT,
                    fresh: false,
                    victim: 0,
                }));
            };
            k.touch_buffer(segment);
            self.bytes[usize::from(self.got)] = k.g.read8(segment, buffer::DATA + within);
            self.got += 1;
        }
        let value =
            table::read(d.kind(), u32::from(self.cluster), &self.bytes[..width]).unwrap_or(0);
        Flow::Return(Ok(value))
    }
}

locals! {
    /// Sets `cluster`'s FAT entry on `drive` to `value`, keeping the bits a
    /// FAT12 entry shares with its neighbour.
    pub struct FatSet {
        drive: u8,
        cluster: u16,
        value: u16,
        got: u8,
        put: u8,
        bytes: [u8; 4],
    }
}

impl Routine for FatSet {
    const ID: Id = Id::FatSet;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if *phase == CALLED {
            take!(child);
            *phase = 0;
        }
        let Some(d) = k.dpb(self.drive) else {
            return Flow::Return(Err(Fault::Error(error::INVALID_DRIVE)));
        };
        if !d.in_volume(self.cluster) {
            return Flow::Return(Err(Fault::Error(error::INVALID_DATA)));
        }
        if self.got == 0 && k.fat_poke(&d, self.cluster, self.value) {
            return Flow::Return(Ok(0));
        }
        let (offset, width) = table::span(d.kind(), u32::from(self.cluster));
        while usize::from(self.got) < width {
            let (sector, within) = fat_place(&d, offset + u64::from(self.got));
            let Some(segment) = k.find_buffer(d.drive, sector) else {
                *phase = CALLED;
                return Flow::Call(Frame::of(&Buffer {
                    drive: d.drive,
                    sector,
                    kind: buffer::FAT,
                    fresh: false,
                    victim: 0,
                }));
            };
            self.bytes[usize::from(self.got)] = k.g.read8(segment, buffer::DATA + within);
            self.got += 1;
            if usize::from(self.got) == width {
                table::write(
                    d.kind(),
                    u32::from(self.cluster),
                    &mut self.bytes[..width],
                    u32::from(self.value),
                );
            }
        }
        while usize::from(self.put) < width {
            let (sector, within) = fat_place(&d, offset + u64::from(self.put));
            let Some(segment) = k.find_buffer(d.drive, sector) else {
                *phase = CALLED;
                return Flow::Call(Frame::of(&Buffer {
                    drive: d.drive,
                    sector,
                    kind: buffer::FAT,
                    fresh: false,
                    victim: 0,
                }));
            };
            k.g.write8(
                segment,
                buffer::DATA + within,
                self.bytes[usize::from(self.put)],
            );
            k.dirty(segment);
            k.touch_buffer(segment);
            self.put += 1;
        }
        Flow::Return(Ok(0))
    }
}

locals! {
    /// Follows `drive`'s chain from `first` `steps` links on: the cluster
    /// there, or `0` where the chain ends first.
    pub struct Walk {
        drive: u8,
        cluster: u16,
        steps: u16,
    }
}

impl Routine for Walk {
    const ID: Id = Id::Walk;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let Some(d) = k.dpb(self.drive) else {
            return Flow::Return(Err(Fault::Error(error::INVALID_DRIVE)));
        };
        if *phase == CALLED {
            let value = take!(child);
            *phase = 0;
            match d.link(value) {
                Link::Next(next) => {
                    self.cluster = u16::try_from(next).unwrap_or(0);
                    self.steps -= 1;
                }
                _ => return Flow::Return(Ok(0)),
            }
        }
        while self.steps > 0 {
            if !d.in_volume(self.cluster) {
                return Flow::Return(Ok(0));
            }
            let Some(value) = k.fat_peek(&d, self.cluster) else {
                *phase = CALLED;
                return Flow::Call(Frame::of(&FatGet {
                    drive: self.drive,
                    cluster: self.cluster,
                    got: 0,
                    bytes: [0; 4],
                }));
            };
            let Link::Next(next) = d.link(value) else {
                return Flow::Return(Ok(0));
            };
            self.cluster = u16::try_from(next).unwrap_or(0);
            self.steps -= 1;
        }
        Flow::Return(Ok(u32::from(if d.in_volume(self.cluster) {
            self.cluster
        } else {
            0
        })))
    }
}

locals! {
    /// Takes a free cluster on `drive`, marks it its chain's end and links it
    /// after `after` (none for `0`): the cluster, or `0` when none is free.
    pub struct Alloc {
        drive: u8,
        after: u16,
        candidate: u16,
        scanned: u16,
        found: u16,
    }
}

const MARKED: u8 = 2;
const LINKED: u8 = 3;

impl Routine for Alloc {
    const ID: Id = Id::Alloc;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let Some(d) = k.dpb(self.drive) else {
            return Flow::Return(Err(Fault::Error(error::INVALID_DRIVE)));
        };
        let total = d.max_cluster.saturating_sub(1);
        match *phase {
            MARKED => {
                take!(child);
                return self.link(k, &d, phase);
            }
            LINKED => {
                take!(child);
                return self.done(k, &d);
            }
            CALLED => {
                let value = take!(child);
                *phase = 0;
                if self.is_free(&d, value) {
                    return self.mark(k, &d, phase);
                }
            }
            _ => {
                let hint = k.get16(d.at + dpb::FREE_HINT);
                self.candidate = if d.in_volume(hint) { hint } else { 2 };
            }
        }
        while self.scanned < total {
            let Some(value) = k.fat_peek(&d, self.candidate) else {
                *phase = CALLED;
                return Flow::Call(Frame::of(&FatGet {
                    drive: self.drive,
                    cluster: self.candidate,
                    got: 0,
                    bytes: [0; 4],
                }));
            };
            if self.is_free(&d, value) {
                return self.mark(k, &d, phase);
            }
        }
        k.set16(d.at + dpb::FREE, 0);
        Flow::Return(Ok(0))
    }
}

impl Alloc {
    /// Whether the candidate's entry `value` says it is free; if not, the
    /// search moves on to the next.
    fn is_free(&mut self, d: &Dpb, value: u32) -> bool {
        if d.link(value) == Link::Free {
            self.found = self.candidate;
            return true;
        }
        self.scanned += 1;
        self.candidate = if self.candidate >= d.max_cluster {
            2
        } else {
            self.candidate + 1
        };
        false
    }

    /// Marks the cluster found its chain's end.
    fn mark(&self, k: &mut Kernel<'_, '_>, d: &Dpb, phase: &mut u8) -> Flow {
        let end = u16::try_from(d.end()).unwrap_or(u16::MAX);
        if k.fat_poke(d, self.found, end) {
            return self.link(k, d, phase);
        }
        *phase = MARKED;
        Flow::Call(Frame::of(&FatSet {
            drive: self.drive,
            cluster: self.found,
            value: end,
            got: 0,
            put: 0,
            bytes: [0; 4],
        }))
    }

    /// Links the cluster found after the chain's last, if it has one.
    fn link(&self, k: &mut Kernel<'_, '_>, d: &Dpb, phase: &mut u8) -> Flow {
        if self.after == 0 || k.fat_poke(d, self.after, self.found) {
            return self.done(k, d);
        }
        *phase = LINKED;
        Flow::Call(Frame::of(&FatSet {
            drive: self.drive,
            cluster: self.after,
            value: self.found,
            got: 0,
            put: 0,
            bytes: [0; 4],
        }))
    }

    fn done(&self, k: &mut Kernel<'_, '_>, d: &Dpb) -> Flow {
        let next = if self.found >= d.max_cluster {
            2
        } else {
            self.found + 1
        };
        k.set16(d.at + dpb::FREE_HINT, next);
        let free = k.get16(d.at + dpb::FREE);
        if free != 0xFFFF {
            k.set16(d.at + dpb::FREE, free.saturating_sub(1));
        }
        Flow::Return(Ok(u32::from(self.found)))
    }
}

locals! {
    /// Frees `drive`'s chain from `cluster` to its end.
    pub struct FreeChain {
        drive: u8,
        cluster: u16,
        next: u16,
        freed: u16,
        read: bool,
    }
}

impl Routine for FreeChain {
    const ID: Id = Id::FreeChain;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let Some(d) = k.dpb(self.drive) else {
            return Flow::Return(Err(Fault::Error(error::INVALID_DRIVE)));
        };
        match *phase {
            CALLED => {
                let value = take!(child);
                self.link(&d, value);
            }
            MARKED => {
                take!(child);
                self.freed_one(k, &d);
            }
            _ => {}
        }
        *phase = 0;
        loop {
            if !d.in_volume(self.cluster) || self.freed >= d.max_cluster {
                return Flow::Return(Ok(u32::from(self.freed)));
            }
            if !self.read {
                let Some(value) = k.fat_peek(&d, self.cluster) else {
                    *phase = CALLED;
                    return Flow::Call(Frame::of(&FatGet {
                        drive: self.drive,
                        cluster: self.cluster,
                        got: 0,
                        bytes: [0; 4],
                    }));
                };
                self.link(&d, value);
            }
            if !k.fat_poke(&d, self.cluster, 0) {
                *phase = MARKED;
                return Flow::Call(Frame::of(&FatSet {
                    drive: self.drive,
                    cluster: self.cluster,
                    value: 0,
                    got: 0,
                    put: 0,
                    bytes: [0; 4],
                }));
            }
            self.freed_one(k, &d);
        }
    }
}

impl FreeChain {
    /// Takes the cluster's entry `value`: the next to free, `0` at the end.
    fn link(&mut self, d: &Dpb, value: u32) {
        self.next = match d.link(value) {
            Link::Next(next) => u16::try_from(next).unwrap_or(0),
            _ => 0,
        };
        self.read = true;
    }

    /// Counts the cluster freed and moves to the next.
    fn freed_one(&mut self, k: &mut Kernel<'_, '_>, d: &Dpb) {
        let free = k.get16(d.at + dpb::FREE);
        if free != 0xFFFF {
            k.set16(d.at + dpb::FREE, free.saturating_add(1));
        }
        let hint = k.get16(d.at + dpb::FREE_HINT);
        if self.cluster < hint {
            k.set16(d.at + dpb::FREE_HINT, self.cluster);
        }
        self.freed += 1;
        self.cluster = self.next;
        self.read = false;
    }
}

locals! {
    /// Counts `drive`'s free clusters, its medium checked first, a FAT sector
    /// at a time, and keeps the count in its DPB, where a count already
    /// kept answers at once.
    pub struct Free {
        drive: u8,
        cluster: u16,
        counted: u16,
    }
}

const COUNTING: u8 = 2;
const SPANNED: u8 = 3;

impl Routine for Free {
    const ID: Id = Id::Free;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if *phase == 0 {
            *phase = CHECKED;
            return Flow::Call(Frame::of(&Media {
                drive: self.drive,
                status: 0,
            }));
        }
        let value = take!(child);
        let Some(d) = k.dpb(self.drive) else {
            return Flow::Return(Err(Fault::Error(error::INVALID_DRIVE)));
        };
        if *phase == SPANNED {
            self.count(&d, value);
            *phase = COUNTING;
        }
        if *phase == CHECKED {
            let kept = k.get16(d.at + dpb::FREE);
            if kept < d.max_cluster {
                return Flow::Return(Ok(u32::from(kept)));
            }
            self.cluster = 2;
            self.counted = 0;
            *phase = COUNTING;
        }
        let kind = d.kind();
        let mut data = [0u8; SECTOR_MAX as usize];
        let bytes = usize::from(d.sector_bytes);
        while self.cluster <= d.max_cluster {
            let (offset, width) = table::span(kind, u32::from(self.cluster));
            let (sector, within) = fat_place(&d, offset);
            let Some(segment) = k.find_buffer(d.drive, sector) else {
                return Flow::Call(Frame::of(&Buffer {
                    drive: d.drive,
                    sector,
                    kind: buffer::FAT,
                    fresh: false,
                    victim: 0,
                }));
            };
            if usize::from(within) + width > bytes {
                // A FAT12 entry across two sectors: read through the buffers.
                let Some(value) = k.fat_peek(&d, self.cluster) else {
                    *phase = SPANNED;
                    return Flow::Call(Frame::of(&FatGet {
                        drive: d.drive,
                        cluster: self.cluster,
                        got: 0,
                        bytes: [0; 4],
                    }));
                };
                self.count(&d, value);
                continue;
            }
            k.touch_buffer(segment);
            k.g.read_bytes(segment, buffer::DATA, &mut data[..bytes]);
            while self.cluster <= d.max_cluster {
                let (offset, width) = table::span(kind, u32::from(self.cluster));
                let (next, within) = fat_place(&d, offset);
                let start = usize::from(within);
                if next != sector || start + width > bytes {
                    break;
                }
                let value = table::read(kind, u32::from(self.cluster), &data[start..start + width])
                    .unwrap_or(0);
                self.count(&d, value);
            }
        }
        k.set16(d.at + dpb::FREE, self.counted);
        Flow::Return(Ok(u32::from(self.counted)))
    }
}

impl Free {
    fn count(&mut self, d: &Dpb, value: u32) {
        if d.link(value) == Link::Free {
            self.counted += 1;
        }
        self.cluster += 1;
    }
}
