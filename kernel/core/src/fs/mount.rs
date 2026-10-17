//! The mount table: the volumes attached to the namespace, and the view paths
//! that project them.
//!
//! An [`Attachment`] is a volume: the driver serving it, the medium of the
//! device it reads, and the [`MountFlags`] it was attached with. A projection
//! ([`MountPoint`]) binds a view [`Path`] to a subtree of one attachment, and
//! its flags are the attachment's plus whatever the projection narrows, so no
//! projection can widen what its volume was attached with. A projection with
//! no attachment is policy only: the `/Storage` catalog, or a boot-layout
//! mount the boot path has yet to back.
//!
//! The most specific projection covering a path — the longest mount-point
//! [`Path`] that prefixes it — decides each operation's policy. The root
//! projection (`/`) covers everything, so resolution always succeeds.
//!
//! Every mutator records its change through `MountTable::changed` under the
//! table's own lock, so the `Mounts` system notice and every directory watcher
//! converge on it instead of polling (`plans/NOTICE.md`,
//! `docs/src/filesystem/watch.md`).

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::blkio::BlkMedium;
use tairix_abi::driver::filesystem::{AllocationClass, MountFlags};
use tairix_abi::driver::DriverHandle;

use super::path::Path;
use super::perm::Metadata;
use super::VfsError;

/// A volume attached to the namespace: the filesystem driver serving it, the
/// storage medium of the device that driver reads, and the flags it was
/// attached with.
///
/// The medium is learned when the volume is attached and never re-derived,
/// so it cannot be sourced from a different place than the driver.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Attachment {
    driver: DriverHandle,
    medium: Option<BlkMedium>,
    flags: MountFlags,
}

impl Attachment {
    /// The volume `driver` serves, read from a device of `medium` and
    /// attached with `flags`.
    ///
    /// `medium` is `None` when the attach path has no classified block
    /// device in hand — a driver serving synthetic content, or a
    /// bootstrap-floor volume brought up before any classified block client
    /// exists. Unknown is recorded as unknown; a medium is never assumed.
    #[must_use]
    pub const fn new(driver: DriverHandle, medium: Option<BlkMedium>, flags: MountFlags) -> Self {
        Self {
            driver,
            medium,
            flags,
        }
    }

    /// The filesystem driver serving the volume.
    #[must_use]
    pub const fn driver(self) -> DriverHandle {
        self.driver
    }

    /// The storage medium of the block device behind the driver, or `None`
    /// when the attach path could not name one.
    #[must_use]
    pub const fn medium(self) -> Option<BlkMedium> {
        self.medium
    }

    /// The flags the volume was attached with, which every projection of it
    /// keeps.
    #[must_use]
    pub const fn flags(self) -> MountFlags {
        self.flags
    }
}

/// One projection in the [`MountTable`]: a view path onto a subtree of an
/// attached volume, or a policy-only mount point.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MountPoint {
    path: Path,
    flags: MountFlags,
    backing: Option<DriverHandle>,
    /// The path *within the backing volume* at which this mount is rooted:
    /// empty for a volume shown from its own root, `["System", "Logs"]` for
    /// `/System/Logs` carved out of the root volume. The delegated walk
    /// prepends it to the remainder below the mount point.
    backing_subtree: Vec<String>,
    /// The permission template the delegated walk applies at and below the
    /// mount point, for a **runtime** mount whose mount point has no node
    /// in the in-RAM layout tree (a hotplug volume under `/Storage/<name>`).
    /// `None` for the boot layout's mounts, whose mount-point node in the
    /// tree is the template.
    template: Option<Metadata>,
    /// The class every driver mutation through this projection allocates
    /// under: whether the writes it takes may draw on the volume's reserve.
    class: AllocationClass,
    /// The table's epoch when this mount last changed.
    changed_at: u64,
}

/// What a directory's listing shows of the backed mounts directly beneath
/// it. Every change to that set moves the count or introduces a later
/// `latest`, so two equal values mean an unchanged set.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ChildMounts {
    count: usize,
    latest: u64,
}

impl MountPoint {
    /// The mount-point path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The projection's flags: its attachment's, plus whatever it narrows.
    #[must_use]
    pub fn flags(&self) -> MountFlags {
        self.flags
    }

    /// The driver of the attached volume this projection shows, or `None`
    /// for a policy-only mount.
    #[must_use]
    pub fn backing(&self) -> Option<DriverHandle> {
        self.backing
    }

    /// The path components within the backing volume at which this mount is
    /// rooted. Empty for a mount rooted at its driver's own root directory;
    /// non-empty for a sub-mount of a larger volume (e.g. `["System",
    /// "Logs"]` for `/System/Logs` backed by the root volume's own
    /// `/System/Logs` directory).
    #[must_use]
    pub fn backing_subtree(&self) -> &[String] {
        &self.backing_subtree
    }

    /// The class every driver mutation through this projection allocates
    /// under.
    #[must_use]
    pub fn class(&self) -> AllocationClass {
        self.class
    }

    /// `true` if this mount is read-only.
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        self.flags.contains(MountFlags::READ_ONLY)
    }

    /// The owned permission template of a runtime mount, or `None` for a
    /// boot-layout mount (whose mount-point node in the in-RAM tree is the
    /// template).
    #[must_use]
    pub fn template(&self) -> Option<&Metadata> {
        self.template.as_ref()
    }
}

