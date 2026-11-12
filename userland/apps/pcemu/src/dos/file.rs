//! Open files: the system file table, each process's handle table, and the
//! handle calls — open, create, read, write, seek, commit, close — over
//! files and character devices alike.

use super::char::{CharIo, LineInput};
use super::data::{buffer, level, sft, sysvars, SEGMENT, SFT_ENTRY, SFT_HEADER, SYSVARS};
use super::device::{attribute, command, header, Device};
use super::dir::{
    first_cluster, found, mode, name_error, resolved, scan, DirScan, Resolve, ENTRY_BYTES,
};
use super::dir_write::{Slot, Store};
use super::engine::{locals, take, words, Far, Fault, Flow, Frame, Id, Kernel, Outcome, Routine};
use super::fs::{
    area, dpb_at, error, Alloc, BlockIo, Buffer, Dpb, FatSet, Flush, FreeChain, MediaLabel, Walk,
};
use super::memory::psp;
use super::name::NameError;
use super::redirect::{self, Remote};
use crate::folder::Creation;
use tairix_fat::dir::{attr, ShortEntry, ENTRY};

/// SFT blocks the kernel walks before it counts the chain as damaged.
const MAX_BLOCKS: u16 = 16;
/// Devices it walks.
const MAX_DEVICES: u16 = 128;

/// The device information word's bits.
pub mod info {
    /// A character device.
    pub const DEVICE: u16 = 0x0080;
    /// A device that is the console's input.
    pub const CONSOLE_INPUT: u16 = 0x0001;
    /// A file not written since it was opened.
    pub const CLEAN: u16 = 0x0040;
    /// A device's input is not at its end.
    pub const NOT_EOF: u16 = 0x0040;
    /// A device in binary mode.
    pub const RAW: u16 = 0x0020;
    /// The date and time were set by function `57h`, to be kept at close.
    pub const STAMPED: u16 = 0x4000;
    /// A file of a shared drive, reached through the redirector.
    pub const REMOTE: u16 = 0x8000;
    /// A file's drive.
    pub const DRIVE: u16 = 0x003F;

    /// Whether a handle's information word is the console's input.
    #[must_use]
    pub const fn console_input(word: u16) -> bool {
        word & (DEVICE | CONSOLE_INPUT) == DEVICE | CONSOLE_INPUT
    }
}

/// An open mode's access bits.
pub mod access {
    /// Read.
    pub const READ: u8 = 0;
    /// Write.
    pub const WRITE: u8 = 1;
    /// Both.
    pub const BOTH: u8 = 2;
    /// The bits.
    pub const MASK: u8 = 0x07;
    /// The handle is not inherited by a child.
    pub const PRIVATE: u8 = 0x80;
}

/// A closed handle's table entry.
pub const CLOSED: u8 = 0xFF;
/// The SFT index the FCBs' own table starts at.
pub const FCB_FILES: u16 = 0x100;

impl Kernel<'_, '_> {
    /// The current PSP.
    pub fn psp(&mut self) -> u16 {
        self.sda16(super::data::sda::PSP)
    }

    /// SFT entry `index`, walking the blocks; from [`FCB_FILES`] on, an
    /// entry of the FCBs' own table.
    pub fn sft(&mut self, index: u16) -> Option<Far> {
        let (at, index) = match index.checked_sub(FCB_FILES) {
            Some(n) => (sysvars::FCBS, n),
            None => (sysvars::FIRST_SFT, index),
        };
        let at = u16::try_from(i32::from(SYSVARS) + i32::from(at)).ok()?;
        let mut block = self.get_far(at);
        let mut base = 0u16;
        for _ in 0..MAX_BLOCKS {
            if block.offset == 0xFFFF {
                return None;
            }
            let count = self.read16_at(block.add(4));
            if index < base.saturating_add(count) {
                return Some(block.add(SFT_HEADER + (index - base) * SFT_ENTRY));
            }
            base = base.saturating_add(count);
            block = self.far_at(block);
        }
        None
    }

    /// The first SFT entry no handle refers to, the chain walked once.
    pub fn free_sft(&mut self) -> Option<(u8, Far)> {
        let at = u16::try_from(i32::from(SYSVARS) + i32::from(sysvars::FIRST_SFT)).ok()?;
        let mut block = self.get_far(at);
        let mut base = 0u16;
        for _ in 0..MAX_BLOCKS {
            if block.offset == 0xFFFF {
                return None;
            }
            let count = self.read16_at(block.add(4));
            for n in 0..count {
                let index = u8::try_from(base + n).ok().filter(|&i| i != CLOSED)?;
                let entry = block.add(SFT_HEADER + n * SFT_ENTRY);
                if self.read16_at(entry) == 0 {
                    return Some((index, entry));
                }
            }
            base = base.saturating_add(count);
            block = self.far_at(block);
        }
        None
    }

    /// A byte of an SFT entry.
    pub fn sft8(&mut self, entry: Far, field: u16) -> u8 {
        let at = entry.add(field);
        self.g.read8(at.segment, at.offset)
    }

    /// Sets a byte of an SFT entry.
    pub fn set_sft8(&mut self, entry: Far, field: u16, value: u8) {
        let at = entry.add(field);
        self.g.write8(at.segment, at.offset, value);
    }

    /// A word of an SFT entry.
    pub fn sft16(&mut self, entry: Far, field: u16) -> u16 {
        self.read16_at(entry.add(field))
    }

    /// Sets a word of an SFT entry.
    pub fn set_sft16(&mut self, entry: Far, field: u16, value: u16) {
        self.write16_at(entry.add(field), value);
    }

    /// A double word of an SFT entry.
    pub fn sft32(&mut self, entry: Far, field: u16) -> u32 {
        u32::from(self.sft16(entry, field)) | u32::from(self.sft16(entry, field + 2)) << 16
    }

    /// Sets a double word of an SFT entry.
    pub fn set_sft32(&mut self, entry: Far, field: u16, value: u32) {
        let (low, high) = words(value);
        self.set_sft16(entry, field, low);
        self.set_sft16(entry, field + 2, high);
    }

    /// The process `psp`'s handle table and its size.
    pub fn jft(&mut self, psp: u16) -> (Far, u16) {
        let pointer = self.far_at(Far::new(psp, psp::JFT_POINTER));
        let size = self.read16_at(Far::new(psp, psp::JFT_SIZE));
        (pointer, size)
    }

    /// The SFT entry the current process's `handle` refers to.
    pub fn handle(&mut self, handle: u16) -> Result<(u8, Far), Fault> {
        let psp = self.psp();
        let (table, size) = self.jft(psp);
        if handle >= size {
            return Err(Fault::Error(error::INVALID_HANDLE));
        }
        let at = table.add(handle);
        let index = self.g.read8(at.segment, at.offset);
        if index == CLOSED {
            return Err(Fault::Error(error::INVALID_HANDLE));
        }
        let entry = self
            .sft(u16::from(index))
            .ok_or(Fault::Error(error::INVALID_HANDLE))?;
        if self.sft16(entry, sft::HANDLES) == 0 {
            return Err(Fault::Error(error::INVALID_HANDLE));
        }
        Ok((index, entry))
    }

