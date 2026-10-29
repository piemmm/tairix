//! Shared host-test fixtures for the filesystem seam and the on-disk
//! application-bundle spawn path: the nodes a test service opens, an
//! in-memory [`FilesystemService`] and a signed-bundle composer, used by the
//! `appspawn`, `aspace` and syscall-handler tests so the fake volume is
//! defined once.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tairix_abi::driver::filesystem::{DirVisit, MountFlags, NodeId, NodeKind};
use tairix_abi::rxe::{LoadHeader, RxePermission, Segment, LOAD_FLAG_PIE};
use tairix_abi::ProgramKind;
use tairix_abi::{
    BundleFileDigest, CapabilityId, CapabilityQuery, DirEntry, DriverHandle, Errno, FileId,
    FileKind, FileStat, NodeExpect, NodeTimes, OpenFlags, RealpathMode, SeekRegion, UnlinkFlags,
    ABI_VERSION_CURRENT, LOAD_MAGIC,
};
use tairix_appload::{AppError, AppLoader, AppLoaderConfig, Clock, LoadedApp};
use tairix_caps::CapabilitySet;
use tairix_itest_harness::app_image::{compose_signed_appinfo, AppManifestSource, PublisherSource};
use tairix_kernel_syscall::SYSCALL_TABLE_HASH;

use crate::appspawn::{AnchorVerifier, FsBundleStore};
use crate::aspace::OpenBacking;
use crate::fs::{
    Acting, At, Attachment, CreateMode, Cwd, FilesystemService, FinalLink, Listing, LookedUp,
    MountTable, NodeHandle, NodeRef, Opened, Path, Projection, ReaddirEntry, Start, VolumeNodes,
};
use crate::test_sink::TestSink;

extern crate std;
use std::collections::{BTreeMap, BTreeSet};
use tairix_sync::Once;
use tairix_util::fallible::concatenated;

/// The deterministic test signing seed; its derived public key is the trust
/// anchor the tests pin.
pub(crate) const SEED: [u8; 32] = [7u8; 32];

/// The deterministic test publisher seed. Distinct from [`SEED`] so the
/// fixture bundle is *delegated*, exercising the certificate the production
/// composer emits rather than the degenerate self-published shape.
pub(crate) const PUBLISHER_SEED: [u8; 32] = [9u8; 32];

/// A volume registration of the calling test's own: every one has an
/// instance no other test's shares, so the nodes pinned in it key locks and
/// watches apart from every other test's.
pub(crate) fn test_volume() -> Arc<VolumeNodes> {
    static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);
    Arc::new(VolumeNodes::new(
        NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed),
    ))
}

/// The root projection of a fresh mount table, shown with `flags` and backed
/// by a volume, as every projection a node is reached through is.
pub(crate) fn projection_with(flags: MountFlags) -> Arc<Projection> {
    let volume = DriverHandle::from_raw(1).expect("a non-zero handle");
    let mut table = MountTable::new(flags);
    table
        .attach(Attachment::new(volume, None, MountFlags::default()))
        .expect("a fresh table attaches the volume");
    table.back_root(volume).expect("the volume backs the root");
    Arc::clone(table.resolve(&Path::root()))
}

/// [`projection_with`] no flags.
pub(crate) fn test_projection() -> Arc<Projection> {
    projection_with(MountFlags::default())
}

/// What an `fs_open` of `path` answers when it reached the node `volume`
/// pinned as `pinned`, of `kind`.
pub(crate) fn opened_node(
    volume: &Arc<VolumeNodes>,
    pinned: u64,
    kind: NodeKind,
    path: &str,
) -> Opened {
    opened_node_in(volume, pinned, kind, path, test_projection())
}

/// As [`opened_node`], reached through `projection`.
pub(crate) fn opened_node_in(
    volume: &Arc<VolumeNodes>,
    pinned: u64,
    kind: NodeKind,
    path: &str,
    projection: Arc<Projection>,
) -> Opened {
    held_open(volume, pinned, kind, path, projection).expect("the heap holds the open")
}

