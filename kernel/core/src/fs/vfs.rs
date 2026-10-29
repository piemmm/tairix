//! The [`Vfs`]: the mount table, the in-RAM tree of the boot layout, and the
//! one resolver every path operation walks through.
//!
//! A caller's path is resolved from the view root, its working directory, or
//! a directory it holds, across every projection it crosses: a projection no
//! volume backs is walked here, a backed one by its driver
//! ([`DelegatedFs::walk_segment`]) under one hold of that driver's lock. An
//! operation then acts on what the walk reached under the hold of the driver
//! it ended on, so nothing between the walk's last step and the change can
//! move the name it acts on.
//!
//! Each in-RAM [`Node`] owns its children directly (a directory is a
//! `BTreeMap<String, Node>`), so its own operations are a borrow walk and
//! removal simply drops a subtree. It holds the boot layout's directories,
//! whose records govern the projections no volume backs.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{
    AllocationClass, DirVisit, FilesystemRead, FilesystemSecurity, FilesystemWrite, MountFlags,
    NameMatching, NodeId, NodeKind as DriverNodeKind,
};
use tairix_abi::driver::DriverHandle;
use tairix_abi::fs::{RealpathMode, RenameFlags, FS_PATH_MAX};
use tairix_abi::{CapabilityId, FileId, ROOT_VIEW, STORAGE_CATALOG};
use tairix_kernel_sec::{GroupId, UserId};
use tairix_sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use super::delegate::{
    Begin, DelegatedFs, DelegatedRef, Descent, Edge, Expect, FinalLink, Links, MissingSteps, Road,
    Walk,
};
use super::listing::{DirPosition, ListEnd};
use super::mount::{Cover, MountTable, Projection};
use super::node::{At, NodeHandle, NodeRef, Start};
use super::path::{
    owned_name, parse_caller_path, spell, CallerPath, Path, Step, TargetStep, MAX_PATH_COMPONENTS,
};
use super::perm::{Access, Credentials, Metadata, Mode};
use super::{BoundedReadError, VfsError};

/// A node's payload: a directory (name → child) or a file (bytes).
#[derive(Clone, Debug, Eq, PartialEq)]
enum NodeKind {
    Directory(BTreeMap<String, Node>),
    File(Vec<u8>),
}

/// A single inode: its access-control [`Metadata`] and its payload.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Node {
    meta: Metadata,
    kind: NodeKind,
}

impl Node {
    fn directory(meta: Metadata) -> Self {
        Self {
            meta,
            kind: NodeKind::Directory(BTreeMap::new()),
        }
    }

    fn file(meta: Metadata, contents: Vec<u8>) -> Self {
        Self {
            meta,
            kind: NodeKind::File(contents),
        }
    }
}

/// What an operation does with what a projection shows, which the
/// projection's flags may refuse.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum MountUse {
    /// Read or inspect, which every projection allows.
    Read,
    /// Change, which a read-only projection refuses.
    Write,
    /// Read as a program to run, which a `noexec` projection refuses.
    Run,
}

impl MountUse {
    /// Refuse this use where `projection`'s flags forbid it.
    ///
    /// # Errors
    ///
    /// [`VfsError::ReadOnly`] for a change on a read-only projection, and
    /// [`VfsError::PermissionDenied`] for a program read on a `noexec` one.
    pub(super) fn admitted_by(self, projection: &Projection) -> Result<(), VfsError> {
        match self {
            Self::Write if projection.is_read_only() => Err(VfsError::ReadOnly),
            Self::Run if projection.flags().contains(MountFlags::NOEXEC) => {
                Err(VfsError::PermissionDenied)
            }
            Self::Read | Self::Write | Self::Run => Ok(()),
        }
    }
}

/// How a resolution treats the links and the missing names it meets.
#[derive(Copy, Clone, Debug)]
pub(super) struct Posture {
    /// Whether a final symbolic link is followed.
    pub(super) final_link: FinalLink,
    /// Whether a missing ancestor ends the walk.
    pub(super) missing: MissingSteps,
    /// Whether a link is followed at all.
    pub(super) links: Links,
}

impl Posture {
    /// Every link followed but a final one `final_link` keeps, and a missing
    /// ancestor refused: what every operation but a canonicalisation asks.
    pub(super) const fn of(final_link: FinalLink) -> Self {
        Self {
            final_link,
            missing: MissingSteps::Refused,
            links: Links::Follow,
        }
    }
}

/// What a resolution keeps of the view path it walks.
pub(super) enum Spelling {
    /// Nothing.
    Unkept,
    /// The view path; a held start is spelled as it was opened, a label.
    Label,
    /// The view path; a held start is spelled `start`, its canonical path.
    Exact(Vec<String>),
}

/// Where a resolution stands between one projection and the next.
enum Stand<'s> {
    /// The floor of a projection.
    Floor(Arc<Projection>),
    /// A directory a descriptor holds, where a relative path starts.
    Held(&'s NodeHandle),
    /// The directory holding the one a bind covers, reached by a caller's
    /// `..` from the bind's floor, which the bind keeps pinned.
    Holder {
        /// The projection it is reached through.
        projection: Arc<Projection>,
        /// The directory.
        dir: NodeRef,
    },
}

/// A caller's path being resolved, across every projection it crosses.
pub(super) struct Resolution<'s> {
    stand: Stand<'s>,
    road: Road,
    /// Whether the start's search permission is yet to be checked.
    check_start: bool,
}

/// What a resolution asks for next.
enum Next {
    /// The driver serving the projection it stands in.
    Volume(Serve),
    /// Nothing: it ended in a projection no volume backs.
    Catalog(CatalogEnd),
}

/// The driver a resolution asks to hold: the one serving `projection`, under
/// the registration `pin` was taken on when the walk stands on a pinned
/// node.
pub(super) struct Serve {
    /// The projection.
    pub(super) projection: Arc<Projection>,
    /// The pin the walk stands on, whose registration must still serve.
    pub(super) pin: Option<NodeRef>,
}

