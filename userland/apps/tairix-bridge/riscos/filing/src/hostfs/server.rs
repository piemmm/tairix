//! The side of HostFS that holds the files: the session's roots, each a
//! directory held open, served through `lib/fscopy`'s tree seam, so every
//! name is reached from a held directory and nothing resolves outside its
//! root; and `Transfer`, the files delivered to the session (`transfer`).
//!
//! - **Names.** A RISC OS leaf crosses to TAIRiX's spelling through
//!   `lib/fsmeta`'s one conversion. A lookup ignores case and keeps it, as
//!   `FileCore`'s do: an exact match wins, and two names that differ only in
//!   case, with neither exact, are refused as ambiguous. A host name RISC OS
//!   could not hold is left out of a listing.
//! - **Metadata.** An object's load and execution words and attributes are
//!   its volume's `acorn.*` attributes where the volume keeps them; else a
//!   `,xxx` suffix gives its type, and else the canonical filetype of what
//!   its name says it holds, or Data; the stamp is its modification time.
//!   Written where the volume keeps them, else as the suffix and the time; an
//!   untyped stamp nothing can keep is refused, never dropped, and refused
//!   before a save empties the file it would have replaced.
//! - **Attributes.** A file is opened to read only where its attributes let
//!   it be read, and to write only where they let it be written; a locked
//!   object is neither saved over, deleted nor renamed.

mod transfer;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use tairix_abi::time::Time64;
use tairix_abi::{
    Errno, FileId, FileKind, FileStat, NodeExpect, RenameFlags, StampChange, TimesChange,
    UnlinkFlags,
};
use tairix_browse::{canonical_filetype, media_for_name};
use tairix_fscopy::tree::{unlink_node, Creation, Dir, FileAccess, Node, WriteAt};
use tairix_fscopy::{Source, Target};
use tairix_fsmeta::preset::acorn::fileswitch::{
    LOCKED, OWNER_READ, OWNER_WRITE, PUBLIC_READ, PUBLIC_WRITE,
};
use tairix_fsmeta::preset::acorn::{self, Filetype, LoadExec};

use super::protocol::{
    Kind, Object, Refusal, Reply, Request, Version, ENTRIES_MOST, NAME_MOST, ROOTS_MOST,
};
use transfer::Transfer;

/// The most entries of one directory the server reads: a containment bound on
/// what one listing costs, no RISC OS directory holding more.
const ENTRIES_BOUND: usize = 1 << 16;

/// The most files the session may hold open at once, FileSwitch's own bound.
const FILES: usize = crate::fileswitch::open::HANDLES as usize;

/// What a file and a directory HostFS makes are born with, less the umask.
const FILE_MODE: u32 = 0o666;
const DIR_MODE: u32 = 0o777;

/// A root as the server is given it: the directory, the name of its folder,
/// and whether the session may write to it.
pub struct Given<D> {
    /// The directory, held.
    pub dir: D,
    /// Its folder's name, as TAIRiX spells it.
    pub name: String,
    /// Whether it may be written.
    pub writable: bool,
}

/// A root the server holds: its directory, its disc's name, and whether the
/// session may write to it.
struct Held<D> {
    dir: D,
    disc: Vec<u8>,
    writable: bool,
}

/// A root a request names: one the server holds, or `Transfer`.
enum Reached<'a, D> {
    Held(&'a Held<D>),
    Transfer,
}

/// Where an open file's bytes are: a file the server opened, or a delivered
/// one, by its number on `Transfer`.
enum Body<F> {
    Own(F),
    Delivered(usize),
}

/// A file the session holds open.
struct Open<F> {
    body: Body<F>,
    root: u32,
    path: Vec<u8>,
    writable: bool,
}

/// The last directory listed, its entries in the order a listing gives them,
/// kept while the directory is unchanged so a listing read a page at a time
/// is read once.
struct Listed {
    root: u32,
    path: Vec<u8>,
    stamp: (Time64, u64),
    entries: Vec<(Vec<u8>, Object)>,
}

/// The server of a session's roots.
pub struct Server<D: Dir> {
    roots: Vec<Held<D>>,
    own: bool,
    transfer: Transfer<D::File>,
    transfer_disc: Vec<u8>,
    seed: [u8; 16],
    files: Vec<Option<Open<D::File>>>,
    epoch: u32,
    listed: Option<Listed>,
    read: Vec<u8>,
}

impl<D: Dir> Server<D> {
    /// The server of `own`, the session's own disc, then `given`, each
    /// root's disc named for its folder, and of `Transfer` after them, each
    /// name made unique; its read-cache seed `seed`, and its handles carrying
    /// `epoch` so one of an earlier session reaches none of this one's files.
    #[must_use]
    pub fn new(own: Option<Given<D>>, given: Vec<Given<D>>, seed: [u8; 16], epoch: u32) -> Self {
        let has_own = own.is_some();
        let mut roots: Vec<Held<D>> = Vec::new();
        for root in own.into_iter().chain(given).take(ROOTS_MOST - 1) {
            let disc = unique_disc(&root.name, &roots.iter().map(|held| held.disc.as_slice()));
            roots.push(Held {
                dir: root.dir,
                disc,
                writable: root.writable,
            });
        }
        let transfer_disc = unique_disc(
            transfer::DISC,
            &roots.iter().map(|held| held.disc.as_slice()),
        );
        Self {
            roots,
            own: has_own,
            transfer: Transfer::new(),
            transfer_disc,
            seed,
            files: Vec::new(),
            epoch: epoch << 8,
            listed: None,
            read: Vec::new(),
        }
    }