    /// The current process's first closed handle.
    pub fn free_handle(&mut self) -> Option<u16> {
        let psp = self.psp();
        let (table, size) = self.jft(psp);
        (0..size).find(|&h| {
            let at = table.add(h);
            self.g.read8(at.segment, at.offset) == CLOSED
        })
    }

    /// Points the current process's `handle` at SFT entry `index`.
    pub fn set_handle(&mut self, handle: u16, index: u8) {
        let psp = self.psp();
        let (table, _) = self.jft(psp);
        let at = table.add(handle);
        self.g.write8(at.segment, at.offset, index);
    }

    /// The character device named by the eight-byte base of `name`, found
    /// along the device chain.
    pub fn char_device(&mut self, name: &[u8; 11]) -> Option<Far> {
        let mut at = Far::new(SEGMENT, super::data::NUL);
        for _ in 0..MAX_DEVICES {
            if self.attributes(at) & attribute::CHARACTER != 0 {
                let mut device = [0u8; 8];
                let n = at.add(header::NAME);
                self.g.read_bytes(n.segment, n.offset, &mut device);
                if device == name[..8] {
                    return Some(at);
                }
            }
            let next = self.far_at(at.add(header::NEXT));
            if next.offset == 0xFFFF {
                return None;
            }
            at = next;
        }
        None
    }

    /// Opens `device` in SFT entry `entry` with `mode`.
    pub fn open_device(&mut self, entry: Far, device: Far, mode: u8) {
        let attributes = self.attributes(device);
        let mut name = [b' '; 11];
        let n = device.add(header::NAME);
        self.g.read_bytes(n.segment, n.offset, &mut name[..8]);
        let word = info::DEVICE | info::NOT_EOF | (attributes & 0x1F);
        self.set_sft16(entry, sft::HANDLES, 1);
        self.set_sft16(entry, sft::MODE, u16::from(mode));
        self.set_sft8(entry, sft::ATTRIBUTE, 0);
        self.set_sft16(entry, sft::INFO, word);
        self.set_sft16(entry, sft::DEVICE, device.offset);
        self.set_sft16(entry, sft::DEVICE + 2, device.segment);
        self.set_sft16(entry, sft::FIRST, 0);
        self.set_sft32(entry, sft::SIZE, 0);
        self.set_sft32(entry, sft::POSITION, 0);
        let at = entry.add(sft::NAME);
        self.g.write_bytes(at.segment, at.offset, &name);
        let owner = self.psp();
        self.set_sft16(entry, sft::OWNER, owner);
    }

    /// Opens the file whose entry the running call found, on `drive`, in SFT
    /// entry `entry` with `mode`.
    pub fn open_file(&mut self, entry: Far, drive: u8, mode: u8) {
        let found = self.found_entry();
        let first = first_cluster(&found);
        self.set_sft16(entry, sft::HANDLES, 1);
        self.set_sft16(entry, sft::MODE, u16::from(mode));
        self.set_sft8(entry, sft::ATTRIBUTE, found.attributes);
        self.set_sft16(entry, sft::INFO, info::CLEAN | u16::from(drive));
        self.set_sft16(entry, sft::DEVICE, dpb_at(drive));
        self.set_sft16(entry, sft::DEVICE + 2, SEGMENT);
        self.set_sft16(entry, sft::FIRST, first);
        self.set_sft16(entry, sft::TIME, found.stamps.written.time);
        self.set_sft16(entry, sft::DATE, found.stamps.written.date);
        self.set_sft32(entry, sft::SIZE, found.size);
        self.set_sft32(entry, sft::POSITION, 0);
        self.set_sft16(entry, sft::RELATIVE, 0);
        self.set_sft16(entry, sft::CLUSTER, first);
        let sector = self.found_sector();
        self.set_sft32(entry, sft::DIR_SECTOR, sector);
        let slot = self.found16(found::SLOT);
        self.set_sft8(entry, sft::DIR_INDEX, u8::try_from(slot).unwrap_or(0));
        let at = entry.add(sft::NAME);
        self.g.write_bytes(at.segment, at.offset, &found.name);
        let owner = self.psp();
        self.set_sft16(entry, sft::OWNER, owner);
    }

    /// The device `entry` is open on, if it is a device's.
    pub fn sft_device(&mut self, entry: Far) -> Option<Far> {
        (self.sft16(entry, sft::INFO) & info::DEVICE != 0).then(|| {
            Far::new(
                self.sft16(entry, sft::DEVICE + 2),
                self.sft16(entry, sft::DEVICE),
            )
        })
    }

    /// The drive a file's `entry` is on.
    pub fn sft_drive(&mut self, entry: Far) -> u8 {
        u8::try_from(self.sft16(entry, sft::INFO) & info::DRIVE).unwrap_or(0)
    }

    /// The access `entry` was opened for: [`access::READ`], [`access::WRITE`]
    /// or [`access::BOTH`].
    pub fn sft_access(&mut self, entry: Far) -> u8 {
        self.sft16(entry, sft::MODE).to_le_bytes()[0] & access::MASK
    }
}

/// The extended error a refused name gives an open.
fn refused(error: NameError) -> Fault {
    Fault::Error(name_error(error))
}

locals! {
    /// Opens the running call's path with `mode` (function `3Dh`'s AL):
    /// a character device its name names, else the file it resolves to.
    /// Returns the handle; with `keep_handle`, only the SFT entry's index,
    /// for the kernel's own use.
    pub struct Open {
        mode: u8,
        keep_handle: bool,
        index: u8,
        handle: u16,
        drive: u8,
    }
}

const RESOLVED: u8 = 1;
const OPENED: u8 = 2;

impl Routine for Open {
    const ID: Id = Id::Open;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            RESOLVED => {
                let found = take!(child);
                if found & resolved::ROOT != 0 {
                    return Flow::Return(Err(Fault::Error(error::ACCESS_DENIED)));
                }
                let attributes = k.found_entry().attributes;
                if attributes & (attr::DIRECTORY | attr::VOLUME_ID) != 0 {
                    return Flow::Return(Err(Fault::Error(error::ACCESS_DENIED)));
                }
                if attributes & attr::READ_ONLY != 0 && self.mode & access::MASK != access::READ {
                    return Flow::Return(Err(Fault::Error(error::ACCESS_DENIED)));
                }
                let (path, _) = k.path(level::PATH);
                self.drive = path[0].wrapping_sub(b'A');
                let Some(at) = self.claim(k) else {
                    return Flow::Return(Err(Fault::Error(error::TOO_MANY_OPEN)));
                };
                k.open_file(at, self.drive, self.mode);
                self.opened(k, phase, at)
            }
            OPENED => {
                take!(child);
                self.done(k)
            }
            _ => {
                if self.mode & access::MASK > access::BOTH {
                    return Flow::Return(Err(Fault::Error(error::INVALID_ACCESS)));
                }
                let (path, len) = k.path(level::PATH);
                let canonical = match k.canonical(&path[..len]) {
                    Ok(canonical) => canonical,
                    Err(error) => return Flow::Return(Err(refused(error))),
                };
                if let Some(device) = canonical
                    .last()
                    .and_then(|last| super::name::field(last, false).ok())
                    .and_then(|field| k.char_device(&field))
                {
                    let Some(at) = self.claim(k) else {
                        return Flow::Return(Err(Fault::Error(error::TOO_MANY_OPEN)));
                    };
                    k.open_device(at, device, self.mode);
                    return self.opened(k, phase, at);
                }
                match redirect::remote(k, &canonical, false) {
                    Ok(Some(remote)) => return self.open_remote(k, &remote),
                    Ok(None) => {}
                    Err(fault) => return Flow::Return(Err(fault)),
                }
                *phase = RESOLVED;
                Flow::Call(Frame::of(&Resolve {
                    mode: mode::EXISTING,
                    drive: 0,
                    directory: 0,
                    component: 0,
                    count: 0,
                    second: false,
                }))
            }
        }
    }
}

