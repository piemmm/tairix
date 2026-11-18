//! `Scrap`: a tree in the session worker's memory, which `Wimp$ScrapDir`
//! names, served through the same tree seam as the session's roots so one
//! server answers both. What it holds is bounded by a share of the session's
//! memory; a write past it is refused as a full disc would refuse it.
//!
//! A node's storage lives while its name or a handle on it does: a file
//! deleted while it is open keeps its bytes until the last handle goes.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_abi::driver::filesystem::VolumeStats;
use tairix_abi::fs::SeekRegion;
use tairix_abi::{
    Access, Errno, FileId, FileKind, FileStat, NodeExpect, NodeTimes, RenameFlags, StampChange,
    TimesChange, TimesKept, UnlinkFlags, FS_ATTR_KEY_MAX, FS_ATTR_VALUE_MAX, FS_NAME_MAX,
};
use tairix_fscopy::tree::{self, Creation, Entry, FileAccess, Kind, WriteAt};
use tairix_fscopy::{Source, Target};

/// The share of a session's memory `Scrap` may hold.
const SHARE: u64 = 8;

/// The block size `Scrap` reports its space in.
const BLOCK: u32 = 512;

/// The most attributes one node keeps: a containment bound, RISC OS keeping
/// five.
const ATTRS_MOST: usize = 16;

/// The volume identity `Scrap`'s nodes report.
const VOLUME: [u8; 16] = *b"riscos-scrap-vol";

/// The most a session of `ceiling` bytes lets `Scrap` hold.
#[must_use]
pub const fn bound(ceiling: u64) -> u64 {
    ceiling / SHARE
}

/// What a node holds.
enum Body {
    File(Vec<u8>),
    Dir(Vec<(String, usize)>),
}

/// A node.
struct Data {
    body: Body,
    parent: usize,
    serial: u64,
    mode: u32,
    times: NodeTimes,
    content_gen: u64,
    attrs: Vec<(String, Vec<u8>)>,
    /// Handles on it.
    handles: u32,
    /// Whether no directory names it any longer.
    detached: bool,
}

/// The tree's nodes, the bytes they hold, and the most they may.
struct Store {
    nodes: Vec<Option<Data>>,
    held: u64,
    most: u64,
    serial: u64,
}

impl Store {
    fn data(&self, at: usize) -> Result<&Data, Errno> {
        self.nodes
            .get(at)
            .and_then(Option::as_ref)
            .ok_or(Errno::Stale)
    }

    fn data_mut(&mut self, at: usize) -> Result<&mut Data, Errno> {
        self.nodes
            .get_mut(at)
            .and_then(Option::as_mut)
            .ok_or(Errno::Stale)
    }

    fn entries(&self, at: usize) -> Result<&Vec<(String, usize)>, Errno> {
        match &self.data(at)?.body {
            Body::Dir(entries) => Ok(entries),
            Body::File(_) => Err(Errno::NotADirectory),
        }
    }

    fn lookup(&self, dir: usize, name: &str) -> Result<usize, Errno> {
        self.entries(dir)?
            .iter()
            .find(|(held, _)| held == name)
            .map(|&(_, at)| at)
            .ok_or(Errno::NotFound)
    }

    /// `bytes` more charged, refused past the bound.
    fn charge(&mut self, bytes: u64) -> Result<(), Errno> {
        let held = self.held.checked_add(bytes).ok_or(Errno::NoSpace)?;
        if held > self.most {
            return Err(Errno::NoSpace);
        }
        self.held = held;
        Ok(())
    }

    fn room(&self) -> u64 {
        self.most.saturating_sub(self.held)
    }

    /// A node made `name` in `dir`.
    fn make(&mut self, dir: usize, name: &str, body: Body, mode: u32) -> Result<usize, Errno> {
        if name.is_empty()
            || name.len() > FS_NAME_MAX
            || name == "."
            || name == ".."
            || name.contains('/')
        {
            return Err(Errno::OutOfRange);
        }
        if self.lookup(dir, name).is_ok() {
            return Err(Errno::AlreadyExists);
        }
        self.charge(name.len() as u64)?;
        self.serial += 1;
        let data = Data {
            body,
            parent: dir,
            serial: self.serial,
            mode,
            times: NodeTimes::default(),
            content_gen: 0,
            attrs: Vec::new(),
            handles: 0,
            detached: false,
        };
        let at = if let Some(at) = self.nodes.iter().position(Option::is_none) {
            at
        } else {
            self.nodes.try_reserve(1).map_err(|_| Errno::OutOfMemory)?;
            self.nodes.push(None);
            self.nodes.len() - 1
        };
        let mut owned = String::new();
        owned
            .try_reserve_exact(name.len())
            .map_err(|_| Errno::OutOfMemory)?;
        owned.push_str(name);
        let Body::Dir(entries) = &mut self.data_mut(dir)?.body else {
            return Err(Errno::NotADirectory);
        };
        entries.try_reserve(1).map_err(|_| Errno::OutOfMemory)?;
        entries.push((owned, at));
        self.nodes[at] = Some(data);
        Ok(at)
    }