    /// `file`, delivered to the session by a drop or an open as `name`, put
    /// on `Transfer` in a directory of its own, writable where `writable`:
    /// its whole name, as RISC OS spells it.
    ///
    /// # Errors
    ///
    /// [`Refusal::BadName`] where `name` crosses to no RISC OS leaf, and
    /// [`Refusal::Failed`] where there is no room for another delivery.
    pub fn deliver(
        &mut self,
        name: &str,
        file: D::File,
        writable: bool,
    ) -> Result<Vec<u8>, Refusal> {
        let mut whole = Vec::new();
        whole
            .try_reserve_exact(
                super::NAME.len() + self.transfer_disc.len() + transfer::PATH_MOST + b"::.$.".len(),
            )
            .map_err(|_| Refusal::Failed)?;
        whole.extend_from_slice(super::NAME.as_bytes());
        whole.extend_from_slice(b"::");
        whole.extend_from_slice(&self.transfer_disc);
        whole.extend_from_slice(b".$.");
        self.transfer.deliver(name, file, writable, &mut whole)?;
        Ok(whole)
    }

    /// The reply to the request `request` holds, appended to `out`; a request
    /// that does not decode is refused.
    pub fn serve(&mut self, request: &[u8], out: &mut Vec<u8>) {
        let reply = match Request::decode(request) {
            Some(request) => self.answer(&request),
            None => Err(Refusal::Failed),
        };
        match reply {
            Ok(Answer::Roots) => match self.discs() {
                Some(discs) => Reply::Roots {
                    seed: self.seed,
                    own: self.own,
                    discs,
                }
                .encode(out),
                None => Reply::Refused(Refusal::Failed).encode(out),
            },
            Ok(Answer::Reply(reply)) => reply.encode(out),
            Ok(Answer::Read) => Reply::Bytes(&self.read).encode(out),
            Err(refusal) => Reply::Refused(refusal).encode(out),
        }
    }

    /// Every disc's name, `Transfer`'s last; none where there is no room.
    fn discs(&self) -> Option<Vec<Vec<u8>>> {
        let mut discs = Vec::new();
        discs.try_reserve_exact(self.roots.len() + 1).ok()?;
        for disc in self
            .roots
            .iter()
            .map(|held| held.disc.as_slice())
            .chain([self.transfer_disc.as_slice()])
        {
            discs.push(tairix_util::fallible::copied(disc)?);
        }
        Some(discs)
    }

    fn answer(&mut self, request: &Request<'_>) -> Result<Answer, Refusal> {
        let reply = match *request {
            Request::Roots => return Ok(Answer::Roots),
            Request::Read { root, path } => Reply::Object(self.read_object(root, path)?),
            Request::List {
                root,
                path,
                offset,
                count,
            } => self.list(root, path, offset, count)?,
            Request::Open { root, path, update } => self.open(root, path, update)?,
            Request::ReadBytes {
                handle,
                offset,
                length,
            } => {
                self.read_bytes(handle, offset, length)?;
                return Ok(Answer::Read);
            }
            Request::WriteBytes {
                handle,
                offset,
                bytes,
            } => {
                let wrote = self
                    .writable(handle)?
                    .write_at(u64::from(offset), bytes)
                    .map_err(refusal)?;
                Reply::Wrote(u32::try_from(wrote).unwrap_or(u32::MAX))
            }
            Request::SetExtent { handle, extent } => {
                self.writable(handle)?
                    .set_len(u64::from(extent))
                    .map_err(refusal)?;
                Reply::Done
            }
            Request::Sync { handle } => {
                self.held(handle)?.0.sync().map_err(refusal)?;
                Reply::Done
            }
            Request::Close { handle, stamp } => self.close(handle, stamp)?,
            Request::Create {
                root,
                path,
                load,
                exec,
                length,
            } => self.create(root, path, load, exec, length)?,
            Request::CreateDir {
                root,
                path,
                load,
                exec,
            } => self.create_dir(root, path, load, exec)?,
            Request::WriteInfo {
                root,
                path,
                load,
                exec,
                attributes,
            } => self.write_info(root, path, load, exec, attributes)?,
            Request::Delete { root, path } => Reply::Object(self.delete(root, path)?),
            Request::Rename { root, from, to } => self.rename(root, from, to)?,
            Request::Space { root } => match self.root(root)? {
                Reached::Transfer => Reply::Space { free: 0, total: 0 },
                Reached::Held(held) => {
                    let stats = held.dir.volume_stats().map_err(refusal)?;
                    let block = u64::from(stats.block_size);
                    Reply::Space {
                        free: stats.free_blocks.saturating_mul(block),
                        total: stats.total_blocks.saturating_mul(block),
                    }
                }
            },
        };
        Ok(Answer::Reply(reply))
    }

