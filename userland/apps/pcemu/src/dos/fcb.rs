//! The FCB calls: files named by a CP/M-style file control block in the
//! caller's memory, behind an extended block's header when it asks for
//! attributes. An open FCB's file is an entry of DOS's own FCB table
//! (`FCBS=`): when the table is full the entry used longest ago, beyond the
//! protected ones, is closed and reused, and an FCB whose entry was taken is
//! opened again by its name when next used.

use super::data::{driver, level, sft, sysvars, SEGMENT, SYSVARS};
use super::dir::{scan, DirScan, Resolve, HIDDEN_KINDS};
use super::dir_write::Store;
use super::engine::{locals, take, Far, Fault, Flow, Frame, Id, Kernel, Outcome, Routine};
use super::file::{access, info, Close, MakeFile, Read, Write, FCB_FILES};
use super::fs::{error, written_back, FreeChain, Media, MediaLabel};
use super::name::{self, upper};
use tairix_fat::dir::{attr, DELETED, ENTRY};

mod remote;

/// An FCB's fields, from the start of its normal part.
pub mod field {
    /// The drive: `0` the current one, `1` A:.
    pub const DRIVE: u16 = 0x00;
    /// The name, as a directory entry holds it.
    pub const NAME: u16 = 0x01;
    /// The block the next sequential record is in, 128 records a block.
    pub const BLOCK: u16 = 0x0C;
    /// The record size.
    pub const RECORD_SIZE: u16 = 0x0E;
    /// The file's size.
    pub const SIZE: u16 = 0x10;
    /// Its date.
    pub const DATE: u16 = 0x14;
    /// Its time.
    pub const TIME: u16 = 0x16;
    /// The FCB-table entry an open FCB's file is in: the kernel's own.
    pub const FILE: u16 = 0x18;
    /// Set while the FCB is open.
    pub const OPEN: u16 = 0x19;
    /// A search's place, over the fields an open FCB uses, where DOS keeps
    /// it — `CHKDSK` finds the entry it fixes by the index: the attributes
    /// looked for, the index of the entry last found, its directory's first
    /// cluster, and the drive, from `1`.
    pub const SEARCH_ATTRIBUTES: u16 = 0x0C;
    pub const SEARCH_INDEX: u16 = 0x0D;
    pub const SEARCH_DIRECTORY: u16 = 0x0F;
    pub const SEARCH_DRIVE: u16 = 0x15;
    /// The record within the block.
    pub const RECORD: u16 = 0x20;
    /// The random record number.
    pub const RANDOM: u16 = 0x21;
    /// A rename's new name.
    pub const NEW_NAME: u16 = 0x11;
    /// What tells an open FCB's file from another's, in the reserved bytes:
    /// the `identity` of the entry it was opened in.
    pub const IDENTITY: u16 = 0x1A;
}

/// An extended FCB's first byte.
const EXTENDED: u8 = 0xFF;
/// Bytes of the extended header before the normal FCB.
const EXTENDED_HEADER: u16 = 7;
/// What [`field::OPEN`] holds while the FCB is open.
const OPEN_MARK: u8 = 0x5A;
/// Records a block.
const BLOCK_RECORDS: u32 = 128;
/// The record size an FCB has when it names none.
const DEFAULT_RECORD: u16 = 128;
/// Bytes of [`field::IDENTITY`].
const IDENTITY_LEN: usize = 5;

/// The FCB a call names: where its normal part is, and the attributes an
/// extended header asks for.
#[derive(Clone, Copy, Debug)]
pub struct Named {
    /// The normal part.
    pub at: Far,
    /// The extended header, if there is one.
    pub extended: Option<Far>,
    /// The attributes searched for: an extended header's, else none but
    /// plain files.
    pub attributes: u8,
}

/// The FCB at `pointer`, as a call names it.
pub fn named(k: &mut Kernel<'_, '_>, pointer: Far) -> Named {
    if k.g.read8(pointer.segment, pointer.offset) == EXTENDED {
        let attributes = k.g.read8(pointer.segment, pointer.offset.wrapping_add(6));
        return Named {
            at: pointer.add(EXTENDED_HEADER),
            extended: Some(pointer),
            attributes,
        };
    }
    Named {
        at: pointer,
        extended: None,
        attributes: 0,
    }
}

impl Kernel<'_, '_> {
    fn fcb8(&mut self, fcb: Far, field: u16) -> u8 {
        let at = fcb.add(field);
        self.g.read8(at.segment, at.offset)
    }

    fn set_fcb8(&mut self, fcb: Far, field: u16, value: u8) {
        let at = fcb.add(field);
        self.g.write8(at.segment, at.offset, value);
    }

    fn fcb16(&mut self, fcb: Far, field: u16) -> u16 {
        self.read16_at(fcb.add(field))
    }

    fn set_fcb16(&mut self, fcb: Far, field: u16, value: u16) {
        self.write16_at(fcb.add(field), value);
    }

    fn fcb_name(&mut self, fcb: Far, field: u16) -> [u8; 11] {
        let mut out = [0u8; 11];
        let at = fcb.add(field);
        self.g.read_bytes(at.segment, at.offset, &mut out);
        out
    }

    /// The drive an FCB names, `0` for A:, if it is one.
    fn fcb_drive(&mut self, fcb: Far) -> Option<u8> {
        let drive = match self.fcb8(fcb, field::DRIVE) {
            0 => self.current_drive(),
            n => n - 1,
        };
        self.is_drive(drive).then_some(drive)
    }

    /// The drive the FCB's search was begun on, while DOS still has it.
    fn search_drive(&mut self, fcb: Far) -> Option<u8> {
        let drive = self.fcb8(fcb, field::SEARCH_DRIVE).checked_sub(1)?;
        self.dpb(drive).is_some().then_some(drive)
    }

