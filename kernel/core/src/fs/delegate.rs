//! Routing VFS resolution to a `drivers/filesystem/*` driver.
//!
//! [`DelegatedFs`] adapts a borrowed [`FilesystemRead`] driver to the VFS's
//! resolution and permission model within one projection of its volume. The
//! driver supplies **structural** I/O only — it mints opaque [`NodeId`]s and
//! reports each node's kind, size, children, and bytes; it makes no
//! permission decision. Ownership, mode bits, ACLs, and the capability gate
//! stay in the VFS, which authorises every node before reading it and every
//! directory it looks a name up in, each against the record the driver
//! stores for it ([`FilesystemSecurity`]).
//!
//! The adapter is bound for one hold of the driver's lock rather than stored
//! in the [`Vfs`](super::Vfs): the live driver is mapped from the
//! projection's [`DriverHandle`](tairix_abi::driver::DriverHandle) by the
//! kernel host. Its walk ([`DelegatedFs::walk_segment`]) stops on a node of
//! the volume or at the projection's floor, where a caller's `..` and a
//! caller's name naming another projection leave it; what it reached is then
//! acted on under the same hold.
//!
//! A node already reached — pinned by a descriptor, or resolved under the
//! same hold of the driver's lock — is a [`HeldNode`]: every byte operation
//! is judged there, once, whichever way the node was reached.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{
    AllocationClass, DirVisit, FilesystemAttrs, FilesystemRead, FilesystemSecurity,
    FilesystemWrite, NodeId, NodeInfo, NodeKind, NodeSecurity, NodeTimes,
};
use tairix_abi::driver::DriverError;
use tairix_abi::fs::{
    mode_after_owner_change, NodeExpect, RenameFlags, SeekRegion, FS_OWNER_UNCHANGED,
    FS_SETGID_BIT, FS_SYMLINK_MAX,
};
use tairix_abi::{CapabilityId, FileId, OpenFlags};
use tairix_fsmeta::{AttrKey, NamespaceAccess, KEY_MAX};
use tairix_kernel_sec::{GroupId, UserId};
use zeroize::Zeroize;

use super::listing::{DirPosition, ListEnd};
use super::mount::Projection;
use super::node::{NodeRef, VolumeNodes};
use super::path::{owned_name, parse_link_target, Path, Step, TargetStep, MAX_PATH_COMPONENTS};
use super::perm::{Access, CreateMode, Credentials, Metadata, Mode};
use super::{BoundedReadError, VfsError};

/// Maximum symbolic links a single resolution may follow.
///
/// The conventional Unix `MAXSYMLINKS`. It is a fail-closed *security*
/// bound on untrusted on-disk structure, not a capacity: a cycle is refused
/// with [`VfsError::LinkLoop`] after this many hops rather than walked until
/// the kernel runs out of stack.
pub const SYMLINK_HOP_MAX: usize = 40;

/// Maximum path steps a single resolution may consume.
///
/// Derived from the two bounds that produce steps — a caller's own path is
/// at most [`MAX_PATH_COMPONENTS`], and each of the [`SYMLINK_HOP_MAX`]
/// permitted hops may splice at most that many more — so the total work of
/// one resolution is bounded even when every component is a link.
pub const MAX_RESOLVE_STEPS: usize = MAX_PATH_COMPONENTS * (SYMLINK_HOP_MAX + 1);

pub use tairix_abi::fs::FinalLink;

/// Structural metadata of a delegated node, paired with the VFS
/// [`Metadata`] that governs it.
///
/// `kind` and `size` come from the driver's on-disk layout; `meta` is the
/// node's own stored record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegatedInfo {
    /// Whether the node is a directory or a regular file.
    pub kind: NodeKind,
    /// How many directory entries name this node, as the driver read it
    /// from the format. Carried through unchanged: the VFS could not count
    /// names without walking every directory on the volume, so the format's
    /// own record is the only honest source.
    pub nlink: u32,
    /// File length in bytes; `0` for a directory.
    pub size: u64,
    /// Bytes of on-disk storage the node's data occupies, as the driver
    /// reports it from the format's own allocation tracking.
    pub allocated: u64,
    /// The permission metadata applied to the node.
    pub meta: Metadata,
    /// The node's number as `stat` reports it ([`NodeInfo::ino`]). Paired
    /// with the projection's volume id it forms the node's system-wide
    /// [`tairix_abi::FileId`], which distinguishes "this file grew" from "a
    /// different file now sits at this name" and keys the file-change
    /// notification.
    pub ino: u64,
    /// The node's four timestamps, as the driver reported them.
    pub times: NodeTimes,
    /// The node's content generation, as the driver reported it.
    pub content_gen: u64,
}

impl DelegatedInfo {
    /// The node `info` describes, governed by `meta`.
    pub(super) fn of(info: &NodeInfo, meta: Metadata) -> Self {
        Self {
            kind: info.kind,
            nlink: info.nlink,
            size: info.size,
            allocated: info.allocated,
            meta,
            ino: info.ino,
            times: info.times,
            content_gen: info.content_gen,
        }
    }
}

/// One entry of a delegated directory listing: the child's name and the
/// structural metadata the listing driver reported for it.
///
/// The metadata's node number ([`NodeInfo::ino`], the number
/// [`DelegatedInfo::ino`] reports for the same node) is what lets a listing
/// consumer tell a second *name* for one node from a second node — the
/// hard-link deduplication `du` needs — without re-`stat`ing every child by
/// path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegatedEntry {
    /// The child's structural metadata (kind, name count, number, sizes,
    /// times), as the listing driver read it.
    pub info: NodeInfo,
    /// The child's name (a single component, never `.`/`..`).
    pub name: String,
}

/// One entry a listing batch hands over, borrowed from the driver for the
/// visit.
#[derive(Debug)]
pub struct DelegatedRef<'a> {
    /// The child's structural metadata, as the listing driver read it.
    pub info: NodeInfo,
    /// The child's name (a single component, never `.`/`..`).
    pub name: &'a str,
}

impl DelegatedRef<'_> {
    /// This entry, owning its name, for a caller that keeps it past the visit.
    ///
    /// # Errors
    ///
    /// [`VfsError::OutOfMemory`] when the name cannot be held.
    pub fn owned(&self) -> Result<DelegatedEntry, VfsError> {
        Ok(DelegatedEntry {
            info: self.info,
            name: owned_name(self.name)?,
        })
    }
}

/// The name a driver listed, if it is a component a path could name: a
/// driver translates what its format records into one, so a `..` or a `/`
/// reaching here is a driver's defect, refused before any caller joins it
/// into a path.
fn component_name(name: &[u8]) -> Option<&str> {
    core::str::from_utf8(name)
        .ok()
        .filter(|name| super::path::is_component(name))
}

/// A name is decrypted user data: wiped before its allocation is released, as
/// the volume cache wipes its own copies.
impl Drop for DelegatedEntry {
    fn drop(&mut self) {
        self.name.zeroize();
    }
}

/// One node a walk stands on, with everything the walk learned about it: the
/// driver's node id, the name that led there, and its structural and
/// permission metadata.
///
/// Carrying the metadata makes `..` back down the stack free, and carrying
/// the name is what lets a completed walk report the *place* its final
/// component occupies, which every name mutation needs.
struct Walked {
    node: NodeId,
    /// The name this node was looked up under, or `None` for the node the
    /// stack stands on at its bottom: a floor, a held start, or a directory
    /// the walk climbed to.
    name: Option<String>,
    info: NodeInfo,
    meta: Metadata,
}

/// The place a path's final name occupies.
///
/// A driver's name mutations are keyed `(dir, name)`, so a walk reports where
/// a name lives and not only what lives there. Under [`FinalLink::Follow`]
/// the place is the one a final symbolic link *names*, which is what makes an
/// `O_CREAT` act on the target instead of on the link.
pub(super) struct Place {
    /// The directory holding the name.
    pub(super) parent: NodeId,
    /// That directory's structural record, which tells whether any name still
    /// reaches it.
    parent_info: NodeInfo,
    /// That directory's own permission metadata, for a caller that must
    /// authorise a change to its entries.
    parent_meta: Metadata,
    /// The final name within `parent`.
    pub(super) name: String,
    /// The node occupying the place, looked up by the walk itself, or `None`
    /// when the name is vacant.
    found: Option<(NodeId, NodeInfo, Metadata)>,
}

impl Place {
    /// This place, once `cred` may search and change the directory holding
    /// it. Asked before the occupant is inspected, so a caller that may not
    /// change the directory learns nothing about what the name holds.
    fn for_write(self, cred: &Credentials<'_>) -> Result<Self, VfsError> {
        self.parent_meta.authorize(cred, Access::Execute)?;
        self.parent_meta.authorize(cred, Access::Write)?;
        Ok(self)
    }

    /// [`Self::for_write`], for a name about to be made: a directory no name
    /// reaches any more takes none.
    fn for_new_name(self, cred: &Credentials<'_>) -> Result<Self, VfsError> {
        if self.parent_info.nlink == 0 {
            return Err(VfsError::NotFound);
        }
        self.for_write(cred)
    }

    /// Whether a node holds the name.
    pub(super) fn is_occupied(&self) -> bool {
        self.found.is_some()
    }
}

/// What a projection shows of its volume: the path *on the backing volume*
/// at which it is rooted, and the flags and class its mutations answer to.
///
/// A whole-volume projection is rooted at the driver's own root directory
/// (an empty [`subtree`](Self::subtree)); a **sub-mount** shows only a
/// subtree of a larger volume. That subtree's root, never the driver root,
/// is the floor a link's `..` and absolute target clamp to — so a link
/// stored inside the projection cannot resolve to a node it does not show,
/// and every node a walk reaches has a path under the mount point.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct MountProjection<'m> {
    /// The projection's root path on the backing volume, empty for one
    /// rooted at the driver's own root.
    subtree: &'m [String],
    /// The class every driver mutation through the projection allocates
    /// under.
    class: AllocationClass,
    /// Whether the projection refuses every mutation.
    read_only: bool,
}

impl<'m> MountProjection<'m> {
    /// What `projection` shows of its volume.
    fn of(projection: &'m Projection) -> Self {
        Self {
            subtree: projection.backing_subtree(),
            class: projection.class(),
            read_only: projection.is_read_only(),
        }
    }
}

/// Whether a descent may run out of tree before it runs out of steps.
///
/// A vacant *final* name is always reported rather than refused — it is the
/// place a create acts on — so this governs only an **ancestor** that does
/// not exist. Every ordinary operation refuses one: a write cannot land in a
/// directory that is not there. Canonicalisation is the one caller that may
/// ask for the rest of the path anyway, because naming a path that does not
/// exist yet is what `realpath -m` is for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum MissingSteps {
    /// A missing ancestor is [`VfsError::NotFound`].
    Refused,
    /// A missing ancestor ends resolution; the steps below it are carried
    /// through unresolved.
    Carried,
}