    fn root(&self, root: u32) -> Result<Reached<'_, D>, Refusal> {
        match self.roots.get(root as usize) {
            Some(held) => Ok(Reached::Held(held)),
            None if root as usize == self.roots.len() => Ok(Reached::Transfer),
            None => Err(Refusal::Unknown),
        }
    }

    /// The directory of the root `root` names, the session allowed to write
    /// to it; none for `Transfer`, whose own rules decide.
    fn writable_root(&self, root: u32) -> Result<Option<&D>, Refusal> {
        match self.root(root)? {
            Reached::Transfer => Ok(None),
            Reached::Held(held) if held.writable => Ok(Some(&held.dir)),
            Reached::Held(_) => Err(Refusal::WriteProtected),
        }
    }

    /// The directory holding `path`'s last leaf, entered from `top` — none
    /// where it is `top` — and that leaf as RISC OS spells it; none for the
    /// root itself.
    fn parent(top: &D, path: &[u8]) -> Result<Option<Parent<D>>, Refusal> {
        if path.is_empty() {
            return Ok(None);
        }
        let mut leaves = path.split(|&b| b == b'.');
        let last = leaves.next_back().unwrap_or_default();
        let mut held: Option<D> = None;
        for leaf in leaves {
            let dir = held.as_ref().unwrap_or(top);
            let name = find(dir, leaf)?.ok_or(Refusal::NotFound)?;
            held = Some(dir.enter(&name).map_err(refusal)?);
        }
        Ok(Some((
            held,
            tairix_util::fallible::copied(last).ok_or(Refusal::Failed)?,
        )))
    }

    /// The object `path` names beneath `root`.
    fn read_object(&self, root: u32, path: &[u8]) -> Result<Object, Refusal> {
        let top = match self.root(root)? {
            Reached::Transfer => return self.transfer.read(path),
            Reached::Held(held) => &held.dir,
        };
        let Some((held, leaf)) = Self::parent(top, path)? else {
            let stat = top.stat().map_err(refusal)?;
            return object(top, &stat, "");
        };
        let dir = held.as_ref().unwrap_or(top);
        let Some(name) = find(dir, &leaf)? else {
            return Ok(Object::NOTHING);
        };
        let node = dir.follow(&name, false).map_err(refusal)?;
        let stat = node.stat().map_err(refusal)?;
        object(&node, &stat, &name)
    }

    fn list(
        &mut self,
        root: u32,
        path: &[u8],
        offset: u32,
        count: u32,
    ) -> Result<Reply<'static>, Refusal> {
        let top = match self.root(root)? {
            Reached::Transfer => return self.transfer.list(path, offset, count),
            Reached::Held(held) => &held.dir,
        };
        let held = match Self::parent(top, path)? {
            None => None,
            Some((held, leaf)) => {
                let dir = held.as_ref().unwrap_or(top);
                let name = find(dir, &leaf)?.ok_or(Refusal::NotFound)?;
                Some(dir.enter(&name).map_err(refusal)?)
            }
        };
        let dir = held.as_ref().unwrap_or(top);
        let stat = dir.stat().map_err(refusal)?;
        let stamp = (stat.times.modified, stat.content_gen);
        let kept = self.listed.as_ref().is_some_and(|listed| {
            listed.root == root && listed.path == path && listed.stamp == stamp
        });
        if !kept {
            let entries = listing(dir)?;
            self.listed = Some(Listed {
                root,
                path: tairix_util::fallible::copied(path).ok_or(Refusal::Failed)?,
                stamp,
                entries,
            });
        }
        let entries = self
            .listed
            .as_ref()
            .map_or(&[][..], |listed| &listed.entries[..]);
        let first = (offset as usize).min(entries.len());
        let end = first
            .saturating_add(count.min(ENTRIES_MOST) as usize)
            .min(entries.len());
        let next = if end == entries.len() {
            u32::MAX
        } else {
            u32::try_from(end).unwrap_or(u32::MAX)
        };
        let mut page = Vec::new();
        page.try_reserve_exact(end - first)
            .map_err(|_| Refusal::Failed)?;
        for (name, object) in &entries[first..end] {
            page.push((
                tairix_util::fallible::copied(name).ok_or(Refusal::Failed)?,
                *object,
            ));
        }
        Ok(Reply::Entries {
            next,
            entries: page,
        })
    }

    fn open(&mut self, root: u32, path: &[u8], update: bool) -> Result<Reply<'static>, Refusal> {
        let (body, extent, writable, version) = match self.root(root)? {
            Reached::Transfer => {
                let opened = self.transfer.open(path, update)?;
                (
                    Body::Delivered(opened.number),
                    opened.extent,
                    opened.writable,
                    opened.version,
                )
            }
            Reached::Held(held) => {
                if update && !held.writable {
                    return Err(Refusal::WriteProtected);
                }
                let (reached, leaf) =
                    Self::parent(&held.dir, path)?.ok_or(Refusal::IsADirectory)?;
                let dir = reached.as_ref().unwrap_or(&held.dir);
                let name = find(dir, &leaf)?.ok_or(Refusal::NotFound)?;
                let access = if update {
                    FileAccess::Both
                } else {
                    FileAccess::Read
                };
                let file = dir
                    .open_reached(&name, access, Creation::Existing, FILE_MODE)
                    .map_err(refusal)?;
                let stat = file.stat().map_err(refusal)?;
                let wanted = if update { OWNER_WRITE } else { OWNER_READ };
                if object(&file, &stat, &name)?.attributes & wanted == 0 {
                    return Err(Refusal::AccessViolation);
                }
                let extent = u32::try_from(stat.size).map_err(|_| Refusal::TooLarge)?;
                (Body::Own(file), extent, update, version_of(&stat))
            }
        };
        let path = tairix_util::fallible::copied(path).ok_or(Refusal::Failed)?;
        let slot = match self.files.iter().position(Option::is_none) {
            Some(slot) => slot,
            None if self.files.len() < FILES && self.files.try_reserve(1).is_ok() => {
                self.files.push(None);
                self.files.len() - 1
            }
            None => return Err(Refusal::Failed),
        };
        self.files[slot] = Some(Open {
            body,
            root,
            path,
            writable,
        });
        Ok(Reply::Opened {
            handle: self.epoch | u32::try_from(slot).unwrap_or(0),
            extent,
            writable,
            version,
        })
    }

    /// The file on `handle`, and whether it was opened to write.
    fn held(&mut self, handle: u32) -> Result<(&mut D::File, bool), Refusal> {
        if handle & !0xFF != self.epoch {
            return Err(Refusal::Unknown);
        }
        let Self {
            files, transfer, ..
        } = self;
        let open = files
            .get_mut((handle & 0xFF) as usize)
            .and_then(Option::as_mut)
            .ok_or(Refusal::Unknown)?;
        let writable = open.writable;
        let file = match &mut open.body {
            Body::Own(file) => file,
            Body::Delivered(number) => transfer.file_mut(*number).ok_or(Refusal::Unknown)?,
        };
        Ok((file, writable))
    }

    /// The file on `handle`, refused where it was not opened to write.
    fn writable(&mut self, handle: u32) -> Result<&mut D::File, Refusal> {
        match self.held(handle)? {
            (file, true) => Ok(file),
            (_, false) => Err(Refusal::AccessViolation),
        }
    }

    fn read_bytes(&mut self, handle: u32, offset: u32, length: u32) -> Result<(), Refusal> {
        let length = length as usize;
        let mut read = core::mem::take(&mut self.read);
        read.clear();
        let filled = Self::fill(self.held(handle), &mut read, offset, length);
        self.read = read;
        filled
    }

    /// `length` bytes of `held` from `offset` into `read`, fewer at the file's
    /// end.
    fn fill(
        held: Result<(&mut D::File, bool), Refusal>,
        read: &mut Vec<u8>,
        offset: u32,
        length: usize,
    ) -> Result<(), Refusal> {
        let (file, _) = held?;
        read.try_reserve(length).map_err(|_| Refusal::Failed)?;
        read.resize(length, 0);
        let mut got = 0;
        while got < length {
            match file.read_at(u64::from(offset) + got as u64, &mut read[got..]) {
                Ok(0) => break,
                Ok(count) => got += count,
                Err(errno) => return Err(refusal(errno)),
            }
        }
        read.truncate(got);
        Ok(())
    }

    fn close(&mut self, handle: u32, stamp: Option<(u32, u32)>) -> Result<Reply<'static>, Refusal> {
        self.held(handle)?;
        let slot = (handle & 0xFF) as usize;
        let Some(open) = self.files.get_mut(slot).and_then(Option::take) else {
            return Err(Refusal::Unknown);
        };
        if let Some((load, exec)) = stamp.filter(|_| open.writable) {
            drop(open.body);
            let attributes = self.read_object(open.root, &open.path)?.attributes;
            self.write_info(open.root, &open.path, load, exec, attributes)?;
        }
        Ok(Reply::Done)
    }

    fn create(
        &mut self,
        root: u32,
        path: &[u8],
        load: u32,
        exec: u32,
        length: u32,
    ) -> Result<Reply<'static>, Refusal> {
        let Some(top) = self.writable_root(root)? else {
            return self.transfer.create(path, load, exec, length);
        };
        let (held, leaf) = Self::parent(top, path)?.ok_or(Refusal::IsADirectory)?;
        let dir = held.as_ref().unwrap_or(top);
        if let Some(name) = find(dir, &leaf)? {
            save_over(dir, &name, load, exec, length)?;
        } else {
            save_new(dir, &crossed(&leaf)?, load, exec, length)?;
        }
        self.listed = None;
        Ok(Reply::Done)
    }

    fn create_dir(
        &mut self,
        root: u32,
        path: &[u8],
        load: u32,
        exec: u32,
    ) -> Result<Reply<'static>, Refusal> {
        let Some(top) = self.writable_root(root)? else {
            return match self.transfer.read(path)?.kind {
                Kind::Nothing => Err(Refusal::WriteProtected),
                _ => Err(Refusal::Exists),
            };
        };
        let (held, leaf) = Self::parent(top, path)?.ok_or(Refusal::Exists)?;
        let dir = held.as_ref().unwrap_or(top);
        if find(dir, &leaf)?.is_some() {
            return Err(Refusal::Exists);
        }
        let name = crossed(&leaf)?;
        dir.make_dir(&name, DIR_MODE).map_err(refusal)?;
        keep(dir, &name, load, exec, DEFAULT_ATTRIBUTES)?;
        self.listed = None;
        Ok(Reply::Done)
    }

    fn write_info(
        &mut self,
        root: u32,
        path: &[u8],
        load: u32,
        exec: u32,
        attributes: u8,
    ) -> Result<Reply<'static>, Refusal> {
        let Some(top) = self.writable_root(root)? else {
            return self.transfer.write_info(path, load, exec, attributes);
        };
        let (held, leaf) = Self::parent(top, path)?.ok_or(Refusal::AccessViolation)?;
        let dir = held.as_ref().unwrap_or(top);
        let name = find(dir, &leaf)?.ok_or(Refusal::NotFound)?;
        keep(dir, &name, load, exec, attributes)?;
        self.listed = None;
        Ok(Reply::Done)
    }

    fn delete(&mut self, root: u32, path: &[u8]) -> Result<Object, Refusal> {
        let Some(top) = self.writable_root(root)? else {
            return self.transfer.delete(path);
        };
        let (held, leaf) = Self::parent(top, path)?.ok_or(Refusal::AccessViolation)?;
        let dir = held.as_ref().unwrap_or(top);
        let Some(name) = find(dir, &leaf)? else {
            return Ok(Object::NOTHING);
        };
        let node = dir.open_node(&name).map_err(refusal)?;
        let stat = node.stat().map_err(refusal)?;
        let gone = object(&node, &stat, &name)?;
        if gone.attributes & LOCKED != 0 {
            return Err(Refusal::Locked);
        }
        let flags = if stat.kind == FileKind::Directory {
            UnlinkFlags::DIRECTORY
        } else {
            UnlinkFlags::EXPECT_NODE
        };
        let expect = NodeExpect::of_node(stat.id);
        dir.unlink(
            &name,
            flags,
            (stat.kind != FileKind::Directory).then_some(&expect),
        )
        .map_err(refusal)?;
        self.listed = None;
        Ok(gone)
    }

    fn rename(&mut self, root: u32, from: &[u8], to: &[u8]) -> Result<Reply<'static>, Refusal> {
        let Some(top) = self.writable_root(root)? else {
            return match self.transfer.read(from)?.kind {
                Kind::Nothing => Err(Refusal::NotFound),
                _ => Err(Refusal::WriteProtected),
            };
        };
        let (from_held, from_leaf) = Self::parent(top, from)?.ok_or(Refusal::AccessViolation)?;
        let (to_held, to_leaf) = Self::parent(top, to)?.ok_or(Refusal::Exists)?;
        let from_dir = from_held.as_ref().unwrap_or(top);
        let to_dir = to_held.as_ref().unwrap_or(top);
        let from_name = find(from_dir, &from_leaf)?.ok_or(Refusal::NotFound)?;
        let node = from_dir.open_node(&from_name).map_err(refusal)?;
        let stat = node.stat().map_err(refusal)?;
        if object(&node, &stat, &from_name)?.attributes & LOCKED != 0 {
            return Err(Refusal::Locked);
        }
        drop(node);
        let suffix = suffix_of(&from_name);
        let to_name = match find(to_dir, &to_leaf)? {
            Some(taken) if !same_entry(from_dir, &from_name, to_dir, &taken) => {
                return Err(Refusal::Exists)
            }
            _ => {
                let mut name = crossed(&to_leaf)?;
                name.push_str(suffix);
                name
            }
        };
        from_dir
            .rename(&from_name, to_dir, &to_name, RenameFlags::NO_REPLACE, None)
            .map_err(refusal)?;
        self.listed = None;
        Ok(Reply::Done)
    }
}