/// A path and the mount that covered it at one reading of the table.
///
/// An operation is judged against this mount and served by the driver looked
/// up under that same reading, so no later attach or detach can pair one
/// volume's driver with another projection's flags.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoveredPath {
    path: Path,
    mount: MountPoint,
}

impl CoveredPath {
    /// The path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The mount that covered it.
    #[must_use]
    pub fn mount(&self) -> &MountPoint {
        &self.mount
    }

    /// The path's components below its mount point.
    #[must_use]
    pub fn remainder(&self) -> &[String] {
        self.path
            .components()
            .get(self.mount.path.depth()..)
            .unwrap_or_default()
    }
}

/// Two paths and the mounts that covered them at one reading of the table,
/// for an operation that names both.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoveredPair {
    first: CoveredPath,
    second: CoveredPath,
}

impl CoveredPair {
    /// The first path, covered.
    #[must_use]
    pub fn first(&self) -> &CoveredPath {
        &self.first
    }

    /// The second path, covered.
    #[must_use]
    pub fn second(&self) -> &CoveredPath {
        &self.second
    }
}

/// The system mount table.
///
/// Always holds a root projection at `/`, so [`MountTable::resolve`] never
/// fails. Attachments and projections are stored in insertion order and
/// scanned linearly: the table holds one entry per mounted volume and view
/// path, so an index would be bloat.
///
/// Every backed projection's attachment is in the table: a projection onto
/// an unattached volume is refused, and detaching a volume withdraws every
/// projection of it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MountTable {
    attachments: Vec<Attachment>,
    mounts: Vec<MountPoint>,
    /// Advances with every mutation, so a directory watch can tell that what
    /// its path reaches, or a listing's mount points, may have moved.
    epoch: u64,
}

impl MountTable {
    /// Construct a table whose only entry is a policy-only root projection
    /// carrying `root_flags`.
    #[must_use]
    pub fn new(root_flags: MountFlags) -> Self {
        Self {
            attachments: Vec::new(),
            mounts: alloc::vec![MountPoint {
                path: Path::root(),
                flags: root_flags,
                backing: None,
                backing_subtree: Vec::new(),
                template: None,
                class: AllocationClass::Ordinary,
                changed_at: 0,
            }],
            epoch: 0,
        }
    }

    /// How many times the table has changed.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Record a change and tell everything that depends on the table's
    /// composition: the `Mounts` notice's subscribers, and every directory
    /// watcher, whose path may now reach another directory or list a mount
    /// point that came or went.
    fn changed(&mut self) -> u64 {
        self.epoch = self.epoch.wrapping_add(1);
        crate::notice::mounts_changed();
        crate::fswatch::mounts_changed();
        self.epoch
    }

    /// Attach a volume, so projections can show it.
    ///
    /// # Errors
    ///
    /// [`VfsError::AlreadyExists`] if its driver is already attached: a
    /// volume is attached once, never re-flagged in place.
    pub fn attach(&mut self, attachment: Attachment) -> Result<(), VfsError> {
        if self.attachment(attachment.driver).is_some() {
            return Err(VfsError::AlreadyExists);
        }
        self.changed();
        self.attachments.push(attachment);
        Ok(())
    }

    /// Detach the volume `driver` serves, withdrawing every projection of it.
    ///
    /// # Errors
    ///
    /// * [`VfsError::InvalidPath`] if the volume backs the root projection,
    ///   which is permanent.
    /// * [`VfsError::NotFound`] if `driver` serves no attached volume.
    pub fn detach(&mut self, driver: DriverHandle) -> Result<(), VfsError> {
        let at = self
            .attachments
            .iter()
            .position(|a| a.driver == driver)
            .ok_or(VfsError::NotFound)?;
        if self.mounts[0].backing == Some(driver) {
            return Err(VfsError::InvalidPath);
        }
        self.changed();
        self.attachments.remove(at);
        self.mounts.retain(|m| m.backing != Some(driver));
        Ok(())
    }

    /// The attached volume `driver` serves.
    #[must_use]
    pub fn attachment(&self, driver: DriverHandle) -> Option<Attachment> {
        self.attachments
            .iter()
            .copied()
            .find(|a| a.driver == driver)
    }

    /// Every attached volume in attach order, each with the view path of the
    /// earliest projection showing its root, or `None` when no projection
    /// shows the volume from its root.
    #[must_use]
    pub fn volumes(&self) -> Vec<(Attachment, Option<Path>)> {
        let mut roots: Vec<(u64, &Path)> = self
            .mounts
            .iter()
            .filter(|m| m.backing_subtree.is_empty())
            .filter_map(|m| Some((m.backing?.as_u64(), &m.path)))
            .collect();
        // Stable, so the earliest projection of a volume sorts first among
        // its equals.
        roots.sort_by_key(|&(driver, _)| driver);
        self.attachments
            .iter()
            .map(|&attachment| {
                let key = attachment.driver.as_u64();
                let first = roots.partition_point(|&(driver, _)| driver < key);
                let shown = roots
                    .get(first)
                    .filter(|&&(driver, _)| driver == key)
                    .map(|&(_, path)| path.clone());
                (attachment, shown)
            })
            .collect()
    }