    /// The node at `at` let go where nothing names or holds it.
    fn reap(&mut self, at: usize) {
        let Some(data) = self.nodes.get(at).and_then(Option::as_ref) else {
            return;
        };
        if !data.detached || data.handles != 0 {
            return;
        }
        let freed = match &data.body {
            Body::File(bytes) => bytes.len() as u64,
            Body::Dir(_) => 0,
        } + data
            .attrs
            .iter()
            .map(|(key, value)| (key.len() + value.len()) as u64)
            .sum::<u64>();
        self.held = self.held.saturating_sub(freed);
        self.nodes[at] = None;
    }

    /// Whether `node` is `ancestor` or lies beneath it.
    fn within(&self, node: usize, ancestor: usize) -> bool {
        let mut at = node;
        for _ in 0..=self.nodes.len() {
            if at == ancestor {
                return true;
            }
            match self.data(at) {
                Ok(data) if data.parent != at => at = data.parent,
                _ => return false,
            }
        }
        false
    }
}

/// `Scrap`'s tree.
pub struct Tree {
    store: Rc<RefCell<Store>>,
}

impl Tree {
    /// An empty tree that may hold `most` bytes.
    #[must_use]
    pub fn new(most: u64) -> Self {
        let root = Data {
            body: Body::Dir(Vec::new()),
            parent: 0,
            serial: 0,
            mode: 0o777,
            times: NodeTimes::default(),
            content_gen: 0,
            attrs: Vec::new(),
            handles: 0,
            detached: false,
        };
        let store = Store {
            nodes: alloc::vec![Some(root)],
            held: 0,
            most,
            serial: 0,
        };
        Self {
            store: Rc::new(RefCell::new(store)),
        }
    }

    /// Its root, held.
    #[must_use]
    pub fn root(&self) -> Node {
        Node::held(&self.store, 0, false, false)
    }
}

/// A node of `Scrap` held open: a directory, or a file read or written as it
/// was opened.
pub struct Node {
    store: Rc<RefCell<Store>>,
    at: usize,
    read: bool,
    write: bool,
}

impl Node {
    fn held(store: &Rc<RefCell<Store>>, at: usize, read: bool, write: bool) -> Self {
        if let Ok(data) = store.borrow_mut().data_mut(at) {
            data.handles = data.handles.saturating_add(1);
        }
        Self {
            store: Rc::clone(store),
            at,
            read,
            write,
        }
    }

    fn child(&self, name: &str, read: bool, write: bool) -> Result<Self, Errno> {
        let at = self.store.borrow().lookup(self.at, name)?;
        Ok(Self::held(&self.store, at, read, write))
    }

    fn file_bytes<T>(&self, with: impl FnOnce(&[u8]) -> T) -> Result<T, Errno> {
        match &self.store.borrow().data(self.at)?.body {
            Body::File(bytes) => Ok(with(bytes)),
            Body::Dir(_) => Err(Errno::IsADirectory),
        }
    }

    /// The file's bytes changed by `change`, which is handed the room the
    /// bound leaves and answers the bytes it added.
    fn change(
        &self,
        change: impl FnOnce(&mut Vec<u8>, u64) -> Result<u64, Errno>,
    ) -> Result<(), Errno> {
        if !self.write {
            return Err(Errno::PermissionDenied);
        }
        let mut store = self.store.borrow_mut();
        let room = store.room();
        let data = store.data_mut(self.at)?;
        let Body::File(bytes) = &mut data.body else {
            return Err(Errno::IsADirectory);
        };
        let before = bytes.len() as u64;
        change(bytes, room)?;
        let after = bytes.len() as u64;
        data.content_gen += 1;
        if after >= before {
            store.held = store.held.saturating_add(after - before);
        } else {
            store.held = store.held.saturating_sub(before - after);
        }
        Ok(())
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        let mut store = self.store.borrow_mut();
        if let Ok(data) = store.data_mut(self.at) {
            data.handles = data.handles.saturating_sub(1);
        }
        store.reap(self.at);
    }
}

