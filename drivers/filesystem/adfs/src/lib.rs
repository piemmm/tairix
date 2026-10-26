//! TAIRiX Acorn ADFS filesystem driver (read/write).
//!
//! Attaches an Acorn ADFS / RISC OS `FileCore` volume sitting behind any
//! [`tairix_abi::driver::block::Block`] device and exposes it through the
//! versioned [`FilesystemRead`], [`FilesystemWrite`], [`FilesystemAttrs`],
//! and [`FilesystemStats`]
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
use tairix_abi::driver::filesystem::{
    DirEntry, DirVisit, FilesystemAttrs, FilesystemAttrsFs, FilesystemAttrsProvider,
    FilesystemRead, FilesystemSecurity, FilesystemStats, FilesystemWrite, NameMatching, NodeId,
    NodeIdentity, NodeInfo, NodeKind, NodeSecurity, NodeTimes, Unpinned, VolumeHost, VolumeStats,
};
use tairix_abi::time::Time64;
use tairix_abi::{CapabilityId, DriverError, DriverHandle, DriverHost};
use tairix_bytedev::ByteDevice;
use tairix_collections::{HashMap, RadixTree};
use tairix_fsmeta::preset::acorn;
use tairix_fsprobe::adfs::Layout;
use tairix_hash::BuildSipHash13;

mod bigdir;
mod bytes;
mod dir;
mod mkfs;
mod newmap;
mod oldmap;
mod served;

#[cfg(test)]
mod tests;

use bigdir::{BigDir, BigHeader, DirStore};
use dir::{FixedDir, FixedFormat, Object};
use newmap::NewMap;
use oldmap::{OldMap, OLD_SECTOR_SIZE, OLD_SECTOR_SIZE_U32};

