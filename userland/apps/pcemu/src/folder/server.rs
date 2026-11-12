//! The front-end's side of a share: the folder the user granted, served as
//! DOS asks of it. Each directory reached has a listing of its own that names
//! every entry in 8.3 form; a name keeps its alias, and an entry its place,
//! for as long as the listing is kept, so a search under way never loses its
//! place to a change beside it. The files the machine opens are held here,
//! behind handles that carry the share's epoch.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::time::Time64;
use tairix_abi::{Errno, FileId, FileKind, FileStat, StampChange, TimesChange};

use super::alias::{self, TRIES};
use super::host::{HostDir, HostFile};
use super::{
    wanted, Access, Creation, DosName, Facts, Folder, From, Op, Refusal, Reply, SharePath, IO_MOST,
};

/// Directories a share keeps a listing of at once, its root among them; each
/// holds its directory open.
pub const LISTINGS: usize = 64;

/// Files the machine may hold open through one share: what DOS's `FILES`
/// reaches.
pub const FILES: usize = 255;

/// Entries one listing names at most, those gone included: no FAT directory
/// holds more.
pub const ENTRIES: usize = 1 << 16;

/// Places a search counts before a directory's entries: its `.` and `..`.
const DOTS: usize = 2;

/// What keys a listing: the directory's identity, or where its volume gives
/// none, the listing it was reached from and its name there.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Key {
    Id(FileId),
    Named { parent: u32, host: String },
}

/// One entry of a listing.
struct Slot {
    alias: DosName,
    /// The host's name, `None` once the entry is gone: it keeps its place.
    host: Option<String>,
    id: FileId,
}

/// What a directory's listing was read at: a change since moves one of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Read {
    modified: Time64,
    changed: Time64,
    content_gen: u64,
}

impl Read {
    const fn of(stat: &FileStat) -> Self {
        Self {
            modified: stat.times.modified,
            changed: stat.times.changed,
            content_gen: stat.content_gen,
        }
    }
}

/// One directory's names as DOS reaches them.
struct Listing<D> {
    /// Its number, never reused, which a search goes on by.
    number: u32,
    key: Key,
    dir: D,
    slots: Vec<Slot>,
    by_alias: BTreeMap<DosName, u32>,
    by_host: BTreeMap<String, u32>,
    /// The next number tried for an alias of each stem and ending, so a run
    /// of names sharing them is not tried from the first each time.
    next_alias: BTreeMap<[u8; 11], u32>,
    read: Option<Read>,
    /// When it was last used, for choosing which to let go.
    used: u64,
}

impl<D: HostDir> Listing<D> {
    fn new(number: u32, key: Key, dir: D) -> Self {
        Self {
            number,
            key,
            dir,
            slots: Vec::new(),
            by_alias: BTreeMap::new(),
            by_host: BTreeMap::new(),
            next_alias: BTreeMap::new(),
            read: None,
            used: 0,
        }
    }

    /// Read the directory again if it changed since it was last read: names
    /// gone keep their places nameless, and new ones are named in byte order,
    /// so the same entries are given the same names.
    fn refresh(&mut self) -> Result<(), Refusal> {
        let read = Read::of(&self.dir.stat().map_err(refusal)?);
        if self.read == Some(read) {
            return Ok(());
        }
        let listed = self.dir.list().map_err(refusal)?;
        let present: BTreeSet<&str> = listed.iter().map(|entry| entry.name.as_str()).collect();
        for slot in &mut self.slots {
            if let Some(host) = slot.host.take_if(|host| !present.contains(host.as_str())) {
                self.by_host.remove(&host);
                self.by_alias.remove(&slot.alias);
            }
        }
        let mut fresh: Vec<_> = listed
            .iter()
            .filter(|entry| match self.by_host.get(&entry.name) {
                Some(&at) => {
                    self.slots[at as usize].id = entry.id;
                    false
                }
                None => true,
            })
            .collect();
        fresh.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        for entry in fresh {
            if self.slots.len() == ENTRIES {
                break;
            }
            if let Some(alias) = self.free_alias(&entry.name) {
                self.insert(alias, entry.name.clone(), entry.id);
            }
        }
        self.read = Some(read);
        Ok(())
    }