impl tree::Node for Node {
    fn stat(&self) -> Result<FileStat, Errno> {
        let store = self.store.borrow();
        let data = store.data(self.at)?;
        let (kind, size) = match &data.body {
            Body::File(bytes) => (FileKind::Regular, bytes.len() as u64),
            Body::Dir(_) => (FileKind::Directory, 0),
        };
        Ok(FileStat {
            kind,
            nlink: u32::from(!data.detached),
            size,
            allocated: size,
            mode: data.mode,
            uid: 0,
            gid: 0,
            id: FileId {
                volume: VOLUME,
                node: data.serial,
            },
            times: data.times,
            content_gen: data.content_gen,
        })
    }

    fn set_mode(&self, mode: u32) -> Result<(), Errno> {
        self.store.borrow_mut().data_mut(self.at)?.mode = mode & 0o7777;
        Ok(())
    }

    fn set_owner(&self, _uid: u32, _gid: u32) -> Result<(), Errno> {
        Err(Errno::NotSupported)
    }

    fn set_times(&self, change: &TimesChange) -> Result<TimesKept, Errno> {
        let mut store = self.store.borrow_mut();
        let data = store.data_mut(self.at)?;
        for (field, stamp) in [
            (&mut data.times.accessed, change.accessed),
            (&mut data.times.modified, change.modified),
        ] {
            match stamp {
                StampChange::At(at) => *field = at,
                StampChange::Keep | StampChange::Now => {}
            }
        }
        Ok(TimesKept::Exactly)
    }

    fn attr_keys(&self) -> Result<Vec<String>, Errno> {
        let store = self.store.borrow();
        Ok(store
            .data(self.at)?
            .attrs
            .iter()
            .map(|(key, _)| key.clone())
            .collect())
    }

    fn attr_value(&self, key: &str) -> Result<Vec<u8>, Errno> {
        let store = self.store.borrow();
        store
            .data(self.at)?
            .attrs
            .iter()
            .find(|(held, _)| held == key)
            .map(|(_, value)| value.clone())
            .ok_or(Errno::NotFound)
    }

    fn attr_set(&self, key: &str, value: &[u8]) -> Result<(), Errno> {
        if key.is_empty() || key.len() > FS_ATTR_KEY_MAX || value.len() > FS_ATTR_VALUE_MAX {
            return Err(Errno::OutOfRange);
        }
        let mut store = self.store.borrow_mut();
        let previous = store
            .data(self.at)?
            .attrs
            .iter()
            .find(|(held, _)| held == key)
            .map(|(held, value)| (held.len() + value.len()) as u64);
        let grown = (key.len() + value.len()) as u64;
        store.held = store.held.saturating_sub(previous.unwrap_or(0));
        if let Err(errno) = store.charge(grown) {
            store.held = store.held.saturating_add(previous.unwrap_or(0));
            return Err(errno);
        }
        let attrs = &mut store.data_mut(self.at)?.attrs;
        let mut owned = Vec::new();
        owned
            .try_reserve_exact(value.len())
            .map_err(|_| Errno::OutOfMemory)?;
        owned.extend_from_slice(value);
        if let Some((_, held)) = attrs.iter_mut().find(|(held, _)| held == key) {
            *held = owned;
            return Ok(());
        }
        if attrs.len() == ATTRS_MOST {
            return Err(Errno::NoSpace);
        }
        let mut name = String::new();
        name.try_reserve_exact(key.len())
            .map_err(|_| Errno::OutOfMemory)?;
        name.push_str(key);
        attrs.push((name, owned));
        Ok(())
    }

    fn attr_remove(&self, key: &str) -> Result<(), Errno> {
        let mut store = self.store.borrow_mut();
        let attrs = &mut store.data_mut(self.at)?.attrs;
        let at = attrs
            .iter()
            .position(|(held, _)| held == key)
            .ok_or(Errno::NotFound)?;
        let (held, value) = attrs.remove(at);
        store.held = store.held.saturating_sub((held.len() + value.len()) as u64);
        Ok(())
    }

    fn sync(&self) -> Result<(), Errno> {
        Ok(())
    }

    fn volume_stats(&self) -> Result<VolumeStats, Errno> {
        let store = self.store.borrow();
        Ok(VolumeStats {
            block_size: BLOCK,
            total_blocks: store.most / u64::from(BLOCK),
            free_blocks: store.room() / u64::from(BLOCK),
            avail_blocks: store.room() / u64::from(BLOCK),
            ..VolumeStats::default()
        })
    }
}

