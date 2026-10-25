//! The one way a first-party tool walks a tree: through directories it holds
//! open, reaching each child by its name from there, never by a joined path
//! and never through a link it was not asked to follow. A name swapped for a
//! link between a tool's reading it and its changing it therefore leads
//! nowhere new: what is done through a held node is done to that node.

use alloc::string::String;
use alloc::vec::Vec;
use core::borrow::Borrow;
use core::fmt;

use tairix_abi::{
    Access, Errno, FileId, FileKind, FileStat, NodeExpect, RenameFlags, UnlinkFlags, FS_NAME_MAX,
};
use tairix_path::{leaf_name, push_name, root_len};
use tairix_util::quote::quote;

use crate::data::{Source, Target};

/// What a name is, read without following it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Kind {
    /// A directory.
    Directory,
    /// A regular file.
    File,
    /// A symbolic link, the link itself.
    Link,
}

impl Kind {
    /// The kind `kind` names.
    #[must_use]
    pub const fn of(kind: FileKind) -> Self {
        match kind {
            FileKind::Directory => Self::Directory,
            FileKind::Regular => Self::File,
            FileKind::Symlink => Self::Link,
        }
    }
}

/// One entry of a held directory, as its listing read it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    /// Its name in the directory.
    pub name: String,
    /// What it is, not followed.
    pub kind: Kind,
    /// The identity it was listed with.
    pub id: FileId,
    /// How many names it had when listed.
    pub nlink: u32,
}

/// A node a tool holds open: what is done through it is done to that node,
/// whatever its name has come to name since.
pub trait Node {
    /// The node's metadata.
    ///
    /// # Errors
    ///
    /// The stat's refusal.
    fn stat(&self) -> Result<FileStat, Errno>;

    /// Give the node the mode bits `mode`.
    ///
    /// # Errors
    ///
    /// The change's refusal.
    fn set_mode(&self, mode: u32) -> Result<(), Errno>;

    /// Give the node to `uid` and `gid`.
    ///
    /// # Errors
    ///
    /// The change's refusal.
    fn set_owner(&self, uid: u32, gid: u32) -> Result<(), Errno>;

    /// The keys of the node's extended attributes.
    ///
    /// # Errors
    ///
    /// [`Errno::NotSupported`] where its volume keeps none, or the listing's
    /// refusal.
    fn attr_keys(&self) -> Result<Vec<String>, Errno>;

    /// The value of the node's attribute `key`.
    ///
    /// # Errors
    ///
    /// [`Errno::NoData`] for a key it no longer carries, or the read's
    /// refusal.
    fn attr_value(&self, key: &str) -> Result<Vec<u8>, Errno>;

    /// Set the node's attribute `key` to `value`.
    ///
    /// # Errors
    ///
    /// The volume's refusal: [`Errno::NotSupported`] for a key it keeps no
    /// attribute of, [`Errno::OutOfRange`] for a value it cannot represent,
    /// [`Errno::NoSpace`] or [`Errno::LengthOutOfRange`] when the node has no
    /// reserved for it, [`Errno::PermissionDenied`] for a key the caller may not
    /// set.
    fn attr_set(&self, key: &str, value: &[u8]) -> Result<(), Errno>;

    /// Remove the node's attribute `key`.
    ///
    /// # Errors
    ///
    /// [`Errno::NoData`] when it carries none, or the volume's refusal.
    fn attr_remove(&self, key: &str) -> Result<(), Errno>;

    /// Put what was written to the node's volume on its medium.
    ///
    /// # Errors
    ///
    /// The commit's refusal.
    fn sync(&self) -> Result<(), Errno>;
}

/// A directory a tool holds open.
pub trait Dir: Node + Sized {
    /// A file reached beneath it; `Source` and `Target` are lib/fscopy's data
    /// seams.
    type File: Node + Source + Target;

    /// Every entry, read once in full, so a walk is not steered by what it
    /// changes.
    ///
    /// # Errors
    ///
    /// The listing's refusal.
    fn entries(&self) -> Result<Vec<Entry>, Errno>;

    /// The child directory `name`, never through a link (`NO_FOLLOW |
    /// DIRECTORY`).
    ///
    /// # Errors
    ///
    /// [`Errno::NotADirectory`] for a name holding anything else, a link
    /// included, or the open's refusal.
    fn open_dir(&self, name: &str) -> Result<Self, Errno>;