    /// A name for `host` nothing in the listing holds: its own where it has
    /// one, else its first free alias.
    fn free_alias(&mut self, host: &str) -> Option<DosName> {
        if let Some(own) = alias::exact(host).filter(|name| !self.by_alias.contains_key(name)) {
            return Some(own);
        }
        let stem = *alias::alias(host, 1)?.bytes();
        let first = self.next_alias.get(&stem).copied().unwrap_or(1);
        let found = (first..=TRIES)
            .filter_map(|n| alias::alias(host, n).map(|name| (n, name)))
            .find(|(_, name)| !self.by_alias.contains_key(name))?;
        self.next_alias.insert(stem, found.0 + 1);
        Some(found.1)
    }

    fn insert(&mut self, alias: DosName, host: String, id: FileId) {
        let at = u32::try_from(self.slots.len()).unwrap_or(u32::MAX);
        self.by_alias.insert(alias, at);
        self.by_host.insert(host.clone(), at);
        self.slots.push(Slot {
            alias,
            host: Some(host),
            id,
        });
    }

    /// The entry DOS names `name`, read again where the directory changed
    /// and it names none.
    fn find(&mut self, name: DosName) -> Result<Option<(String, FileId)>, Refusal> {
        if !self.by_alias.contains_key(&name) {
            self.refresh()?;
        }
        Ok(self.by_alias.get(&name).and_then(|&at| {
            let slot = &self.slots[at as usize];
            slot.host.clone().map(|host| (host, slot.id))
        }))
    }

    /// The entry named `name` is gone.
    fn forget(&mut self, name: DosName) {
        if let Some(at) = self.by_alias.remove(&name) {
            if let Some(host) = self.slots[at as usize].host.take() {
                self.by_host.remove(&host);
            }
        }
    }

    /// The entry named `from` is named `to` now, `host` on the host, in the
    /// place it had: as a FAT directory renames an entry where it lies, so a
    /// search renaming what it finds does not come on it again.
    fn rename(&mut self, from: DosName, to: DosName, host: String, id: FileId) {
        let Some(at) = self.by_alias.remove(&from) else {
            self.insert(to, host, id);
            return;
        };
        let slot = &mut self.slots[at as usize];
        if let Some(old) = slot.host.replace(host.clone()) {
            self.by_host.remove(&old);
        }
        slot.alias = to;
        slot.id = id;
        self.by_alias.insert(to, at);
        self.by_host.insert(host, at);
    }
}

/// A file the machine holds open.
struct Open<F> {
    file: F,
    access: Access,
}

/// A folder served to one machine.
pub struct Server<D: HostDir> {
    /// Kept in place, a let-go one `None`, so an index stays the listing it
    /// was; the root's is the first and is never let go.
    listings: Vec<Option<Listing<D>>>,
    next_listing: u32,
    files: Vec<Option<Open<D::File>>>,
    epoch: u32,
    writable: bool,
    label: DosName,
    /// Seconds the machine's clock runs ahead of the host's.
    offset: i64,
    tick: u64,
    read: Vec<u8>,
}

impl<D: HostDir> Server<D> {
    /// The server of the folder `root`, labelled `label`, writable where the
    /// grant is; its handles carry the low 24 bits of `epoch`, drawn afresh
    /// for each share so a handle of an earlier one reaches none of this
    /// one's files.
    #[must_use]
    pub fn new(root: D, label: DosName, writable: bool, epoch: u32) -> Self {
        let id = root.stat().map_or(FileId::NONE, |stat| stat.id);
        let listings = alloc::vec![Some(Listing::new(0, key_of(id, 0, ""), root))];
        Self {
            listings,
            next_listing: 1,
            files: Vec::new(),
            epoch: epoch << 8,
            writable,
            label,
            offset: 0,
            tick: 0,
            read: Vec::new(),
        }
    }

