//! TAIRiX Acorn ADFS filesystem driver (read/write).
//!
//! Attaches an Acorn ADFS / RISC OS `FileCore` volume sitting behind any
//! [`tairix_abi::driver::block::Block`] device and exposes it through the
//! versioned [`FilesystemRead`], [`FilesystemWrite`], [`FilesystemAttrs`],
//! [`FilesystemStats`] and [`FilesystemLabel`]
//! surfaces (new behaviour ships as a new trait, never by widening the
//! frozen mount/unmount
//! [`Filesystem`](tairix_abi::driver::filesystem::Filesystem)). It reports
//! its single stored datestamp as the node's `created`/`modified`/
//! `accessed`/`changed` times through
//! [`NodeInfo::times`](tairix_abi::driver::filesystem::NodeInfo::times).
//!
//! # Supported formats
//!
//! Every ADFS on-disk format is supported, for reading and writing:
//!
//! * **Old map** volumes — the S (160 KiB), M (320 KiB), and L (640 KiB)
//!   floppies with 1280-byte `Hugo` directories, the D (800 KiB) floppy
//!   with 2048-byte directories, and old-map hard discs. The free-space
//!   map lives in sectors 0–1 and every object is one contiguous run of
//!   256-byte sectors.
//! * **New map** volumes — the E (800 KiB) and F (1600 KiB) floppies and
//!   new-map hard discs, where a multi-zone allocation map of
//!   variable-length fragments backs fragmented objects, and small
//!   objects may share a fragment through the low byte of their indirect
//!   disc address.
//! * **Big directories** — the E+/F+ variable-length directory format
//!   (`SBPr`/`oven` markers with a name heap), alongside the fixed
//!   2048-byte `Hugo`/`Nick` new directories and the 1280-byte old
//!   directories.
//!
//! ADFS has no per-inode owner, mode, ACL, or capability gate; those live
//! in the VFS metadata layer that mounts this driver. The driver therefore
//! makes **no** permission decisions (the VFS is the policy point, this is
//! raw structural I/O), and a create ignores the record it is given.
//!
//! # Pins
//!
//! An object the kernel pins ([`FilesystemRead::pin`]) is reported under a
//! token until its last unpin, since its own id moves when a write relocates
//! it and a directory's carries its parent's address. A removal deletes a
//! pinned object's entry but keeps its fragments until then.
//!
//! # RISC OS metadata
//!
//! Load/exec addresses, the 12-bit filetype, the 40-bit centisecond
//! datestamp, and the `FileCore` attribute bits are surfaced through the
//! shared `tairix_fsmeta` Acorn preset as the canonical `acorn.*`
//! attribute keys, so a copy to `ARXFS` and back is byte-exact.
//!
//! # Public surface
//!
//! The only public *function* is [`register`]. [`Adfs`] is a public *type*
//! the driver host instantiates with [`Adfs::open`] (or formats with
//! [`Adfs::format`]); the host reaches into it only through the
//! filesystem traits.
//!
//! # Capabilities
//!
//! Loading requires
//! [`CapabilityId::DRV_LOAD`](tairix_abi::CapabilityId::DRV_LOAD). The
//! driver runs in user space; it does not request `CAP_DRV_KERNEL`.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use tairix_abi::driver::block::Block;
use tairix_abi::driver::filesystem::{kept_as_stamp, unkept_stamp};
use tairix_abi::driver::filesystem::{
    DirEntry, DirVisit, FilesystemAttrs, FilesystemAttrsFs, FilesystemAttrsProvider,
    FilesystemLabel, FilesystemRead, FilesystemSecurity, FilesystemStats, FilesystemWrite,
    NameMatching, NodeId, NodeIdentity, NodeInfo, NodeKind, NodeSecurity, NodeTimes, Unpinned,
    VolumeHost, VolumeStats, VolumeTitle, WriteRefusal,
};
use tairix_abi::fs::{StampChange, TimesChange, TimesKept};
use tairix_abi::time::Time64;
use tairix_abi::{CapabilityId, DriverError, DriverHandle, DriverHost};
use tairix_bytedev::ByteDevice;
use tairix_collections::{HashMap, RadixTree};
use tairix_fsmeta::preset::acorn;
use tairix_fsprobe::adfs::Layout;
use tairix_hash::BuildSipHash13;

mod served;

#[cfg(test)]
mod tests;

use tairix_filecore::dir::{FixedFormat, Object};
use tairix_filecore::newmap::NewMap;
use tairix_filecore::record::{NAME_LEN, OLD_SECTOR_SIZE};
use tairix_filecore::volume::{Backing, Volume};

pub use served::{Served, BIND_KEYS};
pub use tairix_filecore::format::Variant;

/// Per-driver `DriverHandle` marker returned by [`register`].
const REGISTER_HANDLE_MARKER: u64 = 0x4144_4653_0000_0001; // "ADFS" + index

/// Driver entry point.
///
/// # Errors
///
/// * [`DriverError::PermissionDenied`] if the host did not grant
///   [`CapabilityId::DRV_LOAD`].
///
/// # Capabilities
///
/// Requires [`CapabilityId::DRV_LOAD`].
pub fn register(host: &dyn DriverHost) -> Result<DriverHandle, DriverError> {
    if !host.has_capability(CapabilityId::DRV_LOAD) {
        return Err(DriverError::PermissionDenied);
    }
    DriverHandle::from_raw(REGISTER_HANDLE_MARKER)
}

/// Mask isolating an address in a packed `NodeId`: a new-map indirect
/// address is at most a 19-bit fragment id plus the 8-bit share offset, and
/// an old-map start sector is 24 bits.
const NODE_ADDR_MASK: u64 = (1 << 28) - 1;
/// `NodeId` bit marking a directory.
const NODE_DIR_FLAG: u64 = 1 << 28;
/// `NodeId` validity bit, set on every live node so that no live node
/// ever equals `NodeId::NONE` (`0`).
const NODE_VALID_FLAG: u64 = 1 << 29;
/// `NodeId` bit marking an object with no allocation, which has no address
/// and is named by a digest of its name instead.
const NODE_EMPTY_FLAG: u64 = 1 << 30;
/// Bit position of the parent directory's address.
const NODE_PARENT_SHIFT: u64 = 32;
/// Bit position of the digest bits an empty object keeps past the address
/// field.
const NODE_DIGEST_HIGH_SHIFT: u64 = 60;
/// `NodeId` bit marking a pin's token, whose other bits are a number the
/// mount never gives twice; no object's own id sets it.
const NODE_TOKEN_FLAG: u64 = 1 << 31;
/// How many tokens a mount can give: a token's number fills the 60 bits an
/// object's own id spends on addresses.
const TOKEN_NUMBERS: u64 = 1 << 60;

/// The token numbered `number`, of a directory or a file.
fn token_node(number: u64, is_dir: bool) -> NodeId {
    let kind = if is_dir { NODE_DIR_FLAG } else { 0 };
    NodeId::from_raw(
        NODE_VALID_FLAG
            | NODE_TOKEN_FLAG
            | kind
            | number & NODE_ADDR_MASK
            | (number >> 28) << NODE_PARENT_SHIFT,
    )
}

/// The number of the token `node`, if it is one.
fn token_number(node: NodeId) -> Option<u64> {
    let raw = node.raw();
    let tag = NODE_VALID_FLAG | NODE_TOKEN_FLAG;
    (raw & tag == tag).then_some(raw & NODE_ADDR_MASK | (raw >> NODE_PARENT_SHIFT) << 28)
}

/// The node of `object`, an entry of the directory at `parent`.
///
/// An object is its address, so its node names that and the parent whose
/// entry holds its size, stamp and attributes: resolving it reads one
/// directory, never the tree. Its size is not part of it, so a write that
/// leaves the object where it lies leaves the node as it was; one that gives
/// it its first allocation or moves it, and a truncation to nothing, do not.
/// An object with no allocation (`empty`, as its volume judges its address)
/// is named by a digest of its name, which two siblings may share.
fn child_node(object: &Object, parent: u32, empty: bool) -> NodeId {
    let named = if empty {
        let digest = name_digest(object.name());
        NODE_EMPTY_FLAG
            | u64::from(digest) & NODE_ADDR_MASK
            | u64::from(digest >> 28) << NODE_DIGEST_HIGH_SHIFT
    } else {
        u64::from(object.indaddr) & NODE_ADDR_MASK
    };
    entry_node(parent, object.is_dir(), named)
}

