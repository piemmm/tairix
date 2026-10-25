//! An in-memory filesystem behind the shared tree seam, for the first-party
//! tools' host tests: [`Tree`] is a [`Roots`] whose directory and file
//! handles share it, bound to nodes as the kernel's descriptors are, with an
//! attacker hook that runs between two of its calls and may rename, replace
//! or relink any name.
//!
//! It keeps the kernel's rules a test can observe: the creator's umask on
//! every create through the seam, a link never followed where the seam says
//! so, the rename rules, `NodeExpect` checks answering `Stale`, removed
//! directories, and volumes a rename or link cannot cross. Of the permission
//! checks it models two always: a mode change needs the caller to own the
//! node, whatever it holds, as the kernel has it, and an owner change to
//! another user or group needs [`Tree::privileged`]. [`Tree::checking_permissions`]
//! adds the owner, group and other classes' bits: search on every directory
//! looked in, write and search on a directory a name is made in or dropped
//! from, read and write on a file opened for them, read on the listing of a
//! directory the caller does not own, and write on a node whose attribute a
//! non-owner sets. Every other failure is a test's [`Tree::refuse`].
//!
//! Its caller is [`USER`]. It is test support: a builder, mutator or reader
//! handed a path that names nothing panics. [`resolve`] is the kernel's path
//! resolution in miniature, for a test filesystem keyed by path.

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_abi::fs::{
    mode_after_owner_change, SeekRegion, FS_LINK_MODE, FS_MODE_MASK, FS_OWNER_UNCHANGED,
    FS_SETGID_BIT,
};
use tairix_abi::{
    Access, CapabilityId, Errno, FileId, FileKind, FileStat, NodeExpect, NodeTimes, RenameFlags,
    Time64, UnlinkFlags,
};
use tairix_path::{dir_name, join, leaf_name, parse, root_len};

use crate::data::{Source, Target};
use crate::tree::{entry_name, Dir, Entry, Kind, Node, Roots};

/// Who the tree's caller is, and who every node belongs to unless a builder
/// says otherwise.
pub const USER: (u32, u32) = (1000, 1000);

/// The mode a builder makes a directory in.
const DIR_MODE: u32 = 0o755;

/// The mode a builder makes a file in.
const FILE_MODE: u32 = 0o644;

/// The umask a tree starts with.
const UMASK: u32 = 0o022;

/// The attribute a tree keeps a node's capability gate in, its identifier
/// little-endian, so the gate tools' doubles reach it through a held node.
const GATE: &str = "user.gate";

/// The name an operation builds a replacement under beside `name`, as the
/// system's stager gives one; a name beginning `locked` gets none, so a test
/// can see what a refusal does.
///
/// # Errors
///
/// [`Errno::Busy`] for a name beginning `locked`.
pub fn staged(name: &str) -> Result<String, Errno> {
    if name.starts_with("locked") {
        return Err(Errno::Busy);
    }
    Ok(alloc::format!("{name}.staged"))
}

/// The gate `node` carries, as a [`Tree`] keeps one.
///
/// # Errors
///
/// The attribute read's refusal, or [`Errno::OutOfRange`] for a malformed one.
pub fn gate_of(node: &dyn Node) -> Result<Option<CapabilityId>, Errno> {
    if !node.attr_keys()?.iter().any(|key| key == GATE) {
        return Ok(None);
    }
    let raw = node.attr_value(GATE)?;
    let raw = <[u8; 2]>::try_from(raw.as_slice()).map_err(|_| Errno::OutOfRange)?;
    CapabilityId::from_raw(u16::from_le_bytes(raw)).map(Some)
}

/// Give `node` the gate `cap`, or clear its gate for [`None`], as a [`Tree`]
/// keeps one.
///
/// # Errors
///
/// The attribute change's refusal.
pub fn set_gate(node: &dyn Node, cap: Option<CapabilityId>) -> Result<(), Errno> {
    match cap {
        Some(cap) => node.attr_set(GATE, &cap.as_u16().to_le_bytes()),
        None if node.attr_keys()?.iter().any(|key| key == GATE) => node.attr_remove(GATE),
        None => Ok(()),
    }
}

const ROOT: usize = 0;

/// Hops a resolution takes before it is a cycle.
const MAX_HOPS: usize = 8;

/// Read, write and search, as permission bits of one class.
const READ: u32 = 4;
const WRITE: u32 = 2;
const SEARCH: u32 = 1;

/// A hook run before a call.
type Hook = Box<dyn FnOnce(&Tree)>;

/// When a hook runs.
enum When {
    /// Before the call of this number.
    Call(usize),
    /// Before the first call listed as this line.
    Line(String),
}

enum Body {
    File {
        bytes: Vec<u8>,
        /// The stored runs `(start, end)`; `None` for a file stored
        /// throughout.
        runs: Option<Vec<(u64, u64)>>,
    },
    Dir {
        /// In the order they were made, which is the listing's order.
        entries: Vec<(String, usize)>,
        parent: usize,
        removed: bool,
    },
    Link {
        target: String,
    },
}

struct NodeData {
    body: Body,
    mode: u32,
    uid: u32,
    gid: u32,
    /// Names, for a file or link; a directory's count is derived.
    nlink: u32,
    volume: u8,
    attrs: Vec<(String, Vec<u8>)>,
    content_gen: u64,
    modified: Time64,
    changed: Time64,
    /// Made through the seam, so what is put in it is watched.
    watched: bool,
    /// Bytes written to it through the seam.
    written: usize,
}

struct State {
    nodes: Vec<NodeData>,
    umask: u32,
    privileged: bool,
    next_volume: u8,
    clock: i64,
    generation: u64,
    calls: Vec<String>,
    hooks: Vec<(When, Hook)>,
    refusals: Vec<(String, Errno)>,
    attr_refusals: Vec<(String, Errno)>,
    fill_modes: Vec<(String, u32)>,
    /// Whether each class's permission bits are checked.
    permissions: bool,
    /// Each root spelling a path may begin with, and the directory it names.
    roots: Vec<(String, usize)>,
    /// Nodes whose volume gives them no identity.
    unidentified: Vec<usize>,
    /// Directories listed with no identity, as a mount point is.
    covered: Vec<usize>,
}

impl State {
    fn data(&self, node: usize) -> &NodeData {
        &self.nodes[node]
    }

    fn data_mut(&mut self, node: usize) -> &mut NodeData {
        &mut self.nodes[node]
    }

    fn tick(&mut self) -> Time64 {
        self.clock += 1;
        Time64::from_secs(self.clock)
    }

    fn entries(&self, dir: usize) -> Result<&Vec<(String, usize)>, Errno> {
        match &self.data(dir).body {
            Body::Dir { entries, .. } => Ok(entries),
            _ => Err(Errno::NotADirectory),
        }
    }

    fn is_dir(&self, node: usize) -> bool {
        matches!(self.data(node).body, Body::Dir { .. })
    }