impl<'s> Resolution<'s> {
    /// Resolve `at`, from the start it names when its path is relative,
    /// `posture` deciding how links and missing names are treated and
    /// `spelling` what is kept of the path.
    ///
    /// # Errors
    ///
    /// [`VfsError::InvalidPath`] for a path the caller grammar refuses,
    /// [`VfsError::Stale`] for a catalog start that left the mount table, and
    /// [`VfsError::OutOfMemory`].
    pub(super) fn new(
        vfs: &Vfs,
        at: At<'s>,
        posture: Posture,
        spelling: Spelling,
    ) -> Result<Self, VfsError> {
        Self::begin(
            vfs,
            parse_caller_path(at.path)?,
            at.start,
            posture,
            spelling,
        )
    }

    /// Resolve the absolute `path`, as a reader holding one driver asks.
    ///
    /// # Errors
    ///
    /// As [`Self::new`].
    pub(super) fn of_path(
        vfs: &Vfs,
        path: &Path,
        posture: Posture,
        spelling: Spelling,
    ) -> Result<Self, VfsError> {
        let mut steps = VecDeque::new();
        steps
            .try_reserve_exact(path.depth())
            .map_err(|_| VfsError::OutOfMemory)?;
        for name in path.components() {
            steps.push_back(Step {
                kind: TargetStep::Name(owned_name(name)?),
                from_link: false,
            });
        }
        let path = CallerPath {
            absolute: true,
            steps,
        };
        Self::begin(vfs, path, Start::Root, posture, spelling)
    }

    fn begin(
        vfs: &Vfs,
        path: CallerPath,
        start: Start<'s>,
        posture: Posture,
        spelling: Spelling,
    ) -> Result<Self, VfsError> {
        let floor = |projection: Arc<Projection>, spelling: Spelling| {
            let spelled = match spelling {
                Spelling::Unkept => None,
                Spelling::Label | Spelling::Exact(_) => Some(projection.path().owned_components()?),
            };
            Ok::<_, VfsError>((Stand::Floor(projection), spelled))
        };
        let ((stand, spelled), check_start) = if path.absolute {
            (floor(vfs.root_projection()?, spelling)?, false)
        } else {
            match start {
                Start::Root => (floor(vfs.root_projection()?, spelling)?, true),
                Start::Dir(handle) => {
                    let spelled = match spelling {
                        Spelling::Unkept => None,
                        Spelling::Label => Some(match Path::parse(&handle.opened_as) {
                            Ok(label) => label.into_components(),
                            Err(_) => handle.projection.path().owned_components()?,
                        }),
                        Spelling::Exact(start) => Some(start),
                    };
                    ((Stand::Held(handle), spelled), true)
                }
                Start::Catalog(catalog) => {
                    let live = vfs.mounts().live(catalog).cloned();
                    (floor(live.ok_or(VfsError::Stale)?, spelling)?, true)
                }
            }
        };
        let road = Road::new(
            path.steps,
            posture.final_link,
            posture.missing,
            posture.links,
            spelled,
        );
        Ok(Self {
            stand,
            road,
            check_start,
        })
    }

    /// Walk the projections no volume backs, which need no driver, until the
    /// resolution stands on a volume or ends in one of them.
    fn advance(&mut self, vfs: &Vfs, cred: &Credentials<'_>) -> Result<Next, VfsError> {
        loop {
            let catalog = match &self.stand {
                Stand::Held(handle) => {
                    return Ok(Next::Volume(Serve {
                        projection: Arc::clone(&handle.projection),
                        pin: Some(handle.node.clone()),
                    }));
                }
                Stand::Holder { projection, dir } => {
                    return Ok(Next::Volume(Serve {
                        projection: Arc::clone(projection),
                        pin: Some(dir.clone()),
                    }));
                }
                Stand::Floor(projection) if projection.backing().is_some() => {
                    return Ok(Next::Volume(Serve {
                        projection: Arc::clone(projection),
                        pin: None,
                    }));
                }
                Stand::Floor(projection) => Arc::clone(projection),
            };
            let meta = vfs.template_of(&catalog)?;
            if core::mem::take(&mut self.check_start) {
                meta.authorize(cred, Access::Execute)?;
            }
            let Some(step) = self.road.pending.pop_front() else {
                self.road.spell_absent()?;
                return Ok(Next::Catalog(CatalogEnd {
                    absent: core::mem::take(&mut self.road.absent),
                    spelled: self.road.take_spelled(),
                    catalog,
                }));
            };
            self.road.spend()?;
            let Some(step) = self.road.carried(step)? else {
                continue;
            };
            meta.authorize(cred, Access::Execute)?;
            match step.kind {
                // `/..` is `/`; a link stores nothing here, so every step is
                // the caller's.
                TargetStep::Up => {
                    if let Some(up) = vfs.mounts().parent(&catalog).cloned() {
                        self.cross(up)?;
                    }
                }
                TargetStep::Name(name) => {
                    let child = vfs
                        .mounts()
                        .child(&catalog, &name, NameMatching::Exact)
                        .cloned();
                    match child {
                        Some(child) => self.cross(child)?,
                        None if self.road.pending.is_empty()
                            || self.road.missing == MissingSteps::Carried =>
                        {
                            self.road.vacant(name)?;
                        }
                        None => return Err(VfsError::NotFound),
                    }
                }
            }
        }
    }