/// The node of an entry of the directory at `parent`, `named` by its address
/// or, with no allocation, by its empty flag and name digest.
fn entry_node(parent: u32, is_dir: bool, named: u64) -> NodeId {
    let kind = if is_dir { NODE_DIR_FLAG } else { 0 };
    NodeId::from_raw(
        NODE_VALID_FLAG | kind | (u64::from(parent) & NODE_ADDR_MASK) << NODE_PARENT_SHIFT | named,
    )
}

/// The root directory's node: its address, and no parent.
fn root_node_at(indaddr: u32) -> NodeId {
    NodeId::from_raw(NODE_VALID_FLAG | NODE_DIR_FLAG | u64::from(indaddr) & NODE_ADDR_MASK)
}

/// A digest of `name` as `FileCore` compares it, without regard to case.
fn name_digest(name: &[u8]) -> u32 {
    let [a, b, c, d, ..] =
        tairix_hash::FastHash::hash_bytes(0, Folded::of(name).as_bytes()).to_le_bytes();
    u32::from_le_bytes([a, b, c, d])
}

/// A name as `FileCore` compares names: upper-cased, and no longer than any
/// directory holds.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Folded {
    len: u8,
    bytes: [u8; tairix_filecore::dir::MAX_NAME_LEN],
}

impl Folded {
    fn of(name: &[u8]) -> Self {
        let name = &name[..name.len().min(tairix_filecore::dir::MAX_NAME_LEN)];
        let mut bytes = [0u8; tairix_filecore::dir::MAX_NAME_LEN];
        for (into, &byte) in bytes.iter_mut().zip(name) {
            *into = acorn::UPPER_CASE[usize::from(byte)];
        }
        Self {
            // `MAX_NAME_LEN` is 255, so the length fits.
            len: u8::try_from(name.len()).unwrap_or(u8::MAX),
            bytes,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

/// A directory entry as a pin finds it again: the address of the directory
/// holding it, and its folded name.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct EntryKey {
    parent: u32,
    name: Folded,
}

impl EntryKey {
    fn of(parent: u32, name: &[u8]) -> Self {
        Self {
            parent,
            name: Folded::of(name),
        }
    }
}

/// Address (or old-map start sector) in a packed node; for an empty object,
/// the low bits of its name digest.
fn node_addr(node: NodeId) -> u32 {
    // The masked value spans at most 28 bits, so it always fits `u32`.
    u32::try_from(node.raw() & NODE_ADDR_MASK).unwrap_or(0)
}

/// The address of the directory whose entry holds `node`.
fn node_parent(node: NodeId) -> u32 {
    u32::try_from(node.raw() >> NODE_PARENT_SHIFT & NODE_ADDR_MASK).unwrap_or(0)
}

/// Whether a packed node denotes a directory.
fn node_is_dir(node: NodeId) -> bool {
    node.raw() & NODE_DIR_FLAG != 0
}

/// Whether a packed node denotes an object with no allocation.
fn node_is_empty(node: NodeId) -> bool {
    node.raw() & NODE_EMPTY_FLAG != 0
}

/// Whether the node was packed by this driver (fail-closed guard).
fn node_is_valid(node: NodeId) -> bool {
    node.raw() & NODE_VALID_FLAG != 0
}

/// Whether `node` names `object`, an entry of its parent with no allocation
/// where `empty`.
fn node_names(node: NodeId, object: &Object, empty: bool) -> bool {
    if node_is_dir(node) != object.is_dir() {
        return false;
    }
    if node_is_empty(node) {
        empty && child_node(object, node_parent(node), empty) == node
    } else {
        !empty && object.indaddr == node_addr(node)
    }
}

/// The single stamp a typed object stores, widened to [`Time64`]. An
/// untyped object (raw load/exec addresses) stores no stamp, and RISC OS's
/// epoch itself is a clock that was never set: both honestly report the
/// epoch.
fn object_stamp(object: &Object) -> Time64 {
    match acorn::decode_load_exec(object.load, object.exec) {
        acorn::LoadExec::Typed {
            centiseconds: 0, ..
        }
        | acorn::LoadExec::Untyped { .. } => Time64::UNIX_EPOCH,
        acorn::LoadExec::Typed { centiseconds, .. } => {
            acorn::centiseconds_to_time64(centiseconds).unwrap_or(Time64::UNIX_EPOCH)
        }
    }
}

/// `object` as a typed object of `filetype` stamped at `now`, to the
/// centisecond — or stamped zero, read as none, while the time is unknown or
/// beyond the 1900–2248 RISC OS's 40-bit stamp holds.
fn typed_at(
    object: &Object,
    filetype: acorn::Filetype,
    now: Option<Time64>,
) -> Result<Object, DriverError> {
    let centiseconds = now
        .and_then(|time| {
            Time64::new(time.secs(), time.subsec_nanos() / 10_000_000 * 10_000_000).ok()
        })
        .and_then(|time| acorn::time64_to_centiseconds(time).ok())
        .unwrap_or(0);
    let (load, exec) =
        acorn::encode_typed(filetype, centiseconds).map_err(|_| DriverError::OutOfRange)?;
    Ok(Object {
        load,
        exec,
        ..*object
    })
}

/// `object` restamped at `now` because its contents changed. An untyped
/// object's load and exec words are addresses, not a stamp, and stay.
fn restamped(object: &Object, now: Option<Time64>) -> Result<Object, DriverError> {
    match acorn::decode_load_exec(object.load, object.exec) {
        acorn::LoadExec::Typed { filetype, .. } => typed_at(object, filetype, now),
        acorn::LoadExec::Untyped { .. } => Ok(*object),
    }
}

/// How `object` takes `change`. A typed object's one stamp is its
/// modification time and stands for each of its others, so an access time is
/// kept only as the stamp the change leaves; an untyped object keeps no time.
/// Answers the object to store, if it changes.
fn times_changed(
    object: &Object,
    change: TimesChange,
    now: Option<Time64>,
) -> Result<(Option<Object>, TimesKept), DriverError> {
    let acorn::LoadExec::Typed { filetype, .. } = acorn::decode_load_exec(object.load, object.exec)
    else {
        let accessed = unkept_stamp(change.accessed, change.nearest)?;
        return Ok((
            None,
            accessed.and(unkept_stamp(change.modified, change.nearest)?),
        ));
    };
    let (modified, m) = acorn::STAMP_FIELD.fit_change(change.modified, change.nearest)?;
    let stored = match modified {
        StampChange::Keep => None,
        StampChange::Now => Some(typed_at(object, filetype, now)?),
        StampChange::At(at) => Some(typed_at(object, filetype, Some(at))?),
    };
    let stamp = object_stamp(stored.as_ref().unwrap_or(object));
    let a = kept_as_stamp(change.accessed, stamp, change.nearest)?;
    Ok((stored, a.and(m)))
}

/// The four [`NodeTimes`] of a typed object. ADFS stores exactly one
/// instant (set at creation, rewritten on update), so it is honestly all
/// four; an untyped object stores none and reports the epoch throughout.
fn object_times(object: &Object) -> NodeTimes {
    let stamp = object_stamp(object);
    NodeTimes {
        created: stamp,
        modified: stamp,
        accessed: stamp,
        changed: stamp,
    }
}

/// How many resolved entries an [`Adfs`] keeps.
const RESOLVED_LEN: usize = 8;

/// An ADFS volume attached to a block device.
pub struct Adfs<B: Block> {
    volume: Volume<ByteDevice<B>>,
    /// The volume's stable identity, as the probe derives it.
    identity: [u8; 16],
    /// Entries recently resolved by node, with their index in their parent,
    /// so the read, stat or write that follows a lookup does not read the
    /// parent again; held while the volume's changes stand at `resolved_at`,
    /// since any change may move or rewrite them.
    resolved: [Option<(NodeId, u32, Object)>; RESOLVED_LEN],
    resolved_at: u64,
    /// Where the next resolution is kept, round the slots in turn.
    next_resolved: usize,
    /// The host the mount's stamps take their time from, once it has one.
    host: Option<&'static dyn VolumeHost>,
    /// The kernel's holds on the mount's objects.
    pins: Pins,
    /// Why the volume is served read-only, once a reason is found: at open
    /// for a short device or a read-only grant, on loading a directory for
    /// one this driver will not write back, or once a retitle fails part-way.
    refusal: Option<WriteRefusal>,
}

/// The kernel's holds on a mount's objects.
///
/// An object's own id moves when a write relocates it, and a directory's
/// carries its parent's address, so every object but the root is held under
/// a token and found again by its entry: a file by its directory and folded
/// name, a directory by its address, which it keeps for life. Every table
/// grows fallibly, so a hold that cannot be recorded is refused.
struct Pins {
    /// Each pinned object but the root, by its token's number.
    held: RadixTree<Pin>,
    /// The number of each pinned file's token, by the entry naming it.
    files: HashMap<EntryKey, u64, BuildSipHash13>,
    /// The number of each pinned directory's token, by its address.
    dirs: RadixTree<u64>,
    /// Holds of the root, whose own id never changes.
    root: u32,
    /// The number the next token takes: none the mount has given before.
    next: u64,
}

/// A pinned object: its holds, and where it is found.
struct Pin {
    holds: u32,
    kept: Kept,
}

/// Where a pinned object is found.
#[derive(Clone, Copy)]
enum Kept {
    /// A file, by its entry.
    File(EntryKey),
    /// The directory at `addr`, an entry of the directory at `parent`.
    Dir { addr: u32, parent: u32 },
    /// An object a removal left with no name: its entry as it last stood in
    /// the directory at `parent`, and whether the allocation it names is the
    /// pin's to free.
    Nameless {
        parent: u32,
        object: Object,
        owned: bool,
    },
}

impl Pins {
    fn new() -> Self {
        Self {
            held: RadixTree::new(),
            // Names come off the disc, so a crafted volume must not be able to
            // file every pinned name in one bucket.
            files: HashMap::with_hasher(BuildSipHash13::keyed().unwrap_or(BuildSipHash13::UNKEYED)),
            dirs: RadixTree::new(),
            root: 0,
            next: 0,
        }
    }

    /// One more hold of the token numbered `number`.
    fn hold(&mut self, number: u64) -> Result<(), DriverError> {
        let pin = self.held.get_mut(number).ok_or(DriverError::NotFound)?;
        pin.holds = pin.holds.checked_add(1).ok_or(DriverError::OutOfMemory)?;
        Ok(())
    }

    /// File `kept` under a new token, whose index entry is the caller's to
    /// make.
    fn mint(&mut self, kept: &Kept) -> Result<u64, DriverError> {
        let number = self.next;
        if number >= TOKEN_NUMBERS {
            return Err(DriverError::OutOfMemory);
        }
        let pin = Pin {
            holds: 1,
            kept: *kept,
        };
        self.held
            .try_insert(number, pin)
            .map_err(|_| DriverError::OutOfMemory)?;
        self.next = number + 1;
        Ok(number)
    }

    /// One more hold of `object`, an entry of the directory at `parent`.
    fn pin_entry(&mut self, object: &Object, parent: u32) -> Result<NodeId, DriverError> {
        if let Some(number) = self.token_of(object, parent) {
            self.hold(number)?;
            return Ok(token_node(number, object.is_dir()));
        }
        if object.is_dir() {
            let addr = u64::from(object.indaddr);
            self.dirs
                .try_reserve_key(addr)
                .map_err(|_| DriverError::OutOfMemory)?;
            let number = self.mint(&Kept::Dir {
                addr: object.indaddr,
                parent,
            })?;
            if self.dirs.try_insert(addr, number).is_err() {
                self.held.remove(number);
                return Err(DriverError::OutOfMemory);
            }
            return Ok(token_node(number, true));
        }
        let key = EntryKey::of(parent, object.name());
        self.files
            .try_reserve(1)
            .map_err(|_| DriverError::OutOfMemory)?;
        let number = self.mint(&Kept::File(key))?;
        if self.files.try_insert(key, number).is_err() {
            self.held.remove(number);
            return Err(DriverError::OutOfMemory);
        }
        Ok(token_node(number, false))
    }

    /// The number of the token holding `object`, an entry of the directory at
    /// `parent`, while one does.
    fn token_of(&self, object: &Object, parent: u32) -> Option<u64> {
        if object.is_dir() {
            self.dirs.get(u64::from(object.indaddr)).copied()
        } else if self.files.is_empty() {
            None
        } else {
            self.files
                .get(&EntryKey::of(parent, object.name()))
                .copied()
        }
    }

    /// Where the object the token numbered `number` holds is found.
    fn kept(&self, number: u64) -> Result<Kept, DriverError> {
        self.held
            .get(number)
            .map(|pin| pin.kept)
            .ok_or(DriverError::NotFound)
    }

    /// Release one hold of the token numbered `number`: `None` while others
    /// remain, else where the last left the object.
    fn release(&mut self, number: u64) -> Result<Option<Kept>, DriverError> {
        let pin = self.held.get_mut(number).ok_or(DriverError::NotFound)?;
        if pin.holds > 1 {
            pin.holds -= 1;
            return Ok(None);
        }
        let kept = pin.kept;
        self.held.remove(number);
        self.unindex(&kept);
        Ok(Some(kept))
    }

    /// Withdraw the index entry that finds what `kept` names.
    fn unindex(&mut self, kept: &Kept) {
        match kept {
            Kept::File(key) => {
                self.files.remove(key);
            }
            Kept::Dir { addr, .. } => {
                self.dirs.remove(u64::from(*addr));
            }
            Kept::Nameless { .. } => {}
        }
    }

    /// The object the token numbered `number` holds lost its last name, its
    /// entry last standing as `object` in the directory at `parent`.
    fn unname(&mut self, number: u64, parent: u32, object: &Object, owned: bool) {
        if let Some(pin) = self.held.get_mut(number) {
            let was = pin.kept;
            pin.kept = Kept::Nameless {
                parent,
                object: *object,
                owned,
            };
            self.unindex(&was);
        }
    }

    /// The nameless object the token numbered `number` holds now stands as
    /// `renewed`.
    fn renew(&mut self, number: u64, renewed: &Object) {
        if let Some(Pin {
            kept: Kept::Nameless { object, .. },
            ..
        }) = self.held.get_mut(number)
        {
            *object = *renewed;
        }
    }

    /// Make room for a pin of `object`, an entry of the directory at
    /// `parent`, to follow it to another entry.
    fn make_room(&mut self, object: &Object, parent: u32) -> Result<(), DriverError> {
        if !object.is_dir() && self.token_of(object, parent).is_some() {
            self.files
                .try_reserve(1)
                .map_err(|_| DriverError::OutOfMemory)?;
        }
        Ok(())
    }

    /// `object`, an entry of the directory at `from`, now stands as `name` in
    /// the directory at `to`: its pin follows it.
    fn moved(
        &mut self,
        object: &Object,
        from: u32,
        to: u32,
        name: &[u8],
    ) -> Result<(), DriverError> {
        let Some(number) = self.token_of(object, from) else {
            return Ok(());
        };
        let kept = if object.is_dir() {
            Kept::Dir {
                addr: object.indaddr,
                parent: to,
            }
        } else {
            let key = EntryKey::of(to, name);
            self.files
                .try_insert(key, number)
                .map_err(|_| DriverError::OutOfMemory)?;
            self.files.remove(&EntryKey::of(from, object.name()));
            Kept::File(key)
        };
        if let Some(pin) = self.held.get_mut(number) {
            pin.kept = kept;
        }
        Ok(())
    }
}

/// What a pin keeps of an object losing its last name: the object as the pin
/// holds it, and whether the allocation it names is the pin's to free.
#[derive(Clone, Copy)]
struct Unnamed {
    object: Object,
    owned: bool,
}

/// What a node names.
#[derive(Clone, Copy)]
enum Target {
    /// The root directory, which no entry holds.
    Root,
    /// The `index`-th entry of the directory at `parent`, holding `object`.
    Entry {
        parent: u32,
        index: u32,
        object: Object,
    },
    /// An object no entry names, which the pin numbered `number` keeps as
    /// `object`, its entry as it last stood in the directory at `parent`.
    Nameless {
        number: u64,
        parent: u32,
        object: Object,
    },
}

impl Target {
    /// The object named, and the directory whose entry holds it or last did;
    /// `None` for the root.
    fn object(&self) -> Option<(u32, Object)> {
        match *self {
            Self::Root => None,
            Self::Entry { parent, object, .. } | Self::Nameless { parent, object, .. } => {
                Some((parent, object))
            }
        }
    }
}

impl<B: Block> Adfs<B> {
    /// The instant an operation stamps, or `None` while the host has no time
    /// to give.
    fn now(&self) -> Option<Time64> {
        self.host.and_then(VolumeHost::wall_time)
    }

    /// Attach to an existing ADFS volume on `device`, identifying its
    /// format and validating every checksummed structure on the way.
    ///
    /// # Errors
    ///
    /// * [`DriverError::BadMagic`] if the device holds no structurally
    ///   valid ADFS volume of any variant.
    /// * [`DriverError::Unsupported`] if the device geometry cannot be
    ///   staged.
    /// * [`DriverError::DeviceFault`] on an unrecoverable block read.
    pub fn open(device: B) -> Result<Self, DriverError> {
        let mut device = ByteDevice::new(device)?;
        let layout = tairix_fsprobe::adfs::identify(&mut device)?.ok_or(DriverError::BadMagic)?;
        let identity = tairix_fsprobe::adfs::identity(&layout);
        let (backing, disc_bytes) = match layout {
            Layout::NewMap { record, boot } => {
                let boot_block = boot.is_some();
                let disc_bytes = record.disc_size;
                let map = NewMap::open(&mut device, record)?;
                (Backing::New { map, boot_block }, disc_bytes)
            }
            Layout::OldMap {
                large_directories,
                root_sector,
                disc_sectors,
                ..
            } => (
                Backing::Old {
                    format: if large_directories {
                        FixedFormat::New
                    } else {
                        FixedFormat::Old
                    },
                    root_sector,
                },
                u64::from(disc_sectors) * OLD_SECTOR_SIZE,
            ),
        };
        // An image short of the disc it holds is read where it reaches, and
        // never written: an allocation could land past its end.
        let truncated = (disc_bytes > device.capacity()).then_some(WriteRefusal::Truncated);
        let volume = Volume::open(device, backing)?;
        let refusal = truncated.or(volume
            .copies_disagree()
            .then_some(WriteRefusal::UnsupportedFeature));
        Ok(Self {
            resolved_at: volume.changes(),
            volume,
            identity,
            resolved: [None; RESOLVED_LEN],
            next_resolved: 0,
            host: None,
            pins: Pins::new(),
            refusal,
        })
    }

    /// Format `device` as an empty ADFS volume of `variant` and attach
    /// to it.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NoSpace`] if the device is too small for the
    ///   variant.
    /// * [`DriverError::DeviceFault`] on an unrecoverable block access.
    pub fn format(device: B, variant: Variant) -> Result<Self, DriverError> {
        let mut device = ByteDevice::new(device)?;
        tairix_filecore::format::format_volume(&mut device, variant)?;
        Self::open(device.into_inner())
    }

    /// Detach from the volume, returning the underlying device.
    pub fn into_device(self) -> B {
        self.volume.detach().0.into_inner()
    }

    /// The volume's stable identity, exactly as the volume probe derives it.
    #[must_use]
    pub fn volume_identity(&self) -> [u8; 16] {
        self.identity
    }

    /// Why this handle refuses writes, or `None` while it accepts them.
    #[must_use]
    pub fn write_refusal(&self) -> Option<WriteRefusal> {
        // A directory read with entries no check byte vouches for would revive
        // them, as a later insertion closing the gap would, written back.
        self.refusal.or(self
            .volume
            .stranded()
            .then_some(WriteRefusal::UnsupportedFeature))
    }

    /// Refuse a change once the volume is read-only. Every change calls this
    /// after resolving what it touches and before it writes anything, so a
    /// directory first loaded by that resolution is weighed too.
    fn may_write(&self) -> Result<(), DriverError> {
        match self.write_refusal() {
            Some(_) => Err(DriverError::ReadOnly),
            None => Ok(()),
        }
    }

    /// Refuse every change for `why`, whatever the volume would allow: a
    /// volume served read-only.
    pub(crate) fn refuse_writes(&mut self, why: WriteRefusal) {
        self.refusal.get_or_insert(why);
    }

    /// The root directory's node.
    fn root_node(&self) -> NodeId {
        root_node_at(self.volume.root_addr())
    }
}

/// The VFS's `name` as the disc spells it. Every name crosses once as it
/// enters or leaves the driver, through the one RISC OS name conversion, and
/// nothing within the driver sees the VFS's spelling.
///
/// # Errors
///
/// [`DriverError::LengthOutOfRange`] for a name no directory format holds;
/// [`DriverError::OutOfRange`] for one no RISC OS name can be.
fn spelt(name: &[u8]) -> Result<acorn::Leaf, DriverError> {
    acorn::leaf_to_riscos(name).map_err(|bad| match bad {
        acorn::BadName::Empty | acorn::BadName::TooLong => DriverError::LengthOutOfRange,
        acorn::BadName::Character | acorn::BadName::NoEntry => DriverError::OutOfRange,
    })
}

/// `name`, read from the disc, as the VFS lists it, or `None` for one no
/// RISC OS name can be, which is left out of its directory's listing.
fn listed(name: &[u8]) -> Option<acorn::Leaf> {
    acorn::leaf_from_riscos(name).ok()
}

impl<B: Block> FilesystemRead for Adfs<B> {
    fn root(&self) -> NodeId {
        self.root_node()
    }

    fn name_matching(&self) -> NameMatching {
        NameMatching::RiscOs
    }

    fn node_identity(&self) -> NodeIdentity {
        NodeIdentity::ContentDerived
    }

    fn node_info(&mut self, node: NodeId) -> Result<NodeInfo, DriverError> {
        match self.read_target(node)? {
            // The root has no directory entry and so no stored stamp; every
            // other node's lives in its parent's entry.
            Target::Root => {
                let root = self.volume.root_addr();
                let allocated = self
                    .volume
                    .object_allocated(root, self.volume.root_size())?;
                Ok(NodeInfo {
                    kind: NodeKind::Directory,
                    nlink: NodeInfo::SINGLE_NAME,
                    ino: node.raw(),
                    size: 0,
                    allocated,
                    times: NodeTimes::default(),
                    content_gen: NodeInfo::NO_CONTENT_GEN,
                })
            }
            Target::Entry { parent, object, .. } => {
                self.info_of(&object, parent, NodeInfo::SINGLE_NAME)
            }
            Target::Nameless { parent, object, .. } => self.info_of(&object, parent, 0),
        }
    }

    fn lookup(&mut self, dir: NodeId, name: &[u8]) -> Result<NodeId, DriverError> {
        // A name RISC OS cannot spell names nothing on its disc.
        let name = spelt(name).map_err(|_| DriverError::NotFound)?;
        let dir_addr = self.entries_addr(dir)?;
        let (index, object) = self.child(dir_addr, name.as_bytes())?;
        let node = self.reported(&object, dir_addr);
        self.keep_resolved(node, index, &object);
        Ok(node)
    }

    fn read_at(&mut self, file: NodeId, offset: u64, buf: &mut [u8]) -> Result<usize, DriverError> {
        if node_is_valid(file) && node_is_dir(file) {
            return Err(DriverError::Unsupported);
        }
        let object = match self.read_target(file) {
            Ok(target) => target.object().ok_or(DriverError::Unsupported)?.1,
            // Every empty sibling the digest could name holds no bytes.
            Err(DriverError::Unsupported) if node_is_empty(file) => return Ok(0),
            Err(err) => return Err(err),
        };
        let size = u64::from(object.size);
        // An object with no allocation holds no bytes.
        if buf.is_empty() || self.volume.holds_nothing(object.indaddr) || offset >= size {
            return Ok(0);
        }
        let want = usize::try_from((size - offset).min(buf.len() as u64))
            .map_err(|_| DriverError::LengthOutOfRange)?;
        self.volume
            .object_read(object.indaddr, offset, &mut buf[..want])?;
        Ok(want)
    }

    fn read_dir(
        &mut self,
        dir: NodeId,
        cursor: u64,
        after: &[u8],
        visit: &mut dyn FnMut(&DirEntry, &[u8]) -> DirVisit,
    ) -> Result<(), DriverError> {
        // A removed directory a pin holds lists nothing.
        let Some(indaddr) = self.dir_addr(dir)? else {
            return Ok(());
        };
        // A listing from its start resumes after no name.
        let after = (!after.is_empty()).then(|| spelt(after)).transpose()?;
        let after = after.as_ref().map_or(&[][..], acorn::Leaf::as_bytes);
        // One load serves the whole call, however many entries it lists.
        let handle = self.volume.load_dir(indaddr)?;
        let [i0, i1, i2, i3, g0, g1, g2, g3] = cursor.to_le_bytes();
        let (index, minted) = (
            u32::from_le_bytes([i0, i1, i2, i3]),
            u32::from_le_bytes([g0, g1, g2, g3]),
        );
        let start = if cursor == 0 {
            index
        } else {
            self.volume.resume(&handle, indaddr, index, minted, after)?
        };
        for index in start..Volume::<ByteDevice<B>>::dir_len(&handle) {
            let Some(object) = self.volume.dir_entry(&handle, indaddr, index)? else {
                break;
            };
            // Left out rather than refusing the whole directory.
            let Some(name) = listed(object.name()) else {
                continue;
            };
            let entry = DirEntry {
                node: self.reported(&object, indaddr),
                info: self.info_of(&object, indaddr, NodeInfo::SINGLE_NAME)?,
                next_cursor: (u64::from(self.volume.shapes()) << 32) | u64::from(index + 1),
            };
            if visit(&entry, name.as_bytes()) == DirVisit::Stop {
                break;
            }
        }
        Ok(())
    }

    fn parent(&mut self, dir: NodeId) -> Result<NodeId, DriverError> {
        if !node_is_valid(dir) {
            return Err(DriverError::NotFound);
        }
        if !node_is_dir(dir) {
            return Err(DriverError::Unsupported);
        }
        let root = self.root_node();
        if dir == root {
            return Ok(root);
        }
        // Found in its parent, so a removed directory answers `NotFound`.
        let Target::Entry { parent, .. } = self.write_target(dir)? else {
            return Err(DriverError::NotFound);
        };
        if parent == self.volume.root_addr() {
            return Ok(root);
        }
        let grandparent = self.volume.dir_parent(parent)?;
        Ok(self.reported_dir(parent, grandparent))
    }

    fn pin(&mut self, node: NodeId) -> Result<NodeId, DriverError> {
        if let Some(number) = token_number(node) {
            self.pins.hold(number)?;
            return Ok(node);
        }
        if node == self.root_node() {
            self.pins.root = self
                .pins
                .root
                .checked_add(1)
                .ok_or(DriverError::OutOfMemory)?;
            return Ok(node);
        }
        // Resolved as for a change, so an empty object whose name digest a
        // sibling shares is refused rather than held for the wrong one.
        let Target::Entry { parent, object, .. } = self.write_target(node)? else {
            return Err(DriverError::NotFound);
        };
        self.pins.pin_entry(&object, parent)
    }

    fn unpin(&mut self, pinned: NodeId) -> Result<Unpinned, DriverError> {
        if pinned == self.root_node() {
            let holds = self.pins.root;
            if holds == 0 {
                return Err(DriverError::NotFound);
            }
            self.pins.root = holds - 1;
            return Ok(if holds > 1 {
                Unpinned::Held
            } else {
                Unpinned::Released
            });
        }
        let number = token_number(pinned).ok_or(DriverError::NotFound)?;
        match self.pins.release(number)? {
            None => Ok(Unpinned::Held),
            Some(Kept::File(_) | Kept::Dir { .. }) => Ok(Unpinned::Released),
            Some(Kept::Nameless { object, owned, .. }) => {
                if owned {
                    self.volume.free(object.indaddr, object.size)?;
                }
                Ok(Unpinned::Freed)
            }
        }
    }

    fn release_pins(&mut self) -> Result<(), DriverError> {
        let held = core::mem::take(&mut self.pins.held);
        self.pins.files.clear();
        self.pins.dirs.clear();
        self.pins.root = 0;
        let mut freed = Ok(());
        for (_, pin) in &held {
            if let Kept::Nameless {
                object,
                owned: true,
                ..
            } = pin.kept
            {
                freed = freed.and(self.volume.free(object.indaddr, object.size));
            }
        }
        freed.map_err(DriverError::from)
    }
}

impl<B: Block> Adfs<B> {
    /// The [`NodeInfo`] of `object`, an entry of the directory at `parent`
    /// or the last it stood as there, which `nlink` names reach.
    fn info_of(
        &mut self,
        object: &Object,
        parent: u32,
        nlink: u32,
    ) -> Result<NodeInfo, DriverError> {
        let allocated = self.volume.object_allocated(object.indaddr, object.size)?;
        let (kind, size) = if object.is_dir() {
            (NodeKind::Directory, 0)
        } else {
            (NodeKind::RegularFile, u64::from(object.size))
        };
        Ok(NodeInfo {
            kind,
            nlink,
            ino: child_node(object, parent, self.volume.holds_nothing(object.indaddr)).raw(),
            size,
            allocated,
            times: object_times(object),
            content_gen: NodeInfo::NO_CONTENT_GEN,
        })
    }

    /// What `node` names, for a read: the entry a lookup just resolved it to
    /// stands.
    fn read_target(&mut self, node: NodeId) -> Result<Target, DriverError> {
        if !node_is_valid(node) {
            return Err(DriverError::NotFound);
        }
        if node == self.root_node() {
            return Ok(Target::Root);
        }
        if let Some(number) = token_number(node) {
            return self.token_target(number, node);
        }
        if let Some((index, object)) = self.held(node) {
            return Ok(Target::Entry {
                parent: node_parent(node),
                index,
                object,
            });
        }
        let (parent, index, object) = self.entry_of(node)?;
        self.keep_resolved(node, index, &object);
        Ok(Target::Entry {
            parent,
            index,
            object,
        })
    }

    /// What `node` names, for a change.
    fn write_target(&mut self, node: NodeId) -> Result<Target, DriverError> {
        if let Some(number) = token_number(node) {
            return self.token_target(number, node);
        }
        let (parent, index, object) = self.entry_of(node)?;
        Ok(Target::Entry {
            parent,
            index,
            object,
        })
    }

    /// What `token`, the token numbered `number`, holds.
    fn token_target(&mut self, number: u64, token: NodeId) -> Result<Target, DriverError> {
        match self.pins.kept(number)? {
            Kept::File(key) => {
                if let Some((index, object)) = self.held(token) {
                    return Ok(Target::Entry {
                        parent: key.parent,
                        index,
                        object,
                    });
                }
                let (index, object) = self
                    .volume
                    .dir_lookup(key.parent, key.name.as_bytes())?
                    .ok_or(DriverError::NotFound)?;
                self.keep_resolved(token, index, &object);
                Ok(Target::Entry {
                    parent: key.parent,
                    index,
                    object,
                })
            }
            Kept::Dir { addr, parent } => {
                let own = entry_node(parent, true, u64::from(addr) & NODE_ADDR_MASK);
                let (parent, index, object) = self.entry_of(own)?;
                Ok(Target::Entry {
                    parent,
                    index,
                    object,
                })
            }
            Kept::Nameless { parent, object, .. } => Ok(Target::Nameless {
                number,
                parent,
                object,
            }),
        }
    }

    /// Record `object` as what `target` names.
    fn store(&mut self, target: &Target, object: &Object) -> Result<(), DriverError> {
        match *target {
            Target::Entry { parent, index, .. } => self
                .volume
                .dir_update_at(parent, index, object)
                .map_err(DriverError::from),
            Target::Nameless { number, .. } => {
                self.pins.renew(number, object);
                Ok(())
            }
            Target::Root => Err(DriverError::Unsupported),
        }
    }

    /// The address of the directory `dir` names, or `None` for one a pin
    /// keeps after a removal, which takes no entries.
    ///
    /// # Errors
    ///
    /// * [`DriverError::Unsupported`] if `dir` names no directory.
    /// * [`DriverError::NotFound`] for a token no pin holds.
    fn dir_addr(&self, dir: NodeId) -> Result<Option<u32>, DriverError> {
        if !node_is_valid(dir) || !node_is_dir(dir) {
            return Err(DriverError::Unsupported);
        }
        let Some(number) = token_number(dir) else {
            return Ok(Some(node_addr(dir)));
        };
        Ok(match self.pins.kept(number)? {
            Kept::Dir { addr, .. } => Some(addr),
            Kept::File(_) | Kept::Nameless { .. } => None,
        })
    }

    /// The address of the directory `dir` names, which takes entries.
    fn entries_addr(&self, dir: NodeId) -> Result<u32, DriverError> {
        self.dir_addr(dir)?.ok_or(DriverError::NotFound)
    }

    /// The entry named `name` in the directory at `dir_addr`, with its index.
    fn child(&mut self, dir_addr: u32, name: &[u8]) -> Result<(u32, Object), DriverError> {
        if name.is_empty() || name.len() > tairix_filecore::dir::MAX_NAME_LEN {
            return Err(DriverError::NotFound);
        }
        self.volume
            .dir_lookup(dir_addr, name)?
            .ok_or(DriverError::NotFound)
    }

    /// The id `object`, an entry of the directory at `parent`, is reported
    /// under: its pin's token while one holds it, else its own.
    fn reported(&self, object: &Object, parent: u32) -> NodeId {
        match self.pins.token_of(object, parent) {
            Some(number) => token_node(number, object.is_dir()),
            None => child_node(object, parent, self.volume.holds_nothing(object.indaddr)),
        }
    }

    /// The id the directory at `addr`, an entry of the directory at
    /// `parent`, is reported under.
    fn reported_dir(&self, addr: u32, parent: u32) -> NodeId {
        match self.pins.dirs.get(u64::from(addr)) {
            Some(&number) => token_node(number, true),
            None => entry_node(parent, true, u64::from(addr) & NODE_ADDR_MASK),
        }
    }

    /// The index and entry `node` was last resolved to, while no directory
    /// write has intervened.
    fn held(&self, node: NodeId) -> Option<(u32, Object)> {
        if self.resolved_at != self.volume.changes() {
            return None;
        }
        self.resolved
            .iter()
            .flatten()
            .find(|(held, ..)| *held == node)
            .map(|&(_, index, object)| (index, object))
    }

    /// Remember that `node` names `object`, the `index`-th entry of its
    /// parent, until the next directory write.
    fn keep_resolved(&mut self, node: NodeId, index: u32, object: &Object) {
        if self.resolved_at != self.volume.changes() {
            self.resolved = [None; RESOLVED_LEN];
            self.resolved_at = self.volume.changes();
        }
        self.resolved[self.next_resolved] = Some((node, index, *object));
        self.next_resolved = (self.next_resolved + 1) % RESOLVED_LEN;
    }

    /// Remove the `src_index`-th entry of the directory at `src_addr`,
    /// naming `object`, and give `object` the entry of `existing`, named
    /// `dst_name` in the directory at `dst_addr`, under that spelling. A pin
    /// of `existing` keeps it, nameless; otherwise its allocation is released.
    /// `object`'s entry in the directory at `src_addr` moved to the one at
    /// `dst_addr` as `moving` named `dst_name`, the source entry put back
    /// where the destination will not take it, so no entry is lost.
    fn move_entry(
        &mut self,
        src_addr: u32,
        src_index: u32,
        object: &Object,
        moving: &Object,
        dst_addr: u32,
        dst_name: &[u8],
    ) -> Result<(), DriverError> {
        self.volume.dir_remove_at(src_addr, src_index)?;
        let moved = moving.renamed(dst_name)?;
        if let Err(err) = self.volume.dir_insert(dst_addr, &moved) {
            self.volume.dir_insert(src_addr, object)?;
            return Err(err.into());
        }
        Ok(())
    }

    fn replace_entry(
        &mut self,
        src_addr: u32,
        src_index: u32,
        object: &Object,
        dst_addr: u32,
        dst_name: &[u8],
        existing: &Object,
    ) -> Result<(), DriverError> {
        let pinned = match self.pins.token_of(existing, dst_addr) {
            Some(number) => Some((number, self.unnamed(existing, dst_addr)?)),
            None => None,
        };
        if let Err(err) = self
            .volume
            .respell_over(src_addr, src_index, object, dst_addr, dst_name)
        {
            if let Some((_, unnamed)) = &pinned {
                self.forget_unnamed(unnamed, existing)?;
            }
            return Err(err.into());
        }
        match pinned {
            Some((number, unnamed)) => self.keep_unnamed(number, dst_addr, existing, &unnamed),
            None => self
                .volume
                .release_maybe_shared(existing.indaddr, existing.size, dst_addr)
                .map_err(DriverError::from),
        }
    }

    /// What the pin of `object`, an entry of the directory at `parent` losing
    /// its last name, keeps. A file sharing a fragment moves to one of its
    /// own first, since the fragment is freed with the last entry naming it;
    /// a directory, read no more once it has no name, keeps an allocation
    /// only where nothing else shares it.
    fn unnamed(&mut self, object: &Object, parent: u32) -> Result<Unnamed, DriverError> {
        if object.is_dir() {
            return Ok(Unnamed {
                object: *object,
                owned: self.volume.releases(object.indaddr, parent)?,
            });
        }
        let mut kept = *object;
        if self.volume.is_shared(object.indaddr) {
            kept.indaddr = if object.size == 0 {
                0
            } else {
                self.volume
                    .copy_out(object.indaddr, object.size, u64::from(object.size))?
            };
        }
        Ok(Unnamed {
            object: kept,
            owned: true,
        })
    }

    /// Give back what [`Self::unnamed`] took for `object`, which keeps its
    /// name after all.
    fn forget_unnamed(&mut self, unnamed: &Unnamed, object: &Object) -> Result<(), DriverError> {
        if unnamed.object.indaddr == object.indaddr {
            return Ok(());
        }
        self.volume
            .free(unnamed.object.indaddr, unnamed.object.size)
            .map_err(DriverError::from)
    }

    /// Hand `object`, whose entry in the directory at `parent` is gone, to
    /// the pin numbered `number` as `unnamed` keeps it; a file that moved out
    /// of a shared fragment leaves its share to the fragment's other sharers.
    fn keep_unnamed(
        &mut self,
        number: u64,
        parent: u32,
        object: &Object,
        unnamed: &Unnamed,
    ) -> Result<(), DriverError> {
        self.pins
            .unname(number, parent, &unnamed.object, unnamed.owned);
        if unnamed.object.indaddr == object.indaddr {
            return Ok(());
        }
        self.volume
            .release_maybe_shared(object.indaddr, object.size, parent)
            .map_err(DriverError::from)
    }
}

impl<B: Block> FilesystemWrite for Adfs<B> {
    fn create(
        &mut self,
        dir: NodeId,
        name: &[u8],
        kind: NodeKind,
        _record: &NodeSecurity,
    ) -> Result<NodeId, DriverError> {
        let name = spelt(name)?;
        let name = name.as_bytes();
        let dir_addr = self.entries_addr(dir)?;
        self.volume.validate_new_name(name)?;
        if self.volume.dir_lookup(dir_addr, name)?.is_some() {
            return Err(DriverError::AlreadyExists);
        }
        self.may_write()?;
        // RISC OS types a file it is told nothing about as Data; an untyped
        // object would be a load-and-run image at address zero.
        let mut object = typed_at(&Object::named(name)?, acorn::Filetype::DATA, self.now())?;
        // ADFS keeps no owner, group or mode, so the record has nowhere to
        // go: the access bits are RISC OS's own, set as RISC OS sets them.
        match kind {
            NodeKind::RegularFile => {
                // Created with owner read/write, no data allocation.
                object.attr = acorn::ATTR_OWNER_READ | acorn::ATTR_OWNER_WRITE;
                self.volume.dir_insert(dir_addr, &object)?;
            }
            // Acorn ADFS / FileCore has no symbolic-link object type, so the
            // creation is refused rather than approximated by a file whose
            // contents merely look like a path.
            NodeKind::Symlink => return Err(DriverError::Unsupported),
            NodeKind::Directory => {
                object.attr = acorn::ATTR_DIRECTORY | acorn::ATTR_OWNER_READ;
                // 8-bit ADFS (the 1280-byte directory format) creates
                // directories locked; 32-bit `FileCore` does not.
                if !self.volume.is_big_dir() && self.volume.fixed_format() == FixedFormat::Old {
                    object.attr |= acorn::ATTR_LOCKED;
                }
                let (child, bytes) = self.volume.new_directory(dir_addr, name)?;
                object.indaddr = child;
                object.size = bytes;
                if let Err(err) = self.volume.dir_insert(dir_addr, &object) {
                    // Fail closed: reclaim the orphaned allocation.
                    self.volume.free(child, bytes)?;
                    return Err(err.into());
                }
            }
        }
        Ok(child_node(
            &object,
            dir_addr,
            self.volume.holds_nothing(object.indaddr),
        ))
    }

    fn write_at(&mut self, file: NodeId, offset: u64, data: &[u8]) -> Result<usize, DriverError> {
        let target = self.file_target(file)?;
        self.may_write()?;
        let (parent, object) = target.object().ok_or(DriverError::Unsupported)?;
        if data.is_empty() {
            return Ok(0);
        }
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or(DriverError::LengthOutOfRange)?;
        // An ADFS length field is 32 bits; larger files cannot exist on
        // the format.
        if end > u64::from(u32::MAX) {
            return Err(DriverError::NoSpace);
        }
        let size = object.size;
        let indaddr = if end > u64::from(size) {
            self.volume.grow_object(object.indaddr, size, end, parent)?
        } else {
            object.indaddr
        };
        if offset > u64::from(size) {
            self.volume
                .zero_object_range(indaddr, u64::from(size), offset)?;
        }
        self.volume.object_write(indaddr, offset, data)?;
        // The entry is the object's only metadata, so the stamp costs a
        // directory write only where the write did not already need one.
        let mut updated = restamped(&object, self.now())?;
        updated.indaddr = indaddr;
        // `end` was bounded by `u32::MAX` above.
        updated.size = size.max(u32::try_from(end).unwrap_or(u32::MAX));
        if updated != object {
            self.store(&target, &updated)?;
        }
        Ok(data.len())
    }

    fn set_times(&mut self, node: NodeId, change: TimesChange) -> Result<TimesKept, DriverError> {
        let target = if node == self.root_node() {
            Target::Root
        } else {
            self.write_target(node)?
        };
        let Some((_, object)) = target.object() else {
            let accessed = unkept_stamp(change.accessed, change.nearest)?;
            return Ok(accessed.and(unkept_stamp(change.modified, change.nearest)?));
        };
        let (stored, kept) = times_changed(&object, change, self.now())?;
        if let Some(updated) = stored.filter(|updated| *updated != object) {
            self.may_write()?;
            self.store(&target, &updated)?;
        }
        Ok(kept)
    }

    fn truncate(&mut self, file: NodeId, size: u64) -> Result<(), DriverError> {
        let target = self.file_target(file)?;
        self.may_write()?;
        let (parent, object) = target.object().ok_or(DriverError::Unsupported)?;
        if size > u64::from(u32::MAX) {
            return Err(DriverError::NoSpace);
        }
        let old = u64::from(object.size);
        let mut updated = restamped(&object, self.now())?;
        match size.cmp(&old) {
            core::cmp::Ordering::Greater => {
                updated.indaddr =
                    self.volume
                        .grow_object(object.indaddr, object.size, size, parent)?;
                self.volume.zero_object_range(updated.indaddr, old, size)?;
            }
            core::cmp::Ordering::Less if size == 0 => {
                self.volume
                    .release_maybe_shared(object.indaddr, object.size, parent)?;
                updated.indaddr = 0;
            }
            core::cmp::Ordering::Less => {
                self.volume
                    .shrink_object(object.indaddr, object.size, size)?;
            }
            // A truncation that keeps the length is still the open that
            // truncates a file, and is stamped as one.
            core::cmp::Ordering::Equal => {}
        }
        // `size` was bounded by `u32::MAX` above.
        updated.size = u32::try_from(size).unwrap_or(u32::MAX);
        if updated == object {
            return Ok(());
        }
        self.store(&target, &updated)
    }

    fn remove(&mut self, dir: NodeId, name: &[u8]) -> Result<(), DriverError> {
        let name = spelt(name)?;
        let dir_addr = self.entries_addr(dir)?;
        let (index, object) = self.child(dir_addr, name.as_bytes())?;
        if object.is_dir() && !self.volume.dir_is_empty(object.indaddr)? {
            return Err(DriverError::DirectoryNotEmpty);
        }
        self.may_write()?;
        let Some(number) = self.pins.token_of(&object, dir_addr) else {
            self.volume.dir_remove_at(dir_addr, index)?;
            return self
                .volume
                .release_maybe_shared(object.indaddr, object.size, dir_addr)
                .map_err(DriverError::from);
        };
        let unnamed = self.unnamed(&object, dir_addr)?;
        if let Err(err) = self.volume.dir_remove_at(dir_addr, index) {
            self.forget_unnamed(&unnamed, &object)?;
            return Err(err.into());
        }
        self.keep_unnamed(number, dir_addr, &object, &unnamed)
    }

    fn rename(
        &mut self,
        src_dir: NodeId,
        src_name: &[u8],
        dst_dir: NodeId,
        dst_name: &[u8],
    ) -> Result<(), DriverError> {
        let (src_name, dst_name) = (spelt(src_name)?, spelt(dst_name)?);
        let (src_name, dst_name) = (src_name.as_bytes(), dst_name.as_bytes());
        let dst_addr = self.entries_addr(dst_dir)?;
        let src_addr = self.entries_addr(src_dir)?;
        let (src_index, object) = self.child(src_addr, src_name)?;
        if src_addr == dst_addr && acorn::names_match(src_name, dst_name) {
            // The source's own entry, re-spelled: names sort and match
            // ignoring case, so it keeps its place and takes the spelling.
            if object.name() == dst_name {
                return Ok(());
            }
            self.volume.validate_new_name(dst_name)?;
            self.may_write()?;
            return self
                .volume
                .dir_respell_at(src_addr, src_index, &object.renamed(dst_name)?)
                .map_err(DriverError::from);
        }
        self.volume.validate_new_name(dst_name)?;
        // Moving a directory into itself or its own subtree would
        // detach the cycle from the tree; walk the destination's
        // ancestry (bounded like the liveness walk).
        if object.is_dir() && src_addr != dst_addr {
            let root_addr = self.volume.root_addr();
            let mut cursor = dst_addr;
            let mut reached_root = false;
            for _ in 0..96 {
                if cursor == object.indaddr {
                    return Err(DriverError::DirectoryCycle);
                }
                if cursor == root_addr {
                    reached_root = true;
                    break;
                }
                cursor = self.volume.dir_parent(cursor)?;
            }
            if !reached_root {
                // Ancestry that never reaches the root is corrupt (or
                // cyclic); fail closed rather than risk detaching it.
                return Err(DriverError::BadMagic);
            }
        }
        let replaced = self.volume.dir_lookup(dst_addr, dst_name)?;
        if let Some((_, existing)) = &replaced {
            // Kind-compatible replacement only; a directory may replace
            // only an empty directory.
            if existing.is_dir() != object.is_dir() {
                return Err(DriverError::Unsupported);
            }
            if existing.is_dir() && !self.volume.dir_is_empty(existing.indaddr)? {
                return Err(DriverError::DirectoryNotEmpty);
            }
        }
        // A moved directory's parent pointer follows it. It is read before
        // any entry changes, so a refusal its reading brings stops the move
        // whole.
        let moved_dir = if object.is_dir() && src_addr != dst_addr {
            Some(self.volume.load_dir(object.indaddr)?)
        } else {
            None
        };
        self.may_write()?;
        // A pin of the moved object follows it, so its room is made before
        // an entry changes.
        self.pins.make_room(&object, src_addr)?;
        let copy = self.volume.copy_for_move(&object, src_addr, dst_addr)?;
        let moving = match copy {
            Some(indaddr) => Object { indaddr, ..object },
            None => object,
        };
        let moved = match replaced {
            Some((_, existing)) => {
                self.replace_entry(src_addr, src_index, &moving, dst_addr, dst_name, &existing)
            }
            None => self.move_entry(src_addr, src_index, &object, &moving, dst_addr, dst_name),
        };
        if let Err(err) = moved {
            if let Some(indaddr) = copy {
                // The copy's own failure would hide why the move failed.
                let _ = self
                    .volume
                    .release_maybe_shared(indaddr, object.size, dst_addr);
            }
            return Err(err);
        }
        if copy.is_some() {
            self.volume
                .release_maybe_shared(object.indaddr, object.size, src_addr)?;
        }
        self.pins.moved(&object, src_addr, dst_addr, dst_name)?;
        if let Some(child) = moved_dir {
            self.volume.reparent(object.indaddr, child, dst_addr)?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        // Every mutation writes through to the device; its own cache is the
        // device's to make durable.
        self.volume.disc_mut().flush()
    }

    fn set_volume_host(&mut self, _volume: DriverHandle, host: &'static dyn VolumeHost) {
        self.host = Some(host);
    }
}

/// `object` as the RISC OS fields every format that keeps them surfaces.
fn acorn_object(object: &Object) -> acorn::Object {
    acorn::Object {
        load: object.load,
        exec: object.exec,
        attr: object.attr & acorn::ATTR_BITS,
    }
}

impl<B: Block> Adfs<B> {
    /// The entry non-root `node` names, as `(parent, index, object)`, read
    /// from the one directory that holds it.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NotFound`] for a node whose entry has gone.
    /// * [`DriverError::Unsupported`] for an empty object whose name digest
    ///   another empty sibling shares: it cannot be told apart by node, and
    ///   is reached by name instead, never by a guess.
    fn entry_of(&mut self, node: NodeId) -> Result<(u32, u32, Object), DriverError> {
        if !node_is_valid(node) || node == self.root_node() {
            return Err(DriverError::NotFound);
        }
        let parent = node_parent(node);
        // An address names one entry, so the one last resolved stands; a
        // digest may be shared, so it is checked against the whole parent.
        if let Some((index, object)) = self.held(node).filter(|_| !node_is_empty(node)) {
            return Ok((parent, index, object));
        }
        let handle = self.volume.load_dir(parent)?;
        let mut found = None;
        for index in 0..Volume::<ByteDevice<B>>::dir_len(&handle) {
            let Some(object) = self.volume.dir_entry(&handle, parent, index)? else {
                break;
            };
            if !node_names(node, &object, self.volume.holds_nothing(object.indaddr)) {
                continue;
            }
            if found.is_some() {
                return Err(DriverError::Unsupported);
            }
            found = Some((parent, index, object));
            // An address names one object; only a digest needs the rest.
            if !node_is_empty(node) {
                break;
            }
        }
        found.ok_or(DriverError::NotFound)
    }

    /// What the file `file` names, for a change.
    ///
    /// # Errors
    ///
    /// * [`DriverError::Unsupported`] if `file` names a directory, or an
    ///   empty file whose name digest an empty sibling shares.
    /// * [`DriverError::NotFound`] if it names no live file and no held one.
    fn file_target(&mut self, file: NodeId) -> Result<Target, DriverError> {
        if node_is_valid(file) && node_is_dir(file) {
            return Err(DriverError::Unsupported);
        }
        self.write_target(file)
    }

    /// The attribute bits this volume's directory format can store.
    fn storable_attr_bits(&self) -> u16 {
        match (self.volume.is_big_dir(), self.volume.fixed_format()) {
            (false, FixedFormat::Old) => acorn::ATTR_BITS,
            _ => acorn::FILECORE_ATTR_BITS,
        }
    }
}

impl<B: Block> FilesystemAttrs for Adfs<B> {
    fn get_attr(
        &mut self,
        node: NodeId,
        key: &[u8],
        value_out: &mut [u8],
    ) -> Result<Option<usize>, DriverError> {
        let key = tairix_fsmeta::AttrKey::parse(key).map_err(|_| DriverError::OutOfRange)?;
        if !node_is_valid(node) {
            return Err(DriverError::NotFound);
        }
        // The root has no directory entry to hold metadata.
        let Some((_, object)) = self.read_target(node)?.object() else {
            return Ok(None);
        };
        let mut staging = [0u8; acorn::ATTR_VALUE_MAX];
        let Some(len) = acorn_object(&object)
            .value(key.as_bytes(), &mut staging)
            .map_err(|_| DriverError::OutOfRange)?
        else {
            return Ok(None);
        };
        let out = value_out
            .get_mut(..len)
            .ok_or(DriverError::BufferTooSmall)?;
        out.copy_from_slice(&staging[..len]);
        Ok(Some(len))
    }

    fn set_attr(&mut self, node: NodeId, key: &[u8], value: &[u8]) -> Result<(), DriverError> {
        let key = tairix_fsmeta::AttrKey::parse(key).map_err(|_| DriverError::OutOfRange)?;
        if !node_is_valid(node) {
            return Err(DriverError::NotFound);
        }
        if node == self.root_node() {
            // The root has no directory entry to hold metadata.
            return Err(DriverError::Unsupported);
        }
        let target = self.write_target(node)?;
        self.may_write()?;
        let (_, object) = target.object().ok_or(DriverError::Unsupported)?;
        // ADFS stores no general-purpose attributes; every other namespace
        // has nowhere to live.
        let mut fields = acorn_object(&object);
        fields.set(key.as_bytes(), value, self.storable_attr_bits())?;
        let updated = Object {
            load: fields.load,
            exec: fields.exec,
            attr: fields.attr,
            ..object
        };
        self.store(&target, &updated)
    }

    fn list_attr(
        &mut self,
        node: NodeId,
        index: u64,
        key_out: &mut [u8],
    ) -> Result<Option<usize>, DriverError> {
        if !node_is_valid(node) {
            return Err(DriverError::NotFound);
        }
        let Some((_, object)) = self.read_target(node)?.object() else {
            return Ok(None);
        };
        let Some(key) = acorn_object(&object).key(index) else {
            return Ok(None);
        };
        let out = key_out
            .get_mut(..key.len())
            .ok_or(DriverError::BufferTooSmall)?;
        out.copy_from_slice(key);
        Ok(Some(key.len()))
    }

    fn remove_attr(&mut self, node: NodeId, key: &[u8]) -> Result<(), DriverError> {
        let key = tairix_fsmeta::AttrKey::parse(key).map_err(|_| DriverError::OutOfRange)?;
        if !node_is_valid(node) {
            return Err(DriverError::NotFound);
        }
        if node == self.root_node() {
            return Err(DriverError::NotFound);
        }
        let target = self.write_target(node)?;
        self.may_write()?;
        let (_, object) = target.object().ok_or(DriverError::NotFound)?;
        let mut fields = acorn_object(&object);
        fields.remove(key.as_bytes())?;
        let updated = Object {
            load: fields.load,
            exec: fields.exec,
            ..object
        };
        self.store(&target, &updated)
    }
}

impl<B: Block> FilesystemStats for Adfs<B> {
    fn stats(&mut self) -> Result<VolumeStats, DriverError> {
        let space = self.volume.space()?;
        Ok(VolumeStats {
            block_size: space.unit,
            in_reserve: false,
            total_blocks: space.total,
            free_blocks: space.free,
            avail_blocks: space.free,
            // ADFS has no inode table; 0/0 is the honest "untracked" answer
            // the ABI defines.
            files: 0,
            files_free: 0,
        })
    }
}

/// The disc's title is its name: the ten bytes the disc record keeps, or the
/// old map spells across its two sectors, read through the one decode the
/// probe labels the volume with. A retitle the disc fails, reading its map or
/// writing it, leaves the volume refusing writes, damaged.
impl<B: Block> FilesystemLabel for Adfs<B> {
    fn label(&mut self) -> Result<VolumeTitle, DriverError> {
        let layout = tairix_fsprobe::adfs::identify(self.volume.disc_mut())?;
        let (label, len) = layout.ok_or(DriverError::BadMagic)?.label();
        let text = core::str::from_utf8(&label[..len]).map_err(|_| DriverError::OutOfRange)?;
        VolumeTitle::new(text).map_err(DriverError::from_errno)
    }

    fn set_label(&mut self, title: &VolumeTitle) -> Result<(), DriverError> {
        let name = disc_name(title)?;
        self.may_write()?;
        let stored = self.volume.set_disc_name(name);
        if stored.is_err() {
            self.refusal = Some(WriteRefusal::Damaged);
        }
        stored.map_err(DriverError::from)
    }
}

/// `title` as a disc name: RISC OS's Latin-1, space padded to the ten bytes
/// both maps keep. A space would read back as padding and RISC OS's specials
/// as syntax, so either is refused, as is a character its Latin-1 lacks.
fn disc_name(title: &VolumeTitle) -> Result<[u8; NAME_LEN], DriverError> {
    let mut name = [b' '; NAME_LEN];
    for (index, ch) in title.as_str().chars().enumerate() {
        let byte = acorn::latin1_byte(ch)
            .filter(|&byte| byte != b' ' && !acorn::RESERVED_NAME_BYTES.contains(&byte))
            .ok_or(DriverError::OutOfRange)?;
        *name.get_mut(index).ok_or(DriverError::LengthOutOfRange)? = byte;
    }
    Ok(name)
}

/// ADFS records no owner, mode or ACL: every node reports the one record an
/// ownerless format stands in with, and the mount policy grants a removable
/// volume's users access. The `FileCore` attribute bits are data, surfaced
/// through `acorn.attr`, never a permission decision here.
impl<B: Block> FilesystemSecurity for Adfs<B> {
    fn security(&mut self, node: NodeId) -> Result<NodeSecurity, DriverError> {
        Ok(NodeSecurity::unrecorded(self.node_info(node)?.kind))
    }

    fn set_security(&mut self, _node: NodeId, _security: NodeSecurity) -> Result<(), DriverError> {
        // The format has nowhere to keep any part of the record; storing a
        // silently lossy one is refused whole.
        Err(DriverError::Unsupported)
    }

    fn records_ownership(&mut self, node: NodeId) -> Result<bool, DriverError> {
        self.node_info(node).map(|_| false)
    }
}

impl<B: Block> FilesystemAttrsProvider for Adfs<B> {
    fn attrs_fs(&mut self) -> Option<&mut dyn FilesystemAttrsFs> {
        Some(self)
    }
}
