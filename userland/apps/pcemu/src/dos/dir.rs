//! Directories: scanning one — the root's fixed region or a chain of
//! clusters — for an entry a pattern names or a slot to put one in, and
//! resolving a path to the directory and the entry it names.

use super::data::{buffer, level, SEGMENT};
use super::engine::{locals, take, Fault, Flow, Frame, Id, Kernel, Outcome, Routine};
use super::fs::{cds_at, error, Buffer, Dpb, Media, Walk};
use super::name::{self, Canonical, NameError, PATH_MAX};
use tairix_fat::dir::{attr, slot, ShortEntry, Slot, ENTRY};

/// Bytes of a directory entry, as offsets in a sector take them.
pub const ENTRY_BYTES: u16 = 32;
const _: () = assert!(ENTRY_BYTES as usize == ENTRY);

/// Where the last entry found lies, after its 32 bytes in the scratch.
pub mod found {
    /// The sector holding it.
    pub const SECTOR: u16 = 0x20;
    /// Its index in that sector.
    pub const SLOT: u16 = 0x24;
    /// Its index in its directory.
    pub const INDEX: u16 = 0x26;
    /// Its directory's first cluster, `0` for the root.
    pub const DIRECTORY: u16 = 0x28;
}

/// What a scan looks for.
pub mod scan {
    /// An entry matching the pattern in the scratch's name.
    pub const FIND: u8 = 0;
    /// A slot no entry uses.
    pub const FREE: u8 = 1;
}

/// The attribute bits that hide a file from a search not asking for them.
pub const HIDDEN_FILES: u8 = attr::HIDDEN | attr::SYSTEM;
/// The attribute bits a search must ask for to see an entry carrying them.
pub const HIDDEN_KINDS: u8 = HIDDEN_FILES | attr::DIRECTORY;

/// Whether an entry with attributes `entry` answers a search for
/// `search`, as DOS's matching rule has it.
#[must_use]
pub const fn attributes_match(search: u8, entry: u8) -> bool {
    if entry == attr::LONG_NAME {
        return false;
    }
    if search == attr::VOLUME_ID {
        return entry & attr::VOLUME_ID != 0;
    }
    if entry & attr::VOLUME_ID != 0 {
        return search & attr::VOLUME_ID != 0;
    }
    entry & HIDDEN_KINDS & !search == 0
}

impl Kernel<'_, '_> {
    /// The entry the running call last found.
    pub fn found(&mut self) -> [u8; ENTRY] {
        let at = self.scratch(level::FOUND);
        let mut entry = [0u8; ENTRY];
        self.g.read_bytes(SEGMENT, at, &mut entry);
        entry
    }

    /// The entry the running call last found, decoded.
    pub fn found_entry(&mut self) -> ShortEntry {
        ShortEntry::decode(&self.found())
    }

    /// Sets the entry the running call holds as found, its location kept.
    pub fn set_found_entry(&mut self, entry: &ShortEntry) {
        let at = self.scratch(level::FOUND);
        self.g.write_bytes(SEGMENT, at, &entry.encode());
    }

    /// A field of where it lies.
    pub fn found16(&mut self, field: u16) -> u16 {
        let at = self.scratch(level::FOUND + field);
        self.get16(at)
    }

    /// The sector it lies in.
    pub fn found_sector(&mut self) -> u32 {
        let at = self.scratch(level::FOUND + found::SECTOR);
        self.get32(at)
    }

    /// The running call's name: a pattern, or a component.
    pub fn name(&mut self) -> [u8; 11] {
        let at = self.scratch(level::NAME);
        let mut out = [0u8; 11];
        self.g.read_bytes(SEGMENT, at, &mut out);
        out
    }

    /// Sets it.
    pub fn set_name(&mut self, field: &[u8; 11]) {
        let at = self.scratch(level::NAME);
        self.g.write_bytes(SEGMENT, at, field);
    }

    /// The running call's path, as far as its terminator.
    pub fn path(&mut self, which: u16) -> ([u8; 128], usize) {
        let at = self.scratch(which);
        let mut out = [0u8; 128];
        self.g.read_bytes(SEGMENT, at, &mut out);
        let len = out.iter().position(|&b| b == 0).unwrap_or(out.len());
        (out, len)
    }