/// [`opened_node_in`], refused [`Errno::OutOfMemory`] by a heap that cannot
/// hold it.
fn held_open(
    volume: &Arc<VolumeNodes>,
    pinned: u64,
    kind: NodeKind,
    path: &str,
    projection: Arc<Projection>,
) -> Result<Opened, Errno> {
    let oom = |_| Errno::OutOfMemory;
    let slot = volume.admit().map_err(oom)?;
    let named = concatenated(&[path]).ok_or(Errno::OutOfMemory)?;
    Ok(Opened::Node {
        node: NodeRef::new(slot, NodeId::from_raw(pinned), kind).map_err(oom)?,
        projection,
        kind,
        opened_as: Arc::try_new(named).map_err(|_| Errno::OutOfMemory)?,
        at_floor: false,
    })
}

/// The handle a descriptor on `opened` holds: its opener's own.
pub(crate) fn node_handle(opened: Opened) -> NodeHandle {
    match OpenBacking::opened(opened) {
        OpenBacking::Node(handle) => handle,
        other => panic!("a node was opened, not {other:?}"),
    }
}

/// The path `at` names in a fixture keyed by absolute path: an absolute one
/// as it stands, a relative one after the label of the directory it starts
/// from, `.` being that directory itself.
pub(crate) fn fixture_path(at: At<'_>) -> Result<String, Errno> {
    let held = |parts: &[&str]| concatenated(parts).ok_or(Errno::OutOfMemory);
    if at.path.starts_with('/') {
        return held(&[at.path]);
    }
    let base = match at.start {
        Start::Root => "/",
        Start::Dir(handle) => &*handle.opened_as,
        Start::Catalog(_) => return Err(Errno::NotFound),
    };
    if at.path == "." {
        return held(&[base]);
    }
    held(&[base.trim_end_matches('/'), "/", at.path])
}

/// An in-memory [`FilesystemService`] over a fixed file map. Read-only:
/// every mutating operation fails closed, mirroring the read paths the
/// bundle store actually exercises.
///
/// `read_calls` counts, per path, how many times a file's bytes were read,
/// so a test can prove a file (e.g. `Run`) is read from disk the expected
/// number of times. A node it opens is served by the path it was opened as:
/// nothing in the map is ever renamed, though a swapped name resolves
/// elsewhere.
pub(crate) struct MemFs {
    pub(crate) files: BTreeMap<String, Vec<u8>>,
    volume: Arc<VolumeNodes>,
    /// What every node is reached through.
    projection: Arc<Projection>,
    read_calls: std::sync::Mutex<BTreeMap<String, usize>>,
    stat_error: Option<Errno>,
    /// Paths the fixture reports as symbolic links rather than regular
    /// files, so the bundle store's refusal of a link can be exercised.
    links: BTreeSet<String>,
    /// Whether every file is served as a `noexec` mount serves it.
    noexec: bool,
    /// A name and the target it names a link to once first resolved.
    swap: Option<(String, String)>,
    /// Whether that swap has happened.
    swapped: AtomicBool,
}

/// A path-keyed fixture's stat of a node of `kind` and `size`: it holds no
/// node identity and one name per entry, the answers its listing gives.
fn fixture_stat(kind: FileKind, size: u64) -> FileStat {
    let (mode, nlink) = if kind == FileKind::Directory {
        (0o755, 2)
    } else {
        (0o644, 1)
    };
    FileStat {
        kind,
        nlink,
        size,
        allocated: size,
        mode,
        uid: 0,
        gid: 0,
        id: FileId::NONE,
        times: NodeTimes::default(),
        content_gen: 0,
    }
}

impl MemFs {
    pub(crate) fn new(files: &[(&str, &[u8])]) -> Self {
        Self {
            files: files
                .iter()
                .map(|(path, bytes)| ((*path).to_string(), bytes.to_vec()))
                .collect(),
            volume: test_volume(),
            projection: test_projection(),
            // Every file tallied from the start, so a read takes nothing from
            // the heap.
            read_calls: std::sync::Mutex::new(
                files
                    .iter()
                    .map(|(path, _)| ((*path).to_string(), 0))
                    .collect(),
            ),
            stat_error: None,
            links: BTreeSet::new(),
            noexec: false,
            swap: None,
            swapped: AtomicBool::new(false),
        }
    }