    /// The directory `name` names from here, through a final link as a path
    /// walk takes one it goes on beneath: where an operand's own path leads,
    /// never where a tree's walk descends.
    ///
    /// # Errors
    ///
    /// [`Errno::NotADirectory`] for a name reaching anything else, or the
    /// open's refusal.
    fn enter(&self, name: &str) -> Result<Self, Errno>;

    /// The child file `name`, for reading, or for writing alone when
    /// `write`, never through a link.
    ///
    /// # Errors
    ///
    /// [`Errno::LinkLoop`] for a link, [`Errno::IsADirectory`] for a
    /// directory, or the open's refusal.
    fn open_file(&self, name: &str, write: bool) -> Result<Self::File, Errno>;

    /// What `name` leads to from here through a final link, for reading when
    /// `read` and else with no access: where an operand's own path leads,
    /// never where a tree's walk goes.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] for a link naming nothing, [`Errno::IsADirectory`]
    /// for a directory opened for reading, or the open's refusal.
    fn follow(&self, name: &str, read: bool) -> Result<Self::File, Errno>;

    /// The node `name` names, a link itself included, opened with no read or
    /// write access: for its metadata and attributes.
    ///
    /// # Errors
    ///
    /// The open's refusal.
    fn open_node(&self, name: &str) -> Result<Self::File, Errno>;

    /// A new file `name` born with `mode` less the umask, never through a
    /// link.
    ///
    /// # Errors
    ///
    /// [`Errno::AlreadyExists`] for a name already taken, or the create's
    /// refusal.
    fn create_file(&self, name: &str, mode: u32) -> Result<Self::File, Errno>;

    /// A new directory `name` born with `mode` less the umask, answered open.
    ///
    /// # Errors
    ///
    /// [`Errno::AlreadyExists`] for a name already taken, or the create's
    /// refusal.
    fn make_dir(&self, name: &str, mode: u32) -> Result<Self, Errno>;

    /// A new symbolic link `name` storing `target` verbatim.
    ///
    /// # Errors
    ///
    /// [`Errno::AlreadyExists`] for a name already taken, or the create's
    /// refusal.
    fn symlink(&self, target: &[u8], name: &str) -> Result<(), Errno>;

    /// The target the link `name` stores, verbatim.
    ///
    /// # Errors
    ///
    /// The read's refusal.
    fn readlink(&self, name: &str) -> Result<String, Errno>;

    /// Remove `name` (a directory under `UnlinkFlags::DIRECTORY`), and only
    /// while it names what `expect` describes.
    ///
    /// # Errors
    ///
    /// [`Errno::Stale`] for a name no longer holding what `expect`
    /// describes, or the removal's refusal.
    fn unlink(
        &self,
        name: &str,
        flags: UnlinkFlags,
        expect: Option<&NodeExpect>,
    ) -> Result<(), Errno>;

    /// Move `from` to `to` in `to_dir`, `expect` describing the source.
    ///
    /// # Errors
    ///
    /// [`Errno::CrossVolume`] across volumes, [`Errno::Stale`] for a source
    /// no longer holding what `expect` describes, or the move's refusal.
    fn rename(
        &self,
        from: &str,
        to_dir: &Self,
        to: &str,
        flags: RenameFlags,
        expect: Option<&NodeExpect>,
    ) -> Result<(), Errno>;

    /// A second name `new` in `to_dir` for the node `existing` names here,
    /// a final link of `existing` followed when `follow`; `new` never is.
    ///
    /// # Errors
    ///
    /// The link's refusal.
    fn link(&self, existing: &str, to_dir: &Self, new: &str, follow: bool) -> Result<(), Errno>;

    /// Whether the caller may have `want` of `name`, a link asked about
    /// itself.
    ///
    /// # Errors
    ///
    /// [`Errno::PermissionDenied`] when it may not, or the query's refusal.
    fn access(&self, name: &str, want: Access) -> Result<(), Errno>;
}

/// The last component of `path` as a name its parent holds; `None` for a path
/// ending in a root, `.` or `..`, which no entry stands for.
#[must_use]
pub fn entry_name(path: &str) -> Option<&str> {
    let name = leaf_name(path);
    let named = !(name.is_empty()
        || name.ends_with('/')
        || name == "."
        || name == ".."
        || names_a_root(path));
    named.then_some(name)
}

