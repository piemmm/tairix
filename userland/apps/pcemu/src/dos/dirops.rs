//! The directory calls: make, remove and change a directory, delete and
//! rename a file, read and set attributes, search a directory through the
//! DTA, create a uniquely named file, the extended open, and the calls that
//! only read the kernel's state — the current directory, the canonical
//! name, and the handle table's size.

use super::char::Clock;
use super::data::{cds, level, SEGMENT};
use super::dir::{
    first_cluster, found, mode, name_error, resolved, scan, DirScan, Resolve, ENTRY_BYTES,
    HIDDEN_KINDS,
};
use super::dir_write::{Clear, Slot, Store};
use super::engine::{locals, take, Far, Fault, Flow, Frame, Id, Kernel, Outcome, Routine, Saved};
use super::file::{access, Close, Create, Open, Write, CLOSED};
use super::fs::{cds_at, error, written_back, Alloc, Buffer, FreeChain, Media};
use super::memory::psp;
use super::name::{self, PATH_MAX};
use super::redirect;
use tairix_fat::dir::{attr, ShortEntry, DELETED, DOT, DOTDOT};

impl Resolve {
    /// A resolution of the running call's path, or its second.
    #[must_use]
    pub const fn of(mode: u8, second: bool) -> Self {
        Self {
            mode,
            drive: 0,
            directory: 0,
            component: 0,
            count: 0,
            second,
        }
    }
}

impl Kernel<'_, '_> {
    /// The drive the running call's resolved path `which` is on.
    fn path_drive(&mut self, which: u16) -> u8 {
        let (path, _) = self.path(which);
        path[0].wrapping_sub(b'A')
    }

    /// Whether the resolved path is a drive's current directory.
    fn is_current_directory(&mut self) -> bool {
        let (path, len) = self.path(level::PATH);
        let drives = self.lastdrive();
        (0..drives).any(|drive| {
            let (current, current_len) = self.current_directory(drive);
            current[..current_len] == path[..len]
        })
    }
}

/// The directory entry a new file or directory named `field` gets.
fn new_entry(field: [u8; 11], attributes: u8, stamp: u32, cluster: u16) -> ShortEntry {
    let [time_low, time_high, date_low, date_high] = stamp.to_le_bytes();
    let mut entry = ShortEntry {
        name: field,
        attributes,
        cluster: u32::from(cluster),
        ..ShortEntry::default()
    };
    entry.stamps.written.time = u16::from_le_bytes([time_low, time_high]);
    entry.stamps.written.date = u16::from_le_bytes([date_low, date_high]);
    entry
}

/// Resolves the running call's path with `mode`.
fn resolve(phase: &mut u8, next: u8, mode: u8) -> Flow {
    *phase = next;
    Flow::Call(Frame::of(&Resolve::of(mode, false)))
}

const RESOLVED: u8 = 1;
const STAMPED: u8 = 2;
const PLACED: u8 = 3;
const ALLOCATED: u8 = 4;
const CLEARED: u8 = 5;
const LINKED: u8 = 6;
const STORED: u8 = 7;
const UNDONE: u8 = 8;
/// The last phase of every call that changes a directory: its drive's
/// buffers written back.
const WRITTEN_BACK: u8 = 16;

locals! {
    /// Function `39h`: makes the running call's path a directory, its `.`
    /// and `..` in a zeroed cluster of its own. A failure once the cluster
    /// is taken gives the cluster back.
    pub struct MakeDir {
        drive: u8,
        parent: u16,
        stamp: u32,
        cluster: u16,
        code: u16,
    }
}