    /// Serve every file as a `noexec` mount does: readable as data, refused
    /// as a program.
    pub(crate) fn with_noexec(mut self) -> Self {
        self.noexec = true;
        self.projection = projection_with(MountFlags::NOEXEC);
        self
    }

    /// Report `path` as a symbolic link. Its bytes stay in the map, so a
    /// reader that fails to check the kind would happily return them —
    /// which is exactly what the bundle store must not do.
    pub(crate) fn with_link(mut self, path: &str) -> Self {
        self.links.insert(path.to_string());
        self
    }

    /// Make [`FilesystemService::stat`] and [`FilesystemService::open`] of
    /// a node that *exists* fail with `errno` instead of resolving, modelling
    /// a bundle that is present on the volume but the caller may not inspect
    /// (a permission denial). A genuinely absent path still reports
    /// [`Errno::NotFound`], so the spawn probe's absent-vs-present-but-refused
    /// distinction can be exercised.
    pub(crate) fn with_stat_error(mut self, errno: Errno) -> Self {
        self.stat_error = Some(errno);
        self
    }

    /// Once `name` is first resolved, have it name a link to `target`
    /// instead, as a name swapped between two calls would; a node opened
    /// before the swap keeps reading its own bytes.
    pub(crate) fn with_swap(mut self, name: &str, target: &str) -> Self {
        self.swap = Some((name.to_string(), target.to_string()));
        self
    }

    /// The target the name `path` names a link to, once it was swapped.
    fn swapped_to(&self, path: &str) -> Option<String> {
        let (name, target) = self.swap.as_ref()?;
        (name == path && self.swapped.load(Ordering::Relaxed)).then(|| target.clone())
    }

    /// `path` was resolved: a swap of that name happens now.
    fn resolved(&self, path: &str) {
        if self.swap.as_ref().is_some_and(|(name, _)| name == path) {
            self.swapped.store(true, Ordering::Relaxed);
        }
    }

    /// What `path` names in the map, as a stat that follows no swap reports
    /// it.
    fn entry_stat(&self, path: &str) -> Result<FileStat, Errno> {
        if let Some(body) = self.files.get(path) {
            let kind = if self.links.contains(path) {
                FileKind::Symlink
            } else {
                FileKind::Regular
            };
            return Ok(fixture_stat(kind, body.len() as u64));
        }
        if self.is_dir(path) {
            return Ok(fixture_stat(FileKind::Directory, 0));
        }
        Err(Errno::NotFound)
    }

    /// What an open of the node `path` names, of `kind`, answers, through a
    /// projection as `noexec` as the fixture is.
    fn opened(&self, kind: NodeKind, path: &str) -> Result<Opened, Errno> {
        held_open(&self.volume, 1, kind, path, Arc::clone(&self.projection))
    }

    /// How many times the bytes of `path` were read.
    pub(crate) fn read_calls(&self, path: &str) -> usize {
        self.read_calls
            .lock()
            .expect("read-call counter not poisoned")
            .get(path)
            .copied()
            .unwrap_or(0)
    }

    /// Read `path`'s bytes from `offset`, counting the read.
    fn serve(&self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        let mut tally = self
            .read_calls
            .lock()
            .expect("read-call counter not poisoned");
        match tally.get_mut(path) {
            Some(reads) => *reads += 1,
            None => {
                tally.insert(path.to_string(), 1);
            }
        }
        drop(tally);
        let bytes = self.files.get(path).ok_or(Errno::NotFound)?;
        let start = usize::try_from(offset).map_err(|_| Errno::OutOfRange)?;
        if start >= bytes.len() {
            return Ok(0);
        }
        let read = buf.len().min(bytes.len() - start);
        buf[..read].copy_from_slice(&bytes[start..start + read]);
        Ok(read)
    }

    /// Whether `path` is a directory: a file lives beneath it.
    fn is_dir(&self, path: &str) -> bool {
        self.files.keys().any(|p| beneath(p, path).is_some())
    }