    fn removed(&self, dir: usize) -> bool {
        matches!(self.data(dir).body, Body::Dir { removed: true, .. })
    }

    fn parent_of(&self, dir: usize) -> Result<usize, Errno> {
        match self.data(dir).body {
            Body::Dir { parent, .. } => Ok(parent),
            _ => Err(Errno::NotADirectory),
        }
    }

    fn lookup(&self, dir: usize, name: &str) -> Result<usize, Errno> {
        match name {
            "." => return self.entries(dir).map(|_| dir),
            ".." => return self.parent_of(dir),
            _ => {}
        }
        if self.removed(dir) {
            return Err(Errno::NotFound);
        }
        self.entries(dir)?
            .iter()
            .find(|(entry, _)| entry == name)
            .map(|&(_, node)| node)
            .ok_or(Errno::NotFound)
    }

    fn permits(&self, node: usize, want: u32) -> bool {
        let data = self.data(node);
        let shift = if data.uid == USER.0 {
            6
        } else if data.gid == USER.1 {
            3
        } else {
            0
        };
        (data.mode >> shift) & want == want
    }

    fn require(&self, node: usize, want: u32) -> Result<(), Errno> {
        if !self.permissions || self.permits(node, want) {
            Ok(())
        } else {
            Err(Errno::PermissionDenied)
        }
    }

    /// The node `path` names: each link on the way followed, a final one
    /// only when `follow`; search permission asked of each directory looked
    /// in when `checked`.
    fn resolve(&self, path: &str, follow: bool, checked: bool) -> Result<usize, Errno> {
        self.resolve_from(ROOT, path, follow, checked)
    }

    /// The node `path` names from the directory `start`, an absolute one from
    /// the root.
    fn resolve_from(
        &self,
        start: usize,
        path: &str,
        follow: bool,
        checked: bool,
    ) -> Result<usize, Errno> {
        let (mut at, rest) = match root_len(path) {
            // The system copies in no empty path.
            _ if path.is_empty() => return Err(Errno::LengthOutOfRange),
            _ if path.starts_with('/') => (ROOT, path),
            Ok(0) => (start, path),
            Ok(len) => {
                // The resolver refuses a `..` above an alias or volume root.
                parse(path).map_err(|_| Errno::OutOfRange)?;
                let spelled = path[..len].trim_end_matches('/');
                let named = self.roots.iter().find(|(root, _)| root == spelled);
                (named.ok_or(Errno::NotFound)?.1, &path[len..])
            }
            Err(_) => return Err(Errno::OutOfRange),
        };
        let mut pending: Vec<String> = rest.split('/').rev().map(String::from).collect();
        let mut hops = 0;
        while let Some(part) = pending.pop() {
            if part.is_empty() || part == "." {
                continue;
            }
            if checked {
                self.require(at, SEARCH)?;
            }
            let child = self.lookup(at, &part)?;
            let last = pending.iter().all(|rest| rest.is_empty() || rest == ".");
            if let Body::Link { target } = &self.data(child).body {
                if follow || !last {
                    hops += 1;
                    if hops > MAX_HOPS {
                        return Err(Errno::LinkLoop);
                    }
                    if target.starts_with('/') {
                        at = ROOT;
                    }
                    pending.extend(target.split('/').rev().map(String::from));
                    continue;
                }
            }
            if !last && !self.is_dir(child) {
                return Err(Errno::NotADirectory);
            }
            at = child;
        }
        Ok(at)
    }

    /// A path naming `node`, through live directories.
    fn path_of(&self, node: usize, depth: usize) -> Option<String> {
        if node == ROOT {
            return Some(String::from("/"));
        }
        if depth == 0 {
            return None;
        }
        self.nodes
            .iter()
            .enumerate()
            .find_map(|(index, data)| match &data.body {
                Body::Dir {
                    entries,
                    removed: false,
                    ..
                } if index != node => {
                    entries
                        .iter()
                        .find(|&&(_, child)| child == node)
                        .and_then(|(name, _)| {
                            self.path_of(index, depth - 1)
                                .map(|parent| join(&parent, name))
                        })
                }
                _ => None,
            })
    }

    fn path(&self, node: usize) -> String {
        self.path_of(node, 256).unwrap_or_else(|| String::from("?"))
    }

    fn id(&self, node: usize) -> FileId {
        if self.unidentified.contains(&node) {
            return FileId::NONE;
        }
        FileId {
            volume: [self.data(node).volume; 16],
            node: node as u64 + 1,
        }
    }

    fn stat(&self, node: usize) -> FileStat {
        let data = self.data(node);
        let (kind, size, nlink) = match &data.body {
            Body::File { bytes, .. } => (FileKind::Regular, bytes.len() as u64, data.nlink),
            Body::Link { target } => (FileKind::Symlink, target.len() as u64, data.nlink),
            Body::Dir { entries, .. } => {
                let subdirs = entries.iter().filter(|&&(_, child)| self.is_dir(child));
                let subdirs = u32::try_from(subdirs.count()).unwrap_or(u32::MAX);
                (FileKind::Directory, 0, subdirs.saturating_add(2))
            }
        };
        FileStat {
            kind,
            nlink,
            size,
            allocated: size,
            mode: data.mode,
            uid: data.uid,
            gid: data.gid,
            id: self.id(node),
            times: NodeTimes {
                created: Time64::UNIX_EPOCH,
                modified: data.modified,
                accessed: Time64::UNIX_EPOCH,
                changed: data.changed,
            },
            content_gen: data.content_gen,
        }
    }

    /// A new node of `body` in the directory `dir` under `name`.
    fn insert(&mut self, dir: usize, name: &str, body: Body, mode: u32, watched: bool) -> usize {
        let now = self.tick();
        let volume = self.data(dir).volume;
        let node = self.nodes.len();
        self.nodes.push(NodeData {
            body,
            mode: mode & FS_MODE_MASK,
            uid: USER.0,
            gid: USER.1,
            nlink: 1,
            volume,
            attrs: Vec::new(),
            content_gen: 0,
            modified: now,
            changed: now,
            watched,
            written: 0,
        });
        self.add_entry(dir, name, node);
        node
    }

    fn add_entry(&mut self, dir: usize, name: &str, node: usize) {
        let now = self.tick();
        if self.data(dir).watched {
            let path = self.path(dir);
            let mode = self.data(dir).mode;
            self.fill_modes.push((path, mode));
        }
        let data = self.data_mut(dir);
        if let Body::Dir { entries, .. } = &mut data.body {
            entries.push((String::from(name), node));
        }
        data.modified = now;
        data.changed = now;
    }