impl Routine for MakeDir {
    const ID: Id = Id::MakeDir;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if matches!(*phase, CLEARED | LINKED | STORED) {
            if let Err(Fault::Error(code)) = child {
                self.code = code;
                *phase = UNDONE;
                return Flow::Call(Frame::of(&FreeChain {
                    drive: self.drive,
                    cluster: self.cluster,
                    next: 0,
                    freed: 0,
                    read: false,
                }));
            }
        }
        match *phase {
            RESOLVED => {
                let found = take!(child);
                if found & resolved::ROOT != 0 || found & resolved::EXISTS != 0 {
                    return Flow::Return(Err(Fault::Error(error::ACCESS_DENIED)));
                }
                if name::is_wild(&k.name()) {
                    return Flow::Return(Err(Fault::Error(error::PATH_NOT_FOUND)));
                }
                self.drive = k.path_drive(level::PATH);
                self.parent = u16::try_from(found & 0xFFFF).unwrap_or(0);
                *phase = STAMPED;
                Flow::Call(Frame::of(&Clock { read: true }))
            }
            STAMPED => {
                self.stamp = take!(child);
                *phase = PLACED;
                Flow::Call(Frame::of(&Slot {
                    drive: self.drive,
                    directory: self.parent,
                    grown: false,
                }))
            }
            PLACED => {
                take!(child);
                *phase = ALLOCATED;
                Flow::Call(Frame::of(&Alloc {
                    drive: self.drive,
                    after: 0,
                    candidate: 0,
                    scanned: 0,
                    found: 0,
                }))
            }
            ALLOCATED => {
                self.cluster = u16::try_from(take!(child)).unwrap_or(0);
                if self.cluster == 0 {
                    return Flow::Return(Err(Fault::Error(error::ACCESS_DENIED)));
                }
                *phase = CLEARED;
                Flow::Call(Frame::of(&Clear {
                    drive: self.drive,
                    cluster: self.cluster,
                    left: 0,
                    started: false,
                }))
            }
            CLEARED => {
                take!(child);
                let Some(d) = k.dpb(self.drive) else {
                    return Flow::Return(Err(Fault::Error(error::INVALID_DRIVE)));
                };
                *phase = LINKED;
                Flow::Call(Frame::of(&Buffer {
                    drive: self.drive,
                    sector: d.cluster_sector(self.cluster),
                    kind: 0,
                    fresh: false,
                    victim: 0,
                }))
            }
            LINKED => {
                self.link(k, u16::try_from(take!(child)).unwrap_or(0));
                *phase = STORED;
                Flow::Call(Frame::of(&Store::found(k, self.drive)))
            }
            STORED => {
                take!(child);
                *phase = WRITTEN_BACK;
                written_back(self.drive)
            }
            WRITTEN_BACK => {
                take!(child);
                Flow::Return(Ok(0))
            }
            UNDONE => Flow::Return(Err(Fault::Error(self.code))),
            _ => redirect::served(k, level::PATH, false, |k, remote| {
                redirect::make_dir(k, remote).map(|()| 0)
            })
            .unwrap_or_else(|| resolve(phase, RESOLVED, mode::PARENT)),
        }
    }
}

impl MakeDir {
    /// Writes the new directory's `.` and `..` into the buffer at `segment`,
    /// its cluster's first sector, and stages the parent's entry for it.
    fn link(&self, k: &mut Kernel<'_, '_>, segment: u16) {
        let dot = new_entry(DOT, attr::DIRECTORY, self.stamp, self.cluster);
        let dotdot = new_entry(DOTDOT, attr::DIRECTORY, self.stamp, self.parent);
        k.g.write_bytes(segment, super::data::buffer::DATA, &dot.encode());
        k.g.write_bytes(
            segment,
            super::data::buffer::DATA + ENTRY_BYTES,
            &dotdot.encode(),
        );
        k.dirty(segment);
        let entry = new_entry(k.name(), attr::DIRECTORY, self.stamp, self.cluster);
        k.set_found_entry(&entry);
    }
}

locals! {
    /// Function `3Ah`: removes the empty directory the running call's path
    /// names; neither a root nor a drive's current directory.
    pub struct RemoveDir {
        drive: u8,
        cluster: u16,
        sector: u32,
        slot: u16,
    }
}

const SCANNED: u8 = 2;
const ERASED: u8 = 3;
const FREED: u8 = 4;

impl Routine for RemoveDir {
    const ID: Id = Id::RemoveDir;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            RESOLVED => {
                let found = take!(child);
                if found & resolved::ROOT != 0 {
                    return Flow::Return(Err(Fault::Error(error::ACCESS_DENIED)));
                }
                let entry = k.found_entry();
                if !entry.is_directory() {
                    return Flow::Return(Err(Fault::Error(error::PATH_NOT_FOUND)));
                }
                if k.is_current_directory() {
                    return Flow::Return(Err(Fault::Error(error::CURRENT_DIRECTORY)));
                }
                self.drive = k.path_drive(level::PATH);
                self.cluster = first_cluster(&entry);
                self.sector = k.found_sector();
                self.slot = k.found16(found::SLOT);
                let mut kept = k.found();
                kept[0] = DELETED;
                let at = k.scratch(level::KEPT);
                k.g.write_bytes(SEGMENT, at, &kept);
                k.set_name(&[b'?'; 11]);
                self.scan(phase, 0)
            }
            SCANNED => match child {
                Ok(index) => {
                    if k.found_entry().is_dot() {
                        let next = u16::try_from(index + 1).unwrap_or(u16::MAX);
                        return self.scan(phase, next);
                    }
                    Flow::Return(Err(Fault::Error(error::ACCESS_DENIED)))
                }
                Err(Fault::Error(error::NO_MORE_FILES)) => {
                    *phase = ERASED;
                    Flow::Call(Frame::of(&Store {
                        drive: self.drive,
                        sector: self.sector,
                        slot: self.slot,
                        from: level::KEPT,
                    }))
                }
                Err(fault) => Flow::Return(Err(fault)),
            },
            ERASED => {
                take!(child);
                *phase = FREED;
                Flow::Call(Frame::of(&FreeChain {
                    drive: self.drive,
                    cluster: self.cluster,
                    next: 0,
                    freed: 0,
                    read: false,
                }))
            }
            FREED => {
                take!(child);
                *phase = WRITTEN_BACK;
                written_back(self.drive)
            }
            WRITTEN_BACK => {
                take!(child);
                Flow::Return(Ok(0))
            }
            _ => redirect::served(k, level::PATH, false, |k, remote| {
                redirect::remove_dir(k, remote).map(|()| 0)
            })
            .unwrap_or_else(|| resolve(phase, RESOLVED, mode::EXISTING)),
        }
    }
}

