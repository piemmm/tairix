//! Writing directories: a free slot found for a new entry, a subdirectory
//! grown by a cluster when it has none, a cluster zeroed, and an entry
//! stored back over its slot.

use super::data::{buffer, level, SEGMENT};
use super::dir::{scan, DirScan, ENTRY_BYTES};
use super::engine::{locals, take, Fault, Flow, Frame, Id, Kernel, Outcome, Routine};
use super::fs::{error, Alloc, Buffer, FatGet};
use tairix_fat::dir::ENTRY;
use tairix_fat::table::Link;

locals! {
    /// Finds a free slot in `drive`'s directory `directory` (`0` for the
    /// root), growing a subdirectory by a cluster when every slot is taken.
    /// The slot is left where the scratch records the entry found; a full
    /// root is [`error::ACCESS_DENIED`], as DOS reports it.
    pub struct Slot {
        drive: u8,
        directory: u16,
        grown: bool,
    }
}

const SCANNED: u8 = 1;
const GROWN: u8 = 2;

impl Routine for Slot {
    const ID: Id = Id::Slot;

    fn step(&mut self, _k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            SCANNED => match child {
                Ok(_) => Flow::Return(Ok(0)),
                Err(Fault::Error(error::NO_MORE_FILES)) if self.directory != 0 && !self.grown => {
                    *phase = GROWN;
                    Flow::Call(Frame::of(&Grow {
                        drive: self.drive,
                        cluster: self.directory,
                        hops: 0,
                        added: 0,
                    }))
                }
                Err(Fault::Error(error::NO_MORE_FILES)) => {
                    Flow::Return(Err(Fault::Error(error::ACCESS_DENIED)))
                }
                Err(fault) => Flow::Return(Err(fault)),
            },
            GROWN => {
                take!(child);
                self.grown = true;
                self.scan(phase)
            }
            _ => self.scan(phase),
        }
    }
}

impl Slot {
    fn scan(self, phase: &mut u8) -> Flow {
        *phase = SCANNED;
        Flow::Call(Frame::of(&DirScan {
            drive: self.drive,
            directory: self.directory,
            index: 0,
            attributes: 0,
            mode: scan::FREE,
            cluster: 0,
            position: 0,
        }))
    }
}

locals! {
    /// Extends `drive`'s chain from `cluster` — a subdirectory's first — by
    /// one zeroed cluster: the chain followed to its end, at most a hop per
    /// cluster the volume has, and a free cluster linked on. Returns the new
    /// cluster; a full disk is [`error::ACCESS_DENIED`].
    pub struct Grow {
        drive: u8,
        cluster: u16,
        hops: u16,
        added: u16,
    }
}

const LINK_READ: u8 = 1;
const ALLOCATED: u8 = 2;
const CLEARED: u8 = 3;

impl Routine for Grow {
    const ID: Id = Id::Grow;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let Some(d) = k.dpb(self.drive) else {
            return Flow::Return(Err(Fault::Error(error::INVALID_DRIVE)));
        };
        match *phase {
            ALLOCATED => {
                self.added = u16::try_from(take!(child)).unwrap_or(0);
                if !d.in_volume(self.added) {
                    return Flow::Return(Err(Fault::Error(error::ACCESS_DENIED)));
                }
                *phase = CLEARED;
                return Flow::Call(Frame::of(&Clear {
                    drive: self.drive,
                    cluster: self.added,
                    left: 0,
                    started: false,
                }));
            }
            CLEARED => {
                take!(child);
                return Flow::Return(Ok(u32::from(self.added)));
            }
            LINK_READ => {
                let value = take!(child);
                if let Some(next) = self.follow(&d, value) {
                    self.cluster = next;
                } else {
                    return self.allocate(phase);
                }
            }
            _ => {}
        }
        loop {
            if !d.in_volume(self.cluster) || self.hops > d.max_cluster {
                return Flow::Return(Err(Fault::Error(error::INVALID_DATA)));
            }
            let Some(value) = k.fat_peek(&d, self.cluster) else {
                *phase = LINK_READ;
                return Flow::Call(Frame::of(&FatGet {
                    drive: self.drive,
                    cluster: self.cluster,
                    got: 0,
                    bytes: [0; 4],
                }));
            };
            match self.follow(&d, value) {
                Some(next) => self.cluster = next,
                None => return self.allocate(phase),
            }
        }
    }
}