    /// Drop the entry `name` of `dir`, answering the node it named.
    fn drop_entry(&mut self, dir: usize, name: &str) -> Option<usize> {
        let now = self.tick();
        let data = self.data_mut(dir);
        let Body::Dir { entries, .. } = &mut data.body else {
            return None;
        };
        let index = entries.iter().position(|(entry, _)| entry == name)?;
        let (_, node) = entries.remove(index);
        data.modified = now;
        data.changed = now;
        let gone = self.data_mut(node);
        gone.nlink = gone.nlink.saturating_sub(1);
        gone.changed = now;
        if let Body::Dir { removed, .. } = &mut gone.body {
            *removed = true;
        }
        Some(node)
    }

    /// The refusal a test registered for the call listed as `line`.
    fn refused(&self, line: &str) -> Result<(), Errno> {
        match self.refusals.iter().find(|(call, _)| call == line) {
            Some(&(_, errno)) => Err(errno),
            None => Ok(()),
        }
    }

    /// Whether `node` is `ancestor` or lies beneath it.
    fn within(&self, node: usize, ancestor: usize) -> bool {
        let mut at = node;
        for _ in 0..=self.nodes.len() {
            if at == ancestor {
                return true;
            }
            match self.parent_of(at) {
                Ok(parent) if parent != at => at = parent,
                _ => return false,
            }
        }
        false
    }

    fn retag(&mut self, node: usize, volume: u8) {
        self.data_mut(node).volume = volume;
        let children: Vec<usize> = match &self.data(node).body {
            Body::Dir { entries, .. } => entries.iter().map(|&(_, child)| child).collect(),
            _ => Vec::new(),
        };
        for child in children {
            self.retag(child, volume);
        }
    }
}

/// The in-memory filesystem: a [`Roots`] whose handles share it. Clones
/// share it too.
#[derive(Clone)]
pub struct Tree {
    state: Rc<RefCell<State>>,
}

impl core::fmt::Debug for Tree {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Tree")
    }
}

impl Default for Tree {
    fn default() -> Self {
        Self::new()
    }
}

/// A node of a [`Tree`] held open: as a directory, a file, or a node opened
/// with no access, as the call that opened it asked.
#[derive(Debug)]
pub struct Handle {
    tree: Tree,
    node: usize,
    read: bool,
    write: bool,
}

/// A [`Handle`] on a directory.
pub type TreeDir = Handle;

/// A [`Handle`] on a file, or on a node held for its metadata.
pub type TreeFile = Handle;

impl Tree {
    /// An empty tree: a root directory, the umask `022`.
    #[must_use]
    pub fn new() -> Self {
        let root = NodeData {
            body: Body::Dir {
                entries: Vec::new(),
                parent: ROOT,
                removed: false,
            },
            mode: DIR_MODE,
            uid: USER.0,
            gid: USER.1,
            nlink: 2,
            volume: 1,
            attrs: Vec::new(),
            content_gen: 0,
            modified: Time64::UNIX_EPOCH,
            changed: Time64::UNIX_EPOCH,
            watched: false,
            written: 0,
        };
        Self {
            state: Rc::new(RefCell::new(State {
                nodes: alloc::vec![root],
                umask: UMASK,
                privileged: false,
                next_volume: 2,
                clock: 0,
                generation: 0,
                calls: Vec::new(),
                hooks: Vec::new(),
                refusals: Vec::new(),
                attr_refusals: Vec::new(),
                fill_modes: Vec::new(),
                permissions: false,
                roots: Vec::new(),
                unidentified: Vec::new(),
                covered: Vec::new(),
            })),
        }
    }

    /// The directory `path`, and each missing one on the way.
    #[must_use]
    pub fn dir(self, path: &str) -> Self {
        self.ensure_dir(path);
        self
    }

    /// The file `path` holding `bytes`.
    #[must_use]
    pub fn file(self, path: &str, bytes: &[u8]) -> Self {
        self.put(
            path,
            Body::File {
                bytes: bytes.to_vec(),
                runs: None,
            },
            FILE_MODE,
        );
        self
    }

    /// The file `path`, `len` bytes long, storing `runs` of `(offset,
    /// bytes)` and holes between them.
    #[must_use]
    pub fn sparse(self, path: &str, len: usize, runs: &[(u64, &[u8])]) -> Self {
        let mut bytes = alloc::vec![0u8; len];
        let mut stored = Vec::new();
        for &(at, run) in runs {
            let start = usize::try_from(at).unwrap_or(usize::MAX);
            bytes[start..start + run.len()].copy_from_slice(run);
            stored.push((at, at + run.len() as u64));
        }
        self.put(
            path,
            Body::File {
                bytes,
                runs: Some(stored),
            },
            FILE_MODE,
        );
        self
    }

    /// The symbolic link `path` storing `target`.
    #[must_use]
    pub fn link(self, path: &str, target: &str) -> Self {
        self.relink(path, target);
        self
    }

    /// `path` as a second name for the node `existing` names.
    #[must_use]
    pub fn hard_link(self, path: &str, existing: &str) -> Self {
        let node = self.node(existing);
        let parent = self.ensure_dir(dir_name(path));
        let mut state = self.state.borrow_mut();
        state.add_entry(parent, leaf_name(path), node);
        state.data_mut(node).nlink += 1;
        drop(state);
        self
    }

    /// The directory `path`, made if it is missing, has no identity, as a
    /// node of a volume that gives none.
    #[must_use]
    pub fn unidentified(self, path: &str) -> Self {
        let node = self.ensure_dir(path);
        self.state.borrow_mut().unidentified.push(node);
        self
    }

    /// The directory `path`, made if it is missing, a volume of its own and
    /// listed with no identity, as a volume attached at a mount point is.
    #[must_use]
    pub fn mount_point(self, path: &str) -> Self {
        let tree = self.volume(path);
        let node = tree.ensure_dir(path);
        tree.state.borrow_mut().covered.push(node);
        tree
    }

    /// The root spelling `root` (`id::<volume-id>`, `Home:/`) names the
    /// directory `path`, as the system's resolver names a volume or an alias.
    #[must_use]
    pub fn named_root(self, root: &str, path: &str) -> Self {
        let node = self.ensure_dir(path);
        let spelled = String::from(root.trim_end_matches('/'));
        self.state.borrow_mut().roots.push((spelled, node));
        self
    }

    /// `path` gated by `cap`, kept as [`gate_of`] reads one.
    #[must_use]
    pub fn gated(self, path: &str, cap: CapabilityId) -> Self {
        self.attr(path, GATE, &cap.as_u16().to_le_bytes())
    }

    /// The gate `path` carries, kept as [`set_gate`] keeps one.
    #[must_use]
    pub fn gate(&self, path: &str) -> Option<CapabilityId> {
        let raw = self.attr_of(path, GATE)?;
        let raw = <[u8; 2]>::try_from(raw.as_slice()).ok()?;
        CapabilityId::from_raw(u16::from_le_bytes(raw)).ok()
    }

    /// Creates through the seam take `mask` from the mode they name.
    #[must_use]
    pub fn with_umask(self, mask: u32) -> Self {
        self.state.borrow_mut().umask = mask;
        self
    }