    /// Walk the volume `fs` serves from where the resolution stands, under
    /// the caller's one hold of its driver: what the walk reached, or `None`
    /// when it went on into another projection.
    fn walk<F: FilesystemRead + FilesystemSecurity + ?Sized>(
        &mut self,
        vfs: &Vfs,
        cred: &Credentials<'_>,
        fs: &mut F,
    ) -> Result<Option<Landing>, VfsError> {
        let (projection, begin) = match &self.stand {
            Stand::Held(handle) => (
                Arc::clone(&handle.projection),
                Begin::Held {
                    node: handle.node.pinned(),
                    at_floor: handle.at_floor,
                },
            ),
            Stand::Holder { projection, dir } => (
                Arc::clone(projection),
                Begin::Climbed { node: dir.pinned() },
            ),
            Stand::Floor(projection) => (
                Arc::clone(projection),
                Begin::Floor {
                    check: self.check_start,
                },
            ),
        };
        self.check_start = false;
        // Matched as the volume matches its own entries, so no spelling of a
        // covered name reaches past the projection shown over it.
        let matching = fs.name_matching();
        let beneath = |name: &str| vfs.mounts().child(&projection, name, matching).cloned();
        // Taken once, under this hold of the driver every bind through the
        // projection was made under.
        let covers = projection
            .has_binds_within()
            .then(|| Arc::clone(vfs.mounts().covers()));
        let covered = |node: NodeId| {
            covers
                .as_ref()
                .and_then(|covers| covers.covering(&projection, node))
                .cloned()
        };
        let edge = Edge {
            path: projection.path(),
            has_parent: !projection.path().is_root(),
            beneath: &beneath,
            covered: &covered,
        };
        let descent =
            DelegatedFs::bind(fs, &projection).walk_segment(cred, begin, &mut self.road, &edge)?;
        match descent {
            Descent::Landed(walk) => Ok(Some(Landing {
                spelled: self.road.take_spelled(),
                projection,
                walk,
            })),
            Descent::Above => {
                if let Some(cover) = projection.cover() {
                    self.climb_out(&projection, cover)?;
                } else {
                    let up = vfs.mounts().parent(&projection).cloned();
                    self.cross(up.ok_or(VfsError::NotFound)?)?;
                }
                Ok(None)
            }
            Descent::Into(child) => {
                self.cross(child)?;
                Ok(None)
            }
        }
    }

    /// Take the next step from the floor where the resolution stands, whose
    /// volume no driver serves yet: only the projections shown beneath it are
    /// reached through it, searched under the record that stands for it.
    ///
    /// # Errors
    ///
    /// [`VfsError::Unserved`] for any step that needs the volume itself, and
    /// the permission refusals.
    fn pass_unserved(&mut self, vfs: &Vfs, cred: &Credentials<'_>) -> Result<(), VfsError> {
        let floor = match &self.stand {
            Stand::Floor(floor) if floor.is_live() => Arc::clone(floor),
            _ => return Err(VfsError::Unserved),
        };
        let meta = vfs.template_of(&floor).map_err(|_| VfsError::Unserved)?;
        if core::mem::take(&mut self.check_start) {
            meta.authorize(cred, Access::Execute)?;
        }
        let step = self.road.pending.pop_front().ok_or(VfsError::Unserved)?;
        self.road.spend()?;
        meta.authorize(cred, Access::Execute)?;
        // A link's steps never leave its projection, so every step here is
        // the caller's.
        let next = match step.kind {
            TargetStep::Up if floor.cover().is_some() => return Err(VfsError::Unserved),
            TargetStep::Up => vfs.mounts().parent(&floor).cloned(),
            TargetStep::Name(name) => Some(
                vfs.mounts()
                    .child(&floor, &name, NameMatching::Exact)
                    .cloned()
                    .ok_or(VfsError::Unserved)?,
            ),
        };
        match next {
            Some(projection) => self.cross(projection),
            // `/..` is `/`.
            None => Ok(()),
        }
    }

    /// Stand at the floor of `projection`.
    fn cross(&mut self, projection: Arc<Projection>) -> Result<(), VfsError> {
        self.road.stand_at(projection.path())?;
        self.stand = Stand::Floor(projection);
        Ok(())
    }

    /// Stand where a caller's `..` from the floor of `bind`, which `cover`
    /// says what it covers, leads: the directory holding the covered one.
    fn climb_out(&mut self, bind: &Projection, cover: &Cover) -> Result<(), VfsError> {
        let within = Arc::clone(cover.within());
        let Some(dir) = cover.holder() else {
            return self.cross(within);
        };
        self.road.stand_at(bind.path())?;
        self.road.climbed();
        self.stand = Stand::Holder {
            projection: within,
            dir: dir.clone(),
        };
        Ok(())
    }
}

/// A node a two-path operation keeps named between its walks.
pub(super) enum Kept {
    /// Pinned: the driver's lock is let go between the walks.
    Pinned(NodeRef),
    /// Its id: the one driver is held throughout.
    Id(NodeId),
}

impl Kept {
    fn id(&self) -> NodeId {
        match self {
            Self::Pinned(node) => node.pinned(),
            Self::Id(id) => *id,
        }
    }

    fn pin(&self) -> Option<&NodeRef> {
        match self {
            Self::Pinned(node) => Some(node),
            Self::Id(_) => None,
        }
    }
}

/// The drivers a resolution holds, one projection at a time.
pub(super) trait Drivers {
    /// The driver.
    type Fs: FilesystemRead + FilesystemSecurity + ?Sized;
    /// What an operation learns of a held volume beside its driver.
    type Facts;

    /// Run `op` on the driver serving `serve`, held for the call, once the
    /// projection is still in the mount table and the registration current.
    ///
    /// # Errors
    ///
    /// The refusal of a projection or registration gone since, or `op`'s.
    fn hold<T>(
        &mut self,
        serve: Serve,
        op: impl FnOnce(&mut Self::Fs, &Self::Facts) -> Result<T, VfsError>,
    ) -> Result<T, VfsError>;

