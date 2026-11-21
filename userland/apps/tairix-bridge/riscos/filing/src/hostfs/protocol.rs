//! What crosses HostFS's relay between the session worker and the side that
//! holds the files: requests on the session's roots and on the handles that
//! side minted, and their replies. Every field is bounded, so neither side
//! reads past what was sent, and a reply is believed only where it answers
//! what was asked, within what that request could produce.
//!
//! A path is RISC OS's spelling of a name beneath a root: leaves joined by
//! `.`, a dot within a leaf written `/`, empty for the root itself.

use alloc::vec::Vec;

use tairix_fsmeta::preset::acorn;

/// The most bytes one read or write carries.
pub const DATA_MOST: u32 = 64 * 1024;

/// The most entries one listing answers with.
pub const ENTRIES_MOST: u32 = 64;

/// The most roots a session has.
pub const ROOTS_MOST: usize = 64;

/// The longest path beneath a root a request names, in bytes: as long as the
/// longest name FileSwitch resolves.
pub const PATH_MOST: usize = crate::fileswitch::name::NAME_MOST;

/// The longest leaf, or disc name, in bytes.
pub const NAME_MOST: usize = acorn::LEAF_MAX;

/// What an object is, crossing the relay as the number `FSEntry_File` 5
/// gives it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Kind {
    /// Nothing is there.
    Nothing = 0,
    /// A file.
    File = 1,
    /// A directory.
    Directory = 2,
}

impl Kind {
    /// The number `FSEntry_File` 5 gives it.
    #[must_use]
    pub const fn number(self) -> u32 {
        self as u32
    }

    const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Nothing),
            1 => Some(Self::File),
            2 => Some(Self::Directory),
            _ => None,
        }
    }
}

/// An object as RISC OS sees it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Object {
    /// What it is.
    pub kind: Kind,
    /// Its load word.
    pub load: u32,
    /// Its execution word.
    pub exec: u32,
    /// Its length.
    pub length: u32,
    /// Its attributes, as FileSwitch's byte holds them.
    pub attributes: u8,
}

impl Object {
    /// Nothing, as an object read where none is.
    pub const NOTHING: Self = Self {
        kind: Kind::Nothing,
        load: 0,
        exec: 0,
        length: 0,
        attributes: 0,
    };
}

/// Why a request was refused: what RISC OS reports.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// Nothing is there.
    NotFound,
    /// The object is locked.
    Locked,
    /// The caller may not do that to it.
    AccessViolation,
    /// The volume is full.
    DiscFull,
    /// The root may not be written.
    WriteProtected,
    /// A name RISC OS could not have written.
    BadName,
    /// Two names differ only in case, and neither is the one asked for.
    Ambiguous,
    /// The directory holds something.
    NotEmpty,
    /// The name is taken.
    Exists,
    /// A file was asked for, and a directory is there.
    IsADirectory,
    /// A directory was asked for, and something else is there.
    NotADirectory,
    /// The file is longer than RISC OS can count.
    TooLarge,
    /// What is asked to be kept cannot be: an untyped stamp on a volume that
    /// keeps no RISC OS metadata.
    Unrepresentable,
    /// The root or handle is none the session holds.
    Unknown,
    /// Anything else the host refused.
    Failed,
}

const REFUSALS: [Refusal; 15] = [
    Refusal::NotFound,
    Refusal::Locked,
    Refusal::AccessViolation,
    Refusal::DiscFull,
    Refusal::WriteProtected,
    Refusal::BadName,
    Refusal::Ambiguous,
    Refusal::NotEmpty,
    Refusal::Exists,
    Refusal::IsADirectory,
    Refusal::NotADirectory,
    Refusal::TooLarge,
    Refusal::Unrepresentable,
    Refusal::Unknown,
    Refusal::Failed,
];