impl RemoveDir {
    /// Looks for an entry in the directory from `index`.
    fn scan(&self, phase: &mut u8, index: u16) -> Flow {
        *phase = SCANNED;
        Flow::Call(Frame::of(&DirScan {
            drive: self.drive,
            directory: self.cluster,
            index,
            attributes: HIDDEN_KINDS,
            mode: scan::FIND,
            cluster: 0,
            position: 0,
        }))
    }
}

locals! {
    /// Function `3Bh`: makes the directory the running call's path names its
    /// drive's current one.
    pub struct ChangeDir {}
}

impl Routine for ChangeDir {
    const ID: Id = Id::ChangeDir;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if *phase != RESOLVED {
            return redirect::served(k, level::PATH, false, |k, remote| {
                redirect::change_dir(k, remote).map(|()| 0)
            })
            .unwrap_or_else(|| resolve(phase, RESOLVED, mode::EXISTING));
        }
        let found = take!(child);
        let cluster = if found & resolved::ROOT != 0 {
            0
        } else {
            let entry = k.found_entry();
            if !entry.is_directory() {
                return Flow::Return(Err(Fault::Error(error::PATH_NOT_FOUND)));
            }
            first_cluster(&entry)
        };
        let (path, len) = k.path(level::PATH);
        if len >= usize::from(cds::PATH_LEN) {
            return Flow::Return(Err(Fault::Error(error::PATH_NOT_FOUND)));
        }
        let drive = k.path_drive(level::PATH);
        let at = cds_at(drive);
        k.g.write_bytes(SEGMENT, at + cds::PATH, &path[..len]);
        k.set8(at + cds::PATH + u16::try_from(len).unwrap_or(0), 0);
        k.set16(at + cds::CLUSTER, cluster);
        Flow::Return(Ok(0))
    }
}

locals! {
    /// Function `41h`: deletes the file the running call's path names: its
    /// entry marked deleted first, then its clusters freed, so a failure
    /// between the two loses clusters rather than linking them twice.
    pub struct Delete {
        drive: u8,
        cluster: u16,
    }
}

impl Routine for Delete {
    const ID: Id = Id::Delete;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            RESOLVED => {
                let found = take!(child);
                if found & resolved::ROOT != 0 {
                    return Flow::Return(Err(Fault::Error(error::ACCESS_DENIED)));
                }
                let mut entry = k.found_entry();
                if entry.attributes & (attr::DIRECTORY | attr::VOLUME_ID | attr::READ_ONLY) != 0 {
                    return Flow::Return(Err(Fault::Error(error::ACCESS_DENIED)));
                }
                self.drive = k.path_drive(level::PATH);
                self.cluster = first_cluster(&entry);
                entry.name[0] = DELETED;
                k.set_found_entry(&entry);
                *phase = ERASED;
                Flow::Call(Frame::of(&Store::found(k, self.drive)))
            }
            ERASED => {
                take!(child);
                if !k.dpb(self.drive).is_some_and(|d| d.in_volume(self.cluster)) {
                    *phase = WRITTEN_BACK;
                    return written_back(self.drive);
                }
                *phase = FREED;
                Flow::Call(Frame::of(&FreeChain {
                    drive: self.drive,
                    cluster: self.cluster,
                    next: 0,
                    freed: 0,
                    read: false,
                }))
            }
            FREED => {
                take!(child);
                *phase = WRITTEN_BACK;
                written_back(self.drive)
            }
            WRITTEN_BACK => {
                take!(child);
                Flow::Return(Ok(0))
            }
            _ => {
                if refers_to_device(k) {
                    return Flow::Return(Err(Fault::Error(error::ACCESS_DENIED)));
                }
                redirect::served(k, level::PATH, false, |k, remote| {
                    redirect::delete(k, remote).map(|()| 0)
                })
                .unwrap_or_else(|| resolve(phase, RESOLVED, mode::EXISTING))
            }
        }
    }
}