    /// The flags a projection onto `backing` carries: the attachment's, with
    /// `narrowing` added.
    fn projected(
        &self,
        narrowing: MountFlags,
        backing: Option<DriverHandle>,
    ) -> Result<MountFlags, VfsError> {
        match backing {
            None => Ok(narrowing),
            Some(driver) => self
                .attachment(driver)
                .map(|attached| attached.flags.union(narrowing))
                .ok_or(VfsError::NotFound),
        }
    }

    /// Add a projection at `path` of the root of the volume `backing` serves,
    /// or a policy-only mount point when `backing` is `None`, adding
    /// `narrowing` to the volume's flags.
    ///
    /// # Errors
    ///
    /// * [`VfsError::AlreadyExists`] if a mount already covers exactly
    ///   `path`.
    /// * [`VfsError::NotFound`] if `backing` serves no attached volume.
    pub fn mount(
        &mut self,
        path: Path,
        narrowing: MountFlags,
        backing: Option<DriverHandle>,
    ) -> Result<(), VfsError> {
        self.mount_classed(path, narrowing, backing, AllocationClass::Ordinary)
    }

    /// [`MountTable::mount`] for a projection whose writes allocate under
    /// `class`.
    ///
    /// # Errors
    ///
    /// As [`MountTable::mount`].
    pub fn mount_classed(
        &mut self,
        path: Path,
        narrowing: MountFlags,
        backing: Option<DriverHandle>,
        class: AllocationClass,
    ) -> Result<(), VfsError> {
        self.insert(path, narrowing, backing, Vec::new(), class)
    }

    /// Add a **sub-mount** at `path`: the subtree `backing_subtree` of the
    /// volume `backing` serves, for a mount point whose path differs from
    /// the content's path on the volume. An empty `backing_subtree` is
    /// exactly [`MountTable::mount`].
    ///
    /// # Errors
    ///
    /// As [`MountTable::mount`].
    pub fn mount_rebased(
        &mut self,
        path: Path,
        narrowing: MountFlags,
        backing: Option<DriverHandle>,
        backing_subtree: Vec<String>,
    ) -> Result<(), VfsError> {
        self.insert(
            path,
            narrowing,
            backing,
            backing_subtree,
            AllocationClass::Ordinary,
        )
    }

    /// Attach `attachment` and show its root at `path` under its own
    /// permission `template`, as one change: a hotplug volume arriving under
    /// `/Storage`, whose mount point has no node in the in-RAM layout tree.
    ///
    /// # Errors
    ///
    /// [`VfsError::AlreadyExists`] if the volume is already attached or a
    /// mount already covers exactly `path`; nothing is changed.
    pub fn attach_at(
        &mut self,
        attachment: Attachment,
        path: Path,
        template: Metadata,
    ) -> Result<(), VfsError> {
        if self.attachment(attachment.driver).is_some()
            || self.mounts.iter().any(|m| m.path == path)
        {
            return Err(VfsError::AlreadyExists);
        }
        let changed_at = self.changed();
        self.attachments.push(attachment);
        self.mounts.push(MountPoint {
            path,
            flags: attachment.flags,
            backing: Some(attachment.driver),
            backing_subtree: Vec::new(),
            template: Some(template),
            class: AllocationClass::Ordinary,
            changed_at,
        });
        Ok(())
    }

    fn insert(
        &mut self,
        path: Path,
        narrowing: MountFlags,
        backing: Option<DriverHandle>,
        backing_subtree: Vec<String>,
        class: AllocationClass,
    ) -> Result<(), VfsError> {
        if self.mounts.iter().any(|m| m.path == path) {
            return Err(VfsError::AlreadyExists);
        }
        let flags = self.projected(narrowing, backing)?;
        let changed_at = self.changed();
        self.mounts.push(MountPoint {
            path,
            flags,
            backing,
            backing_subtree,
            template: None,
            class,
            changed_at,
        });
        Ok(())
    }

    /// Show the root of the volume `driver` serves at `/` — the shape of a
    /// block-backed root volume, the whole tree on one volume as the
    /// installer lays it out. The root projection keeps its own flags and
    /// gains the volume's.
    ///
    /// # Errors
    ///
    /// * [`VfsError::AlreadyExists`] if the root projection is already
    ///   backed: a second root volume is a wiring defect, never a silent
    ///   re-mount (fail closed).
    /// * [`VfsError::NotFound`] if `driver` serves no attached volume.
    pub fn back_root(&mut self, driver: DriverHandle) -> Result<(), VfsError> {
        self.back(0, driver, Vec::new())
    }

    /// Back the **existing** policy-only mount at exactly `path` with the
    /// subtree `backing_subtree` of the volume `driver` serves. The mount
    /// keeps its own flags as its narrowing and gains the volume's.
    ///
    /// This turns a policy-only mount of the default layout
    /// ([`Vfs::with_default_layout`](super::Vfs::with_default_layout)) into
    /// a backed one once the boot path knows which volume backs it.
    ///
    /// # Errors
    ///
    /// * [`VfsError::NotFound`] if no mount covers exactly `path`, or
    ///   `driver` serves no attached volume.
    /// * [`VfsError::AlreadyExists`] if that mount is already backed — a
    ///   mount is backed once, never silently re-backed (fail closed).
    pub fn set_backing(
        &mut self,
        path: &Path,
        driver: DriverHandle,
        backing_subtree: Vec<String>,
    ) -> Result<(), VfsError> {
        let at = self
            .mounts
            .iter()
            .position(|m| &m.path == path)
            .ok_or(VfsError::NotFound)?;
        self.back(at, driver, backing_subtree)
    }