    /// The FCB's record size, the default for none.
    fn record_size(&mut self, fcb: Far) -> u16 {
        match self.fcb16(fcb, field::RECORD_SIZE) {
            0 => DEFAULT_RECORD,
            size => size,
        }
    }

    /// The FCB's random record number: three bytes of it for records of 64
    /// bytes or more.
    fn random_record(&mut self, fcb: Far) -> u32 {
        let low = u32::from(self.fcb16(fcb, field::RANDOM));
        let high = u32::from(self.fcb16(fcb, field::RANDOM + 2));
        let record = low | high << 16;
        if self.record_size(fcb) >= 64 {
            record & 0x00FF_FFFF
        } else {
            record
        }
    }

    fn set_random_record(&mut self, fcb: Far, record: u32) {
        let (low, high) = super::engine::words(record);
        self.set_fcb16(fcb, field::RANDOM, low);
        if self.record_size(fcb) >= 64 {
            self.set_fcb8(fcb, field::RANDOM + 2, high.to_le_bytes()[0]);
        } else {
            self.set_fcb16(fcb, field::RANDOM + 2, high);
        }
    }

    /// The sequential record the FCB is at.
    fn sequential_record(&mut self, fcb: Far) -> u32 {
        let block = u32::from(self.fcb16(fcb, field::BLOCK));
        block * BLOCK_RECORDS + u32::from(self.fcb8(fcb, field::RECORD) & 0x7F)
    }

    fn set_sequential_record(&mut self, fcb: Far, record: u32) {
        let block = u16::try_from(record / BLOCK_RECORDS).unwrap_or(u16::MAX);
        self.set_fcb16(fcb, field::BLOCK, block);
        self.set_fcb8(
            fcb,
            field::RECORD,
            u8::try_from(record % BLOCK_RECORDS).unwrap_or(0),
        );
    }

    /// The FCB table: its first block and the entries in it.
    fn fcb_table(&mut self) -> Option<(Far, u16)> {
        let at = u16::try_from(i32::from(SYSVARS) + i32::from(sysvars::FCBS)).ok()?;
        let block = self.get_far(at);
        if block.offset == 0xFFFF {
            return None;
        }
        Some((block, self.read16_at(block.add(4))))
    }

    /// The SFT index of the open file `fcb` names, if its entry is still
    /// its own: the file it was opened on, by what places that file rather
    /// than by a name, which a pattern or a name in lower case would not
    /// match.
    fn fcb_file(&mut self, fcb: Far) -> Option<u16> {
        if self.fcb8(fcb, field::OPEN) != OPEN_MARK {
            return None;
        }
        let index = FCB_FILES + u16::from(self.fcb8(fcb, field::FILE));
        let entry = self.sft(index)?;
        let drive = self.fcb8(fcb, field::DRIVE).wrapping_sub(1);
        let mut held = [0u8; IDENTITY_LEN];
        let at = fcb.add(field::IDENTITY);
        self.g.read_bytes(at.segment, at.offset, &mut held);
        let open = self.sft16(entry, sft::HANDLES) != 0
            && self.sft16(entry, sft::MODE) & sft::FCB_MODE != 0
            && (self.sft_device(entry).is_some() || self.sft_drive(entry) == drive)
            && self.identity(entry) == held;
        open.then_some(index)
    }

    /// What tells the file open in SFT entry `entry` from another's: where
    /// its directory entry lies — a share's handle on a shared drive — or
    /// a device's header.
    fn identity(&mut self, entry: Far) -> [u8; IDENTITY_LEN] {
        let mut out = [0u8; IDENTITY_LEN];
        if let Some(device) = self.sft_device(entry) {
            out[..2].copy_from_slice(&device.offset.to_le_bytes());
            out[2..4].copy_from_slice(&device.segment.to_le_bytes());
        } else {
            out[..4].copy_from_slice(&self.sft32(entry, sft::DIR_SECTOR).to_le_bytes());
            out[4] = self.sft8(entry, sft::DIR_INDEX);
        }
        out
    }

    /// An FCB-table entry to open a file in: a free one, else the one used
    /// longest ago past the protected ones, which is still open when the
    /// flag says so.
    fn claim_fcb_file(&mut self) -> Option<(u16, bool)> {
        let (_, count) = self.fcb_table()?;
        let at = u16::try_from(i32::from(SYSVARS) + i32::from(sysvars::PROTECTED_FCBS)).ok()?;
        let protected = self.get16(at);
        let clock = self.get16(driver::FCB_CLOCK);
        let mut oldest: Option<(u16, u16)> = None;
        for n in 0..count {
            let entry = self.sft(FCB_FILES + n)?;
            if self.sft16(entry, sft::HANDLES) == 0 {
                return Some((FCB_FILES + n, false));
            }
            if n < protected {
                continue;
            }
            let age = clock.wrapping_sub(self.sft16(entry, sft::USED));
            if oldest.is_none_or(|(_, best)| age > best) {
                oldest = Some((n, age));
            }
        }
        oldest.map(|(n, _)| (FCB_FILES + n, true))
    }

    /// Stamps FCB-table entry `entry` used now.
    fn touch_fcb_file(&mut self, entry: Far) {
        let clock = self.get16(driver::FCB_CLOCK).wrapping_add(1);
        self.set16(driver::FCB_CLOCK, clock);
        self.set_sft16(entry, sft::USED, clock);
    }

    /// Sets the running call's path to every name in `drive`'s current
    /// directory, for a lookup there.
    fn set_current_pattern(&mut self, drive: u8) {
        let mut path = *b"A:????????.???";
        path[0] = b'A' + drive;
        self.set_path(level::PATH, &path);
    }
}