    /// Keep `node`, of `kind`, which `fs` reached under the hold `facts`
    /// describe, named once the hold ends.
    ///
    /// # Errors
    ///
    /// [`VfsError::OutOfMemory`], or the driver's refusal to hold it.
    fn keep(
        fs: &mut Self::Fs,
        facts: &Self::Facts,
        node: NodeId,
        kind: DriverNodeKind,
    ) -> Result<Kept, VfsError>;

    /// The published identity of the volume the hold `facts` describe is
    /// of, which every node id it reports carries.
    fn volume(facts: &Self::Facts) -> [u8; 16];
}

/// The one driver a boot reader holds: it serves the first volume a walk
/// holds, and no other.
struct One<'f, F: ?Sized> {
    fs: &'f mut F,
    serving: Option<DriverHandle>,
}

impl<F: FilesystemRead + FilesystemSecurity + ?Sized> Drivers for One<'_, F> {
    type Fs = F;
    type Facts = ();

    fn hold<T>(
        &mut self,
        serve: Serve,
        op: impl FnOnce(&mut Self::Fs, &Self::Facts) -> Result<T, VfsError>,
    ) -> Result<T, VfsError> {
        let handle = serve.projection.backing().ok_or(VfsError::NotFound)?;
        if !serve.projection.is_live() {
            return Err(VfsError::NotFound);
        }
        if *self.serving.get_or_insert(handle) != handle {
            return Err(VfsError::CrossVolume);
        }
        op(&mut *self.fs, &())
    }

    fn keep(
        _fs: &mut Self::Fs,
        _facts: &Self::Facts,
        node: NodeId,
        _kind: DriverNodeKind,
    ) -> Result<Kept, VfsError> {
        Ok(Kept::Id(node))
    }

    /// A boot reader's volume publishes no identity.
    fn volume(_facts: &Self::Facts) -> [u8; 16] {
        FileId::NONE.volume
    }
}

/// Where a resolution ended on a volume.
pub(super) struct Landing {
    /// The projection it ended in.
    pub(super) projection: Arc<Projection>,
    /// What it reached there.
    pub(super) walk: Walk,
    /// The view path it ended at, when it kept one.
    pub(super) spelled: Option<Vec<String>>,
}

/// Where a resolution ended in a projection no volume backs: the catalog
/// itself, or names beneath it that no projection is shown under.
pub(super) struct CatalogEnd {
    /// The catalog.
    pub(super) catalog: Arc<Projection>,
    /// The names beneath it, outermost first; empty at the catalog itself.
    pub(super) absent: Vec<String>,
    /// The view path it ended at, when the resolution kept one.
    pub(super) spelled: Option<Vec<String>>,
}

/// The place a two-path operation's final name occupies, its directory kept
/// named between the walks.
struct KeptPlace {
    projection: Arc<Projection>,
    dir: Kept,
    name: String,
}

/// How many names a walk that reached `walk` left below the deepest node
/// that exists: none, the final one, or more.
pub(super) fn vacancy(walk: &Walk) -> usize {
    match walk {
        Walk::Bottom { .. } => 0,
        Walk::Leaf(place) => usize::from(!place.is_occupied()),
        Walk::Beyond => 2,
    }
}

/// The canonical answer of a walk that left `vacant` names below the
/// deepest node that exists and ended at `spelled`, as `mode` allows.
///
/// # Errors
///
/// [`VfsError::NotFound`] for a missing component `mode` requires, and
/// [`VfsError::InvalidPath`] for an answer this VFS would not take back.
pub(super) fn canonical_answer(
    vacant: usize,
    mode: RealpathMode,
    spelled: Option<Vec<String>>,
) -> Result<String, VfsError> {
    let allowed = match vacant {
        0 => true,
        1 => mode.tolerates_vacant_final(),
        _ => mode.tolerates_missing_intermediate(),
    };
    if !allowed {
        return Err(VfsError::NotFound);
    }
    spell_canonical(&spelled.ok_or(VfsError::Io)?)
}

/// What a canonicalisation's posture is under `mode`.
pub(super) const fn canonicalising(mode: RealpathMode) -> Posture {
    Posture {
        // `readlink -f` on a link reports what it names, never the link.
        final_link: FinalLink::Follow,
        missing: if mode.tolerates_missing_intermediate() {
            MissingSteps::Carried
        } else {
            MissingSteps::Refused
        },
        links: Links::Follow,
    }
}

/// Spell the canonical `/`-view path `components`, refusing an answer this
/// VFS would not accept back: none is ever longer than [`FS_PATH_MAX`] or
/// deeper than [`MAX_PATH_COMPONENTS`], so a caller is never handed a name it
/// is then refused for spelling.
///
/// # Errors
///
/// [`VfsError::InvalidPath`] past either bound.
pub(super) fn spell_canonical(components: &[String]) -> Result<String, VfsError> {
    if components.len() > MAX_PATH_COMPONENTS {
        return Err(VfsError::InvalidPath);
    }
    let spelled = spell(components)?;
    if spelled.len() > FS_PATH_MAX {
        return Err(VfsError::InvalidPath);
    }
    Ok(spelled)
}

/// An in-RAM virtual filesystem tree.
///
/// The mount table lives behind its own [`RwLock`] so a runtime volume
/// attach/detach can add or retract a mount through a shared `&Vfs` (the
/// set-once boot cell) while concurrent operations resolve against it.
/// Every reader takes the guard only for the tiny lookup and copies what
/// it needs out — never across a driver call, which may park.
pub struct Vfs {
    root: Node,
    mounts: RwLock<MountTable>,
}

impl Vfs {
    /// Construct a VFS whose root directory carries `root_meta` and a
    /// single writable root mount. The root starts empty.
    #[must_use]
    pub fn new(root_meta: Metadata) -> Self {
        Self {
            root: Node::directory(root_meta),
            mounts: RwLock::new(MountTable::new(MountFlags::default())),
        }
    }