/// Remove `name` from `dir` only while it holds the node `id`.
///
/// # Errors
///
/// [`Errno::Stale`] for a name holding another node, or the removal's
/// refusal.
pub fn unlink_node<D: Dir>(dir: &D, name: &str, id: FileId) -> Result<(), Errno> {
    dir.unlink(
        name,
        UnlinkFlags::EXPECT_NODE,
        Some(&NodeExpect::of_node(id)),
    )
}

/// Whether `path` spells only a root — the view's, an alias's, a volume's —
/// with nothing after it but `/` and `.`: a directory no other holds.
#[must_use]
pub fn names_a_root(path: &str) -> bool {
    root_len(path).is_ok_and(|len| {
        len > 0
            && path
                .get(len..)
                .is_some_and(|rest| rest.split('/').all(|step| matches!(step, "" | ".")))
    })
}

/// The view root `/` as a tool met it, so a recursive change refuses it
/// however an operand reaches it.
#[derive(Default)]
pub struct ViewRoot {
    id: Option<FileId>,
}

impl ViewRoot {
    /// Whether `path` spells the view root: `/`, then nothing but `/` and `.`.
    #[must_use]
    pub fn spelled(path: &str) -> bool {
        path.starts_with('/') && names_a_root(path)
    }

    /// Whether `held` is the view root's directory, `/`'s identity read
    /// through `roots` the first time it is asked. Where there is no `/`,
    /// nothing is it; a node without an identity is never it.
    ///
    /// # Errors
    ///
    /// The refusal of either stat.
    pub fn holds<R: Roots>(&mut self, roots: &R, held: &R::Dir) -> Result<bool, Errno> {
        let id = held.stat()?.id;
        let root = if let Some(root) = self.id {
            root
        } else {
            let root = match roots.open_node("/", false).and_then(|node| node.stat()) {
                Ok(stat) => stat.id,
                Err(Errno::NotFound) => FileId::NONE,
                Err(errno) => return Err(errno),
            };
            *self.id.insert(root)
        };
        Ok(!root.is_none() && id == root)
    }
}

/// Where a tool's operands are opened from: the working directory.
pub trait Roots {
    /// The directories it opens.
    type Dir: Dir;

    /// The directory `path` names, a final link followed only when `follow`.
    ///
    /// # Errors
    ///
    /// [`Errno::NotADirectory`] for anything else, [`Errno::NotFound`] for a
    /// link naming nothing, or the open's refusal.
    fn open_dir(&self, path: &str, follow: bool) -> Result<Self::Dir, Errno>;

    /// The directory holding `path`'s last component, opened through the path
    /// as written, and that component: how an operand acted on by name is
    /// reached.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for a path whose last component is no name (a
    /// root, `.` or `..`), or the open's refusal.
    fn parent(&self, path: &str) -> Result<(Self::Dir, String), Errno>;

    /// The node `path` names, opened with no read or write access, a final
    /// link followed only when `follow`.
    ///
    /// # Errors
    ///
    /// The open's refusal.
    fn open_node(&self, path: &str, follow: bool) -> Result<<Self::Dir as Dir>::File, Errno>;
}

/// What a visitor answers for an entry it is shown before any descent.
pub enum Step<D> {
    /// Open the entry as a directory, never through a link, and visit its
    /// entries; one that is no directory by then is refused.
    Descend,
    /// Visit the entries of the directory the visitor opened from this entry,
    /// so a walk goes on beneath the very node the visitor acted on.
    Into(D),
    /// Go on with the next entry.
    Skip,
}

/// What a depth-first walk does at each entry.
pub trait Visitor<D: Dir> {
    /// Why the visitor stopped the walk.
    type Error;

    /// `entry` of `dir`, at `path` beneath the walk's root, before any
    /// descent into it.
    ///
    /// # Errors
    ///
    /// The visitor's own, which ends the walk.
    fn enter(&mut self, dir: &D, entry: &Entry, path: &str) -> Result<Step<D>, Self::Error>;

    /// The directory `entry` of `dir`, held as `held`, after its entries were
    /// visited.
    ///
    /// # Errors
    ///
    /// The visitor's own, which ends the walk.
    fn leave(&mut self, dir: &D, entry: &Entry, path: &str, held: &D) -> Result<(), Self::Error>;