impl Source for Node {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        if !self.read {
            return Err(Errno::PermissionDenied);
        }
        self.file_bytes(|bytes| {
            let start = usize::try_from(offset).unwrap_or(usize::MAX);
            let rest = bytes.get(start..).unwrap_or_default();
            let take = rest.len().min(buf.len());
            buf[..take].copy_from_slice(&rest[..take]);
            take
        })
    }

    fn next_region(&mut self, offset: u64, region: SeekRegion) -> Result<Option<u64>, Errno> {
        let len = self.size()?;
        Ok(match region {
            SeekRegion::Data => (offset < len).then_some(offset),
            SeekRegion::Hole => (offset <= len).then_some(len),
        })
    }

    fn size(&mut self) -> Result<u64, Errno> {
        self.file_bytes(|bytes| bytes.len() as u64)
    }
}

impl Target for Node {
    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> Result<(), Errno> {
        let wrote = self.write_at(offset, bytes)?;
        if wrote == bytes.len() {
            Ok(())
        } else {
            Err(Errno::NoSpace)
        }
    }

    fn set_len(&mut self, len: u64) -> Result<(), Errno> {
        let len = usize::try_from(len).map_err(|_| Errno::NoSpace)?;
        self.change(|bytes, room| {
            let grown = len.saturating_sub(bytes.len()) as u64;
            if grown > room {
                return Err(Errno::NoSpace);
            }
            bytes
                .try_reserve(len.saturating_sub(bytes.len()))
                .map_err(|_| Errno::OutOfMemory)?;
            bytes.resize(len, 0);
            Ok(grown)
        })
    }
}

impl WriteAt for Node {
    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<usize, Errno> {
        let start = usize::try_from(offset).map_err(|_| Errno::NoSpace)?;
        let mut wrote = 0;
        self.change(|bytes, room| {
            let end = start.saturating_add(data.len());
            let grown = end.saturating_sub(bytes.len()) as u64;
            let allowed = if grown > room {
                data.len()
                    .saturating_sub(usize::try_from(grown - room).unwrap_or(usize::MAX))
            } else {
                data.len()
            };
            if allowed == 0 && !data.is_empty() {
                return Err(Errno::NoSpace);
            }
            let end = start + allowed;
            if bytes.len() < end {
                bytes
                    .try_reserve(end - bytes.len())
                    .map_err(|_| Errno::OutOfMemory)?;
                bytes.resize(end, 0);
            }
            bytes[start..end].copy_from_slice(&data[..allowed]);
            wrote = allowed;
            Ok(0)
        })?;
        Ok(wrote)
    }
}

impl tree::Dir for Node {
    type File = Self;

    fn entries(&self, most: usize) -> Result<Vec<Entry>, Errno> {
        let store = self.store.borrow();
        let mut entries = Vec::new();
        for (name, at) in store.entries(self.at)?.iter().take(most) {
            let data = store.data(*at)?;
            entries.try_reserve(1).map_err(|_| Errno::OutOfMemory)?;
            entries.push(Entry {
                name: name.clone(),
                kind: match data.body {
                    Body::File(_) => Kind::File,
                    Body::Dir(_) => Kind::Directory,
                },
                id: FileId {
                    volume: VOLUME,
                    node: data.serial,
                },
                nlink: 1,
            });
        }
        Ok(entries)
    }

    fn open_dir(&self, name: &str) -> Result<Self, Errno> {
        let child = self.child(name, false, false)?;
        match child.store.borrow().data(child.at)?.body {
            Body::Dir(_) => {}
            Body::File(_) => return Err(Errno::NotADirectory),
        }
        Ok(child)
    }

    fn enter(&self, name: &str) -> Result<Self, Errno> {
        self.open_dir(name)
    }

    fn open_file(&self, name: &str, write: bool) -> Result<Self, Errno> {
        let child = self.child(name, !write, write)?;
        child.file_bytes(|_| ())?;
        Ok(child)
    }

    fn follow(&self, name: &str, read: bool) -> Result<Self, Errno> {
        let child = self.child(name, read, false)?;
        if read {
            child.file_bytes(|_| ())?;
        }
        Ok(child)
    }

    fn open_node(&self, name: &str) -> Result<Self, Errno> {
        self.child(name, false, false)
    }