/// Whether a walk follows the symbolic links it meets.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Links {
    /// Follow each as the posture asks.
    Follow,
    /// Refuse any it would follow with [`VfsError::LinkLoop`]: a walk that
    /// must reach its node by physical names alone.
    Refuse,
}

/// What a resolution carries from one projection's walk to the next.
pub(super) struct Road {
    /// The steps left to take.
    pub(super) pending: VecDeque<Step>,
    /// The names below the deepest node that exists, which nothing occupies.
    pub(super) absent: Vec<String>,
    /// Whether a final symbolic link is followed.
    final_link: FinalLink,
    /// Whether a missing ancestor ends the walk.
    pub(super) missing: MissingSteps,
    /// Whether a link met is followed at all.
    links: Links,
    /// The view path of the node the walk's stack stands on at its bottom,
    /// kept only for an operation that reports where it ended.
    spelled: Option<Vec<String>>,
    hops: usize,
    steps: usize,
}

impl Road {
    /// A walk of `pending`, a final link followed as `final_link` says, a
    /// missing ancestor answered as `missing` says, and the view path kept
    /// from `spelled` when it is given.
    pub(super) fn new(
        pending: VecDeque<Step>,
        final_link: FinalLink,
        missing: MissingSteps,
        links: Links,
        spelled: Option<Vec<String>>,
    ) -> Self {
        Self {
            pending,
            absent: Vec::new(),
            final_link,
            missing,
            links,
            spelled,
            hops: 0,
            steps: 0,
        }
    }

    /// Count one step against the resolution's bound.
    ///
    /// # Errors
    ///
    /// [`VfsError::LinkLoop`] past [`MAX_RESOLVE_STEPS`].
    pub(super) fn spend(&mut self) -> Result<(), VfsError> {
        self.steps += 1;
        if self.steps > MAX_RESOLVE_STEPS {
            return Err(VfsError::LinkLoop);
        }
        Ok(())
    }

    fn hop(&mut self) -> Result<(), VfsError> {
        self.hops += 1;
        if self.hops > SYMLINK_HOP_MAX {
            return Err(VfsError::LinkLoop);
        }
        Ok(())
    }

    /// `step`, unless the names below the deepest node that exists take it:
    /// a name joins them and a `..` takes the last of them back, so the walk
    /// resumes on real nodes once they are gone.
    ///
    /// # Errors
    ///
    /// [`VfsError::OutOfMemory`] when a name cannot be held.
    pub(super) fn carried(&mut self, step: Step) -> Result<Option<Step>, VfsError> {
        if self.absent.is_empty() {
            return Ok(Some(step));
        }
        match step.kind {
            TargetStep::Up => {
                self.absent.pop();
            }
            TargetStep::Name(name) => self.vacant(name)?,
        }
        Ok(None)
    }

    /// `name` names nothing beneath the deepest node that exists.
    ///
    /// # Errors
    ///
    /// [`VfsError::OutOfMemory`] when it cannot be held.
    pub(super) fn vacant(&mut self, name: String) -> Result<(), VfsError> {
        self.absent
            .try_reserve(1)
            .map_err(|_| VfsError::OutOfMemory)?;
        self.absent.push(name);
        Ok(())
    }

    /// The walk stands at the floor of the projection shown at `path`.
    ///
    /// # Errors
    ///
    /// [`VfsError::OutOfMemory`] when the path cannot be held.
    pub(super) fn stand_at(&mut self, path: &Path) -> Result<(), VfsError> {
        if let Some(spelled) = self.spelled.as_mut() {
            *spelled = path.owned_components()?;
        }
        Ok(())
    }

    /// The walk's stack climbed to the parent of what it stood on.
    fn climbed(&mut self) {
        if let Some(spelled) = self.spelled.as_mut() {
            spelled.pop();
        }
    }

    /// A link's target, to be walked before what follows it.
    fn splice(&mut self, steps: Vec<TargetStep>) -> Result<(), VfsError> {
        self.pending
            .try_reserve(steps.len())
            .map_err(|_| VfsError::OutOfMemory)?;
        for kind in steps.into_iter().rev() {
            self.pending.push_front(Step {
                kind,
                from_link: true,
            });
        }
        Ok(())
    }

    /// Spell the names the walk's stack holds above its bottom.
    fn spell_down(&mut self, stack: &[Walked]) -> Result<(), VfsError> {
        if let Some(spelled) = self.spelled.as_mut() {
            spelled
                .try_reserve(stack.len())
                .map_err(|_| VfsError::OutOfMemory)?;
            for walked in stack.iter().skip(1) {
                spelled.push(owned_name(walked.name.as_deref().ok_or(VfsError::Io)?)?);
            }
        }
        Ok(())
    }

    /// Spell the names below the deepest node that exists, where the walk
    /// ended.
    ///
    /// # Errors
    ///
    /// [`VfsError::OutOfMemory`] when they cannot be held.
    pub(super) fn spell_absent(&mut self) -> Result<(), VfsError> {
        if let Some(spelled) = self.spelled.as_mut() {
            spelled
                .try_reserve(self.absent.len())
                .map_err(|_| VfsError::OutOfMemory)?;
            for name in &self.absent {
                spelled.push(owned_name(name)?);
            }
        }
        Ok(())
    }

    /// The view path the walk ended at, when it kept one.
    pub(super) fn take_spelled(&mut self) -> Option<Vec<String>> {
        self.spelled.take()
    }
}

/// Where one projection's walk begins.
#[derive(Copy, Clone, Debug)]
pub(super) enum Begin {
    /// The projection's floor; `check` asks for search permission on it
    /// even if no step looks a name up there.
    Floor {
        /// Whether the floor is where the caller's relative path starts.
        check: bool,
    },
    /// A directory a descriptor holds there, where the caller's relative path
    /// starts.
    Held {
        /// Its pinned id.
        node: NodeId,
        /// Whether it is the projection's floor.
        at_floor: bool,
    },
}

/// What a walk knows of the projection it walks beyond its volume.
pub(super) struct Edge<'e> {
    /// The view path of the projection's floor.
    pub(super) path: &'e Path,
    /// Whether a caller's `..` at the floor leaves the projection: not the
    /// root's, whose `..` is itself.
    pub(super) has_parent: bool,
    /// The projection shown beneath the floor under a name, if one is.
    pub(super) beneath: &'e dyn Fn(&str) -> Option<Arc<Projection>>,
}

/// Where one projection's walk stopped.
// One per resolution, consumed at once: boxing the landed walk would put a
// heap allocation on every path walk.
#[allow(clippy::large_enum_variant)]
pub(super) enum Descent {
    /// Every step it was given was taken on this volume.
    Landed(Walk),
    /// A caller's `..` at the floor: the walk goes on at the floor of the
    /// projection this one sits beneath.
    Above,
    /// A caller's name at the floor that another projection is shown under:
    /// the walk goes on at that projection's floor.
    Into(Arc<Projection>),
}

/// What a walk reached on its volume.
// As `Descent`: returned once per walk and consumed at once.
#[allow(clippy::large_enum_variant)]
pub(super) enum Walk {
    /// A node the walk stood on that no directory of it holds: the floor, a
    /// held start, or a directory it climbed to.
    Bottom {
        /// The node.
        node: NodeId,
        /// Its structural record.
        info: NodeInfo,
        /// Its permission metadata.
        meta: Metadata,
        /// Whether it is the projection's floor.
        at_floor: bool,
    },
    /// The path's final name, in the directory holding it.
    Leaf(Place),
    /// Names beneath a directory that does not exist, which only a
    /// canonicalisation tolerating every missing component reaches.
    Beyond,
}

/// A node a walk reached.
pub(super) struct Reached {
    /// The node.
    pub(super) node: NodeId,
    /// Its structural record.
    pub(super) info: NodeInfo,
    /// Its permission metadata.
    pub(super) meta: Metadata,
    /// Whether it is the projection's floor.
    pub(super) at_floor: bool,
}

impl Walk {
    /// The node the walk reached.
    ///
    /// # Errors
    ///
    /// [`VfsError::NotFound`] when its final name is vacant.
    pub(super) fn reached(self) -> Result<Reached, VfsError> {
        match self {
            Self::Bottom {
                node,
                info,
                meta,
                at_floor,
            } => Ok(Reached {
                node,
                info,
                meta,
                at_floor,
            }),
            Self::Leaf(Place {
                found: Some((node, info, meta)),
                ..
            }) => Ok(Reached {
                node,
                info,
                meta,
                at_floor: false,
            }),
            Self::Leaf(_) | Self::Beyond => Err(VfsError::NotFound),
        }
    }

    /// The place the walk's final name occupies.
    ///
    /// # Errors
    ///
    /// [`VfsError::InvalidPath`] for a node no directory of the walk holds,
    /// which no name mutation may name, and [`VfsError::NotFound`] beneath a
    /// directory that does not exist.
    pub(super) fn place(self) -> Result<Place, VfsError> {
        match self {
            Self::Leaf(place) => Ok(place),
            Self::Bottom { .. } => Err(VfsError::InvalidPath),
            Self::Beyond => Err(VfsError::NotFound),
        }
    }

    /// The vacant place a name is to be made at.
    fn for_creation(self) -> Result<Place, VfsError> {
        match self {
            Self::Leaf(place) => Ok(place),
            // The name is a directory the walk stood on.
            Self::Bottom { .. } => Err(VfsError::AlreadyExists),
            Self::Beyond => Err(VfsError::NotFound),
        }
    }
}

/// What a removal or a rename expects of the node a name holds.
#[derive(Copy, Clone)]
pub(super) enum Expect<'a> {
    /// The node of this identity.
    Node(FileId),
    /// The node this records, unchanged since it was read.
    Unchanged(&'a NodeExpect),
}

impl<'a> Expect<'a> {
    /// This expectation of a node of the volume published as `volume`, the
    /// one the operation holds.
    pub(super) const fn on(self, volume: [u8; 16]) -> Expected<'a> {
        Expected {
            expect: self,
            volume,
        }
    }
}

/// An [`Expect`] of a node of one volume, checked under the hold of the
/// driver's lock that changes it.
#[derive(Copy, Clone)]
pub(super) struct Expected<'a> {
    expect: Expect<'a>,
    volume: [u8; 16],
}