/// Whether the running call's path names a character device.
fn refers_to_device(k: &mut Kernel<'_, '_>) -> bool {
    let (path, len) = k.path(level::PATH);
    let Ok(canonical) = k.canonical(&path[..len]) else {
        return false;
    };
    canonical
        .last()
        .and_then(|last| name::field(last, false).ok())
        .and_then(|field| k.char_device(&field))
        .is_some()
}

locals! {
    /// Function `43h`: the attributes of the file or directory the running
    /// call's path names, or, with `set`, sets them to `value` — the
    /// directory and label bits excepted, which no call changes.
    pub struct Attributes {
        set: bool,
        value: u8,
    }
}

impl Routine for Attributes {
    const ID: Id = Id::Attributes;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            RESOLVED => {
                let found = take!(child);
                if found & resolved::ROOT != 0 {
                    return Flow::Return(Err(Fault::Error(error::FILE_NOT_FOUND)));
                }
                let mut entry = k.found_entry();
                if !self.set {
                    return Flow::Return(Ok(u32::from(entry.attributes)));
                }
                let fixed = attr::DIRECTORY | attr::VOLUME_ID;
                if self.value & attr::VOLUME_ID != 0
                    || (self.value ^ entry.attributes) & attr::DIRECTORY != 0
                {
                    return Flow::Return(Err(Fault::Error(error::ACCESS_DENIED)));
                }
                entry.attributes = entry.attributes & fixed | self.value & !fixed;
                k.set_found_entry(&entry);
                let drive = k.path_drive(level::PATH);
                *phase = STORED;
                Flow::Call(Frame::of(&Store::found(k, drive)))
            }
            STORED => {
                take!(child);
                Flow::Return(Ok(0))
            }
            _ => {
                let set = self.set.then_some(self.value);
                redirect::served(k, level::PATH, false, |k, remote| {
                    let held = redirect::attributes_of(k, remote, set)?;
                    Ok(if set.is_some() { 0 } else { u32::from(held) })
                })
                .unwrap_or_else(|| resolve(phase, RESOLVED, mode::EXISTING))
            }
        }
    }
}

/// The DTA's fields a directory search keeps its place in and reports its
/// finding through.
pub(crate) mod dta {
    /// The drive, `1` for A:.
    pub const DRIVE: u16 = 0x00;
    /// The pattern, as an entry holds a name.
    pub const PATTERN: u16 = 0x01;
    /// The attributes searched for.
    pub const ATTRIBUTES: u16 = 0x0C;
    /// The index of the entry last found.
    pub const INDEX: u16 = 0x0D;
    /// The directory's first cluster.
    pub const DIRECTORY: u16 = 0x0F;
    /// Bytes the search uses.
    pub const LEN: u16 = 0x2B;
}

/// What a directory search reports of the entry it found, in the DTA.
pub(crate) mod finding {
    pub const ATTRIBUTE: u16 = 0x15;
    pub const TIME: u16 = 0x16;
    pub const DATE: u16 = 0x18;
    pub const SIZE: u16 = 0x1A;
    /// `NAME.EXT` and a terminator.
    pub const NAME: u16 = 0x1E;
}

locals! {
    /// Functions `4Eh` and `4Fh`: finds the first entry the running call's
    /// path — a pattern — names with attributes `attributes`, or, `next`,
    /// the next after the one the DTA last reported.
    pub struct Find {
        next: bool,
        attributes: u8,
        drive: u8,
        directory: u16,
        index: u16,
    }
}

const CHECKED: u8 = 9;