    /// Sets it, ASCIZ.
    pub fn set_path(&mut self, which: u16, path: &[u8]) {
        let at = self.scratch(which);
        let len = path.len().min(127);
        self.g.write_bytes(SEGMENT, at, &path[..len]);
        self.set8(at + u16::try_from(len).unwrap_or(127), 0);
    }

    /// The current drive.
    pub fn current_drive(&mut self) -> u8 {
        self.sda8(super::data::sda::DRIVE)
    }

    /// Drive letters there are.
    pub fn lastdrive(&mut self) -> u8 {
        let at = u16::try_from(
            i32::from(super::data::SYSVARS) + i32::from(super::data::sysvars::LASTDRIVE),
        )
        .unwrap_or(0);
        self.get8(at).clamp(1, super::data::DRIVES)
    }

    /// `drive`'s current directory, as its CDS holds it.
    pub fn current_directory(&mut self, drive: u8) -> ([u8; PATH_MAX], usize) {
        let mut out = [0u8; PATH_MAX];
        self.g
            .read_bytes(SEGMENT, cds_at(drive) + super::data::cds::PATH, &mut out);
        let len = out.iter().position(|&b| b == 0).unwrap_or(out.len());
        (out, len)
    }

    /// `path` made canonical against the current drive and its directory.
    pub fn canonical(&mut self, path: &[u8]) -> Result<Canonical, NameError> {
        let current = self.current_drive();
        let (drive, rest) = name::drive_of(path, current);
        let drives = self.lastdrive();
        if drive >= drives {
            return Err(NameError::Drive);
        }
        let (directory, len) = self.current_directory(drive);
        name::canonical(drive, rest, &directory[..len], drives)
    }
}

/// The first cluster of `entry`, as DOS reads it: FAT12 and FAT16 keep
/// only the low word.
#[must_use]
pub fn first_cluster(entry: &ShortEntry) -> u16 {
    u16::try_from(entry.cluster & 0xFFFF).unwrap_or(0)
}

/// The extended error a refused name stands for.
#[must_use]
pub const fn name_error(error: NameError) -> u16 {
    match error {
        NameError::Path => error::PATH_NOT_FOUND,
        NameError::File => error::FILE_NOT_FOUND,
        NameError::Drive => error::INVALID_DRIVE,
    }
}

locals! {
    /// Scans `drive`'s directory `directory` (`0` for the root) from entry
    /// `index`: for the first entry matching the running call's name with
    /// attributes `attributes`, or for a free slot. The entry is left in the
    /// running call's scratch, and its index returned; none found is
    /// [`error::NO_MORE_FILES`].
    pub struct DirScan {
        drive: u8,
        directory: u16,
        index: u16,
        attributes: u8,
        mode: u8,
        cluster: u16,
        position: u16,
    }
}

const CALLED: u8 = 1;
const WALKED: u8 = 2;