    /// The machine's clock now runs `offset` seconds ahead of the host's: the
    /// times the share answers, and those DOS sets, are moved by it.
    pub fn set_clock_offset(&mut self, offset: i64) {
        self.offset = offset;
    }

    /// The listing kept at `at`. Every index a caller holds is one a walk
    /// just reached, which no later keep lets go before it, so a missing one
    /// is a fault, refused rather than trusted.
    fn listing(&self, at: usize) -> Result<&Listing<D>, Refusal> {
        self.listings
            .get(at)
            .and_then(Option::as_ref)
            .ok_or(Refusal::Fault)
    }

    /// As [`Self::listing`], marked used now.
    fn listing_mut(&mut self, at: usize) -> Result<&mut Listing<D>, Refusal> {
        self.tick += 1;
        let tick = self.tick;
        let listing = self
            .listings
            .get_mut(at)
            .and_then(Option::as_mut)
            .ok_or(Refusal::Fault)?;
        listing.used = tick;
        Ok(listing)
    }

    /// The listing of the directory `path` names.
    fn walk(&mut self, path: &SharePath) -> Result<usize, Refusal> {
        let mut at = 0;
        for name in path.names() {
            at = self.enter(at, name)?;
        }
        self.listing_mut(at)?;
        Ok(at)
    }

    /// The listing of the directory `name` names in listing `at`.
    fn enter(&mut self, at: usize, name: DosName) -> Result<usize, Refusal> {
        let parent = self.listing_mut(at)?;
        let (host, id) = parent.find(name)?.ok_or(Refusal::PathNotFound)?;
        let number = parent.number;
        if let Some(kept) = self.kept(&key_of(id, number, &host)) {
            return Ok(kept);
        }
        let dir = self
            .listing(at)?
            .dir
            .enter(&host)
            .map_err(|errno| path_refusal(refusal(errno)))?;
        // A link reaches a directory under the identity of the one it names.
        let key = key_of(dir.stat().map_err(refusal)?.id, number, &host);
        if let Some(kept) = self.kept(&key) {
            return Ok(kept);
        }
        Ok(self.keep(key, dir))
    }

    fn kept(&self, key: &Key) -> Option<usize> {
        self.listings
            .iter()
            .position(|listing| listing.as_ref().is_some_and(|listing| &listing.key == key))
    }

    /// Keep a listing of `dir`, letting the least lately used go where
    /// [`LISTINGS`] are kept.
    fn keep(&mut self, key: Key, dir: D) -> usize {
        let number = self.next_listing;
        self.next_listing = self.next_listing.wrapping_add(1).max(1);
        let listing = Some(Listing::new(number, key, dir));
        let free = self.listings.iter().position(Option::is_none);
        let at = match free {
            Some(at) => at,
            None if self.listings.len() < LISTINGS => {
                self.listings.push(None);
                self.listings.len() - 1
            }
            None => self
                .listings
                .iter()
                .enumerate()
                .skip(1)
                .min_by_key(|(_, listing)| listing.as_ref().map_or(0, |listing| listing.used))
                .map_or(1, |(at, _)| at),
        };
        self.listings[at] = listing;
        self.tick += 1;
        if let Some(listing) = self.listings[at].as_mut() {
            listing.used = self.tick;
        }
        at
    }

    /// The listing of `dir` and the entry `name` names in it, if it names one.
    fn entry(
        &mut self,
        dir: &SharePath,
        name: DosName,
    ) -> Result<(usize, Option<(String, FileId)>), Refusal> {
        let at = self.walk(dir)?;
        let found = self.listing_mut(at)?.find(name)?;
        Ok((at, found))
    }