/// The FCB results functions report in AL.
pub mod result {
    /// Done.
    pub const DONE: u32 = 0x00;
    /// Reading: the end of the file, nothing read. Writing: the disk full.
    pub const END: u32 = 0x01;
    /// The DTA would wrap past its segment's end.
    pub const WRAP: u32 = 0x02;
    /// Reading: a part record at the end, zero-filled.
    pub const PART: u32 = 0x03;
    /// Failed.
    pub const FAILED: u32 = 0xFF;
}

locals! {
    /// Resolves drive `drive`'s current directory, its medium checked, and
    /// sets the running call's name to `pattern`'s FCB name. Returns the
    /// directory's first cluster.
    pub struct Lookup {
        drive: u8,
        fcb: Far,
        name_field: u16,
    }
}

const RESOLVED: u8 = 1;

impl Routine for Lookup {
    const ID: Id = Id::FcbLookup;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if *phase == RESOLVED {
            let found = take!(child);
            let pattern = k.fcb_name(self.fcb, self.name_field);
            k.set_name(&pattern.map(upper));
            return Flow::Return(Ok(found & 0xFFFF));
        }
        k.set_current_pattern(self.drive);
        *phase = RESOLVED;
        Flow::Call(Frame::of(&Resolve::of(super::dir::mode::PARENT, false)))
    }
}

locals! {
    /// Function `0Fh` or `16h`: opens the file the FCB `fcb` names — made
    /// first, empty, when `create` — into an FCB-table entry; or, with
    /// `reopen`, opens an open FCB's file again in place of an entry taken
    /// from it. Returns the entry's SFT index.
    pub struct FcbOpen {
        fcb: Far,
        attributes: u8,
        create: bool,
        reopen: bool,
        drive: u8,
        file: u16,
        device: Far,
        /// The name of the shared drive's file found to open, and whether
        /// the share lets it be written.
        shared: Option<[u8; 11]>,
        writable: bool,
    }
}

const LOOKED: u8 = 2;
const FOUND: u8 = 3;
const EVICTED: u8 = 4;

impl Routine for FcbOpen {
    const ID: Id = Id::FcbOpen;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            LOOKED => {
                let directory = u16::try_from(take!(child)).unwrap_or(0);
                *phase = FOUND;
                Flow::Call(Frame::of(&DirScan {
                    drive: self.drive,
                    directory,
                    index: 0,
                    attributes: self.attributes & HIDDEN_KINDS,
                    mode: scan::FIND,
                    cluster: 0,
                    position: 0,
                }))
            }
            FOUND => {
                if let Err(fault) = child {
                    return Flow::Return(Err(match fault {
                        Fault::Error(error::NO_MORE_FILES) => Fault::Error(error::FILE_NOT_FOUND),
                        other => other,
                    }));
                }
                self.claim(k, phase)
            }
            EVICTED => {
                take!(child);
                self.open(k)
            }
            _ => {
                let Some(drive) = k.fcb_drive(self.fcb) else {
                    return Flow::Return(Err(Fault::Error(error::INVALID_DRIVE)));
                };
                self.drive = drive;
                let pattern = k.fcb_name(self.fcb, field::NAME).map(upper);
                if let Some(device) = k.char_device(&pattern) {
                    self.device = device;
                    return self.claim(k, phase);
                }
                if k.share_of(drive).is_some() {
                    return self.find_shared(k, phase, pattern);
                }
                if self.create {
                    let mut spelt = [0u8; 13];
                    let len = name::spell(&pattern, &mut spelt);
                    if len == 0 || name::is_wild(&pattern) {
                        return Flow::Return(Err(Fault::Error(error::PATH_NOT_FOUND)));
                    }
                    // A label belongs in the root, where a search for one looks.
                    let root: &[u8] = if self.attributes & attr::VOLUME_ID != 0 {
                        b"\\"
                    } else {
                        b""
                    };
                    let path = [&[b'A' + drive, b':'][..], root, &spelt[..len]].concat();
                    k.set_path(level::PATH, &path);
                    *phase = FOUND;
                    return Flow::Call(Frame::of(&MakeFile {
                        attributes: self.attributes
                            & (attr::READ_ONLY | attr::HIDDEN | attr::SYSTEM | attr::VOLUME_ID),
                        new: false,
                        drive: 0,
                        directory: 0,
                        exists: false,
                        stamp: 0,
                    }));
                }
                *phase = LOOKED;
                Flow::Call(Frame::of(&Lookup {
                    drive,
                    fcb: self.fcb,
                    name_field: field::NAME,
                }))
            }
        }
    }
}

impl FcbOpen {
    /// Finds the file of a shared drive to open, or to make, before an
    /// FCB-table entry is taken for it.
    fn find_shared(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, pattern: [u8; 11]) -> Flow {
        if self.create {
            // A share's label is the name it was shared under.
            if self.attributes & attr::VOLUME_ID != 0 {
                return Flow::Return(Err(Fault::Error(error::ACCESS_DENIED)));
            }
            if crate::folder::DosName::new(pattern, false).is_none() {
                return Flow::Return(Err(Fault::Error(error::PATH_NOT_FOUND)));
            }
            self.shared = Some(pattern);
            self.writable = true;
        } else {
            match remote::find_file(k, self.fcb, self.attributes, self.drive) {
                Ok(facts) => {
                    self.shared = Some(*facts.name.bytes());
                    self.writable = facts.writable;
                }
                Err(fault) => return Flow::Return(Err(fault)),
            }
        }
        self.claim(k, phase)
    }

    /// Takes an FCB-table entry, closing the one used longest ago when none
    /// is free.
    fn claim(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        let Some((file, in_use)) = k.claim_fcb_file() else {
            return Flow::Return(Err(Fault::Error(error::TOO_MANY_OPEN)));
        };
        self.file = file;
        if !in_use {
            return self.open(k);
        }
        *phase = EVICTED;
        Flow::Call(Frame::of(&Close {
            handle: 0,
            index: file,
            by_index: true,
        }))
    }