impl Routine for DirScan {
    const ID: Id = Id::DirScan;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let Some(d) = k.dpb(self.drive) else {
            return Flow::Return(Err(Fault::Error(error::INVALID_DRIVE)));
        };
        match *phase {
            CALLED => {
                take!(child);
                *phase = 0;
            }
            WALKED => {
                let cluster = u16::try_from(take!(child)).unwrap_or(0);
                *phase = 0;
                if cluster == 0 {
                    return Flow::Return(Err(Fault::Error(error::NO_MORE_FILES)));
                }
                self.cluster = cluster;
                self.position = self.index / per_cluster(&d);
            }
            _ => {
                if self.cluster == 0 {
                    self.cluster = self.directory;
                    self.position = 0;
                }
            }
        }
        let pattern = k.name();
        loop {
            let Some(sector) = self.sector(&d) else {
                if self.directory == 0 {
                    return Flow::Return(Err(Fault::Error(error::NO_MORE_FILES)));
                }
                let target = self.index / per_cluster(&d);
                if self.index == u16::MAX || !d.in_volume(self.cluster) {
                    return Flow::Return(Err(Fault::Error(error::NO_MORE_FILES)));
                }
                *phase = WALKED;
                return Flow::Call(Frame::of(&Walk {
                    drive: self.drive,
                    cluster: self.cluster,
                    steps: target - self.position,
                }));
            };
            let Some(segment) = k.find_buffer(d.drive, sector) else {
                *phase = CALLED;
                return Flow::Call(Frame::of(&Buffer {
                    drive: d.drive,
                    sector,
                    kind: 0,
                    fresh: false,
                    victim: 0,
                }));
            };
            k.touch_buffer(segment);
            let per_sector = d.sector_bytes / ENTRY_BYTES;
            let place = self.index % per_sector;
            let mut entry = [0u8; ENTRY];
            k.g.read_bytes(segment, buffer::DATA + place * ENTRY_BYTES, &mut entry);
            let hit = match (self.mode, slot(&entry)) {
                (scan::FREE, Slot::End | Slot::Deleted) => true,
                (scan::FREE, _) | (_, Slot::Deleted | Slot::Long) => false,
                (_, Slot::End) => return Flow::Return(Err(Fault::Error(error::NO_MORE_FILES))),
                _ => {
                    let found = ShortEntry::decode(&entry);
                    attributes_match(self.attributes, found.attributes)
                        && name::matches(&pattern, &found.name)
                }
            };
            if hit {
                let at = k.scratch(level::FOUND);
                k.g.write_bytes(SEGMENT, at, &entry);
                k.set32(at + found::SECTOR, sector);
                k.set16(at + found::SLOT, place);
                k.set16(at + found::INDEX, self.index);
                k.set16(at + found::DIRECTORY, self.directory);
                return Flow::Return(Ok(u32::from(self.index)));
            }
            if self.index == u16::MAX {
                return Flow::Return(Err(Fault::Error(error::NO_MORE_FILES)));
            }
            self.index += 1;
        }
    }
}

/// Entries a cluster holds.
fn per_cluster(d: &Dpb) -> u16 {
    u16::try_from(d.cluster_bytes() / u32::from(ENTRY_BYTES))
        .unwrap_or(u16::MAX)
        .max(1)
}

impl DirScan {
    /// The sector holding entry `index`, when the scan has reached its
    /// cluster; `None` past the root's end, or when the chain must be
    /// walked on first.
    fn sector(&self, d: &Dpb) -> Option<u32> {
        let per_sector = d.sector_bytes / ENTRY_BYTES;
        if self.directory == 0 {
            return (self.index < d.root_entries)
                .then(|| u32::from(d.root) + u32::from(self.index / per_sector));
        }
        let target = self.index / per_cluster(d);
        if target != self.position || !d.in_volume(self.cluster) {
            return None;
        }
        let within = self.index % per_cluster(d) / per_sector;
        Some(d.cluster_sector(self.cluster) + u32::from(within))
    }
}

/// What [`Resolve`] reports, above the directory's cluster.
pub mod resolved {
    /// The last component names an entry, which the scratch holds.
    pub const EXISTS: u32 = 1 << 16;
    /// The path is a drive's root.
    pub const ROOT: u32 = 1 << 17;
}

/// How [`Resolve`] treats the path's last component.
pub mod mode {
    /// It must name an existing entry.
    pub const EXISTING: u8 = 0;
    /// It is looked up in its directory, which must exist; it may be a
    /// pattern.
    pub const PARENT: u8 = 1;
}

locals! {
    /// Resolves the running call's path — its second, when `second` —
    /// made canonical (and written back so), its medium checked, each
    /// directory in it walked from the root. Returns the last component's
    /// directory cluster with [`resolved`] flags; the last component's name
    /// is left in the scratch's name and, when it exists, its entry in the
    /// scratch.
    pub struct Resolve {
        mode: u8,
        drive: u8,
        directory: u16,
        component: u8,
        count: u8,
        second: bool,
    }
}

const CHECKED: u8 = 1;
const STEPPED: u8 = 2;
const LOOKED: u8 = 3;