    /// What `stat`, of the entry `host` named `name` in listing `at`, is to
    /// DOS; `None` for what it cannot hold — a file past 4 GiB, a node
    /// neither file nor directory.
    fn facts(
        &self,
        listing: &Listing<D>,
        name: DosName,
        host: &str,
        stat: &FileStat,
    ) -> Option<Facts> {
        let directory = match stat.kind {
            FileKind::Directory => true,
            FileKind::Regular => false,
            FileKind::Symlink => return None,
        };
        Some(Facts {
            name,
            directory,
            size: if directory {
                0
            } else {
                u32::try_from(stat.size).ok()?
            },
            modified: shifted(stat.times.modified, self.offset),
            writable: self.writable && listing.dir.may_write(Some(host)),
            hidden: host.starts_with('.'),
        })
    }

    fn look(&mut self, path: &SharePath) -> Result<Facts, Refusal> {
        let Some((dir, name)) = path.split_last() else {
            let root = self.listing(0)?;
            let stat = root.dir.stat().map_err(refusal)?;
            return Ok(Facts {
                name: self.label,
                directory: true,
                size: 0,
                modified: shifted(stat.times.modified, self.offset),
                writable: self.writable && root.dir.may_write(None),
                hidden: false,
            });
        };
        let (at, found) = self.entry(&dir, name)?;
        let (host, _) = found.ok_or(Refusal::NotFound)?;
        let listing = self.listing(at)?;
        let stat = listing.dir.look(&host).map_err(refusal)?;
        self.facts(listing, name, &host, &stat)
            .ok_or(Refusal::Denied)
    }