impl Expected<'_> {
    /// Whether `info` describes the node expected: a node of another volume
    /// never is, whatever its number there.
    fn met_by(self, info: &NodeInfo) -> bool {
        let here = FileId {
            volume: self.volume,
            node: info.ino,
        };
        match self.expect {
            Expect::Node(id) => id == here,
            Expect::Unchanged(was) => {
                was.id == here
                    && info.content_gen == was.content_gen
                    && info.size == was.size
                    && info.times.modified == was.modified
                    && info.times.changed == was.changed
            }
        }
    }
}

/// What a walk knows of where its projection's floor is.
struct FloorAt {
    /// The floor's node, once found.
    node: Option<NodeId>,
    /// Whether the bottom of the walk's stack is the floor, once known.
    bottom: Option<bool>,
}

/// Maps a [`DriverError`] onto the VFS error type.
///
/// Every structural refusal a driver can report carries its own code, so
/// this mapping is total and the same wherever a driver call is made: a
/// taken name is [`VfsError::AlreadyExists`] whichever operation met it, a
/// populated directory is [`VfsError::NotEmpty`], a move that would make a
/// directory its own descendant is [`VfsError::DirectoryCycle`], a full
/// volume and exhausted memory are [`VfsError::NoSpace`] and
/// [`VfsError::OutOfMemory`], a volume that has stopped writing is
/// [`VfsError::ReadOnly`], and a change the volume's own record forbids is
/// [`VfsError::PermissionDenied`]. Only an unrecoverable backing fault
/// reaches [`VfsError::Io`].
///
/// A driver also answers [`DriverError::Unsupported`] for a node of the
/// wrong kind, but every walk and operation here checks kinds before it
/// calls the driver, so the code arrives only as what the format cannot do.
pub(super) const fn map_driver_error(error: DriverError) -> VfsError {
    match error {
        DriverError::NotFound => VfsError::NotFound,
        DriverError::Unsupported => VfsError::NotSupported,
        DriverError::BufferTooSmall | DriverError::LengthOutOfRange => VfsError::InvalidPath,
        DriverError::AlreadyExists => VfsError::AlreadyExists,
        DriverError::DirectoryNotEmpty => VfsError::NotEmpty,
        DriverError::DirectoryCycle => VfsError::DirectoryCycle,
        // A fixed on-disk count, exhausted: reported as itself so a caller
        // is not told to free space that would not help.
        DriverError::TooManyLinks => VfsError::TooManyLinks,
        DriverError::NoSpace => VfsError::NoSpace,
        DriverError::OutOfMemory => VfsError::OutOfMemory,
        DriverError::ReadOnly => VfsError::ReadOnly,
        // The volume's own record forbids the change — a node `chattr` holds
        // fixed — whatever the caller may do elsewhere.
        DriverError::PermissionDenied => VfsError::PermissionDenied,
        _ => VfsError::Io,
    }
}

/// A filesystem driver bound to one projection of its volume, walking and
/// acting within it, every node judged by the record it stores.
///
/// The driver is borrowed behind [`FilesystemRead`] and
/// [`FilesystemSecurity`], and [`FilesystemWrite`] for the mutating methods.
pub struct DelegatedFs<'fs, R: FilesystemRead + ?Sized> {
    fs: &'fs mut R,
    mount: MountProjection<'fs>,
}

/// The permission metadata the record `fs` stores for `node` grants.
fn stored_record<R: FilesystemSecurity + ?Sized>(
    fs: &mut R,
    node: NodeId,
) -> Result<Metadata, VfsError> {
    let sec = fs.security(node).map_err(map_driver_error)?;
    Ok(Metadata::from_node_security(&sec))
}

/// The record a node of `kind` that `cred` creates in the directory `dir` is
/// born with, `mode` naming the bits it asked for, handed to the driver in
/// the creating call so a create has no second step to undo.
///
/// It is the bits `mode` lets through, owned by the creator, who is never
/// locked out of a node it just made: in `dir`'s group when `dir` is setgid,
/// which a new directory inherits with the bit, else in the creator's own; a
/// file keeps setgid only in a group its creator belongs to.
fn creation_record(
    cred: &Credentials<'_>,
    dir: &Metadata,
    kind: NodeKind,
    mode: CreateMode,
) -> NodeSecurity {
    let inherits = dir.mode.has(Mode::SETGID);
    let group = if inherits { dir.group } else { cred.gid };
    let mut bits = mode.bits();
    if inherits && kind == NodeKind::Directory {
        bits |= FS_SETGID_BIT;
    }
    if kind == NodeKind::RegularFile && !cred.is_in_group(group) {
        bits &= !FS_SETGID_BIT;
    }
    NodeSecurity::new(bits, cred.uid.0, group.0)
}

impl<'fs, R: FilesystemRead + ?Sized> DelegatedFs<'fs, R> {
    /// Bind `fs` to what `projection` shows of its volume.
    pub(super) fn bind(fs: &'fs mut R, projection: &'fs Projection) -> Self {
        Self {
            fs,
            mount: MountProjection::of(projection),
        }
    }
}

impl<R: FilesystemRead + FilesystemWrite + ?Sized> DelegatedFs<'_, R> {
    /// Run one driver mutation under the class the projection gives its
    /// writes.
    fn mutate<T, E>(&mut self, op: impl FnOnce(&mut R) -> Result<T, E>) -> Result<T, E> {
        with_class(self.fs, self.mount.class, op)
    }
}

/// Run one driver mutation of `fs` under `class`, putting the ordinary class
/// back after it.
fn with_class<R: FilesystemWrite + ?Sized, T, E>(
    fs: &mut R,
    class: AllocationClass,
    op: impl FnOnce(&mut R) -> Result<T, E>,
) -> Result<T, E> {
    fs.set_allocation_class(class);
    let result = op(fs);
    fs.set_allocation_class(AllocationClass::Ordinary);
    result
}

/// Validate `key` against the shared `lib/fsmeta` grammar and refuse the
/// privileged namespaces.
///
/// `system.*` and `trusted.*` guard a security boundary whose dedicated
/// capability is introduced together with the first service that holds and
/// enforces it; until that service exists the namespaces are reserved and
/// every request fails closed as a permission denial — the same answer a
/// capability check would give a holder-less caller.
fn parse_unprivileged_key(key: &[u8]) -> Result<AttrKey, VfsError> {
    let key = AttrKey::parse(key).map_err(|_| VfsError::InvalidKey)?;
    if key.access() == NamespaceAccess::Privileged {
        return Err(VfsError::PermissionDenied);
    }
    Ok(key)
}

/// Map a [`DriverError`] from a [`FilesystemAttrs`] call onto the VFS
/// error surface. Attribute reads and writes carry outcomes the
/// path-resolution mapping ([`map_driver_error`]) would misreport — a
/// too-small *value* buffer is the caller's to grow, not an invalid path,
/// and an exhausted attribute bound is a space refusal — so they map here.
/// A namespace the format has nowhere to keep is what the format cannot do,
/// which a copy reports as not carried rather than as a failed device.
const fn map_attr_driver_error(error: DriverError) -> VfsError {
    match error {
        DriverError::NotFound => VfsError::NotFound,
        DriverError::Unsupported => VfsError::NotSupported,
        DriverError::BufferTooSmall => VfsError::BufferTooSmall,
        DriverError::NoSpace => VfsError::NoSpace,
        DriverError::OutOfMemory => VfsError::OutOfMemory,
        DriverError::OutOfRange | DriverError::LengthOutOfRange => VfsError::InvalidKey,
        DriverError::ReadOnly => VfsError::ReadOnly,
        _ => VfsError::Io,
    }
}