    fn open_reached(
        &self,
        name: &str,
        access: FileAccess,
        creation: Creation,
        mode: u32,
    ) -> Result<Self, Errno> {
        let found = self.store.borrow().lookup(self.at, name);
        let at = match (creation, found) {
            (Creation::New, Ok(_)) => return Err(Errno::AlreadyExists),
            (Creation::Existing, Err(errno)) => return Err(errno),
            (_, Ok(at)) => at,
            (_, Err(_)) => {
                self.store
                    .borrow_mut()
                    .make(self.at, name, Body::File(Vec::new()), mode)?
            }
        };
        let node = Self::held(
            &self.store,
            at,
            access.reads(),
            access.writes() || creation == Creation::Truncate,
        );
        node.file_bytes(|_| ())?;
        if creation == Creation::Truncate {
            node.change(|bytes, _| {
                bytes.clear();
                Ok(0)
            })?;
        }
        Ok(Self::held(&self.store, at, access.reads(), access.writes()))
    }

    fn create_file(&self, name: &str, mode: u32) -> Result<Self, Errno> {
        let at = self
            .store
            .borrow_mut()
            .make(self.at, name, Body::File(Vec::new()), mode)?;
        Ok(Self::held(&self.store, at, false, true))
    }

    fn make_dir(&self, name: &str, mode: u32) -> Result<Self, Errno> {
        let at = self
            .store
            .borrow_mut()
            .make(self.at, name, Body::Dir(Vec::new()), mode)?;
        Ok(Self::held(&self.store, at, false, false))
    }

    fn symlink(&self, _target: &[u8], _name: &str) -> Result<(), Errno> {
        Err(Errno::NotSupported)
    }

    fn readlink(&self, _name: &str) -> Result<String, Errno> {
        Err(Errno::NotSupported)
    }

    fn unlink(
        &self,
        name: &str,
        flags: UnlinkFlags,
        expect: Option<&NodeExpect>,
    ) -> Result<(), Errno> {
        let mut store = self.store.borrow_mut();
        let at = store.lookup(self.at, name)?;
        let data = store.data(at)?;
        if let Some(expect) = expect {
            if expect.id.node != data.serial {
                return Err(Errno::Stale);
            }
        }
        match &data.body {
            Body::Dir(entries) if !entries.is_empty() => return Err(Errno::NotEmpty),
            Body::File(_) if flags.is_directory_only() => return Err(Errno::NotADirectory),
            _ => {}
        }
        let Body::Dir(entries) = &mut store.data_mut(self.at)?.body else {
            return Err(Errno::NotADirectory);
        };
        entries.retain(|(held, _)| held != name);
        store.held = store.held.saturating_sub(name.len() as u64);
        store.data_mut(at)?.detached = true;
        store.reap(at);
        Ok(())
    }

    fn rename(
        &self,
        from: &str,
        to_dir: &Self,
        to: &str,
        flags: RenameFlags,
        _expect: Option<&NodeExpect>,
    ) -> Result<(), Errno> {
        if !Rc::ptr_eq(&self.store, &to_dir.store) {
            return Err(Errno::CrossVolume);
        }
        if to.is_empty() || to.len() > FS_NAME_MAX || to == "." || to == ".." || to.contains('/') {
            return Err(Errno::OutOfRange);
        }
        let mut store = self.store.borrow_mut();
        let moving = store.lookup(self.at, from)?;
        if store.within(to_dir.at, moving) {
            return Err(Errno::OutOfRange);
        }
        if store.lookup(to_dir.at, to).is_ok() {
            if flags.refuses_replace() || self.at != to_dir.at || from != to {
                return Err(Errno::AlreadyExists);
            }
            return Ok(());
        }
        let mut name = String::new();
        name.try_reserve_exact(to.len())
            .map_err(|_| Errno::OutOfMemory)?;
        name.push_str(to);
        if let Body::Dir(entries) = &mut store.data_mut(to_dir.at)?.body {
            entries.try_reserve(1).map_err(|_| Errno::OutOfMemory)?;
        }
        let Body::Dir(entries) = &mut store.data_mut(self.at)?.body else {
            return Err(Errno::NotADirectory);
        };
        entries.retain(|(held, _)| held != from);
        let Body::Dir(entries) = &mut store.data_mut(to_dir.at)?.body else {
            return Err(Errno::NotADirectory);
        };
        entries.push((name, moving));
        store.data_mut(moving)?.parent = to_dir.at;
        store.held = store
            .held
            .saturating_sub(from.len() as u64)
            .saturating_add(to.len() as u64);
        Ok(())
    }

    fn link(
        &self,
        _existing: &str,
        _to_dir: &Self,
        _new: &str,
        _follow: bool,
    ) -> Result<(), Errno> {
        Err(Errno::NotSupported)
    }

    fn access(&self, name: &str, _want: Access, _follow: bool) -> Result<(), Errno> {
        if name.is_empty() {
            return Ok(());
        }
        self.store.borrow().lookup(self.at, name).map(drop)
    }
}

#[cfg(test)]
mod tests;