impl Open {
    /// Takes a free SFT entry, and a handle for it unless the kernel keeps
    /// the entry itself.
    fn claim(&mut self, k: &mut Kernel<'_, '_>) -> Option<Far> {
        if !self.keep_handle {
            self.handle = k.free_handle()?;
        }
        let (index, at) = k.free_sft()?;
        self.index = index;
        k.set_sft16(at, sft::HANDLES, 0xFFFF);
        Some(at)
    }

    /// Opens the file of a shared drive `remote` names.
    fn open_remote(&mut self, k: &mut Kernel<'_, '_>, remote: &Remote) -> Flow {
        let Some(at) = self.claim(k) else {
            return Flow::Return(Err(Fault::Error(error::TOO_MANY_OPEN)));
        };
        if let Err(fault) = redirect::open(k, remote, self.mode, Creation::Existing, at) {
            k.set_sft16(at, sft::HANDLES, 0);
            return Flow::Return(Err(fault));
        }
        self.done(k)
    }

    /// Calls the device's open request, if it takes one.
    fn opened(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, at: Far) -> Flow {
        let (device, unit) = match k.sft_device(at) {
            Some(device) => (device, 0),
            None => match k.dpb(self.drive) {
                Some(d) => (d.driver, d.unit),
                None => return self.done(k),
            },
        };
        if k.attributes(device) & attribute::OPEN_CLOSE == 0 {
            return self.done(k);
        }
        k.request(unit, command::OPEN);
        *phase = OPENED;
        Flow::Call(Frame::of(&Device { header: device }))
    }

    fn done(self, k: &mut Kernel<'_, '_>) -> Flow {
        if self.keep_handle {
            return Flow::Return(Ok(u32::from(self.index)));
        }
        k.set_handle(self.handle, self.index);
        Flow::Return(Ok(u32::from(self.handle)))
    }
}

locals! {
    /// Makes the directory entry of a new file at the running call's path,
    /// with attributes `attributes`: an existing file's is emptied in
    /// place, unless `new`. Returns the drive; the entry, and where it lies,
    /// are left as the scratch's entry found.
    pub struct MakeFile {
        attributes: u8,
        new: bool,
        drive: u8,
        directory: u16,
        exists: bool,
        stamp: u32,
    }
}

const MAKE_RESOLVED: u8 = 1;
const STAMPED: u8 = 2;
const PLACED: u8 = 3;
const STORED: u8 = 4;
const LABEL_DIRECTORY: u8 = 5;
const LABEL_SCANNED: u8 = 6;
const LABELLED: u8 = 7;
const WRITTEN_BACK: u8 = 8;

impl Routine for MakeFile {
    const ID: Id = Id::MakeFile;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            MAKE_RESOLVED => {
                let found = take!(child);
                if found & resolved::ROOT != 0 {
                    return Flow::Return(Err(Fault::Error(error::ACCESS_DENIED)));
                }
                if super::name::is_wild(&k.name()) {
                    return Flow::Return(Err(Fault::Error(error::PATH_NOT_FOUND)));
                }
                let (path, _) = k.path(level::PATH);
                self.drive = path[0].wrapping_sub(b'A');
                self.directory = u16::try_from(found & 0xFFFF).unwrap_or(0);
                self.exists = found & resolved::EXISTS != 0;
                if self.exists {
                    if self.new {
                        return Flow::Return(Err(Fault::Error(error::FILE_EXISTS)));
                    }
                    let attributes = k.found_entry().attributes;
                    if attributes & (attr::DIRECTORY | attr::VOLUME_ID | attr::READ_ONLY) != 0 {
                        return Flow::Return(Err(Fault::Error(error::ACCESS_DENIED)));
                    }
                }
                *phase = STAMPED;
                Flow::Call(Frame::of(&super::char::Clock { read: true }))
            }
            STAMPED => {
                self.stamp = take!(child);
                *phase = PLACED;
                if !self.exists {
                    return Flow::Call(Frame::of(&Slot {
                        drive: self.drive,
                        directory: self.directory,
                        grown: false,
                    }));
                }
                let first = first_cluster(&k.found_entry());
                if !k.dpb(self.drive).is_some_and(|d| d.in_volume(first)) {
                    return self.step(k, phase, Ok(0));
                }
                Flow::Call(Frame::of(&FreeChain {
                    drive: self.drive,
                    cluster: first,
                    next: 0,
                    freed: 0,
                    read: false,
                }))
            }
            PLACED => {
                take!(child);
                let entry = self.entry(k);
                k.set_found_entry(&entry);
                *phase = STORED;
                Flow::Call(Frame::of(&Store::found(k, self.drive)))
            }
            STORED => {
                take!(child);
                if self.attributes & attr::VOLUME_ID == 0 {
                    *phase = WRITTEN_BACK;
                    return super::fs::written_back(self.drive);
                }
                *phase = LABELLED;
                Flow::Call(Frame::of(&MediaLabel::of(self.drive, false)))
            }
            LABELLED => {
                *phase = WRITTEN_BACK;
                super::fs::written_back(self.drive)
            }
            WRITTEN_BACK => {
                take!(child);
                Flow::Return(Ok(u32::from(self.drive)))
            }
            LABEL_DIRECTORY => self.find_label(k, phase, take!(child)),
            // A directory holds one label at most.
            LABEL_SCANNED => match child {
                Ok(_) => Flow::Return(Err(Fault::Error(error::ACCESS_DENIED))),
                Err(Fault::Error(error::NO_MORE_FILES)) => {
                    self.attributes = attr::VOLUME_ID;
                    self.exists = false;
                    *phase = STAMPED;
                    Flow::Call(Frame::of(&super::char::Clock { read: true }))
                }
                Err(fault) => Flow::Return(Err(fault)),
            },
            _ => {
                if self.attributes & attr::DIRECTORY != 0 {
                    return Flow::Return(Err(Fault::Error(error::ACCESS_DENIED)));
                }
                if self.attributes & attr::VOLUME_ID != 0 {
                    return Self::begin_label(k, phase);
                }
                *phase = MAKE_RESOLVED;
                Flow::Call(Frame::of(&Resolve {
                    mode: mode::PARENT,
                    drive: 0,
                    directory: 0,
                    component: 0,
                    count: 0,
                    second: false,
                }))
            }
        }
    }
}

