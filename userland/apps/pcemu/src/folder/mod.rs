//! Shared folders: a folder of the host the user granted, served to the
//! machine as a network-redirected drive (`plans/PCEMU.md` PC16.7).
//!
//! The front-end is the folder's only holder and serves it ([`server`]),
//! naming each host entry in 8.3 form ([`alias`]). The machine's DOS asks it
//! through a [`Folder`] in host terms — what a node is, its size and its
//! modification instant — and makes every DOS conversion itself. [`Op`] and
//! [`Reply`] are the one protocol both ways, with the codec the link carries
//! them in.

pub mod alias;
pub mod host;
#[cfg(any(test, feature = "test-util"))]
pub mod memhost;
pub mod server;

use tairix_abi::driver::filesystem::VolumeStats;
use tairix_abi::time::Time64;
use tairix_sandbox::wire::{Reader, Room, WireError, Writer};

use crate::dos::name::{self, is_forbidden, upper, PATH_MAX};

/// Folders one machine shares at once.
pub const SHARES: usize = 4;

/// Bytes a share path's spelling holds at most: what a DOS path holds past
/// its drive, its first backslash and its terminator.
const SPELLED: usize = PATH_MAX - 4;

/// Names a path within a share holds at most: one-letter names a backslash
/// apart, filling a DOS path.
pub const DEPTH: usize = SPELLED.div_ceil(2);

/// Bytes one read or write moves at most.
pub const IO_MOST: usize = 32 << 10;

/// A name in the eleven-byte form a directory entry holds: a base of eight
/// and an extension of three, each padded with spaces, in upper case.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DosName([u8; 11]);

impl DosName {
    /// A directory's entry for itself.
    pub const DOT: Self = Self(*b".          ");

    /// A directory's entry for the directory holding it.
    pub const DOT_DOT: Self = Self(*b"..         ");

    /// `field` where DOS could have made it: each part a run of bytes a name
    /// may hold, in upper case, then only spaces, the base not empty, and a
    /// `?` only where `wild`. A first byte of `05h` stands for `E5h`, which
    /// marks an entry free.
    #[must_use]
    pub fn new(field: [u8; 11], wild: bool) -> Option<Self> {
        if field == Self::DOT.0 || field == Self::DOT_DOT.0 {
            return Some(Self(field));
        }
        let allowed = |b: u8| {
            (b == b'?' && wild)
                || (b != b'?' && b != b'*' && b != b'.' && !is_forbidden(b) && upper(b) == b)
        };
        let part = |bytes: &[u8], first: usize| {
            let used = bytes.iter().position(|&b| b == b' ').unwrap_or(bytes.len());
            let shaped = bytes[used..].iter().all(|&b| b == b' ')
                && bytes[..used]
                    .iter()
                    .enumerate()
                    .all(|(at, &b)| (at + first == 0 && b == 0x05) || allowed(b));
            shaped.then_some(used)
        };
        let base = part(&field[..8], 0)?;
        part(&field[8..], 8)?;
        (base > 0).then_some(Self(field))
    }

    /// Its eleven bytes.
    #[must_use]
    pub const fn bytes(&self) -> &[u8; 11] {
        &self.0
    }

    /// Whether it holds a wildcard, so it names a pattern rather than a node.
    #[must_use]
    pub fn is_wild(&self) -> bool {
        self.0.contains(&b'?')
    }

    /// Whether it is a directory's entry for itself or its parent, which no
    /// path names.
    #[must_use]
    pub fn is_dots(&self) -> bool {
        *self == Self::DOT || *self == Self::DOT_DOT
    }

    /// Whether `name` is one this pattern matches.
    #[must_use]
    pub fn matches(&self, name: &Self) -> bool {
        crate::dos::name::matches(&self.0, &name.0)
    }

    /// Decode one, a pattern only where `wild`.
    pub(crate) fn decode(r: &mut Reader<'_>, wild: bool) -> Result<Self, WireError> {
        let mut field = [0u8; 11];
        field.copy_from_slice(r.take(11)?);
        Self::new(field, wild).ok_or(WireError::Malformed)
    }
}

