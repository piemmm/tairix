//! The library's record: what an application's store keeps of every machine,
//! one record written to two blobs in turn, so a write cut short leaves the
//! other whole.
//!
//! The library holds each machine's number, minted once and never reused, and
//! its name; the application gives the rest of a machine ([`Kept`]), the
//! blobs a machine owns beside its state, and the bounds of its store.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Debug;

use tairix_abi::appdata_ipc::APPDATA_BLOB_MAX_COUNT;
use tairix_crt::{Tube, FULL};
use tairix_sandbox::wire::{Reader, WireError, Writer};

/// The blobs the record is written to in turn: generation `g` goes to the
/// `g % 2`th.
pub const RECORD_BLOBS: [&str; 2] = ["library-a", "library-b"];

/// The longest name a machine takes, in bytes: its window is titled with it.
pub const MAX_NAME: usize = tairix_abi::window_ipc::WINDOW_TITLE_MAX;

const CRC: usize = 4;

/// A machine as an application keeps it: its number and name, which the
/// library reads, and its own fields, which only the application knows.
pub trait Kept: Clone + Debug + Eq + Sized {
    /// Why the record may not hold a machine, beyond its name.
    type Invalid: Copy + Debug + Eq;
    /// The record's magic.
    const MAGIC: u32;
    /// The record's version.
    const VERSION: u16;
    /// The most machines the library holds: every one's blobs, and the
    /// record's two copies, fit the application's store ([`store_holds`]).
    const MAX_MACHINES: usize;
    /// The most bytes a record copy is read to.
    const MAX_RECORD: usize;
    /// The blobs a machine owns beside its state, named
    /// `<word>-<number>-<index>`: each word, and how many indices it takes.
    const PARTS: &'static [(&'static str, usize)];

    /// Its number.
    fn id(&self) -> u32;
    /// Its name.
    fn name(&self) -> &str;
    /// Write its own fields, which follow its number and name.
    fn put(&self, w: &mut Writer);
    /// The machine numbered `id` called `name` whose own fields `r` holds
    /// next; none for fields no machine holds.
    ///
    /// # Errors
    ///
    /// A [`WireError`] for a record cut short.
    fn get(id: u32, name: String, r: &mut Reader<'_>) -> Result<Option<Self>, WireError>;
    /// Why the record may not hold it, if it may not.
    fn invalid(&self) -> Option<Self::Invalid>;
    /// Whether it owns its blob of `word` and `index`, one of [`Self::PARTS`].
    fn holds(&self, word: &str, index: usize) -> bool;
}

/// Why a library change or record was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibraryError<I> {
    /// Not a library record, or one cut short or corrupted.
    Malformed,
    /// The library holds as many machines as it can.
    Full,
    /// A name that is empty, too long, or holds a control character.
    Name,
    /// No machine has that number, or one already does.
    NoMachine,
    /// A machine the application's own rules refuse.
    Invalid(I),
    /// The heap would not hold it.
    OutOfMemory,
}

/// Every machine the store keeps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Library<M> {
    /// The newest generation on the store.
    generation: u64,
    next_id: u32,
    machines: Vec<M>,
}