    /// `path` holding the mode bits `mode`.
    #[must_use]
    pub fn with_mode(self, path: &str, mode: u32) -> Self {
        self.chmod(path, mode);
        self
    }

    /// `path` belonging to `uid` and `gid`.
    #[must_use]
    pub fn owned(self, path: &str, uid: u32, gid: u32) -> Self {
        self.chown(path, uid, gid);
        self
    }

    /// The caller may give a node away.
    #[must_use]
    pub fn privileged(self) -> Self {
        self.state.borrow_mut().privileged = true;
        self
    }

    /// Check each permission class's bits as the module says.
    #[must_use]
    pub fn checking_permissions(self) -> Self {
        self.state.borrow_mut().permissions = true;
        self
    }

    /// `path` carrying the attribute `key` set to `value`.
    #[must_use]
    pub fn attr(self, path: &str, key: &str, value: &[u8]) -> Self {
        let node = self.node(path);
        let mut state = self.state.borrow_mut();
        let attrs = &mut state.data_mut(node).attrs;
        attrs.retain(|(held, _)| held != key);
        attrs.push((String::from(key), value.to_vec()));
        drop(state);
        self
    }

    /// Setting the attribute `key` anywhere answers `errno`.
    #[must_use]
    pub fn refuses_attr(self, key: &str, errno: Errno) -> Self {
        self.state
            .borrow_mut()
            .attr_refusals
            .push((String::from(key), errno));
        self
    }

    /// The directory `path`, made if it is missing, and everything beneath
    /// it are a volume of their own: a rename or link across it answers
    /// [`Errno::CrossVolume`].
    #[must_use]
    pub fn volume(self, path: &str) -> Self {
        let node = self.ensure_dir(path);
        let mut state = self.state.borrow_mut();
        let volume = state.next_volume;
        state.next_volume += 1;
        state.retag(node, volume);
        drop(state);
        self
    }

    /// Every seam call whose [`calls`](Self::calls) line is `call` (`"unlink
    /// /d/a"`, `"rename /a -> /b"`) answers `errno` instead of acting; it is
    /// still counted and listed. A data call, which is neither, answers it
    /// when its line would be `call`: `"read /f"`, `"write /f"`, `"set_len
    /// /f"`, `"next_region /f"`, `"size /f"`, by its file's path.
    #[must_use]
    pub fn refuse(self, call: &str, errno: Errno) -> Self {
        self.state
            .borrow_mut()
            .refusals
            .push((String::from(call), errno));
        self
    }

    /// Move the name `from` to `to`, replacing what `to` names, past the hook.
    pub fn rename(&self, from: &str, to: &str) {
        let source = self.parent_dir(from);
        let target = self.parent_dir(to);
        let mut state = self.state.borrow_mut();
        let node = state
            .lookup(source, leaf_name(from))
            .unwrap_or_else(|_| panic!("rename: {from} names nothing"));
        state.drop_entry(target, leaf_name(to));
        if let Body::Dir { entries, .. } = &mut state.data_mut(source).body {
            entries.retain(|(name, _)| name != leaf_name(from));
        }
        state.add_entry(target, leaf_name(to), node);
        if let Body::Dir { parent, .. } = &mut state.data_mut(node).body {
            *parent = target;
        }
    }

    /// Drop the name `path`, a directory with all beneath it, past the hook.
    pub fn remove(&self, path: &str) {
        let parent = self.parent_dir(path);
        self.state
            .borrow_mut()
            .drop_entry(parent, leaf_name(path))
            .unwrap_or_else(|| panic!("remove: {path} names nothing"));
    }

    /// Put a link storing `target` at `path`, in place of what it names, past
    /// the hook.
    pub fn relink(&self, path: &str, target: &str) {
        let parent = self.ensure_dir(dir_name(path));
        let mut state = self.state.borrow_mut();
        state.drop_entry(parent, leaf_name(path));
        state.insert(
            parent,
            leaf_name(path),
            Body::Link {
                target: String::from(target),
            },
            FS_LINK_MODE,
            false,
        );
    }

    /// The file `path` holds `bytes` from now, made if it is missing, past the
    /// hook.
    pub fn write(&self, path: &str, bytes: &[u8]) {
        let parent = self.parent_dir(path);
        let mut state = self.state.borrow_mut();
        let found = state.lookup(parent, leaf_name(path));
        let Ok(node) = found else {
            state.insert(
                parent,
                leaf_name(path),
                Body::File {
                    bytes: bytes.to_vec(),
                    runs: None,
                },
                FILE_MODE,
                false,
            );
            return;
        };
        let now = state.tick();
        state.generation += 1;
        let generation = state.generation;
        let data = state.data_mut(node);
        match &mut data.body {
            Body::File { bytes: held, runs } => {
                *held = bytes.to_vec();
                *runs = None;
            }
            _ => panic!("write: {path} is no file"),
        }
        data.content_gen = generation;
        data.modified = now;
        data.changed = now;
    }

    /// `path` holds the mode bits `mode` from now, past the hook.
    pub fn chmod(&self, path: &str, mode: u32) {
        let node = self.node(path);
        let mut state = self.state.borrow_mut();
        let now = state.tick();
        let data = state.data_mut(node);
        data.mode = mode & FS_MODE_MASK;
        data.changed = now;
    }

    /// `path` belongs to `uid` and `gid` from now, past the hook.
    pub fn chown(&self, path: &str, uid: u32, gid: u32) {
        let node = self.node(path);
        let mut state = self.state.borrow_mut();
        let now = state.tick();
        let data = state.data_mut(node);
        data.uid = uid;
        data.gid = gid;
        data.changed = now;
    }

    /// The mode bits of what `path` names, a final link not followed.
    #[must_use]
    pub fn mode(&self, path: &str) -> Option<u32> {
        self.read(path, |data| data.mode & FS_MODE_MASK)
    }

    /// The bytes of the file `path` names.
    #[must_use]
    pub fn bytes(&self, path: &str) -> Option<Vec<u8>> {
        self.read(path, |data| match &data.body {
            Body::File { bytes, .. } => Some(bytes.clone()),
            _ => None,
        })
        .flatten()
    }

    /// What `path` names, a final link not followed.
    #[must_use]
    pub fn kind(&self, path: &str) -> Option<Kind> {
        self.read(path, |data| match data.body {
            Body::File { .. } => Kind::File,
            Body::Dir { .. } => Kind::Directory,
            Body::Link { .. } => Kind::Link,
        })
    }

    /// Whether `path` names anything, a link included.
    #[must_use]
    pub fn exists(&self, path: &str) -> bool {
        self.kind(path).is_some()
    }

    /// Who owns what `path` names: its user and group.
    #[must_use]
    pub fn owner(&self, path: &str) -> Option<(u32, u32)> {
        self.read(path, |data| (data.uid, data.gid))
    }