    /// The immediate children of `dir`, derived from the file paths.
    fn children(&self, dir: &str) -> Result<Vec<ReaddirEntry>, Errno> {
        let mut out: Vec<ReaddirEntry> = Vec::new();
        for (path, body) in &self.files {
            let Some(rest) = beneath(path, dir) else {
                continue;
            };
            let (kind, name, size) = match rest.split_once('/') {
                Some((first, _)) => (FileKind::Directory, first, 0),
                None => (FileKind::Regular, rest, body.len() as u64),
            };
            if !out.iter().any(|e| e.name == name) {
                let name = concatenated(&[name]).ok_or(Errno::OutOfMemory)?;
                out.try_reserve(1).map_err(|_| Errno::OutOfMemory)?;
                out.push(ReaddirEntry {
                    kind,
                    size,
                    allocated: size,
                    modified: tairix_abi::time::Time64::UNIX_EPOCH,
                    // A path-keyed fixture has no node objects to identify
                    // and holds no second name for one — the same answers its
                    // `stat` gives.
                    id: FileId::NONE,
                    nlink: 1,
                    name,
                    content_gen: 0,
                });
            }
        }
        Ok(out)
    }
}

/// What of `path` lies beneath the directory `dir`, if it lies there.
fn beneath<'p>(path: &'p str, dir: &str) -> Option<&'p str> {
    path.strip_prefix(dir.strip_suffix('/').unwrap_or(dir))?
        .strip_prefix('/')
}

impl FilesystemService for MemFs {
    fn open(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        at: At<'_>,
        flags: OpenFlags,
        _mode: CreateMode,
    ) -> Result<Opened, Errno> {
        // Read-only fixture: a read open of an existing file resolves, any
        // mutating open fails closed like every other mutating operation.
        if flags.contains(OpenFlags::WRITE) || flags.contains(OpenFlags::CREATE) {
            return Err(Errno::NotImplemented);
        }
        let named = fixture_path(at)?;
        let target = self.swapped_to(&named);
        self.resolved(&named);
        let is_link = target.is_some() || self.links.contains(&named);
        let path = match target {
            Some(target) if !flags.contains(OpenFlags::NO_FOLLOW) => target,
            _ if is_link && flags.contains(OpenFlags::NO_FOLLOW) => {
                // A link holds no bytes; a resolve-only open names it.
                if flags.is_read() || flags.is_write() {
                    return Err(Errno::LinkLoop);
                }
                return self.opened(NodeKind::Symlink, &named);
            }
            // A link the fixture keeps no target for dangles.
            _ if is_link => return Err(Errno::NotFound),
            _ => named,
        };
        let kind = if self.files.contains_key(&path) {
            NodeKind::RegularFile
        } else if self.is_dir(&path) {
            NodeKind::Directory
        } else {
            return Err(Errno::NotFound);
        };
        if let Some(err) = self.stat_error {
            return Err(err);
        }
        self.opened(kind, &path)
    }

    fn read_node(
        &self,
        node: &NodeHandle,
        _who: &Acting<'_>,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, Errno> {
        self.serve(&node.opened_as, offset, buf)
    }

    fn write_node(
        &self,
        _node: &NodeHandle,
        _who: &Acting<'_>,
        _offset: u64,
        _append: bool,
        _data: &[u8],
    ) -> Result<usize, Errno> {
        Err(Errno::NotImplemented)
    }