    /// An entry of `dir` a descent into failed with `errno`: it would not open
    /// as a directory ([`Errno::NotADirectory`] for one that is none by then),
    /// its entries would not list, or it is a directory the walk is already
    /// inside ([`Errno::LinkLoop`]). It is neither descended nor left.
    ///
    /// # Errors
    ///
    /// The visitor's own, which ends the walk.
    fn refused(
        &mut self,
        dir: &D,
        entry: &Entry,
        path: &str,
        errno: Errno,
    ) -> Result<(), Self::Error>;

    /// The walk's own root, whose entries would not list with `errno`.
    ///
    /// # Errors
    ///
    /// The visitor's own.
    fn unlisted_root(&mut self, path: &str, errno: Errno) -> Result<(), Self::Error>;
}

/// What a [`Cursor`] reaches next.
pub enum Reached<'c, D> {
    /// `entry` of `dir`, at `path`; [`Cursor::descend`] goes into it.
    Entry {
        /// The directory holding it.
        dir: &'c D,
        /// The entry, as the listing read it.
        entry: &'c Entry,
        /// Its path beneath the cursor's root.
        path: &'c str,
    },
    /// The directory `entry` of `dir`, held as `held`, after its entries.
    Left {
        /// The directory holding it.
        dir: &'c D,
        /// Its entry, as the listing read it.
        entry: &'c Entry,
        /// Its path beneath the cursor's root.
        path: &'c str,
        /// The directory itself, as the walk held it.
        held: &'c D,
    },
}

/// One directory a cursor is inside.
struct Level<D> {
    /// The directory and its entry's index in the level above; `None` for the
    /// root, which the cursor's owner provides.
    held: Option<(D, usize)>,
    /// The identity its entry was listed with; none for the root.
    id: FileId,
    /// How much of the cursor's path names this directory.
    path_len: usize,
    entries: Vec<Entry>,
    next: usize,
}

/// Where a cursor stands between two calls.
enum At<D> {
    /// Its path names the innermost directory.
    Between,
    /// At the innermost directory's entry `next - 1`, its name on the path.
    Entry,
    /// Just out of the directory `D`, entry `usize` of the innermost one,
    /// whose path the path still is.
    Left(D, usize),
    /// Past the root's last entry.
    End,
}

/// A depth-first walk of a tree that keeps its place between calls: each
/// directory's entries read whole before any is reached, descent only on the
/// caller's word and never through a link, and a directory left only after
/// its entries were reached. [`walk`] is the loop over one; a paused or
/// time-sliced operation holds one between its units.
///
/// It holds one directory and its listing a level and one path for them all,
/// so it costs the tree's depth, never its size; what the heap cannot hold is
/// refused as [`Errno::OutOfMemory`].
pub struct Cursor<D: Dir, R: Borrow<D> = D> {
    root: R,
    path: String,
    levels: Vec<Level<D>>,
    at: At<D>,
}

impl<D: Dir, R: Borrow<D>> Cursor<D, R> {
    /// A cursor before the first entry of `root`, known as `root_path`, whose
    /// entries are read here.
    ///
    /// # Errors
    ///
    /// The listing's refusal, [`Errno::OutOfRange`] for an entry no directory
    /// can hold, or [`Errno::OutOfMemory`].
    pub fn new(root: R, root_path: &str) -> Result<Self, Errno> {
        let entries = listing(root.borrow())?;
        let mut path = String::new();
        path.try_reserve(root_path.len() + 1 + FS_NAME_MAX)
            .map_err(|_| Errno::OutOfMemory)?;
        path.push_str(root_path);
        let mut levels = Vec::new();
        levels.try_reserve(1).map_err(|_| Errno::OutOfMemory)?;
        levels.push(Level {
            held: None,
            id: FileId::NONE,
            path_len: path.len(),
            entries,
            next: 0,
        });
        Ok(Self {
            root,
            path,
            levels,
            at: At::Between,
        })
    }

    /// The directory the cursor walks.
    pub fn root(&self) -> &D {
        self.root.borrow()
    }