    /// Opens the device, or the entry found, in the claimed FCB-table entry.
    fn open(&mut self, k: &mut Kernel<'_, '_>) -> Flow {
        let Some(entry) = k.sft(self.file) else {
            return Flow::Return(Err(Fault::Error(error::TOO_MANY_OPEN)));
        };
        if self.device.segment != 0 {
            k.open_device(entry, self.device, access::BOTH);
        } else {
            if let Some(name) = self.shared {
                let opened = remote::open(k, self.drive, name, self.create, self.writable, entry);
                if let Err(fault) = opened {
                    return Flow::Return(Err(fault));
                }
            } else {
                let read_only = k.found_entry().attributes & attr::READ_ONLY != 0;
                let mode = if read_only {
                    access::READ
                } else {
                    access::BOTH
                };
                k.open_file(entry, self.drive, mode);
            }
            if self.create {
                let info = k.sft16(entry, sft::INFO);
                k.set_sft16(entry, sft::INFO, info & !info::CLEAN);
            }
        }
        let word = k.sft16(entry, sft::MODE);
        k.set_sft16(entry, sft::MODE, word | sft::FCB_MODE);
        k.touch_fcb_file(entry);
        self.fill(k, entry);
        Flow::Return(Ok(u32::from(self.file)))
    }

    /// The FCB's fields for the file now open in `entry`.
    fn fill(&self, k: &mut Kernel<'_, '_>, entry: Far) {
        let fcb = self.fcb;
        if k.fcb8(fcb, field::DRIVE) == 0 {
            k.set_fcb8(fcb, field::DRIVE, self.drive + 1);
        }
        if !self.reopen {
            k.set_fcb16(fcb, field::BLOCK, 0);
            k.set_fcb16(fcb, field::RECORD_SIZE, DEFAULT_RECORD);
        }
        let size = k.sft32(entry, sft::SIZE);
        k.write16_at(
            fcb.add(field::SIZE),
            u16::try_from(size & 0xFFFF).unwrap_or(0),
        );
        k.write16_at(
            fcb.add(field::SIZE + 2),
            u16::try_from(size >> 16).unwrap_or(0),
        );
        let date = k.sft16(entry, sft::DATE);
        let time = k.sft16(entry, sft::TIME);
        k.set_fcb16(fcb, field::DATE, date);
        k.set_fcb16(fcb, field::TIME, time);
        k.set_fcb8(
            fcb,
            field::FILE,
            u8::try_from(self.file - FCB_FILES).unwrap_or(0),
        );
        k.set_fcb8(fcb, field::OPEN, OPEN_MARK);
        let identity = k.identity(entry);
        let at = fcb.add(field::IDENTITY);
        k.g.write_bytes(at.segment, at.offset, &identity);
    }
}

locals! {
    /// Function `10h`: closes the FCB `fcb`'s file, its entry written back.
    /// One whose entry was taken is closed already.
    pub struct FcbClose {
        fcb: Far,
    }
}

const CLOSED: u8 = 1;

impl Routine for FcbClose {
    const ID: Id = Id::FcbClose;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if *phase == CLOSED {
            take!(child);
            return Flow::Return(Ok(result::DONE));
        }
        let was_open = k.fcb8(self.fcb, field::OPEN) == OPEN_MARK;
        let file = k.fcb_file(self.fcb);
        k.set_fcb8(self.fcb, field::OPEN, 0);
        let Some(index) = file else {
            return Flow::Return(Ok(if was_open {
                result::DONE
            } else {
                result::FAILED
            }));
        };
        *phase = CLOSED;
        Flow::Call(Frame::of(&Close {
            handle: 0,
            index,
            by_index: true,
        }))
    }
}

locals! {
    /// Functions `14h`, `15h`, `21h`, `22h`, `27h` and `28h`: moves records
    /// between the DTA and the FCB `fcb`'s file — sequentially from its
    /// block and record, or at its random record, one or `records` of them
    /// — in transfers as large as a segment holds. Returns the result code.
    pub struct FcbIo {
        function: u8,
        fcb: Far,
        file: u16,
        records: u16,
        done: u32,
        wrapped: bool,
    }
}

const OPENED: u8 = 1;
const MOVED: u8 = 2;

impl Routine for FcbIo {
    const ID: Id = Id::FcbIo;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            OPENED => {
                self.file = u16::try_from(take!(child)).unwrap_or(0);
                self.start(k, phase)
            }
            MOVED => {
                let moved = take!(child);
                self.done += moved;
                let size = u32::from(k.record_size(self.fcb));
                let wanted = u32::from(self.records) * size;
                let chunk = chunk_of(size, wanted - (self.done - moved));
                if moved < chunk || self.done >= wanted {
                    return self.finish(k);
                }
                self.transfer(k, phase)
            }
            _ => match k.fcb_file(self.fcb) {
                Some(file) => {
                    self.file = file;
                    self.start(k, phase)
                }
                None if k.fcb8(self.fcb, field::OPEN) == OPEN_MARK => {
                    *phase = OPENED;
                    Flow::Call(Frame::of(&FcbOpen {
                        fcb: self.fcb,
                        reopen: true,
                        ..FcbOpen::default()
                    }))
                }
                None => Flow::Return(Ok(result::END)),
            },
        }
    }
}

/// Bytes the next transfer moves: as many whole records of `size` as fit a
/// transfer, up to `left`.
fn chunk_of(size: u32, left: u32) -> u32 {
    let most = (0xFFFF / size).max(1) * size;
    left.min(most)
}