    /// Construct a VFS pre-populated with the default layout: the root-view
    /// entries ([`ROOT_VIEW`]), the two writable exceptions under `/System`
    /// (`/System/Logs`, `/System/Settings`), and the mount policy.
    ///
    /// `/Users` is a plain directory of whatever backs `/`. `/Storage` is a
    /// mount with no backing: the catalog of the volumes attached beneath it,
    /// read-only so nothing can be stored in it. A mount exists only where a
    /// different volume, or a narrower policy, applies.
    ///
    /// `owner`/`group` own the system directories. The layout is fixed and
    /// correct by construction.
    #[must_use]
    pub fn with_default_layout(owner: UserId, group: GroupId) -> Self {
        let dir_meta = || Metadata::new(owner, group, Mode::from_bits(0o755));

        let mut system = Node::directory(dir_meta());
        if let NodeKind::Directory(entries) = &mut system.kind {
            entries.insert("Logs".to_string(), Node::directory(dir_meta()));
            entries.insert("Settings".to_string(), Node::directory(dir_meta()));
        }

        let mut root = Node::directory(dir_meta());
        if let NodeKind::Directory(entries) = &mut root.kind {
            for name in ROOT_VIEW {
                let node = if name == "System" {
                    system.clone()
                } else {
                    Node::directory(dir_meta())
                };
                entries.insert(name.to_string(), node);
            }
        }

        let mut mounts = MountTable::new(MountFlags::default());
        // The writable `/System/Logs` and `/System/Settings` projections sit
        // beneath the read-only `/System`'s floor.
        let catalog = alloc::format!("/{STORAGE_CATALOG}");
        // The two writable grafts take the system's own writes, so those may
        // draw on their volume's reserve once users have filled it.
        let policy = [
            ("/System", MountFlags::READ_ONLY, AllocationClass::Ordinary),
            ("/System/Logs", MountFlags::NOEXEC, AllocationClass::System),
            (
                "/System/Settings",
                MountFlags::NOEXEC,
                AllocationClass::System,
            ),
            (
                catalog.as_str(),
                MountFlags::READ_ONLY.union(MountFlags::NOEXEC),
                AllocationClass::Ordinary,
            ),
        ];
        for (path, flags, class) in policy {
            if let Ok(p) = Path::parse(path) {
                let _ = mounts.mount_classed(p, flags, None, class);
            }
        }

        Self {
            root,
            mounts: RwLock::new(mounts),
        }
    }