    fn back(
        &mut self,
        at: usize,
        driver: DriverHandle,
        backing_subtree: Vec<String>,
    ) -> Result<(), VfsError> {
        if self.mounts[at].backing.is_some() {
            return Err(VfsError::AlreadyExists);
        }
        let flags = self.projected(self.mounts[at].flags, Some(driver))?;
        let changed_at = self.changed();
        let mount = &mut self.mounts[at];
        mount.flags = flags;
        mount.backing = Some(driver);
        mount.backing_subtree = backing_subtree;
        mount.changed_at = changed_at;
        Ok(())
    }

    /// Remove the projection at exactly `path`. The volume it showed stays
    /// attached.
    ///
    /// # Errors
    ///
    /// * [`VfsError::InvalidPath`] if `path` is the root mount, which
    ///   cannot be unmounted.
    /// * [`VfsError::NotFound`] if no mount covers exactly `path`.
    pub fn unmount(&mut self, path: &Path) -> Result<(), VfsError> {
        if path.is_root() {
            return Err(VfsError::InvalidPath);
        }
        let before = self.mounts.len();
        self.mounts.retain(|m| &m.path != path);
        if self.mounts.len() == before {
            return Err(VfsError::NotFound);
        }
        self.changed();
        Ok(())
    }

    /// The most specific mount covering `path` (the longest mount-point
    /// that is a prefix of `path`). Never fails: the root mount covers
    /// every path.
    #[must_use]
    pub fn resolve(&self, path: &Path) -> &MountPoint {
        self.mounts
            .iter()
            .filter(|m| m.path.is_prefix_of(path))
            .max_by_key(|m| m.path.depth())
            .unwrap_or(&self.mounts[0])
    }

    /// `path` with the mount covering it.
    #[must_use]
    pub fn cover(&self, path: Path) -> CoveredPath {
        CoveredPath {
            mount: self.resolve(&path).clone(),
            path,
        }
    }

    /// `first` and `second` with the mounts covering them, read together.
    #[must_use]
    pub fn cover_pair(&self, first: Path, second: Path) -> CoveredPair {
        CoveredPair {
            first: self.cover(first),
            second: self.cover(second),
        }
    }

    /// `true` if writes to `path` are forbidden by the covering mount's
    /// read-only flag.
    #[must_use]
    pub fn is_read_only(&self, path: &Path) -> bool {
        self.resolve(path).is_read_only()
    }

    /// The mounts whose mount point is a **direct child** of `path`, in
    /// insertion order.
    ///
    /// These are the entries a directory listing merges into `path`'s own
    /// listing so a mount point appears in its parent directory even when the
    /// parent volume holds no node of that name — the runtime
    /// `/Storage/<name>` mounts are exactly that shape, and this is what
    /// enumerates the `Storage:` catalog view. A mount with no backing is
    /// listed too: the catalog itself is stored on no volume, so this merge is
    /// the only thing that shows it in `/`.
    pub fn direct_children<'a>(&'a self, path: &'a Path) -> impl Iterator<Item = &'a MountPoint> {
        self.mounts
            .iter()
            .filter(move |m| m.path.depth() == path.depth() + 1 && path.is_prefix_of(&m.path))
    }

    /// What the listing of `path` shows of the mounts directly beneath it, for
    /// a directory watch to tell whether that set moved.
    #[must_use]
    pub fn children_of(&self, path: &Path) -> ChildMounts {
        self.direct_children(path)
            .fold(ChildMounts::default(), |seen, mount| ChildMounts {
                count: seen.count + 1,
                latest: seen.latest.max(mount.changed_at),
            })
    }

    /// Every mount in the table, in insertion order (the permanent root
    /// mount first).
    ///
    /// A read-only view for the System Information introspection feed: the
    /// order is stable across calls while the table is unchanged, so a broker
    /// paging the mount list never skips or repeats an entry.
    pub fn iter(&self) -> impl Iterator<Item = &MountPoint> {
        self.mounts.iter()
    }

    /// Number of mounts (including the root mount).
    #[must_use]
    pub fn len(&self) -> usize {
        self.mounts.len()
    }

    /// Always `false`: the root mount is permanent.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.mounts.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tairix_abi::blkio::BlkTransport;

    fn p(text: &str) -> Path {
        Path::parse(text).expect("valid path")
    }

    fn handle(raw: u64) -> DriverHandle {
        DriverHandle::from_raw(raw).expect("non-zero handle")
    }

    /// Attach the volume `raw` serves with `flags`, on a device whose medium
    /// the attach path could not name.
    fn attached(table: &mut MountTable, raw: u64, flags: MountFlags) -> DriverHandle {
        let driver = handle(raw);
        table
            .attach(Attachment::new(driver, None, flags))
            .expect("attach");
        driver
    }