impl MakeFile {
    /// The new entry: the name or label, the attributes with the archive
    /// bit, written at the stamp taken.
    fn entry(&self, k: &mut Kernel<'_, '_>) -> ShortEntry {
        let [time_low, time_high, date_low, date_high] = self.stamp.to_le_bytes();
        let name = if self.attributes & attr::VOLUME_ID != 0 {
            Self::label(k)
        } else {
            k.name()
        };
        let mut entry = ShortEntry {
            name,
            attributes: self.attributes | attr::ARCHIVE,
            ..ShortEntry::default()
        };
        entry.stamps.written.time = u16::from_le_bytes([time_low, time_high]);
        entry.stamps.written.date = u16::from_le_bytes([date_low, date_high]);
        entry
    }

    /// A volume label the running call's path names: the label kept, and
    /// the directory before it looked up. Its name is a label's, spaces and
    /// all, not a file's.
    fn begin_label(k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        let (path, len) = k.path(level::PATH);
        let path = &path[..len];
        let split = path
            .iter()
            .rposition(|&b| matches!(b, b'\\' | b'/' | b':'))
            .map_or(0, |at| at + 1);
        let Some(label) = super::name::label_field(&path[split..]) else {
            return Flow::Return(Err(Fault::Error(error::PATH_NOT_FOUND)));
        };
        let kept = k.scratch(level::KEPT);
        k.g.write_bytes(SEGMENT, kept, &label);
        let directory = match &path[..split] {
            [] => b".".to_vec(),
            [drive, b':'] => alloc::vec![*drive, b':', b'.'],
            [.., b'\\' | b'/'] if split > 3 => path[..split - 1].to_vec(),
            directory => directory.to_vec(),
        };
        k.set_path(level::PATH, &directory);
        *phase = LABEL_DIRECTORY;
        Flow::Call(Frame::of(&Resolve::of(mode::EXISTING, false)))
    }

    /// The label's directory is found: any label already there is looked
    /// for.
    fn find_label(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, found: u32) -> Flow {
        let (path, _) = k.path(level::PATH);
        self.drive = path[0].wrapping_sub(b'A');
        self.directory = if found & resolved::ROOT != 0 {
            0
        } else {
            let entry = k.found_entry();
            if !entry.is_directory() {
                return Flow::Return(Err(Fault::Error(error::PATH_NOT_FOUND)));
            }
            first_cluster(&entry)
        };
        k.set_name(&[b'?'; 11]);
        *phase = LABEL_SCANNED;
        Flow::Call(Frame::of(&DirScan {
            drive: self.drive,
            directory: self.directory,
            index: 0,
            attributes: attr::VOLUME_ID,
            mode: scan::FIND,
            cluster: 0,
            position: 0,
        }))
    }

    /// The label [`Self::begin_label`] kept.
    fn label(k: &mut Kernel<'_, '_>) -> [u8; 11] {
        let mut label = [b' '; 11];
        let kept = k.scratch(level::KEPT);
        k.g.read_bytes(SEGMENT, kept, &mut label);
        label
    }
}

locals! {
    /// Creates the running call's path as a file with attributes
    /// `attributes` — emptying one that exists, unless `new` — and opens it
    /// with `mode`: functions `3Ch`, `5Bh`, `5Ah` and `6Ch`. A path naming a
    /// character device opens the device. Returns the handle; a free handle
    /// and SFT entry are found before the disk is touched.
    pub struct Create {
        attributes: u8,
        new: bool,
        mode: u8,
        handle: u16,
    }
}

const MADE: u8 = 1;
const DEVICE_OPENED: u8 = 2;

impl Routine for Create {
    const ID: Id = Id::Create;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            MADE => {
                let drive = u8::try_from(take!(child)).unwrap_or(0);
                let (Some(handle), Some((index, at))) = (k.free_handle(), k.free_sft()) else {
                    return Flow::Return(Err(Fault::Error(error::TOO_MANY_OPEN)));
                };
                k.open_file(at, drive, self.mode);
                let info = k.sft16(at, sft::INFO);
                k.set_sft16(at, sft::INFO, info & !info::CLEAN);
                k.set_handle(handle, index);
                self.handle = handle;
                let Some(d) = k.dpb(drive) else {
                    return Flow::Return(Ok(u32::from(handle)));
                };
                if k.attributes(d.driver) & attribute::OPEN_CLOSE == 0 {
                    return Flow::Return(Ok(u32::from(handle)));
                }
                k.request(d.unit, command::OPEN);
                *phase = DEVICE_OPENED;
                Flow::Call(Frame::of(&Device { header: d.driver }))
            }
            DEVICE_OPENED => {
                take!(child);
                Flow::Return(Ok(u32::from(self.handle)))
            }
            _ => {
                if self.mode & access::MASK > access::BOTH {
                    return Flow::Return(Err(Fault::Error(error::INVALID_ACCESS)));
                }
                if k.free_handle().is_none() || k.free_sft().is_none() {
                    return Flow::Return(Err(Fault::Error(error::TOO_MANY_OPEN)));
                }
                // A label is no device's name, and its spaces no file's.
                if self.attributes & attr::VOLUME_ID == 0 {
                    let (path, len) = k.path(level::PATH);
                    let canonical = match k.canonical(&path[..len]) {
                        Ok(canonical) => canonical,
                        Err(error) => return Flow::Return(Err(refused(error))),
                    };
                    if let Some(device) = canonical
                        .last()
                        .and_then(|last| super::name::field(last, false).ok())
                        .and_then(|field| k.char_device(&field))
                    {
                        let (Some(handle), Some((index, at))) = (k.free_handle(), k.free_sft())
                        else {
                            return Flow::Return(Err(Fault::Error(error::TOO_MANY_OPEN)));
                        };
                        k.open_device(at, device, self.mode);
                        k.set_handle(handle, index);
                        return Flow::Return(Ok(u32::from(handle)));
                    }
                    match redirect::remote(k, &canonical, true) {
                        Ok(Some(remote)) => return self.create_remote(k, &remote),
                        Ok(None) => {}
                        Err(fault) => return Flow::Return(Err(fault)),
                    }
                } else if !matches!(redirect::remote_path(k, level::PATH, false), Ok(None)) {
                    // A share's label is the name it was shared under.
                    return Flow::Return(Err(Fault::Error(error::ACCESS_DENIED)));
                }
                *phase = MADE;
                Flow::Call(Frame::of(&MakeFile {
                    attributes: self.attributes,
                    new: self.new,
                    drive: 0,
                    directory: 0,
                    exists: false,
                    stamp: 0,
                }))
            }
        }
    }
}