/// A request to the side that holds the files.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Request<'a> {
    /// The session's roots.
    Roots,
    /// The object `path` names.
    Read {
        /// The root, by its place in the session's roots.
        root: u32,
        /// The path beneath the root.
        path: &'a [u8],
    },
    /// Up to `count` entries of the directory `path` names, from `offset`.
    List {
        /// The root, by its place in the session's roots.
        root: u32,
        /// The path beneath the root.
        path: &'a [u8],
        /// The offset it starts at.
        offset: u32,
        /// The most entries wanted.
        count: u32,
    },
    /// The file `path` names opened, for update where `update`.
    Open {
        /// The root, by its place in the session's roots.
        root: u32,
        /// The path beneath the root.
        path: &'a [u8],
        /// Whether it is opened to be written too.
        update: bool,
    },
    /// `length` bytes of the file on `handle` from `offset`.
    ReadBytes {
        /// The handle the file was opened on.
        handle: u32,
        /// The offset it starts at.
        offset: u32,
        /// Its length in bytes.
        length: u32,
    },
    /// `bytes` written to the file on `handle` at `offset`.
    WriteBytes {
        /// The handle the file was opened on.
        handle: u32,
        /// The offset it starts at.
        offset: u32,
        /// The bytes.
        bytes: &'a [u8],
    },
    /// The file on `handle` made `extent` bytes long.
    SetExtent {
        /// The handle the file was opened on.
        handle: u32,
        /// The extent.
        extent: u32,
    },
    /// The file on `handle` put on its medium.
    Sync {
        /// The handle the file was opened on.
        handle: u32,
    },
    /// The file on `handle` closed, restamped `load` and `exec` where given.
    Close {
        /// The handle the file was opened on.
        handle: u32,
        /// The new load and execution words, if it is restamped.
        stamp: Option<(u32, u32)>,
    },
    /// A file `length` bytes of zeros made at `path`, stamped `load` and
    /// `exec`, or the one there emptied to it.
    Create {
        /// The root, by its place in the session's roots.
        root: u32,
        /// The path beneath the root.
        path: &'a [u8],
        /// The load word.
        load: u32,
        /// The execution word.
        exec: u32,
        /// Its length in bytes.
        length: u32,
    },
    /// A directory made at `path`, stamped `load` and `exec`.
    CreateDir {
        /// The root, by its place in the session's roots.
        root: u32,
        /// The path beneath the root.
        path: &'a [u8],
        /// The load word.
        load: u32,
        /// The execution word.
        exec: u32,
    },
    /// The object `path` names given `load`, `exec` and `attributes`.
    WriteInfo {
        /// The root, by its place in the session's roots.
        root: u32,
        /// The path beneath the root.
        path: &'a [u8],
        /// The load word.
        load: u32,
        /// The execution word.
        exec: u32,
        /// The attributes, as FileSwitch's byte holds them.
        attributes: u8,
    },
    /// The object `path` names deleted.
    Delete {
        /// The root, by its place in the session's roots.
        root: u32,
        /// The path beneath the root.
        path: &'a [u8],
    },
    /// The object `from` names renamed `to`, both beneath `root`.
    Rename {
        /// The root, by its place in the session's roots.
        root: u32,
        /// The object's path.
        from: &'a [u8],
        /// Its new path.
        to: &'a [u8],
    },
    /// The free and total space of the volume `root` lies on.
    Space {
        /// The root, by its place in the session's roots.
        root: u32,
    },
}

/// One version of a file's data: the file, and the generation of its data
/// its volume gave, which the volume never gives twice, so bytes read from
/// one version may be kept for as long as the version is the file's.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Version {
    /// The volume the file is on.
    pub volume: [u8; 16],
    /// The file's number there.
    pub node: u64,
    /// The generation, never zero.
    pub generation: u64,
}

/// A reply to a [`Request`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Reply<'a> {
    /// The session's roots, each its disc's name as RISC OS spells it, and
    /// the seed the worker keys its read cache by.
    Roots {
        /// The seed.
        seed: [u8; 16],
        /// Whether the first disc is the session's own, the user's RISC OS
        /// disc.
        own: bool,
        /// The discs' names.
        discs: Vec<Vec<u8>>,
    },
    /// An object, [`Kind::Nothing`] where none is.
    Object(Object),
    /// Entries of a directory, each its name and what it is, and where the
    /// next listing goes on from, `u32::MAX` once none are left.
    Entries {
        /// Where the next listing goes on from.
        next: u32,
        /// The entries.
        entries: Vec<(Vec<u8>, Object)>,
    },
    /// A file opened: its handle, its extent, whether it may be written, and
    /// the version of its data opened, none where its volume names none.
    Opened {
        /// The handle the file was opened on.
        handle: u32,
        /// The extent.
        extent: u32,
        /// Whether it may be written.
        writable: bool,
        /// The version.
        version: Option<Version>,
    },
    /// Bytes read, fewer than asked for only at the file's end.
    Bytes(&'a [u8]),
    /// How many bytes a write put down, fewer than given only where the
    /// volume filled.
    Wrote(u32),
    /// Done.
    Done,
    /// The volume's free and total bytes.
    Space {
        /// Free bytes.
        free: u64,
        /// Total bytes.
        total: u64,
    },
    /// Refused.
    Refused(Refusal),
}