/// A path within a share: the names from its root, none for the root, held
/// as DOS spells them — each `NAME.EXT`, a backslash between — so a path is
/// as small as the longest DOS names, not a field of eleven bytes a name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SharePath {
    spelled: [u8; SPELLED],
    len: u8,
}

impl SharePath {
    /// The share's root.
    pub const ROOT: Self = Self {
        spelled: [0; SPELLED],
        len: 0,
    };

    /// The path `names` spell, if DOS could name it and none is a pattern or
    /// a dotted entry.
    #[must_use]
    pub fn of(names: &[DosName]) -> Option<Self> {
        names
            .iter()
            .try_fold(Self::ROOT, |path, &name| path.child(name))
    }

    /// The path of `name` in this directory, if DOS could name it.
    #[must_use]
    pub fn child(&self, name: DosName) -> Option<Self> {
        if name.is_wild() || name.is_dots() {
            return None;
        }
        let mut spelled = [0u8; 13];
        let len = name::spell(name.bytes(), &mut spelled);
        let mut path = *self;
        let mut at = usize::from(self.len);
        if !self.is_root() {
            *path.spelled.get_mut(at)? = b'\\';
            at += 1;
        }
        path.spelled
            .get_mut(at..at + len)?
            .copy_from_slice(&spelled[..len]);
        path.len = u8::try_from(at + len).ok()?;
        Some(path)
    }

    /// Its names, from the root.
    pub fn names(&self) -> impl Iterator<Item = DosName> + '_ {
        self.spelled[..usize::from(self.len)]
            .split(|&byte| byte == b'\\')
            .filter(|part| !part.is_empty())
            .filter_map(|part| DosName::new(name::field(part, false).ok()?, false))
    }

    /// Whether it is the share's root.
    #[must_use]
    pub const fn is_root(&self) -> bool {
        self.len == 0
    }

    /// The directory it lies in and its last name, or `None` for the root.
    #[must_use]
    pub fn split_last(&self) -> Option<(Self, DosName)> {
        let spelled = &self.spelled[..usize::from(self.len)];
        let (parent, last) = match spelled.iter().rposition(|&byte| byte == b'\\') {
            Some(at) => (at, &spelled[at + 1..]),
            None if spelled.is_empty() => return None,
            None => (0, spelled),
        };
        let last = DosName::new(name::field(last, false).ok()?, false)?;
        let mut dir = *self;
        dir.spelled[parent..].fill(0);
        dir.len = u8::try_from(parent).ok()?;
        Some((dir, last))
    }

    fn encode<R: Room>(&self, w: &mut Writer<R>) {
        w.u8(self.len);
        w.raw(&self.spelled[..usize::from(self.len)]);
    }

    /// Decode one, refusing any spelling but the one its names make.
    fn decode(r: &mut Reader<'_>) -> Result<Self, WireError> {
        let len = usize::from(r.u8()?);
        if len > SPELLED {
            return Err(WireError::Malformed);
        }
        let spelled = r.take(len)?;
        let mut path = Self::ROOT;
        if len > 0 {
            for part in spelled.split(|&byte| byte == b'\\') {
                let name = name::field(part, false)
                    .ok()
                    .and_then(|field| DosName::new(field, false))
                    .ok_or(WireError::Malformed)?;
                path = path.child(name).ok_or(WireError::Malformed)?;
            }
        }
        if path.spelled[..usize::from(path.len)] != *spelled {
            return Err(WireError::Malformed);
        }
        Ok(path)
    }
}

/// What a node of the folder is, as the host has it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Facts {
    /// Its name in the directory it lies in; the root's is the share's label.
    pub name: DosName,
    /// Whether it is a directory.
    pub directory: bool,
    /// Its length in bytes, `0` for a directory.
    pub size: u32,
    /// When its contents last changed, on the machine's clock: the host's
    /// instant moved by the machine's offset, which DOS reads as its local
    /// time.
    pub modified: Time64,
    /// Whether the user may change it through the share.
    pub writable: bool,
    /// Whether the host hides it: a name beginning with a dot.
    pub hidden: bool,
}