impl FcbIo {
    fn is_write(&self) -> bool {
        matches!(self.function, 0x15 | 0x22 | 0x28)
    }

    /// The record the transfer starts at.
    fn first_record(&self, k: &mut Kernel<'_, '_>) -> u32 {
        match self.function {
            0x14 | 0x15 => k.sequential_record(self.fcb),
            _ => k.random_record(self.fcb),
        }
    }

    fn start(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        let Some(entry) = k.sft(self.file) else {
            return Flow::Return(Ok(result::END));
        };
        k.touch_fcb_file(entry);
        let size = u32::from(k.record_size(self.fcb));
        let first = self.first_record(k);
        if !matches!(self.function, 0x14 | 0x15) {
            k.set_sequential_record(self.fcb, first);
        }
        self.records = if matches!(self.function, 0x27 | 0x28) {
            k.caller(super::engine::Saved::Cx)
        } else {
            1
        };
        let Some(position) = first.checked_mul(size) else {
            return Flow::Return(Ok(result::END));
        };
        k.set_sft32(entry, sft::POSITION, position);
        if self.function == 0x28 && self.records == 0 {
            *phase = MOVED;
            return Flow::Call(Frame::of(&Write {
                index: self.file,
                buffer: Far::default(),
                count: 0,
                done: 0,
                kept: 0,
                next: 0,
            }));
        }
        let dta = k.dta();
        let room = (0x1_0000 - u32::from(dta.offset)) / size;
        if room < u32::from(self.records) {
            self.wrapped = true;
            self.records = u16::try_from(room).unwrap_or(0);
            if self.records == 0 {
                return Flow::Return(Ok(result::WRAP));
            }
        }
        self.done = 0;
        self.transfer(k, phase)
    }

    /// Moves the next run of records.
    fn transfer(&self, k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        let size = u32::from(k.record_size(self.fcb));
        let wanted = u32::from(self.records) * size;
        let count = u16::try_from(chunk_of(size, wanted - self.done)).unwrap_or(0);
        let buffer = k.dta().add(u16::try_from(self.done).unwrap_or(0));
        *phase = MOVED;
        if self.is_write() {
            Flow::Call(Frame::of(&Write {
                index: self.file,
                buffer,
                count,
                done: 0,
                kept: 0,
                next: 0,
            }))
        } else {
            Flow::Call(Frame::of(&Read {
                index: self.file,
                buffer,
                count,
                done: 0,
                steps: 0,
            }))
        }
    }

    /// The transfer is over: a part record filled out, the FCB moved on.
    fn finish(&self, k: &mut Kernel<'_, '_>) -> Flow {
        let size = u32::from(k.record_size(self.fcb));
        let whole = self.done / size;
        let part = self.done % size;
        let mut moved = whole;
        let code = if self.is_write() {
            if let Some(entry) = k.sft(self.file) {
                let length = k.sft32(entry, sft::SIZE);
                k.write16_at(
                    self.fcb.add(field::SIZE),
                    u16::try_from(length & 0xFFFF).unwrap_or(0),
                );
                k.write16_at(
                    self.fcb.add(field::SIZE + 2),
                    u16::try_from(length >> 16).unwrap_or(0),
                );
            }
            if whole < u32::from(self.records) {
                result::END
            } else if self.wrapped {
                result::WRAP
            } else {
                result::DONE
            }
        } else if part != 0 {
            let dta = k.dta();
            let from = u16::try_from(self.done).unwrap_or(0);
            let fill = u16::try_from(size - part).unwrap_or(0);
            for n in 0..fill {
                let at = dta.add(from.wrapping_add(n));
                k.g.write8(at.segment, at.offset, 0);
            }
            moved += 1;
            result::PART
        } else if whole == 0 {
            result::END
        } else if self.wrapped && whole == u32::from(self.records) {
            result::WRAP
        } else if whole < u32::from(self.records) {
            result::END
        } else {
            result::DONE
        };
        let first = self.first_record(k);
        match self.function {
            0x14 | 0x15 => k.set_sequential_record(self.fcb, first + moved),
            0x27 | 0x28 => {
                let next = first + moved;
                k.set_random_record(self.fcb, next);
                k.set_sequential_record(self.fcb, next);
                k.set_caller(super::engine::Saved::Cx, u16::try_from(moved).unwrap_or(0));
            }
            _ => {}
        }
        Flow::Return(Ok(code))
    }
}

locals! {
    /// Functions `11h` and `12h`: finds the first entry in the current
    /// directory the FCB `fcb`'s name matches, or, `next`, the next after
    /// the one its search found last; the entry is copied to the DTA as an
    /// unopened FCB, extended when the search's was.
    pub struct FcbSearch {
        fcb: Far,
        extended: bool,
        attributes: u8,
        next: bool,
        drive: u8,
        directory: u16,
        index: u16,
    }
}

const SCANNED: u8 = 3;
const MEDIA_CHECKED: u8 = 8;