    /// The next entry, or the directory just left; `None` once the root's
    /// entries are all reached.
    pub fn reach(&mut self) -> Option<Reached<'_, D>> {
        match core::mem::replace(&mut self.at, At::Between) {
            At::End => {
                self.at = At::End;
                return None;
            }
            At::Entry | At::Left(..) => {
                let len = self.levels.last().map_or(0, |level| level.path_len);
                self.path.truncate(len);
            }
            At::Between => {}
        }
        let Some(top) = self.levels.last_mut() else {
            self.at = At::End;
            return None;
        };
        if let Some(entry) = top.entries.get(top.next) {
            top.next += 1;
            // `descend` and `new` reserve room for any name a listing holds.
            push_name(&mut self.path, &entry.name);
            self.at = At::Entry;
            return self
                .entry()
                .map(|(dir, entry, path)| Reached::Entry { dir, entry, path });
        }
        if let Some((held, index)) = self.levels.pop().and_then(|done| done.held) {
            self.at = At::Left(held, index);
            self.left()
        } else {
            self.at = At::End;
            None
        }
    }

    /// The entry [`reach`](Self::reach) reached, while the cursor is at it.
    pub fn entry(&self) -> Option<(&D, &Entry, &str)> {
        if !matches!(self.at, At::Entry) {
            return None;
        }
        let top = self.levels.last()?;
        let entry = top.entries.get(top.next.checked_sub(1)?)?;
        Some((self.dir_of(top), entry, &self.path))
    }

    /// Go into the entry the cursor is at, opened as a directory through no
    /// link and listed whole.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] when the cursor is at no entry,
    /// [`Errno::NotADirectory`] for an entry that is no directory now,
    /// [`Errno::LinkLoop`] for a directory the cursor is already inside, the
    /// open's or listing's refusal, or [`Errno::OutOfMemory`]. The cursor stays
    /// at the entry.
    pub fn descend(&mut self) -> Result<(), Errno> {
        let (dir, entry, _) = self.entry().ok_or(Errno::OutOfRange)?;
        let held = dir.open_dir(&entry.name)?;
        self.descend_into(held)
    }

    /// Go into `held`, the directory the caller opened from the entry the
    /// cursor is at, listed whole.
    ///
    /// # Errors
    ///
    /// As [`descend`](Self::descend), but for the open.
    pub fn descend_into(&mut self, held: D) -> Result<(), Errno> {
        let top = self.levels.last().ok_or(Errno::OutOfRange)?;
        let index = top.next.checked_sub(1).ok_or(Errno::OutOfRange)?;
        let id = match (&self.at, top.entries.get(index)) {
            (At::Entry, Some(entry)) => entry.id,
            _ => return Err(Errno::OutOfRange),
        };
        if !id.is_none() && self.levels.iter().any(|level| level.id == id) {
            return Err(Errno::LinkLoop);
        }
        let entries = listing(&held)?;
        self.levels.try_reserve(1).map_err(|_| Errno::OutOfMemory)?;
        self.path
            .try_reserve(1 + FS_NAME_MAX)
            .map_err(|_| Errno::OutOfMemory)?;
        self.levels.push(Level {
            held: Some((held, index)),
            id,
            path_len: self.path.len(),
            entries,
            next: 0,
        });
        self.at = At::Between;
        Ok(())
    }

    fn dir_of<'c>(&'c self, level: &'c Level<D>) -> &'c D {
        level
            .held
            .as_ref()
            .map_or_else(|| self.root.borrow(), |(dir, _)| dir)
    }

    fn left(&self) -> Option<Reached<'_, D>> {
        let At::Left(held, index) = &self.at else {
            return None;
        };
        let top = self.levels.last()?;
        Some(Reached::Left {
            dir: self.dir_of(top),
            entry: top.entries.get(*index)?,
            path: &self.path,
            held,
        })
    }
}

/// `dir`'s entries, each a name a directory can hold.
fn listing<D: Dir>(dir: &D) -> Result<Vec<Entry>, Errno> {
    let entries = dir.entries()?;
    let named = |name: &str| {
        !name.is_empty()
            && name.len() <= FS_NAME_MAX
            && name != "."
            && name != ".."
            && !name.contains('/')
    };
    if entries.iter().all(|entry| named(&entry.name)) {
        Ok(entries)
    } else {
        Err(Errno::OutOfRange)
    }
}