    #[test]
    fn a_directorys_child_mounts_move_only_when_its_own_mounts_do() {
        let mut table = MountTable::new(MountFlags::default());
        let storage = p("/Storage");
        let users = p("/Users");
        let empty = table.children_of(&storage);
        let usb = attached(&mut table, 1, MountFlags::default());
        table
            .mount(p("/Storage/usb"), MountFlags::default(), Some(usb))
            .expect("mount");
        let one = table.children_of(&storage);
        assert_ne!(one, empty);
        assert_eq!(
            table.children_of(&users),
            empty,
            "another directory is untouched"
        );
        let deeper = attached(&mut table, 2, MountFlags::default());
        table
            .mount(
                p("/Storage/usb/deeper"),
                MountFlags::default(),
                Some(deeper),
            )
            .expect("mount");
        assert_eq!(
            table.children_of(&storage),
            one,
            "a deeper mount lists elsewhere"
        );
        table.unmount(&p("/Storage/usb")).expect("unmount");
        let again = attached(&mut table, 3, MountFlags::default());
        table
            .mount(p("/Storage/usb"), MountFlags::default(), Some(again))
            .expect("remount");
        assert_ne!(
            table.children_of(&storage),
            one,
            "the same name mounted again is a change"
        );
        let before = table.children_of(&storage);
        table
            .mount(p("/Storage/card"), MountFlags::default(), None)
            .expect("unbacked mount");
        let unbacked = table.children_of(&storage);
        assert_ne!(unbacked, before, "an unbacked mount is listed too");
        let card = attached(&mut table, 4, MountFlags::default());
        table
            .set_backing(&p("/Storage/card"), card, Vec::new())
            .expect("backs");
        assert_ne!(
            table.children_of(&storage),
            unbacked,
            "backing it is a change"
        );
    }

    #[test]
    fn root_mount_covers_everything() {
        let table = MountTable::new(MountFlags::default());
        assert_eq!(table.resolve(&p("/anything/deep")).path(), &Path::root());
        assert!(!table.is_read_only(&p("/anything")));
    }

    #[test]
    fn longest_prefix_wins() {
        let mut table = MountTable::new(MountFlags::default());
        table
            .mount(p("/System"), MountFlags::READ_ONLY, None)
            .expect("mount /System");
        table
            .mount(p("/System/Logs"), MountFlags::NOEXEC, None)
            .expect("mount /System/Logs");

        assert!(table.is_read_only(&p("/System/Drivers/x")));
        // The writable child mount shadows the read-only parent.
        assert!(!table.is_read_only(&p("/System/Logs/boot")));
        assert_eq!(
            table.resolve(&p("/System/Logs/boot")).path(),
            &p("/System/Logs")
        );
    }

    #[test]
    fn duplicate_mount_is_rejected() {
        let mut table = MountTable::new(MountFlags::default());
        table
            .mount(p("/Storage"), MountFlags::default(), None)
            .expect("first");
        assert_eq!(
            table.mount(p("/Storage"), MountFlags::READ_ONLY, None),
            Err(VfsError::AlreadyExists)
        );
    }

    #[test]
    fn a_volume_is_attached_once() {
        let mut table = MountTable::new(MountFlags::default());
        let driver = attached(&mut table, 0x5A00, MountFlags::READ_ONLY);
        assert_eq!(
            table.attach(Attachment::new(driver, None, MountFlags::default())),
            Err(VfsError::AlreadyExists),
            "a second attach would re-flag the volume in place"
        );
        assert_eq!(
            table.attachment(driver).map(Attachment::flags),
            Some(MountFlags::READ_ONLY)
        );
    }

    #[test]
    fn no_projection_widens_its_volumes_flags() {
        let mut table = MountTable::new(MountFlags::default());
        let stick = attached(
            &mut table,
            0x5A01,
            MountFlags::READ_ONLY.union(MountFlags::NOEXEC),
        );
        // A projection asking for nothing still carries the volume's flags.
        table
            .mount(p("/Storage/stick"), MountFlags::default(), Some(stick))
            .expect("whole volume");
        assert_eq!(
            table.resolve(&p("/Storage/stick/run")).flags(),
            MountFlags::READ_ONLY.union(MountFlags::NOEXEC)
        );

        // A narrowing adds to them.
        let root = attached(&mut table, 0x5A02, MountFlags::default());
        table.back_root(root).expect("root");
        table
            .mount_rebased(
                p("/System/Logs"),
                MountFlags::NOEXEC,
                Some(root),
                alloc::vec![String::from("System"), String::from("Logs")],
            )
            .expect("sub-mount");
        assert_eq!(
            table.resolve(&p("/System/Logs/x")).flags(),
            MountFlags::NOEXEC
        );
        assert_eq!(table.resolve(&p("/other")).flags(), MountFlags::default());

        // Backing a policy-only mount keeps its policy and adds the volume's.
        let system = attached(&mut table, 0x5A03, MountFlags::READ_ONLY);
        table
            .mount(p("/System"), MountFlags::NOEXEC, None)
            .expect("policy mount");
        table
            .set_backing(&p("/System"), system, Vec::new())
            .expect("backs");
        assert_eq!(
            table.resolve(&p("/System/Commands")).flags(),
            MountFlags::READ_ONLY.union(MountFlags::NOEXEC)
        );
    }