impl Facts {
    fn encode<R: Room>(&self, w: &mut Writer<R>) {
        w.raw(self.name.bytes());
        w.u8(u8::from(self.directory) | u8::from(self.writable) << 1 | u8::from(self.hidden) << 2);
        w.u32(self.size);
        put_time(w, self.modified);
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, WireError> {
        let name = DosName::decode(r, false)?;
        let flags = r.u8()?;
        if flags & !0b111 != 0 {
            return Err(WireError::Malformed);
        }
        let size = r.u32()?;
        let directory = flags & 1 != 0;
        if directory && size != 0 {
            return Err(WireError::Malformed);
        }
        Ok(Self {
            name,
            directory,
            size,
            modified: time(r)?,
            writable: flags & 2 != 0,
            hidden: flags & 4 != 0,
        })
    }
}

/// What an open may do with the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Read it.
    Read,
    /// Write it.
    Write,
    /// Both.
    Both,
}

impl Access {
    /// Whether it writes.
    #[must_use]
    pub const fn writes(self) -> bool {
        !matches!(self, Self::Read)
    }
}

/// What an open does where the name is free, or taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Creation {
    /// Open the file the name names; refused where it names none.
    Existing,
    /// Make the file, or empty the one the name names.
    Truncate,
    /// Make the file; refused where the name is taken.
    New,
}

/// Where a search goes on from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum From {
    /// The first entry of directory `dir`, listed afresh.
    Start {
        /// The directory.
        dir: SharePath,
    },
    /// The entry after the one an earlier answer found: the place is all
    /// DOS keeps of a search, so a listing let go meanwhile finds no more.
    Next {
        /// The listing it found in.
        table: u32,
        /// The place after it.
        index: u32,
    },
}

/// The DOS attribute bits a search asks for beside a plain file's.
pub mod wanted {
    /// Hidden files.
    pub const HIDDEN: u8 = 0x02;
    /// Directories.
    pub const DIRECTORY: u8 = 0x10;
}

/// What the machine asks of a folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op<'a> {
    /// What the node `path` names is.
    Look {
        /// The node.
        path: SharePath,
    },
    /// The next entry whose name `pattern` matches, of a kind `attributes`
    /// admits ([`wanted`]); a directory other than the root lists itself and
    /// its parent first, as `.` and `..`, where directories are wanted.
    Find {
        /// The names wanted.
        pattern: DosName,
        /// The kinds wanted beside plain files.
        attributes: u8,
        /// Where to go on from.
        from: From,
    },
    /// Open the file `path` names.
    Open {
        /// The file.
        path: SharePath,
        /// What may be done with it.
        access: Access,
        /// What happens where the name is free or taken.
        creation: Creation,
    },
    /// Read `len` bytes, at most [`IO_MOST`], from `offset`.
    Read {
        /// The open file.
        handle: u32,
        /// Where.
        offset: u32,
        /// How many.
        len: u32,
    },
    /// Write `bytes`, at most [`IO_MOST`], at `offset`.
    Write {
        /// The open file.
        handle: u32,
        /// Where.
        offset: u32,
        /// What.
        bytes: &'a [u8],
    },
    /// Cut or extend the file to `size` bytes.
    Resize {
        /// The open file.
        handle: u32,
        /// Its length.
        size: u32,
    },
    /// Close the file, stamping its modification at `stamp` first.
    Close {
        /// The open file.
        handle: u32,
        /// The instant DOS set, on the machine's clock, where it set one.
        stamp: Option<Time64>,
    },
    /// Put what was written to the file on its medium.
    Commit {
        /// The open file.
        handle: u32,
    },
    /// How long the file is now.
    Size {
        /// The open file.
        handle: u32,
    },
    /// Remove the file — or the empty directory, where `directory` — `path`
    /// names.
    Remove {
        /// The node.
        path: SharePath,
        /// Whether it is a directory.
        directory: bool,
    },
    /// Give the node `from` names the name `to`, which nothing holds.
    Rename {
        /// The node.
        from: SharePath,
        /// Its new place.
        to: SharePath,
    },
    /// Make the directory `path`.
    MakeDir {
        /// The directory.
        path: SharePath,
    },
    /// The space of the volume the folder lies on.
    Space,
}