/// Walk `root` (known to its caller as `root_path`) depth first through a
/// [`Cursor`], showing `visitor` each entry and each directory left, and
/// descending where it answers so.
///
/// # Errors
///
/// The first error the visitor answers.
pub fn walk<D: Dir, V: Visitor<D>>(
    root: &D,
    root_path: &str,
    visitor: &mut V,
) -> Result<(), V::Error> {
    let mut cursor = match Cursor::<D, &D>::new(root, root_path) {
        Ok(cursor) => cursor,
        Err(errno) => return visitor.unlisted_root(root_path, errno),
    };
    while let Some(reached) = cursor.reach() {
        let step = match reached {
            Reached::Left {
                dir,
                entry,
                path,
                held,
            } => {
                visitor.leave(dir, entry, path, held)?;
                continue;
            }
            Reached::Entry { dir, entry, path } => visitor.enter(dir, entry, path)?,
        };
        let descended = match step {
            Step::Skip => continue,
            Step::Descend => cursor.descend(),
            Step::Into(held) => cursor.descend_into(held),
        };
        if let (Err(errno), Some((dir, entry, path))) = (descended, cursor.entry()) {
            visitor.refused(dir, entry, path, errno)?;
        }
    }
    Ok(())
}

/// What an operand names through its final link: a directory held as one, so
/// a walk can descend the very node it changed, or any other node.
pub enum Operand<D: Dir> {
    /// A directory.
    Directory(D),
    /// Anything else, held with no read or write access.
    Node(D::File),
}

/// Why an operand named through its final link was not opened.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Unopened {
    /// A symbolic link naming nothing.
    Dangling,
    /// The open's refusal.
    Refused(Errno),
}

/// Open the operand `path` through its final link.
///
/// # Errors
///
/// [`Unopened::Dangling`] for a link naming nothing, else the open's
/// refusal.
pub fn open_operand<R: Roots>(roots: &R, path: &str) -> Result<Operand<R::Dir>, Unopened> {
    match roots.open_dir(path, true) {
        Ok(dir) => Ok(Operand::Directory(dir)),
        Err(Errno::NotADirectory) => roots
            .open_node(path, true)
            .map(Operand::Node)
            .map_err(Unopened::Refused),
        Err(Errno::NotFound) if dangles(roots, path) => Err(Unopened::Dangling),
        Err(errno) => Err(Unopened::Refused(errno)),
    }
}

/// Whether `path`, which names nothing through its final link, is a link.
fn dangles<R: Roots>(roots: &R, path: &str) -> bool {
    roots
        .open_node(path, false)
        .and_then(|node| node.stat())
        .is_ok_and(|stat| stat.kind == FileKind::Symlink)
}

/// What a tool does to each node of an operand it changes node by node, each
/// through its own descriptor, a directory before its entries.
pub trait EachNode<D: Dir> {
    /// Why the tool stopped the run.
    type Error;

    /// `node`, at `path`, read as `stat`, acted on through itself.
    ///
    /// # Errors
    ///
    /// The tool's own, which ends the run.
    fn node(&mut self, node: &dyn Node, stat: &FileStat, path: &str) -> Result<(), Self::Error>;

    /// The link at `path`, met inside a tree and left alone with what it
    /// names.
    ///
    /// # Errors
    ///
    /// The tool's own, which ends the run.
    fn link(&mut self, path: &str) -> Result<(), Self::Error>;

    /// The operand `path`, a link naming nothing.
    ///
    /// # Errors
    ///
    /// The tool's own, which ends the run.
    fn dangling(&mut self, path: &str) -> Result<(), Self::Error>;

    /// `path` would not open or be read, for `errno`.
    ///
    /// # Errors
    ///
    /// The tool's own, which ends the run.
    fn unopened(&mut self, path: &str, errno: Errno) -> Result<(), Self::Error>;

    /// The directory at `path` would not be gone into, for `errno`:
    /// [`Errno::Stale`] for a name no longer holding the node acted on.
    ///
    /// # Errors
    ///
    /// The tool's own, which ends the run.
    fn unread(&mut self, path: &str, errno: Errno) -> Result<(), Self::Error>;

    /// Whether the directory operand `dir`, at `path`, is to be refused a
    /// recursive change; the tool reports its own refusal.
    ///
    /// # Errors
    ///
    /// The tool's own, which ends the run.
    fn refuses(&mut self, dir: &D, path: &str) -> Result<bool, Self::Error> {
        let _ = (dir, path);
        Ok(false)
    }
}