    #[test]
    fn a_projection_onto_an_unattached_volume_is_refused() {
        let mut table = MountTable::new(MountFlags::default());
        let stranger = handle(0x5A10);
        assert_eq!(
            table.mount(p("/Storage/a"), MountFlags::default(), Some(stranger)),
            Err(VfsError::NotFound)
        );
        assert_eq!(table.back_root(stranger), Err(VfsError::NotFound));
        table
            .mount(p("/Storage/c"), MountFlags::default(), None)
            .expect("policy mount");
        assert_eq!(
            table.set_backing(&p("/Storage/c"), stranger, Vec::new()),
            Err(VfsError::NotFound)
        );
        assert_eq!(table.len(), 2, "only the policy mount and the root exist");
        assert_eq!(table.resolve(&p("/Storage/c")).backing(), None);
    }

    #[test]
    fn detaching_a_volume_withdraws_every_projection_of_it() {
        let mut table = MountTable::new(MountFlags::default());
        let volume = attached(&mut table, 0x5A20, MountFlags::NOEXEC);
        let other = attached(&mut table, 0x5A21, MountFlags::NOEXEC);
        table
            .mount(p("/Storage/vol"), MountFlags::default(), Some(volume))
            .expect("root of it");
        table
            .mount_rebased(
                p("/Storage/docs"),
                MountFlags::default(),
                Some(volume),
                alloc::vec![String::from("docs")],
            )
            .expect("a subtree of it");
        table
            .mount(p("/Storage/other"), MountFlags::default(), Some(other))
            .expect("another volume");

        table.detach(volume).expect("detach");
        assert_eq!(table.attachment(volume), None);
        assert_eq!(table.resolve(&p("/Storage/vol/x")).path(), &Path::root());
        assert_eq!(table.resolve(&p("/Storage/docs/x")).path(), &Path::root());
        assert_eq!(
            table.resolve(&p("/Storage/other/x")).backing(),
            Some(other),
            "an unrelated volume stays"
        );
        assert_eq!(table.detach(volume), Err(VfsError::NotFound));
    }

    #[test]
    fn the_root_volume_cannot_be_detached() {
        let mut table = MountTable::new(MountFlags::default());
        let root = attached(&mut table, 0x5A30, MountFlags::default());
        table.back_root(root).expect("root");
        assert_eq!(table.detach(root), Err(VfsError::InvalidPath));
        assert_eq!(table.resolve(&p("/x")).backing(), Some(root));
    }

    #[test]
    fn an_unshown_volume_stays_attached() {
        let mut table = MountTable::new(MountFlags::default());
        let volume = attached(&mut table, 0x5A40, MountFlags::NOEXEC);
        table
            .mount(p("/Storage/vol"), MountFlags::default(), Some(volume))
            .expect("mount");
        table.unmount(&p("/Storage/vol")).expect("unmount");
        assert_eq!(
            table.volumes(),
            [(Attachment::new(volume, None, MountFlags::NOEXEC), None)]
        );
    }

    #[test]
    fn each_volume_reports_the_earliest_projection_of_its_root() {
        let mut table = MountTable::new(MountFlags::default());
        let root = attached(&mut table, 0x5A51, MountFlags::default());
        let system = attached(&mut table, 0x5A50, MountFlags::READ_ONLY);
        let hidden = attached(&mut table, 0x5A52, MountFlags::NOEXEC);
        table.back_root(root).expect("root");
        table
            .mount_rebased(
                p("/System/Logs"),
                MountFlags::NOEXEC,
                Some(root),
                alloc::vec![String::from("System"), String::from("Logs")],
            )
            .expect("a subtree of the root volume");
        table
            .mount(p("/System"), MountFlags::default(), Some(system))
            .expect("system");
        table
            .mount(p("/Storage/again"), MountFlags::default(), Some(system))
            .expect("the system volume's root shown twice");
        table
            .mount_rebased(
                p("/Storage/part"),
                MountFlags::default(),
                Some(hidden),
                alloc::vec![String::from("part")],
            )
            .expect("only a subtree of it");

        let volumes = table.volumes();
        let shown: Vec<_> = volumes
            .iter()
            .map(|(attachment, at)| (attachment.driver(), at.clone()))
            .collect();
        assert_eq!(
            shown,
            [
                (root, Some(Path::root())),
                (system, Some(p("/System"))),
                (hidden, None),
            ],
            "attach order, each from its earliest root projection"
        );
    }

    #[test]
    fn back_root_attaches_a_driver_exactly_once() {
        let mut table = MountTable::new(MountFlags::default());
        assert_eq!(table.resolve(&p("/System")).backing(), None);

        let backing = attached(&mut table, 0x5EC0, MountFlags::default());
        table.back_root(backing).expect("first backing attaches");
        assert_eq!(table.resolve(&p("/System")).backing(), Some(backing));

        // A second root volume is refused, and the first stays attached.
        let second = attached(&mut table, 0x5EC1, MountFlags::default());
        assert_eq!(table.back_root(second), Err(VfsError::AlreadyExists));
        assert_eq!(table.resolve(&p("/System")).backing(), Some(backing));
    }