/// A folder's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply<'a> {
    /// It is done.
    Done,
    /// What the node is.
    Looked(Facts),
    /// The entry found, and where the next search of the listing goes on
    /// from.
    Found {
        /// The entry.
        facts: Facts,
        /// The listing it was found in.
        table: u32,
        /// The place after it.
        next: u32,
    },
    /// The file is open.
    Opened {
        /// The handle it is reached through.
        handle: u32,
        /// The node it is, the same for every handle on it.
        file: u32,
        /// What it is now.
        facts: Facts,
    },
    /// The bytes read: fewer than asked at the file's end.
    Read(&'a [u8]),
    /// How many bytes were written: fewer than offered when the volume is
    /// full.
    Wrote(u32),
    /// How long the file is.
    Size(u32),
    /// The volume's space.
    Space(VolumeStats),
}

impl<'a> Reply<'a> {
    /// The reply with nothing borrowed, or for a read the bytes it lends.
    ///
    /// # Errors
    ///
    /// A read's bytes, which only the caller can keep.
    pub const fn detach(self) -> Result<Reply<'static>, &'a [u8]> {
        Ok(match self {
            Self::Read(bytes) => return Err(bytes),
            Self::Done => Reply::Done,
            Self::Looked(facts) => Reply::Looked(facts),
            Self::Found { facts, table, next } => Reply::Found { facts, table, next },
            Self::Opened {
                handle,
                file,
                facts,
            } => Reply::Opened {
                handle,
                file,
                facts,
            },
            Self::Wrote(count) => Reply::Wrote(count),
            Self::Size(size) => Reply::Size(size),
            Self::Space(stats) => Reply::Space(stats),
        })
    }
}

/// Why a folder refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The last name names nothing.
    NotFound,
    /// A directory on the way names nothing, or no directory.
    PathNotFound,
    /// The share, the host or the node's kind refuses it.
    Denied,
    /// The name is taken.
    Exists,
    /// The search found nothing more.
    NoMore,
    /// The directory holds entries.
    NotEmpty,
    /// The volume is full.
    Full,
    /// The share has as many files open as it holds.
    TooMany,
    /// The handle names no file open through the share.
    Handle,
    /// The host failed.
    Fault,
    /// The folder is no longer shared.
    Gone,
}

const REFUSALS: [Refusal; 11] = [
    Refusal::NotFound,
    Refusal::PathNotFound,
    Refusal::Denied,
    Refusal::Exists,
    Refusal::NoMore,
    Refusal::NotEmpty,
    Refusal::Full,
    Refusal::TooMany,
    Refusal::Handle,
    Refusal::Fault,
    Refusal::Gone,
];

/// A folder the machine shares as a network drive.
pub struct Share {
    /// The label its root carries.
    pub label: DosName,
    /// Whether it may be changed.
    pub writable: bool,
    /// The drive DOS gave it, `0` for `A:`.
    pub drive: Option<u8>,
    /// Whether the front-end was told the drive it is now.
    pub announced: bool,
    /// The folder.
    pub folder: alloc::boxed::Box<dyn Folder>,
    /// What a read lands in on its way to guest memory, kept for the next.
    pub scratch: alloc::vec::Vec<u8>,
}

impl core::fmt::Debug for Share {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Share")
            .field("label", &self.label)
            .field("writable", &self.writable)
            .field("drive", &self.drive)
            .field("announced", &self.announced)
            .finish_non_exhaustive()
    }
}