    /// The mount table, read-locked for the duration of the returned guard.
    ///
    /// Hold the guard only for the lookup and copy the needed facts out;
    /// never hold it across a driver operation, which may park while the
    /// guard's contenders would spin.
    pub fn mounts(&self) -> RwLockReadGuard<'_, MountTable> {
        self.mounts.read()
    }

    /// The mount table, write-locked, for wiring a block-backed filesystem
    /// driver into a subtree or adding/retracting a runtime mount.
    pub fn mounts_write(&self) -> RwLockWriteGuard<'_, MountTable> {
        self.mounts.write()
    }

    /// Whether the mount table could be write-locked now, so a test can prove
    /// no operation holds a guard on it into a driver.
    #[cfg(test)]
    pub(crate) fn mounts_unlocked(&self) -> bool {
        self.mounts.try_write().is_some()
    }

    /// The projection showing `/`.
    fn root_projection(&self) -> Result<Arc<Projection>, VfsError> {
        self.mounts().root().cloned().ok_or(VfsError::NotFound)
    }

    /// Resolve `way` across every projection it crosses, holding one driver
    /// at a time through `drivers`, and run `on_volume` on what it reached
    /// under the same hold of the driver it ended on — or `on_catalog` where
    /// it ended in a projection no volume backs.
    ///
    /// # Errors
    ///
    /// The walk's refusal, or the one `on_volume` or `on_catalog` answers.
    pub(super) fn resolve<D: Drivers, T>(
        &self,
        cred: &Credentials<'_>,
        drivers: &mut D,
        mut way: Resolution<'_>,
        on_volume: impl FnOnce(&mut D::Fs, &D::Facts, Landing) -> Result<T, VfsError>,
        on_catalog: impl FnOnce(CatalogEnd) -> Result<T, VfsError>,
    ) -> Result<T, VfsError> {
        let mut on_volume = Some(on_volume);
        loop {
            let serve = match way.advance(self, cred)? {
                Next::Volume(serve) => serve,
                Next::Catalog(end) => return on_catalog(end),
            };
            let mut held = false;
            let done = drivers.hold(serve, |fs, facts| {
                held = true;
                match way.walk(self, cred, fs)? {
                    Some(landing) => match on_volume.take() {
                        Some(op) => op(fs, facts, landing).map(Some),
                        None => Err(VfsError::Io),
                    },
                    None => Ok(None),
                }
            });
            match done {
                Ok(Some(done)) => return Ok(done),
                Ok(None) => {}
                // Nothing serves the volume yet, as before the root is
                // unlocked: the projections shown beneath it are still reached.
                Err(VfsError::Unserved) if !held => way.pass_unserved(self, cred)?,
                Err(refused) => return Err(refused),
            }
        }
    }

    /// Resolve `way` to the place its final name occupies, the directory
    /// holding it kept named for the rest of a two-path operation.
    fn kept_place<D: Drivers>(
        &self,
        cred: &Credentials<'_>,
        drivers: &mut D,
        way: Resolution<'_>,
    ) -> Result<KeptPlace, VfsError> {
        self.resolve(
            cred,
            drivers,
            way,
            |fs, facts, landing| {
                MountUse::Write.admitted_by(&landing.projection)?;
                let place = landing.walk.place()?;
                let dir = D::keep(fs, facts, place.parent, DriverNodeKind::Directory)?;
                Ok(KeptPlace {
                    projection: landing.projection,
                    dir,
                    name: place.name,
                })
            },
            |_| Err(VfsError::ReadOnly),
        )
    }

    /// Move the name `from` reaches to the place `to` reaches, both within
    /// one projection, under one hold of its driver once both directories
    /// are kept named.
    ///
    /// # Errors
    ///
    /// [`VfsError::CrossVolume`] for two projections, and otherwise as
    /// [`DelegatedFs::rename_places`].
    pub(super) fn rename_between<D>(
        &self,
        cred: &Credentials<'_>,
        drivers: &mut D,
        from: Resolution<'_>,
        to: Resolution<'_>,
        flags: RenameFlags,
        expect: Option<Expect<'_>>,
    ) -> Result<(), VfsError>
    where
        D: Drivers,
        D::Fs: FilesystemWrite,
    {
        let src = self.kept_place(cred, drivers, from)?;
        let dst = self.kept_place(cred, drivers, to)?;
        if *src.projection != *dst.projection {
            return Err(VfsError::CrossVolume);
        }
        let serve = Serve {
            projection: Arc::clone(&src.projection),
            pin: src.dir.pin().cloned(),
        };
        let guarded = |node| {
            src.projection.has_binds_within()
                && self.mounts().covers().guards(&src.projection, node)
        };
        drivers.hold(serve, |fs, facts| {
            DelegatedFs::bind(fs, &src.projection).rename_places(
                cred,
                src.dir.id(),
                &src.name,
                dst.dir.id(),
                &dst.name,
                flags,
                expect.map(|expect| expect.on(D::volume(facts))),
                &guarded,
            )
        })
    }

    /// Give the node `existing` reaches the second name `new` reaches, both
    /// within one projection, under one hold of its driver once the node and
    /// the new name's directory are kept named.
    ///
    /// # Errors
    ///
    /// [`VfsError::CrossVolume`] for two projections, and otherwise as
    /// [`DelegatedFs::link_places`].
    pub(super) fn link_between<D>(
        &self,
        cred: &Credentials<'_>,
        drivers: &mut D,
        existing: Resolution<'_>,
        new: Resolution<'_>,
    ) -> Result<(), VfsError>
    where
        D: Drivers,
        D::Fs: FilesystemWrite,
    {
        // The node gaining a name is resolved first: a refusal there leaves the
        // new name's directory untouched.
        let (projection, node) = self.resolve(
            cred,
            drivers,
            existing,
            |fs, facts, landing| {
                MountUse::Write.admitted_by(&landing.projection)?;
                let reached = landing.walk.reached()?;
                if reached.info.kind == DriverNodeKind::Directory {
                    return Err(VfsError::IsADirectory);
                }
                let node = D::keep(fs, facts, reached.node, reached.info.kind)?;
                Ok((landing.projection, node))
            },
            |_| Err(VfsError::ReadOnly),
        )?;
        let new = self.kept_place(cred, drivers, new)?;
        if *projection != *new.projection {
            return Err(VfsError::CrossVolume);
        }
        let serve = Serve {
            projection: Arc::clone(&new.projection),
            pin: new.dir.pin().cloned(),
        };
        drivers.hold(serve, |fs, _| {
            DelegatedFs::bind(fs, &new.projection).link_places(
                cred,
                node.id(),
                new.dir.id(),
                &new.name,
            )
        })
    }

    /// The record standing for `projection`'s floor where no volume answers
    /// for it — a catalog, or a volume no driver serves yet: a runtime
    /// projection's template, or its node in the boot layout.
    ///
    /// # Errors
    ///
    /// [`VfsError::NotFound`] for a projection with neither.
    pub(super) fn template_of<'a>(
        &'a self,
        projection: &'a Projection,
    ) -> Result<&'a Metadata, VfsError> {
        projection
            .template()
            .or_else(|| self.node_at(projection.path()).map(|node| &node.meta))
            .ok_or(VfsError::NotFound)
    }

    /// The boot layout's node at `path`, unauthorised: whoever asks has
    /// walked to it.
    fn node_at(&self, path: &Path) -> Option<&Node> {
        let mut node = &self.root;
        for name in path.components() {
            let NodeKind::Directory(entries) = &node.kind else {
                return None;
            };
            node = entries.get(name)?;
        }
        Some(node)
    }

    /// Run `op` on what `way` reaches, `fs` serving the one volume the walk
    /// may reach, `purpose` refused where that projection's flags forbid it.
    /// A projection no volume backs serves nothing to a one-driver reader,
    /// and a walk going on to a second volume is [`VfsError::CrossVolume`].
    fn on_one<F, T>(
        &self,
        cred: &Credentials<'_>,
        way: Resolution<'_>,
        fs: &mut F,
        purpose: MountUse,
        op: impl FnOnce(&mut DelegatedFs<'_, F>, Walk, Option<Vec<String>>) -> Result<T, VfsError>,
    ) -> Result<T, VfsError>
    where
        F: FilesystemRead + FilesystemSecurity + ?Sized,
    {
        let mut one = One { fs, serving: None };
        self.resolve(
            cred,
            &mut one,
            way,
            |fs, (), landing| {
                let Landing {
                    projection,
                    walk,
                    spelled,
                } = landing;
                purpose.admitted_by(&projection)?;
                op(&mut DelegatedFs::bind(fs, &projection), walk, spelled)
            },
            |_| Err(VfsError::NotFound),
        )
    }

    /// Hand `each` the entries of the directory at `path` from `at`, as
    /// [`HeldNode::list_from`](super::delegate::HeldNode::list_from) does,
    /// `fs` serving the one volume it lives on.
    ///
    /// # Errors
    ///
    /// * [`VfsError::NotFound`] if the path names nothing, or ends in a
    ///   projection no volume backs.
    /// * [`VfsError::CrossVolume`] if its walk goes on from the first volume
    ///   it holds onto another, which `fs` does not serve.
    /// * [`VfsError::NotADirectory`], [`VfsError::PermissionDenied`],
    ///   [`VfsError::InvalidPath`], or [`VfsError::Io`].
    pub fn list_via_secured<F: FilesystemRead + FilesystemSecurity + ?Sized>(
        &self,
        cred: &Credentials<'_>,
        path: &Path,
        fs: &mut F,
        final_link: FinalLink,
        at: &mut DirPosition,
        each: &mut dyn FnMut(&DelegatedRef<'_>) -> DirVisit,
    ) -> Result<ListEnd, VfsError> {
        let way = Resolution::of_path(self, path, Posture::of(final_link), Spelling::Unkept)?;
        self.on_one(cred, way, fs, MountUse::Read, |delegated, walk, _| {
            delegated.held(walk)?.0.list_from(cred, false, at, each)
        })
    }

    /// Append up to `len` leading bytes of the regular file at `path`, of at
    /// most `max` bytes, to `buf`, `fs` serving the one volume it lives on.
    ///
    /// Nothing is read unless the node is a regular file of at most `max`
    /// bytes that `cred` may read; a refusal leaves `buf` as it was, any bytes
    /// already read into it zeroed first.
    ///
    /// # Errors
    ///
    /// The [`BoundedReadError`] naming the first check that refused.
    pub fn read_bounded_via_secured<F: FilesystemRead + FilesystemSecurity + ?Sized>(
        &self,
        cred: &Credentials<'_>,
        path: &Path,
        fs: &mut F,
        max: u64,
        len: usize,
        buf: &mut Vec<u8>,
    ) -> Result<(), BoundedReadError> {
        let posture = Posture::of(FinalLink::Follow);
        let way = Resolution::of_path(self, path, posture, Spelling::Unkept)?;
        self.on_one(cred, way, fs, MountUse::Read, |delegated, walk, _| {
            Ok(delegated.read_bounded_at(cred, walk, max, len, buf))
        })?
    }

    /// Look up the [`Metadata`] of the inode at `path`.
    ///
    /// Requires search (execute) permission on every intermediate
    /// directory but no permission on the target itself, mirroring POSIX
    /// `stat`.
    ///
    /// # Errors
    ///
    /// [`VfsError::InvalidPath`], [`VfsError::NotFound`],
    /// [`VfsError::NotADirectory`], or [`VfsError::PermissionDenied`].
    pub fn metadata(&self, cred: &Credentials<'_>, path: &Path) -> Result<&Metadata, VfsError> {
        Ok(&self.resolve_in_ram(cred, path)?.meta)
    }

    /// Create a directory at `path` with mode `mode`.
    ///
    /// # Errors
    ///
    /// * [`VfsError::ReadOnly`] if the covering mount is read-only.
    /// * [`VfsError::PermissionDenied`] if the caller lacks write
    ///   permission on the parent directory.
    /// * [`VfsError::AlreadyExists`], [`VfsError::NotFound`],
    ///   [`VfsError::NotADirectory`], or [`VfsError::InvalidPath`].
    pub fn mkdir(
        &mut self,
        cred: &Credentials<'_>,
        path: &Path,
        mode: Mode,
    ) -> Result<(), VfsError> {
        let meta = Metadata::new(cred.uid, cred.gid, mode);
        self.create(cred, path, Node::directory(meta))
    }

    /// Create a regular file at `path` with mode `mode` and `contents`.
    ///
    /// # Errors
    ///
    /// As [`Vfs::mkdir`].
    pub fn create_file(
        &mut self,
        cred: &Credentials<'_>,
        path: &Path,
        mode: Mode,
        contents: Vec<u8>,
    ) -> Result<(), VfsError> {
        let meta = Metadata::new(cred.uid, cred.gid, mode);
        self.create(cred, path, Node::file(meta, contents))
    }

    /// Read the contents of the file at `path`.
    ///
    /// # Errors
    ///
    /// * [`VfsError::IsADirectory`] if `path` names a directory.
    /// * [`VfsError::PermissionDenied`] if the caller lacks read
    ///   permission on the file.
    /// * [`VfsError::NotFound`], [`VfsError::NotADirectory`], or
    ///   [`VfsError::InvalidPath`].
    pub fn read(&self, cred: &Credentials<'_>, path: &Path) -> Result<&[u8], VfsError> {
        let node = self.resolve_in_ram(cred, path)?;
        let NodeKind::File(bytes) = &node.kind else {
            return Err(VfsError::IsADirectory);
        };
        node.meta.authorize(cred, Access::Read)?;
        Ok(bytes)
    }

    /// Replace the contents of the file at `path`.
    ///
    /// # Errors
    ///
    /// * [`VfsError::ReadOnly`] if the covering mount is read-only.
    /// * [`VfsError::IsADirectory`] if `path` names a directory.
    /// * [`VfsError::PermissionDenied`] if the caller lacks write
    ///   permission on the file.
    /// * [`VfsError::NotFound`], [`VfsError::NotADirectory`], or
    ///   [`VfsError::InvalidPath`].
    pub fn write(
        &mut self,
        cred: &Credentials<'_>,
        path: &Path,
        contents: Vec<u8>,
    ) -> Result<(), VfsError> {
        if self.mounts.read().is_read_only(path) {
            return Err(VfsError::ReadOnly);
        }
        let node = self.resolve_mut(cred, path)?;
        let NodeKind::File(bytes) = &mut node.kind else {
            return Err(VfsError::IsADirectory);
        };
        node.meta.authorize(cred, Access::Write)?;
        *bytes = contents;
        Ok(())
    }

    /// List the entry names of the directory at `path`, sorted.
    ///
    /// # Errors
    ///
    /// * [`VfsError::NotADirectory`] if `path` names a file.
    /// * [`VfsError::PermissionDenied`] if the caller lacks read
    ///   permission on the directory.
    /// * [`VfsError::NotFound`] or [`VfsError::InvalidPath`].
    pub fn list(&self, cred: &Credentials<'_>, path: &Path) -> Result<Vec<String>, VfsError> {
        let node = self.resolve_in_ram(cred, path)?;
        let NodeKind::Directory(entries) = &node.kind else {
            return Err(VfsError::NotADirectory);
        };
        node.meta.authorize(cred, Access::Read)?;
        Ok(entries.keys().cloned().collect())
    }

    /// Remove the (empty) directory or file at `path`.
    ///
    /// With `dir_only` the removal succeeds only when the name is an
    /// (empty) directory — the atomic `rmdir` posture, decided here in the
    /// same lookup that removes the entry, never by a caller-side stat a
    /// concurrent rename could invalidate.
    ///
    /// # Errors
    ///
    /// * [`VfsError::ReadOnly`] if the covering mount is read-only.
    /// * [`VfsError::NotEmpty`] if `path` is a non-empty directory.
    /// * [`VfsError::NotADirectory`] if `dir_only` and `path` is not a
    ///   directory.
    /// * [`VfsError::PermissionDenied`] if the caller lacks write
    ///   permission on the parent directory, or
    ///   [`Metadata::admits_name_removal`] refuses the occupant.
    /// * [`VfsError::NotFound`], [`VfsError::NotADirectory`], or
    ///   [`VfsError::InvalidPath`].
    pub fn remove(
        &mut self,
        cred: &Credentials<'_>,
        path: &Path,
        dir_only: bool,
    ) -> Result<(), VfsError> {
        let name = owned_name(path.file_name().ok_or(VfsError::InvalidPath)?)?;
        let parent_path = path.parent()?.ok_or(VfsError::InvalidPath)?;
        // Removal mutates the parent directory, so the parent's covering
        // mount governs writability (e.g. removing the `/System/Logs`
        // mount point is forbidden by the read-only `/System`).
        if self.mounts.read().is_read_only(&parent_path) {
            return Err(VfsError::ReadOnly);
        }
        let parent = self.resolve_mut(cred, &parent_path)?;
        parent.meta.authorize(cred, Access::Write)?;
        let NodeKind::Directory(entries) = &mut parent.kind else {
            return Err(VfsError::NotADirectory);
        };

        let occupant = entries.get(&name).ok_or(VfsError::NotFound)?;
        occupant.meta.admits_name_removal(&parent.meta, cred)?;
        match &occupant.kind {
            NodeKind::Directory(children) if !children.is_empty() => {
                return Err(VfsError::NotEmpty)
            }
            NodeKind::File(_) if dir_only => return Err(VfsError::NotADirectory),
            NodeKind::Directory(_) | NodeKind::File(_) => {}
        }
        entries.remove(&name);
        Ok(())
    }

    /// Set (or clear) the capability gate on the inode at `path`. Only the inode's owner may change it.
    ///
    /// # Errors
    ///
    /// * [`VfsError::ReadOnly`] if the covering mount is read-only.
    /// * [`VfsError::PermissionDenied`] if the caller is not the owner.
    /// * [`VfsError::NotFound`], [`VfsError::NotADirectory`], or
    ///   [`VfsError::InvalidPath`].
    pub fn set_required_cap(
        &mut self,
        cred: &Credentials<'_>,
        path: &Path,
        cap: Option<CapabilityId>,
    ) -> Result<(), VfsError> {
        if self.mounts.read().is_read_only(path) {
            return Err(VfsError::ReadOnly);
        }
        let node = self.resolve_mut(cred, path)?;
        if node.meta.owner != cred.uid {
            return Err(VfsError::PermissionDenied);
        }
        node.meta.required_cap = cap;
        Ok(())
    }

    /// Insert `child` at `path`, enforcing the shared create preconditions:
    /// read-only refusal, parent resolution, parent write permission, and the
    /// no-clobber existence check.
    fn create(&mut self, cred: &Credentials<'_>, path: &Path, child: Node) -> Result<(), VfsError> {
        let name = owned_name(path.file_name().ok_or(VfsError::InvalidPath)?)?;
        let parent_path = path.parent()?.ok_or(VfsError::InvalidPath)?;
        // Creation mutates the parent directory, so the parent's covering
        // mount governs writability.
        if self.mounts.read().is_read_only(&parent_path) {
            return Err(VfsError::ReadOnly);
        }
        let parent = self.resolve_mut(cred, &parent_path)?;
        parent.meta.authorize(cred, Access::Write)?;
        let NodeKind::Directory(entries) = &mut parent.kind else {
            return Err(VfsError::NotADirectory);
        };
        if entries.contains_key(&name) {
            return Err(VfsError::AlreadyExists);
        }
        entries.insert(name, child);
        Ok(())
    }

    /// Resolve `path` to a node of the in-RAM tree, enforcing search (execute)
    /// permission on every directory descended into.
    fn resolve_in_ram(&self, cred: &Credentials<'_>, path: &Path) -> Result<&Node, VfsError> {
        let mut node = &self.root;
        for component in path.components() {
            let NodeKind::Directory(entries) = &node.kind else {
                return Err(VfsError::NotADirectory);
            };
            node.meta.authorize(cred, Access::Execute)?;
            node = entries.get(component).ok_or(VfsError::NotFound)?;
        }
        Ok(node)
    }

    /// Mutable counterpart to [`Vfs::resolve`].
    fn resolve_mut(&mut self, cred: &Credentials<'_>, path: &Path) -> Result<&mut Node, VfsError> {
        let mut node = &mut self.root;
        for component in path.components() {
            let NodeKind::Directory(entries) = &mut node.kind else {
                return Err(VfsError::NotADirectory);
            };
            node.meta.authorize(cred, Access::Execute)?;
            node = entries.get_mut(component).ok_or(VfsError::NotFound)?;
        }
        Ok(node)
    }
}

#[cfg(test)]
#[path = "vfs_tests.rs"]
mod tests;