    fn truncate_node(
        &self,
        _node: &NodeHandle,
        _who: &Acting<'_>,
        _size: u64,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    fn punch_node(
        &self,
        _node: &NodeHandle,
        _who: &Acting<'_>,
        _offset: u64,
        _len: u64,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    fn seek_region_node(
        &self,
        _node: &NodeHandle,
        _who: &Acting<'_>,
        _offset: u64,
        _region: SeekRegion,
    ) -> Result<u64, Errno> {
        Err(Errno::NotImplemented)
    }

    fn stat_node(&self, node: &NodeHandle, _who: &Acting<'_>) -> Result<FileStat, Errno> {
        self.entry_stat(&node.opened_as)
    }

    fn readdir_node(
        &self,
        node: &NodeHandle,
        _who: &Acting<'_>,
        at: &mut Listing,
        each: &mut dyn FnMut(&DirEntry<'_>) -> DirVisit,
    ) -> Result<(), Errno> {
        let children = self.children(&node.opened_as)?;
        if children.is_empty() {
            return Err(Errno::NotFound);
        }
        crate::fs::listing::serve_fixed(&children, at, each);
        Ok(())
    }

    fn lookup_entries_node(
        &self,
        _node: &NodeHandle,
        _who: &Acting<'_>,
        _names: &[&[u8]],
    ) -> Result<LookedUp, Errno> {
        Err(Errno::NotImplemented)
    }

    fn sync_node(&self, _node: &NodeHandle, _who: &Acting<'_>) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    fn chdir(&self, _uid: u32, _caps: &dyn CapabilityQuery, at: At<'_>) -> Result<Cwd, Errno> {
        let path = fixture_path(at)?;
        if !self.is_dir(&path) {
            return Err(Errno::NotFound);
        }
        Ok(Cwd::Dir(node_handle(opened_node(
            &self.volume,
            1,
            NodeKind::Directory,
            &path,
        ))))
    }

    fn canonical_of(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        start: Start<'_>,
    ) -> Result<String, Errno> {
        // Nothing in the map is ever renamed, so a directory's label names it.
        fixture_path(At { start, path: "." })
    }

    fn read_program(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        at: At<'_>,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, Errno> {
        if self.noexec {
            return Err(Errno::PermissionDenied);
        }
        let path = fixture_path(at)?;
        let target = self.swapped_to(&path);
        self.resolved(&path);
        self.serve(target.as_deref().unwrap_or(&path), offset, buf)
    }

    fn readdir_bounded(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        at: At<'_>,
        _final_link: FinalLink,
        limit: usize,
    ) -> Result<Vec<ReaddirEntry>, Errno> {
        let children = self.children(&fixture_path(at)?)?;
        if children.is_empty() {
            return Err(Errno::NotFound);
        }
        if children.len() > limit {
            return Err(Errno::LimitExceeded);
        }
        Ok(children)
    }

    fn stat(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        at: At<'_>,
        final_link: FinalLink,
    ) -> Result<FileStat, Errno> {
        let path = fixture_path(at)?;
        let target = self.swapped_to(&path);
        self.resolved(&path);
        if let Some(target) = target {
            return match final_link {
                FinalLink::Keep => Ok(fixture_stat(FileKind::Symlink, target.len() as u64)),
                FinalLink::Follow => self.entry_stat(&target),
            };
        }
        // A configured refusal shadows an *existing* node only: a present
        // bundle the caller may not inspect fails with the given errno, while
        // a genuinely absent path still reports absence.
        if let Some(err) = self.stat_error {
            if self.files.contains_key(&path) || self.is_dir(&path) {
                return Err(err);
            }
        }
        self.entry_stat(&path)
    }

    fn symlink(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        _target: &str,
        _at: At<'_>,
    ) -> Result<(), Errno> {
        // The fixture's file map has no link object type; it refuses rather
        // than approximating one with a regular file holding a path.
        Err(Errno::NotSupported)
    }

    fn readlink(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        _at: At<'_>,
    ) -> Result<String, Errno> {
        Err(Errno::NotSupported)
    }

    fn realpath(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        _at: At<'_>,
        _mode: RealpathMode,
    ) -> Result<String, Errno> {
        Err(Errno::NotSupported)
    }

    fn link(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        _existing: At<'_>,
        _new: At<'_>,
        _existing_link: FinalLink,
    ) -> Result<(), Errno> {
        // The fixture's file map holds one name per entry; a second name
        // for one node is not something it can represent.
        Err(Errno::NotSupported)
    }

    fn sync_all(&self, _uid: u32, _caps: &dyn CapabilityQuery) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    fn mkdir(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        _at: At<'_>,
        _mode: CreateMode,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    fn unlink(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        _at: At<'_>,
        _flags: UnlinkFlags,
        _expect: Option<&NodeExpect>,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    fn rename(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        _src: At<'_>,
        _dst: At<'_>,
        _flags: tairix_abi::RenameFlags,
        _expect: Option<&NodeExpect>,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }

    fn set_mode(
        &self,
        _uid: u32,
        _caps: &dyn CapabilityQuery,
        _at: At<'_>,
        _mode: u32,
        _final_link: FinalLink,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }
}

/// A minimal valid single-segment PIE `rxe` whose CFI tag is the kernel's
/// compiled-in syscall-table hash — exactly what the load gate accepts
/// (mirrors `crate::spawn`'s `tiny_image` fixture, retagged).
pub(crate) fn tiny_run() -> Vec<u8> {
    let seg = Segment {
        vaddr: 0x1000,
        file_offset: (LoadHeader::WIRE_LEN + Segment::WIRE_LEN) as u64,
        file_size: 4,
        mem_size: 4096,
        permission: RxePermission::ReadExecute,
    };
    let header = LoadHeader {
        magic: LOAD_MAGIC,
        abi_version: ABI_VERSION_CURRENT,
        flags: LOAD_FLAG_PIE,
        segment_count: 1,
        needed_count: 0,
        entry: 0x1000,
        cfi_tag: SYSCALL_TABLE_HASH,
    };
    let mut rxe = Vec::new();
    rxe.extend_from_slice(&header.to_le_bytes());
    rxe.extend_from_slice(&seg.to_le_bytes());
    rxe.extend_from_slice(&[0x13, 0x00, 0x00, 0x00]);
    rxe
}

/// Compose a signed `ps` bundle (manifest + `Run` + one help document) in a
/// [`MemFs`] under `/System/Commands/ps.app`, returning the filesystem, the
/// signer's public key, and the `Run` bytes.
///
/// The `AppInfo` is composed and signed by the **same** host composer the
/// image build uses, so the kernel store/verifier and the composer can
/// never drift. Like every bundle that build plants, it is *delegated*: the
/// publisher certifies [`SEED`]'s key, so the load gate's certificate check
/// is on the path these tests take.
pub(crate) fn composed_bundle(caps: Vec<CapabilityId>) -> (MemFs, [u8; 32], Vec<u8>) {
    composed_bundle_signed_by(&SEED, caps)
}

/// The same bundle composed with an arbitrary build signing `seed` under the
/// one [`PUBLISHER_SEED`] publisher, so a test can model the same app
/// re-signed for a later release.
pub(crate) fn composed_bundle_signed_by(
    seed: &[u8; 32],
    caps: Vec<CapabilityId>,
) -> (MemFs, [u8; 32], Vec<u8>) {
    composed_bundle_published_by(seed, PublisherSource::Delegating(&PUBLISHER_SEED), caps)
}

/// The same bundle again, with the publisher story stated verbatim, so a test
/// can compose a validly-signed manifest that makes a publisher claim the
/// load gate has to refuse on the certificate alone.
pub(crate) fn composed_bundle_published_by(
    seed: &[u8; 32],
    publisher: PublisherSource<'_>,
    caps: Vec<CapabilityId>,
) -> (MemFs, [u8; 32], Vec<u8>) {
    let run = tiny_run();
    let help = b"# ps\n";
    let manifest = AppManifestSource {
        id: "os.tairix.ps".to_string(),
        name: "ps".to_string(),
        title: None,
        version: "1.0".to_string(),
        kind: ProgramKind::Command,
        capabilities: caps,
        associations: Vec::new(),
        browses: Vec::new(),
        library: None,
        library_icon: None,
        purpose: None,
        author: None,
        icon_bar: true,
        multi_instance: false,
        writes_documents: false,
        saver: tairix_itest_harness::app_image::SaverDeclaration::default(),
    };
    let composed = compose_signed_appinfo(
        seed,
        publisher,
        &manifest,
        SYSCALL_TABLE_HASH,
        &[
            BundleFileDigest {
                path: "Help/en-US/ps.md",
                bytes: help,
            },
            BundleFileDigest {
                path: "Run",
                bytes: &run,
            },
        ],
    )
    .expect("composes");
    let fs = MemFs::new(&[
        ("/System/Commands/ps.app/AppInfo", composed.bytes.as_slice()),
        ("/System/Commands/ps.app/Run", run.as_slice()),
        ("/System/Commands/ps.app/Help/en-US/ps.md", help.as_slice()),
    ]);
    (fs, composed.signer_pubkey, run)
}

/// A `CapabilityQuery` granting nothing — the mock filesystem enforces
/// no permissions.
pub(crate) struct NoCaps;
impl CapabilityQuery for NoCaps {
    fn holds(&self, _cap: CapabilityId) -> bool {
        false
    }
}

/// A fixed clock: this fixture asserts load *outcomes*, not timing, so a
/// constant reading (zero-length phases) is sufficient.
struct NullClock;
impl Clock for NullClock {
    fn now_ns(&self) -> u64 {
        0
    }
}

/// Compose a signed screensaver bundle at `bundle` (`<store>/<Name>.app`)
/// signed by `seed` and self-published, declaring `feeds`, returning the
/// filesystem and the signer's public key.
pub(crate) fn composed_saver(
    bundle: &str,
    seed: &[u8; 32],
    feeds: tairix_abi::saver::SaverFeeds,
) -> (MemFs, [u8; 32]) {
    let run = tiny_run();
    let preview = b"picture";
    let manifest = AppManifestSource {
        id: "os.tairix.waves".to_string(),
        name: "waves".to_string(),
        title: Some("Waves".to_string()),
        version: "1.0".to_string(),
        kind: ProgramKind::Screensaver,
        capabilities: Vec::new(),
        associations: Vec::new(),
        browses: Vec::new(),
        library: None,
        library_icon: None,
        purpose: None,
        author: None,
        icon_bar: true,
        multi_instance: false,
        writes_documents: false,
        saver: tairix_itest_harness::app_image::SaverDeclaration {
            feeds,
            ..Default::default()
        },
    };
    let composed = compose_signed_appinfo(
        seed,
        PublisherSource::SelfPublished,
        &manifest,
        SYSCALL_TABLE_HASH,
        &[
            BundleFileDigest {
                path: "Resources/Preview.png",
                bytes: preview,
            },
            BundleFileDigest {
                path: "Run",
                bytes: &run,
            },
        ],
    )
    .expect("composes");
    let fs = MemFs::new(&[
        (&format!("{bundle}/AppInfo"), composed.bytes.as_slice()),
        (&format!("{bundle}/Run"), run.as_slice()),
        (
            &format!("{bundle}/Resources/Preview.png"),
            preview.as_slice(),
        ),
    ]);
    (fs, composed.signer_pubkey)
}

/// Run the full `tairix_appload` gate over `fs`, exactly as the spawn
/// path does.
pub(crate) fn gate_load(fs: &MemFs, anchor: [u8; 32]) -> Result<LoadedApp, AppError> {
    gate_load_at(
        fs,
        anchor,
        "/System/Commands/ps.app",
        tairix_appload::LoadMode::Ordinary,
    )
}

/// The gate over `fs` for `bundle`, spawned in `mode`.
pub(crate) fn gate_load_at(
    fs: &MemFs,
    anchor: [u8; 32],
    bundle: &str,
    mode: tairix_appload::LoadMode,
) -> Result<LoadedApp, AppError> {
    let sink: &'static TestSink = crate::forever(Box::new(TestSink::new()));
    let store = FsBundleStore::new(fs, 1000, &NoCaps);
    let verifier = AnchorVerifier::new(anchor);
    let clock = NullClock;
    let loader = AppLoader::new(AppLoaderConfig {
        accepted_abi_version: ABI_VERSION_CURRENT,
        syscall_table_hash: SYSCALL_TABLE_HASH,
        store: &store,
        verifier: &verifier,
        clock: &clock,
        sink,
    });
    loader.load(bundle, &CapabilitySet::from_words([u64::MAX; 4]), mode)
}

/// A verified [`LoadedApp`] straight from the shared load gate, over the
/// composed in-memory test bundle.
///
/// Verified once and shared: the gate's signature round is the same work on
/// every call and the result is immutable behind its `Arc`, so a caller that
/// only needs *a* verified app pays nothing. A test that needs two
/// independent loads composes and gates the bundle itself.
pub(crate) fn verified_app() -> Arc<LoadedApp> {
    static APP: Once<Arc<LoadedApp>> = Once::new();
    APP.call_once_infallible(|| {
        let (fs, anchor, _run) = composed_bundle(Vec::new());
        Arc::new(gate_load(&fs, anchor).expect("the composed bundle verifies"))
    })
    .expect("a fresh cell")
    .clone()
}