impl Create {
    /// Creates the file of a shared drive `remote` names, as a local one is:
    /// never a directory, and never at a pattern.
    fn create_remote(&mut self, k: &mut Kernel<'_, '_>, remote: &Remote) -> Flow {
        if self.attributes & attr::DIRECTORY != 0 {
            return Flow::Return(Err(Fault::Error(error::ACCESS_DENIED)));
        }
        if remote.last.is_some_and(|last| last.is_wild()) {
            return Flow::Return(Err(Fault::Error(error::PATH_NOT_FOUND)));
        }
        let (Some(handle), Some((index, at))) = (k.free_handle(), k.free_sft()) else {
            return Flow::Return(Err(Fault::Error(error::TOO_MANY_OPEN)));
        };
        let creation = if self.new {
            Creation::New
        } else {
            Creation::Truncate
        };
        if let Err(fault) = redirect::open(k, remote, self.mode, creation, at) {
            return Flow::Return(Err(fault));
        }
        let word = k.sft16(at, sft::INFO);
        k.set_sft16(at, sft::INFO, word & !info::CLEAN);
        k.set_handle(handle, index);
        Flow::Return(Ok(u32::from(handle)))
    }
}

locals! {
    /// Reads up to `count` bytes from SFT entry `index` to `buffer`: a
    /// device's through its driver, a file's from its position, whole
    /// sectors straight from the disk and parts through the buffers.
    /// Returns the bytes read.
    pub struct Read {
        index: u16,
        buffer: Far,
        count: u16,
        done: u16,
        steps: u16,
    }
}

const DEVICE_READ: u8 = 1;
const STEP: u8 = 2;
const DIRECT: u8 = 3;
const PART: u8 = 4;

impl Routine for Read {
    const ID: Id = Id::Read;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let Some(entry) = k.sft(self.index) else {
            return Flow::Return(Err(Fault::Error(error::INVALID_HANDLE)));
        };
        if let Some(device) = k.sft_device(entry) {
            return self.device_read(k, phase, child, entry, device);
        }
        if k.sft_remote(entry).is_some() {
            let read = redirect::read(k, entry, self.buffer, self.count);
            return Flow::Return(read.map(u32::from));
        }
        self.file_read(k, phase, child, entry)
    }
}

impl Read {
    /// A device's read: through the line editor for a cooked console, else
    /// a request of the device; nothing once its input has ended.
    fn device_read(
        self,
        k: &mut Kernel<'_, '_>,
        phase: &mut u8,
        child: Outcome,
        entry: Far,
        device: Far,
    ) -> Flow {
        if *phase == DEVICE_READ {
            let read = take!(child);
            return Flow::Return(Ok(read));
        }
        if k.sft16(entry, sft::INFO) & info::NOT_EOF == 0 {
            return Flow::Return(Ok(0));
        }
        *phase = DEVICE_READ;
        let raw = k.sft16(entry, sft::INFO) & info::RAW != 0;
        let console = k.attributes(device) & attribute::STDIN != 0;
        if console && !raw {
            return Flow::Call(Frame::of(&LineInput {
                device,
                buffer: self.buffer,
                count: self.count,
                entry,
                piped: false,
            }));
        }
        Flow::Call(Frame::of(&CharIo {
            device,
            command: command::INPUT,
            buffer: self.buffer,
            count: self.count,
            done: 0,
            status: 0,
        }))
    }

    /// A file's read from its position.
    fn file_read(
        &mut self,
        k: &mut Kernel<'_, '_>,
        phase: &mut u8,
        child: Outcome,
        entry: Far,
    ) -> Flow {
        let drive = k.sft_drive(entry);
        let Some(d) = k.dpb(drive) else {
            return Flow::Return(Err(Fault::Error(error::INVALID_DRIVE)));
        };
        match *phase {
            STEP => {
                let cluster = take!(child);
                if !reached(k, entry, &d, cluster, self.steps) {
                    return Flow::Return(Ok(u32::from(self.done)));
                }
            }
            DIRECT => {
                let moved = take!(child);
                let bytes = u16::try_from(moved)
                    .unwrap_or(0)
                    .saturating_mul(d.sector_bytes);
                self.advance(k, entry, bytes);
            }
            PART => {
                take!(child);
            }
            _ => {
                if k.sft_access(entry) == access::WRITE {
                    return Flow::Return(Err(Fault::Error(error::ACCESS_DENIED)));
                }
            }
        }
        loop {
            let position = k.sft32(entry, sft::POSITION);
            let size = k.sft32(entry, sft::SIZE);
            if self.done >= self.count || position >= size {
                return Flow::Return(Ok(u32::from(self.done)));
            }
            let want = u32::from(self.count - self.done).min(size - position);
            let Some(cluster) = cluster_for(k, entry, &d, position) else {
                if rewind(k, entry, &d, position) {
                    continue;
                }
                let wanted = u16::try_from(position / d.cluster_bytes()).unwrap_or(u16::MAX);
                self.steps = wanted - k.sft16(entry, sft::RELATIVE);
                *phase = STEP;
                return walk_on(k, entry, &d, self.steps);
            };
            let within_cluster = position % d.cluster_bytes();
            let sector = d.cluster_sector(cluster) + within_cluster / u32::from(d.sector_bytes);
            let within = u16::try_from(position % u32::from(d.sector_bytes)).unwrap_or(0);
            if within == 0
                && want >= u32::from(d.sector_bytes)
                && k.find_buffer(drive, sector).is_none()
            {
                let run = direct_run(k, &d, sector, within_cluster, want);
                *phase = DIRECT;
                return Flow::Call(Frame::of(&BlockIo {
                    drive,
                    sector,
                    count: run,
                    address: self.buffer.add(self.done),
                    write: false,
                    area: area::DATA,
                    status: 0,
                }));
            }
            let Some(segment) = k.find_buffer(drive, sector) else {
                *phase = PART;
                return Flow::Call(Frame::of(&Buffer {
                    drive,
                    sector,
                    kind: 0,
                    fresh: false,
                    victim: 0,
                }));
            };
            k.touch_buffer(segment);
            let take = u16::try_from(want.min(u32::from(d.sector_bytes - within))).unwrap_or(0);
            let mut bytes = [0u8; 512];
            let span = usize::from(take).min(bytes.len());
            k.g.read_bytes(segment, buffer::DATA + within, &mut bytes[..span]);
            let to = self.buffer.add(self.done);
            k.g.write_bytes(to.segment, to.offset, &bytes[..span]);
            self.advance(k, entry, take);
        }
    }

    fn advance(&mut self, k: &mut Kernel<'_, '_>, entry: Far, bytes: u16) {
        self.done = self.done.saturating_add(bytes);
        let position = k.sft32(entry, sft::POSITION).wrapping_add(u32::from(bytes));
        k.set_sft32(entry, sft::POSITION, position);
    }
}

/// The cluster holding byte `position` of the file open in `entry`, if the
/// entry's remembered cluster is it; else `None`, and the caller steps.
fn cluster_for(k: &mut Kernel<'_, '_>, entry: Far, d: &Dpb, position: u32) -> Option<u16> {
    let wanted = u16::try_from(position / d.cluster_bytes()).unwrap_or(u16::MAX);
    let relative = k.sft16(entry, sft::RELATIVE);
    let cluster = k.sft16(entry, sft::CLUSTER);
    (wanted == relative && d.in_volume(cluster)).then_some(cluster)
}