impl Routine for FcbSearch {
    const ID: Id = Id::FcbSearch;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            LOOKED => {
                let directory = u16::try_from(take!(child)).unwrap_or(0);
                self.directory = if self.attributes == attr::VOLUME_ID {
                    0
                } else {
                    directory
                };
                self.index = 0;
                k.set_fcb16(self.fcb, field::SEARCH_DIRECTORY, self.directory);
                k.set_fcb8(self.fcb, field::SEARCH_ATTRIBUTES, self.attributes);
                k.set_fcb8(self.fcb, field::SEARCH_DRIVE, self.drive + 1);
                self.scan(phase)
            }
            MEDIA_CHECKED => {
                take!(child);
                self.scan(phase)
            }
            SCANNED => {
                let index = match child {
                    Ok(index) => index,
                    Err(Fault::Error(error::NO_MORE_FILES)) => {
                        return Flow::Return(Ok(result::FAILED))
                    }
                    Err(fault) => return Flow::Return(Err(fault)),
                };
                k.set_fcb16(
                    self.fcb,
                    field::SEARCH_INDEX,
                    u16::try_from(index).unwrap_or(u16::MAX),
                );
                let entry = k.found();
                let dta = k.dta();
                let mut out = [0u8; 7 + 1 + ENTRY];
                let mut at = 0;
                if self.extended {
                    out[0] = EXTENDED;
                    out[6] = self.attributes;
                    at = 7;
                }
                out[at] = self.drive + 1;
                out[at + 1..at + 1 + ENTRY].copy_from_slice(&entry);
                k.g.write_bytes(dta.segment, dta.offset, &out[..at + 1 + ENTRY]);
                Flow::Return(Ok(result::DONE))
            }
            _ => {
                if self.next {
                    if let Some(drive) = remote::search_drive(k, self.fcb) {
                        if k.share_of(drive).is_none() {
                            return Flow::Return(Ok(result::FAILED));
                        }
                        let attributes = k.fcb8(self.fcb, field::SEARCH_ATTRIBUTES);
                        let extended = self.extended;
                        return Flow::Return(remote::search(
                            k, self.fcb, extended, attributes, drive, true,
                        ));
                    }
                }
                let drive = if self.next {
                    k.search_drive(self.fcb)
                } else {
                    k.fcb_drive(self.fcb)
                };
                let Some(drive) = drive else {
                    return Flow::Return(Ok(result::FAILED));
                };
                if !self.next && k.share_of(drive).is_some() {
                    let (extended, attributes) = (self.extended, self.attributes);
                    return Flow::Return(remote::search(
                        k, self.fcb, extended, attributes, drive, false,
                    ));
                }
                self.drive = drive;
                if self.next {
                    let pattern = k.fcb_name(self.fcb, field::NAME).map(upper);
                    k.set_name(&pattern);
                    self.directory = k.fcb16(self.fcb, field::SEARCH_DIRECTORY);
                    self.attributes = k.fcb8(self.fcb, field::SEARCH_ATTRIBUTES);
                    self.index = k.fcb16(self.fcb, field::SEARCH_INDEX).saturating_add(1);
                    *phase = MEDIA_CHECKED;
                    return Flow::Call(Frame::of(&Media { drive, status: 0 }));
                }
                *phase = LOOKED;
                Flow::Call(Frame::of(&Lookup {
                    drive,
                    fcb: self.fcb,
                    name_field: field::NAME,
                }))
            }
        }
    }
}

impl FcbSearch {
    fn scan(&self, phase: &mut u8) -> Flow {
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
    /// Function `13h`: deletes every file in the current directory the FCB
    /// `fcb`'s name matches but read-only ones, each entry marked deleted
    /// before its clusters are freed.
    pub struct FcbDelete {
        fcb: Far,
        attributes: u8,
        drive: u8,
        directory: u16,
        index: u16,
        cluster: u16,
        deleted: u16,
    }
}

const ERASED: u8 = 4;
const FREED: u8 = 5;
const UNLABELLED: u8 = 6;
/// The last phase of a call that changed a directory: its drive's buffers
/// written back.
const WRITTEN_BACK: u8 = 8;

impl Routine for FcbDelete {
    const ID: Id = Id::FcbDelete;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            LOOKED => {
                let directory = u16::try_from(take!(child)).unwrap_or(0);
                self.directory = if self.labels() { 0 } else { directory };
                self.index = 0;
                self.scan(phase)
            }
            SCANNED => {
                let index = match child {
                    Ok(index) => u16::try_from(index).unwrap_or(u16::MAX),
                    Err(Fault::Error(error::NO_MORE_FILES))
                        if self.labels() && self.deleted > 0 =>
                    {
                        *phase = UNLABELLED;
                        return Flow::Call(Frame::of(&MediaLabel::of(self.drive, true)));
                    }
                    Err(Fault::Error(error::NO_MORE_FILES)) if self.deleted > 0 => {
                        *phase = WRITTEN_BACK;
                        return written_back(self.drive);
                    }
                    Err(Fault::Error(error::NO_MORE_FILES)) => {
                        return Flow::Return(Ok(result::FAILED))
                    }
                    Err(fault) => return Flow::Return(Err(fault)),
                };
                self.index = index.saturating_add(1);
                let mut entry = k.found_entry();
                let refused = if self.labels() {
                    attr::DIRECTORY
                } else {
                    attr::READ_ONLY | attr::DIRECTORY | attr::VOLUME_ID
                };
                if entry.attributes & refused != 0 {
                    return self.scan(phase);
                }
                self.cluster = super::dir::first_cluster(&entry);
                entry.name[0] = DELETED;
                k.set_found_entry(&entry);
                *phase = ERASED;
                Flow::Call(Frame::of(&Store::found(k, self.drive)))
            }
            ERASED => {
                take!(child);
                self.deleted += 1;
                if !k.dpb(self.drive).is_some_and(|d| d.in_volume(self.cluster)) {
                    return self.rescan(k, phase);
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
                self.rescan(k, phase)
            }
            UNLABELLED => {
                *phase = WRITTEN_BACK;
                written_back(self.drive)
            }
            WRITTEN_BACK => {
                take!(child);
                Flow::Return(Ok(result::DONE))
            }
            _ => {
                let Some(drive) = k.fcb_drive(self.fcb) else {
                    return Flow::Return(Ok(result::FAILED));
                };
                if k.share_of(drive).is_some() {
                    return Flow::Return(remote::delete(k, self.fcb, self.attributes, drive));
                }
                self.drive = drive;
                *phase = LOOKED;
                Flow::Call(Frame::of(&Lookup {
                    drive,
                    fcb: self.fcb,
                    name_field: field::NAME,
                }))
            }
        }
    }
}