    fn find(&mut self, pattern: DosName, attributes: u8, from: From) -> Result<Reply<'_>, Refusal> {
        let (at, first) = match from {
            From::Start { dir } => {
                let at = self.walk(&dir)?;
                self.listing_mut(at)?.refresh()?;
                (at, 0)
            }
            From::Next { table, index } => {
                let at = self
                    .listings
                    .iter()
                    .position(|listing| {
                        listing
                            .as_ref()
                            .is_some_and(|listing| listing.number == table)
                    })
                    .ok_or(Refusal::NoMore)?;
                (at, usize::try_from(index).map_err(|_| Refusal::NoMore)?)
            }
        };
        self.listing_mut(at)?;
        let listing = self.listing(at)?;
        let number = listing.number;
        let next = |index: usize| u32::try_from(index).map_err(|_| Refusal::NoMore);
        // A directory below the root lists itself and its parent first,
        // where a search wants directories, as DOS's own do.
        let dots = if at != 0 && attributes & wanted::DIRECTORY != 0 {
            DOTS
        } else {
            0
        };
        let mut index = first;
        while index < dots {
            let name = [DosName::DOT, DosName::DOT_DOT][index];
            index += 1;
            if pattern.matches(&name) {
                let stat = listing.dir.stat().map_err(refusal)?;
                let facts = Facts {
                    name,
                    directory: true,
                    size: 0,
                    modified: shifted(stat.times.modified, self.offset),
                    writable: self.writable && listing.dir.may_write(None),
                    hidden: false,
                };
                return Ok(Reply::Found {
                    facts,
                    table: number,
                    next: next(index)?,
                });
            }
        }
        while let Some(slot) = listing.slots.get(index - dots) {
            index += 1;
            let Some(host) = slot.host.as_deref() else {
                continue;
            };
            if !pattern.matches(&slot.alias) {
                continue;
            }
            // An entry gone since the listing was read, or one DOS cannot
            // hold, is passed over.
            let Ok(stat) = listing.dir.look(host) else {
                continue;
            };
            let Some(facts) = self.facts(listing, slot.alias, host, &stat) else {
                continue;
            };
            let admitted = (!facts.directory || attributes & wanted::DIRECTORY != 0)
                && (!facts.hidden || attributes & wanted::HIDDEN != 0);
            if admitted {
                return Ok(Reply::Found {
                    facts,
                    table: number,
                    next: next(index)?,
                });
            }
        }
        Err(Refusal::NoMore)
    }

    fn open(
        &mut self,
        path: &SharePath,
        access: Access,
        creation: Creation,
    ) -> Result<Reply<'_>, Refusal> {
        let (dir, name) = path.split_last().ok_or(Refusal::Denied)?;
        if (access.writes() || creation != Creation::Existing) && !self.writable {
            return Err(Refusal::Denied);
        }
        let slot = self
            .files
            .iter()
            .position(Option::is_none)
            .or_else(|| (self.files.len() < FILES).then_some(self.files.len()))
            .ok_or(Refusal::TooMany)?;
        let (at, found) = self.entry(&dir, name)?;
        let made = found.is_none();
        let (host, how) = match (found, creation) {
            (Some(_), Creation::New) => return Err(Refusal::Exists),
            (Some((host, _)), how) => (host, how),
            (None, Creation::Existing) => return Err(Refusal::NotFound),
            (None, how) => (alias::host_name(&name), how),
        };
        let listing = self.listing(at)?;
        let file = listing.dir.open(&host, access, how).map_err(refusal)?;
        let stat = file.stat().map_err(refusal)?;
        let facts = self
            .facts(listing, name, &host, &stat)
            .filter(|facts| !facts.directory)
            .ok_or(Refusal::Denied)?;
        if made {
            self.listing_mut(at)?.insert(name, host, stat.id);
        }
        let open = Open { file, access };
        if slot == self.files.len() {
            self.files.push(Some(open));
        } else {
            self.files[slot] = Some(open);
        }
        Ok(Reply::Opened {
            handle: self.epoch | u32::try_from(slot + 1).map_err(|_| Refusal::TooMany)?,
            file: node_of(stat.id),
            facts,
        })
    }

    /// Where in the table of open files `handle` points, if it is one of
    /// this share's.
    fn slot_of(&self, handle: u32) -> Result<usize, Refusal> {
        if handle & !0xFF != self.epoch {
            return Err(Refusal::Handle);
        }
        usize::try_from(handle & 0xFF)
            .ok()
            .and_then(|slot| slot.checked_sub(1))
            .ok_or(Refusal::Handle)
    }

    /// The file `handle` names.
    fn held(&mut self, handle: u32) -> Result<&mut Open<D::File>, Refusal> {
        let slot = self.slot_of(handle)?;
        self.files
            .get_mut(slot)
            .and_then(Option::as_mut)
            .ok_or(Refusal::Handle)
    }

    fn read_at(&mut self, handle: u32, offset: u32, len: u32) -> Result<Reply<'_>, Refusal> {
        let len = usize::try_from(len)
            .map_err(|_| Refusal::Fault)?
            .min(IO_MOST);
        let mut buffer = core::mem::take(&mut self.read);
        if buffer.len() < len {
            buffer
                .try_reserve_exact(len - buffer.len())
                .map_err(|_| Refusal::Fault)?;
            buffer.resize(len, 0);
        }
        let open = self.held(handle).and_then(|open| match open.access {
            Access::Write => Err(Refusal::Denied),
            Access::Read | Access::Both => Ok(open),
        });
        let read = open.and_then(|open| {
            let mut got = 0;
            while got < len {
                let at = u64::from(offset) + got as u64;
                match open
                    .file
                    .read_at(at, &mut buffer[got..len])
                    .map_err(refusal)?
                {
                    0 => break,
                    n => got += n,
                }
            }
            Ok(got)
        });
        self.read = buffer;
        let got = read?;
        Ok(Reply::Read(&self.read[..got]))
    }

    fn write_at(&mut self, handle: u32, offset: u32, bytes: &[u8]) -> Result<Reply<'_>, Refusal> {
        let open = self.held(handle)?;
        if !open.access.writes() {
            return Err(Refusal::Denied);
        }
        // DOS reaches no byte past 4 GiB, so what would land there is not
        // written, as a full volume's is not.
        let room = usize::try_from(u64::from(u32::MAX) - u64::from(offset))
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let written = match open.file.write_at(u64::from(offset), &bytes[..room]) {
            Ok(written) => written,
            Err(Errno::NoSpace) => 0,
            Err(errno) => return Err(refusal(errno)),
        };
        Ok(Reply::Wrote(
            u32::try_from(written).map_err(|_| Refusal::Fault)?,
        ))
    }

    fn resize(&mut self, handle: u32, size: u32) -> Result<Reply<'_>, Refusal> {
        let open = self.held(handle)?;
        if !open.access.writes() {
            return Err(Refusal::Denied);
        }
        open.file.set_len(u64::from(size)).map_err(refusal)?;
        Ok(Reply::Done)
    }

    fn close(&mut self, handle: u32, stamp: Option<Time64>) -> Result<Reply<'_>, Refusal> {
        let slot = self.slot_of(handle)?;
        let open = self
            .files
            .get_mut(slot)
            .and_then(Option::take)
            .ok_or(Refusal::Handle)?;
        match stamp {
            Some(at) => open
                .file
                .set_times(&TimesChange {
                    accessed: StampChange::Keep,
                    modified: StampChange::At(shifted(at, self.offset.saturating_neg())),
                    nearest: true,
                })
                .map(|()| Reply::Done)
                .map_err(refusal),
            None => Ok(Reply::Done),
        }
    }

    fn remove(&mut self, path: &SharePath, directory: bool) -> Result<Reply<'_>, Refusal> {
        let (dir, name) = path.split_last().ok_or(Refusal::Denied)?;
        if !self.writable {
            return Err(Refusal::Denied);
        }
        let (at, found) = self.entry(&dir, name)?;
        let (host, _) = found.ok_or(if directory {
            Refusal::PathNotFound
        } else {
            Refusal::NotFound
        })?;
        match self.listing(at)?.dir.remove(&host, directory) {
            Ok(()) => {}
            Err(Errno::NotADirectory) => return Err(Refusal::PathNotFound),
            Err(Errno::IsADirectory) => return Err(Refusal::Denied),
            Err(errno) => return Err(refusal(errno)),
        }
        self.listing_mut(at)?.forget(name);
        Ok(Reply::Done)
    }

    fn rename(&mut self, from: &SharePath, to: &SharePath) -> Result<Reply<'_>, Refusal> {
        let (from_dir, from_name) = from.split_last().ok_or(Refusal::Denied)?;
        let (to_dir, to_name) = to.split_last().ok_or(Refusal::Denied)?;
        if !self.writable {
            return Err(Refusal::Denied);
        }
        let (source, found) = self.entry(&from_dir, from_name)?;
        let (host, _) = found.ok_or(Refusal::NotFound)?;
        let (target, taken) = self.entry(&to_dir, to_name)?;
        if taken.is_some() {
            return Err(Refusal::Denied);
        }
        let to_host = alias::host_name(&to_name);
        let (source_dir, target_dir) = (&self.listing(source)?.dir, &self.listing(target)?.dir);
        source_dir
            .rename(&host, target_dir, &to_host)
            .map_err(|errno| match errno {
                Errno::AlreadyExists => Refusal::Denied,
                errno => refusal(errno),
            })?;
        let id = target_dir
            .look(&to_host)
            .map_or(FileId::NONE, |stat| stat.id);
        if source == target {
            self.listing_mut(source)?
                .rename(from_name, to_name, to_host, id);
        } else {
            self.listing_mut(source)?.forget(from_name);
            self.listing_mut(target)?.insert(to_name, to_host, id);
        }
        Ok(Reply::Done)
    }

    fn make_dir(&mut self, path: &SharePath) -> Result<Reply<'_>, Refusal> {
        let (dir, name) = path.split_last().ok_or(Refusal::Denied)?;
        if !self.writable {
            return Err(Refusal::Denied);
        }
        let (at, found) = self.entry(&dir, name)?;
        if found.is_some() {
            return Err(Refusal::Denied);
        }
        let host = alias::host_name(&name);
        let listing = self.listing(at)?;
        listing.dir.make_dir(&host).map_err(|errno| match errno {
            Errno::AlreadyExists => Refusal::Denied,
            errno => refusal(errno),
        })?;
        let id = listing.dir.look(&host).map_or(FileId::NONE, |stat| stat.id);
        self.listing_mut(at)?.insert(name, host, id);
        Ok(Reply::Done)
    }
}