/// The directory holding a path's last leaf, none where it is the root, and
/// that leaf.
type Parent<D> = (Option<D>, Vec<u8>);

/// A save over the file `name` in `dir`: emptied to `length` once `load` and
/// `exec` are known to be keepable, its attributes kept; refused, untouched,
/// where they forbid it.
fn save_over<D: Dir>(
    dir: &D,
    name: &str,
    load: u32,
    exec: u32,
    length: u32,
) -> Result<(), Refusal> {
    let node = dir.open_node(name).map_err(refusal)?;
    let stat = node.stat().map_err(refusal)?;
    if stat.kind == FileKind::Directory {
        return Err(Refusal::IsADirectory);
    }
    let attributes = object(&node, &stat, name)?.attributes;
    if attributes & LOCKED != 0 {
        return Err(Refusal::Locked);
    }
    if attributes & OWNER_WRITE == 0 {
        return Err(Refusal::AccessViolation);
    }
    let stamping = keep_first(&node, load, exec, attributes, None)?;
    drop(node);
    let mut file = dir
        .open_reached(name, FileAccess::Write, Creation::Truncate, FILE_MODE)
        .map_err(refusal)?;
    file.set_len(u64::from(length)).map_err(refusal)?;
    drop(file);
    keep_rest(dir, name, &stamping)
}