impl FcbDelete {
    /// Whether the FCB asks for labels alone, which lie in the root.
    const fn labels(&self) -> bool {
        self.attributes == attr::VOLUME_ID
    }

    fn scan(&self, phase: &mut u8) -> Flow {
        *phase = SCANNED;
        let attributes = if self.labels() {
            attr::VOLUME_ID
        } else {
            self.attributes & HIDDEN_KINDS & !attr::DIRECTORY
        };
        Flow::Call(Frame::of(&DirScan {
            drive: self.drive,
            directory: self.directory,
            index: self.index,
            attributes,
            mode: scan::FIND,
            cluster: 0,
            position: 0,
        }))
    }

    /// Looks on for the next match, the pattern set again.
    fn rescan(&self, k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        let pattern = k.fcb_name(self.fcb, field::NAME).map(upper);
        k.set_name(&pattern);
        self.scan(phase)
    }
}

locals! {
    /// Function `17h`: renames every file in the current directory the FCB
    /// `fcb`'s name matches to its second name, a `?` there keeping the
    /// old name's byte; a name already taken ends the call.
    pub struct FcbRename {
        fcb: Far,
        attributes: u8,
        drive: u8,
        directory: u16,
        index: u16,
        sector: u32,
        slot: u16,
        renamed: u16,
        outcome: u8,
    }
}

const CHECKED: u8 = 6;
const STORED: u8 = 7;

impl Routine for FcbRename {
    const ID: Id = Id::FcbRename;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            LOOKED => {
                self.directory = u16::try_from(take!(child)).unwrap_or(0);
                self.index = 0;
                self.scan(phase)
            }
            SCANNED => {
                let index = match child {
                    Ok(index) => u16::try_from(index).unwrap_or(u16::MAX),
                    Err(Fault::Error(error::NO_MORE_FILES)) if self.renamed > 0 => {
                        return self.ended(phase, result::DONE)
                    }
                    Err(Fault::Error(error::NO_MORE_FILES)) => {
                        return Flow::Return(Ok(result::FAILED))
                    }
                    Err(fault) => return Flow::Return(Err(fault)),
                };
                self.index = index.saturating_add(1);
                let mut entry = k.found_entry();
                if entry.attributes & attr::VOLUME_ID != 0 {
                    return self.rescan(k, phase);
                }
                let template = k.fcb_name(self.fcb, field::NEW_NAME).map(upper);
                for (byte, &want) in entry.name.iter_mut().zip(&template) {
                    if want != b'?' {
                        *byte = want;
                    }
                }
                self.sector = k.found_sector();
                self.slot = k.found16(super::dir::found::SLOT);
                let at = k.scratch(level::KEPT);
                k.g.write_bytes(SEGMENT, at, &entry.encode());
                k.set_name(&entry.name);
                *phase = CHECKED;
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
            CHECKED => match child {
                Ok(_) if self.renamed > 0 => self.ended(phase, result::FAILED),
                Ok(_) => Flow::Return(Ok(result::FAILED)),
                Err(Fault::Error(error::NO_MORE_FILES)) => {
                    *phase = STORED;
                    Flow::Call(Frame::of(&Store {
                        drive: self.drive,
                        sector: self.sector,
                        slot: self.slot,
                        from: level::KEPT,
                    }))
                }
                Err(fault) => Flow::Return(Err(fault)),
            },
            STORED => {
                take!(child);
                self.renamed += 1;
                self.rescan(k, phase)
            }
            WRITTEN_BACK => {
                take!(child);
                Flow::Return(Ok(u32::from(self.outcome)))
            }
            _ => {
                let Some(drive) = k.fcb_drive(self.fcb) else {
                    return Flow::Return(Ok(result::FAILED));
                };
                if k.share_of(drive).is_some() {
                    return Flow::Return(remote::rename(k, self.fcb, self.attributes, drive));
                }
                self.drive = drive;
                *phase = LOOKED;
                Flow::Call(Frame::of(&Lookup {
                    drive,
                    fcb: self.fcb,
                    name_field: field::NAME,
                }))
            }
        }
    }
}

impl FcbRename {
    /// The call ends with `outcome` once what it renamed is written back.
    fn ended(&mut self, phase: &mut u8, outcome: u32) -> Flow {
        self.outcome = u8::try_from(outcome).unwrap_or(u8::MAX);
        *phase = WRITTEN_BACK;
        written_back(self.drive)
    }

    fn scan(&self, phase: &mut u8) -> Flow {
        *phase = SCANNED;
        Flow::Call(Frame::of(&DirScan {
            drive: self.drive,
            directory: self.directory,
            index: self.index,
            attributes: self.attributes & HIDDEN_KINDS,
            mode: scan::FIND,
            cluster: 0,
            position: 0,
        }))
    }

    fn rescan(&self, k: &mut Kernel<'_, '_>, phase: &mut u8) -> Flow {
        let pattern = k.fcb_name(self.fcb, field::NAME).map(upper);
        k.set_name(&pattern);
        self.scan(phase)
    }
}

locals! {
    /// Function `23h`: sets the FCB `fcb`'s random record to the file's size
    /// in its records, a part record counting as one.
    pub struct FcbSize {
        fcb: Far,
        attributes: u8,
        drive: u8,
    }
}

impl Routine for FcbSize {
    const ID: Id = Id::FcbSize;

