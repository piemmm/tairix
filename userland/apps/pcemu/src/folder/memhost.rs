//! An in-memory folder for the tests: directories, files and links, each node
//! with an identity, a modification time and a lock against changes, every
//! change moving its directory's stamps as a real volume's does. A link names
//! a node by its path from the root.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_abi::driver::filesystem::VolumeStats;
use tairix_abi::time::Time64;
use tairix_abi::{Errno, FileId, FileKind, FileStat, NodeTimes, StampChange, TimesChange};

use super::host::{HostDir, HostEntry, HostFile};
use super::{Access, Creation};

/// Blocks the in-memory volume holds, and each one's bytes.
const BLOCKS: u64 = 4096;
const BLOCK: u64 = 512;

/// The longest a file may grow: the whole volume, so no request asks the
/// tests for more memory than it holds.
const LONGEST: u64 = BLOCKS * BLOCK;

enum Body {
    Dir(BTreeMap<String, usize>),
    File(Vec<u8>),
    Link(String),
}

struct Node {
    body: Body,
    modified: Time64,
    changed: Time64,
    content_gen: u64,
    locked: bool,
}

struct State {
    nodes: Vec<Node>,
    clock: i64,
    full: bool,
    lists: usize,
}

impl State {
    fn tick(&mut self) -> Time64 {
        self.clock += 1;
        Time64::from_secs(1_000_000_000 + self.clock)
    }

    /// The node `name` names in directory `dir`, through links.
    fn child(&self, dir: usize, name: &str) -> Result<usize, Errno> {
        let Body::Dir(entries) = &self.nodes[dir].body else {
            return Err(Errno::NotADirectory);
        };
        let at = *entries.get(name).ok_or(Errno::NotFound)?;
        self.followed(at, 0)
    }

    fn followed(&self, at: usize, depth: u8) -> Result<usize, Errno> {
        match &self.nodes[at].body {
            Body::Link(target) if depth < 8 => self.followed(self.path(target)?, depth + 1),
            Body::Link(_) => Err(Errno::LinkLoop),
            Body::Dir(_) | Body::File(_) => Ok(at),
        }
    }

    fn path(&self, path: &str) -> Result<usize, Errno> {
        path.split('/')
            .filter(|part| !part.is_empty())
            .try_fold(0, |at, part| self.child(at, part))
    }

    fn entries_mut(&mut self, dir: usize) -> Result<&mut BTreeMap<String, usize>, Errno> {
        match &mut self.nodes[dir].body {
            Body::Dir(entries) => Ok(entries),
            _ => Err(Errno::NotADirectory),
        }
    }

    fn touched(&mut self, node: usize) {
        let now = self.tick();
        let touched = &mut self.nodes[node];
        touched.modified = now;
        touched.changed = now;
        touched.content_gen += 1;
    }

    fn add(&mut self, dir: usize, name: &str, body: Body) -> Result<usize, Errno> {
        if self.nodes[dir].locked {
            return Err(Errno::PermissionDenied);
        }
        if self.entries_mut(dir)?.contains_key(name) {
            return Err(Errno::AlreadyExists);
        }
        let now = self.tick();
        self.nodes.push(Node {
            body,
            modified: now,
            changed: now,
            content_gen: 1,
            locked: false,
        });
        let at = self.nodes.len() - 1;
        self.entries_mut(dir)?.insert(String::from(name), at);
        self.touched(dir);
        Ok(at)
    }

    fn used(&self) -> u64 {
        self.nodes
            .iter()
            .map(|node| match &node.body {
                Body::File(bytes) => (bytes.len() as u64).div_ceil(BLOCK),
                Body::Dir(_) | Body::Link(_) => 0,
            })
            .sum()
    }