impl Routine for Find {
    const ID: Id = Id::Find;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let at = k.dta();
        match *phase {
            RESOLVED => {
                let found = take!(child);
                if found & resolved::ROOT != 0 {
                    return Flow::Return(Err(Fault::Error(error::NO_MORE_FILES)));
                }
                self.drive = k.path_drive(level::PATH);
                self.directory = u16::try_from(found & 0xFFFF).unwrap_or(0);
                self.index = 0;
                let mut head = [0u8; finding::ATTRIBUTE as usize];
                head[usize::from(dta::DRIVE)] = self.drive + 1;
                head[1..12].copy_from_slice(&k.name());
                head[usize::from(dta::ATTRIBUTES)] = self.attributes;
                head[0x0D..0x0F].copy_from_slice(&u16::MAX.to_le_bytes());
                head[0x0F..0x11].copy_from_slice(&self.directory.to_le_bytes());
                k.g.write_bytes(at.segment, at.offset, &head);
                self.scan(phase)
            }
            CHECKED => {
                take!(child);
                self.scan(phase)
            }
            SCANNED => {
                let index = match child {
                    Ok(index) => index,
                    Err(fault) => {
                        k.write16_at(at.add(dta::INDEX), u16::MAX);
                        return Flow::Return(Err(fault));
                    }
                };
                let entry = k.found_entry();
                let mut record = [0u8; (dta::LEN - finding::ATTRIBUTE) as usize];
                record[0] = entry.attributes;
                let time = usize::from(finding::TIME - finding::ATTRIBUTE);
                record[time..time + 2].copy_from_slice(&entry.stamps.written.time.to_le_bytes());
                record[time + 2..time + 4]
                    .copy_from_slice(&entry.stamps.written.date.to_le_bytes());
                record[time + 4..time + 8].copy_from_slice(&entry.size.to_le_bytes());
                let mut spelt = [0u8; 13];
                let len = name::spell(&entry.name, &mut spelt);
                let from = usize::from(finding::NAME - finding::ATTRIBUTE);
                record[from..from + len].copy_from_slice(&spelt[..len]);
                k.write16_at(at.add(dta::INDEX), u16::try_from(index).unwrap_or(u16::MAX));
                let place = at.add(finding::ATTRIBUTE);
                k.g.write_bytes(place.segment, place.offset, &record);
                Flow::Return(Ok(0))
            }
            _ if self.next => {
                if redirect::is_remote_search(k, at) {
                    return Flow::Return(redirect::find_next(k, at).map(|()| 0));
                }
                let drive = k.g.read8(at.segment, at.offset);
                if drive & 0x80 != 0 || drive == 0 || k.dpb(drive - 1).is_none() {
                    return Flow::Return(Err(Fault::Error(error::NO_MORE_FILES)));
                }
                self.drive = drive - 1;
                let mut pattern = [0u8; 11];
                let from = at.add(dta::PATTERN);
                k.g.read_bytes(from.segment, from.offset, &mut pattern);
                k.set_name(&pattern);
                self.attributes =
                    k.g.read8(at.segment, at.offset.wrapping_add(dta::ATTRIBUTES));
                self.index = k.read16_at(at.add(dta::INDEX)).saturating_add(1);
                self.directory = k.read16_at(at.add(dta::DIRECTORY));
                *phase = CHECKED;
                Flow::Call(Frame::of(&Media {
                    drive: self.drive,
                    status: 0,
                }))
            }
            _ => {
                let attributes = self.attributes;
                redirect::served(k, level::PATH, true, |k, remote| {
                    redirect::find_first(k, remote, attributes, at).map(|()| 0)
                })
                .unwrap_or_else(|| resolve(phase, RESOLVED, mode::PARENT))
            }
        }
    }
}

impl Find {
    fn scan(self, phase: &mut u8) -> Flow {
        *phase = SCANNED;
        Flow::Call(Frame::of(&DirScan {
            drive: self.drive,
            directory: self.directory,
            index: self.index,
            attributes: self.attributes,
            mode: scan::FIND,
            cluster: 0,
            position: 0,
        }))
    }
}

locals! {
    /// Function `56h`: renames the file or directory the running call's
    /// path names to its second path, on the same drive; a file may move
    /// to another directory, a directory may not.
    pub struct Rename {
        drive: u8,
        directory: u16,
        sector: u32,
        slot: u16,
        is_directory: bool,
    }
}

const OLD_FOUND: u8 = 1;
const NEW_FOUND: u8 = 2;
const SLOTTED: u8 = 3;
const MOVED: u8 = 4;
const RENAMED: u8 = 5;