    fn step(&mut self, k: &mut Kernel<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            LOOKED => {
                let directory = u16::try_from(take!(child)).unwrap_or(0);
                *phase = SCANNED;
                Flow::Call(Frame::of(&DirScan {
                    drive: self.drive,
                    directory,
                    index: 0,
                    attributes: self.attributes & HIDDEN_KINDS,
                    mode: scan::FIND,
                    cluster: 0,
                    position: 0,
                }))
            }
            SCANNED => match child {
                Ok(_) => {
                    let size = k.found_entry().size;
                    let records = size.div_ceil(u32::from(k.record_size(self.fcb)));
                    k.set_random_record(self.fcb, records);
                    Flow::Return(Ok(result::DONE))
                }
                Err(Fault::Error(error::NO_MORE_FILES)) => Flow::Return(Ok(result::FAILED)),
                Err(fault) => Flow::Return(Err(fault)),
            },
            _ => {
                let Some(drive) = k.fcb_drive(self.fcb) else {
                    return Flow::Return(Ok(result::FAILED));
                };
                if k.share_of(drive).is_some() {
                    return Flow::Return(remote::size(k, self.fcb, self.attributes, drive));
                }
                self.drive = drive;
                *phase = LOOKED;
                Flow::Call(Frame::of(&Lookup {
                    drive,
                    fcb: self.fcb,
                    name_field: field::NAME,
                }))
            }
        }
    }
}

/// Function `24h`: the FCB's random record set from its block and record.
pub fn set_random(k: &mut Kernel<'_, '_>, fcb: Far) {
    let record = k.sequential_record(fcb);
    k.set_random_record(fcb, record);
}

/// What function `29h` parsed: where the string goes on, and the result in
/// AL — `00h` no wildcard, `01h` one, `FFh` a drive that is none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parsed {
    /// Bytes of the string taken.
    pub taken: u16,
    /// AL.
    pub result: u8,
}

/// The parse control bits of function `29h`.
pub mod parse {
    /// Leading separators are skipped.
    pub const SKIP: u8 = 0x01;
    /// The FCB's drive stays when the string names none.
    pub const KEEP_DRIVE: u8 = 0x02;
    /// Its name stays when the string has none.
    pub const KEEP_NAME: u8 = 0x04;
    /// Its extension stays when the string has none.
    pub const KEEP_EXTENSION: u8 = 0x08;
}

const fn is_blank(byte: u8) -> bool {
    byte == b' ' || byte == b'\t'
}

const fn is_separator(byte: u8) -> bool {
    matches!(byte, b':' | b'.' | b';' | b',' | b'=' | b'+')
}

const fn ends_name(byte: u8) -> bool {
    byte <= b' '
        || matches!(
            byte,
            b'.' | b'"'
                | b'/'
                | b'\\'
                | b'['
                | b']'
                | b':'
                | b'|'
                | b'<'
                | b'>'
                | b'+'
                | b'='
                | b';'
                | b','
        )
}

/// Function `29h`: parses the file name `text` opens with into the FCB at
/// `fcb` under the control bits `control`.
pub fn parse_name(k: &mut Kernel<'_, '_>, text: Far, fcb: Far, control: u8) -> Parsed {
    let mut bytes = [0u8; 128];
    k.g.read_bytes(text.segment, text.offset, &mut bytes);
    let mut at = 0usize;
    while at < bytes.len() && is_blank(bytes[at]) {
        at += 1;
    }
    if control & parse::SKIP != 0 && at < bytes.len() && is_separator(bytes[at]) {
        at += 1;
        while at < bytes.len() && is_blank(bytes[at]) {
            at += 1;
        }
    }
    let mut result = 0u8;
    if at + 1 < bytes.len() && bytes[at + 1] == b':' && bytes[at].is_ascii_alphabetic() {
        let drive = upper(bytes[at]) - b'A';
        if drive >= k.lastdrive() || !k.is_drive(drive) {
            result = 0xFF;
        }
        k.set_fcb8(fcb, field::DRIVE, drive + 1);
        at += 2;
    } else if control & parse::KEEP_DRIVE == 0 {
        k.set_fcb8(fcb, field::DRIVE, 0);
    }
    let mut name = k.fcb_name(fcb, field::NAME);
    let (base, taken) = parse_part(&bytes[at..], 8);
    at += taken;
    if let Some(base) = base {
        name[..8].copy_from_slice(&base[..8]);
    } else if control & parse::KEEP_NAME == 0 {
        name[..8].fill(b' ');
    }
    let mut extension = None;
    if at < bytes.len() && bytes[at] == b'.' {
        at += 1;
        let (part, taken) = parse_part(&bytes[at..], 3);
        at += taken;
        extension = Some(part.unwrap_or([b' '; 8]));
    }
    match extension {
        Some(part) => name[8..].copy_from_slice(&part[..3]),
        None if control & parse::KEEP_EXTENSION == 0 => name[8..].fill(b' '),
        None => {}
    }
    let at_name = fcb.add(field::NAME);
    k.g.write_bytes(at_name.segment, at_name.offset, &name);
    if result == 0 && name.contains(&b'?') {
        result = 1;
    }
    Parsed {
        taken: u16::try_from(at).unwrap_or(0),
        result,
    }
}

/// A name part of at most `len` bytes from `bytes`, upper case and `*`
/// filled out with `?`, blank padded; `None` when the part is empty. The
/// bytes taken.
fn parse_part(bytes: &[u8], len: usize) -> (Option<[u8; 8]>, usize) {
    let mut out = [b' '; 8];
    let mut filled = 0;
    let mut taken = 0;
    while taken < bytes.len() && !ends_name(bytes[taken]) {
        let byte = bytes[taken];
        taken += 1;
        if filled >= len {
            continue;
        }
        if byte == b'*' {
            out[filled..len].fill(b'?');
            filled = len;
        } else {
            out[filled] = upper(byte);
            filled += 1;
        }
    }
    ((taken > 0).then_some(out), taken)
}