    fn stat(&self, at: usize) -> FileStat {
        let node = &self.nodes[at];
        let (kind, size) = match &node.body {
            Body::Dir(_) => (FileKind::Directory, 0),
            Body::File(bytes) => (FileKind::Regular, bytes.len() as u64),
            Body::Link(target) => (FileKind::Symlink, target.len() as u64),
        };
        FileStat {
            kind,
            nlink: 1,
            size,
            allocated: size,
            mode: if node.locked { 0o444 } else { 0o644 },
            uid: 1000,
            gid: 1000,
            id: id_of(at),
            times: NodeTimes {
                created: node.changed,
                modified: node.modified,
                accessed: node.modified,
                changed: node.changed,
            },
            content_gen: node.content_gen,
        }
    }
}

const fn id_of(at: usize) -> FileId {
    FileId {
        volume: [7; 16],
        node: at as u64,
    }
}

/// An in-memory folder, shared by every handle on it.
#[derive(Clone)]
pub struct MemHost {
    state: Rc<RefCell<State>>,
}

impl Default for MemHost {
    fn default() -> Self {
        Self::new()
    }
}

impl MemHost {
    /// An empty folder.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Rc::new(RefCell::new(State {
                nodes: alloc::vec![Node {
                    body: Body::Dir(BTreeMap::new()),
                    modified: Time64::from_secs(1_000_000_000),
                    changed: Time64::from_secs(1_000_000_000),
                    content_gen: 1,
                    locked: false,
                }],
                clock: 0,
                full: false,
                lists: 0,
            })),
        }
    }

    /// Its root, held.
    #[must_use]
    pub fn root(&self) -> MemDir {
        MemDir {
            state: Rc::clone(&self.state),
            node: 0,
        }
    }

    /// The directory at `path`, made with those above it where missing.
    fn make_path(&self, path: &str) -> usize {
        let mut state = self.state.borrow_mut();
        path.split('/')
            .filter(|part| !part.is_empty())
            .fold(0, |at, part| match state.child(at, part) {
                Ok(child) => child,
                Err(_) => state
                    .add(at, part, Body::Dir(BTreeMap::new()))
                    .expect("a fixture directory is made"),
            })
    }

    fn split(path: &str) -> (&str, &str) {
        path.rsplit_once('/').unwrap_or(("", path))
    }

    /// With a directory at `path`.
    #[must_use]
    pub fn dir(self, path: &str) -> Self {
        self.make_path(path);
        self
    }

    /// With a file at `path` holding `bytes`.
    #[must_use]
    pub fn file(self, path: &str, bytes: &[u8]) -> Self {
        let (dir, name) = Self::split(path);
        let at = self.make_path(dir);
        self.state
            .borrow_mut()
            .add(at, name, Body::File(bytes.to_vec()))
            .expect("a fixture file is made");
        self
    }

    /// With a link at `path` to the node at `target`, a path from the root.
    #[must_use]
    pub fn link(self, path: &str, target: &str) -> Self {
        let (dir, name) = Self::split(path);
        let at = self.make_path(dir);
        self.state
            .borrow_mut()
            .add(at, name, Body::Link(String::from(target)))
            .expect("a fixture link is made");
        self
    }

    /// With the node at `path` refusing every change.
    #[must_use]
    pub fn locked(self, path: &str) -> Self {
        {
            let mut state = self.state.borrow_mut();
            let at = state.path(path).expect("a fixture node");
            state.nodes[at].locked = true;
        }
        self
    }

    /// With the volume full: no byte more is written.
    #[must_use]
    pub fn full(self) -> Self {
        self.state.borrow_mut().full = true;
        self
    }

    /// The bytes of the file at `path`.
    #[must_use]
    pub fn bytes(&self, path: &str) -> Option<Vec<u8>> {
        let state = self.state.borrow();
        match &state.nodes[state.path(path).ok()?].body {
            Body::File(bytes) => Some(bytes.clone()),
            _ => None,
        }
    }

    /// The names directory `path` holds, in byte order.
    #[must_use]
    pub fn names(&self, path: &str) -> Vec<String> {
        let state = self.state.borrow();
        state
            .path(path)
            .ok()
            .and_then(|at| match &state.nodes[at].body {
                Body::Dir(entries) => Some(entries.keys().cloned().collect()),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// When the node at `path` last changed.
    #[must_use]
    pub fn modified(&self, path: &str) -> Option<Time64> {
        let state = self.state.borrow();
        state.path(path).ok().map(|at| state.nodes[at].modified)
    }

    /// How many times any directory was listed.
    #[must_use]
    pub fn lists(&self) -> usize {
        self.state.borrow().lists
    }

    /// Make the file at `path` hold `bytes`, as another program would.
    pub fn replace(&self, path: &str, bytes: &[u8]) {
        let mut state = self.state.borrow_mut();
        let at = state.path(path).expect("a fixture file");
        if let Body::File(held) = &mut state.nodes[at].body {
            held.clear();
            held.extend_from_slice(bytes);
        }
        state.touched(at);
    }

    /// Put a file at `path` outside the share, as another program would.
    pub fn put(&self, path: &str, bytes: &[u8]) {
        let (dir, name) = Self::split(path);
        let at = self.make_path(dir);
        self.state
            .borrow_mut()
            .add(at, name, Body::File(bytes.to_vec()))
            .expect("a file is put");
    }
}

/// A directory of a [`MemHost`], held.
pub struct MemDir {
    state: Rc<RefCell<State>>,
    node: usize,
}

/// A file of a [`MemHost`], held.
pub struct MemFile {
    state: Rc<RefCell<State>>,
    node: usize,
    access: Access,
}

impl HostDir for MemDir {
    type File = MemFile;

    fn list(&self) -> Result<Vec<HostEntry>, Errno> {
        let mut state = self.state.borrow_mut();
        state.lists += 1;
        let Body::Dir(entries) = &state.nodes[self.node].body else {
            return Err(Errno::NotADirectory);
        };
        Ok(entries
            .iter()
            .map(|(name, &at)| HostEntry {
                name: name.clone(),
                id: id_of(at),
            })
            .collect())
    }

    fn stat(&self) -> Result<FileStat, Errno> {
        Ok(self.state.borrow().stat(self.node))
    }

    fn look(&self, name: &str) -> Result<FileStat, Errno> {
        let state = self.state.borrow();
        state.child(self.node, name).map(|at| state.stat(at))
    }

    fn may_write(&self, name: Option<&str>) -> bool {
        let state = self.state.borrow();
        match name {
            None => !state.nodes[self.node].locked,
            Some(name) => state
                .child(self.node, name)
                .is_ok_and(|at| !state.nodes[at].locked),
        }
    }

    fn enter(&self, name: &str) -> Result<Self, Errno> {
        let state = self.state.borrow();
        let at = state.child(self.node, name)?;
        match state.nodes[at].body {
            Body::Dir(_) => Ok(Self {
                state: Rc::clone(&self.state),
                node: at,
            }),
            _ => Err(Errno::NotADirectory),
        }
    }

    fn open(&self, name: &str, access: Access, creation: Creation) -> Result<MemFile, Errno> {
        let mut state = self.state.borrow_mut();
        let at = match state.child(self.node, name) {
            Ok(_) if creation == Creation::New => return Err(Errno::AlreadyExists),
            Ok(at) => at,
            Err(Errno::NotFound) if creation != Creation::Existing => {
                state.add(self.node, name, Body::File(Vec::new()))?
            }
            Err(errno) => return Err(errno),
        };
        if access.writes() && state.nodes[at].locked {
            return Err(Errno::PermissionDenied);
        }
        match &mut state.nodes[at].body {
            Body::Dir(_) => return Err(Errno::IsADirectory),
            Body::File(bytes) if creation == Creation::Truncate => bytes.clear(),
            _ => {}
        }
        Ok(MemFile {
            state: Rc::clone(&self.state),
            node: at,
            access,
        })
    }

    fn make_dir(&self, name: &str) -> Result<(), Errno> {
        self.state
            .borrow_mut()
            .add(self.node, name, Body::Dir(BTreeMap::new()))
            .map(drop)
    }

    fn remove(&self, name: &str, directory: bool) -> Result<(), Errno> {
        let mut state = self.state.borrow_mut();
        if state.nodes[self.node].locked {
            return Err(Errno::PermissionDenied);
        }
        let at = *state
            .entries_mut(self.node)?
            .get(name)
            .ok_or(Errno::NotFound)?;
        match (&state.nodes[at].body, directory) {
            (Body::Dir(entries), true) if !entries.is_empty() => return Err(Errno::NotEmpty),
            (Body::Dir(_), false) => return Err(Errno::IsADirectory),
            (Body::File(_) | Body::Link(_), true) => return Err(Errno::NotADirectory),
            _ => {}
        }
        state.entries_mut(self.node)?.remove(name);
        state.touched(self.node);
        Ok(())
    }

    fn rename(&self, from: &str, to_dir: &Self, to: &str) -> Result<(), Errno> {
        let mut state = self.state.borrow_mut();
        if state.nodes[self.node].locked || state.nodes[to_dir.node].locked {
            return Err(Errno::PermissionDenied);
        }
        if state.entries_mut(to_dir.node)?.contains_key(to) {
            return Err(Errno::AlreadyExists);
        }
        let at = state
            .entries_mut(self.node)?
            .remove(from)
            .ok_or(Errno::NotFound)?;
        state.entries_mut(to_dir.node)?.insert(String::from(to), at);
        state.touched(self.node);
        state.touched(to_dir.node);
        Ok(())
    }

    fn space(&self) -> Result<VolumeStats, Errno> {
        let state = self.state.borrow();
        let free = if state.full {
            0
        } else {
            BLOCKS.saturating_sub(state.used())
        };
        Ok(VolumeStats {
            block_size: u32::try_from(BLOCK).unwrap_or(u32::MAX),
            in_reserve: false,
            total_blocks: BLOCKS,
            free_blocks: free,
            avail_blocks: free,
            files: 0,
            files_free: 0,
        })
    }
}

impl HostFile for MemFile {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        let state = self.state.borrow();
        let Body::File(bytes) = &state.nodes[self.node].body else {
            return Err(Errno::IsADirectory);
        };
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let count = buf.len().min(bytes.len() - start);
        buf[..count].copy_from_slice(&bytes[start..start + count]);
        Ok(count)
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<usize, Errno> {
        if !self.access.writes() {
            return Err(Errno::PermissionDenied);
        }
        let mut state = self.state.borrow_mut();
        if state.full {
            return Err(Errno::NoSpace);
        }
        let Body::File(bytes) = &mut state.nodes[self.node].body else {
            return Err(Errno::IsADirectory);
        };
        let start = usize::try_from(offset).map_err(|_| Errno::OutOfRange)?;
        let end = start
            .checked_add(data.len())
            .filter(|&end| u64::try_from(end).is_ok_and(|end| end <= LONGEST))
            .ok_or(Errno::NoSpace)?;
        if bytes.len() < end {
            bytes.resize(end, 0);
        }
        bytes[start..start + data.len()].copy_from_slice(data);
        state.touched(self.node);
        Ok(data.len())
    }

    fn set_len(&mut self, len: u64) -> Result<(), Errno> {
        if !self.access.writes() {
            return Err(Errno::PermissionDenied);
        }
        let mut state = self.state.borrow_mut();
        let Body::File(bytes) = &mut state.nodes[self.node].body else {
            return Err(Errno::IsADirectory);
        };
        if len > LONGEST {
            return Err(Errno::NoSpace);
        }
        bytes.resize(usize::try_from(len).map_err(|_| Errno::OutOfRange)?, 0);
        state.touched(self.node);
        Ok(())
    }

    fn stat(&self) -> Result<FileStat, Errno> {
        Ok(self.state.borrow().stat(self.node))
    }

    fn set_times(&self, change: &TimesChange) -> Result<(), Errno> {
        let mut state = self.state.borrow_mut();
        if state.nodes[self.node].locked {
            return Err(Errno::PermissionDenied);
        }
        if let StampChange::At(at) = change.modified {
            state.nodes[self.node].modified = at;
        }
        Ok(())
    }

    fn sync(&self) -> Result<(), Errno> {
        Ok(())
    }
}