impl Share {
    /// Share `folder`, labelled `label`, as no drive yet.
    #[must_use]
    pub fn new(label: DosName, writable: bool, folder: alloc::boxed::Box<dyn Folder>) -> Self {
        Self {
            label,
            writable,
            drive: None,
            announced: false,
            folder,
            scratch: alloc::vec::Vec::new(),
        }
    }

    /// It has no drive now: the front-end is told, where it had one.
    pub fn lose_drive(&mut self) {
        if self.drive.take().is_some() {
            self.announced = false;
        }
    }
}

/// A folder the machine reaches: the front-end's server, through the link
/// or, in tests, directly.
pub trait Folder {
    /// Answer `op`; a reply's bytes are the folder's until its next call.
    ///
    /// # Errors
    ///
    /// Why it refused.
    fn call(&mut self, op: &Op<'_>) -> Result<Reply<'_>, Refusal>;
}

mod tag {
    pub const LOOK: u8 = 1;
    pub const FIND: u8 = 2;
    pub const OPEN: u8 = 3;
    pub const READ: u8 = 4;
    pub const WRITE: u8 = 5;
    pub const RESIZE: u8 = 6;
    pub const CLOSE: u8 = 7;
    pub const COMMIT: u8 = 8;
    pub const REMOVE: u8 = 9;
    pub const RENAME: u8 = 10;
    pub const MAKE_DIR: u8 = 11;
    pub const SPACE: u8 = 12;
    pub const SIZE: u8 = 13;

    pub const DONE: u8 = 1;
    pub const LOOKED: u8 = 2;
    pub const FOUND: u8 = 3;
    pub const OPENED: u8 = 4;
    pub const READ_BYTES: u8 = 5;
    pub const WROTE: u8 = 6;
    pub const SPACE_LEFT: u8 = 7;
    pub const SIZED: u8 = 8;
}

impl<'a> Op<'a> {
    /// Encode it.
    pub fn encode<R: Room>(&self, w: &mut Writer<R>) {
        match *self {
            Self::Look { path } => {
                w.u8(tag::LOOK);
                path.encode(w);
            }
            Self::Find {
                pattern,
                attributes,
                from,
            } => {
                w.u8(tag::FIND);
                w.raw(pattern.bytes());
                w.u8(attributes);
                match from {
                    From::Start { dir } => {
                        w.u8(0);
                        dir.encode(w);
                    }
                    From::Next { table, index } => {
                        w.u8(1);
                        w.u32(table);
                        w.u32(index);
                    }
                }
            }
            Self::Open {
                path,
                access,
                creation,
            } => {
                w.u8(tag::OPEN);
                path.encode(w);
                w.u8(access as u8);
                w.u8(creation as u8);
            }
            Self::Read {
                handle,
                offset,
                len,
            } => {
                w.u8(tag::READ);
                w.u32(handle);
                w.u32(offset);
                w.u32(len);
            }
            Self::Write {
                handle,
                offset,
                bytes,
            } => {
                w.u8(tag::WRITE);
                w.u32(handle);
                w.u32(offset);
                w.bytes(bytes);
            }
            Self::Resize { handle, size } => {
                w.u8(tag::RESIZE);
                w.u32(handle);
                w.u32(size);
            }
            Self::Close { handle, stamp } => {
                w.u8(tag::CLOSE);
                w.u32(handle);
                match stamp {
                    Some(at) => {
                        w.u8(1);
                        put_time(w, at);
                    }
                    None => w.u8(0),
                }
            }
            Self::Commit { handle } => {
                w.u8(tag::COMMIT);
                w.u32(handle);
            }
            Self::Size { handle } => {
                w.u8(tag::SIZE);
                w.u32(handle);
            }
            Self::Remove { path, directory } => {
                w.u8(tag::REMOVE);
                path.encode(w);
                w.u8(u8::from(directory));
            }
            Self::Rename { from, to } => {
                w.u8(tag::RENAME);
                from.encode(w);
                to.encode(w);
            }
            Self::MakeDir { path } => {
                w.u8(tag::MAKE_DIR);
                path.encode(w);
            }
            Self::Space => w.u8(tag::SPACE),
        }
    }

