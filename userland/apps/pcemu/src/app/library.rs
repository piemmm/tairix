//! The machine library: what the application's store keeps of every machine,
//! one record written to two blobs in turn, so a write cut short leaves the
//! other whole.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::appdata_ipc::APPDATA_BLOB_MAX_COUNT;
use tairix_abi::time::CivilTime;
use tairix_crt::{Tube, FULL};
use tairix_sandbox::wire::{Reader, WireError, Writer};

use crate::board::BoardConfig;
use crate::dos::setup::sizes;
use crate::link::config;
use crate::storage::hdc::{DiskType, LowLevel};

/// The blobs the record is written to in turn: generation `g` goes to the
/// `g % 2`th.
pub const RECORD_BLOBS: [&str; 2] = ["library-a", "library-b"];

/// The blobs a machine may own: its two fixed disks and its state.
const BLOBS_PER_MACHINE: usize = 3;

/// The most machines the library holds: its two copies, and every machine's
/// blobs, fit the application's store.
pub const MAX_MACHINES: usize = (APPDATA_BLOB_MAX_COUNT - RECORD_BLOBS.len()) / BLOBS_PER_MACHINE;

/// The longest name a machine takes, in bytes: its window is titled with it.
pub const MAX_NAME: usize = tairix_abi::window_ipc::WINDOW_TITLE_MAX;

/// The most bytes a record copy is read to: twenty machines of the longest
/// names and every part fitted come to a few kilobytes.
pub const MAX_RECORD: usize = 16 * 1024;

const MAGIC: u32 = u32::from_le_bytes(*b"PCLB");
const VERSION: u16 = 2;
const CRC: usize = 4;

/// A fixed disk a machine owns; its image is the blob [`disk_blob`] names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredDisk {
    /// The drive.
    pub kind: DiskType,
    /// Its low-level format, which the image does not record.
    pub format: LowLevel,
}

/// One machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Machine {
    /// Minted once, never reused.
    pub id: u32,
    /// What the user calls it.
    pub name: String,
    /// Its hardware.
    pub config: BoardConfig,
    /// Its fixed disks, which only a machine with a controller has.
    pub disks: [Option<StoredDisk>; 2],
    /// Seconds its clock card runs ahead of the host's clock; behind when
    /// negative.
    pub clock_offset: i64,
    /// The tube its monitor's picture is drawn through.
    pub display: Tube,
}

impl Machine {
    /// The sectors of fixed disk `drive`'s image: what its drive holds as the
    /// controller records it.
    #[must_use]
    pub fn disk_sectors(&self, drive: usize) -> Option<u64> {
        let disk = self.disks.get(drive)?.as_ref()?;
        let recording = self.config.fixed?.recording;
        Some(u64::from(sizes(disk.kind, recording).0))
    }

    /// What the clock card reads as the machine starts at host time
    /// `host_secs`.
    #[must_use]
    pub fn clock_at(&self, host_secs: i64) -> CivilTime {
        CivilTime::from_unix_secs(host_secs.saturating_add(self.clock_offset))
    }
}

/// The offset that has a clock card read `guest` at host time `host_secs`;
/// none for a reading no instant has.
#[must_use]
pub fn clock_offset(guest: &CivilTime, host_secs: i64) -> Option<i64> {
    guest.to_unix_secs()?.checked_sub(host_secs)
}

/// Every machine the store keeps.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Library {
    /// The newest generation on the store.
    generation: u64,
    next_id: u32,
    machines: Vec<Machine>,
}

/// Why a library change or record was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibraryError {
    /// Not a library record, or one cut short or corrupted.
    Malformed,
    /// The library holds as many machines as it can.
    Full,
    /// A name that is empty, too long, or holds a control character.
    Name,
    /// No machine has that number, or one already does.
    NoMachine,
    /// Fixed disks on a machine with no controller to drive them.
    NoController,
    /// A display effect stronger than full strength.
    Display,
    /// The heap would not hold it.
    OutOfMemory,
}

/// One record of the library, and where it goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// The blob not holding the newest copy.
    pub blob: &'static str,
    /// What it holds.
    pub bytes: Vec<u8>,
    /// Its generation, which [`Library::written`] is told once it is on the
    /// store.
    pub generation: u64,
}