impl Routine for Rename {
    const ID: Id = Id::Rename;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            OLD_FOUND => {
                let found = take!(child);
                if found & resolved::ROOT != 0 {
                    return Flow::Return(Err(Fault::Error(error::ACCESS_DENIED)));
                }
                let entry = k.found_entry();
                if entry.attributes & attr::VOLUME_ID != 0 {
                    return Flow::Return(Err(Fault::Error(error::ACCESS_DENIED)));
                }
                self.drive = k.path_drive(level::PATH);
                self.directory = u16::try_from(found & 0xFFFF).unwrap_or(0);
                self.sector = k.found_sector();
                self.slot = k.found16(found::SLOT);
                self.is_directory = entry.is_directory();
                let kept = k.found();
                let at = k.scratch(level::KEPT);
                k.g.write_bytes(SEGMENT, at, &kept);
                *phase = NEW_FOUND;
                Flow::Call(Frame::of(&Resolve::of(mode::PARENT, true)))
            }
            NEW_FOUND => {
                let found = take!(child);
                if k.path_drive(level::PATH2) != self.drive {
                    return Flow::Return(Err(Fault::Error(error::NOT_SAME_DEVICE)));
                }
                if found & (resolved::ROOT | resolved::EXISTS) != 0 {
                    return Flow::Return(Err(Fault::Error(error::ACCESS_DENIED)));
                }
                let field = k.name();
                if name::is_wild(&field) {
                    return Flow::Return(Err(Fault::Error(error::PATH_NOT_FOUND)));
                }
                let at = k.scratch(level::KEPT);
                k.g.write_bytes(SEGMENT, at, &field);
                let directory = u16::try_from(found & 0xFFFF).unwrap_or(0);
                if directory == self.directory {
                    *phase = RENAMED;
                    return Flow::Call(Frame::of(&Store {
                        drive: self.drive,
                        sector: self.sector,
                        slot: self.slot,
                        from: level::KEPT,
                    }));
                }
                if self.is_directory {
                    return Flow::Return(Err(Fault::Error(error::ACCESS_DENIED)));
                }
                *phase = SLOTTED;
                Flow::Call(Frame::of(&Slot {
                    drive: self.drive,
                    directory,
                    grown: false,
                }))
            }
            SLOTTED => {
                take!(child);
                *phase = MOVED;
                Flow::Call(Frame::of(&Store {
                    drive: self.drive,
                    sector: k.found_sector(),
                    slot: k.found16(found::SLOT),
                    from: level::KEPT,
                }))
            }
            MOVED => {
                take!(child);
                let at = k.scratch(level::KEPT);
                k.set8(at, DELETED);
                *phase = RENAMED;
                Flow::Call(Frame::of(&Store {
                    drive: self.drive,
                    sector: self.sector,
                    slot: self.slot,
                    from: level::KEPT,
                }))
            }
            RENAMED => {
                take!(child);
                *phase = WRITTEN_BACK;
                written_back(self.drive)
            }
            WRITTEN_BACK => {
                take!(child);
                Flow::Return(Ok(0))
            }
            _ => {
                if refers_to_device(k) {
                    return Flow::Return(Err(Fault::Error(error::ACCESS_DENIED)));
                }
                if let Some(flow) = renamed_remotely(k) {
                    return flow;
                }
                *phase = OLD_FOUND;
                Flow::Call(Frame::of(&Resolve::of(mode::EXISTING, false)))
            }
        }
    }
}

/// Function `56h` where either name is on a shared drive: both must be on
/// the same one.
fn renamed_remotely(k: &mut Kernel<'_, '_>) -> Option<Flow> {
    let both = redirect::remote_path(k, level::PATH, false)
        .and_then(|from| Ok((from, redirect::remote_path(k, level::PATH2, false)?)));
    Some(Flow::Return(match both {
        Ok((None, None)) => return None,
        Ok((Some(from), Some(to))) => redirect::rename(k, &from, &to).map(|()| 0),
        Ok(_) => Err(Fault::Error(error::NOT_SAME_DEVICE)),
        Err(fault) => Err(fault),
    }))
}

/// Names function `5Ah` tries before it gives up.
const TEMP_TRIES: u16 = 64;

locals! {
    /// Function `5Ah`: creates a file of attributes `attributes` with a name
    /// no file in the directory the caller's path names has, made from the
    /// tick count; the name is appended to the caller's path. Returns the
    /// handle.
    pub struct TempFile {
        attributes: u8,
        path: Far,
        tries: u16,
        seed: u16,
    }
}

const CREATED: u8 = 1;

impl Routine for TempFile {
    const ID: Id = Id::TempFile;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if *phase == CREATED {
            match child {
                Ok(handle) => {
                    let (_, len) = caller_path(k, self.path);
                    let at = self.path.add(u16::try_from(len).unwrap_or(0));
                    k.g.write_bytes(at.segment, at.offset, &temp_name(self.seed, self.tries));
                    k.g.write8(at.segment, at.offset.wrapping_add(8), 0);
                    return Flow::Return(Ok(handle));
                }
                Err(Fault::Error(error::FILE_EXISTS)) if self.tries + 1 < TEMP_TRIES => {
                    self.tries += 1;
                }
                Err(fault) => return Flow::Return(Err(fault)),
            }
        } else {
            self.seed =
                k.g.read16(crate::bios::bda::SEGMENT, crate::bios::bda::TICKS);
        }
        let (mut path, len) = caller_path(k, self.path);
        if len + 8 >= path.len() {
            return Flow::Return(Err(Fault::Error(error::PATH_NOT_FOUND)));
        }
        path[len..len + 8].copy_from_slice(&temp_name(self.seed, self.tries));
        k.set_path(level::PATH, &path[..len + 8]);
        *phase = CREATED;
        Flow::Call(Frame::of(&Create {
            attributes: self.attributes,
            new: true,
            mode: access::BOTH,
            handle: 0,
        }))
    }
}