/// Bytes appended to a message.
struct Out<'a>(&'a mut Vec<u8>);

impl Out<'_> {
    fn byte(&mut self, byte: u8) {
        self.0.push(byte);
    }

    fn word(&mut self, word: u32) {
        self.0.extend_from_slice(&word.to_le_bytes());
    }

    fn long(&mut self, long: u64) {
        self.0.extend_from_slice(&long.to_le_bytes());
    }

    fn words(&mut self, words: &[u32]) {
        for &word in words {
            self.word(word);
        }
    }

    /// A path on the root numbered `root`.
    fn at(&mut self, root: u32, path: &[u8]) {
        self.word(root);
        self.text(path);
    }

    /// `bytes` after their length, which the caller has bounded.
    fn text(&mut self, bytes: &[u8]) {
        let length = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        self.word(length);
        self.0.extend_from_slice(bytes);
    }

    fn object(&mut self, object: &Object) {
        self.byte(object.kind as u8);
        self.word(object.load);
        self.word(object.exec);
        self.word(object.length);
        self.byte(object.attributes);
    }
}

/// A message read from its start, every read refused past its end.
struct In<'a> {
    bytes: &'a [u8],
}

impl<'a> In<'a> {
    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let (taken, rest) = self.bytes.split_at_checked(count)?;
        self.bytes = rest;
        Some(taken)
    }

    fn byte(&mut self) -> Option<u8> {
        self.take(1).map(|taken| taken[0])
    }

    fn word(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn long(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    /// Bytes after their length, refused past `most`.
    fn text(&mut self, most: usize) -> Option<&'a [u8]> {
        let length = usize::try_from(self.word()?).ok()?;
        if length > most {
            return None;
        }
        self.take(length)
    }

    fn flag(&mut self) -> Option<bool> {
        match self.byte()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }

    fn object(&mut self) -> Option<Object> {
        Some(Object {
            kind: Kind::from_byte(self.byte()?)?,
            load: self.word()?,
            exec: self.word()?,
            length: self.word()?,
            attributes: self.byte()?,
        })
    }

    /// Disc names, each one RISC OS can hold, no more than a session has.
    fn discs(&mut self) -> Option<Vec<Vec<u8>>> {
        let count = usize::try_from(self.word()?).ok()?;
        if count > ROOTS_MOST {
            return None;
        }
        let mut discs = Vec::new();
        discs.try_reserve_exact(count).ok()?;
        for _ in 0..count {
            discs.push(self.leaf()?);
        }
        Some(discs)
    }

    /// Entries of a listing, each a name RISC OS can hold and its object, no
    /// more than `most`.
    fn entries(&mut self, most: u32) -> Option<Vec<(Vec<u8>, Object)>> {
        let count = self.word()?;
        if count > most {
            return None;
        }
        let mut entries = Vec::new();
        entries.try_reserve_exact(count as usize).ok()?;
        for _ in 0..count {
            entries.push((self.leaf()?, self.object()?));
        }
        Some(entries)
    }

    /// A name RISC OS can hold, as a leaf or a disc's name.
    fn leaf(&mut self) -> Option<Vec<u8>> {
        let leaf = self.text(NAME_MOST)?;
        acorn::leaf_from_riscos(leaf).ok()?;
        tairix_util::fallible::copied(leaf)
    }

    /// A version of a file's data.
    fn version(&mut self) -> Option<Version> {
        Some(Version {
            volume: self.take(16)?.try_into().ok()?,
            node: self.long()?,
            generation: self.long().filter(|&generation| generation != 0)?,
        })
    }

    /// Whatever was read, where nothing is left over.
    fn done<T>(self, read: T) -> Option<T> {
        self.bytes.is_empty().then_some(read)
    }
}