/// How [`Library::load`] found the store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Found {
    /// No record was ever written: the first run.
    Fresh,
    /// The newest copy that checks out.
    Read,
    /// Records are held and none checks out: the library reads as empty, said
    /// as such. This load sweeps nothing and no number a blob carries is
    /// minted again, so nothing is overwritten on a guess; what the lost
    /// record held is swept only once a record is written again.
    Unreadable,
}

/// The store as [`Library::load`] makes it out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loaded {
    /// The machines.
    pub library: Library,
    /// How the record was found.
    pub found: Found,
    /// Blobs belonging to no machine the record holds: what a deletion or a
    /// creation cut short left behind.
    pub orphans: Vec<String>,
}

/// The blob machine `id`'s fixed disk `drive` is kept in.
#[must_use]
pub fn disk_blob(id: u32, drive: usize) -> String {
    format!("disk-{id}-{drive}")
}

/// The blob machine `id`'s last screen is kept in.
#[must_use]
pub fn state_blob(id: u32) -> String {
    format!("state-{id}")
}

/// What a machine's blob holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Disk(usize),
    State,
}

/// The machine a blob called `name` belongs to and what of it it holds; none
/// for a name the library never gives.
fn owner(name: &str) -> Option<(u32, Part)> {
    let (id, part) = if let Some(rest) = name.strip_prefix("disk-") {
        let (id, drive) = rest.split_once('-')?;
        let drive = match drive {
            "0" => 0,
            "1" => 1,
            _ => return None,
        };
        (id, Part::Disk(drive))
    } else {
        (name.strip_prefix("state-")?, Part::State)
    };
    let id: u32 = id.parse().ok()?;
    let spelled = match part {
        Part::Disk(drive) => disk_blob(id, drive),
        Part::State => state_blob(id),
    };
    (spelled == name).then_some((id, part))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_NAME && !name.chars().any(char::is_control)
}

/// `base`, made to fit a name with `suffix` after it.
fn named(base: &str, suffix: &str) -> String {
    let keep = base.floor_char_boundary(MAX_NAME.saturating_sub(suffix.len()));
    format!("{}{suffix}", &base[..keep])
}

/// The slot generation `generation` is written to.
fn slot_of(generation: u64) -> usize {
    usize::from(generation % 2 == 1)
}