/// The caller's ASCIZ path at `at`, at most 127 bytes, and its length.
pub fn caller_path(k: &mut Kernel<'_, '_>, at: Far) -> ([u8; 128], usize) {
    let mut path = [0u8; 128];
    k.g.read_bytes(at.segment, at.offset, &mut path[..127]);
    let len = path.iter().position(|&b| b == 0).unwrap_or(127);
    (path, len)
}

/// The eight hexadecimal digits function `5Ah` names its `tries`th file
/// with.
fn temp_name(seed: u16, tries: u16) -> [u8; 8] {
    let value = u32::from(seed) << 16 | u32::from(tries.wrapping_mul(0x9E37) ^ seed.rotate_left(7));
    let mut out = [0u8; 8];
    super::message::hex(value, &mut out);
    out
}

locals! {
    /// Function `6Ch`: opens the running call's path with `mode`, or creates
    /// it with `attributes`, as `action` says: its low nibble what to do
    /// when the file exists (`0` fail, `1` open, `2` replace), its high one
    /// when it does not (`0` fail, `1` create). Returns the handle, and
    /// above it what was done: `1` opened, `2` created, `3` replaced.
    pub struct ExtendedOpen {
        mode: u8,
        attributes: u8,
        action: u8,
        handle: u16,
    }
}

const OPENED: u8 = 1;
const REPLACED: u8 = 2;
const REFUSED: u8 = 3;
const MADE: u8 = 4;

impl Routine for ExtendedOpen {
    const ID: Id = Id::ExtendedOpen;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let exists = self.action & 0x0F;
        let absent = self.action >> 4;
        match *phase {
            OPENED => match child {
                Ok(handle) => {
                    self.handle = u16::try_from(handle).unwrap_or(0);
                    match exists {
                        1 => Flow::Return(Ok(u32::from(self.handle) | 1 << 16)),
                        2 => {
                            let (index, entry) = match k.handle(self.handle) {
                                Ok(found) => found,
                                Err(fault) => return Flow::Return(Err(fault)),
                            };
                            if k.sft_device(entry).is_some() {
                                return Flow::Return(Ok(u32::from(self.handle) | 3 << 16));
                            }
                            k.set_sft32(entry, super::data::sft::POSITION, 0);
                            *phase = REPLACED;
                            Flow::Call(Frame::of(&Write {
                                index: u16::from(index),
                                buffer: Far::default(),
                                count: 0,
                                done: 0,
                                kept: 0,
                                next: 0,
                            }))
                        }
                        _ => {
                            *phase = REFUSED;
                            Flow::Call(Frame::of(&Close {
                                handle: self.handle,
                                index: 0,
                                by_index: false,
                            }))
                        }
                    }
                }
                Err(Fault::Error(error::FILE_NOT_FOUND)) if absent == 1 => {
                    *phase = MADE;
                    Flow::Call(Frame::of(&Create {
                        attributes: self.attributes,
                        new: true,
                        mode: self.mode,
                        handle: 0,
                    }))
                }
                Err(fault) => Flow::Return(Err(fault)),
            },
            REPLACED => {
                take!(child);
                Flow::Return(Ok(u32::from(self.handle) | 3 << 16))
            }
            REFUSED => {
                take!(child);
                Flow::Return(Err(Fault::Error(error::FILE_EXISTS)))
            }
            MADE => {
                let handle = take!(child);
                Flow::Return(Ok(handle | 2 << 16))
            }
            _ => {
                if exists > 2 || absent > 1 || self.mode & access::MASK > access::BOTH {
                    return Flow::Return(Err(Fault::Error(error::INVALID_FUNCTION)));
                }
                *phase = OPENED;
                Flow::Call(Frame::of(&Open {
                    mode: self.mode,
                    keep_handle: false,
                    index: 0,
                    handle: 0,
                    drive: 0,
                }))
            }
        }
    }
}

locals! {
    /// Function `47h`: drive `drive`'s current directory, without its drive
    /// and root, copied ASCIZ to the caller's DS:SI. The medium is checked
    /// first, as programs ask this to learn of a change, and a directory a
    /// change left unknown is looked up again: one the new medium lacks, or
    /// cannot be read for, leaves the drive at its root, as DOS's
    /// `ValidateCDS` does.
    pub struct CurrentDir {
        drive: u8,
    }
}

/// [`CurrentDir`]'s phases.
mod current {
    pub const CHECKED: u8 = 1;
    pub const LOOKED_UP: u8 = 2;
}