/// Moves the entry's remembered cluster back to the file's first when the
/// one holding byte `target` lies behind it, or it remembers none; `true`
/// when it did.
fn rewind(k: &mut Kernel<'_, '_>, entry: Far, d: &Dpb, target: u32) -> bool {
    let relative = k.sft16(entry, sft::RELATIVE);
    let cluster = k.sft16(entry, sft::CLUSTER);
    let wanted = u16::try_from(target / d.cluster_bytes()).unwrap_or(u16::MAX);
    if wanted >= relative && d.in_volume(cluster) {
        return false;
    }
    let first = k.sft16(entry, sft::FIRST);
    k.set_sft16(entry, sft::RELATIVE, 0);
    k.set_sft16(entry, sft::CLUSTER, first);
    true
}

/// A walk from the remembered cluster `steps` links on.
fn walk_on(k: &mut Kernel<'_, '_>, entry: Far, d: &Dpb, steps: u16) -> Flow {
    let cluster = k.sft16(entry, sft::CLUSTER);
    Flow::Call(Frame::of(&Walk {
        drive: d.drive,
        cluster,
        steps,
    }))
}

/// Records `reached`, `steps` links past the remembered cluster, as the one
/// remembered; `false` when the chain ended first.
fn reached(k: &mut Kernel<'_, '_>, entry: Far, d: &Dpb, reached: u32, steps: u16) -> bool {
    match u16::try_from(reached) {
        Ok(cluster) if d.in_volume(cluster) => {
            let relative = k.sft16(entry, sft::RELATIVE);
            k.set_sft16(entry, sft::RELATIVE, relative.saturating_add(steps));
            k.set_sft16(entry, sft::CLUSTER, cluster);
            true
        }
        _ => false,
    }
}

locals! {
    /// Writes `count` bytes from `buffer` to SFT entry `index` at its
    /// position: a device's through its driver; a file's growing its chain
    /// as it goes. No bytes ends the file at the position, its chain cut or
    /// grown to fit. Returns the bytes written, fewer once the disk is full.
    pub struct Write {
        index: u16,
        buffer: Far,
        count: u16,
        done: u16,
        kept: u16,
        next: u16,
    }
}

const ALLOCATED: u8 = 5;
const CUT_WALKED: u8 = 6;
const CUT_READ: u8 = 7;
const CUT_MARKED: u8 = 8;
const CUT_FREED: u8 = 9;

impl Routine for Write {
    const ID: Id = Id::Write;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let Some(entry) = k.sft(self.index) else {
            return Flow::Return(Err(Fault::Error(error::INVALID_HANDLE)));
        };
        if let Some(device) = k.sft_device(entry) {
            if *phase == DEVICE_READ {
                let written = take!(child);
                return Flow::Return(Ok(written));
            }
            *phase = DEVICE_READ;
            return Flow::Call(Frame::of(&CharIo {
                device,
                command: command::OUTPUT,
                buffer: self.buffer,
                count: self.count,
                done: 0,
                status: 0,
            }));
        }
        if k.sft_remote(entry).is_some() {
            let written = redirect::write(k, entry, self.buffer, self.count);
            return Flow::Return(written.map(u32::from));
        }
        let drive = k.sft_drive(entry);
        let Some(d) = k.dpb(drive) else {
            return Flow::Return(Err(Fault::Error(error::INVALID_DRIVE)));
        };
        if let Some(flow) = self.resume(k, phase, child, entry, &d) {
            return flow;
        }
        self.write_more(k, phase, entry, &d)
    }
}

impl Write {
    /// Takes up the routine it called last, `None` once the write may go on.
    fn resume(
        &mut self,
        k: &mut Kernel<'_, '_>,
        phase: &mut u8,
        child: Outcome,
        entry: Far,
        d: &Dpb,
    ) -> Option<Flow> {
        let drive = d.drive;
        let value = match child {
            Ok(value) => value,
            Err(fault) if matches!(*phase, STEP | ALLOCATED | DIRECT | PART) => {
                return Some(Flow::Return(Err(fault)))
            }
            Err(_) => 0,
        };
        match *phase {
            STEP => {
                if !reached(k, entry, d, value, 1) {
                    let after = k.sft16(entry, sft::CLUSTER);
                    *phase = ALLOCATED;
                    return Some(Flow::Call(Frame::of(&Alloc {
                        drive,
                        after,
                        candidate: 0,
                        scanned: 0,
                        found: 0,
                    })));
                }
            }
            ALLOCATED => {
                let cluster = u16::try_from(value).unwrap_or(0);
                if cluster == 0 {
                    return Some(Flow::Return(Ok(u32::from(self.done))));
                }
                if k.sft16(entry, sft::FIRST) == 0 {
                    k.set_sft16(entry, sft::FIRST, cluster);
                    k.set_sft16(entry, sft::RELATIVE, 0);
                } else {
                    let relative = k.sft16(entry, sft::RELATIVE);
                    k.set_sft16(entry, sft::RELATIVE, relative + 1);
                }
                k.set_sft16(entry, sft::CLUSTER, cluster);
            }
            DIRECT => {
                let bytes = u16::try_from(value)
                    .unwrap_or(0)
                    .saturating_mul(d.sector_bytes);
                self.advance(k, entry, bytes);
            }
            PART => {}
            CUT_WALKED | CUT_READ | CUT_MARKED | CUT_FREED => {
                return Some(self.cut(k, phase, child, entry, d));
            }
            _ => {
                if k.sft_access(entry) == access::READ {
                    return Some(Flow::Return(Err(Fault::Error(error::ACCESS_DENIED))));
                }
                let info = k.sft16(entry, sft::INFO);
                k.set_sft16(entry, sft::INFO, info & !info::CLEAN);
                let position = k.sft32(entry, sft::POSITION);
                if self.count == 0 && position < k.sft32(entry, sft::SIZE) {
                    return Some(self.cut(k, phase, Ok(0), entry, d));
                }
            }
        }
        None
    }