/// The byte each request opens with.
mod request_tag {
    pub(super) const ROOTS: u8 = 0;
    pub(super) const READ: u8 = 1;
    pub(super) const LIST: u8 = 2;
    pub(super) const OPEN: u8 = 3;
    pub(super) const READ_BYTES: u8 = 4;
    pub(super) const WRITE_BYTES: u8 = 5;
    pub(super) const SET_EXTENT: u8 = 6;
    pub(super) const SYNC: u8 = 7;
    pub(super) const CLOSE: u8 = 8;
    pub(super) const CREATE: u8 = 9;
    pub(super) const CREATE_DIR: u8 = 10;
    pub(super) const WRITE_INFO: u8 = 11;
    pub(super) const DELETE: u8 = 12;
    pub(super) const RENAME: u8 = 13;
    pub(super) const SPACE: u8 = 14;
}

/// The byte each reply opens with.
mod reply_tag {
    pub(super) const ROOTS: u8 = 0;
    pub(super) const OBJECT: u8 = 1;
    pub(super) const ENTRIES: u8 = 2;
    pub(super) const OPENED: u8 = 3;
    pub(super) const BYTES: u8 = 4;
    pub(super) const WROTE: u8 = 5;
    pub(super) const DONE: u8 = 6;
    pub(super) const SPACE: u8 = 7;
    pub(super) const REFUSED: u8 = 8;
}

impl<'a> Request<'a> {
    const fn tag(&self) -> u8 {
        match self {
            Self::Roots => request_tag::ROOTS,
            Self::Read { .. } => request_tag::READ,
            Self::List { .. } => request_tag::LIST,
            Self::Open { .. } => request_tag::OPEN,
            Self::ReadBytes { .. } => request_tag::READ_BYTES,
            Self::WriteBytes { .. } => request_tag::WRITE_BYTES,
            Self::SetExtent { .. } => request_tag::SET_EXTENT,
            Self::Sync { .. } => request_tag::SYNC,
            Self::Close { .. } => request_tag::CLOSE,
            Self::Create { .. } => request_tag::CREATE,
            Self::CreateDir { .. } => request_tag::CREATE_DIR,
            Self::WriteInfo { .. } => request_tag::WRITE_INFO,
            Self::Delete { .. } => request_tag::DELETE,
            Self::Rename { .. } => request_tag::RENAME,
            Self::Space { .. } => request_tag::SPACE,
        }
    }

    /// The request as its bytes, appended to `out`; the caller has bounded
    /// every field.
    pub fn encode(&self, out: &mut Vec<u8>) {
        let mut out = Out(out);
        out.byte(self.tag());
        match *self {
            Self::Roots => {}
            Self::Read { root, path } | Self::Delete { root, path } => out.at(root, path),
            Self::List {
                root,
                path,
                offset,
                count,
            } => {
                out.at(root, path);
                out.words(&[offset, count]);
            }
            Self::Open { root, path, update } => {
                out.at(root, path);
                out.byte(u8::from(update));
            }
            Self::ReadBytes {
                handle,
                offset,
                length,
            } => out.words(&[handle, offset, length]),
            Self::WriteBytes {
                handle,
                offset,
                bytes,
            } => {
                out.words(&[handle, offset]);
                out.text(bytes);
            }
            Self::SetExtent { handle, extent } => out.words(&[handle, extent]),
            Self::Sync { handle } => out.word(handle),
            Self::Close { handle, stamp } => {
                out.word(handle);
                out.byte(u8::from(stamp.is_some()));
                if let Some((load, exec)) = stamp {
                    out.words(&[load, exec]);
                }
            }
            Self::Create {
                root,
                path,
                load,
                exec,
                length,
            } => {
                out.at(root, path);
                out.words(&[load, exec, length]);
            }
            Self::CreateDir {
                root,
                path,
                load,
                exec,
            } => {
                out.at(root, path);
                out.words(&[load, exec]);
            }
            Self::WriteInfo {
                root,
                path,
                load,
                exec,
                attributes,
            } => {
                out.at(root, path);
                out.words(&[load, exec]);
                out.byte(attributes);
            }
            Self::Rename { root, from, to } => {
                out.at(root, from);
                out.text(to);
            }
            Self::Space { root } => out.word(root),
        }
    }