impl Routine for CurrentDir {
    const ID: Id = Id::CurrentDir;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let at = cds_at(self.drive);
        match *phase {
            // A shared drive has no medium to check, its directory kept by DOS.
            0 if k.share_of(self.drive).is_some() => self.copy_out(k),
            0 => {
                *phase = current::CHECKED;
                Flow::Call(Frame::of(&Media {
                    drive: self.drive,
                    status: 0,
                }))
            }
            current::CHECKED => {
                take!(child);
                if k.get16(at + cds::CLUSTER) != cds::UNKNOWN {
                    return self.copy_out(k);
                }
                let (path, len) = k.current_directory(self.drive);
                k.set_path(level::PATH, &path[..len]);
                *phase = current::LOOKED_UP;
                Flow::Call(Frame::of(&ChangeDir {}))
            }
            _ => {
                match child {
                    Ok(_) => {}
                    Err(Fault::Error(_)) => {
                        let root = k.get16(at + cds::ROOT).min(cds::PATH_LEN - 2);
                        k.set8(at + cds::PATH + root + 1, 0);
                        k.set16(at + cds::CLUSTER, 0);
                    }
                    Err(fault) => return Flow::Return(Err(fault)),
                }
                self.copy_out(k)
            }
        }
    }
}

impl CurrentDir {
    fn copy_out(self, k: &mut Kernel<'_, '_>) -> Flow {
        let buffer = k.caller_far(Saved::Ds, Saved::Si);
        let (path, len) = k.current_directory(self.drive);
        let root = usize::from(k.get16(cds_at(self.drive) + cds::ROOT)).min(len);
        let rest = path.get(root + 1..len).unwrap_or(&[]);
        k.g.write_bytes(buffer.segment, buffer.offset, rest);
        let end = buffer.add(u16::try_from(rest.len()).unwrap_or(0));
        k.g.write8(end.segment, end.offset, 0);
        Flow::Return(Ok(0))
    }
}

/// Function `60h`: the canonical form of the path at `from`, copied ASCIZ
/// to `to`; a character device's as `X:/NAME`.
///
/// # Errors
///
/// The extended error a path no canonical one is gives.
pub fn truename(k: &mut Kernel<'_, '_>, from: Far, to: Far) -> Result<(), u16> {
    let (path, len) = caller_path(k, from);
    let canonical = k.canonical(&path[..len]).map_err(name_error)?;
    let device = canonical
        .last()
        .and_then(|last| name::field(last, false).ok())
        .filter(|field| k.char_device(field).is_some());
    let mut out = [0u8; PATH_MAX + 1];
    let len = if let Some(field) = device {
        let mut spelt = [0u8; 13];
        let n = name::spell(&field, &mut spelt);
        out[0] = canonical.as_bytes()[0];
        out[1] = b':';
        out[2] = b'/';
        out[3..3 + n].copy_from_slice(&spelt[..n]);
        3 + n
    } else {
        let bytes = canonical.as_bytes();
        out[..bytes.len()].copy_from_slice(bytes);
        bytes.len()
    };
    k.g.write_bytes(to.segment, to.offset, &out[..=len]);
    Ok(())
}

/// Function `67h`: the current process's handle table made `count` handles
/// long — its own twenty in the PSP up to that many, a block of its own
/// beyond — every open handle kept.
///
/// # Errors
///
/// [`error::TOO_MANY_OPEN`] when an open handle lies beyond `count`; the
/// allocation's error when no block is free.
pub fn set_handle_count(k: &mut Kernel<'_, '_>, count: u16) -> Result<(), u16> {
    let process = k.psp();
    let (table, size) = k.jft(process);
    for handle in count..size {
        let at = table.add(handle);
        if k.g.read8(at.segment, at.offset) != CLOSED {
            return Err(error::TOO_MANY_OPEN);
        }
    }
    let internal = Far::new(process, psp::JFT);
    let fresh = if count <= psp::JFT_COUNT {
        internal
    } else {
        let rule = k.strategy();
        let segment = k
            .allocate(count.div_ceil(16), process, rule)
            .map_err(super::memory::Refusal::error)?;
        Far::new(segment, 0)
    };
    if fresh == table {
        for handle in size..count {
            let to = fresh.add(handle);
            k.g.write8(to.segment, to.offset, CLOSED);
        }
    } else {
        for handle in 0..count {
            let index = if handle < size {
                let at = table.add(handle);
                k.g.read8(at.segment, at.offset)
            } else {
                CLOSED
            };
            let to = fresh.add(handle);
            k.g.write8(to.segment, to.offset, index);
        }
        let owned = table.offset == 0
            && k.block(table.segment.wrapping_sub(1))
                .is_some_and(|block| block.owner == process);
        if table != internal && owned {
            let _ = k.free(table.segment);
        }
    }
    k.g.write16(process, psp::JFT_SIZE, count);
    k.g.write16(process, psp::JFT_POINTER, fresh.offset);
    k.g.write16(process, psp::JFT_POINTER + 2, fresh.segment);
    Ok(())
}