    /// The value of the attribute `key` on what `path` names.
    #[must_use]
    pub fn attr_of(&self, path: &str, key: &str) -> Option<Vec<u8>> {
        self.read(path, |data| {
            data.attrs
                .iter()
                .find(|(held, _)| held == key)
                .map(|(_, value)| value.clone())
        })
        .flatten()
    }

    /// The target the link `path` stores.
    #[must_use]
    pub fn link_target(&self, path: &str) -> Option<String> {
        self.read(path, |data| match &data.body {
            Body::Link { target } => Some(target.clone()),
            _ => None,
        })
        .flatten()
    }

    /// The identity of what `path` names, a final link not followed.
    #[must_use]
    pub fn id(&self, path: &str) -> Option<FileId> {
        let state = self.state.borrow();
        let node = state.resolve(path, false, false).ok()?;
        Some(state.stat(node).id)
    }

    /// The bytes written through the seam to the file `path` names.
    #[must_use]
    pub fn written(&self, path: &str) -> usize {
        self.read(path, |data| data.written).unwrap_or(0)
    }

    /// Each node made through the seam, by path, with its mode as a byte was
    /// written to it or an entry made in it.
    #[must_use]
    pub fn fill_modes(&self) -> Vec<(String, u32)> {
        self.state.borrow().fill_modes.clone()
    }

    /// Every path the tree holds, sorted.
    #[must_use]
    pub fn paths(&self) -> Vec<String> {
        let state = self.state.borrow();
        let mut paths = Vec::new();
        let mut pending = alloc::vec![(String::from("/"), ROOT)];
        while let Some((path, node)) = pending.pop() {
            if let Ok(entries) = state.entries(node) {
                for (name, child) in entries {
                    pending.push((join(&path, name), *child));
                }
            }
            paths.push(path);
        }
        paths.sort();
        paths
    }

    /// `path` as the kernel resolves it, every link on the way and at its end
    /// followed.
    ///
    /// # Errors
    ///
    /// The resolution's refusal.
    pub fn canonical(&self, path: &str) -> Result<String, Errno> {
        let state = self.state.borrow();
        let node = state.resolve(path, true, true)?;
        Ok(state.path(node))
    }

    /// One line per seam call made so far, `"<method> <path>"`.
    #[must_use]
    pub fn calls(&self) -> Vec<String> {
        self.state.borrow().calls.clone()
    }

    /// Run `hook` once, immediately before seam call `n` (counting from 0).
    pub fn before_call(&self, n: usize, hook: Box<dyn FnOnce(&Tree)>) {
        self.state.borrow_mut().hooks.push((When::Call(n), hook));
    }

    /// Run `hook` once, immediately before the first seam call listed as
    /// `line`.
    pub fn before_line(&self, line: &str, hook: Box<dyn FnOnce(&Tree)>) {
        self.state
            .borrow_mut()
            .hooks
            .push((When::Line(String::from(line)), hook));
    }

    /// The file `path` names, for reading and, when `write`, writing too, a
    /// final link followed only when `follow`: the open a tool makes of an
    /// operand it reads through its link.
    ///
    /// # Errors
    ///
    /// As [`Dir::open_file`].
    pub fn open_file(&self, path: &str, follow: bool, write: bool) -> Result<Handle, Errno> {
        self.begin_path("open_file", path)?;
        let state = self.state.borrow();
        let node = state.resolve(path, follow, true)?;
        let opened = opening(&state, node, write);
        drop(state);
        opened.map(|()| self.handle(node, true, write))
    }

    fn handle(&self, node: usize, read: bool, write: bool) -> Handle {
        Handle {
            tree: self.clone(),
            node,
            read,
            write,
        }
    }

    fn node(&self, path: &str) -> usize {
        self.state
            .borrow()
            .resolve(path, false, false)
            .unwrap_or_else(|_| panic!("{path} names nothing"))
    }

    fn parent_dir(&self, path: &str) -> usize {
        self.node(dir_name(path))
    }

    fn read<T>(&self, path: &str, with: impl FnOnce(&NodeData) -> T) -> Option<T> {
        let state = self.state.borrow();
        let node = state.resolve(path, false, false).ok()?;
        Some(with(state.data(node)))
    }

    fn ensure_dir(&self, path: &str) -> usize {
        if entry_name(path).is_none() {
            return ROOT;
        }
        if let Ok(node) = self.state.borrow().resolve(path, true, false) {
            return node;
        }
        let parent = self.ensure_dir(dir_name(path));
        self.state.borrow_mut().insert(
            parent,
            leaf_name(path),
            Body::Dir {
                entries: Vec::new(),
                parent,
                removed: false,
            },
            DIR_MODE,
            false,
        )
    }

    fn put(&self, path: &str, body: Body, mode: u32) {
        let parent = self.ensure_dir(dir_name(path));
        let mut state = self.state.borrow_mut();
        state.drop_entry(parent, leaf_name(path));
        state.insert(parent, leaf_name(path), body, mode, false);
    }

    /// Count a call listed as `line`, running a hook due before it.
    fn count(&self, line: String) {
        let due: Vec<Hook> = {
            let mut state = self.state.borrow_mut();
            let n = state.calls.len();
            let mut due = Vec::new();
            let mut index = 0;
            while index < state.hooks.len() {
                let hit = match &state.hooks[index].0 {
                    When::Call(at) => *at == n,
                    When::Line(listed) => *listed == line,
                };
                if hit {
                    due.push(state.hooks.remove(index).1);
                } else {
                    index += 1;
                }
            }
            state.calls.push(line);
            due
        };
        for hook in due {
            hook(self);
        }
    }

    /// Count a call on a held node, `name` beneath it when given, unless a
    /// refusal answers it.
    fn begin(&self, method: &str, node: usize, name: Option<&str>) -> Result<(), Errno> {
        let subject = self.subject(node, name);
        self.listed(&format!("{method} {subject}"))
    }

    fn begin_path(&self, method: &str, path: &str) -> Result<(), Errno> {
        self.listed(&format!("{method} {path}"))
    }

    fn listed(&self, line: &str) -> Result<(), Errno> {
        self.count(String::from(line));
        self.state.borrow().refused(line)
    }

    fn subject(&self, node: usize, name: Option<&str>) -> String {
        let path = self.state.borrow().path(node);
        match name {
            Some(name) => join(&path, name),
            None => path,
        }
    }

    /// The refusal of the data call `method` on `node`.
    fn data_failure(&self, method: &str, node: usize) -> Result<(), Errno> {
        let state = self.state.borrow();
        let line = format!("{method} {}", state.path(node));
        state.refused(&line)
    }
}

/// Whether an open for reading, and writing when `write`, may have `node`.
fn opening(state: &State, node: usize, write: bool) -> Result<(), Errno> {
    match state.data(node).body {
        Body::Link { .. } => return Err(Errno::LinkLoop),
        Body::Dir { .. } => return Err(Errno::IsADirectory),
        Body::File { .. } => {}
    }
    state.require(node, if write { WRITE } else { READ })
}