    /// Writes on from the position until the count is written, the disk
    /// fills, or a routine must run first.
    fn write_more(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, entry: Far, d: &Dpb) -> Flow {
        let drive = d.drive;
        loop {
            let position = k.sft32(entry, sft::POSITION);
            if self.count == 0 && position == 0 {
                k.set_sft32(entry, sft::SIZE, 0);
                return Flow::Return(Ok(0));
            }
            if self.count != 0 && self.done >= self.count {
                return Flow::Return(Ok(u32::from(self.done)));
            }
            if k.sft16(entry, sft::FIRST) == 0 {
                *phase = ALLOCATED;
                return Flow::Call(Frame::of(&Alloc {
                    drive,
                    after: 0,
                    candidate: 0,
                    scanned: 0,
                    found: 0,
                }));
            }
            let target = self.target(k, entry);
            let Some(cluster) = cluster_for(k, entry, d, target) else {
                if rewind(k, entry, d, target) {
                    continue;
                }
                *phase = STEP;
                return walk_on(k, entry, d, 1);
            };
            if self.count == 0 {
                // The chain reaches the position: the file ends there.
                k.set_sft32(entry, sft::SIZE, position);
                return Flow::Return(Ok(0));
            }
            let want = u32::from(self.count - self.done);
            let within_cluster = position % d.cluster_bytes();
            let sector = d.cluster_sector(cluster) + within_cluster / u32::from(d.sector_bytes);
            let within = u16::try_from(position % u32::from(d.sector_bytes)).unwrap_or(0);
            if within == 0
                && want >= u32::from(d.sector_bytes)
                && k.find_buffer(drive, sector).is_none()
            {
                let run = direct_run(k, d, sector, within_cluster, want);
                *phase = DIRECT;
                return Flow::Call(Frame::of(&BlockIo {
                    drive,
                    sector,
                    count: run,
                    address: self.buffer.add(self.done),
                    write: true,
                    area: area::DATA,
                    status: 0,
                }));
            }
            let take = u16::try_from(want.min(u32::from(d.sector_bytes - within))).unwrap_or(0);
            let Some(segment) = k.find_buffer(drive, sector) else {
                *phase = PART;
                let size = k.sft32(entry, sft::SIZE);
                let whole = within == 0 && take == d.sector_bytes;
                let beyond = position - u32::from(within) >= size;
                return Flow::Call(Frame::of(&Buffer {
                    drive,
                    sector,
                    kind: 0,
                    fresh: whole || beyond,
                    victim: 0,
                }));
            };
            let mut bytes = [0u8; 512];
            let span = usize::from(take).min(bytes.len());
            let from = self.buffer.add(self.done);
            k.g.read_bytes(from.segment, from.offset, &mut bytes[..span]);
            k.g.write_bytes(segment, buffer::DATA + within, &bytes[..span]);
            k.dirty(segment);
            k.touch_buffer(segment);
            self.advance(k, entry, take);
        }
    }
}

impl Write {
    /// The byte whose cluster must exist next: the position for data, the
    /// last byte of the file to be for a write of none.
    fn target(&self, k: &mut Kernel<'_, '_>, entry: Far) -> u32 {
        let position = k.sft32(entry, sft::POSITION);
        if self.count == 0 {
            position.saturating_sub(1)
        } else {
            position
        }
    }

    fn advance(&mut self, k: &mut Kernel<'_, '_>, entry: Far, bytes: u16) {
        self.done = self.done.saturating_add(bytes);
        let position = k.sft32(entry, sft::POSITION).wrapping_add(u32::from(bytes));
        k.set_sft32(entry, sft::POSITION, position);
        let size = k.sft32(entry, sft::SIZE);
        if position > size {
            k.set_sft32(entry, sft::SIZE, position);
        }
    }

    /// A write of no bytes below the end: the file ends at the position,
    /// its chain cut after the clusters that hold it.
    fn cut(
        &mut self,
        k: &mut Kernel<'_, '_>,
        phase: &mut u8,
        child: Outcome,
        entry: Far,
        d: &Dpb,
    ) -> Flow {
        let position = k.sft32(entry, sft::POSITION);
        let first = k.sft16(entry, sft::FIRST);
        match *phase {
            CUT_WALKED => {
                self.kept = u16::try_from(take!(child)).unwrap_or(0);
                if !d.in_volume(self.kept) {
                    return Self::ended(k, entry, d, position);
                }
                *phase = CUT_READ;
                Flow::Call(Frame::of(&super::fs::FatGet {
                    drive: d.drive,
                    cluster: self.kept,
                    got: 0,
                    bytes: [0; 4],
                }))
            }
            CUT_READ => {
                let value = take!(child);
                self.next = match d.link(value) {
                    tairix_fat::table::Link::Next(next) => u16::try_from(next).unwrap_or(0),
                    _ => 0,
                };
                *phase = CUT_MARKED;
                Flow::Call(Frame::of(&FatSet {
                    drive: d.drive,
                    cluster: self.kept,
                    value: u16::try_from(d.end()).unwrap_or(u16::MAX),
                    got: 0,
                    put: 0,
                    bytes: [0; 4],
                }))
            }
            CUT_MARKED => {
                take!(child);
                if !d.in_volume(self.next) {
                    return Self::ended(k, entry, d, position);
                }
                *phase = CUT_FREED;
                Flow::Call(Frame::of(&FreeChain {
                    drive: d.drive,
                    cluster: self.next,
                    next: 0,
                    freed: 0,
                    read: false,
                }))
            }
            CUT_FREED => {
                take!(child);
                if position == 0 {
                    k.set_sft16(entry, sft::FIRST, 0);
                }
                Self::ended(k, entry, d, position)
            }
            _ if position == 0 => {
                if first == 0 {
                    return Self::ended(k, entry, d, 0);
                }
                self.next = first;
                *phase = CUT_MARKED;
                self.cut(k, phase, Ok(0), entry, d)
            }
            _ => {
                let keep = (position - 1) / d.cluster_bytes();
                *phase = CUT_WALKED;
                Flow::Call(Frame::of(&Walk {
                    drive: d.drive,
                    cluster: first,
                    steps: u16::try_from(keep).unwrap_or(u16::MAX),
                }))
            }
        }
    }

    /// The file ends at `position`: its size, and the remembered cluster
    /// back at the start should the cut have taken it.
    fn ended(k: &mut Kernel<'_, '_>, entry: Far, d: &Dpb, position: u32) -> Flow {
        k.set_sft32(entry, sft::SIZE, position);
        let kept = position.div_ceil(d.cluster_bytes());
        if u32::from(k.sft16(entry, sft::RELATIVE)) >= kept {
            let first = k.sft16(entry, sft::FIRST);
            k.set_sft16(entry, sft::RELATIVE, 0);
            k.set_sft16(entry, sft::CLUSTER, first);
        }
        Flow::Return(Ok(0))
    }
}

/// Sectors a direct transfer from `sector` moves: the whole sectors
/// wanted, within its cluster, stopping before any a buffer holds.
fn direct_run(k: &mut Kernel<'_, '_>, d: &Dpb, sector: u32, within_cluster: u32, want: u32) -> u16 {
    let bytes = u32::from(d.sector_bytes);
    let left = d.cluster_sectors() - u16::try_from(within_cluster / bytes).unwrap_or(0);
    let run = u16::try_from(want / bytes).unwrap_or(u16::MAX).min(left);
    (1..run)
        .find(|&n| k.find_buffer(d.drive, sector + u32::from(n)).is_some())
        .unwrap_or(run)
}

locals! {
    /// Writes SFT entry `index`'s directory entry back from it: its first
    /// cluster and size, its time and date — now, unless function `57h` set
    /// them — and the archive bit.
    pub struct DirUpdate {
        index: u16,
        stamp: u32,
    }
}