    #[test]
    fn an_attachment_records_the_medium_the_device_reported() {
        let mut table = MountTable::new(MountFlags::default());
        for (raw, medium) in [
            (0x5ED0, Some(BlkMedium::of_transport(BlkTransport::Ata))),
            (0x5ED1, Some(BlkMedium::of_transport(BlkTransport::Nvme))),
            (0x5ED2, None),
        ] {
            table
                .attach(Attachment::new(handle(raw), medium, MountFlags::NOEXEC))
                .expect("attach");
            assert_eq!(
                table.attachment(handle(raw)).map(Attachment::medium),
                Some(medium),
                "unknown is recorded as unknown, never a plausible medium"
            );
        }
    }

    #[test]
    fn set_backing_attaches_a_driver_and_subtree_preserving_flags() {
        let mut table = MountTable::new(MountFlags::default());
        table
            .mount(p("/Users"), MountFlags::NOEXEC, None)
            .expect("mount /Users");

        let backing = attached(&mut table, 0x5701, MountFlags::default());
        table
            .set_backing(&p("/Users"), backing, alloc::vec!["Users".into()])
            .expect("first backing attaches");

        let mount = table.resolve(&p("/Users/alice"));
        assert_eq!(mount.backing(), Some(backing));
        // The layout's flags are kept by attaching a backing volume.
        assert_eq!(mount.flags(), MountFlags::NOEXEC);
        // The sub-mount is rooted at the volume's own `/Users` directory.
        assert_eq!(mount.backing_subtree(), &[String::from("Users")]);
    }

    #[test]
    fn set_backing_is_refused_for_an_unknown_mount_or_a_second_backing() {
        let mut table = MountTable::new(MountFlags::default());
        let backing = attached(&mut table, 0x5702, MountFlags::default());
        // No mount covers exactly `/Storage` yet.
        assert_eq!(
            table.set_backing(&p("/Storage"), backing, Vec::new()),
            Err(VfsError::NotFound)
        );

        table
            .mount(p("/Storage"), MountFlags::default(), None)
            .expect("mount /Storage");
        table
            .set_backing(&p("/Storage"), backing, Vec::new())
            .expect("first backing attaches");
        // A second backing is refused, and the first stays attached.
        let other = attached(&mut table, 0x5703, MountFlags::READ_ONLY);
        assert_eq!(
            table.set_backing(&p("/Storage"), other, Vec::new()),
            Err(VfsError::AlreadyExists)
        );
        let mount = table.resolve(&p("/Storage"));
        assert_eq!(mount.backing(), Some(backing));
        // The refused backing left the first one's flags untouched.
        assert_eq!(mount.flags(), MountFlags::default());
    }

    #[test]
    fn a_runtime_mount_carries_its_own_template() {
        use super::super::perm::Mode;
        use tairix_kernel_sec::{GroupId, UserId};

        let mut table = MountTable::new(MountFlags::default());
        let volume = Attachment::new(
            handle(0x564F),
            Some(BlkMedium::of_transport(BlkTransport::Usb)),
            MountFlags::NOEXEC,
        );
        let template = Metadata::new(UserId(0), GroupId(0), Mode::from_bits(0o755));
        table
            .attach_at(volume, p("/Storage/usb1"), template.clone())
            .expect("runtime mount");

        let mount = table.resolve(&p("/Storage/usb1/file"));
        assert_eq!(mount.backing(), Some(volume.driver()));
        assert_eq!(mount.flags(), MountFlags::NOEXEC);
        assert_eq!(mount.template(), Some(&template));
        assert_eq!(table.attachment(volume.driver()), Some(volume));
        // Boot-layout mounts carry no template (their tree node is it).
        assert_eq!(table.resolve(&p("/other")).template(), None);
    }