impl<R: FilesystemRead + FilesystemSecurity + ?Sized> DelegatedFs<'_, R> {
    /// Walk `road`'s steps within this projection from where `begin` stands,
    /// enforcing search (execute) permission on every directory a name or a
    /// `..` is looked up in — the start directory included — until the steps
    /// run out or the walk must leave the projection.
    ///
    /// `..` pops the walked stack, and below what the stack holds it climbs
    /// with the driver's `parent`, so it names the directory the walk really
    /// came through, never one a spelling suggests. At the floor a caller's
    /// `..` leaves for the projection above ([`Descent::Above`]) and a
    /// caller's name another projection is shown under leaves for that one
    /// ([`Descent::Into`]). What a link supplies never leaves: its `..`
    /// stops at the floor, its absolute target restarts there, and a name it
    /// supplies that another projection is shown under is not found, so a
    /// link cannot resolve outside what its projection shows.
    ///
    /// Links in every position but the last are always followed: such a
    /// component is being used as a directory, so what matters is what it
    /// names. The road's posture decides the last one —
    /// [`FinalLink::Keep`] is the `lstat`/`readlink`/`unlink` posture.
    ///
    /// # Errors
    ///
    /// [`VfsError::LinkLoop`] past [`SYMLINK_HOP_MAX`] hops or
    /// [`MAX_RESOLVE_STEPS`] steps, [`VfsError::NotADirectory`] for a step
    /// taken from a non-directory, [`VfsError::NotFound`] for a missing
    /// ancestor or a climb from a directory no name reaches, and the
    /// permission refusals.
    pub(super) fn walk_segment(
        &mut self,
        cred: &Credentials<'_>,
        begin: Begin,
        road: &mut Road,
        edge: &Edge<'_>,
    ) -> Result<Descent, VfsError> {
        let mut floor = FloorAt {
            node: None,
            bottom: None,
        };
        let first = self.segment_start(cred, begin, &mut floor)?;
        let mut stack = Vec::new();
        stack.try_reserve(1).map_err(|_| VfsError::OutOfMemory)?;
        stack.push(first);

        while let Some(step) = road.pending.pop_front() {
            road.spend()?;
            let Some(step) = road.carried(step)? else {
                continue;
            };
            let Some(top) = stack.last() else {
                return Err(VfsError::Io);
            };
            if top.info.kind != NodeKind::Directory {
                return Err(VfsError::NotADirectory);
            }
            top.meta.authorize(cred, Access::Execute)?;
            let here = top.node;
            let at_bottom = stack.len() == 1;

            match step.kind {
                TargetStep::Up => {
                    if !at_bottom {
                        stack.pop();
                        continue;
                    }
                    if self.at_floor(cred, here, &mut floor)? {
                        // A link's `..` never leaves its projection, and `/..`
                        // is `/`.
                        if step.from_link || !edge.has_parent {
                            continue;
                        }
                        return Ok(Descent::Above);
                    }
                    let up = self.fs.parent(here).map_err(map_driver_error)?;
                    let climbed = self.walked(up, None)?;
                    stack.clear();
                    stack.push(climbed);
                    floor.bottom = None;
                    road.climbed();
                }
                TargetStep::Name(name) => {
                    if at_bottom && self.at_floor(cred, here, &mut floor)? {
                        if let Some(child) = (edge.beneath)(name.as_str()) {
                            // A link's name never crosses into the projection
                            // shown here, and what that projection covers has
                            // no path in the view.
                            if step.from_link {
                                return Err(VfsError::NotFound);
                            }
                            return Ok(Descent::Into(child));
                        }
                    }
                    let is_final = road.pending.is_empty();
                    let child = match self.fs.lookup(here, name.as_bytes()) {
                        Ok(child) => child,
                        // A vacant *final* name is still a place — it is what
                        // a create acts on. A vacant name anywhere earlier is
                        // a missing ancestor, reported unless the caller asked
                        // to be told the rest of the path instead.
                        Err(DriverError::NotFound)
                            if is_final || road.missing == MissingSteps::Carried =>
                        {
                            road.vacant(name)?;
                            continue;
                        }
                        Err(err) => return Err(map_driver_error(err)),
                    };
                    let walked = self.walked(child, Some(name))?;
                    if walked.info.kind == NodeKind::Symlink
                        && !(is_final && road.final_link == FinalLink::Keep)
                    {
                        if road.links == Links::Refuse {
                            return Err(VfsError::LinkLoop);
                        }
                        road.hop()?;
                        let target = self.read_link_target(child, walked.info.size)?;
                        let parsed = parse_link_target(&target)?;
                        // A relative target continues from the link's own
                        // directory, where the stack already stands.
                        if parsed.is_absolute() {
                            let floor_node = self.floor_node(cred, &mut floor)?;
                            let floor_walked = self.walked(floor_node, None)?;
                            stack.clear();
                            stack.push(floor_walked);
                            floor.bottom = Some(true);
                            road.stand_at(edge.path)?;
                        }
                        road.splice(parsed.into_steps())?;
                        continue;
                    }
                    stack.try_reserve(1).map_err(|_| VfsError::OutOfMemory)?;
                    stack.push(walked);
                }
            }
        }
        self.land(cred, stack, road, &mut floor)
            .map(Descent::Landed)
    }

    /// The node a segment starts from — its projection's floor or a held node —
    /// checked as a searchable directory where `begin` asks for it.
    fn segment_start(
        &mut self,
        cred: &Credentials<'_>,
        begin: Begin,
        floor: &mut FloorAt,
    ) -> Result<Walked, VfsError> {
        let (first, check) = match begin {
            Begin::Floor { check } => {
                let root = self.projected_root(cred)?;
                floor.node = Some(root.node);
                floor.bottom = Some(true);
                (root, check)
            }
            Begin::Held { node, at_floor } => {
                if at_floor {
                    floor.node = Some(node);
                }
                floor.bottom = Some(at_floor);
                (self.walked(node, None)?, true)
            }
        };
        if check {
            if first.info.kind != NodeKind::Directory {
                return Err(VfsError::NotADirectory);
            }
            first.meta.authorize(cred, Access::Execute)?;
        }
        Ok(first)
    }

    /// The [`Walk`] a walk that took all its steps reports, the view path it
    /// ended at spelled when the road keeps one.
    fn land(
        &mut self,
        cred: &Credentials<'_>,
        mut stack: Vec<Walked>,
        road: &mut Road,
        floor: &mut FloorAt,
    ) -> Result<Walk, VfsError> {
        road.spell_down(&stack)?;
        road.spell_absent()?;
        match road.absent.len() {
            0 => {}
            1 => {
                let (Some(name), Some(parent)) = (road.absent.pop(), stack.pop()) else {
                    return Err(VfsError::Io);
                };
                return Ok(Walk::Leaf(Place {
                    parent: parent.node,
                    parent_info: parent.info,
                    parent_meta: parent.meta,
                    name,
                    found: None,
                }));
            }
            _ => return Ok(Walk::Beyond),
        }
        let top = stack.pop().ok_or(VfsError::Io)?;
        match (top.name, stack.pop()) {
            (_, None) => {
                let at_floor = self.at_floor(cred, top.node, floor)?;
                Ok(Walk::Bottom {
                    node: top.node,
                    info: top.info,
                    meta: top.meta,
                    at_floor,
                })
            }
            (Some(name), Some(parent)) => Ok(Walk::Leaf(Place {
                parent: parent.node,
                parent_info: parent.info,
                parent_meta: parent.meta,
                name,
                found: Some((top.node, top.info, top.meta)),
            })),
            // A named entry is only ever pushed on top of the directory it
            // was looked up in.
            (None, Some(_)) => Err(VfsError::Io),
        }
    }

    /// The node the projection shows as its floor: the driver's own root for
    /// a whole-volume projection, or the node the projection's subtree names
    /// for a sub-mount.
    ///
    /// The subtree is a path *on the backing volume*, resolved from the
    /// driver root under search permission on every directory it passes and
    /// following no link, so the floor is where the projection was made.
    ///
    /// # Errors
    ///
    /// [`VfsError::NotFound`] if the subtree names nothing, and
    /// [`VfsError::NotADirectory`] if it names something else.
    fn projected_root(&mut self, cred: &Credentials<'_>) -> Result<Walked, VfsError> {
        let subtree = self.mount.subtree;
        let root = self.fs.root();
        let mut here = self.walked(root, None)?;
        for name in subtree {
            if here.info.kind != NodeKind::Directory {
                return Err(VfsError::NotADirectory);
            }
            here.meta.authorize(cred, Access::Execute)?;
            let child = self
                .fs
                .lookup(here.node, name.as_bytes())
                .map_err(map_driver_error)?;
            here = self.walked(child, None)?;
        }
        if here.info.kind != NodeKind::Directory {
            return Err(VfsError::NotADirectory);
        }
        Ok(here)
    }

    /// `node` with its records, reached under `name`.
    fn walked(&mut self, node: NodeId, name: Option<String>) -> Result<Walked, VfsError> {
        let info = self.fs.node_info(node).map_err(map_driver_error)?;
        let meta = stored_record(self.fs, node)?;
        Ok(Walked {
            node,
            name,
            info,
            meta,
        })
    }

    /// The projection's floor, found once per walk.
    fn floor_node(
        &mut self,
        cred: &Credentials<'_>,
        floor: &mut FloorAt,
    ) -> Result<NodeId, VfsError> {
        if let Some(node) = floor.node {
            return Ok(node);
        }
        let node = self.projected_root(cred)?.node;
        floor.node = Some(node);
        Ok(node)
    }

    /// Whether `bottom`, which the walk's stack stands on, is the floor.
    fn at_floor(
        &mut self,
        cred: &Credentials<'_>,
        bottom: NodeId,
        floor: &mut FloorAt,
    ) -> Result<bool, VfsError> {
        if let Some(known) = floor.bottom {
            return Ok(known);
        }
        let is_floor = self.floor_node(cred, floor)? == bottom;
        floor.bottom = Some(is_floor);
        Ok(is_floor)
    }

    /// Read a link's stored target, sized from the node's own reported
    /// length so nothing is truncated and no fixed buffer is guessed.
    fn read_link_target(&mut self, node: NodeId, size: u64) -> Result<String, VfsError> {
        let len = usize::try_from(size).map_err(|_| VfsError::InvalidPath)?;
        if len == 0 || len > FS_SYMLINK_MAX {
            return Err(VfsError::InvalidPath);
        }
        let mut buf = Vec::new();
        buf.try_reserve_exact(len)
            .map_err(|_| VfsError::OutOfMemory)?;
        buf.resize(len, 0);
        let read = self
            .fs
            .read_link(node, &mut buf)
            .map_err(map_driver_error)?;
        if read != len {
            // The driver disagreed with the length its own `node_info`
            // reported; refuse rather than resolve a partial target.
            return Err(VfsError::Io);
        }
        String::from_utf8(buf).map_err(|_| VfsError::InvalidPath)
    }

    /// The node `walk` reached, held for operations under this same hold of
    /// the driver's lock, and whether it is the projection's floor.
    ///
    /// # Errors
    ///
    /// [`VfsError::NotFound`] if the final name is vacant.
    pub(super) fn held(&mut self, walk: Walk) -> Result<(HeldNode<'_, R>, bool), VfsError> {
        let reached = walk.reached()?;
        let held = HeldNode::judged(
            &mut *self.fs,
            reached.node,
            reached.info,
            reached.meta,
            self.mount.class,
            self.mount.read_only,
        );
        Ok((held, reached.at_floor))
    }

    /// The stored target of the symbolic link `walk` reached, exactly as it
    /// was stored and still unresolved.
    ///
    /// # Errors
    ///
    /// * [`VfsError::InvalidPath`] if the walk reached something other than
    ///   a symbolic link, or one whose bytes are empty, over-long, or not
    ///   UTF-8.
    /// * [`VfsError::NotSupported`] if the format stores no links.
    /// * [`VfsError::NotFound`] or [`VfsError::Io`].
    pub(super) fn read_link_at(&mut self, walk: Walk) -> Result<String, VfsError> {
        let reached = walk.reached()?;
        if reached.info.kind != NodeKind::Symlink {
            return Err(VfsError::InvalidPath);
        }
        self.read_link_target(reached.node, reached.info.size)
    }

    /// Append up to `len` of the leading bytes of the regular file `walk`
    /// reached to `buf`.
    ///
    /// Nothing is read unless the node is a regular file of at most `max`
    /// bytes that `cred` may read. A refusal leaves `buf` as it was, any
    /// bytes already read into it zeroed first, since a boot file can hold
    /// credentials.
    ///
    /// # Errors
    ///
    /// The [`BoundedReadError`] naming the first check that refused.
    pub(super) fn read_bounded_at(
        &mut self,
        cred: &Credentials<'_>,
        walk: Walk,
        max: u64,
        len: usize,
        buf: &mut Vec<u8>,
    ) -> Result<(), BoundedReadError> {
        let reached = walk.reached()?;
        if reached.info.kind != NodeKind::RegularFile {
            return Err(BoundedReadError::NotAFile);
        }
        if reached.info.size > max {
            return Err(BoundedReadError::TooLarge);
        }
        reached.meta.authorize(cred, Access::Read)?;
        let size = usize::try_from(reached.info.size)
            .map_err(|_| BoundedReadError::TooLarge)?
            .min(len);
        let start = buf.len();
        buf.try_reserve_exact(size)
            .map_err(|_| BoundedReadError::OutOfMemory)?;
        buf.resize(start + size, 0);
        let refusal = match self.fs.read_at(reached.node, 0, &mut buf[start..]) {
            Ok(read) if read == size => return Ok(()),
            Ok(_) => BoundedReadError::ShortRead,
            Err(err) => BoundedReadError::Vfs(map_driver_error(err)),
        };
        buf[start..].zeroize();
        buf.truncate(start);
        Err(refusal)
    }

    /// The names from the projection's floor down to the directory `node`,
    /// each found by climbing with the driver's `parent` and looking for
    /// the child in its parent's listing, which `cred` must be able to read.
    ///
    /// # Errors
    ///
    /// [`VfsError::NotFound`] for a directory no name reaches any more, or a
    /// name gone from its parent's listing; [`VfsError::InvalidPath`] past
    /// [`MAX_PATH_COMPONENTS`] names; the permission refusals.
    pub(super) fn names_down_to(
        &mut self,
        cred: &Credentials<'_>,
        node: NodeId,
    ) -> Result<Vec<String>, VfsError> {
        let floor = self.projected_root(cred)?.node;
        if self.fs.node_info(node).map_err(map_driver_error)?.nlink == 0 {
            return Err(VfsError::NotFound);
        }
        let mut names = Vec::new();
        let mut here = node;
        while here != floor {
            if names.len() == MAX_PATH_COMPONENTS {
                return Err(VfsError::InvalidPath);
            }
            let up = self.fs.parent(here).map_err(map_driver_error)?;
            stored_record(self.fs, up)?.authorize(cred, Access::Read)?;
            let name = self.name_in(up, here)?;
            names.try_reserve(1).map_err(|_| VfsError::OutOfMemory)?;
            names.push(name);
            here = up;
        }
        names.reverse();
        Ok(names)
    }

    /// The name `child` has in the directory `dir`'s listing.
    fn name_in(&mut self, dir: NodeId, child: NodeId) -> Result<String, VfsError> {
        let mut found = None;
        self.fs
            .read_dir(dir, 0, &[], &mut |entry, name| {
                if entry.node != child {
                    return DirVisit::Take;
                }
                found = Some(
                    component_name(name)
                        .ok_or(VfsError::Io)
                        .and_then(owned_name),
                );
                DirVisit::Stop
            })
            .map_err(map_driver_error)?;
        found.unwrap_or(Err(VfsError::NotFound))
    }
}