impl<M> Default for Library<M> {
    fn default() -> Self {
        Self::new()
    }
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
pub struct Loaded<M> {
    /// The machines.
    pub library: Library<M>,
    /// How the record was found.
    pub found: Found,
    /// Blobs belonging to no machine the record holds: what a deletion or a
    /// creation cut short left behind.
    pub orphans: Vec<String>,
}

/// The blob machine `id`'s state — its last screen and any snapshot — is
/// kept in.
#[must_use]
pub fn state_blob(id: u32) -> String {
    format!("state-{id}")
}

/// The blob of machine `id`'s own part `word`, its `index`th.
#[must_use]
pub fn part_blob(word: &str, id: u32, index: usize) -> String {
    format!("{word}-{id}-{index}")
}

/// The most machines an application's store holds beside the record's two
/// copies, each machine owning its state and the blobs `parts` names.
#[must_use]
pub const fn store_holds(parts: &[(&str, usize)]) -> usize {
    let mut per_machine = 1;
    let mut at = 0;
    while at < parts.len() {
        per_machine += parts[at].1;
        at += 1;
    }
    (APPDATA_BLOB_MAX_COUNT - RECORD_BLOBS.len()) / per_machine
}

const fn strengths(tube: Tube) -> [u16; 5] {
    [
        tube.scanlines,
        tube.glow,
        tube.fuzz,
        tube.phosphor,
        tube.wobble,
    ]
}

/// Write the strengths of `tube`, which a machine's picture is drawn through.
pub fn put_tube(w: &mut Writer, tube: Tube) {
    for strength in strengths(tube) {
        w.u16(strength);
    }
}

/// The tube whose strengths `r` holds next, as [`put_tube`] wrote them.
///
/// # Errors
///
/// A [`WireError`] for a record cut short.
pub fn get_tube(r: &mut Reader<'_>) -> Result<Tube, WireError> {
    Ok(Tube {
        scanlines: r.u16()?,
        glow: r.u16()?,
        fuzz: r.u16()?,
        phosphor: r.u16()?,
        wobble: r.u16()?,
    })
}

/// Whether no effect of `tube` is drawn stronger than full strength.
#[must_use]
pub fn tube_within(tube: Tube) -> bool {
    strengths(tube).iter().all(|&strength| strength <= FULL)
}

/// What of a machine a blob holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    State,
    Own(&'static str, usize),
}

/// The machine a blob called `name` belongs to and what of it it holds; none
/// for a name the library never gives.
fn owner<M: Kept>(name: &str) -> Option<(u32, Part)> {
    let (id, part) = if let Some(id) = name.strip_prefix("state-") {
        (id, Part::State)
    } else {
        M::PARTS.iter().find_map(|&(word, count)| {
            let rest = name.strip_prefix(word)?.strip_prefix('-')?;
            let (id, index) = rest.split_once('-')?;
            let index = index.parse::<usize>().ok().filter(|&index| index < count)?;
            Some((id, Part::Own(word, index)))
        })?
    };
    let id: u32 = id.parse().ok()?;
    let spelled = match part {
        Part::State => state_blob(id),
        Part::Own(word, index) => part_blob(word, id, index),
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

impl<M> Library<M> {
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
    pub fn machines(&self) -> &[M] {
        &self.machines
    }
}

impl<M: Kept> Library<M> {
    /// Machine `id`.
    #[must_use]
    pub fn machine(&self, id: u32) -> Option<&M> {
        self.machines.iter().find(|machine| machine.id() == id)
    }

    /// A number for a machine about to be made, never minted again whether it
    /// is added or not.
    ///
    /// # Errors
    ///
    /// [`LibraryError::Full`] with no room for another machine or no number
    /// left.
    pub fn mint(&mut self) -> Result<u32, LibraryError<M::Invalid>> {
        if self.machines.len() >= M::MAX_MACHINES || self.next_id == u32::MAX {
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
    /// [`LibraryError::Invalid`], or [`LibraryError::OutOfMemory`].
    pub fn add(&mut self, machine: M) -> Result<(), LibraryError<M::Invalid>> {
        if self.machines.len() >= M::MAX_MACHINES {
            return Err(LibraryError::Full);
        }
        let id = machine.id();
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
    /// [`LibraryError::Invalid`].
    pub fn replace(&mut self, machine: M) -> Result<(), LibraryError<M::Invalid>> {
        check(&machine)?;
        let held = self
            .machines
            .iter_mut()
            .find(|held| held.id() == machine.id())
            .ok_or(LibraryError::NoMachine)?;
        *held = machine;
        Ok(())
    }

    /// Take machine `id` out of the library; its number is never used again.
    pub fn remove(&mut self, id: u32) -> Option<M> {
        let at = self
            .machines
            .iter()
            .position(|machine| machine.id() == id)?;
        Some(self.machines.remove(at))
    }

    /// `base`, or `base` with the least number after it that no machine is
    /// called: what a new machine of that kind is named.
    #[must_use]
    pub fn fresh_name(&self, base: &str) -> String {
        let taken = |name: &str| self.machines.iter().any(|machine| machine.name() == name);
        let first = named(base, "");
        if !taken(&first) {
            return first;
        }
        (2..=M::MAX_MACHINES + 1)
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
    pub fn record(&self) -> Result<Record, LibraryError<M::Invalid>> {
        let generation = self.generation.checked_add(1).ok_or(LibraryError::Full)?;
        let mut w = Writer::try_with_capacity(512).ok_or(LibraryError::OutOfMemory)?;
        w.u32(M::MAGIC);
        w.u16(M::VERSION);
        w.u64(generation);
        w.u32(self.next_id);
        w.u16(u16::try_from(self.machines.len()).map_err(|_| LibraryError::Full)?);
        for machine in &self.machines {
            w.u32(machine.id());
            w.str(machine.name());
            machine.put(&mut w);
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
    pub fn decode(bytes: &[u8]) -> Result<Self, LibraryError<M::Invalid>> {
        if bytes.len() > M::MAX_RECORD {
            return Err(LibraryError::Malformed);
        }
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
        if r.u32()? != M::MAGIC || r.u16()? != M::VERSION {
            return Ok(None);
        }
        let generation = r.u64()?;
        let next_id = r.u32()?;
        let count = usize::from(r.u16()?);
        if count > M::MAX_MACHINES {
            return Ok(None);
        }
        let mut machines: Vec<M> = Vec::new();
        machines
            .try_reserve_exact(count)
            .map_err(|_| WireError::Malformed)?;
        for _ in 0..count {
            let id = r.u32()?;
            let name = String::from(r.str(MAX_NAME)?);
            let Some(machine) = M::get(id, name, r)? else {
                return Ok(None);
            };
            let unique = machines.iter().all(|held| held.id() != id);
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
    pub fn load(listing: &[(String, u64)], copies: [Option<&[u8]>; 2]) -> Loaded<M> {
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
            .filter_map(|(name, _)| Some((name, owner::<M>(name)?)))
        {
            if found == Found::Unreadable {
                library.next_id = library.next_id.max(id.saturating_add(1));
                continue;
            }
            let held = library.machine(id).is_some_and(|machine| match part {
                Part::State => true,
                Part::Own(word, index) => machine.holds(word, index),
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

/// The library's record kept on the store: one write at a time, a change made
/// while one is under way written once it lands, and the machines deleted
/// meanwhile removed only once a record without them is on the store, so a
/// write that fails never leaves a record naming blobs already gone.
#[derive(Debug, Default)]
pub struct Writes {
    /// The machines whose blobs wait on the write under way, while one is.
    under_way: Option<Vec<u32>>,
    /// Changed since the write under way was made.
    dirty: bool,
    /// The last write failed and nothing has been written since.
    failed: bool,
    /// Machines deleted whose blobs wait on the next write.
    doomed: Vec<u32>,
}

/// What became of a record write.
#[derive(Debug, PartialEq, Eq)]
pub struct Landed {
    /// The machines whose blobs may now be removed.
    pub removals: Vec<u32>,
    /// A failure the user has not been told of since the last success.
    pub newly_failed: bool,
    /// The library changed while it was written: write it again.
    pub again: bool,
}

impl Writes {
    /// Machine `id` was deleted: its blobs go once a record without it is
    /// kept.
    pub fn doom(&mut self, id: u32) {
        if self.doomed.try_reserve(1).is_ok() {
            self.doomed.push(id);
        }
    }

    /// Whether a write may begin now; one under way marks the library to be
    /// written again once it lands.
    pub fn begin(&mut self) -> bool {
        if self.under_way.is_some() {
            self.dirty = true;
            return false;
        }
        self.under_way = Some(core::mem::take(&mut self.doomed));
        true
    }

    /// The write under way landed with `ok`.
    pub fn landed(&mut self, ok: bool) -> Landed {
        let waited = self.under_way.take().unwrap_or_default();
        let was_failed = core::mem::replace(&mut self.failed, !ok);
        let newly_failed = !ok && !was_failed;
        let removals = if ok {
            waited
        } else {
            self.doomed.extend(waited);
            Vec::new()
        };
        Landed {
            removals,
            newly_failed,
            again: core::mem::take(&mut self.dirty),
        }
    }

    /// Whether the last write failed and none is under way: one to try
    /// again before the program ends.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.failed && self.under_way.is_none()
    }
}

/// Refuse a machine no record may hold.
fn check<M: Kept>(machine: &M) -> Result<(), LibraryError<M::Invalid>> {
    if !valid_name(machine.name()) {
        return Err(LibraryError::Name);
    }
    machine
        .invalid()
        .map_or(Ok(()), |why| Err(LibraryError::Invalid(why)))
}

#[cfg(test)]
#[path = "record_tests.rs"]
mod tests;