impl Routine for Resolve {
    const ID: Id = Id::Resolve;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            CHECKED => {
                take!(child);
            }
            STEPPED => {
                if let Err(fault) = child {
                    return Flow::Return(Err(match fault {
                        Fault::Error(error::NO_MORE_FILES) => Fault::Error(error::PATH_NOT_FOUND),
                        other => other,
                    }));
                }
                let entry = k.found_entry();
                if !entry.is_directory() {
                    return Flow::Return(Err(Fault::Error(error::PATH_NOT_FOUND)));
                }
                self.directory = first_cluster(&entry);
                self.component += 1;
            }
            LOOKED => {
                return match child {
                    Ok(_) => Flow::Return(Ok(u32::from(self.directory) | resolved::EXISTS)),
                    Err(Fault::Error(error::NO_MORE_FILES)) if self.mode == mode::PARENT => {
                        Flow::Return(Ok(u32::from(self.directory)))
                    }
                    Err(Fault::Error(error::NO_MORE_FILES)) => {
                        Flow::Return(Err(Fault::Error(error::FILE_NOT_FOUND)))
                    }
                    Err(fault) => Flow::Return(Err(fault)),
                };
            }
            _ => {
                let (path, len) = k.path(self.buffer());
                let canonical = match k.canonical(&path[..len]) {
                    Ok(canonical) => canonical,
                    Err(refused) => return Flow::Return(Err(Fault::Error(name_error(refused)))),
                };
                k.set_path(self.buffer(), canonical.as_bytes());
                self.drive = canonical.drive();
                self.count = u8::try_from(canonical.components().count()).unwrap_or(u8::MAX);
                if k.dpb(self.drive).is_none() {
                    return Flow::Return(Err(Fault::Error(error::INVALID_DRIVE)));
                }
                *phase = CHECKED;
                return Flow::Call(Frame::of(&Media {
                    drive: self.drive,
                    status: 0,
                }));
            }
        }
        let (path, len) = k.path(self.buffer());
        let Some(canonical) = parse(&path[..len]) else {
            return Flow::Return(Err(Fault::Error(error::PATH_NOT_FOUND)));
        };
        if self.count == 0 {
            return Flow::Return(Ok(resolved::ROOT));
        }
        let Some(component) = canonical.components().nth(usize::from(self.component)) else {
            return Flow::Return(Err(Fault::Error(error::PATH_NOT_FOUND)));
        };
        let last = self.component + 1 == self.count;
        let Ok(field) = name::field(component, last) else {
            return Flow::Return(Err(Fault::Error(error::PATH_NOT_FOUND)));
        };
        k.set_name(&field);
        if last {
            if name::is_wild(&field) {
                return if self.mode == mode::PARENT {
                    Flow::Return(Ok(u32::from(self.directory)))
                } else {
                    Flow::Return(Err(Fault::Error(error::FILE_NOT_FOUND)))
                };
            }
            *phase = LOOKED;
            return Flow::Call(Frame::of(&DirScan {
                drive: self.drive,
                directory: self.directory,
                index: 0,
                attributes: HIDDEN_KINDS,
                mode: scan::FIND,
                cluster: 0,
                position: 0,
            }));
        }
        *phase = STEPPED;
        Flow::Call(Frame::of(&DirScan {
            drive: self.drive,
            directory: self.directory,
            index: 0,
            attributes: HIDDEN_KINDS,
            mode: scan::FIND,
            cluster: 0,
            position: 0,
        }))
    }
}

impl Resolve {
    /// The path buffer it resolves.
    const fn buffer(self) -> u16 {
        if self.second {
            level::PATH2
        } else {
            level::PATH
        }
    }
}

/// A canonical path read back from the scratch.
fn parse(path: &[u8]) -> Option<Canonical> {
    if path.len() < 3 || path.len() >= PATH_MAX || path[1] != b':' {
        return None;
    }
    let mut out = Canonical {
        bytes: [0; PATH_MAX],
        len: path.len(),
    };
    out.bytes[..path.len()].copy_from_slice(path);
    Some(out)
}