impl<F: FilesystemRead + FilesystemSecurity + FilesystemWrite + ?Sized> DelegatedFs<'_, F> {
    /// Create an empty child of `kind` at the vacant place `walk` reached,
    /// asking `mode`.
    ///
    /// `walk` ends at the place the caller's own posture for a final link
    /// gives: `mkdir` keeps a final link, so making a directory over one is
    /// [`VfsError::AlreadyExists`], while `open` with `O_CREAT` follows it,
    /// so creating through a *dangling* link creates the file it names.
    ///
    /// # Errors
    ///
    /// * [`VfsError::AlreadyExists`] if a node already holds the name, or
    ///   the walk ended on a directory it stood on.
    /// * [`VfsError::NotFound`] for a missing ancestor, or a directory no
    ///   name reaches any more.
    /// * [`VfsError::PermissionDenied`] or [`VfsError::Io`].
    pub(super) fn create_at(
        &mut self,
        cred: &Credentials<'_>,
        walk: Walk,
        kind: NodeKind,
        mode: CreateMode,
    ) -> Result<(), VfsError> {
        let place = walk.for_creation()?.for_new_name(cred)?;
        self.create_in(cred, &place, kind, mode).map(|_| ())
    }

    /// Create an empty child of `kind` at the vacant `place`, which the
    /// caller may write, asking `mode`, returning the node it made.
    fn create_in(
        &mut self,
        cred: &Credentials<'_>,
        place: &Place,
        kind: NodeKind,
        mode: CreateMode,
    ) -> Result<NodeId, VfsError> {
        if place.found.is_some() {
            return Err(VfsError::AlreadyExists);
        }
        let record = creation_record(cred, &place.parent_meta, kind, mode);
        self.mutate(|fs| fs.create(place.parent, place.name.as_bytes(), kind, &record))
            .map_err(map_driver_error)
    }

    /// Open what `walk` reached as `flags` asks — refusing a node of the
    /// wrong kind or an access it withholds, creating a missing regular file
    /// asking `mode` under [`OpenFlags::CREATE`], or a directory under
    /// [`OpenFlags::CREATE`] with [`OpenFlags::DIRECTORY`], and truncating
    /// under [`OpenFlags::TRUNCATE`] — and pin what it names, all under the
    /// caller's one hold of the driver's lock. Answers the pin, and whether
    /// it is the projection's floor.
    ///
    /// The release slot is reserved on `nodes` before anything is made or
    /// pinned, and given back when the open is refused.
    ///
    /// # Errors
    ///
    /// * [`VfsError::InvalidPath`] for [`OpenFlags::CREATE`] with
    ///   [`OpenFlags::DIRECTORY`] but without [`OpenFlags::EXCLUSIVE`], or
    ///   asking to read or write the directory it makes.
    /// * [`VfsError::AlreadyExists`] for an existing node under
    ///   [`OpenFlags::EXCLUSIVE`] with [`OpenFlags::CREATE`].
    /// * [`VfsError::LinkLoop`] for byte access to a link
    ///   [`OpenFlags::NO_FOLLOW`] kept.
    /// * [`VfsError::NotADirectory`] for a non-directory under
    ///   [`OpenFlags::DIRECTORY`]; [`VfsError::IsADirectory`] for byte access
    ///   to a directory.
    /// * [`VfsError::ReadOnly`] for a create on a read-only projection.
    /// * [`VfsError::PermissionDenied`], [`VfsError::NotFound`],
    ///   [`VfsError::OutOfMemory`], or the driver's refusal to pin.
    pub(super) fn open_at(
        &mut self,
        cred: &Credentials<'_>,
        walk: Walk,
        flags: OpenFlags,
        nodes: &Arc<VolumeNodes>,
        mode: CreateMode,
    ) -> Result<(NodeRef, bool), VfsError> {
        // What an open can be refused for comes before the volume changes, and
        // the truncate comes last, so a refused open leaves the file as it was.
        let slot = nodes.admit()?;
        let makes_directory = flags.contains(OpenFlags::CREATE.union(OpenFlags::DIRECTORY));
        if makes_directory
            && (!flags.contains(OpenFlags::EXCLUSIVE) || flags.is_read() || flags.is_write())
        {
            return Err(VfsError::InvalidPath);
        }
        let (node, kind, at_floor, created) = match walk {
            Walk::Bottom {
                node,
                info,
                meta,
                at_floor,
            } => {
                Self::opened(cred, flags, &info, &meta)?;
                (node, info.kind, at_floor, None)
            }
            Walk::Leaf(Place {
                found: Some((node, info, meta)),
                ..
            }) => {
                Self::opened(cred, flags, &info, &meta)?;
                (node, info.kind, false, None)
            }
            Walk::Leaf(place) => {
                if !flags.contains(OpenFlags::CREATE) {
                    return Err(VfsError::NotFound);
                }
                if self.mount.read_only {
                    return Err(VfsError::ReadOnly);
                }
                let kind = if makes_directory {
                    NodeKind::Directory
                } else {
                    NodeKind::RegularFile
                };
                let place = place.for_new_name(cred)?;
                let node = self.create_in(cred, &place, kind, mode)?;
                (node, kind, false, Some(place))
            }
            Walk::Beyond => return Err(VfsError::NotFound),
        };
        let held = self
            .fs
            .pin(node)
            .map_err(map_driver_error)
            .and_then(|pinned| NodeRef::new(slot, pinned, kind));
        let held = match held {
            Ok(held) => held,
            Err(err) => {
                // The name this open made goes with it; one it could not take
                // back is an empty node the caller may remove.
                if let Some(place) = created {
                    let _ = self.mutate(|fs| fs.remove(place.parent, place.name.as_bytes()));
                }
                return Err(err);
            }
        };
        // `OpenFlags::from_bits` admits a truncate only with write access,
        // which `opened` asked for.
        if flags.contains(OpenFlags::TRUNCATE) {
            self.mutate(|fs| fs.truncate(held.pinned(), 0))
                .map_err(map_driver_error)?;
        }
        Ok((held, at_floor))
    }

    /// Whether an open asking `flags` may have the existing node `info` and
    /// `meta` describe: the right kind, and the access the descriptor will
    /// carry.
    fn opened(
        cred: &Credentials<'_>,
        flags: OpenFlags,
        info: &NodeInfo,
        meta: &Metadata,
    ) -> Result<(), VfsError> {
        if flags.contains(OpenFlags::CREATE) && flags.contains(OpenFlags::EXCLUSIVE) {
            return Err(VfsError::AlreadyExists);
        }
        let byte_access = flags.is_read() || flags.is_write();
        // A link stores a path, not bytes; the resolve-only handle is what
        // `lstat` and `readlink` hold. Only reachable under `NO_FOLLOW`.
        if info.kind == NodeKind::Symlink && byte_access {
            return Err(VfsError::LinkLoop);
        }
        if flags.contains(OpenFlags::DIRECTORY) {
            if info.kind != NodeKind::Directory {
                return Err(VfsError::NotADirectory);
            }
        } else if info.kind == NodeKind::Directory && byte_access {
            return Err(VfsError::IsADirectory);
        }
        if flags.is_read() {
            meta.authorize(cred, Access::Read)?;
        }
        if flags.is_write() {
            meta.authorize(cred, Access::Write)?;
        }
        Ok(())
    }

    /// Pin the directory `walk` reached as a working directory, which `cred`
    /// must be able to search. Answers the pin, and whether it is the
    /// projection's floor.
    ///
    /// # Errors
    ///
    /// [`VfsError::NotADirectory`], [`VfsError::PermissionDenied`],
    /// [`VfsError::NotFound`], [`VfsError::OutOfMemory`], or the driver's
    /// refusal to pin.
    pub(super) fn chdir_at(
        &mut self,
        cred: &Credentials<'_>,
        walk: Walk,
        nodes: &Arc<VolumeNodes>,
    ) -> Result<(NodeRef, bool), VfsError> {
        let slot = nodes.admit()?;
        let reached = walk.reached()?;
        if reached.info.kind != NodeKind::Directory {
            return Err(VfsError::NotADirectory);
        }
        reached.meta.authorize(cred, Access::Execute)?;
        let pinned = self.fs.pin(reached.node).map_err(map_driver_error)?;
        Ok((
            NodeRef::new(slot, pinned, NodeKind::Directory)?,
            reached.at_floor,
        ))
    }

    /// Create a symbolic link at the vacant place `walk` reached, whose
    /// stored target is `target`, already checked against the one
    /// link-target grammar.
    ///
    /// The target is **not** resolved: it is data, so the only authority the
    /// call needs is the right to create a name in the link's own parent, and
    /// the link may legitimately dangle.
    ///
    /// # Errors
    ///
    /// * [`VfsError::AlreadyExists`] if a node already holds the name.
    /// * [`VfsError::NotSupported`] if the format has no link object type —
    ///   it refuses rather than approximating one.
    /// * [`VfsError::PermissionDenied`], [`VfsError::NotFound`], or
    ///   [`VfsError::Io`].
    pub(super) fn create_link_at(
        &mut self,
        cred: &Credentials<'_>,
        walk: Walk,
        target: &str,
    ) -> Result<(), VfsError> {
        let place = walk.for_creation()?.for_new_name(cred)?;
        // The new name is the name as typed: making a link over an existing
        // one replaces nothing, it is refused.
        if place.found.is_some() {
            return Err(VfsError::AlreadyExists);
        }
        let record = creation_record(
            cred,
            &place.parent_meta,
            NodeKind::Symlink,
            CreateMode::LINK,
        );
        self.mutate(|fs| {
            fs.create_link(
                place.parent,
                place.name.as_bytes(),
                target.as_bytes(),
                &record,
            )
        })
        .map(|_| ())
        .map_err(map_driver_error)
    }

    /// Unlink the name `walk` reached, its final link kept: POSIX unlinks
    /// the link, so removing one never touches what it names.
    ///
    /// With `dir_only` the removal succeeds only when the name holds an
    /// (empty) directory — the atomic `rmdir` posture, decided under the same
    /// hold that removes the entry. With `expect` the name must still hold
    /// the node the caller read, decided under that hold too.
    ///
    /// # Errors
    ///
    /// * [`VfsError::InvalidPath`] for a node no directory of the walk holds.
    /// * [`VfsError::NotFound`] if the name is vacant.
    /// * [`VfsError::Stale`] if the node holding it is not the one expected.
    /// * [`VfsError::NotEmpty`] for a non-empty directory.
    /// * [`VfsError::NotADirectory`] if `dir_only` and it is not one.
    /// * [`VfsError::PermissionDenied`] — including when
    ///   [`Metadata::admits_name_removal`] refuses: a capability gate the
    ///   caller does not hold, or a sticky parent the caller owns no part of.
    pub(super) fn remove_at(
        &mut self,
        cred: &Credentials<'_>,
        walk: Walk,
        dir_only: bool,
        expect: Option<Expected<'_>>,
    ) -> Result<(), VfsError> {
        let place = walk.place()?.for_write(cred)?;
        let Some((node, info, meta)) = &place.found else {
            return Err(VfsError::NotFound);
        };
        meta.admits_name_removal(&place.parent_meta, cred)?;
        if expect.is_some_and(|expect| !expect.met_by(info)) {
            return Err(VfsError::Stale);
        }
        // Checked here so NotEmpty is answered without a driver round trip;
        // the driver still checks, and its refusal carries the same class.
        if info.kind == NodeKind::Directory {
            let mut occupied = false;
            self.fs
                .read_dir(*node, 0, &[], &mut |_, _| {
                    occupied = true;
                    DirVisit::Stop
                })
                .map_err(map_driver_error)?;
            if occupied {
                return Err(VfsError::NotEmpty);
            }
        } else if dir_only {
            return Err(VfsError::NotADirectory);
        }
        self.mutate(|fs| fs.remove(place.parent, place.name.as_bytes()))
            .map_err(map_driver_error)
    }

    /// The place `name` occupies in the directory `dir`, read afresh under
    /// this hold.
    fn place_in(&mut self, dir: NodeId, name: &str) -> Result<Place, VfsError> {
        let parent = self.walked(dir, None)?;
        if parent.info.kind != NodeKind::Directory {
            return Err(VfsError::NotADirectory);
        }
        let found = match self.fs.lookup(dir, name.as_bytes()) {
            Ok(node) => {
                let occupant = self.walked(node, None)?;
                Some((node, occupant.info, occupant.meta))
            }
            Err(DriverError::NotFound) => None,
            Err(err) => return Err(map_driver_error(err)),
        };
        Ok(Place {
            parent: dir,
            parent_info: parent.info,
            parent_meta: parent.meta,
            name: owned_name(name)?,
            found,
        })
    }

    /// Move `src_name` in the directory `src_dir` to `dst_name` in `dst_dir`,
    /// within this projection, preserving the node's identity and contents.
    /// Both directories were resolved before this hold and kept named since,
    /// so each name's place is read afresh here and nothing seen before the
    /// hold is trusted.
    ///
    /// Neither end follows a final symbolic link: POSIX renames the link
    /// itself and replaces a link at the destination, never what either
    /// names.
    ///
    /// Authorises search + write on both directories; when a directory is
    /// moved to a *different* parent its `..` link is rewritten, so write
    /// permission on the moved directory itself is additionally required
    /// (POSIX). Either end must pass [`Metadata::admits_name_removal`] — its
    /// capability gate and its parent's sticky bit — even within one parent.
    /// With `expect` the source name must still hold the node expected.
    ///
    /// Under [`RenameFlags::NO_REPLACE`] a destination naming any entry but
    /// the source's own refuses the move. The source's own entry is the same
    /// name in the same directory under the volume's matching rule, which on
    /// a folding volume takes in a re-spelling of it.
    ///
    /// # Errors
    ///
    /// * [`VfsError::NotFound`] if the source does not exist, or the
    ///   destination directory has no name left.
    /// * [`VfsError::Stale`] if the source is not the node expected.
    /// * [`VfsError::AlreadyExists`] if the destination is occupied under
    ///   [`RenameFlags::NO_REPLACE`].
    /// * [`VfsError::NotEmpty`] if the destination is a non-empty directory.
    /// * [`VfsError::DirectoryCycle`] if the move would place a directory
    ///   inside its own subtree.
    /// * [`VfsError::NotADirectory`] if a directory would replace a
    ///   non-directory, and [`VfsError::IsADirectory`] if a non-directory
    ///   would replace a directory.
    /// * [`VfsError::PermissionDenied`] or [`VfsError::Io`].
    // Two places, each a directory and a name, and what the move may do.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn rename_places(
        &mut self,
        cred: &Credentials<'_>,
        src_dir: NodeId,
        src_name: &str,
        dst_dir: NodeId,
        dst_name: &str,
        flags: RenameFlags,
        expect: Option<Expected<'_>>,
    ) -> Result<(), VfsError> {
        let src = self.place_in(src_dir, src_name)?.for_write(cred)?;
        let dst = self.place_in(dst_dir, dst_name)?.for_new_name(cred)?;
        let Some((src_node, src_info, src_meta)) = &src.found else {
            return Err(VfsError::NotFound);
        };
        // The source loses its name and a replaced destination is destroyed.
        src_meta.admits_name_removal(&src.parent_meta, cred)?;
        if let Some((_, _, dst_meta)) = &dst.found {
            dst_meta.admits_name_removal(&dst.parent_meta, cred)?;
        }
        if expect.is_some_and(|expect| !expect.met_by(src_info)) {
            return Err(VfsError::Stale);
        }
        let onto_itself = src.parent == dst.parent
            && self
                .fs
                .name_matching()
                .matches(src.name.as_bytes(), dst.name.as_bytes());
        if flags.refuses_replace() && dst.found.is_some() && !onto_itself {
            return Err(VfsError::AlreadyExists);
        }
        let moving_dir = src_info.kind == NodeKind::Directory;
        if let Some((dst_node, dst_info, _)) = &dst.found {
            if dst_node != src_node && moving_dir != (dst_info.kind == NodeKind::Directory) {
                return Err(if moving_dir {
                    VfsError::NotADirectory
                } else {
                    VfsError::IsADirectory
                });
            }
        }
        // A directory moved to a different parent has its `..` rewritten, so
        // write permission on the directory itself is required as well.
        if src.parent != dst.parent && moving_dir {
            src_meta.authorize(cred, Access::Write)?;
        }
        self.mutate(|fs| {
            fs.rename(
                src.parent,
                src.name.as_bytes(),
                dst.parent,
                dst.name.as_bytes(),
            )
        })
        .map_err(map_driver_error)
    }

    /// Give `node` a second name, `name` in the directory `dir` — a hard
    /// link — both resolved before this hold and kept named since.
    ///
    /// A **directory** is refused here rather than only in each driver: the
    /// tree staying a tree is what makes the physical `..` walk well-defined.
    /// The new name is authorised exactly as a create in its own parent, and
    /// nothing further is required of the caller against the node: a second
    /// name confers no authority the first did not.
    ///
    /// # Errors
    ///
    /// * [`VfsError::IsADirectory`] for a directory.
    /// * [`VfsError::NotFound`] for a node or a directory no name reaches
    ///   any more.
    /// * [`VfsError::AlreadyExists`] if `name` already names something.
    /// * [`VfsError::TooManyLinks`] if the format's per-node name count
    ///   would overflow.
    /// * [`VfsError::NotSupported`] if the format holds one name per node.
    /// * [`VfsError::PermissionDenied`] or [`VfsError::Io`].
    pub(super) fn link_places(
        &mut self,
        cred: &Credentials<'_>,
        node: NodeId,
        dir: NodeId,
        name: &str,
    ) -> Result<(), VfsError> {
        let info = self.fs.node_info(node).map_err(map_driver_error)?;
        match info.kind {
            NodeKind::RegularFile | NodeKind::Symlink => {}
            NodeKind::Directory => return Err(VfsError::IsADirectory),
        }
        if info.nlink == 0 {
            return Err(VfsError::NotFound);
        }
        let place = self.place_in(dir, name)?.for_new_name(cred)?;
        if place.found.is_some() {
            return Err(VfsError::AlreadyExists);
        }
        self.mutate(|fs| fs.link(place.parent, place.name.as_bytes(), node))
            .map_err(map_driver_error)
    }
}