impl Roots for Tree {
    type Dir = Handle;

    fn open_dir(&self, path: &str, follow: bool) -> Result<Handle, Errno> {
        self.begin_path("open_dir", path)?;
        let node = self.state.borrow().resolve(path, follow, true)?;
        if !self.state.borrow().is_dir(node) {
            return Err(Errno::NotADirectory);
        }
        Ok(self.handle(node, false, false))
    }

    fn parent(&self, path: &str) -> Result<(Handle, String), Errno> {
        self.begin_path("parent", path)?;
        let name = entry_name(path).ok_or(Errno::OutOfRange)?;
        let node = self.state.borrow().resolve(dir_name(path), true, true)?;
        if !self.state.borrow().is_dir(node) {
            return Err(Errno::NotADirectory);
        }
        Ok((self.handle(node, false, false), String::from(name)))
    }

    fn open_node(&self, path: &str, follow: bool) -> Result<Handle, Errno> {
        self.begin_path("open_node", path)?;
        let node = self.state.borrow().resolve(path, follow, true)?;
        Ok(self.handle(node, false, false))
    }
}

impl Handle {
    fn state(&self) -> core::cell::Ref<'_, State> {
        self.tree.state.borrow()
    }

    fn state_mut(&self) -> core::cell::RefMut<'_, State> {
        self.tree.state.borrow_mut()
    }

    /// The child `name` of this directory, after search permission.
    fn child(&self, name: &str) -> Result<usize, Errno> {
        let state = self.state();
        state.require(self.node, SEARCH)?;
        state.lookup(self.node, name)
    }

    /// Whether a name may be made or dropped here: a live directory the
    /// caller may write and search.
    fn writable(&self) -> Result<(), Errno> {
        let state = self.state();
        state.entries(self.node)?;
        if state.removed(self.node) {
            return Err(Errno::NotFound);
        }
        state.require(self.node, WRITE | SEARCH)
    }

    fn make(&self, name: &str, body: Body, mode: u32) -> Result<usize, Errno> {
        self.writable()?;
        if self.state().lookup(self.node, name).is_ok() {
            return Err(Errno::AlreadyExists);
        }
        let mut state = self.state_mut();
        let mut mode = mode & !state.umask;
        // As the kernel creates: a setgid directory gives its group, and a new
        // directory its bit, to what is made in it.
        let parent = state.data(self.node);
        let inherited = (parent.mode & FS_SETGID_BIT != 0).then_some(parent.gid);
        let directory = matches!(body, Body::Dir { .. });
        if inherited.is_some() && directory {
            mode |= FS_SETGID_BIT;
        }
        let node = state.insert(self.node, name, body, mode, true);
        if let Some(gid) = inherited {
            state.data_mut(node).gid = gid;
        }
        Ok(node)
    }

    fn owns(&self) -> bool {
        self.state().data(self.node).uid == USER.0
    }

    /// Check `expect` against what `node` is, as `EXPECT_NODE` (`node_only`)
    /// or `EXPECT_UNCHANGED` asks.
    fn expected(
        &self,
        node: usize,
        asked: bool,
        node_only: bool,
        expect: Option<&NodeExpect>,
    ) -> Result<(), Errno> {
        if !asked {
            return Ok(());
        }
        let expect = expect.ok_or(Errno::OutOfRange)?;
        if node_only && !expect.names_only_the_node() {
            return Err(Errno::OutOfRange);
        }
        let stat = self.state().stat(node);
        let met = if node_only {
            expect.id == stat.id
        } else {
            *expect == NodeExpect::of(&stat)
        };
        if met {
            Ok(())
        } else {
            Err(Errno::Stale)
        }
    }

    fn touch(&self) {
        let mut state = self.state_mut();
        let now = state.tick();
        state.data_mut(self.node).changed = now;
    }
}

impl Node for Handle {
    fn stat(&self) -> Result<FileStat, Errno> {
        self.tree.begin("stat", self.node, None)?;
        Ok(self.state().stat(self.node))
    }

    fn set_mode(&self, mode: u32) -> Result<(), Errno> {
        self.tree.begin("set_mode", self.node, None)?;
        if !self.owns() {
            return Err(Errno::PermissionDenied);
        }
        self.state_mut().data_mut(self.node).mode = mode & FS_MODE_MASK;
        self.touch();
        Ok(())
    }

    fn set_owner(&self, uid: u32, gid: u32) -> Result<(), Errno> {
        self.tree.begin("set_owner", self.node, None)?;
        let (privileged, held, is_dir) = {
            let state = self.state();
            let data = state.data(self.node);
            (
                state.privileged,
                (data.uid, data.gid),
                state.is_dir(self.node),
            )
        };
        let kept = |asked: u32, current: u32| {
            if asked == FS_OWNER_UNCHANGED {
                current
            } else {
                asked
            }
        };
        let wanted = (kept(uid, held.0), kept(gid, held.1));
        if wanted == held {
            return Ok(());
        }
        // As the kernel has it: only the privileged give a node away, and
        // its own owner may move it only into the owner's group.
        let own_group = held.0 == USER.0 && wanted == USER;
        if !privileged && !own_group {
            return Err(Errno::PermissionDenied);
        }
        {
            let mut state = self.state_mut();
            let data = state.data_mut(self.node);
            (data.uid, data.gid) = wanted;
            data.mode = mode_after_owner_change(data.mode, is_dir);
        }
        self.touch();
        Ok(())
    }

    fn attr_keys(&self) -> Result<Vec<String>, Errno> {
        self.tree.begin("attr_keys", self.node, None)?;
        Ok(self
            .state()
            .data(self.node)
            .attrs
            .iter()
            .map(|(key, _)| key.clone())
            .collect())
    }

    fn attr_value(&self, key: &str) -> Result<Vec<u8>, Errno> {
        self.tree.begin("attr_value", self.node, None)?;
        self.state()
            .data(self.node)
            .attrs
            .iter()
            .find(|(held, _)| held == key)
            .map(|(_, value)| value.clone())
            .ok_or(Errno::NoData)
    }

    fn attr_set(&self, key: &str, value: &[u8]) -> Result<(), Errno> {
        self.tree.begin("attr_set", self.node, None)?;
        {
            let state = self.state();
            if let Some(&(_, errno)) = state.attr_refusals.iter().find(|(held, _)| held == key) {
                return Err(errno);
            }
            if state.data(self.node).uid != USER.0 {
                state.require(self.node, WRITE)?;
            }
        }
        {
            let mut state = self.state_mut();
            let attrs = &mut state.data_mut(self.node).attrs;
            attrs.retain(|(held, _)| held != key);
            attrs.push((String::from(key), value.to_vec()));
        }
        self.touch();
        Ok(())
    }