impl Grow {
    /// The cluster after this one, counting the hop; `None` at the chain's
    /// end.
    fn follow(&mut self, d: &super::fs::Dpb, value: u32) -> Option<u16> {
        match d.link(value) {
            Link::Next(next) => {
                self.hops += 1;
                u16::try_from(next).ok()
            }
            _ => None,
        }
    }

    fn allocate(self, phase: &mut u8) -> Flow {
        *phase = ALLOCATED;
        Flow::Call(Frame::of(&Alloc {
            drive: self.drive,
            after: self.cluster,
            candidate: 0,
            scanned: 0,
            found: 0,
        }))
    }
}

locals! {
    /// Zeroes `drive`'s cluster `cluster` through buffers taken fresh, its
    /// last sector first, so the first is the one a caller about to write
    /// into it finds still buffered.
    pub struct Clear {
        drive: u8,
        cluster: u16,
        left: u8,
        started: bool,
    }
}

const TAKEN: u8 = 1;

impl Routine for Clear {
    const ID: Id = Id::Clear;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let Some(d) = k.dpb(self.drive) else {
            return Flow::Return(Err(Fault::Error(error::INVALID_DRIVE)));
        };
        if !self.started {
            self.started = true;
            self.left = u8::try_from(d.cluster_sectors()).unwrap_or(u8::MAX);
        }
        if *phase == TAKEN {
            let segment = u16::try_from(take!(child)).unwrap_or(0);
            zero(k, segment, d.sector_bytes);
            self.left -= 1;
        }
        while self.left > 0 {
            let sector = d.cluster_sector(self.cluster) + u32::from(self.left - 1);
            if let Some(segment) = k.find_buffer(self.drive, sector) {
                zero(k, segment, d.sector_bytes);
                self.left -= 1;
                continue;
            }
            *phase = TAKEN;
            return Flow::Call(Frame::of(&Buffer {
                drive: self.drive,
                sector,
                kind: 0,
                fresh: true,
                victim: 0,
            }));
        }
        Flow::Return(Ok(0))
    }
}

/// Zeroes the buffer at `segment`'s sector, to be written back.
fn zero(k: &mut Kernel<'_, '_>, segment: u16, bytes: u16) {
    let empty = [0u8; super::data::SECTOR_MAX as usize];
    k.g.write_bytes(
        segment,
        buffer::DATA,
        &empty[..usize::from(bytes).min(empty.len())],
    );
    k.dirty(segment);
    k.touch_buffer(segment);
}

locals! {
    /// Writes the entry at the running call's scratch `from` over slot `slot`
    /// of `drive`'s directory sector `sector`.
    pub struct Store {
        drive: u8,
        sector: u32,
        slot: u16,
        from: u16,
    }
}

impl Routine for Store {
    const ID: Id = Id::Store;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let Some(d) = k.dpb(self.drive) else {
            return Flow::Return(Err(Fault::Error(error::INVALID_DRIVE)));
        };
        if self.slot >= d.sector_bytes / ENTRY_BYTES
            || !matches!(self.from, level::FOUND | level::KEPT)
        {
            return Flow::Return(Err(Fault::Error(error::INVALID_DATA)));
        }
        if *phase == TAKEN {
            take!(child);
        }
        let Some(segment) = k.find_buffer(self.drive, self.sector) else {
            *phase = TAKEN;
            return Flow::Call(Frame::of(&Buffer {
                drive: self.drive,
                sector: self.sector,
                kind: 0,
                fresh: false,
                victim: 0,
            }));
        };
        let mut entry = [0u8; ENTRY];
        let from = k.scratch(self.from);
        k.g.read_bytes(SEGMENT, from, &mut entry);
        k.g.write_bytes(segment, buffer::DATA + self.slot * ENTRY_BYTES, &entry);
        k.dirty(segment);
        k.touch_buffer(segment);
        Flow::Return(Ok(0))
    }
}

impl Store {
    /// A store of the entry the scratch found, back where it was found.
    pub fn found(k: &mut Kernel<'_, '_>, drive: u8) -> Self {
        Self {
            drive,
            sector: k.found_sector(),
            slot: k.found16(super::dir::found::SLOT),
            from: level::FOUND,
        }
    }
}