    /// The request `bytes` hold; none for anything malformed or out of
    /// bounds.
    #[must_use]
    pub fn decode(bytes: &'a [u8]) -> Option<Self> {
        let mut at = In { bytes };
        let request = match at.byte()? {
            request_tag::ROOTS => Self::Roots,
            request_tag::READ => Self::Read {
                root: at.word()?,
                path: at.text(PATH_MOST)?,
            },
            request_tag::LIST => Self::List {
                root: at.word()?,
                path: at.text(PATH_MOST)?,
                offset: at.word()?,
                count: at.word().filter(|&count| count <= ENTRIES_MOST)?,
            },
            request_tag::OPEN => Self::Open {
                root: at.word()?,
                path: at.text(PATH_MOST)?,
                update: at.flag()?,
            },
            request_tag::READ_BYTES => Self::ReadBytes {
                handle: at.word()?,
                offset: at.word()?,
                length: at.word().filter(|&length| length <= DATA_MOST)?,
            },
            request_tag::WRITE_BYTES => Self::WriteBytes {
                handle: at.word()?,
                offset: at.word()?,
                bytes: at.text(DATA_MOST as usize)?,
            },
            request_tag::SET_EXTENT => Self::SetExtent {
                handle: at.word()?,
                extent: at.word()?,
            },
            request_tag::SYNC => Self::Sync { handle: at.word()? },
            request_tag::CLOSE => Self::Close {
                handle: at.word()?,
                stamp: if at.flag()? {
                    Some((at.word()?, at.word()?))
                } else {
                    None
                },
            },
            request_tag::CREATE => Self::Create {
                root: at.word()?,
                path: at.text(PATH_MOST)?,
                load: at.word()?,
                exec: at.word()?,
                length: at.word()?,
            },
            request_tag::CREATE_DIR => Self::CreateDir {
                root: at.word()?,
                path: at.text(PATH_MOST)?,
                load: at.word()?,
                exec: at.word()?,
            },
            request_tag::WRITE_INFO => Self::WriteInfo {
                root: at.word()?,
                path: at.text(PATH_MOST)?,
                load: at.word()?,
                exec: at.word()?,
                attributes: at.byte()?,
            },
            request_tag::DELETE => Self::Delete {
                root: at.word()?,
                path: at.text(PATH_MOST)?,
            },
            request_tag::RENAME => Self::Rename {
                root: at.word()?,
                from: at.text(PATH_MOST)?,
                to: at.text(PATH_MOST)?,
            },
            request_tag::SPACE => Self::Space { root: at.word()? },
            _ => return None,
        };
        at.done(request)
    }
}

impl<'a> Reply<'a> {
    const fn tag(&self) -> u8 {
        match self {
            Self::Roots { .. } => reply_tag::ROOTS,
            Self::Object(_) => reply_tag::OBJECT,
            Self::Entries { .. } => reply_tag::ENTRIES,
            Self::Opened { .. } => reply_tag::OPENED,
            Self::Bytes(_) => reply_tag::BYTES,
            Self::Wrote(_) => reply_tag::WROTE,
            Self::Done => reply_tag::DONE,
            Self::Space { .. } => reply_tag::SPACE,
            Self::Refused(_) => reply_tag::REFUSED,
        }
    }