    #[test]
    fn a_runtime_attach_changes_nothing_when_refused() {
        use super::super::perm::Mode;
        use tairix_kernel_sec::{GroupId, UserId};

        let mut table = MountTable::new(MountFlags::default());
        let template = Metadata::new(UserId(0), GroupId(0), Mode::from_bits(0o755));
        let first = Attachment::new(handle(0x5650), None, MountFlags::NOEXEC);
        table
            .attach_at(first, p("/Storage/usb1"), template.clone())
            .expect("runtime mount");
        let volumes = table.volumes();

        // The name is taken: the second volume is not attached either.
        let second = Attachment::new(handle(0x5651), None, MountFlags::NOEXEC);
        assert_eq!(
            table.attach_at(second, p("/Storage/usb1"), template.clone()),
            Err(VfsError::AlreadyExists)
        );
        // The volume is already attached: no second mount point appears.
        assert_eq!(
            table.attach_at(first, p("/Storage/usb2"), template),
            Err(VfsError::AlreadyExists)
        );
        assert_eq!(table.volumes(), volumes);
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn direct_children_lists_the_mounts_one_level_below() {
        let mut table = MountTable::new(MountFlags::default());
        let backed = attached(&mut table, 0x5704, MountFlags::NOEXEC);
        table
            .mount(p("/Storage/usb1"), MountFlags::default(), Some(backed))
            .expect("backed child");
        // A mount with no backing is a directory of the view all the same.
        table
            .mount(p("/Storage/policy"), MountFlags::NOEXEC, None)
            .expect("policy child");
        // A deeper mount is not a *direct* child of /Storage.
        table
            .mount(
                p("/Storage/usb1/nested"),
                MountFlags::default(),
                Some(backed),
            )
            .expect("grandchild");
        // An unrelated sibling subtree is not listed either.
        table
            .mount(p("/Users/vol"), MountFlags::default(), Some(backed))
            .expect("unrelated");

        let storage = p("/Storage");
        let children: Vec<_> = table
            .direct_children(&storage)
            .map(MountPoint::path)
            .collect();
        assert_eq!(children, [&p("/Storage/usb1"), &p("/Storage/policy")]);
        // Every mount here sits two levels deep, so the root itself has no
        // direct-child mounts.
        let root = Path::root();
        assert_eq!(table.direct_children(&root).count(), 0);
    }

    #[test]
    fn unmount_root_is_refused() {
        let mut table = MountTable::new(MountFlags::default());
        assert_eq!(table.unmount(&Path::root()), Err(VfsError::InvalidPath));
    }

    #[test]
    fn unmount_unknown_is_not_found() {
        let mut table = MountTable::new(MountFlags::default());
        assert_eq!(table.unmount(&p("/Storage/usb0")), Err(VfsError::NotFound));
    }

    #[test]
    fn mount_then_unmount_round_trips() {
        let mut table = MountTable::new(MountFlags::default());
        table
            .mount(p("/Storage/usb0"), MountFlags::NOEXEC, None)
            .expect("mount");
        assert_eq!(table.len(), 2);
        table.unmount(&p("/Storage/usb0")).expect("unmount");
        assert_eq!(table.len(), 1);
        assert!(!table.is_empty());
    }

    /// Every successful mutation reaches the `Mounts` notice, so a
    /// subscriber's places rail cannot silently stop matching the machine —
    /// and a *refused* mutation changed nothing, so it moves nothing.
    ///
    /// Monotone in the shared counter (observe, mutate, observe greater)
    /// rather than an exact step: the counter is process-global and the test
    /// binary runs tests concurrently.
    #[test]
    fn every_mutator_moves_the_mounts_notice_generation() {
        use super::super::perm::Mode;
        use tairix_abi::NoticeTopic;
        use tairix_kernel_sec::{GroupId, UserId};
        let generation =
            || crate::notice::generation(NoticeTopic::Mounts, &crate::seat::NULL_SEAT_REGISTRY);
        let mut table = MountTable::new(MountFlags::default());

        let moved = |label: &str, before: u64| {
            assert!(generation() > before, "{label} owes a notice");
        };

        let before = generation();
        let backing = attached(&mut table, 0x5F00, MountFlags::default());
        moved("attach", before);

        let before = generation();
        table
            .mount(p("/Storage/a"), MountFlags::NOEXEC, None)
            .expect("mount");
        moved("mount", before);

        let before = generation();
        table
            .mount_rebased(
                p("/System/Logs"),
                MountFlags::NOEXEC,
                Some(backing),
                alloc::vec![String::from("System"), String::from("Logs")],
            )
            .expect("sub-mount");
        moved("mount_rebased", before);

        let before = generation();
        table
            .attach_at(
                Attachment::new(handle(0x5F02), None, MountFlags::NOEXEC),
                p("/Storage/b"),
                Metadata::new(UserId(0), GroupId(0), Mode::from_bits(0o755)),
            )
            .expect("runtime mount");
        moved("attach_at", before);

        let before = generation();
        table.back_root(backing).expect("root backing");
        moved("back_root", before);

        let before = generation();
        table
            .set_backing(&p("/Storage/a"), backing, Vec::new())
            .expect("backing attaches");
        moved("set_backing", before);

        let before = generation();
        table.unmount(&p("/Storage/a")).expect("unmount");
        moved("unmount", before);

        let before = generation();
        let leaving = attached(&mut table, 0x5F01, MountFlags::NOEXEC);
        table.detach(leaving).expect("detach");
        moved("detach", before);

        // A refusal changed no mount, so it is not news. Asserted against
        // the *table* rather than the counter: the counter is process-global,
        // so "unchanged" is not a property one test can observe while
        // another's mounts are moving. Every mutator above bumps only after
        // its validation returns, which is what these refusals exercise.
        let occupied = table.resolve(&p("/Storage/b")).backing();
        let len = table.len();
        let shown = table.volumes();
        assert_eq!(
            table.mount(p("/Storage/b"), MountFlags::default(), None),
            Err(VfsError::AlreadyExists)
        );
        assert_eq!(table.unmount(&p("/Storage/gone")), Err(VfsError::NotFound));
        assert_eq!(table.back_root(backing), Err(VfsError::AlreadyExists));
        assert_eq!(table.detach(leaving), Err(VfsError::NotFound));
        assert_eq!(
            table.attach(Attachment::new(backing, None, MountFlags::default())),
            Err(VfsError::AlreadyExists)
        );
        assert_eq!(table.len(), len, "a refused mutation changes no mount");
        assert_eq!(table.volumes(), shown, "nor any attachment");
        assert_eq!(table.resolve(&p("/Storage/b")).backing(), occupied);
    }
}