pub use mkfs::AdfsVariant;
pub use served::{Served, BIND_KEYS};

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
/// An object with no allocation is named by a digest of its name, which two
/// siblings may share.
fn child_node(object: &Object, parent: u32) -> NodeId {
    let named = if object.indaddr == 0 {
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
    bytes: [u8; dir::MAX_NAME_LEN],
}

impl Folded {
    fn of(name: &[u8]) -> Self {
        let name = &name[..name.len().min(dir::MAX_NAME_LEN)];
        let mut bytes = [0u8; dir::MAX_NAME_LEN];
        for (into, &byte) in bytes.iter_mut().zip(name) {
            *into = byte.to_ascii_uppercase();
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

/// Whether `node` names `object`, an entry of its parent.
fn node_names(node: NodeId, object: &Object) -> bool {
    if node_is_dir(node) != object.is_dir() {
        return false;
    }
    if node_is_empty(node) {
        object.indaddr == 0 && child_node(object, node_parent(node)) == node
    } else {
        object.indaddr != 0 && object.indaddr == node_addr(node)
    }
}

/// A fixed directory's size, in the `u32` form node packing uses.
fn fixed_size_u32(format: FixedFormat) -> u32 {
    match format {
        FixedFormat::Old => 1280,
        FixedFormat::New => dir::NEW_DIR_SIZE_U32,
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

/// The filetype a new object takes: Data, as RISC OS gives a file created
/// without one. A new untyped object would be a load-and-run image at
/// address zero.
const FILETYPE_DATA: u16 = 0xFFD;

/// `object` as a typed object of `filetype` stamped at `now`, to the
/// centisecond — or stamped zero, read as none, while the time is unknown or
/// beyond the 1900–2248 RISC OS's 40-bit stamp holds.
fn typed_at(object: &Object, filetype: u16, now: Option<Time64>) -> Result<Object, DriverError> {
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

/// The volume's map flavour and root geometry.
enum Backing {
    /// Old-map volume: contiguous objects addressed by start sector.
    Old {
        /// The fixed directory format the volume uses.
        format: FixedFormat,
        /// Start sector of the root directory.
        root_sector: u32,
    },
    /// New-map volume: fragmented objects addressed indirectly.
    New {
        /// The allocation-map engine (owns the validated disc record).
        map: NewMap,
        /// Whether the volume carries a boot block whose disc-record
        /// copy must be kept in step.
        boot_block: bool,
    },
}

/// How many resolved entries an [`Adfs`] keeps.
const RESOLVED_LEN: usize = 8;

/// An ADFS volume attached to a block device.
pub struct Adfs<B: Block> {
    volume: ByteDevice<B>,
    backing: Backing,
    /// The volume's stable identity, as the probe derives it.
    identity: [u8; 16],
    /// Bumped by every insertion and removal, which shift a sorted
    /// directory's entries: a listing cursor carries the generation it was
    /// minted in, so its index is trusted only while nothing has moved.
    generation: u32,
    /// Entries recently resolved by node, with their index in their parent,
    /// so the read, stat or write that follows a lookup does not read the
    /// parent again. Emptied by every directory write, which may change or
    /// move any of them.
    resolved: [Option<(NodeId, u32, Object)>; RESOLVED_LEN],
    /// Where the next resolution is kept, round the slots in turn.
    next_resolved: usize,
    /// The big directory last validated whole, so a listing's later batches
    /// and the lookups between them do not re-read it to check it again.
    /// Replaced by each write's own result, and dropped by any other.
    validated: Option<(u32, BigHeader)>,
    /// The host the mount's stamps take their time from, once it has one.
    host: Option<&'static dyn VolumeHost>,
    /// The kernel's holds on the mount's objects.
    pins: Pins,
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
        let mut volume = ByteDevice::new(device)?;
        let layout = tairix_fsprobe::adfs::identify(&mut volume)?.ok_or(DriverError::BadMagic)?;
        let identity = layout.identity();
        let backing = match layout {
            Layout::NewMap { record, boot_block } => Backing::New {
                map: NewMap::open(&mut volume, record)?,
                boot_block,
            },
            Layout::OldMap {
                large_directories,
                root_sector,
                ..
            } => Backing::Old {
                format: if large_directories {
                    FixedFormat::New
                } else {
                    FixedFormat::Old
                },
                root_sector,
            },
        };
        let mut adfs = Self {
            volume,
            backing,
            identity,
            generation: 0,
            resolved: [None; RESOLVED_LEN],
            next_resolved: 0,
            validated: None,
            host: None,
            pins: Pins::new(),
        };
        // The root directory must itself validate.
        let root = adfs.root_addr();
        adfs.load_dir(root)?;
        Ok(adfs)
    }

    /// Format `device` as an empty ADFS volume of `variant` and attach
    /// to it.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NoSpace`] if the device is too small for the
    ///   variant.
    /// * [`DriverError::DeviceFault`] on an unrecoverable block access.
    pub fn format(device: B, variant: AdfsVariant) -> Result<Self, DriverError> {
        let mut volume = ByteDevice::new(device)?;
        mkfs::format_volume(&mut volume, variant)?;
        Self::open(volume.into_inner())
    }

    /// Detach from the volume, returning the underlying device.
    pub fn into_device(self) -> B {
        self.volume.into_inner()
    }

    /// The volume's stable identity, exactly as the volume probe derives it.
    #[must_use]
    pub fn volume_identity(&self) -> [u8; 16] {
        self.identity
    }

    /// The root directory's node.
    fn root_node(&self) -> NodeId {
        root_node_at(self.root_addr())
    }

    /// The root directory's address (old map: start sector).
    fn root_addr(&self) -> u32 {
        match &self.backing {
            Backing::Old { root_sector, .. } => *root_sector,
            Backing::New { map, .. } => map.record.root,
        }
    }

    /// The root directory's size, which no entry records: the volume's
    /// fixed directory size, or a big root's in the disc record.
    fn root_size(&self) -> u32 {
        match &self.backing {
            Backing::Old { format, .. } => fixed_size_u32(*format),
            Backing::New { map, .. } if map.record.format_version != 0 => map.record.root_size,
            Backing::New { .. } => dir::NEW_DIR_SIZE_U32,
        }
    }

    /// Read exactly `buf.len()` bytes at `offset` within the object at
    /// `indaddr` (which must be allocated that far).
    fn object_read(
        &mut self,
        indaddr: u32,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<(), DriverError> {
        match &self.backing {
            Backing::Old { .. } => {
                let at = u64::from(indaddr) * OLD_SECTOR_SIZE + offset;
                self.volume.read(at, buf)
            }
            Backing::New { map, .. } => {
                let mut done = 0usize;
                while done < buf.len() {
                    let (at, run) = map.locate(&mut self.volume, indaddr, offset + done as u64)?;
                    let take = usize::try_from(run.min((buf.len() - done) as u64))
                        .map_err(|_| DriverError::LengthOutOfRange)?;
                    if take == 0 {
                        return Err(DriverError::BadMagic);
                    }
                    self.volume.read(at, &mut buf[done..done + take])?;
                    done += take;
                }
                Ok(())
            }
        }
    }

    /// Write `data` at `offset` within the object at `indaddr` (which
    /// must be allocated that far).
    fn object_write(&mut self, indaddr: u32, offset: u64, data: &[u8]) -> Result<(), DriverError> {
        match &self.backing {
            Backing::Old { .. } => {
                let at = u64::from(indaddr) * OLD_SECTOR_SIZE + offset;
                self.volume.write(at, data)
            }
            Backing::New { map, .. } => {
                let mut done = 0usize;
                while done < data.len() {
                    let (at, run) = map.locate(&mut self.volume, indaddr, offset + done as u64)?;
                    let take = usize::try_from(run.min((data.len() - done) as u64))
                        .map_err(|_| DriverError::LengthOutOfRange)?;
                    if take == 0 {
                        return Err(DriverError::BadMagic);
                    }
                    self.volume.write(at, &data[done..done + take])?;
                    done += take;
                }
                Ok(())
            }
        }
    }
}

impl<B: Block> Adfs<B> {
    /// Whether the volume uses big (E+/F+) directories.
    fn is_big_dir(&self) -> bool {
        matches!(&self.backing, Backing::New { map, .. } if map.record.format_version != 0)
    }

    /// The fixed directory format of a non-big-directory volume.
    fn fixed_format(&self) -> FixedFormat {
        match &self.backing {
            Backing::Old { format, .. } => *format,
            Backing::New { .. } => FixedFormat::New,
        }
    }

    /// Load and validate the directory object at `indaddr`.
    ///
    /// A fixed directory's size is dictated by the volume's format and a big
    /// directory's by its own validated header, and the allocation map bounds
    /// every access. A big directory validated whole since its last write is
    /// taken as it stands.
    fn load_dir(&mut self, indaddr: u32) -> Result<DirHandle, DriverError> {
        if self.is_big_dir() {
            if let Some((held, header)) = self.validated {
                if held == indaddr {
                    return Ok(DirHandle::Big(BigDir { header }));
                }
            }
            let mut store = ObjectStore {
                adfs: self,
                indaddr,
                size: u32::MAX,
            };
            let dir = BigDir::load(&mut store, u32::MAX)?;
            self.validated = Some((indaddr, dir.header));
            Ok(DirHandle::Big(dir))
        } else {
            let format = self.fixed_format();
            let mut data = [0u8; dir::NEW_DIR_SIZE];
            self.object_read(indaddr, 0, &mut data[..format.size()])?;
            Ok(DirHandle::Fixed(FixedDir::parse(&data, format)?))
        }
    }

    /// A directory is about to be written: forget what was resolved, and
    /// what was validated, until the write re-seats it.
    fn directories_changing(&mut self) {
        self.resolved = [None; RESOLVED_LEN];
        self.validated = None;
    }

    /// The big directory at `indaddr` stands as `dir`, just written.
    fn validated_as(&mut self, indaddr: u32, dir: &BigDir) {
        self.validated = Some((indaddr, dir.header));
    }

    /// Write a mutated fixed directory back to `indaddr`.
    fn store_fixed_dir(&mut self, indaddr: u32, dir: &FixedDir) -> Result<(), DriverError> {
        self.directories_changing();
        let size = dir.format.size();
        // Re-stage through the object map so a fragmented new-map
        // directory lands in the right places.
        let mut done = 0usize;
        while done < size {
            let take = (size - done).min(512);
            let data = &dir.data[done..done + take];
            self.object_write(indaddr, done as u64, data)?;
            done += take;
        }
        Ok(())
    }

    /// The `index`-th entry of the directory at `indaddr`, if any.
    fn dir_get(&mut self, indaddr: u32, index: u32) -> Result<Option<Object>, DriverError> {
        let handle = self.load_dir(indaddr)?;
        self.dir_entry(&handle, indaddr, index)
    }

    /// How many entries the loaded directory `handle` holds.
    fn dir_len(handle: &DirHandle) -> u32 {
        match handle {
            // A fixed directory holds at most 77 entries.
            DirHandle::Fixed(dir) => u32::try_from(dir.count()).unwrap_or(u32::MAX),
            DirHandle::Big(dir) => dir.header.entries,
        }
    }

    /// The `index`-th entry of the loaded directory `handle` at `indaddr`.
    fn dir_entry(
        &mut self,
        handle: &DirHandle,
        indaddr: u32,
        index: u32,
    ) -> Result<Option<Object>, DriverError> {
        match handle {
            DirHandle::Fixed(dir) => Ok(usize::try_from(index).ok().and_then(|i| dir.entry(i))),
            DirHandle::Big(dir) => {
                let mut store = ObjectStore {
                    adfs: self,
                    indaddr,
                    size: u32::MAX,
                };
                dir.entry(&mut store, index)
            }
        }
    }

    /// Where a listing of `handle` resumes for `cursor`, which follows the
    /// entry named `after`.
    ///
    /// The cursor's own index holds while no insertion or removal has run
    /// since it was minted. Otherwise entries may have shifted, and the
    /// listing resumes just past `after` in the directory's sort order, which
    /// nothing shifts: an entry present throughout is listed exactly once.
    fn resume_index(
        &mut self,
        handle: &DirHandle,
        indaddr: u32,
        cursor: u64,
        after: &[u8],
    ) -> Result<u32, DriverError> {
        let [i0, i1, i2, i3, g0, g1, g2, g3] = cursor.to_le_bytes();
        let index = u32::from_le_bytes([i0, i1, i2, i3]);
        let minted = u32::from_le_bytes([g0, g1, g2, g3]);
        if cursor == 0 || minted == self.generation || after.is_empty() {
            return Ok(index);
        }
        let (mut low, mut high) = (0u32, Self::dir_len(handle));
        while low < high {
            let mid = low + (high - low) / 2;
            let object = self
                .dir_entry(handle, indaddr, mid)?
                .ok_or(DriverError::BadMagic)?;
            if dir::name_cmp(object.name(), after) == core::cmp::Ordering::Greater {
                high = mid;
            } else {
                low = mid + 1;
            }
        }
        Ok(low)
    }

    /// Find the entry named `name` in the directory at `indaddr`.
    fn dir_lookup(
        &mut self,
        indaddr: u32,
        name: &[u8],
    ) -> Result<Option<(u32, Object)>, DriverError> {
        match self.load_dir(indaddr)? {
            DirHandle::Fixed(dir) => Ok(dir
                .find(name)
                // A fixed directory holds at most 77 entries.
                .map(|(index, object)| (u32::try_from(index).unwrap_or(0), object))),
            DirHandle::Big(dir) => {
                let mut store = ObjectStore {
                    adfs: self,
                    indaddr,
                    size: u32::MAX,
                };
                dir.find(&mut store, name)
            }
        }
    }

    /// On-disc bytes allocated to the object at `indaddr` whose recorded
    /// size is `size`.
    fn object_allocated(&mut self, indaddr: u32, size: u32) -> Result<u64, DriverError> {
        match &self.backing {
            Backing::Old { .. } => {
                // An old-map object is exactly its contiguous sector run.
                Ok(u64::from(size).div_ceil(OLD_SECTOR_SIZE) * OLD_SECTOR_SIZE)
            }
            Backing::New { map, .. } => {
                if indaddr == 0 {
                    return Ok(0);
                }
                if indaddr & 0xFF != 0 {
                    // A shared-fragment object owns only its share
                    // granules; the fragment total belongs to several
                    // objects.
                    let granule = 1u64
                        << (u32::from(map.record.log2sharesize)
                            + u32::from(map.record.log2secsize));
                    return Ok(u64::from(size).div_ceil(granule) * granule);
                }
                map.object_allocated_bytes(&mut self.volume, indaddr >> 8)
            }
        }
    }
}

impl<B: Block> Adfs<B> {
    /// Allocate a fresh object of `bytes`, returning its indirect disc
    /// address (old map: start sector).
    fn allocate(&mut self, bytes: u64) -> Result<u32, DriverError> {
        match &self.backing {
            Backing::Old { .. } => {
                let sectors = u32::try_from(bytes.div_ceil(OLD_SECTOR_SIZE))
                    .map_err(|_| DriverError::NoSpace)?;
                let mut map = OldMap::load(&mut self.volume)?;
                let start = map.allocate(sectors)?;
                map.store(&mut self.volume)?;
                Ok(start)
            }
            Backing::New { map, .. } => map.allocate_object(&mut self.volume, bytes),
        }
    }

    /// Free the allocation of the object at `indaddr` whose recorded
    /// size is `size` (whole fragments only; the shared-fragment policy
    /// lives in the callers).
    fn free(&mut self, indaddr: u32, size: u32) -> Result<(), DriverError> {
        if indaddr == 0 {
            return Ok(());
        }
        self.directories_changing();
        match &self.backing {
            Backing::Old { .. } => {
                // A zero-length object occupies no sectors.
                let sectors = u64::from(size).div_ceil(OLD_SECTOR_SIZE);
                if sectors == 0 {
                    return Ok(());
                }
                // A `u32` size spans at most 2^24 sectors.
                let sectors = u32::try_from(sectors).map_err(|_| DriverError::BadMagic)?;
                let mut map = OldMap::load(&mut self.volume)?;
                map.free_span(indaddr, sectors)?;
                map.store(&mut self.volume)
            }
            Backing::New { map, .. } => map.free_object(&mut self.volume, indaddr >> 8),
        }
    }

    /// Copy `len` bytes from one object to another through the staging
    /// buffer.
    fn copy_object(&mut self, src: u32, dst: u32, len: u64) -> Result<(), DriverError> {
        let mut buf = [0u8; 512];
        let mut done = 0u64;
        while done < len {
            let take = usize::try_from((len - done).min(512)).unwrap_or(512);
            self.object_read(src, done, &mut buf[..take])?;
            self.object_write(dst, done, &buf[..take])?;
            done += take as u64;
        }
        Ok(())
    }

    /// Grow the object at `indaddr` (recorded size `size`) so its
    /// allocation covers `new_size` bytes, relocating it when in-place
    /// growth is impossible. Returns the object's (possibly new)
    /// indirect disc address.
    ///
    /// A relocated shared-fragment object, an entry of the directory at
    /// `parent`, leaves its old fragment to [`Self::release_maybe_shared`].
    fn grow_object(
        &mut self,
        indaddr: u32,
        size: u32,
        new_size: u64,
        parent: u32,
    ) -> Result<u32, DriverError> {
        if new_size <= u64::from(size) && indaddr != 0 {
            return Ok(indaddr);
        }
        match &self.backing {
            Backing::Old { .. } => {
                let old_sectors =
                    u32::try_from(u64::from(size).div_ceil(OLD_SECTOR_SIZE)).unwrap_or(0);
                let new_sectors = u32::try_from(new_size.div_ceil(OLD_SECTOR_SIZE))
                    .map_err(|_| DriverError::NoSpace)?;
                if new_sectors <= old_sectors {
                    return Ok(indaddr);
                }
                let mut map = OldMap::load(&mut self.volume)?;
                if indaddr != 0 && old_sectors != 0 {
                    // Try to consume free space directly after the run.
                    if map.try_extend(indaddr + old_sectors, new_sectors - old_sectors) {
                        map.store(&mut self.volume)?;
                        return Ok(indaddr);
                    }
                }
                // Relocate: allocate the full run, copy, free the old.
                let start = map.allocate(new_sectors)?;
                if indaddr != 0 && old_sectors != 0 {
                    map.free_span(indaddr, old_sectors)?;
                }
                map.store(&mut self.volume)?;
                if indaddr != 0 && size != 0 {
                    self.copy_object(indaddr, start, u64::from(size))?;
                }
                Ok(start)
            }
            Backing::New { map, .. } => {
                if indaddr == 0 {
                    return map.allocate_object(&mut self.volume, new_size);
                }
                if indaddr.trailing_zeros() >= 8
                    && map.extend_object(&mut self.volume, indaddr >> 8, new_size)?
                {
                    return Ok(indaddr);
                }
                // Relocate to a fresh exclusive object (also the path a
                // shared-fragment object takes to grow).
                let new_indaddr = self.copy_out(indaddr, size, new_size)?;
                self.release_maybe_shared(indaddr, size, parent)?;
                Ok(new_indaddr)
            }
        }
    }

    /// Shrink the allocation of the object at `indaddr` from `size` to
    /// `new_size` bytes.
    fn shrink_object(&mut self, indaddr: u32, size: u32, new_size: u64) -> Result<(), DriverError> {
        if indaddr == 0 || new_size >= u64::from(size) {
            return Ok(());
        }
        match &self.backing {
            Backing::Old { .. } => {
                let old_sectors =
                    u32::try_from(u64::from(size).div_ceil(OLD_SECTOR_SIZE)).unwrap_or(0);
                let new_sectors =
                    u32::try_from(new_size.div_ceil(OLD_SECTOR_SIZE)).unwrap_or(old_sectors);
                if new_sectors >= old_sectors {
                    return Ok(());
                }
                let mut map = OldMap::load(&mut self.volume)?;
                map.free_span(indaddr + new_sectors, old_sectors - new_sectors)?;
                map.store(&mut self.volume)
            }
            Backing::New { map, .. } => {
                if indaddr & 0xFF != 0 {
                    // A shared-fragment object cannot release granules
                    // without moving its fragment neighbours; the slack
                    // is reclaimed when the object is removed or grows
                    // away.
                    return Ok(());
                }
                map.shrink_object(&mut self.volume, indaddr >> 8, new_size)
            }
        }
    }

    /// Release the allocation behind `indaddr`, an entry of the directory
    /// at `parent`, honouring fragment sharing: a shared fragment is freed
    /// only when nothing else uses it.
    fn release_maybe_shared(
        &mut self,
        indaddr: u32,
        size: u32,
        parent: u32,
    ) -> Result<(), DriverError> {
        if self.releases(indaddr, parent)? {
            self.free(indaddr, size)
        } else {
            Ok(())
        }
    }

    /// Whether releasing the allocation behind `indaddr`, an entry of the
    /// directory at `parent`, frees it: a shared fragment is freed only when
    /// nothing else uses it.
    fn releases(&mut self, indaddr: u32, parent: u32) -> Result<bool, DriverError> {
        Ok(indaddr != 0
            && !(self.is_shared(indaddr) && self.fragment_shared_in(parent, indaddr)?))
    }

    /// Whether the object at `indaddr` lies in a fragment it may share.
    fn is_shared(&self, indaddr: u32) -> bool {
        matches!(&self.backing, Backing::New { .. }) && indaddr & 0xFF != 0
    }

    /// A fresh object of its own, `bytes` long, holding the first `size`
    /// bytes of the object at `indaddr`.
    fn copy_out(&mut self, indaddr: u32, size: u32, bytes: u64) -> Result<u32, DriverError> {
        let fresh = self.allocate(bytes)?;
        if let Err(err) = self.copy_object(indaddr, fresh, u64::from(size)) {
            // The copy's failure is the one to report.
            let _ = self.free(fresh, u32::try_from(bytes).unwrap_or(u32::MAX));
            return Err(err);
        }
        Ok(fresh)
    }

    /// Whether the directory at `parent`, or another of its entries, uses
    /// the fragment of the object at `indaddr`.
    ///
    /// They are the only objects that can: `FileCore` shares a disc object
    /// only between a directory and the files in it, or files of one
    /// directory (RISC OS PRM, `FileCore`), so one directory answers what a
    /// walk of the tree once did.
    fn fragment_shared_in(&mut self, parent: u32, indaddr: u32) -> Result<bool, DriverError> {
        let fragment = indaddr >> 8;
        if parent >> 8 == fragment {
            return Ok(true);
        }
        let handle = self.load_dir(parent)?;
        for index in 0..Self::dir_len(&handle) {
            let Some(object) = self.dir_entry(&handle, parent, index)? else {
                break;
            };
            if object.indaddr >> 8 == fragment && object.indaddr != indaddr {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// Bytes reserved in front of a shared-fragment object by its share
/// offset (the object sits `(N - 1)` granules into its fragment).
fn share_base_bytes(record: &tairix_fsprobe::adfs::DiscRecord, indaddr: u32) -> u64 {
    let share = u64::from(indaddr & 0xFF);
    if share == 0 {
        0
    } else {
        (share - 1) << (u32::from(record.log2sharesize) + u32::from(record.log2secsize))
    }
}

/// Largest big directory the format permits (4096 KiB).
const MAX_BIG_DIR: u32 = 4096 * 1024;

/// Characters `FileCore` forbids in an object name (the path and
/// wildcard vocabulary), on top of requiring printable ASCII.
const FORBIDDEN_NAME_BYTES: &[u8] = b"$&%@\\^:.#*\"|";

impl<B: Block> Adfs<B> {
    /// Validate a name being written to a directory.
    fn validate_new_name(&self, name: &[u8]) -> Result<(), DriverError> {
        let max = if self.is_big_dir() {
            dir::MAX_NAME_LEN
        } else {
            dir::FIXED_NAME_LEN
        };
        if name.is_empty() || name.len() > max {
            return Err(DriverError::LengthOutOfRange);
        }
        if name
            .iter()
            .any(|&b| !(0x21..=0x7E).contains(&b) || FORBIDDEN_NAME_BYTES.contains(&b))
        {
            return Err(DriverError::OutOfRange);
        }
        Ok(())
    }

    /// Grow the object at `indaddr` to hold `new_bytes` **without
    /// relocating it** (directories must keep their address). Returns
    /// whether the growth succeeded.
    fn grow_in_place(&mut self, indaddr: u32, new_bytes: u64) -> Result<bool, DriverError> {
        match &self.backing {
            // Old-map directories are fixed-size; nothing ever grows in
            // place.
            Backing::Old { .. } => Ok(false),
            Backing::New { map, .. } => {
                let total = share_base_bytes(&map.record, indaddr) + new_bytes;
                map.extend_object(&mut self.volume, indaddr >> 8, total)
            }
        }
    }

    /// Insert `object` into the directory at `dir_addr`, growing a big
    /// directory in place when it is full.
    fn dir_insert(&mut self, dir_addr: u32, object: &Object) -> Result<(), DriverError> {
        self.generation = self.generation.wrapping_add(1);
        match self.load_dir(dir_addr)? {
            DirHandle::Fixed(mut dir) => {
                dir.insert(object)?;
                self.store_fixed_dir(dir_addr, &dir)
            }
            DirHandle::Big(mut dir) => {
                let mut store = ObjectStore {
                    adfs: self,
                    indaddr: dir_addr,
                    size: u32::MAX,
                };
                match dir.insert(&mut store, object) {
                    Err(DriverError::NoSpace) => {}
                    Ok(()) => {
                        self.validated_as(dir_addr, &dir);
                        return Ok(());
                    }
                    Err(err) => return Err(err),
                }
                // Full: grow by one grain and retry.
                let new_size = dir
                    .header
                    .size
                    .checked_add(bigdir::BIG_DIR_GRAIN)
                    .filter(|&s| s <= MAX_BIG_DIR)
                    .ok_or(DriverError::NoSpace)?;
                if !self.grow_in_place(dir_addr, u64::from(new_size))? {
                    return Err(DriverError::NoSpace);
                }
                let mut store = ObjectStore {
                    adfs: self,
                    indaddr: dir_addr,
                    size: u32::MAX,
                };
                dir.grow(&mut store, new_size)?;
                dir.insert(&mut store, object)?;
                self.validated_as(dir_addr, &dir);
                self.record_dir_size(dir_addr, new_size)
            }
        }
    }

    /// Remove the entry at `index` from the directory at `dir_addr`.
    fn dir_remove_at(&mut self, dir_addr: u32, index: u32) -> Result<(), DriverError> {
        self.generation = self.generation.wrapping_add(1);
        match self.load_dir(dir_addr)? {
            DirHandle::Fixed(mut dir) => {
                dir.remove(index as usize);
                self.store_fixed_dir(dir_addr, &dir)
            }
            DirHandle::Big(mut dir) => {
                let mut store = ObjectStore {
                    adfs: self,
                    indaddr: dir_addr,
                    size: u32::MAX,
                };
                dir.remove(&mut store, index)?;
                self.validated_as(dir_addr, &dir);
                Ok(())
            }
        }
    }

    /// Rewrite the entry at `index` of the directory at `dir_addr` with
    /// `object`'s metadata (the name is unchanged).
    fn dir_update_at(
        &mut self,
        dir_addr: u32,
        index: u32,
        object: &Object,
    ) -> Result<(), DriverError> {
        match self.load_dir(dir_addr)? {
            DirHandle::Fixed(mut dir) => {
                dir.update(index as usize, object);
                self.store_fixed_dir(dir_addr, &dir)
            }
            DirHandle::Big(mut dir) => {
                let mut store = ObjectStore {
                    adfs: self,
                    indaddr: dir_addr,
                    size: u32::MAX,
                };
                dir.update(&mut store, index, object)?;
                self.validated_as(dir_addr, &dir);
                Ok(())
            }
        }
    }

    /// As [`Self::dir_update_at`], also giving the entry `object`'s spelling of
    /// its name, which matches the stored one ignoring case.
    fn dir_respell_at(
        &mut self,
        dir_addr: u32,
        index: u32,
        object: &Object,
    ) -> Result<(), DriverError> {
        match self.load_dir(dir_addr)? {
            DirHandle::Fixed(mut dir) => {
                dir.update(index as usize, object);
                self.store_fixed_dir(dir_addr, &dir)
            }
            DirHandle::Big(mut dir) => {
                let mut store = ObjectStore {
                    adfs: self,
                    indaddr: dir_addr,
                    size: u32::MAX,
                };
                dir.update_respelt(&mut store, index, object)?;
                self.validated_as(dir_addr, &dir);
                Ok(())
            }
        }
    }

    /// The parent address recorded inside the directory at `dir_addr`.
    fn dir_parent(&mut self, dir_addr: u32) -> Result<u32, DriverError> {
        match self.load_dir(dir_addr)? {
            DirHandle::Fixed(dir) => Ok(dir.parent()),
            DirHandle::Big(dir) => Ok(dir.header.parent),
        }
    }

    /// Record a grown big directory's new size where its size is
    /// stored: the parent's entry, or the disc record for the root.
    fn record_dir_size(&mut self, dir_addr: u32, new_size: u32) -> Result<(), DriverError> {
        let root = self.root_node();
        if dir_addr == node_addr(root) {
            // Only big directories grow, and they exist only on new-map
            // volumes.
            let Backing::New { map, boot_block } = &mut self.backing else {
                return Err(DriverError::BadMagic);
            };
            let boot = *boot_block;
            return map.set_root_size(&mut self.volume, boot, new_size);
        }
        let parent = self.dir_parent(dir_addr)?;
        // Find the directory's entry in its parent by address.
        let mut index = 0u32;
        loop {
            let Some(entry) = self.dir_get(parent, index)? else {
                // The tree disagrees with the child's parent pointer.
                return Err(DriverError::BadMagic);
            };
            if entry.indaddr == dir_addr {
                let mut updated = entry;
                updated.size = new_size;
                return self.dir_update_at(parent, index, &updated);
            }
            index += 1;
        }
    }
}

/// A loaded, validated directory of either shape. A fixed directory is
/// held whole on the stack by design (no read or write path allocates),
/// so the variant size difference is deliberate.
#[allow(clippy::large_enum_variant)]
enum DirHandle {
    /// A 1280- or 2048-byte fixed directory, held in memory.
    Fixed(FixedDir),
    /// A big directory (the engine streams through the object).
    Big(BigDir),
}

impl<B: Block> FilesystemRead for Adfs<B> {
    fn root(&self) -> NodeId {
        self.root_node()
    }

    fn name_matching(&self) -> NameMatching {
        NameMatching::AsciiCaseInsensitive
    }

    fn node_identity(&self) -> NodeIdentity {
        NodeIdentity::ContentDerived
    }

    fn node_info(&mut self, node: NodeId) -> Result<NodeInfo, DriverError> {
        match self.read_target(node)? {
            // The root has no directory entry and so no stored stamp; every
            // other node's lives in its parent's entry.
            Target::Root => {
                let root = self.root_addr();
                let allocated = self.object_allocated(root, self.root_size())?;
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
        let dir_addr = self.entries_addr(dir)?;
        let (index, object) = self.child(dir_addr, name)?;
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
        if buf.is_empty() || object.indaddr == 0 || offset >= size {
            return Ok(0);
        }
        let want = usize::try_from((size - offset).min(buf.len() as u64))
            .map_err(|_| DriverError::LengthOutOfRange)?;
        self.object_read(object.indaddr, offset, &mut buf[..want])?;
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
        // One load serves the whole call, however many entries it lists.
        let handle = self.load_dir(indaddr)?;
        let start = self.resume_index(&handle, indaddr, cursor, after)?;
        for index in start..Self::dir_len(&handle) {
            let Some(object) = self.dir_entry(&handle, indaddr, index)? else {
                break;
            };
            let entry = DirEntry {
                node: self.reported(&object, indaddr),
                info: self.info_of(&object, indaddr, NodeInfo::SINGLE_NAME)?,
                next_cursor: (u64::from(self.generation) << 32) | u64::from(index + 1),
            };
            if visit(&entry, object.name()) == DirVisit::Stop {
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
        if parent == self.root_addr() {
            return Ok(root);
        }
        let grandparent = self.dir_parent(parent)?;
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
                    self.free(object.indaddr, object.size)?;
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
                freed = freed.and(self.free(object.indaddr, object.size));
            }
        }
        freed
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
        let allocated = self.object_allocated(object.indaddr, object.size)?;
        let (kind, size) = if object.is_dir() {
            (NodeKind::Directory, 0)
        } else {
            (NodeKind::RegularFile, u64::from(object.size))
        };
        Ok(NodeInfo {
            kind,
            nlink,
            ino: child_node(object, parent).raw(),
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
            Target::Entry { parent, index, .. } => self.dir_update_at(parent, index, object),
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
        if name.is_empty() || name.len() > dir::MAX_NAME_LEN {
            return Err(DriverError::NotFound);
        }
        self.dir_lookup(dir_addr, name)?
            .ok_or(DriverError::NotFound)
    }

    /// The id `object`, an entry of the directory at `parent`, is reported
    /// under: its pin's token while one holds it, else its own.
    fn reported(&self, object: &Object, parent: u32) -> NodeId {
        match self.pins.token_of(object, parent) {
            Some(number) => token_node(number, object.is_dir()),
            None => child_node(object, parent),
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
        self.resolved
            .iter()
            .flatten()
            .find(|(held, ..)| *held == node)
            .map(|&(_, index, object)| (index, object))
    }

    /// Remember that `node` names `object`, the `index`-th entry of its
    /// parent, until the next directory write.
    fn keep_resolved(&mut self, node: NodeId, index: u32, object: &Object) {
        self.resolved[self.next_resolved] = Some((node, index, *object));
        self.next_resolved = (self.next_resolved + 1) % RESOLVED_LEN;
    }

    /// Whether the directory at `dir_addr` has no entries.
    fn dir_is_empty(&mut self, dir_addr: u32) -> Result<bool, DriverError> {
        match self.load_dir(dir_addr)? {
            DirHandle::Fixed(dir) => Ok(dir.count() == 0),
            DirHandle::Big(dir) => Ok(dir.header.entries == 0),
        }
    }

    /// The `Hugo`/`Nick` marker of the fixed directory at `dir_addr`,
    /// copied onto directories created inside it so a volume stays
    /// marker-consistent.
    fn fixed_marker(&mut self, dir_addr: u32) -> Result<[u8; 4], DriverError> {
        match self.load_dir(dir_addr)? {
            DirHandle::Fixed(dir) => {
                let mut marker = [0u8; 4];
                marker.copy_from_slice(&dir.data[1..5]);
                Ok(marker)
            }
            DirHandle::Big(_) => Err(DriverError::BadMagic),
        }
    }

    /// Zero the byte range `[from, to)` of the object at `indaddr`.
    fn zero_object_range(&mut self, indaddr: u32, from: u64, to: u64) -> Result<(), DriverError> {
        let zeroes = [0u8; 512];
        let mut at = from;
        while at < to {
            let take = usize::try_from((to - at).min(512)).unwrap_or(512);
            self.object_write(indaddr, at, &zeroes[..take])?;
            at += take as u64;
        }
        Ok(())
    }

    /// Remove the `src_index`-th entry of the directory at `src_addr`,
    /// naming `object`, and give `object` the entry of `existing`, named
    /// `dst_name` in the directory at `dst_addr`, under that spelling. A pin
    /// of `existing` keeps it, nameless; otherwise its allocation is released.
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
        if let Err(err) = self.respell_over(src_addr, src_index, object, dst_addr, dst_name) {
            if let Some((_, unnamed)) = &pinned {
                self.forget_unnamed(unnamed, existing)?;
            }
            return Err(err);
        }
        match pinned {
            Some((number, unnamed)) => self.keep_unnamed(number, dst_addr, existing, &unnamed),
            None => self.release_maybe_shared(existing.indaddr, existing.size, dst_addr),
        }
    }

    /// Remove the `src_index`-th entry of the directory at `src_addr` and
    /// point the entry named `dst_name` in the directory at `dst_addr` at
    /// `object` under that spelling.
    fn respell_over(
        &mut self,
        src_addr: u32,
        src_index: u32,
        object: &Object,
        dst_addr: u32,
        dst_name: &[u8],
    ) -> Result<(), DriverError> {
        self.dir_remove_at(src_addr, src_index)?;
        // The destination entry holds the replaced object's place in the sort
        // order, which the name asked for shares; indices may have shifted
        // when the source entry left the same directory, so the name is
        // looked up afresh.
        let Some((dst_index, _)) = self.dir_lookup(dst_addr, dst_name)? else {
            return Err(DriverError::BadMagic);
        };
        self.dir_respell_at(dst_addr, dst_index, &object.renamed(dst_name)?)
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
                owned: self.releases(object.indaddr, parent)?,
            });
        }
        let mut kept = *object;
        if self.is_shared(object.indaddr) {
            kept.indaddr = if object.size == 0 {
                0
            } else {
                self.copy_out(object.indaddr, object.size, u64::from(object.size))?
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
        self.free(unnamed.object.indaddr, unnamed.object.size)
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
        self.release_maybe_shared(object.indaddr, object.size, parent)
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
        let dir_addr = self.entries_addr(dir)?;
        self.validate_new_name(name)?;
        if self.dir_lookup(dir_addr, name)?.is_some() {
            return Err(DriverError::AlreadyExists);
        }
        let mut object = typed_at(&Object::named(name)?, FILETYPE_DATA, self.now())?;
        // ADFS keeps no owner, group or mode, so the record has nowhere to
        // go: the access bits are RISC OS's own, set as RISC OS sets them.
        match kind {
            NodeKind::RegularFile => {
                // Created with owner read/write, no data allocation.
                object.attr = acorn::ATTR_OWNER_READ | acorn::ATTR_OWNER_WRITE;
                self.dir_insert(dir_addr, &object)?;
            }
            // Acorn ADFS / FileCore has no symbolic-link object type, so the
            // creation is refused rather than approximated by a file whose
            // contents merely look like a path.
            NodeKind::Symlink => return Err(DriverError::Unsupported),
            NodeKind::Directory => {
                object.attr = acorn::ATTR_DIRECTORY | acorn::ATTR_OWNER_READ;
                // 8-bit ADFS (the 1280-byte directory format) creates
                // directories locked; 32-bit `FileCore` does not.
                if !self.is_big_dir() && self.fixed_format() == FixedFormat::Old {
                    object.attr |= acorn::ATTR_LOCKED;
                }
                let (bytes, big) = if self.is_big_dir() {
                    (bigdir::BIG_DIR_GRAIN, true)
                } else {
                    (fixed_size_u32(self.fixed_format()), false)
                };
                let child = self.allocate(u64::from(bytes))?;
                let seeded = if big {
                    let mut store = ObjectStore {
                        adfs: self,
                        indaddr: child,
                        size: u32::MAX,
                    };
                    BigDir::initialise(&mut store, bytes, name, dir_addr)
                } else {
                    let format = self.fixed_format();
                    self.fixed_marker(dir_addr).and_then(|marker| {
                        let fresh = FixedDir::initialise(format, marker, name, dir_addr);
                        self.store_fixed_dir(child, &fresh)
                    })
                };
                object.indaddr = child;
                object.size = bytes;
                let inserted = seeded.and_then(|()| self.dir_insert(dir_addr, &object));
                if let Err(err) = inserted {
                    // Fail closed: reclaim the orphaned allocation.
                    self.free(child, bytes)?;
                    return Err(err);
                }
            }
        }
        Ok(child_node(&object, dir_addr))
    }

    fn write_at(&mut self, file: NodeId, offset: u64, data: &[u8]) -> Result<usize, DriverError> {
        let target = self.file_target(file)?;
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
            self.grow_object(object.indaddr, size, end, parent)?
        } else {
            object.indaddr
        };
        if offset > u64::from(size) {
            self.zero_object_range(indaddr, u64::from(size), offset)?;
        }
        self.object_write(indaddr, offset, data)?;
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

    fn truncate(&mut self, file: NodeId, size: u64) -> Result<(), DriverError> {
        let target = self.file_target(file)?;
        let (parent, object) = target.object().ok_or(DriverError::Unsupported)?;
        if size > u64::from(u32::MAX) {
            return Err(DriverError::NoSpace);
        }
        let old = u64::from(object.size);
        let mut updated = restamped(&object, self.now())?;
        match size.cmp(&old) {
            core::cmp::Ordering::Greater => {
                updated.indaddr = self.grow_object(object.indaddr, object.size, size, parent)?;
                self.zero_object_range(updated.indaddr, old, size)?;
            }
            core::cmp::Ordering::Less if size == 0 => {
                self.release_maybe_shared(object.indaddr, object.size, parent)?;
                updated.indaddr = 0;
            }
            core::cmp::Ordering::Less => {
                self.shrink_object(object.indaddr, object.size, size)?;
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
        let dir_addr = self.entries_addr(dir)?;
        let (index, object) = self.child(dir_addr, name)?;
        if object.is_dir() && !self.dir_is_empty(object.indaddr)? {
            return Err(DriverError::DirectoryNotEmpty);
        }
        let Some(number) = self.pins.token_of(&object, dir_addr) else {
            self.dir_remove_at(dir_addr, index)?;
            return self.release_maybe_shared(object.indaddr, object.size, dir_addr);
        };
        let unnamed = self.unnamed(&object, dir_addr)?;
        if let Err(err) = self.dir_remove_at(dir_addr, index) {
            self.forget_unnamed(&unnamed, &object)?;
            return Err(err);
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
        let dst_addr = self.entries_addr(dst_dir)?;
        let src_addr = self.entries_addr(src_dir)?;
        let (src_index, object) = self.child(src_addr, src_name)?;
        if src_addr == dst_addr && dir::name_eq(src_name, dst_name) {
            // The source's own entry, re-spelled: names sort and match
            // ignoring case, so it keeps its place and takes the spelling.
            if object.name() == dst_name {
                return Ok(());
            }
            self.validate_new_name(dst_name)?;
            return self.dir_respell_at(src_addr, src_index, &object.renamed(dst_name)?);
        }
        self.validate_new_name(dst_name)?;
        // Moving a directory into itself or its own subtree would
        // detach the cycle from the tree; walk the destination's
        // ancestry (bounded like the liveness walk).
        if object.is_dir() && src_addr != dst_addr {
            let root_addr = self.root_addr();
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
                cursor = self.dir_parent(cursor)?;
            }
            if !reached_root {
                // Ancestry that never reaches the root is corrupt (or
                // cyclic); fail closed rather than risk detaching it.
                return Err(DriverError::BadMagic);
            }
        }
        let replaced = self.dir_lookup(dst_addr, dst_name)?;
        if let Some((_, existing)) = &replaced {
            // Kind-compatible replacement only; a directory may replace
            // only an empty directory.
            if existing.is_dir() != object.is_dir() {
                return Err(DriverError::Unsupported);
            }
            if existing.is_dir() && !self.dir_is_empty(existing.indaddr)? {
                return Err(DriverError::DirectoryNotEmpty);
            }
        }
        // A pin of the moved object follows it, so its room is made before
        // an entry changes.
        self.pins.make_room(&object, src_addr)?;
        if let Some((_, existing)) = replaced {
            self.replace_entry(src_addr, src_index, &object, dst_addr, dst_name, &existing)?;
        } else {
            self.dir_remove_at(src_addr, src_index)?;
            let moved = object.renamed(dst_name)?;
            if let Err(err) = self.dir_insert(dst_addr, &moved) {
                // Roll the source entry back so no entry is lost.
                self.dir_insert(src_addr, &object)?;
                return Err(err);
            }
        }
        self.pins.moved(&object, src_addr, dst_addr, dst_name)?;
        // A moved directory's parent pointer follows it.
        if object.is_dir() && src_addr != dst_addr {
            match self.load_dir(object.indaddr)? {
                DirHandle::Fixed(mut child) => {
                    child.set_parent(dst_addr);
                    self.store_fixed_dir(object.indaddr, &child)?;
                }
                DirHandle::Big(mut child) => {
                    let mut store = ObjectStore {
                        adfs: self,
                        indaddr: object.indaddr,
                        size: u32::MAX,
                    };
                    child.set_parent(&mut store, dst_addr)?;
                }
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        // Every mutation writes through to the device.
        Ok(())
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
        let handle = self.load_dir(parent)?;
        let mut found = None;
        for index in 0..Self::dir_len(&handle) {
            let Some(object) = self.dir_entry(&handle, parent, index)? else {
                break;
            };
            if !node_names(node, &object) {
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
        match (self.is_big_dir(), self.fixed_format()) {
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
        let (_, object) = target.object().ok_or(DriverError::Unsupported)?;
        let decoded = acorn::decode_load_exec(object.load, object.exec);
        let mut updated = object;
        match key.as_bytes() {
            b"acorn.loadaddr" => {
                updated.load =
                    acorn::addr_from_value(value).map_err(|_| DriverError::OutOfRange)?;
            }
            b"acorn.execaddr" => {
                updated.exec =
                    acorn::addr_from_value(value).map_err(|_| DriverError::OutOfRange)?;
            }
            b"acorn.attr" => {
                let attr = acorn::attr_from_value(value).map_err(|_| DriverError::OutOfRange)?;
                // The directory bit is the object's kind, not a free
                // attribute, and the volume's directory format bounds
                // the storable bits.
                if (attr & acorn::ATTR_DIRECTORY != 0) != object.is_dir()
                    || attr & !self.storable_attr_bits() != 0
                {
                    return Err(DriverError::OutOfRange);
                }
                updated.attr = attr;
            }
            b"acorn.filetype" => {
                let filetype =
                    acorn::filetype_from_value(value).map_err(|_| DriverError::OutOfRange)?;
                let centiseconds = match decoded {
                    acorn::LoadExec::Typed { centiseconds, .. } => centiseconds,
                    acorn::LoadExec::Untyped { .. } => 0,
                };
                let (load, exec) = acorn::encode_typed(filetype, centiseconds)
                    .map_err(|_| DriverError::OutOfRange)?;
                updated.load = load;
                updated.exec = exec;
            }
            b"acorn.datestamp" => {
                let centiseconds =
                    acorn::datestamp_from_value(value).map_err(|_| DriverError::OutOfRange)?;
                let acorn::LoadExec::Typed { filetype, .. } = decoded else {
                    // The stamp lives inside the filetype encoding; an
                    // untyped object has nowhere to keep one.
                    return Err(DriverError::Unsupported);
                };
                let (load, exec) = acorn::encode_typed(filetype, centiseconds)
                    .map_err(|_| DriverError::OutOfRange)?;
                updated.load = load;
                updated.exec = exec;
            }
            // ADFS stores no general-purpose attributes; every other
            // namespace has nowhere to live.
            _ => return Err(DriverError::Unsupported),
        }
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
        let (_, object) = target.object().ok_or(DriverError::NotFound)?;
        let decoded = acorn::decode_load_exec(object.load, object.exec);
        let mut updated = object;
        match key.as_bytes() {
            b"acorn.filetype" => match decoded {
                acorn::LoadExec::Typed { .. } => {
                    // Untyping clears the whole encoding — the stamp
                    // lives inside it.
                    updated.load = 0;
                    updated.exec = 0;
                }
                acorn::LoadExec::Untyped { .. } => return Err(DriverError::NotFound),
            },
            b"acorn.datestamp" => match decoded {
                acorn::LoadExec::Typed { filetype, .. } => {
                    let (load, exec) =
                        acorn::encode_typed(filetype, 0).map_err(|_| DriverError::OutOfRange)?;
                    updated.load = load;
                    updated.exec = exec;
                }
                acorn::LoadExec::Untyped { .. } => return Err(DriverError::NotFound),
            },
            // The addresses and attribute bits are structural fields of
            // every entry; they cannot be absent.
            b"acorn.loadaddr" | b"acorn.execaddr" | b"acorn.attr" => {
                return Err(DriverError::Unsupported)
            }
            _ => return Err(DriverError::NotFound),
        }
        self.store(&target, &updated)
    }
}

impl<B: Block> FilesystemStats for Adfs<B> {
    fn stats(&mut self) -> Result<VolumeStats, DriverError> {
        match &self.backing {
            Backing::Old { .. } => {
                let map = OldMap::load(&mut self.volume)?;
                let free = map.free_sectors();
                Ok(VolumeStats {
                    block_size: OLD_SECTOR_SIZE_U32,
                    in_reserve: false,
                    total_blocks: u64::from(map.disc_sectors()),
                    free_blocks: free,
                    avail_blocks: free,
                    // ADFS has no inode table; 0/0 is the honest
                    // "untracked" answer the ABI defines.
                    files: 0,
                    files_free: 0,
                })
            }
            Backing::New { map, .. } => {
                let bpmb = map.record.bytes_per_map_bit();
                let free = map.free_bytes(&mut self.volume)? / bpmb;
                Ok(VolumeStats {
                    block_size: u32::try_from(bpmb).unwrap_or(u32::MAX),
                    in_reserve: false,
                    total_blocks: map.record.disc_size / bpmb,
                    free_blocks: free,
                    avail_blocks: free,
                    files: 0,
                    files_free: 0,
                })
            }
        }
    }
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

/// [`DirStore`] adapter exposing one object's bytes to the big-directory
/// engine.
struct ObjectStore<'a, B: Block> {
    adfs: &'a mut Adfs<B>,
    indaddr: u32,
    size: u32,
}

impl<B: Block> DirStore for ObjectStore<'_, B> {
    fn read_at(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), DriverError> {
        if u64::from(offset) + buf.len() as u64 > u64::from(self.size) {
            return Err(DriverError::BadMagic);
        }
        self.adfs.object_read(self.indaddr, u64::from(offset), buf)
    }

    fn write_at(&mut self, offset: u32, data: &[u8]) -> Result<(), DriverError> {
        if u64::from(offset) + data.len() as u64 > u64::from(self.size) {
            return Err(DriverError::BadMagic);
        }
        self.adfs.directories_changing();
        self.adfs
            .object_write(self.indaddr, u64::from(offset), data)
    }
}