    fn attr_remove(&self, key: &str) -> Result<(), Errno> {
        self.tree.begin("attr_remove", self.node, None)?;
        let removed = {
            let mut state = self.state_mut();
            let attrs = &mut state.data_mut(self.node).attrs;
            let before = attrs.len();
            attrs.retain(|(held, _)| held != key);
            attrs.len() != before
        };
        if !removed {
            return Err(Errno::NoData);
        }
        self.touch();
        Ok(())
    }

    fn sync(&self) -> Result<(), Errno> {
        self.tree.begin("sync", self.node, None)
    }
}

impl Dir for Handle {
    type File = Self;

    fn entries(&self) -> Result<Vec<Entry>, Errno> {
        self.tree.begin("entries", self.node, None)?;
        let state = self.state();
        let entries = state.entries(self.node)?;
        if state.data(self.node).uid != USER.0 {
            state.require(self.node, READ)?;
        }
        if state.removed(self.node) {
            return Ok(Vec::new());
        }
        Ok(entries
            .iter()
            .map(|(name, child)| Entry {
                name: name.clone(),
                kind: match state.data(*child).body {
                    Body::File { .. } => Kind::File,
                    Body::Dir { .. } => Kind::Directory,
                    Body::Link { .. } => Kind::Link,
                },
                id: if state.covered.contains(child) {
                    FileId::NONE
                } else {
                    state.id(*child)
                },
                nlink: state.stat(*child).nlink,
            })
            .collect())
    }

    fn open_dir(&self, name: &str) -> Result<Self, Errno> {
        self.tree.begin("open_dir", self.node, Some(name))?;
        let child = self.child(name)?;
        if !self.state().is_dir(child) {
            return Err(Errno::NotADirectory);
        }
        Ok(self.tree.handle(child, false, false))
    }

    fn enter(&self, name: &str) -> Result<Self, Errno> {
        self.tree.begin("enter", self.node, Some(name))?;
        let node = self.state().resolve_from(self.node, name, true, true)?;
        if !self.state().is_dir(node) {
            return Err(Errno::NotADirectory);
        }
        Ok(self.tree.handle(node, false, false))
    }

    fn open_file(&self, name: &str, write: bool) -> Result<Self, Errno> {
        self.tree.begin("open_file", self.node, Some(name))?;
        let child = self.child(name)?;
        opening(&self.state(), child, write)?;
        Ok(self.tree.handle(child, !write, write))
    }

    fn follow(&self, name: &str, read: bool) -> Result<Self, Errno> {
        self.tree.begin("follow", self.node, Some(name))?;
        let node = self.state().resolve_from(self.node, name, true, true)?;
        if read {
            opening(&self.state(), node, false)?;
        }
        Ok(self.tree.handle(node, read, false))
    }

    fn open_node(&self, name: &str) -> Result<Self, Errno> {
        self.tree.begin("open_node", self.node, Some(name))?;
        let child = self.child(name)?;
        Ok(self.tree.handle(child, false, false))
    }

    fn create_file(&self, name: &str, mode: u32) -> Result<Self, Errno> {
        self.tree.begin("create_file", self.node, Some(name))?;
        let body = Body::File {
            bytes: Vec::new(),
            runs: None,
        };
        let node = self.make(name, body, mode)?;
        Ok(self.tree.handle(node, false, true))
    }

    fn make_dir(&self, name: &str, mode: u32) -> Result<Self, Errno> {
        self.tree.begin("make_dir", self.node, Some(name))?;
        let body = Body::Dir {
            entries: Vec::new(),
            parent: self.node,
            removed: false,
        };
        let node = self.make(name, body, mode)?;
        Ok(self.tree.handle(node, false, false))
    }

    fn symlink(&self, target: &[u8], name: &str) -> Result<(), Errno> {
        self.tree.begin("symlink", self.node, Some(name))?;
        let target = core::str::from_utf8(target).map_err(|_| Errno::OutOfRange)?;
        let body = Body::Link {
            target: String::from(target),
        };
        let node = self.make(name, body, FS_LINK_MODE)?;
        self.state_mut().data_mut(node).mode = FS_LINK_MODE;
        Ok(())
    }

    fn readlink(&self, name: &str) -> Result<String, Errno> {
        self.tree.begin("readlink", self.node, Some(name))?;
        let child = self.child(name)?;
        match &self.state().data(child).body {
            Body::Link { target } => Ok(target.clone()),
            _ => Err(Errno::OutOfRange),
        }
    }

    fn unlink(
        &self,
        name: &str,
        flags: UnlinkFlags,
        expect: Option<&NodeExpect>,
    ) -> Result<(), Errno> {
        self.tree.begin("unlink", self.node, Some(name))?;
        self.writable()?;
        let child = self.state().lookup(self.node, name)?;
        self.expected(
            child,
            flags.expects(),
            flags.contains(UnlinkFlags::EXPECT_NODE),
            expect,
        )?;
        {
            let state = self.state();
            match state.entries(child) {
                Ok(entries) if !entries.is_empty() => return Err(Errno::NotEmpty),
                Err(_) if flags.is_directory_only() => return Err(Errno::NotADirectory),
                _ => {}
            }
        }
        self.state_mut().drop_entry(self.node, name);
        Ok(())
    }

    fn rename(
        &self,
        from: &str,
        to_dir: &Self,
        to: &str,
        flags: RenameFlags,
        expect: Option<&NodeExpect>,
    ) -> Result<(), Errno> {
        let source = self.tree.subject(self.node, Some(from));
        let target = to_dir.tree.subject(to_dir.node, Some(to));
        self.tree.listed(&format!("rename {source} -> {target}"))?;
        self.writable()?;
        to_dir.writable()?;
        let moving = self.state().lookup(self.node, from)?;
        self.expected(moving, flags.expects(), true, expect)?;
        let state = self.state();
        if state.data(self.node).volume != state.data(to_dir.node).volume {
            return Err(Errno::CrossVolume);
        }
        let occupant = state.lookup(to_dir.node, to).ok();
        if occupant == Some(moving) {
            return Ok(());
        }
        if occupant.is_some() && flags.refuses_replace() {
            return Err(Errno::AlreadyExists);
        }
        let moving_dir = state.is_dir(moving);
        if moving_dir && state.within(to_dir.node, moving) {
            return Err(Errno::OutOfRange);
        }
        if let Some(occupant) = occupant {
            match (moving_dir, state.entries(occupant)) {
                (true, Ok(entries)) if !entries.is_empty() => return Err(Errno::NotEmpty),
                (true, Err(_)) => return Err(Errno::NotADirectory),
                (false, Ok(_)) => return Err(Errno::IsADirectory),
                _ => {}
            }
        }
        drop(state);
        let mut state = self.state_mut();
        state.drop_entry(to_dir.node, to);
        if let Body::Dir { entries, .. } = &mut state.data_mut(self.node).body {
            entries.retain(|(name, _)| name != from);
        }
        state.add_entry(to_dir.node, to, moving);
        if let Body::Dir { parent, .. } = &mut state.data_mut(moving).body {
            *parent = to_dir.node;
        }
        Ok(())
    }