/// A save as the new file `name` in `dir`, `length` bytes long; nothing made
/// where `load` and `exec` cannot be kept.
fn save_new<D: Dir>(dir: &D, name: &str, load: u32, exec: u32, length: u32) -> Result<(), Refusal> {
    let mut file = dir
        .open_reached(name, FileAccess::Write, Creation::New, FILE_MODE)
        .map_err(refusal)?;
    let made = file.stat().map_err(refusal)?.id;
    let sized = file.set_len(u64::from(length)).map_err(refusal);
    drop(file);
    let kept = sized.and_then(|()| keep(dir, name, load, exec, DEFAULT_ATTRIBUTES));
    if kept.is_err() {
        // The refusal is the answer; the file this save made goes with it.
        let _ = unlink_node(dir, name, made);
    }
    kept
}

/// What [`Server::answer`] answers with: a reply, or one built from the
/// server's own state as it is encoded.
enum Answer {
    Roots,
    Read,
    Reply(Reply<'static>),
}

/// Each mode bit and the attribute it is: the owner's and the public's read
/// and write.
const MODE_ATTRIBUTES: [(u32, u8); 4] = [
    (0o400, OWNER_READ),
    (0o200, OWNER_WRITE),
    (0o004, PUBLIC_READ),
    (0o002, PUBLIC_WRITE),
];

/// What a file HostFS makes is given: read and write for its owner, read for
/// others.
const DEFAULT_ATTRIBUTES: u8 = OWNER_READ | OWNER_WRITE | PUBLIC_READ;

/// Whether `a` in `a_dir` and `b` in `b_dir` are one entry: a rename that
/// changes only a name's case.
fn same_entry<D: Dir>(a_dir: &D, a: &str, b_dir: &D, b: &str) -> bool {
    let id = |dir: &D, name: &str| {
        dir.open_node(name)
            .and_then(|node| node.stat())
            .map(|stat| stat.id)
    };
    matches!((id(a_dir, a), id(b_dir, b)), (Ok(x), Ok(y)) if x == y)
}

/// The RISC OS leaf `leaf` as TAIRiX spells it.
fn crossed(leaf: &[u8]) -> Result<String, Refusal> {
    let crossed = acorn::leaf_from_riscos(leaf).map_err(|_| Refusal::BadName)?;
    let text = core::str::from_utf8(crossed.as_bytes()).map_err(|_| Refusal::BadName)?;
    let mut name = String::new();
    name.try_reserve_exact(text.len() + SUFFIX)
        .map_err(|_| Refusal::Failed)?;
    name.push_str(text);
    Ok(name)
}

/// The length of a `,xxx` suffix.
const SUFFIX: usize = 4;

/// The `,xxx` suffix `name` ends with, empty for none.
fn suffix_of(name: &str) -> &str {
    if Filetype::split_suffix(name).is_some() {
        &name[name.len() - SUFFIX..]
    } else {
        ""
    }
}

/// The host name `leaf` answers to in `dir`: an exact match, else the one
/// name that matches ignoring case; none where nothing does.
fn find<D: Dir>(dir: &D, leaf: &[u8]) -> Result<Option<String>, Refusal> {
    let name = crossed(leaf)?;
    match dir.open_node(&name) {
        Ok(_) => return Ok(Some(name)),
        Err(Errno::NotFound) => {}
        Err(errno) => return Err(refusal(errno)),
    }
    let mut exact: Option<String> = None;
    let mut folded: Option<String> = None;
    let mut ambiguous = false;
    for entry in dir.entries(ENTRIES_BOUND).map_err(refusal)? {
        let Some(spelt) = riscos_name(&entry.name) else {
            continue;
        };
        if spelt.as_bytes() == leaf {
            ambiguous |= exact.is_some();
            exact = Some(entry.name);
        } else if acorn::names_match(spelt.as_bytes(), leaf) {
            ambiguous |= folded.is_some();
            folded = Some(entry.name);
        }
    }
    match (exact, folded) {
        (Some(_), _) | (None, Some(_)) if ambiguous => Err(Refusal::Ambiguous),
        (Some(name), _) | (None, Some(name)) => Ok(Some(name)),
        (None, None) => Ok(None),
    }
}

/// `name` without the `,xxx` suffix that ends it.
fn stem(name: &str) -> &str {
    Filetype::split_suffix(name).map_or(name, |(stem, _)| stem)
}

/// The RISC OS leaf a host name `name` is listed as: its `,xxx` suffix taken
/// off, crossed; none for a name RISC OS could not hold.
fn riscos_name(name: &str) -> Option<acorn::Leaf> {
    let base = stem(name);
    let spelt = acorn::leaf_to_riscos(base.as_bytes()).ok()?;
    (spelt.as_bytes().len() <= NAME_MOST).then_some(spelt)
}

/// Every entry of `dir` RISC OS can name, as a listing gives them: in the
/// order `FileCore` keeps a directory's names, each with what it is.
fn listing<D: Dir>(dir: &D) -> Result<Vec<(Vec<u8>, Object)>, Refusal> {
    let mut entries = Vec::new();
    for entry in dir.entries(ENTRIES_BOUND).map_err(refusal)? {
        let Some(spelt) = riscos_name(&entry.name) else {
            continue;
        };
        let Ok(node) = dir.follow(&entry.name, false) else {
            continue;
        };
        let Ok(stat) = node.stat() else {
            continue;
        };
        let Ok(object) = object(&node, &stat, &entry.name) else {
            continue;
        };
        entries.try_reserve(1).map_err(|_| Refusal::Failed)?;
        entries.push((spelt.as_bytes().to_vec(), object));
    }
    entries.sort_by(|(a, _), (b, _)| acorn::name_order(a, b));
    Ok(entries)
}

/// The `acorn.*` key `key` as the tree seam takes one.
fn key(key: &'static [u8]) -> &'static str {
    core::str::from_utf8(key).unwrap_or_default()
}

/// What `node`, named `name` and statted `stat`, is to RISC OS.
fn object<N: Node>(node: &N, stat: &FileStat, name: &str) -> Result<Object, Refusal> {
    let kind = match stat.kind {
        FileKind::Directory => Kind::Directory,
        _ => Kind::File,
    };
    let length = match kind {
        Kind::File => u32::try_from(stat.size).map_err(|_| Refusal::TooLarge)?,
        _ => 0,
    };
    // A value another program wrote malformed is no metadata at all.
    let kept = |name: &'static [u8]| node.attr_value(key(name)).ok();
    let word =
        |name: &'static [u8]| kept(name).and_then(|value| acorn::addr_from_value(&value).ok());
    let (load, exec) =
        if let (Some(load), Some(exec)) = (word(acorn::LOADADDR_KEY), word(acorn::EXECADDR_KEY)) {
            (load, exec)
        } else {
            let filetype = match kind {
                Kind::Directory => Filetype::DATA.get(),
                _ => implied_type(name),
            };
            typed_stamp(filetype, stat.times.modified)?
        };
    let attributes = kept(acorn::ATTR_KEY)
        .and_then(|value| acorn::attr_from_value(&value).ok())
        .map_or_else(|| attributes_of_mode(stat.mode), acorn::attr_to_fileswitch);
    Ok(Object {
        kind,
        load,
        exec,
        length,
        attributes,
    })
}

/// The type a file named `name` has with no RISC OS metadata: its `,xxx`
/// suffix's, else the canonical one of what its name says it holds, else
/// Data.
fn implied_type(name: &str) -> u16 {
    match Filetype::split_suffix(name) {
        Some((_, filetype)) => filetype.get(),
        None => media_for_name(name)
            .and_then(canonical_filetype)
            .map_or(Filetype::DATA.get(), Filetype::get),
    }
}

/// The load and execution addresses of type `filetype` stamped `modified`.
fn typed_stamp(filetype: u16, modified: Time64) -> Result<(u32, u32), Refusal> {
    let centiseconds =
        acorn::time64_to_centiseconds(modified).map_err(|_| Refusal::Unrepresentable)?;
    acorn::encode_typed(Filetype::from_low_bits(filetype), centiseconds)
        .map_err(|_| Refusal::Unrepresentable)
}

/// The version of `stat`'s data its volume names, none where it names none.
fn version_of(stat: &FileStat) -> Option<Version> {
    (stat.content_gen != 0 && stat.id != FileId::default()).then_some(Version {
        volume: stat.id.volume,
        node: stat.id.node,
        generation: stat.content_gen,
    })
}

/// RISC OS's attributes for the mode bits `mode`.
fn attributes_of_mode(mode: u32) -> u8 {
    MODE_ATTRIBUTES
        .iter()
        .filter(|(bit, _)| mode & bit != 0)
        .fold(0, |bits, (_, attribute)| bits | attribute)
}

/// The mode bits `attributes` give, over `mode`'s others.
fn mode_of_attributes(mode: u32, attributes: u8) -> u32 {
    MODE_ATTRIBUTES
        .iter()
        .fold(mode, |mode, &(bit, attribute)| {
            if attributes & attribute != 0 {
                mode | bit
            } else {
                mode & !bit
            }
        })
}

/// What keeping a load and execution address and attributes on a node
/// leaves to do once its data has changed.
enum Stamping {
    /// Nothing: the volume keeps them whole, and has.
    Kept,
    /// Its modification time and mode bits, and the type its name must give.
    Stamp {
        filetype: Filetype,
        modified: Time64,
        attributes: u8,
    },
}

impl Stamping {
    /// The stamp written to `node`: the kind of node it is, none where
    /// nothing was left to write.
    fn finish<N: Node>(&self, node: &N) -> Result<Option<FileKind>, Refusal> {
        let Self::Stamp {
            modified,
            attributes,
            ..
        } = *self
        else {
            return Ok(None);
        };
        node.set_times(&TimesChange {
            accessed: StampChange::Keep,
            modified: StampChange::At(modified),
            nearest: true,
        })
        .map_err(refusal)?;
        let stat = node.stat().map_err(refusal)?;
        let mode = mode_of_attributes(stat.mode, attributes);
        if mode != stat.mode {
            node.set_mode(mode).map_err(refusal)?;
        }
        Ok(Some(stat.kind))
    }
}

/// The first of keeping `load`, `exec` and `attributes` on `node`, done
/// before its data changes so a refusal leaves it as it was: written as the
/// volume's `acorn.*` attributes where it keeps them, else checked to be
/// keepable as a stamp, and as a type its name gives where that name, `fixed`,
/// cannot change.
fn keep_first<N: Node>(
    node: &N,
    load: u32,
    exec: u32,
    attributes: u8,
    fixed: Option<&str>,
) -> Result<Stamping, Refusal> {
    let kept = node
        .attr_set(key(acorn::LOADADDR_KEY), &acorn::addr_to_value(load))
        .and_then(|()| node.attr_set(key(acorn::EXECADDR_KEY), &acorn::addr_to_value(exec)));
    match kept {
        Ok(()) => {
            let (value, length) = acorn::attr_to_value(acorn::attr_from_fileswitch(attributes))
                .map_err(|_| Refusal::Failed)?;
            node.attr_set(key(acorn::ATTR_KEY), &value[..length])
                .map_err(refusal)?;
            return Ok(Stamping::Kept);
        }
        Err(Errno::NotSupported) => {}
        Err(errno) => return Err(refusal(errno)),
    }
    if attributes & LOCKED != 0 {
        return Err(Refusal::Unrepresentable);
    }
    let LoadExec::Typed {
        filetype,
        centiseconds,
    } = acorn::decode_load_exec(load, exec)
    else {
        return Err(Refusal::Unrepresentable);
    };
    let modified =
        acorn::centiseconds_to_time64(centiseconds).map_err(|_| Refusal::Unrepresentable)?;
    if fixed.is_some_and(|name| implied_type(name) != filetype.get()) {
        return Err(Refusal::Unrepresentable);
    }
    Ok(Stamping::Stamp {
        filetype,
        modified,
        attributes,
    })
}

/// The rest of keeping on `name` in `dir`: the stamp written, and the type
/// given by a `,xxx` suffix where the name does not give it.
fn keep_rest<D: Dir>(dir: &D, name: &str, stamping: &Stamping) -> Result<(), Refusal> {
    let Stamping::Stamp { filetype, .. } = *stamping else {
        return Ok(());
    };
    let kind = stamping.finish(&dir.open_node(name).map_err(refusal)?)?;
    if kind == Some(FileKind::Directory) || implied_type(name) == filetype.get() {
        return Ok(());
    }
    let base = stem(name);
    let mut renamed = String::new();
    renamed
        .try_reserve_exact(base.len() + SUFFIX)
        .map_err(|_| Refusal::Failed)?;
    renamed.push_str(base);
    if media_for_name(base)
        .and_then(canonical_filetype)
        .map(Filetype::get)
        != Some(filetype.get())
    {
        write!(renamed, "{}", filetype.suffix()).map_err(|_| Refusal::Failed)?;
    }
    dir.rename(name, dir, &renamed, RenameFlags::NO_REPLACE, None)
        .map_err(refusal)
}

/// `load`, `exec` and `attributes` kept on `name` in `dir`, whose data is not
/// changing.
fn keep<D: Dir>(dir: &D, name: &str, load: u32, exec: u32, attributes: u8) -> Result<(), Refusal> {
    let stamping = keep_first(
        &dir.open_node(name).map_err(refusal)?,
        load,
        exec,
        attributes,
        None,
    )?;
    keep_rest(dir, name, &stamping)
}

/// `number`'s decimal digits appended to `out`.
fn push_decimal(out: &mut Vec<u8>, number: usize) {
    let mut digits = [0u8; 20];
    let mut at = digits.len();
    let mut left = number;
    loop {
        at -= 1;
        digits[at] = b'0' + u8::try_from(left % 10).unwrap_or(0);
        left /= 10;
        if left == 0 || at == 0 {
            break;
        }
    }
    out.extend_from_slice(&digits[at..]);
}

/// The name RISC OS reports for what the host refused.
fn refusal(errno: Errno) -> Refusal {
    match errno {
        Errno::NotFound | Errno::LinkLoop => Refusal::NotFound,
        Errno::PermissionDenied => Refusal::AccessViolation,
        Errno::NoSpace => Refusal::DiscFull,
        Errno::ReadOnly => Refusal::WriteProtected,
        Errno::NotEmpty => Refusal::NotEmpty,
        Errno::AlreadyExists => Refusal::Exists,
        Errno::IsADirectory => Refusal::IsADirectory,
        Errno::NotADirectory => Refusal::NotADirectory,
        Errno::LengthOutOfRange => Refusal::TooLarge,
        Errno::TimestampOutOfRange => Refusal::Unrepresentable,
        _ => Refusal::Failed,
    }
}

/// `name` as a disc's name: each character crossed as a leaf's are, each a
/// RISC OS name cannot hold `_`, as long as a leaf may be.
fn disc_name(name: &str) -> Vec<u8> {
    let mut disc = Vec::new();
    for ch in name.chars() {
        let byte = acorn::latin1_byte(ch)
            .map(acorn::crossed_name_byte)
            .filter(|&byte| acorn::is_name_byte(byte))
            .unwrap_or(b'_');
        if disc.len() == NAME_MOST {
            break;
        }
        disc.push(byte);
    }
    if disc.is_empty() {
        disc.push(b'_');
    }
    disc
}

/// `name`'s disc name, made unique among `taken` ignoring case by a number
/// after it.
pub(crate) fn unique_disc<'a, I: Iterator<Item = &'a [u8]> + Clone>(
    name: &str,
    taken: &I,
) -> Vec<u8> {
    let base = disc_name(name);
    let clash = |candidate: &[u8]| {
        taken
            .clone()
            .any(|held| acorn::names_match(held, candidate))
    };
    if !clash(&base) {
        return base;
    }
    let mut digits = Vec::new();
    for number in 2..=ROOTS_MOST + 1 {
        digits.clear();
        push_decimal(&mut digits, number);
        let mut candidate = base.clone();
        candidate.truncate(NAME_MOST - digits.len());
        candidate.extend_from_slice(&digits);
        if !clash(&candidate) {
            return candidate;
        }
    }
    base
}

#[cfg(test)]
mod tests;