/// A node already reached — pinned by a descriptor, or resolved under the
/// same hold of the driver's lock — judged against the records it was read
/// with. No path stands between it and its caller, so nothing above it is
/// searched.
pub struct HeldNode<'fs, R: ?Sized> {
    fs: &'fs mut R,
    node: NodeId,
    info: NodeInfo,
    meta: Metadata,
    class: AllocationClass,
    read_only: bool,
}

impl<'fs, R: FilesystemRead + FilesystemSecurity + ?Sized> HeldNode<'fs, R> {
    /// `node`, its records read from `fs`, shown through `projection`.
    ///
    /// # Errors
    ///
    /// The driver's refusal to report the node, which no name or hold
    /// reaches as [`VfsError::NotFound`].
    pub fn of(fs: &'fs mut R, node: NodeId, projection: &Projection) -> Result<Self, VfsError> {
        let info = fs.node_info(node).map_err(map_driver_error)?;
        let meta = stored_record(fs, node)?;
        Ok(Self::judged(
            fs,
            node,
            info,
            meta,
            projection.class(),
            projection.is_read_only(),
        ))
    }
}

impl<'fs, R: FilesystemRead + ?Sized> HeldNode<'fs, R> {
    fn judged(
        fs: &'fs mut R,
        node: NodeId,
        info: NodeInfo,
        meta: Metadata,
        class: AllocationClass,
        read_only: bool,
    ) -> Self {
        Self {
            fs,
            node,
            info,
            meta,
            class,
            read_only,
        }
    }