/// Act on the operand `path` through its final link, and when `recursive`
/// and it is a directory on every node beneath it, a link met there left
/// alone and a directory gone into only while its name holds the node acted
/// on.
///
/// # Errors
///
/// The first error `tool` answers.
pub fn each_node<R: Roots, T: EachNode<R::Dir>>(
    roots: &R,
    path: &str,
    recursive: bool,
    tool: &mut T,
) -> Result<(), T::Error> {
    if !recursive {
        // Nothing beneath it is reached, so one resolution serves.
        return match roots.open_node(path, true) {
            Ok(node) => match node.stat() {
                Ok(stat) => tool.node(&node, &stat, path),
                Err(errno) => tool.unopened(path, errno),
            },
            Err(Errno::NotFound) if dangles(roots, path) => tool.dangling(path),
            Err(errno) => tool.unopened(path, errno),
        };
    }
    match open_operand(roots, path) {
        Ok(Operand::Directory(dir)) => {
            if recursive && tool.refuses(&dir, path)? {
                return Ok(());
            }
            match dir.stat() {
                Ok(stat) => tool.node(&dir, &stat, path)?,
                Err(errno) => return tool.unopened(path, errno),
            }
            if recursive {
                walk(&dir, path, &mut Each(tool))?;
            }
            Ok(())
        }
        Ok(Operand::Node(node)) => match node.stat() {
            Ok(stat) => tool.node(&node, &stat, path),
            Err(errno) => tool.unopened(path, errno),
        },
        Err(Unopened::Dangling) => tool.dangling(path),
        Err(Unopened::Refused(errno)) => tool.unopened(path, errno),
    }
}

/// GNU's `-v` line for a link a recursive change met and left alone.
#[must_use]
pub fn untouched_link(path: &str) -> String {
    alloc::format!(
        "neither symbolic link {} nor referent has been changed\n",
        quote(path)
    )
}

/// GNU's refusal, by the tool `word`, to work recursively on the view root
/// reached as `path`: two lines, the second naming the override.
pub struct RootRefusal<'a> {
    /// The tool's own command word.
    pub word: &'a str,
    /// The operand as given.
    pub path: &'a str,
}

impl fmt::Display for RootRefusal<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "it is dangerous to operate recursively on {}",
            quote(self.path)
        )?;
        if self.path != "/" {
            f.write_str(" (same as '/')")?;
        }
        write!(
            f,
            "\n{}: use --no-preserve-root to override this failsafe",
            self.word
        )
    }
}

/// The walk of an [`EachNode`] tool beneath its operand.
struct Each<'t, T>(&'t mut T);

impl<D: Dir, T: EachNode<D>> Visitor<D> for Each<'_, T> {
    type Error = T::Error;

    fn enter(&mut self, dir: &D, entry: &Entry, path: &str) -> Result<Step<D>, T::Error> {
        if entry.kind == Kind::Link {
            self.0.link(path)?;
            return Ok(Step::Skip);
        }
        let opened = dir
            .open_node(&entry.name)
            .and_then(|node| node.stat().map(|stat| (node, stat)));
        let (node, stat) = match opened {
            Ok(opened) => opened,
            Err(errno) => {
                self.0.unopened(path, errno)?;
                return Ok(Step::Skip);
            }
        };
        match stat.kind {
            // Swapped for a link since the listing: left alone like one listed.
            FileKind::Symlink => self.0.link(path)?,
            FileKind::Regular => self.0.node(&node, &stat, path)?,
            FileKind::Directory => {
                self.0.node(&node, &stat, path)?;
                // Opened again to list it, after a change that may have been
                // what lets it be listed.
                let held = dir
                    .open_dir(&entry.name)
                    .and_then(|held| match held.stat() {
                        Ok(now) if now.id == stat.id => Ok(held),
                        Ok(_) => Err(Errno::Stale),
                        Err(errno) => Err(errno),
                    });
                match held {
                    Ok(held) => return Ok(Step::Into(held)),
                    Err(errno) => self.0.unread(path, errno)?,
                }
            }
        }
        Ok(Step::Skip)
    }

    fn leave(&mut self, _: &D, _: &Entry, _: &str, _: &D) -> Result<(), T::Error> {
        Ok(())
    }

    fn refused(&mut self, _: &D, _: &Entry, path: &str, errno: Errno) -> Result<(), T::Error> {
        self.0.unread(path, errno)
    }

    fn unlisted_root(&mut self, path: &str, errno: Errno) -> Result<(), T::Error> {
        self.0.unread(path, errno)
    }
}

#[cfg(all(test, feature = "test-support"))]
#[path = "tree_tests.rs"]
mod tests;