    fn link(&self, existing: &str, to_dir: &Self, new: &str, follow: bool) -> Result<(), Errno> {
        let source = self.tree.subject(self.node, Some(existing));
        let target = to_dir.tree.subject(to_dir.node, Some(new));
        self.tree.listed(&format!("link {source} -> {target}"))?;
        to_dir.writable()?;
        let node = if follow {
            self.state().resolve_from(self.node, existing, true, true)?
        } else {
            self.child(existing)?
        };
        {
            let state = self.state();
            if state.is_dir(node) {
                return Err(Errno::PermissionDenied);
            }
            if state.data(node).volume != state.data(to_dir.node).volume {
                return Err(Errno::CrossVolume);
            }
            if state.lookup(to_dir.node, new).is_ok() {
                return Err(Errno::AlreadyExists);
            }
        }
        let mut state = self.state_mut();
        state.add_entry(to_dir.node, new, node);
        state.data_mut(node).nlink += 1;
        Ok(())
    }

    fn access(&self, name: &str, want: Access) -> Result<(), Errno> {
        self.tree.begin("access", self.node, Some(name))?;
        let child = self.child(name)?;
        let bits = [
            (Access::READ, READ),
            (Access::WRITE, WRITE),
            (Access::EXEC, SEARCH),
        ]
        .iter()
        .filter(|(asked, _)| want.contains(*asked))
        .fold(0, |bits, (_, bit)| bits | bit);
        self.state().require(child, bits)
    }
}

impl Handle {
    fn file_bytes<T>(
        &self,
        with: impl FnOnce(&[u8], Option<&[(u64, u64)]>) -> T,
    ) -> Result<T, Errno> {
        match &self.state().data(self.node).body {
            Body::File { bytes, runs } => Ok(with(bytes, runs.as_deref())),
            Body::Dir { .. } => Err(Errno::IsADirectory),
            Body::Link { .. } => Err(Errno::LinkLoop),
        }
    }

    fn changed_bytes(&self, change: impl FnOnce(&mut Vec<u8>)) -> Result<(), Errno> {
        if !self.write {
            return Err(Errno::PermissionDenied);
        }
        let mut state = self.state_mut();
        if state.data(self.node).watched {
            let path = state.path(self.node);
            let mode = state.data(self.node).mode;
            state.fill_modes.push((path, mode));
        }
        let now = state.tick();
        state.generation += 1;
        let generation = state.generation;
        let data = state.data_mut(self.node);
        match &mut data.body {
            Body::File { bytes, runs } => {
                change(bytes);
                *runs = None;
            }
            Body::Dir { .. } => return Err(Errno::IsADirectory),
            Body::Link { .. } => return Err(Errno::LinkLoop),
        }
        data.content_gen = generation;
        data.modified = now;
        data.changed = now;
        Ok(())
    }
}

impl Source for Handle {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        self.tree.data_failure("read", self.node)?;
        if !self.read {
            return Err(Errno::PermissionDenied);
        }
        self.file_bytes(|bytes, _| {
            let start = usize::try_from(offset).unwrap_or(usize::MAX);
            let Some(rest) = bytes.get(start..) else {
                return 0;
            };
            let take = rest.len().min(buf.len());
            buf[..take].copy_from_slice(&rest[..take]);
            take
        })
    }

    fn next_region(&mut self, offset: u64, region: SeekRegion) -> Result<Option<u64>, Errno> {
        self.tree.data_failure("next_region", self.node)?;
        self.file_bytes(|bytes, runs| {
            let len = bytes.len() as u64;
            let Some(runs) = runs else {
                return region.in_solid_file(offset, len);
            };
            if offset >= len {
                return None;
            }
            let inside = runs
                .iter()
                .find(|(start, end)| (*start..*end).contains(&offset));
            match (region, inside) {
                (SeekRegion::Data, Some(_)) | (SeekRegion::Hole, None) => Some(offset),
                (SeekRegion::Hole, Some(&(_, end))) => Some(end),
                (SeekRegion::Data, None) => runs
                    .iter()
                    .map(|&(start, _)| start)
                    .find(|&start| start > offset),
            }
        })
    }

    fn size(&mut self) -> Result<u64, Errno> {
        self.tree.data_failure("size", self.node)?;
        self.file_bytes(|bytes, _| bytes.len() as u64)
    }
}

impl Target for Handle {
    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> Result<(), Errno> {
        self.tree.data_failure("write", self.node)?;
        let start = usize::try_from(offset).map_err(|_| Errno::LengthOutOfRange)?;
        self.changed_bytes(|held| {
            let end = start + bytes.len();
            if held.len() < end {
                held.resize(end, 0);
            }
            held[start..end].copy_from_slice(bytes);
        })?;
        self.state_mut().data_mut(self.node).written += bytes.len();
        Ok(())
    }

    fn set_len(&mut self, len: u64) -> Result<(), Errno> {
        self.tree.data_failure("set_len", self.node)?;
        let len = usize::try_from(len).map_err(|_| Errno::LengthOutOfRange)?;
        self.changed_bytes(|held| held.resize(len, 0))
    }
}

/// `path` resolved as the kernel resolves it, in miniature: each component a
/// link (`link_of` answering its target) replaced by that target — absolute,
/// or relative to the link's directory — and each `..` climbing what was
/// walked.
///
/// # Errors
///
/// [`Errno::LinkLoop`] for a cycle of links.
pub fn resolve(path: &str, link_of: impl Fn(&str) -> Option<String>) -> Result<String, Errno> {
    let mut walked: Vec<String> = Vec::new();
    let mut pending: Vec<String> = path.split('/').rev().map(String::from).collect();
    let mut hops = 0;
    while let Some(part) = pending.pop() {
        match part.as_str() {
            "" | "." => continue,
            ".." => {
                walked.pop();
                continue;
            }
            _ => {}
        }
        if let Some(target) = link_of(&spelled(walked.iter().chain([&part]))) {
            hops += 1;
            if hops > MAX_HOPS {
                return Err(Errno::LinkLoop);
            }
            if target.starts_with('/') {
                walked.clear();
            }
            pending.extend(target.split('/').rev().map(String::from));
            continue;
        }
        walked.push(part);
    }
    Ok(spelled(walked.iter()))
}

/// The absolute path `components` spell, `/` for none.
fn spelled<'a>(components: impl Iterator<Item = &'a String>) -> String {
    let mut path = String::new();
    for component in components {
        path.push('/');
        path.push_str(component);
    }
    if path.is_empty() {
        path.push('/');
    }
    path
}

#[cfg(test)]
#[path = "fixture_tests.rs"]
mod tests;