    /// The node's structural metadata and the permission metadata governing
    /// it.
    #[must_use]
    pub fn into_info(self) -> DelegatedInfo {
        DelegatedInfo::of(&self.info, self.meta)
    }

    /// The node's kind.
    #[must_use]
    pub fn kind(&self) -> NodeKind {
        self.info.kind
    }

    /// The node's number as `stat` reports it ([`NodeInfo::ino`]).
    #[must_use]
    pub fn ino(&self) -> u64 {
        self.info.ino
    }

    /// Whether no name reaches the node any more.
    #[must_use]
    pub fn is_removed(&self) -> bool {
        self.info.nlink == 0
    }

    /// Whether `cred` may have `want` access to the node through a
    /// descriptor, as a byte operation of that access asks. `owner_exempt`
    /// lets the owner pass the mode ([`Metadata::authorize_held`]).
    ///
    /// # Errors
    ///
    /// [`VfsError::PermissionDenied`].
    pub fn admits(
        &self,
        cred: &Credentials<'_>,
        want: Access,
        owner_exempt: bool,
    ) -> Result<(), VfsError> {
        self.meta.authorize_held(cred, want, owner_exempt)
    }

    /// Read up to `buf.len()` bytes at `offset`, returning the number read
    /// (short of `buf.len()` at end-of-file). `owner_exempt` lets the owner
    /// pass the mode ([`Metadata::authorize_held`]).
    ///
    /// # Errors
    ///
    /// [`VfsError::IsADirectory`] for a directory, [`VfsError::LinkLoop`] for
    /// a link, [`VfsError::PermissionDenied`], or the driver's refusal.
    pub fn read(
        &mut self,
        cred: &Credentials<'_>,
        owner_exempt: bool,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, VfsError> {
        deny_non_file(self.info.kind)?;
        self.meta.authorize_held(cred, Access::Read, owner_exempt)?;
        self.fs
            .read_at(self.node, offset, buf)
            .map_err(map_driver_error)
    }

    /// Where the next region of `region`'s kind begins at or after `offset`,
    /// or `None` when none does. Needs read permission: where the holes lie
    /// is part of what reading the file reveals.
    ///
    /// # Errors
    ///
    /// As [`Self::read`].
    pub fn seek_region(
        &mut self,
        cred: &Credentials<'_>,
        owner_exempt: bool,
        offset: u64,
        region: SeekRegion,
    ) -> Result<Option<u64>, VfsError> {
        deny_non_file(self.info.kind)?;
        self.meta.authorize_held(cred, Access::Read, owner_exempt)?;
        self.fs
            .seek_region(self.node, offset, region)
            .map_err(map_driver_error)
    }

    /// Hand `each` the directory's entries from `at` on, in the driver's
    /// stable on-disk order, moving `at` past each one it takes, until it
    /// answers [`DirVisit::Stop`] or the directory ends; a directory no name
    /// reaches any more has none. Answers how the batch ended.
    ///
    /// Each entry carries the structural [`NodeInfo`] the listing driver
    /// read, so a caller never re-resolves a child by path — a child whose
    /// *path* another projection shadows would otherwise be judged against
    /// the wrong volume.
    ///
    /// # Errors
    ///
    /// * [`VfsError::NotADirectory`] if the node is not a directory.
    /// * [`VfsError::PermissionDenied`] if its metadata denies read.
    /// * [`VfsError::Io`] for an entry whose name is empty, over-long or not
    ///   UTF-8, for a driver cursor that fails to advance, and for a device
    ///   fault.
    pub fn list_from(
        &mut self,
        cred: &Credentials<'_>,
        owner_exempt: bool,
        at: &mut DirPosition,
        each: &mut dyn FnMut(&DelegatedRef<'_>) -> DirVisit,
    ) -> Result<ListEnd, VfsError> {
        self.listable(cred, owner_exempt)?;
        let start = at.cursor;
        let resume = at.after.clone();
        let mut end = ListEnd::Exhausted;
        let mut took = false;
        let mut failed = None;
        self.fs
            .read_dir(self.node, start, resume.get(), &mut |entry, name| {
                // A malformed name fails the batch closed rather than being
                // listed around.
                let Some(name) = component_name(name) else {
                    failed = Some(VfsError::Io);
                    return DirVisit::Stop;
                };
                let listed = DelegatedRef {
                    info: entry.info,
                    name,
                };
                if each(&listed) == DirVisit::Stop {
                    end = ListEnd::Stopped;
                    return DirVisit::Stop;
                }
                took = true;
                at.cursor = entry.next_cursor;
                if let Err(err) = at.after.set(name.as_bytes()) {
                    failed = Some(err);
                    return DirVisit::Stop;
                }
                DirVisit::Take
            })
            .map_err(map_driver_error)?;
        if let Some(err) = failed {
            return Err(err);
        }
        // A cursor that does not move would hand the next batch the same
        // entries for ever.
        if took && at.cursor == start {
            return Err(VfsError::Io);
        }
        Ok(end)
    }

    /// For each of `names`, the entry [`list_from`](Self::list_from) would
    /// hand over for it, or [`None`] where the directory holds no such name —
    /// under the same authorisation, with one lookup per name rather than a
    /// read of every entry. With no names it is the listing's authorisation
    /// alone.
    ///
    /// # Errors
    ///
    /// As [`list_from`](Self::list_from), and [`VfsError::NotFound`] for a
    /// directory no name reaches any more.
    pub fn lookup_entries(
        &mut self,
        cred: &Credentials<'_>,
        owner_exempt: bool,
        names: &[&[u8]],
    ) -> Result<Vec<Option<DelegatedEntry>>, VfsError> {
        self.listable(cred, owner_exempt)?;
        if self.is_removed() {
            return Err(VfsError::NotFound);
        }
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(names.len())
            .map_err(|_| VfsError::OutOfMemory)?;
        for &name in names {
            let entry = match self.fs.lookup(self.node, name) {
                Ok(node) => {
                    let info = self.fs.node_info(node).map_err(map_driver_error)?;
                    let name = core::str::from_utf8(name).map_err(|_| VfsError::Io)?;
                    Some(DelegatedEntry {
                        info,
                        name: owned_name(name)?,
                    })
                }
                Err(DriverError::NotFound) => None,
                Err(err) => return Err(map_driver_error(err)),
            };
            entries.push(entry);
        }
        Ok(entries)
    }

    /// Whether `cred` may list this directory: the one check
    /// [`list_from`](Self::list_from) and
    /// [`lookup_entries`](Self::lookup_entries) share.
    fn listable(&self, cred: &Credentials<'_>, owner_exempt: bool) -> Result<(), VfsError> {
        if self.info.kind != NodeKind::Directory {
            return Err(VfsError::NotADirectory);
        }
        self.meta.authorize_held(cred, Access::Read, owner_exempt)
    }

    /// Refuse a change of the node's own record on a read-only projection,
    /// through a grant — whose holder could not restore what it changed — or
    /// past the node's capability gate.
    fn changeable(&self, cred: &Credentials<'_>, granted: bool) -> Result<(), VfsError> {
        if self.read_only {
            return Err(VfsError::ReadOnly);
        }
        if granted {
            return Err(VfsError::PermissionDenied);
        }
        match self.meta.required_cap {
            Some(cap) if !cred.caps.holds(cap) => Err(VfsError::PermissionDenied),
            _ => Ok(()),
        }
    }
}

impl<R: FilesystemRead + FilesystemWrite + ?Sized> HeldNode<'_, R> {
    /// Write `data` at `offset`, or at the end of the file when `append`,
    /// returning the number of bytes written.
    ///
    /// # Errors
    ///
    /// [`VfsError::ReadOnly`] on a read-only projection,
    /// [`VfsError::IsADirectory`] for a directory, [`VfsError::LinkLoop`] for
    /// a link, [`VfsError::PermissionDenied`], [`VfsError::NoSpace`] on a
    /// full volume, or the driver's other refusals.
    pub fn write(
        &mut self,
        cred: &Credentials<'_>,
        owner_exempt: bool,
        offset: u64,
        append: bool,
        data: &[u8],
    ) -> Result<usize, VfsError> {
        self.writable(cred, owner_exempt)?;
        // Read under this same hold, so the end cannot move before the write.
        let offset = if append { self.info.size } else { offset };
        let node = self.node;
        with_class(self.fs, self.class, |fs| fs.write_at(node, offset, data))
            .map_err(map_driver_error)
    }

    /// Set the file's length to `size`.
    ///
    /// # Errors
    ///
    /// As [`Self::write`].
    pub fn truncate(
        &mut self,
        cred: &Credentials<'_>,
        owner_exempt: bool,
        size: u64,
    ) -> Result<(), VfsError> {
        self.writable(cred, owner_exempt)?;
        let node = self.node;
        with_class(self.fs, self.class, |fs| fs.truncate(node, size)).map_err(map_driver_error)
    }

    /// Punch a hole of `len` bytes at `offset`: the range reads as zeros and
    /// its storage is released. Gated as a write of the same bytes.
    ///
    /// # Errors
    ///
    /// [`VfsError::NotSupported`] where the format keeps no holes, else as
    /// [`Self::write`].
    pub fn punch(
        &mut self,
        cred: &Credentials<'_>,
        owner_exempt: bool,
        offset: u64,
        len: u64,
    ) -> Result<(), VfsError> {
        self.writable(cred, owner_exempt)?;
        let node = self.node;
        with_class(self.fs, self.class, |fs| fs.punch(node, offset, len)).map_err(map_driver_error)
    }

    /// Whether `cred` may write the file's bytes. They are the file's to
    /// grant, as reading them is: write permission on its directory governs
    /// its name, never its contents.
    fn writable(&self, cred: &Credentials<'_>, owner_exempt: bool) -> Result<(), VfsError> {
        if self.read_only {
            return Err(VfsError::ReadOnly);
        }
        deny_non_file(self.info.kind)?;
        self.meta.authorize_held(cred, Access::Write, owner_exempt)
    }
}

impl<R: FilesystemRead + FilesystemSecurity + FilesystemWrite + ?Sized> HeldNode<'_, R> {
    /// Set the node's permission bits to `mode` (the `chmod(2)` shape),
    /// leaving its ownership, ACL, and capability gate untouched.
    ///
    /// Only the node's **owner** may change its mode — mode bits grant no
    /// write-implies-chmod, and holding a capability grants no override —
    /// and never through a grant, whose holder could not restore what it
    /// changed. A node carrying a `required_cap` gate additionally demands
    /// that capability. `mode` must already be validated to the permission
    /// mask by the caller.
    ///
    /// # Errors
    ///
    /// [`VfsError::ReadOnly`] on a read-only projection,
    /// [`VfsError::PermissionDenied`], [`VfsError::NotSupported`] where the
    /// format stores no record of the node's own, or [`VfsError::Io`].
    pub fn set_mode(
        &mut self,
        cred: &Credentials<'_>,
        granted: bool,
        mode: u32,
    ) -> Result<(), VfsError> {
        self.changeable(cred, granted)?;
        if cred.uid != self.meta.owner {
            return Err(VfsError::PermissionDenied);
        }
        let mut sec = self.fs.security(self.node).map_err(map_driver_error)?;
        sec.mode = mode;
        let node = self.node;
        with_class(self.fs, self.class, |fs| fs.set_security(node, sec)).map_err(map_driver_error)
    }