impl<D: HostDir> Folder for Server<D> {
    fn call(&mut self, op: &Op<'_>) -> Result<Reply<'_>, Refusal> {
        match *op {
            Op::Look { path } => self.look(&path).map(Reply::Looked),
            Op::Find {
                pattern,
                attributes,
                from,
            } => self.find(pattern, attributes, from),
            Op::Open {
                path,
                access,
                creation,
            } => self.open(&path, access, creation),
            Op::Read {
                handle,
                offset,
                len,
            } => self.read_at(handle, offset, len),
            Op::Write {
                handle,
                offset,
                bytes,
            } => self.write_at(handle, offset, bytes),
            Op::Resize { handle, size } => self.resize(handle, size),
            Op::Close { handle, stamp } => self.close(handle, stamp),
            Op::Commit { handle } => {
                self.held(handle)?.file.sync().map_err(refusal)?;
                Ok(Reply::Done)
            }
            Op::Size { handle } => {
                let size = self.held(handle)?.file.stat().map_err(refusal)?.size;
                u32::try_from(size)
                    .map(Reply::Size)
                    .map_err(|_| Refusal::Denied)
            }
            Op::Remove { path, directory } => self.remove(&path, directory),
            Op::Rename { from, to } => self.rename(&from, &to),
            Op::MakeDir { path } => self.make_dir(&path),
            Op::Space => self
                .listing(0)?
                .dir
                .space()
                .map(Reply::Space)
                .map_err(refusal),
        }
    }
}

/// `at` moved by `secs` seconds: from the host's clock to the machine's, or
/// back.
fn shifted(at: Time64, secs: i64) -> Time64 {
    Time64::new(at.secs().saturating_add(secs), at.subsec_nanos()).unwrap_or(at)
}

/// What keys the listing of the directory `host`, reached from listing
/// `parent`, whose identity is `id`.
fn key_of(id: FileId, parent: u32, host: &str) -> Key {
    if id == FileId::NONE {
        Key::Named {
            parent,
            host: String::from(host),
        }
    } else {
        Key::Id(id)
    }
}

/// The number a node is given to the machine: equal for every handle on one
/// node, and where two nodes share one, the machine only forgets more of what
/// it read.
fn node_of(id: FileId) -> u32 {
    let mut bytes = [0u8; 24];
    bytes[..16].copy_from_slice(&id.volume);
    bytes[16..].copy_from_slice(&id.node.to_le_bytes());
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|word| u32::from_le_bytes(*word))
        .fold(0, |acc, word| acc.rotate_left(7) ^ word)
}

/// What the host's refusal is to DOS.
fn refusal(errno: Errno) -> Refusal {
    match errno {
        Errno::NotFound => Refusal::NotFound,
        Errno::NotADirectory => Refusal::PathNotFound,
        Errno::PermissionDenied | Errno::ReadOnly | Errno::IsADirectory | Errno::LinkLoop => {
            Refusal::Denied
        }
        Errno::AlreadyExists => Refusal::Exists,
        Errno::NotEmpty => Refusal::NotEmpty,
        Errno::NoSpace => Refusal::Full,
        _ => Refusal::Fault,
    }
}

/// A refusal met on the way to a directory: what names nothing there is the
/// path's.
const fn path_refusal(refusal: Refusal) -> Refusal {
    match refusal {
        Refusal::NotFound => Refusal::PathNotFound,
        other => other,
    }
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