    /// Decode one, every field in shape.
    ///
    /// # Errors
    ///
    /// [`WireError`] for bytes that are not one.
    pub fn decode(r: &mut Reader<'a>) -> Result<Self, WireError> {
        Ok(match r.u8()? {
            tag::LOOK => Self::Look {
                path: SharePath::decode(r)?,
            },
            tag::FIND => Self::Find {
                pattern: DosName::decode(r, true)?,
                attributes: r.u8()?,
                from: match r.u8()? {
                    0 => From::Start {
                        dir: SharePath::decode(r)?,
                    },
                    1 => From::Next {
                        table: r.u32()?,
                        index: r.u32()?,
                    },
                    _ => return Err(WireError::Malformed),
                },
            },
            tag::OPEN => Self::Open {
                path: SharePath::decode(r)?,
                access: match r.u8()? {
                    0 => Access::Read,
                    1 => Access::Write,
                    2 => Access::Both,
                    _ => return Err(WireError::Malformed),
                },
                creation: match r.u8()? {
                    0 => Creation::Existing,
                    1 => Creation::Truncate,
                    2 => Creation::New,
                    _ => return Err(WireError::Malformed),
                },
            },
            tag::READ => {
                let (handle, offset, len) = (r.u32()?, r.u32()?, r.u32()?);
                if len == 0 || len as usize > IO_MOST {
                    return Err(WireError::Malformed);
                }
                Self::Read {
                    handle,
                    offset,
                    len,
                }
            }
            tag::WRITE => {
                let (handle, offset) = (r.u32()?, r.u32()?);
                let bytes = r.bytes(IO_MOST)?;
                if bytes.is_empty() {
                    return Err(WireError::Malformed);
                }
                Self::Write {
                    handle,
                    offset,
                    bytes,
                }
            }
            tag::RESIZE => Self::Resize {
                handle: r.u32()?,
                size: r.u32()?,
            },
            tag::CLOSE => Self::Close {
                handle: r.u32()?,
                stamp: if crate::link::flag(r)? {
                    Some(time(r)?)
                } else {
                    None
                },
            },
            tag::COMMIT => Self::Commit { handle: r.u32()? },
            tag::SIZE => Self::Size { handle: r.u32()? },
            tag::REMOVE => {
                let path = SharePath::decode(r)?;
                if path.len == 0 {
                    return Err(WireError::Malformed);
                }
                Self::Remove {
                    path,
                    directory: crate::link::flag(r)?,
                }
            }
            tag::RENAME => {
                let (from, to) = (SharePath::decode(r)?, SharePath::decode(r)?);
                if from.len == 0 || to.len == 0 {
                    return Err(WireError::Malformed);
                }
                Self::Rename { from, to }
            }
            tag::MAKE_DIR => {
                let path = SharePath::decode(r)?;
                if path.len == 0 {
                    return Err(WireError::Malformed);
                }
                Self::MakeDir { path }
            }
            tag::SPACE => Self::Space,
            _ => return Err(WireError::Malformed),
        })
    }
}

impl<'a> Op<'a> {
    /// The op with nothing borrowed, and the bytes a write lends.
    #[must_use]
    pub const fn detach(self) -> (Op<'static>, Option<&'a [u8]>) {
        match self {
            Self::Write {
                handle,
                offset,
                bytes,
            } => (
                Op::Write {
                    handle,
                    offset,
                    bytes: &[],
                },
                Some(bytes),
            ),
            Self::Look { path } => (Op::Look { path }, None),
            Self::Find {
                pattern,
                attributes,
                from,
            } => (
                Op::Find {
                    pattern,
                    attributes,
                    from,
                },
                None,
            ),
            Self::Open {
                path,
                access,
                creation,
            } => (
                Op::Open {
                    path,
                    access,
                    creation,
                },
                None,
            ),
            Self::Read {
                handle,
                offset,
                len,
            } => (
                Op::Read {
                    handle,
                    offset,
                    len,
                },
                None,
            ),
            Self::Resize { handle, size } => (Op::Resize { handle, size }, None),
            Self::Close { handle, stamp } => (Op::Close { handle, stamp }, None),
            Self::Commit { handle } => (Op::Commit { handle }, None),
            Self::Size { handle } => (Op::Size { handle }, None),
            Self::Remove { path, directory } => (Op::Remove { path, directory }, None),
            Self::Rename { from, to } => (Op::Rename { from, to }, None),
            Self::MakeDir { path } => (Op::MakeDir { path }, None),
            Self::Space => (Op::Space, None),
        }
    }
}

/// Encode a folder's answer.
pub fn encode_answer<R: Room>(answer: &Result<Reply<'_>, Refusal>, w: &mut Writer<R>) {
    match answer {
        Err(refusal) => {
            w.u8(0);
            w.u8(*refusal as u8);
        }
        Ok(Reply::Done) => w.u8(tag::DONE),
        Ok(Reply::Looked(facts)) => {
            w.u8(tag::LOOKED);
            facts.encode(w);
        }
        Ok(Reply::Found { facts, table, next }) => {
            w.u8(tag::FOUND);
            facts.encode(w);
            w.u32(*table);
            w.u32(*next);
        }
        Ok(Reply::Opened {
            handle,
            file,
            facts,
        }) => {
            w.u8(tag::OPENED);
            w.u32(*handle);
            w.u32(*file);
            facts.encode(w);
        }
        Ok(Reply::Read(bytes)) => {
            w.u8(tag::READ_BYTES);
            w.bytes(bytes);
        }
        Ok(Reply::Wrote(count)) => {
            w.u8(tag::WROTE);
            w.u32(*count);
        }
        Ok(Reply::Size(size)) => {
            w.u8(tag::SIZED);
            w.u32(*size);
        }
        Ok(Reply::Space(stats)) => {
            w.u8(tag::SPACE_LEFT);
            w.raw(&stats.to_le_bytes());
        }
    }
}

/// Decode a folder's answer, every field in shape.
///
/// # Errors
///
/// [`WireError`] for bytes that are not one.
pub fn decode_answer<'a>(r: &mut Reader<'a>) -> Result<Result<Reply<'a>, Refusal>, WireError> {
    Ok(Ok(match r.u8()? {
        0 => {
            let refusal = REFUSALS
                .get(usize::from(r.u8()?))
                .copied()
                .ok_or(WireError::Malformed)?;
            return Ok(Err(refusal));
        }
        tag::DONE => Reply::Done,
        tag::LOOKED => Reply::Looked(Facts::decode(r)?),
        tag::FOUND => Reply::Found {
            facts: Facts::decode(r)?,
            table: r.u32()?,
            next: r.u32()?,
        },
        tag::OPENED => Reply::Opened {
            handle: r.u32()?,
            file: r.u32()?,
            facts: Facts::decode(r)?,
        },
        tag::READ_BYTES => Reply::Read(r.bytes(IO_MOST)?),
        tag::WROTE => {
            let count = r.u32()?;
            if count as usize > IO_MOST {
                return Err(WireError::Malformed);
            }
            Reply::Wrote(count)
        }
        tag::SIZED => Reply::Size(r.u32()?),
        tag::SPACE_LEFT => {
            let mut stats = [0u8; VolumeStats::WIRE_LEN];
            stats.copy_from_slice(r.take(VolumeStats::WIRE_LEN)?);
            Reply::Space(VolumeStats::from_le_bytes(&stats).map_err(|_| WireError::Malformed)?)
        }
        _ => return Err(WireError::Malformed),
    }))
}

fn put_time<R: Room>(w: &mut Writer<R>, at: Time64) {
    w.u64(at.secs().cast_unsigned());
    w.u32(at.subsec_nanos());
}

fn time(r: &mut Reader<'_>) -> Result<Time64, WireError> {
    let secs = r.u64()?.cast_signed();
    Time64::new(secs, r.u32()?).map_err(|_| WireError::Malformed)
}

#[cfg(test)]
#[path = "folder_tests.rs"]
mod tests;