impl Library {
    /// A library of no machines.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            generation: 0,
            next_id: 1,
            machines: Vec::new(),
        }
    }

    /// The machines, in the order they were made.
    #[must_use]
    pub fn machines(&self) -> &[Machine] {
        &self.machines
    }

    /// Machine `id`.
    #[must_use]
    pub fn machine(&self, id: u32) -> Option<&Machine> {
        self.machines.iter().find(|machine| machine.id == id)
    }

    /// A number for a machine about to be made, never minted again whether it
    /// is added or not.
    ///
    /// # Errors
    ///
    /// [`LibraryError::Full`] with no room for another machine or no number
    /// left.
    pub fn mint(&mut self) -> Result<u32, LibraryError> {
        if self.machines.len() >= MAX_MACHINES || self.next_id == u32::MAX {
            return Err(LibraryError::Full);
        }
        let id = self.next_id;
        self.next_id += 1;
        Ok(id)
    }

    /// Add `machine`, its number minted by [`Self::mint`].
    ///
    /// # Errors
    ///
    /// [`LibraryError::Full`], [`LibraryError::Name`],
    /// [`LibraryError::NoMachine`] for a number not minted or already held,
    /// [`LibraryError::NoController`], or [`LibraryError::OutOfMemory`].
    pub fn add(&mut self, machine: Machine) -> Result<(), LibraryError> {
        if self.machines.len() >= MAX_MACHINES {
            return Err(LibraryError::Full);
        }
        let id = machine.id;
        if id == 0 || id >= self.next_id || self.machine(id).is_some() {
            return Err(LibraryError::NoMachine);
        }
        check(&machine)?;
        self.machines
            .try_reserve(1)
            .map_err(|_| LibraryError::OutOfMemory)?;
        self.machines.push(machine);
        Ok(())
    }

    /// Put `machine` in place of the machine with its number.
    ///
    /// # Errors
    ///
    /// [`LibraryError::NoMachine`], [`LibraryError::Name`] or
    /// [`LibraryError::NoController`].
    pub fn replace(&mut self, machine: Machine) -> Result<(), LibraryError> {
        check(&machine)?;
        let held = self
            .machines
            .iter_mut()
            .find(|held| held.id == machine.id)
            .ok_or(LibraryError::NoMachine)?;
        *held = machine;
        Ok(())
    }

    /// Take machine `id` out of the library; its number is never used again.
    pub fn remove(&mut self, id: u32) -> Option<Machine> {
        let at = self.machines.iter().position(|machine| machine.id == id)?;
        Some(self.machines.remove(at))
    }

    /// `base`, or `base` with the least number after it that no machine is
    /// called: what a new machine of that kind is named.
    #[must_use]
    pub fn fresh_name(&self, base: &str) -> String {
        let taken = |name: &str| self.machines.iter().any(|machine| machine.name == name);
        let first = named(base, "");
        if !taken(&first) {
            return first;
        }
        (2..=MAX_MACHINES + 1)
            .map(|n| named(base, &format!(" {n}")))
            .find(|name| !taken(name))
            .unwrap_or(first)
    }

    /// The name a copy of machine `name` is given.
    #[must_use]
    pub fn copy_name(&self, name: &str) -> String {
        self.fresh_name(&named(name, " copy"))
    }

    /// The record of the library as it stands, for the blob not holding the
    /// newest copy on the store.
    ///
    /// # Errors
    ///
    /// [`LibraryError::OutOfMemory`], or [`LibraryError::Full`] once the
    /// generations run out.
    pub fn record(&self) -> Result<Record, LibraryError> {
        let generation = self.generation.checked_add(1).ok_or(LibraryError::Full)?;
        let mut w = Writer::try_with_capacity(512).ok_or(LibraryError::OutOfMemory)?;
        w.u32(MAGIC);
        w.u16(VERSION);
        w.u64(generation);
        w.u32(self.next_id);
        w.u16(u16::try_from(self.machines.len()).map_err(|_| LibraryError::Full)?);
        for machine in &self.machines {
            w.u32(machine.id);
            w.str(&machine.name);
            config::put_config(&mut w, &machine.config);
            for disk in &machine.disks {
                match disk {
                    Some(disk) => {
                        w.u8(1);
                        config::put_fixed(&mut w, &disk.kind, &disk.format);
                    }
                    None => w.u8(0),
                }
            }
            w.u64(u64::from_le_bytes(machine.clock_offset.to_le_bytes()));
            let tube = machine.display;
            for strength in [
                tube.scanlines,
                tube.glow,
                tube.fuzz,
                tube.phosphor,
                tube.wobble,
            ] {
                w.u16(strength);
            }
        }
        let mut bytes = w.finish();
        let sum = tairix_crc32::checksum(&bytes);
        bytes
            .try_reserve_exact(CRC)
            .map_err(|_| LibraryError::OutOfMemory)?;
        bytes.extend_from_slice(&sum.to_le_bytes());
        Ok(Record {
            blob: RECORD_BLOBS[slot_of(generation)],
            bytes,
            generation,
        })
    }

    /// A record of `generation` is on the store: the next goes to the other
    /// blob. A record that failed is never said written, so the next is
    /// written over it rather than over the copy still whole.
    pub fn written(&mut self, generation: u64) {
        self.generation = self.generation.max(generation);
    }

    /// The library one record holds.
    ///
    /// # Errors
    ///
    /// [`LibraryError::Malformed`] for anything but a whole record that checks
    /// out, every field in range.
    pub fn decode(bytes: &[u8]) -> Result<Self, LibraryError> {
        let body = bytes
            .len()
            .checked_sub(CRC)
            .map(|at| bytes.split_at(at))
            .filter(|(body, sum)| {
                <[u8; CRC]>::try_from(*sum)
                    .is_ok_and(|sum| u32::from_le_bytes(sum) == tairix_crc32::checksum(body))
            })
            .map(|(body, _)| body)
            .ok_or(LibraryError::Malformed)?;
        let mut r = Reader::new(body);
        let decoded = Self::read(&mut r).map_err(|_| LibraryError::Malformed)?;
        if !r.is_exhausted() {
            return Err(LibraryError::Malformed);
        }
        decoded.ok_or(LibraryError::Malformed)
    }

    fn read(r: &mut Reader<'_>) -> Result<Option<Self>, WireError> {
        if r.u32()? != MAGIC || r.u16()? != VERSION {
            return Ok(None);
        }
        let generation = r.u64()?;
        let next_id = r.u32()?;
        let count = usize::from(r.u16()?);
        if count > MAX_MACHINES {
            return Ok(None);
        }
        let mut machines = Vec::new();
        machines
            .try_reserve_exact(count)
            .map_err(|_| WireError::Malformed)?;
        for _ in 0..count {
            let id = r.u32()?;
            let name = r.str(MAX_NAME)?;
            let config = config::config(r)?;
            let mut disks = [None, None];
            for disk in &mut disks {
                *disk = match r.u8()? {
                    0 => None,
                    1 => {
                        let (kind, format) = config::fixed(r)?;
                        Some(StoredDisk { kind, format })
                    }
                    _ => return Ok(None),
                };
            }
            let clock_offset = i64::from_le_bytes(r.u64()?.to_le_bytes());
            let display = Tube {
                scanlines: r.u16()?,
                glow: r.u16()?,
                fuzz: r.u16()?,
                phosphor: r.u16()?,
                wobble: r.u16()?,
            };
            let machine = Machine {
                id,
                name: String::from(name),
                config,
                disks,
                clock_offset,
                display,
            };
            let unique = machines.iter().all(|held: &Machine| held.id != id);
            if id == 0 || id >= next_id || !unique || check(&machine).is_err() {
                return Ok(None);
            }
            machines.push(machine);
        }
        Ok(Some(Self {
            generation,
            next_id,
            machines,
        }))
    }

    /// The newest of the two copies, as read from [`RECORD_BLOBS`] in order,
    /// that checks out and was written to the blob it was read from; an empty
    /// library with neither, and whether either did.
    #[must_use]
    pub fn newest(copies: [Option<&[u8]>; 2]) -> (Self, bool) {
        let mut slot = 0;
        let [a, b] = copies.map(|copy| {
            let held = copy
                .and_then(|bytes| Self::decode(bytes).ok())
                .filter(|library| slot_of(library.generation) == slot);
            slot += 1;
            held
        });
        match (a, b) {
            (Some(a), Some(b)) => (if b.generation > a.generation { b } else { a }, true),
            (Some(one), None) | (None, Some(one)) => (one, true),
            (None, None) => (Self::new(), false),
        }
    }

    /// The library the store holds: `listing` names every blob in it, and
    /// `copies` are the two records as read.
    #[must_use]
    pub fn load(listing: &[(String, u64)], copies: [Option<&[u8]>; 2]) -> Loaded {
        let (mut library, read) = Self::newest(copies);
        let recorded = listing
            .iter()
            .any(|(name, _)| RECORD_BLOBS.contains(&name.as_str()));
        let found = match (read, recorded) {
            (true, _) => Found::Read,
            (false, false) => Found::Fresh,
            (false, true) => Found::Unreadable,
        };
        let mut orphans = Vec::new();
        for (name, (id, part)) in listing
            .iter()
            .filter_map(|(name, _)| Some((name, owner(name)?)))
        {
            if found == Found::Unreadable {
                library.next_id = library.next_id.max(id.saturating_add(1));
                continue;
            }
            let held = library.machine(id).is_some_and(|machine| match part {
                Part::Disk(drive) => machine.disks[drive].is_some(),
                Part::State => true,
            });
            if !held {
                orphans.push(name.clone());
            }
        }
        Loaded {
            library,
            found,
            orphans,
        }
    }
}

/// Refuse a machine no record may hold.
fn check(machine: &Machine) -> Result<(), LibraryError> {
    if !valid_name(&machine.name) {
        return Err(LibraryError::Name);
    }
    if machine.config.fixed.is_none() && machine.disks.iter().any(Option::is_some) {
        return Err(LibraryError::NoController);
    }
    let tube = machine.display;
    if [
        tube.scanlines,
        tube.glow,
        tube.fuzz,
        tube.phosphor,
        tube.wobble,
    ]
    .iter()
    .any(|&strength| strength > FULL)
    {
        return Err(LibraryError::Display);
    }
    Ok(())
}

#[cfg(test)]
#[path = "library_tests.rs"]
mod tests;