    /// The reply as its bytes, appended to `out`; the caller has bounded
    /// every field.
    pub fn encode(&self, out: &mut Vec<u8>) {
        let mut out = Out(out);
        out.byte(self.tag());
        match self {
            Self::Roots { seed, own, discs } => {
                out.0.extend_from_slice(seed);
                out.byte(u8::from(*own));
                out.word(u32::try_from(discs.len()).unwrap_or(u32::MAX));
                for disc in discs {
                    out.text(disc);
                }
            }
            Self::Object(object) => out.object(object),
            Self::Entries { next, entries } => {
                out.word(*next);
                out.word(u32::try_from(entries.len()).unwrap_or(u32::MAX));
                for (name, object) in entries {
                    out.text(name);
                    out.object(object);
                }
            }
            Self::Opened {
                handle,
                extent,
                writable,
                version,
            } => {
                out.words(&[*handle, *extent]);
                out.byte(u8::from(*writable));
                out.byte(u8::from(version.is_some()));
                if let Some(version) = version {
                    out.0.extend_from_slice(&version.volume);
                    out.long(version.node);
                    out.long(version.generation);
                }
            }
            Self::Bytes(bytes) => out.text(bytes),
            Self::Wrote(count) => out.word(*count),
            Self::Done => {}
            Self::Space { free, total } => {
                out.long(*free);
                out.long(*total);
            }
            Self::Refused(refusal) => {
                let index = REFUSALS
                    .iter()
                    .position(|r| r == refusal)
                    .unwrap_or(REFUSALS.len() - 1);
                out.byte(u8::try_from(index).unwrap_or(u8::MAX));
            }
        }
    }

    /// The reply `bytes` hold to `asked`: none for anything malformed, out of
    /// bounds, or more than `asked` could produce.
    #[must_use]
    pub fn decode(bytes: &'a [u8], asked: &Request<'_>) -> Option<Self> {
        let mut at = In { bytes };
        let reply = match at.byte()? {
            reply_tag::ROOTS => {
                let seed = at.take(16)?.try_into().ok()?;
                let own = at.flag()?;
                let discs = at.discs()?;
                if own && discs.is_empty() {
                    return None;
                }
                Self::Roots { seed, own, discs }
            }
            reply_tag::OBJECT => Self::Object(at.object()?),
            reply_tag::ENTRIES => {
                let Request::List { count: asked, .. } = *asked else {
                    return None;
                };
                Self::Entries {
                    next: at.word()?,
                    entries: at.entries(asked.min(ENTRIES_MOST))?,
                }
            }
            reply_tag::OPENED => {
                let Request::Open { update, .. } = *asked else {
                    return None;
                };
                let (handle, extent, writable) = (at.word()?, at.word()?, at.flag()?);
                if writable && !update {
                    return None;
                }
                let version = if at.flag()? {
                    Some(at.version()?)
                } else {
                    None
                };
                Self::Opened {
                    handle,
                    extent,
                    writable,
                    version,
                }
            }
            reply_tag::BYTES => {
                let Request::ReadBytes { length, .. } = *asked else {
                    return None;
                };
                Self::Bytes(at.text(length.min(DATA_MOST) as usize)?)
            }
            reply_tag::WROTE => {
                let Request::WriteBytes { bytes, .. } = *asked else {
                    return None;
                };
                let count = at.word()?;
                if count as usize > bytes.len() {
                    return None;
                }
                Self::Wrote(count)
            }
            reply_tag::DONE => Self::Done,
            reply_tag::SPACE => Self::Space {
                free: at.long()?,
                total: at.long()?,
            },
            reply_tag::REFUSED => Self::Refused(*REFUSALS.get(usize::from(at.byte()?))?),
            _ => return None,
        };
        let shaped = match (&reply, asked) {
            (Self::Refused(_), Request::Roots) => false,
            (Self::Refused(_), _)
            | (Self::Roots { .. }, Request::Roots)
            | (Self::Object(_), Request::Read { .. } | Request::Delete { .. })
            | (Self::Entries { .. }, Request::List { .. })
            | (Self::Opened { .. }, Request::Open { .. })
            | (Self::Bytes(_), Request::ReadBytes { .. })
            | (Self::Wrote(_), Request::WriteBytes { .. })
            | (Self::Space { .. }, Request::Space { .. })
            | (
                Self::Done,
                Request::SetExtent { .. }
                | Request::Sync { .. }
                | Request::Close { .. }
                | Request::Create { .. }
                | Request::CreateDir { .. }
                | Request::WriteInfo { .. }
                | Request::Rename { .. },
            ) => true,
            _ => false,
        };
        if !shaped {
            return None;
        }
        at.done(reply)
    }
}

#[cfg(test)]
mod tests;