impl Routine for DirUpdate {
    const ID: Id = Id::DirUpdate;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let Some(entry) = k.sft(self.index) else {
            return Flow::Return(Ok(0));
        };
        let info = k.sft16(entry, sft::INFO);
        if info & (info::DEVICE | info::CLEAN) != 0 {
            return Flow::Return(Ok(0));
        }
        let drive = k.sft_drive(entry);
        let sector = k.sft32(entry, sft::DIR_SECTOR);
        match *phase {
            0 if info & info::STAMPED == 0 => {
                *phase = STAMPED;
                return Flow::Call(Frame::of(&super::char::Clock { read: true }));
            }
            STAMPED => {
                self.stamp = take!(child);
                let (time, date) = words(self.stamp);
                k.set_sft16(entry, sft::TIME, time);
                k.set_sft16(entry, sft::DATE, date);
                let info = k.sft16(entry, sft::INFO);
                k.set_sft16(entry, sft::INFO, info | info::STAMPED);
            }
            PART => {
                take!(child);
            }
            _ => {}
        }
        let Some(segment) = k.find_buffer(drive, sector) else {
            *phase = PART;
            return Flow::Call(Frame::of(&Buffer {
                drive,
                sector,
                kind: 0,
                fresh: false,
                victim: 0,
            }));
        };
        let slot = u16::from(k.sft8(entry, sft::DIR_INDEX));
        let Some(d) = k.dpb(drive) else {
            return Flow::Return(Err(Fault::Error(error::INVALID_DRIVE)));
        };
        if slot >= d.sector_bytes / ENTRY_BYTES {
            return Flow::Return(Err(Fault::Error(error::INVALID_DATA)));
        }
        let at = buffer::DATA + slot * ENTRY_BYTES;
        let mut raw = [0u8; ENTRY];
        k.g.read_bytes(segment, at, &mut raw);
        let mut stored = ShortEntry::decode(&raw);
        stored.attributes |= attr::ARCHIVE;
        stored.stamps.written.time = k.sft16(entry, sft::TIME);
        stored.stamps.written.date = k.sft16(entry, sft::DATE);
        stored.cluster = stored.cluster & 0xFFFF_0000 | u32::from(k.sft16(entry, sft::FIRST));
        stored.size = k.sft32(entry, sft::SIZE);
        k.g.write_bytes(segment, at, &stored.encode());
        k.dirty(segment);
        let info = k.sft16(entry, sft::INFO);
        k.set_sft16(entry, sft::INFO, (info | info::CLEAN) & !info::STAMPED);
        Flow::Return(Ok(0))
    }
}

locals! {
    /// Closes handle `handle` of the current process — or, `by_index`, the
    /// kernel's own reference to SFT entry `index`: the entry's last
    /// reference writes the directory entry back, flushes the drive, and
    /// calls the device's close request.
    pub struct Close {
        handle: u16,
        index: u16,
        by_index: bool,
    }
}

const UPDATED: u8 = 1;
const FLUSHED: u8 = 2;
const CLOSED_DEVICE: u8 = 3;

impl Routine for Close {
    const ID: Id = Id::Close;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            UPDATED => {
                take!(child);
                let Some(entry) = k.sft(self.index) else {
                    return Flow::Return(Ok(0));
                };
                let drive = k.sft_drive(entry);
                *phase = FLUSHED;
                Flow::Call(Frame::of(&Flush {
                    segment: 0,
                    drive,
                    copy: 0,
                }))
            }
            FLUSHED => {
                take!(child);
                self.device_close(k, phase)
            }
            CLOSED_DEVICE => {
                take!(child);
                Flow::Return(Ok(0))
            }
            _ => {
                let (index, entry) = if self.by_index {
                    match k.sft(self.index) {
                        Some(entry) if k.sft16(entry, sft::HANDLES) != 0 => (self.index, entry),
                        _ => return Flow::Return(Err(Fault::Error(error::INVALID_HANDLE))),
                    }
                } else {
                    match k.handle(self.handle) {
                        Ok((index, entry)) => (u16::from(index), entry),
                        Err(fault) => return Flow::Return(Err(fault)),
                    }
                };
                self.index = index;
                if !self.by_index {
                    k.set_handle(self.handle, CLOSED);
                }
                let handles = k.sft16(entry, sft::HANDLES).saturating_sub(1);
                if handles > 0 {
                    k.set_sft16(entry, sft::HANDLES, handles);
                    return Flow::Return(Ok(0));
                }
                if k.sft_device(entry).is_some() {
                    return self.device_close(k, phase);
                }
                if k.sft_remote(entry).is_some() {
                    k.set_sft16(entry, sft::HANDLES, 0);
                    return Flow::Return(redirect::close(k, entry).map(|()| 0));
                }
                *phase = UPDATED;
                Flow::Call(Frame::of(&DirUpdate { index, stamp: 0 }))
            }
        }
    }
}

impl Close {
    fn device_close(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        let Some(entry) = k.sft(self.index) else {
            return Flow::Return(Ok(0));
        };
        let (device, unit) = if let Some(device) = k.sft_device(entry) {
            (device, 0)
        } else {
            let drive = k.sft_drive(entry);
            let Some(d) = k.dpb(drive) else {
                k.set_sft16(entry, sft::HANDLES, 0);
                return Flow::Return(Ok(0));
            };
            (d.driver, d.unit)
        };
        k.set_sft16(entry, sft::HANDLES, 0);
        if k.attributes(device) & attribute::OPEN_CLOSE == 0 {
            return Flow::Return(Ok(0));
        }
        k.request(unit, command::CLOSE);
        *phase = CLOSED_DEVICE;
        Flow::Call(Frame::of(&Device { header: device }))
    }
}

locals! {
    /// Commits SFT entry `index` (function `68h`): its directory entry
    /// written back and its drive's buffers flushed; a device's needs
    /// nothing.
    pub struct Commit {
        index: u16,
    }
}

impl Routine for Commit {
    const ID: Id = Id::Commit;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let Some(entry) = k.sft(self.index) else {
            return Flow::Return(Err(Fault::Error(error::INVALID_HANDLE)));
        };
        if k.sft_device(entry).is_some() {
            return Flow::Return(Ok(0));
        }
        if k.sft_remote(entry).is_some() {
            return Flow::Return(redirect::commit(k, entry).map(|()| 0));
        }
        match *phase {
            UPDATED => {
                take!(child);
                let drive = k.sft_drive(entry);
                *phase = FLUSHED;
                Flow::Call(Frame::of(&Flush {
                    segment: 0,
                    drive,
                    copy: 0,
                }))
            }
            FLUSHED => {
                take!(child);
                Flow::Return(Ok(0))
            }
            _ => {
                *phase = UPDATED;
                Flow::Call(Frame::of(&DirUpdate {
                    index: self.index,
                    stamp: 0,
                }))
            }
        }
    }
}

/// The new position function `42h` gives: `offset` from the start, the
/// position, or the end; DOS lets a position go below zero, where it wraps.
#[must_use]
pub fn seek(origin: u8, offset: u32, position: u32, size: u32) -> Option<u32> {
    match origin {
        0 => Some(offset),
        1 => Some(position.wrapping_add(offset)),
        2 => Some(size.wrapping_add(offset)),
        _ => None,
    }
}