    /// Set the owning user and/or group of the node (the `chown(2)` /
    /// `chgrp(2)` shape), leaving its mode's permission triads, ACL, and
    /// capability gate otherwise untouched.
    ///
    /// `uid` / `gid` of [`FS_OWNER_UNCHANGED`] leave that field as it is; a
    /// call changing neither touches no state. Reassigning the **uid**, or
    /// setting a **gid** the caller is not a member of, requires
    /// [`CapabilityId::FS_CHOWN`]; without it only the node's owner may change
    /// the group, and only to a group the caller already belongs to. The
    /// node's capability gate is honoured first, and nothing changes through
    /// a grant.
    ///
    /// On any actual change a file loses its set-user-ID bit, and its
    /// set-group-ID bit when its group may run it, so a reassigned file never
    /// carries a stale set-*id* escalation; a directory keeps both, its
    /// set-group-ID bit being what gives new entries its group.
    ///
    /// # Errors
    ///
    /// [`VfsError::ReadOnly`] on a read-only projection,
    /// [`VfsError::PermissionDenied`], [`VfsError::NotSupported`] where the
    /// format stores no record of the node's own, or [`VfsError::Io`].
    pub fn set_owner(
        &mut self,
        cred: &Credentials<'_>,
        granted: bool,
        uid: u32,
        gid: u32,
    ) -> Result<(), VfsError> {
        self.changeable(cred, granted)?;
        let mut sec = self.fs.security(self.node).map_err(map_driver_error)?;
        let owner = if uid == FS_OWNER_UNCHANGED {
            sec.uid
        } else {
            uid
        };
        let group = if gid == FS_OWNER_UNCHANGED {
            sec.gid
        } else {
            gid
        };
        let owner_changed = owner != sec.uid;
        let group_changed = group != sec.gid;
        if !owner_changed && !group_changed {
            return Ok(());
        }
        if !cred.caps.holds(CapabilityId::FS_CHOWN) {
            // Reassigning the owner is always privileged.
            if owner_changed {
                return Err(VfsError::PermissionDenied);
            }
            // Group change is the owner's, and only to a group they belong to.
            if group_changed && !(UserId(sec.uid) == cred.uid && cred.is_in_group(GroupId(group))) {
                return Err(VfsError::PermissionDenied);
            }
        }
        sec.uid = owner;
        sec.gid = group;
        sec.mode = mode_after_owner_change(sec.mode, self.info.kind == NodeKind::Directory);
        let node = self.node;
        with_class(self.fs, self.class, |fs| fs.set_security(node, sec)).map_err(map_driver_error)
    }
}

impl<R: FilesystemRead + FilesystemAttrs + ?Sized> HeldNode<'_, R> {
    /// Read the extended attribute `key` into `value_out`, returning the
    /// value's byte count.
    ///
    /// The key is validated against the one shared `lib/fsmeta` grammar and
    /// its namespace's access class decides the gate: the ordinary
    /// namespaces need read permission on the node, judged as a read through
    /// the descriptor is, while the privileged namespaces (`system`,
    /// `trusted`) are refused outright until the service that holds their
    /// capability exists.
    ///
    /// # Errors
    ///
    /// * [`VfsError::InvalidKey`] for a key outside the grammar.
    /// * [`VfsError::NoData`] if the node carries no such attribute (a
    ///   value may legitimately be empty, so absence is never an empty
    ///   read).
    /// * [`VfsError::BufferTooSmall`] if the value does not fit
    ///   `value_out` (never truncated).
    /// * [`VfsError::PermissionDenied`] or [`VfsError::Io`].
    pub fn get_attr(
        &mut self,
        cred: &Credentials<'_>,
        owner_exempt: bool,
        key: &[u8],
        value_out: &mut [u8],
    ) -> Result<usize, VfsError> {
        let key = parse_unprivileged_key(key)?;
        self.meta.authorize_held(cred, Access::Read, owner_exempt)?;
        match self
            .fs
            .get_attr(self.node, key.as_bytes(), value_out)
            .map_err(map_attr_driver_error)?
        {
            Some(len) => Ok(len),
            None => Err(VfsError::NoData),
        }
    }

    /// Yield the `index`-th *visible* extended-attribute key into `key_out`,
    /// returning its byte count, or `None` once `index` is past the last
    /// visible attribute.
    ///
    /// Needs read permission on the node. Keys in a privileged namespace are
    /// omitted from the enumeration entirely — `index` addresses the filtered
    /// sequence, so a caller can never learn a `system.*` key exists, not
    /// even as a gap. Enumeration scans through a fixed [`KEY_MAX`] scratch,
    /// so an over-long `key_out` refusal can only name the key actually
    /// selected, never one that was skipped.
    ///
    /// # Errors
    ///
    /// * [`VfsError::BufferTooSmall`] if the selected key does not fit
    ///   `key_out`.
    /// * [`VfsError::PermissionDenied`] or [`VfsError::Io`].
    pub fn list_attr(
        &mut self,
        cred: &Credentials<'_>,
        owner_exempt: bool,
        index: u64,
        key_out: &mut [u8],
    ) -> Result<Option<usize>, VfsError> {
        self.meta.authorize_held(cred, Access::Read, owner_exempt)?;
        let mut scratch = [0u8; KEY_MAX];
        let mut visible = 0u64;
        let mut raw = 0u64;
        loop {
            let Some(len) = self
                .fs
                .list_attr(self.node, raw, &mut scratch)
                .map_err(map_attr_driver_error)?
            else {
                return Ok(None);
            };
            raw += 1;
            // A stored key that fails the shared grammar cannot be judged,
            // so it is hidden like a privileged one (fail closed) — a
            // conforming driver never stores such a key.
            let readable = AttrKey::parse(&scratch[..len])
                .is_ok_and(|k| k.access() != NamespaceAccess::Privileged);
            if !readable {
                continue;
            }
            if visible == index {
                let Some(out) = key_out.get_mut(..len) else {
                    return Err(VfsError::BufferTooSmall);
                };
                out.copy_from_slice(&scratch[..len]);
                return Ok(Some(len));
            }
            visible += 1;
        }
    }
}

impl<R: FilesystemRead + FilesystemAttrs + FilesystemWrite + ?Sized> HeldNode<'_, R> {
    /// Set (insert or replace) the extended attribute `key` to `value`, in
    /// one copy-on-write driver transaction.
    ///
    /// Gated exactly as [`Self::get_attr`], with write permission in place of
    /// read, on a writable projection. The value is opaque; the driver
    /// enforces the fixed per-inode bounds and fails closed at them.
    ///
    /// # Errors
    ///
    /// * [`VfsError::ReadOnly`] on a read-only projection.
    /// * [`VfsError::InvalidKey`] for a key outside the grammar.
    /// * [`VfsError::NoSpace`] at the per-inode attribute bounds or a full
    ///   volume.
    /// * [`VfsError::NotSupported`] if the format has nowhere to keep the
    ///   key's namespace.
    /// * [`VfsError::PermissionDenied`] or [`VfsError::Io`].
    pub fn set_attr(
        &mut self,
        cred: &Credentials<'_>,
        owner_exempt: bool,
        key: &[u8],
        value: &[u8],
    ) -> Result<(), VfsError> {
        if self.read_only {
            return Err(VfsError::ReadOnly);
        }
        let key = parse_unprivileged_key(key)?;
        self.meta
            .authorize_held(cred, Access::Write, owner_exempt)?;
        let node = self.node;
        with_class(self.fs, self.class, |fs| {
            fs.set_attr(node, key.as_bytes(), value)
        })
        .map_err(map_attr_driver_error)
    }

    /// Remove the extended attribute `key`, in one copy-on-write driver
    /// transaction, gated exactly as [`Self::set_attr`].
    ///
    /// # Errors
    ///
    /// * [`VfsError::ReadOnly`] on a read-only projection.
    /// * [`VfsError::InvalidKey`] for a key outside the grammar.
    /// * [`VfsError::NoData`] if the node carries no such attribute.
    /// * [`VfsError::PermissionDenied`] or [`VfsError::Io`].
    pub fn remove_attr(
        &mut self,
        cred: &Credentials<'_>,
        owner_exempt: bool,
        key: &[u8],
    ) -> Result<(), VfsError> {
        if self.read_only {
            return Err(VfsError::ReadOnly);
        }
        let key = parse_unprivileged_key(key)?;
        self.meta
            .authorize_held(cred, Access::Write, owner_exempt)?;
        let node = self.node;
        with_class(self.fs, self.class, |fs| {
            fs.remove_attr(node, key.as_bytes())
        })
        .map_err(|error| match error {
            DriverError::NotFound => VfsError::NoData,
            other => map_attr_driver_error(other),
        })
    }
}

/// Refuse a node whose bytes a read or write may not touch.
///
/// A directory's entries are not a byte stream, and a link's content is a
/// path — following it was the resolution's job, so a link reaching here
/// would mean an operation about to treat one as a file.
fn deny_non_file(kind: NodeKind) -> Result<(), VfsError> {
    match kind {
        NodeKind::RegularFile => Ok(()),
        NodeKind::Directory => Err(VfsError::IsADirectory),
        NodeKind::Symlink => Err(VfsError::LinkLoop),
    }
}

#[cfg(test)]
#[path = "delegate_tests.rs"]
mod tests;
