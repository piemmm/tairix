//! Image volumes (`plans/DISK-IMAGES.md`): the volume a disk-image file holds,
//! attached for the principal that handed the kernel the file.
//!
//! The file is the volume's device: whole 512-byte blocks, read and written
//! through the caller's own descriptor of it, held through the volume it lies
//! on rather than a bind it was opened through, as the caller's identity
//! frozen at the attach. The kernel recognises it with the probe every volume goes
//! through, locks it, publishes the volume's node under the image-volume node
//! for `devmgr` to load the format's driver, and waits for that driver to serve
//! or refuse it. The volume is presented as its attacher's, never executes, and
//! leaves when its owner ejects it, when the attacher's login session ends, or
//! with the volume its file lies on.
//!
//! An archive (`plans/ARCHIVES.md`) attaches the same way: the probe finds no
//! disk format in the file's whole blocks and an archive in its exact bytes,
//! so the volume is the file's every byte, read only, and a writable one is
//! written through work files beside it and a commit that replaces it.
//!
//! The registry is a spin lock: an image whose file's volume left is retired
//! from contexts that may not park.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use tairix_abi::blkio::{BlkForm, BlkMedium, BlkTransport};
use tairix_abi::driver::block::{Block, BlockGeometry};
use tairix_abi::driver::filesystem::{MountFlags, VolumeTitle};
use tairix_abi::fsserve::{ServeOffer, SERVED_IDLE_NS};
use tairix_abi::imageattach::{ImageAccess, ImageAttach, ImageAttachReply, ImageRefusal};
use tairix_abi::netattach::AttachReply;
use tairix_abi::sysinfo::{MountAvailability, VolumeClass};
use tairix_abi::volume::{
    ArchiveName, DetachMode, FormatName, ViewBindRequest, VolumeFsType, VOLUME_NAME_MAX,
};
use tairix_abi::{
    CapabilityId, CapabilityQuery, DriverError, DriverHandle, Errno, FileId, FileStat,
    HwDeviceClass, HwNode, LockRange, ProcId, FS_NAME_MAX, HW_NODE_ROOT_ID,
};
use tairix_bytedev::ByteDevice;
use tairix_caps::CapabilitySet;
use tairix_crypto::Sha256Stream;
use tairix_fsprobe::name::{candidates, chosen, NameBase};
use tairix_fsprobe::{ProbedVolume, Recognised};
use tairix_inline::{ArrayString, ArrayVec};
use tairix_kernel_core::filelock::{self, Held, LockedNode, OwnerId, Request};
use tairix_kernel_core::fs::served::{shows_life, ServedFs, ServerActivity};
use tairix_kernel_core::fs::{
    Access, Acting, AnswerSink, At, Authority, FinalLink, ImageDirectory, ImageSource, Judged,
    NodeHandle, Principal, ServeChannel, Start, Titling, VolumeNames, VolumeNodes,
};
use tairix_kernel_core::session_reaper::{self, SessionObserver};
use tairix_kernel_core::waitq::{
    nearest_timed_deadline, park_calling_task, volume_attach_wake, wait_arch, WaitQueueArch,
    VOLUME_ATTACH_WAITQ,
};
use tairix_kernel_core::{
    Attachment, BindsWithin, DriverLock, FilesystemService, SleepLock, Vfs, VfsError,
};
use tairix_kernel_sec::ProcessId;
use tairix_log::{log, Event, EventId, Field, FieldValue, Level, Sink};
use tairix_sync::SpinLock;
use tairix_util::fallible::concatenated;

use super::{
    audit_retitled, mount_flags, probed_title, reattach, record, record_retired, records_owners,
    refusal_level, refuse_retitle, storage_path, take_down, volume_node, write_held_title, GivenUp,
    RuntimeVolumeService, Wiring, Writes, NAME_IN_USE,
};
use crate::hwtree_node_ids::IMAGE_VOLUMES_NODE_ID;
use crate::served_volume::{Granted, ServedVolume};
use crate::system_mount::{
    cached, publish_volume_identity, FS_SERVICE, LATE_FILESYSTEM, VOLUME_FOREST,
};
use crate::volume_policy::{presentation_word, Presented, PresentedFs};

/// Audit event: an image volume's node was published; it is mounted once its
/// format's driver serves it.
const IMAGE_VOLUME_AWAITING: EventId = EventId(4320);
/// Audit event: an image volume was served and mounted for its attacher.
const IMAGE_VOLUME_ATTACHED: EventId = EventId(4321);
/// Audit event: an image attach was refused.
const IMAGE_ATTACH_REFUSED: EventId = EventId(4322);
/// Audit event: an image volume was detached: ejected by its owner or the
/// mount authority, at the end of its attacher's session, or ahead of the
/// volume its file lies on.
const IMAGE_VOLUME_DETACHED: EventId = EventId(4323);
/// Audit event: an image volume's detach was refused.
const IMAGE_DETACH_REFUSED: EventId = EventId(4324);
/// Audit event: an image volume left with its file's volume, or its driver
/// was lost.
const IMAGE_VOLUME_RETIRED: EventId = EventId(4325);

/// A caller's authority short of the mount authority: what an image's target
/// is bound as, so the bind keeps an owner's rules — an empty directory of the
/// caller's own, never the catalog — whoever attaches.
struct ShortOfMount<'a>(&'a dyn CapabilityQuery);

impl CapabilityQuery for ShortOfMount<'_> {
    fn holds(&self, cap: CapabilityId) -> bool {
        cap != CapabilityId::FS_MOUNT && self.0.holds(cap)
    }
}

/// Where image volumes' mount handles are numbered from (`"IMG"`): disjoint
/// from every other volume's.
const IMAGE_HANDLE_BASE: u64 = 0x494D_4700_0000_0000;

/// The domain-separation label of an image volume's identity.
const IDENTITY_CONTEXT: &[u8] = b"tairix-image-volume/v1";

/// The block an image is read and written in: the smallest every format's
/// structures are laid out in.
const IMAGE_BLOCK: u32 = 512;

/// The image volumes one principal may hold: each costs a driver process.
pub(super) const IMAGE_VOLUMES_PER_PRINCIPAL: usize = 16;

/// How deep image volumes may nest: an image whose file lies on an image
/// volume this deep is refused.
pub(super) const IMAGE_NESTING_MAX: u8 = 4;

/// How long an attach waits for its volume's driver to first touch the
/// volume. After that the driver's liveness bounds the wait, as every served
/// call's.
const IMAGE_DRIVER_START_NS: u64 = 30_000_000_000;

#[path = "image_archive.rs"]
mod archive;

/// A disk-image file as the block device its volume is served over: whole
/// blocks of it, read and written as the identity frozen into its hold; or an
/// archive file, its every byte, the last block read to the file's end and
/// then zeros.
pub(crate) struct ImageBlock {
    file: NodeHandle,
    uid: u32,
    caps: CapabilitySet,
    /// The file's length.
    len: u64,
    blocks: u64,
    writable: bool,
    form: BlkForm,
    shape: Shape,
    /// The name an archive was attached by.
    archive_name: Option<ArchiveName>,
}

/// What an image file is read as.
enum Shape {
    /// A disk image: its whole blocks, a trailing part-block outside them.
    Disk,
    /// An archive: its exact bytes, never written in place, and for a
    /// writable one its work files.
    Archive(Option<alloc::boxed::Box<archive::WorkFiles>>),
}

impl ImageBlock {
    /// The device `file` is, written only when `writable`.
    ///
    /// # Errors
    ///
    /// [`Errno::PermissionDenied`] for a hold acting as no identity frozen
    /// into it, or one bounded by a grant's ceiling, which byte operations do
    /// not enforce; the file's `stat` refusal.
    fn new(file: NodeHandle, writable: bool) -> Result<(Self, FileStat), Errno> {
        let (uid, caps) = match &file.authority {
            Authority::Frozen(frozen) if frozen.grant.is_none() => (frozen.uid, frozen.caps),
            Authority::Frozen(_) | Authority::Holder => return Err(Errno::PermissionDenied),
        };
        let mut block = Self {
            file,
            uid,
            caps,
            len: 0,
            blocks: 0,
            writable,
            form: BlkForm::Unspecified,
            shape: Shape::Disk,
            archive_name: None,
        };
        let stat = FS_SERVICE.stat_node(&block.file, &block.acting())?;
        block.len = stat.size;
        block.blocks = stat.size / u64::from(IMAGE_BLOCK);
        Ok((block, stat))
    }

    /// Read the file as an archive: every byte of it, its last block to its
    /// end then zeros.
    fn read_as_archive(&mut self) {
        self.blocks = self.len.div_ceil(u64::from(IMAGE_BLOCK));
        self.shape = Shape::Archive(None);
    }

    fn acting(&self) -> Acting<'_> {
        Acting {
            uid: self.uid,
            caps: &self.caps,
            granted: false,
        }
    }

    /// Where in the file `len` bytes of whole blocks at `lba` begin.
    fn offset(&self, lba: u64, len: usize) -> Result<u64, DriverError> {
        let block = IMAGE_BLOCK as usize;
        if len == 0 || !len.is_multiple_of(block) {
            return Err(DriverError::LengthOutOfRange);
        }
        match lba.checked_add((len / block) as u64) {
            Some(end) if end <= self.blocks => Ok(lba * u64::from(IMAGE_BLOCK)),
            _ => Err(DriverError::LengthOutOfRange),
        }
    }
}

impl Block for ImageBlock {
    fn geometry(&self) -> Result<BlockGeometry, DriverError> {
        Ok(BlockGeometry {
            block_size: IMAGE_BLOCK,
            block_count: self.blocks,
        })
    }

    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DriverError> {
        let start = self.offset(lba, buf.len())?;
        // Only an archive's last block runs past the file, and reads as zeros
        // there.
        let held = usize::try_from(self.len.saturating_sub(start))
            .map_or(buf.len(), |left| left.min(buf.len()));
        let (file, past) = buf.split_at_mut(held);
        let mut filled = 0;
        while filled < file.len() {
            let at = start + filled as u64;
            match FS_SERVICE.read_node(&self.file, &self.acting(), at, &mut file[filled..]) {
                // The file shrank under its volume: what it no longer holds is
                // no block's content, and never reads as zeros.
                Ok(0) => return Err(DriverError::DeviceFault),
                Ok(read) => filled += read,
                Err(errno) => return Err(DriverError::from_errno(errno)),
            }
        }
        past.fill(0);
        Ok(())
    }

    fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), DriverError> {
        if !self.writable {
            return Err(DriverError::Unsupported);
        }
        let start = self.offset(lba, buf.len())?;
        let mut written = 0;
        while written < buf.len() {
            let at = start + written as u64;
            match FS_SERVICE.write_node(&self.file, &self.acting(), at, false, &buf[written..]) {
                Ok(0) => return Err(DriverError::DeviceFault),
                Ok(wrote) => written += wrote,
                Err(errno) => return Err(DriverError::from_errno(errno)),
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DriverError> {
        self.sync_directory().map_err(DriverError::from_errno)?;
        FS_SERVICE
            .sync_node(&self.file, &self.acting())
            .map_err(DriverError::from_errno)
    }

    fn medium(&self) -> Option<BlkMedium> {
        Some(image_medium(self.form))
    }
}

/// What an image laid out for `form` presents itself as.
const fn image_medium(form: BlkForm) -> BlkMedium {
    BlkMedium::of_transport(BlkTransport::Virtual).with_form(form)
}

/// The advisory lock an image volume holds over its whole file, so a second
/// writable attach, or a writer that locks, is turned away.
struct FileHold {
    owner: OwnerId,
    file: LockedNode,
}

impl FileHold {
    /// Lock all of `file` for an attach on `task`, exclusively when
    /// `exclusive`.
    ///
    /// # Errors
    ///
    /// [`Errno::Busy`] while another holder's lock is in the way.
    fn take(file: &NodeHandle, exclusive: bool, task: u64) -> Result<Self, Errno> {
        let hold = Self {
            owner: filelock::mint_owner(),
            file: LockedNode::of(&file.node),
        };
        let mode = if exclusive {
            Held::Exclusive
        } else {
            Held::Shared
        };
        lock_whole(hold.file, hold.owner, mode, task)?;
        Ok(hold)
    }
}

/// Lock all of `file` for `owner` as `held`, on behalf of `task`.
///
/// # Errors
///
/// [`Errno::Busy`] while another holder's lock is in the way, and
/// [`Errno::OutOfMemory`] when the kernel cannot record the lock.
fn lock_whole(file: LockedNode, owner: OwnerId, held: Held, task: u64) -> Result<(), Errno> {
    filelock::acquire(&Request {
        file,
        owner,
        // The kernel holds it for the attachment, which outlives the
        // attaching process, and the attachment bound is what bounds it.
        pid: ProcessId::KERNEL,
        task,
        range: LockRange::WHOLE,
        held,
        limit: u64::MAX,
        waiting: false,
    })
    .map(|wakes| wakes.wake())
    .map_err(|refusal| match refusal {
        filelock::Refusal::OutOfMemory => Errno::OutOfMemory,
        _ => Errno::Busy,
    })
}

impl Drop for FileHold {
    fn drop(&mut self) {
        for wakes in filelock::release_owner(self.owner) {
            wakes.wake();
        }
    }
}

/// Why a volume awaiting its driver will not be served.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Refusal {
    /// Its driver could not open it.
    Driver(DriverError),
    /// The catalog name asked for it was no longer free to show it under.
    Name(Errno),
    /// The kernel could not mount what the driver offered.
    Kernel(Errno),
}

/// Where an image volume stands.
enum Stage {
    /// Published; its attach waits for a driver to serve or refuse it.
    Awaiting,
    /// It will not be served; its attach takes it out.
    Refused(Refusal),
    /// Mounted as `name`, served over `server` while a driver serves it.
    Mounted {
        name: String,
        format: FormatName,
        server: Option<u64>,
    },
    /// Its file's volume left while it held writes it had not published: the
    /// stand-in answers until its owner ejects it.
    Lost { name: String },
}

/// One image volume.
struct ImageVolume {
    id: [u8; 16],
    owner: u32,
    /// The login sessions it was handed to: it is detached once every one
    /// has ended.
    sessions: Vec<ProcId>,
    /// The attaching task, waiting for the volume's driver while it attaches.
    task: u64,
    /// The registration its file is pinned through.
    file_volume: Arc<VolumeNodes>,
    /// The volume its file lies on.
    backing: Option<DriverHandle>,
    /// How many image volumes deep it lies, itself counted.
    depth: u8,
    served: Arc<ServedVolume<ImageBlock>>,
    /// Its node under the image volumes' parent, until withdrawn.
    node: Option<u32>,
    handle: DriverHandle,
    fstype: VolumeFsType,
    medium: BlkMedium,
    /// What the mount listing names it from: the file's name.
    source: String,
    /// What the mount listing titles it: its filesystem's label.
    title: VolumeTitle,
    /// The catalog name it must take, held for it while it awaits.
    asked: Option<String>,
    /// What its catalog name is made from otherwise.
    base: NameBase,
    presented: Presented,
    /// How writable its attach asked it to be: a first serve that would mount
    /// a `Writable` one read-only is refused.
    access: ImageAccess,
    /// The lock over its file, which an archive's commit moves to the file
    /// that replaces it.
    hold: Arc<SleepLock<FileHold>>,
    stage: Stage,
    custody: Custody,
}

/// Who acts on an image volume, and the teardown it owes.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct Custody {
    /// What holds it: nothing else acts on it until that ends.
    holder: Holder,
    /// Its file's volume left while it was held; the holder retires it as it
    /// lets go.
    retiring: bool,
    /// An attach is answering for it: no other attach takes it up, and no
    /// settling task takes it down, until that answer is delivered or undone.
    answering: bool,
    /// The teardown it owes, which a settling task does once nothing holds
    /// it: never the task that found it owed, which may be its driver's own.
    /// An attach that gave up takes its own volume back first, unless it is
    /// being killed or something holds the volume.
    owed: Owed,
    /// A settling task could not do what it owes, nothing else having let go
    /// of an image volume while it tried: the [`Registry::releases`] it
    /// failed at. It is tried again once that count moves on.
    stuck_at: Option<u64>,
}

impl Custody {
    /// Whether an operation holds it.
    fn claimed(self) -> bool {
        self.holder != Holder::Nobody
    }

    /// Whether a settling task may do what it owes now, `releases` claims
    /// having been let go.
    fn settleable(self, releases: u64) -> bool {
        self.owed != Owed::Nothing
            && !self.claimed()
            && !self.answering
            && self.stuck_at != Some(releases)
    }

    /// Let go of the hold taken `by` a holder when `since` claims had been let
    /// go, `releases` having been let go now, answering whether that counts as
    /// one. A settling task that failed does not: it waits for something else
    /// to let go, unless something already did while it tried.
    fn let_go(&mut self, by: Holder, since: u64, releases: u64) -> bool {
        self.holder = Holder::Nobody;
        if by == Holder::Settler && self.owed != Owed::Nothing {
            self.stuck_at = (releases == since).then_some(releases);
            return false;
        }
        by.releases()
    }
}

/// What holds an image volume.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
enum Holder {
    #[default]
    Nobody,
    /// An attach's serve, a detach, a retirement.
    Operation,
    /// A settling task doing what it owes.
    Settler,
    /// A settling task taking down what lies on the volume it settles.
    Clearing,
}

impl Holder {
    /// What holds each volume lying on one this holds while it is taken down.
    fn beneath(self) -> Self {
        match self {
            Self::Settler | Self::Clearing => Self::Clearing,
            Self::Nobody | Self::Operation => Self::Operation,
        }
    }

    /// Whether its letting go is something else letting go, to a settling task
    /// that failed: a settling task's own hold beneath the volume it settles
    /// is not, or a teardown failing beneath it would be retried at once,
    /// forever.
    fn releases(self) -> bool {
        self != Self::Clearing
    }
}

/// The teardown an image volume owes.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
enum Owed {
    #[default]
    Nothing,
    /// Its attach gave up, so no one was told its name: it is taken out.
    Abandoned,
    /// No login session it was handed to is open: it is detached, forced.
    Orphaned,
}

/// Where in `volumes`, owners with their volumes' custody, the first a
/// settling task may take up lies, `releases` claims having been let go: one
/// of an owner no settling task serves already, so one principal's slow
/// teardowns hold up only its own.
fn first_settleable<'a>(
    mut volumes: impl Iterator<Item = (u32, &'a Custody)> + Clone,
    releases: u64,
) -> Option<usize> {
    let mut serving = ArrayVec::<u32, SETTLERS>::new();
    for (owner, _) in volumes
        .clone()
        .filter(|(_, custody)| custody.holder == Holder::Settler)
    {
        if serving.try_push(owner).is_err() {
            break;
        }
    }
    volumes.position(|(owner, custody)| custody.settleable(releases) && !serving.contains(&owner))
}

/// How many tasks settle image volumes at once: each holds one. The reaper
/// settles on its own task until the notifier's serve.
const SETTLERS: usize = session_reaper::NOTIFIER_TASKS + 1;

impl ImageVolume {
    /// Whether its file can no longer be reached as it was attached: its
    /// registration retired, its volume registered afresh, or a stand-in
    /// answering for it.
    fn stranded(&self) -> bool {
        self.file_volume.is_retired()
            || self
                .backing
                .is_some_and(|backing| !reaches(LATE_FILESYSTEM.reach(backing), &self.file_volume))
    }

    /// The catalog name it holds, mounted or lost.
    fn name(&self) -> Option<&str> {
        match &self.stage {
            Stage::Mounted { name, .. } | Stage::Lost { name } => Some(name),
            Stage::Awaiting | Stage::Refused(_) => None,
        }
    }
}

/// Whether `registered`, what answers for a volume now, is the registration
/// `file_volume`'s nodes were reached through, with no stand-in answering.
fn reaches(registered: Option<(Arc<VolumeNodes>, bool)>, file_volume: &Arc<VolumeNodes>) -> bool {
    registered.is_some_and(|(nodes, stand_in)| !stand_in && Arc::ptr_eq(&nodes, file_volume))
}

/// What a principal has attached from one file already.
enum Attached {
    /// Its volume, as `reply` names it, and how the attach took it up.
    Mounted {
        reply: AttachReply,
        taken: Option<Taken>,
    },
    /// Its volume, which cannot be taken up now, for the cause named.
    Busy(&'static str),
}

/// An attach's taking up of a volume attached already.
#[derive(Copy, Clone, Debug)]
struct Taken {
    /// The served volume taken up.
    volume: u64,
    /// The attaching session's hold on it is this attach's own.
    added: bool,
}

/// The image volumes attached and attaching.
pub(super) struct ImageVolumes {
    registry: SpinLock<Registry>,
    next_handle: AtomicU64,
}

/// The image volumes, and how many claims on them have been let go, under one
/// guard.
struct Registry {
    /// In ascending order of their served volumes' ids.
    volumes: Vec<ImageVolume>,
    /// Claims let go that count as releases ([`Custody::let_go`]): a teardown
    /// stuck at an earlier count may succeed now.
    releases: u64,
}

impl ImageVolumes {
    /// An empty registry.
    pub(super) const fn new() -> Self {
        Self {
            registry: SpinLock::new(Registry {
                volumes: Vec::new(),
                releases: 0,
            }),
            next_handle: AtomicU64::new(IMAGE_HANDLE_BASE + 1),
        }
    }

    /// Whether the volume published as `id` is an image's.
    pub(super) fn holds(&self, id: &[u8; 16]) -> bool {
        self.registry.lock().volumes.iter().any(|v| v.id == *id)
    }

    /// Whether the volume published as `id` is still served as it was
    /// mounted: neither taken out nor unregistered, which taking it down does
    /// first.
    fn still_served(&self, id: &[u8; 16]) -> bool {
        self.registry
            .lock()
            .volumes
            .iter()
            .find(|v| v.id == *id)
            .is_some_and(|v| LATE_FILESYSTEM.reach(v.handle).is_some())
    }

    /// Whether served volume `volume` is an image's.
    pub(super) fn serves(&self, volume: u64) -> bool {
        position(&self.registry.lock().volumes, volume).is_some()
    }

    /// Whether an image volume other than `except`'s holds the catalog name
    /// `name`, or awaits its driver to take it.
    pub(super) fn reserves(&self, name: &str, except: Option<u64>) -> bool {
        self.registry.lock().volumes.iter().any(|v| {
            except != Some(v.served.id())
                && (v.name() == Some(name) || v.asked.as_deref() == Some(name))
        })
    }

    /// Whether image volume `volume` awaits a driver to serve it.
    pub(super) fn awaiting_server(&self, volume: u64) -> Result<(), Errno> {
        let registry = self.registry.lock();
        let volumes = &registry.volumes;
        let entry = position(volumes, volume)
            .map(|at| &volumes[at])
            .filter(|v| !v.served.retired())
            .ok_or(Errno::NotFound)?;
        match entry.stage {
            Stage::Awaiting | Stage::Mounted { server: None, .. } => Ok(()),
            Stage::Mounted {
                server: Some(_), ..
            } => Err(Errno::AlreadyExists),
            Stage::Refused(_) | Stage::Lost { .. } => Err(Errno::NotFound),
        }
    }

    /// Whether an image volume's file lies on the volume registered as `host`.
    pub(super) fn backed_by(&self, host: DriverHandle) -> bool {
        self.registry
            .lock()
            .volumes
            .iter()
            .any(|v| v.backing == Some(host))
    }

    /// How many image volumes deep a file on the volume `host` lies.
    fn depth_on(&self, host: Option<DriverHandle>) -> u8 {
        host.and_then(|host| {
            self.registry
                .lock()
                .volumes
                .iter()
                .find(|v| v.handle == host)
                .map(|v| v.depth)
        })
        .unwrap_or(0)
    }

    /// What the principal and file behind `id` have attached already. When
    /// `attach` would adopt it — a volume as writable as it asks — the volume
    /// is taken up for `session` too, living until that session has ended as
    /// well and owing no teardown it owed: the decision is made under the
    /// guard its teardown is claimed under, and the attach then answers for it
    /// until [`Self::answered`]. One owing its teardown is leaving to an
    /// attach that would not. How writable it is is read under the same
    /// guard, with no operation claiming the volume, so no resumed serve
    /// changes it meanwhile.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] when the session cannot be recorded; the
    /// reply's refusal of the name.
    fn attached_as(
        &self,
        id: &[u8; 16],
        session: ProcId,
        attach: &ImageAttach<'_>,
    ) -> Result<Option<Attached>, Errno> {
        let mut registry = self.registry.lock();
        let volumes = &mut registry.volumes;
        let Some(entry) = volumes.iter_mut().find(|v| v.id == *id) else {
            return Ok(None);
        };
        let read_only = entry.served.read_only();
        let adopting = attach.adopt && attach.access.accepts(read_only);
        let name = match &entry.stage {
            _ if entry.custody.answering => return Ok(Some(Attached::Busy("attach_in_progress"))),
            _ if entry.custody.claimed() => return Ok(Some(Attached::Busy("volume_busy"))),
            _ if !adopting && entry.custody.owed != Owed::Nothing => {
                return Ok(Some(Attached::Busy("volume_leaving")))
            }
            // What its writes were is lost until its owner ejects it.
            Stage::Lost { .. } => return Ok(Some(Attached::Busy("volume_lost"))),
            Stage::Awaiting | Stage::Refused(_) => {
                return Ok(Some(Attached::Busy("volume_leaving")))
            }
            Stage::Mounted { name, .. } => name,
        };
        let reply = AttachReply::new(*id, name, read_only)?;
        if !adopting {
            return Ok(Some(Attached::Mounted { reply, taken: None }));
        }
        let added = !entry.sessions.contains(&session);
        if added {
            entry
                .sessions
                .try_reserve(1)
                .map_err(|_| Errno::OutOfMemory)?;
            entry.sessions.push(session);
        }
        entry.custody.owed = Owed::Nothing;
        entry.custody.stuck_at = None;
        entry.custody.answering = true;
        Ok(Some(Attached::Mounted {
            reply,
            taken: Some(Taken {
                volume: entry.served.id(),
                added,
            }),
        }))
    }

    /// The attach answering for image volume `volume` is done: delivered, or,
    /// for `undo`, not, so it lets go of the hold it gave that session. One
    /// no session holds then owes its detach.
    fn answered(&self, volume: u64, undo: Option<ProcId>) {
        let owes = {
            let mut registry = self.registry.lock();
            let volumes = &mut registry.volumes;
            let Some(at) = position(volumes, volume) else {
                return;
            };
            let entry = &mut volumes[at];
            entry.custody.answering = false;
            if let Some(session) = undo {
                entry.sessions.retain(|held| *held != session);
                if entry.sessions.is_empty() && entry.custody.owed == Owed::Nothing {
                    entry.custody.owed = Owed::Orphaned;
                }
            }
            entry.custody.owed != Owed::Nothing
        };
        if owes {
            session_reaper::owe_work();
        }
    }

    /// The reply naming the volume published as `id`, mounted or lost.
    ///
    /// # Errors
    ///
    /// [`Errno::NotAttached`] for one taken out, or no longer shown, before it
    /// could be answered; the reply's refusal of the name.
    fn reply_for(&self, id: &[u8; 16]) -> Result<AttachReply, Errno> {
        let registry = self.registry.lock();
        let entry = registry
            .volumes
            .iter()
            .find(|v| v.id == *id)
            .ok_or(Errno::NotAttached)?;
        let name = entry.name().ok_or(Errno::NotAttached)?;
        AttachReply::new(*id, name, entry.served.read_only())
    }

    /// Every login session an image volume shown in the catalog was handed,
    /// once each and in order, or `None` for want of memory.
    fn held_sessions(&self) -> Option<Vec<ProcId>> {
        let mut sessions: Vec<ProcId> = Vec::new();
        {
            let registry = self.registry.lock();
            let volumes = &registry.volumes;
            let named = || volumes.iter().filter(|v| v.name().is_some());
            sessions
                .try_reserve_exact(named().map(|v| v.sessions.len()).sum())
                .ok()?;
            for entry in named() {
                sessions.extend_from_slice(&entry.sessions);
            }
        }
        sessions.sort_unstable();
        sessions.dedup();
        Some(sessions)
    }

    /// Forget the login sessions in `ended`, which is in order, leaving every
    /// image volume shown in the catalog that no open session holds now owed
    /// its detach; answers whether one came to owe it.
    fn end_sessions(&self, ended: &[ProcId]) -> bool {
        let mut registry = self.registry.lock();
        let volumes = &mut registry.volumes;
        let mut orphaned = false;
        for entry in volumes.iter_mut().filter(|v| v.name().is_some()) {
            entry
                .sessions
                .retain(|session| ended.binary_search(session).is_err());
            if entry.sessions.is_empty() && entry.custody.owed == Owed::Nothing {
                entry.custody.owed = Owed::Orphaned;
                orphaned = true;
            }
        }
        orphaned
    }

    /// Whether a settling task has a teardown it could do now.
    fn owes(&self) -> bool {
        settleable_in(&self.registry.lock()).is_some()
    }

    /// How many image volumes `uid` holds.
    fn owned_by(&self, uid: u32) -> usize {
        self.registry
            .lock()
            .volumes
            .iter()
            .filter(|v| v.owner == uid)
            .count()
    }

    /// A mount handle no volume has held this boot.
    fn mint_handle(&self) -> Result<DriverHandle, Errno> {
        let serial = self.next_handle.fetch_add(1, Ordering::Relaxed);
        DriverHandle::from_raw(serial).map_err(|_| Errno::NoSpace)
    }

    /// Enter `volume`, refusing a second of its identity, one past its
    /// owner's bound, one asking a name another image volume holds or asked
    /// first, and one whose file can no longer be reached, each judged under
    /// the guard so concurrent attaches cannot pass them together and no
    /// retirement can miss one. A refused one is let go outside the guard,
    /// its lock with it.
    ///
    /// # Errors
    ///
    /// The cause and refusal to record.
    fn enter(&self, volume: ImageVolume) -> Result<(), (&'static str, Errno)> {
        let mut registry = self.registry.lock();
        let volumes = &mut registry.volumes;
        let asked = volume.asked.as_deref();
        let refusal = if volume.stranded() {
            ("backing_gone", Errno::NotAttached)
        } else if volumes.iter().any(|v| v.id == volume.id) {
            ("identity_attached", Errno::Busy)
        } else if volumes.iter().filter(|v| v.owner == volume.owner).count()
            >= IMAGE_VOLUMES_PER_PRINCIPAL
        {
            ("too_many_images", Errno::LimitExceeded)
        } else if asked.is_some_and(|asked| {
            volumes
                .iter()
                .any(|v| v.name() == Some(asked) || v.asked.as_deref() == Some(asked))
        }) {
            NAME_IN_USE
        } else if volumes.try_reserve(1).is_err() {
            ("registry_full", Errno::OutOfMemory)
        } else {
            let at = volumes.partition_point(|v| v.served.id() < volume.served.id());
            volumes.insert(at, volume);
            return Ok(());
        };
        drop(registry);
        drop(volume);
        Err(refusal)
    }

    /// Take image volume `volume` out.
    fn take(&self, volume: u64) -> Option<ImageVolume> {
        let mut registry = self.registry.lock();
        let volumes = &mut registry.volumes;
        position(volumes, volume).map(|at| volumes.remove(at))
    }
}

/// Where served volume `volume`'s image sits in `volumes`.
fn position(volumes: &[ImageVolume], volume: u64) -> Option<usize> {
    volumes
        .binary_search_by_key(&volume, |v| v.served.id())
        .ok()
}

/// [`first_settleable`] of `registry`.
fn settleable_in(registry: &Registry) -> Option<usize> {
    first_settleable(
        registry.volumes.iter().map(|v| (v.owner, &v.custody)),
        registry.releases,
    )
}

/// An operation's hold on one image volume, let go on drop: the attach
/// waiting on it is woken, what was left for the holder that parks on
/// nothing is done, and a teardown owed is left to the settling tasks.
struct ImageClaim<'a> {
    service: &'a RuntimeVolumeService,
    volume: u64,
    /// The task its attach waits on.
    task: u64,
    /// What holds it.
    by: Holder,
    /// [`Registry::releases`] as it was claimed.
    since: u64,
}

impl Drop for ImageClaim<'_> {
    fn drop(&mut self) {
        let (owes, retiring, lost_server) = {
            let mut registry = self.service.images.registry.lock();
            let releases = registry.releases;
            let (counts, retiring, lost_server) = match position(&registry.volumes, self.volume) {
                Some(at) => {
                    let entry = &mut registry.volumes[at];
                    let counts = entry.custody.let_go(self.by, self.since, releases);
                    let retiring = core::mem::take(&mut entry.custody.retiring);
                    (counts, retiring, server_gone(entry))
                }
                None => (self.by.releases(), false, None),
            };
            if counts {
                registry.releases += 1;
            }
            (settleable_in(&registry).is_some(), retiring, lost_server)
        };
        volume_attach_wake(self.task);
        if owes {
            session_reaper::owe_work();
        }
        let Ok(wiring) = self.service.wiring() else {
            return;
        };
        if let Some(host) =
            lost_server.and_then(|server| self.service.image_server_lost(server, wiring.audit))
        {
            self.service.retire_images_on(host);
        }
        if retiring {
            let volume = self.volume;
            self.service
                .retire_where(&|v| v.served.id() == volume, Vec::new(), wiring.audit);
        }
    }
}

/// The server `entry` was mounted over, when that server has gone.
fn server_gone(entry: &ImageVolume) -> Option<u64> {
    match entry.stage {
        Stage::Mounted {
            server: Some(server),
            ..
        } if !tairix_kernel_core::callreg::contains(tairix_kernel_ipc::EndpointId(server)) => {
            Some(server)
        }
        _ => None,
    }
}

/// An attach's hold on the volume it builds: unless the attach stands, its
/// answer delivered, the volume leaves with it.
struct Building<'a> {
    service: &'a RuntimeVolumeService,
    volume: u64,
    end: BuildEnd,
}

/// How an attach's hold on the volume it builds ends.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum BuildEnd {
    /// It gave up before telling anyone the volume's name.
    Abandoned,
    /// Its answer was delivered.
    Stands,
    /// It took the volume back itself.
    TakenBack,
}

impl Building<'_> {
    /// Take the volume back on the attaching `task` before the attach answers
    /// that it gave up ([`RuntimeVolumeService::take_back`]).
    fn take_back(mut self, task: u64, audit: &'static (dyn Sink + Sync)) {
        self.service.take_back(self.volume, task, audit);
        self.end = BuildEnd::TakenBack;
    }
}

impl Drop for Building<'_> {
    fn drop(&mut self) {
        match self.end {
            BuildEnd::Stands => self.service.images.answered(self.volume, None),
            BuildEnd::Abandoned => self.service.abandon(self.volume),
            BuildEnd::TakenBack => {}
        }
    }
}

/// An attach's taking up of a volume attached already: unless its answer is
/// delivered, the session hold it added is let go.
struct Adopted<'a> {
    images: &'a ImageVolumes,
    taken: Taken,
    session: ProcId,
    delivered: bool,
}

impl Drop for Adopted<'_> {
    fn drop(&mut self) {
        let undo = (self.taken.added && !self.delivered).then_some(self.session);
        self.images.answered(self.taken.volume, undo);
    }
}

/// A held image volume, and what detaching it needs, copied out of the
/// registry.
struct Claimed<'a> {
    claim: ImageClaim<'a>,
    id: [u8; 16],
    handle: DriverHandle,
    name: ArrayString<VOLUME_NAME_MAX>,
    writes: Writes,
}

impl<'a> Claimed<'a> {
    /// Claim `entry`, shown in the catalog, for `service`, held `by` an
    /// operation or a settling task, `since` claims having been let go.
    fn of(
        service: &'a RuntimeVolumeService,
        entry: &mut ImageVolume,
        by: Holder,
        since: u64,
    ) -> Self {
        entry.custody.holder = by;
        Self {
            claim: ImageClaim {
                service,
                volume: entry.served.id(),
                task: entry.task,
                by,
                since,
            },
            id: entry.id,
            handle: entry.handle,
            // A catalog name is at most this long, so nothing is cut.
            name: ArrayString::from_str_truncating(entry.name().unwrap_or_default()),
            writes: match entry.stage {
                Stage::Lost { .. } => Writes::Lost,
                // The loss of what the driver held was audited as it went.
                Stage::Mounted { server: None, .. } => Writes::Gone,
                _ => Writes::Served,
            },
        }
    }
}

/// What a serve mounts a volume from, snapshotted out of the registry.
struct ServeTarget {
    id: [u8; 16],
    owner: u32,
    handle: DriverHandle,
    fstype: VolumeFsType,
    medium: BlkMedium,
    served: Arc<ServedVolume<ImageBlock>>,
    source: String,
    title: VolumeTitle,
    asked: Option<String>,
    base: NameBase,
    presented: Presented,
    access: ImageAccess,
    task: u64,
    /// The name it was mounted as, for a driver serving it again.
    mounted_as: Option<String>,
}

/// How an attach's wait for its driver ended.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Waited {
    Mounted,
    Refused(Refusal),
    Interrupted,
    TimedOut,
    /// It was taken out before the attach could answer.
    Gone,
    /// No scheduler can park the attach to wait for its driver.
    Unparkable,
}

/// An attach's wait for its volume's driver: judged once the driver's start
/// allowance runs out and every idle interval after, it gives up only on a
/// driver that showed no life since it was last judged, and never while an
/// operation holds the volume.
struct DriverWait {
    judged_at: u64,
    mark: Option<u64>,
}

/// What a [`DriverWait`] does next.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Step {
    Over(Waited),
    /// Wait on, judging the driver again at this deadline.
    Rejudge(u64),
    Wait,
}

impl DriverWait {
    /// A wait begun at `now`, its driver's activity reading `mark`.
    fn begin(now: u64, mark: Option<u64>) -> Self {
        Self {
            judged_at: now.saturating_add(IMAGE_DRIVER_START_NS),
            mark,
        }
    }

    /// The step at `now` for a volume whose stage `settled` the wait or not,
    /// that an operation has `claimed` or not, whose attacher was `killed`
    /// or not, and whose driver's activity reads `seen`.
    fn step(
        &mut self,
        settled: Option<Waited>,
        claimed: bool,
        killed: bool,
        now: u64,
        seen: Option<u64>,
    ) -> Step {
        if let Some(waited) = settled {
            return Step::Over(waited);
        }
        if killed {
            return Step::Over(Waited::Interrupted);
        }
        if now < self.judged_at {
            return Step::Wait;
        }
        if !claimed && !shows_life(self.mark, seen) {
            return Step::Over(Waited::TimedOut);
        }
        self.mark = seen;
        self.judged_at = now.saturating_add(SERVED_IDLE_NS);
        Step::Rejudge(self.judged_at)
    }
}

/// The node an image volume's `node` is under the image-volume node.
fn image_node(node: Option<u32>) -> Option<(u32, u32)> {
    node.map(|node| (IMAGE_VOLUMES_NODE_ID, node))
}

/// The kernel's own parent of image volumes' nodes: a bus beneath the root
/// with no match key and no resource, so no driver binds it and none's
/// departure takes an image volume's node with it.
#[must_use]
pub(crate) fn image_volumes_node() -> HwNode {
    HwNode::new(IMAGE_VOLUMES_NODE_ID, HW_NODE_ROOT_ID, HwDeviceClass::Bus)
}

/// Why an attach goes no further than preparing its volume.
enum Unprepared {
    /// The file holds nothing the kernel attaches.
    Refused(ImageRefusal),
    /// The caller has this file's volume attached already, as `reply` names
    /// it, taken up as `taken` says.
    AlreadyAttached {
        reply: AttachReply,
        taken: Option<Taken>,
    },
    /// The catalog name asked for it was refused, for the cause named.
    Name(&'static str, Errno),
    /// Refused, for the cause named.
    Failed(&'static str, Errno),
}

impl RuntimeVolumeService {
    /// Tell `answer` the caller's attach took up the volume `reply` names,
    /// attached already: the session hold `taken` added stays only once the
    /// answer is delivered.
    fn answer_adopted(
        &self,
        reply: AttachReply,
        taken: Option<Taken>,
        session: ProcId,
        answer: AnswerSink<'_>,
    ) -> Result<(), Errno> {
        let mut adopted = taken.map(|taken| Adopted {
            images: &self.images,
            taken,
            session,
            delivered: false,
        });
        answer(&ImageAttachReply::AlreadyAttached(reply))?;
        if let Some(adopted) = &mut adopted {
            adopted.delivered = true;
        }
        Ok(())
    }

    /// Attach the volume the image file `source` holds for `caller`, as
    /// `attach` asks, and hand the answer to `answer`.
    ///
    /// # Errors
    ///
    /// As [`tairix_kernel_core::VolumeService::attach_image`].
    pub(super) fn attach_image_volume(
        &self,
        wiring: &Wiring,
        caller: &Principal<'_>,
        source: ImageSource,
        attach: &ImageAttach<'_>,
        answer: AnswerSink<'_>,
    ) -> Result<(), Errno> {
        let audit = wiring.audit;
        let session = source.session;
        let opened_as = Arc::clone(&source.file.opened_as);
        let file_name = last_component(&opened_as);
        let label = attach.name.unwrap_or(file_name);
        let refused = |cause: &'static str, errno: Errno| {
            record(
                audit,
                Level::Warn,
                IMAGE_ATTACH_REFUSED,
                ATTACH_REFUSED,
                label,
                cause,
            );
            errno
        };
        let entry = match self.prepare_image(caller, source, attach, file_name) {
            Ok(entry) => entry,
            Err(Unprepared::Refused(why)) => return answer(&refused_reply(audit, label, why)),
            Err(Unprepared::AlreadyAttached { reply, taken }) => {
                return self
                    .answer_adopted(reply, taken, session, answer)
                    .map_err(|errno| refused("answer_undelivered", errno));
            }
            Err(Unprepared::Name(cause, errno)) => {
                refused(cause, errno);
                return answer(&ImageAttachReply::NameRefused(errno));
            }
            Err(Unprepared::Failed(cause, errno)) => return Err(refused(cause, errno)),
        };
        let (id, task, fstype) = (entry.id, entry.task, entry.fstype);
        let served = Arc::clone(&entry.served);
        let volume = served.id();
        if let Err((cause, errno)) = self.images.enter(entry) {
            self.retire_served(Some(&served), None);
            refused(cause, errno);
            if (cause, errno) == NAME_IN_USE {
                return answer(&ImageAttachReply::NameRefused(errno));
            }
            return Err(errno);
        }
        let mut building = Building {
            service: self,
            volume,
            end: BuildEnd::Abandoned,
        };
        // An unprivileged principal asks for it, so it may not spend the ids
        // devices need.
        let node = volume_node(IMAGE_VOLUMES_NODE_ID, fstype, volume)
            .and_then(|node| {
                wiring
                    .tree
                    .publish_requested_child(IMAGE_VOLUMES_NODE_ID, node)
            })
            .map_err(|errno| refused("volume_node_unpublished", errno))?;
        self.record_node(volume, node);
        record(
            audit,
            Level::Info,
            IMAGE_VOLUME_AWAITING,
            "image-volume: published; awaiting its filesystem driver",
            label,
            presentation_word(attach.presentation),
        );
        match self.await_driver(volume, task, &served.activity()) {
            Waited::Mounted => {}
            Waited::Refused(Refusal::Driver(why)) => {
                return answer(&refused_reply(audit, label, refusal_of(why)));
            }
            // Recorded with its cause where it was refused.
            Waited::Refused(Refusal::Name(errno)) => {
                return answer(&ImageAttachReply::NameRefused(errno));
            }
            // Recorded with its cause where it was refused.
            Waited::Refused(Refusal::Kernel(errno)) => return Err(errno),
            Waited::Interrupted => return Err(refused("interrupted", Errno::Interrupted)),
            Waited::TimedOut => return Err(refused("driver_silent", Errno::TimedOut)),
            Waited::Gone => return Err(refused(GONE.0, GONE.1)),
            Waited::Unparkable => return Err(refused("unparkable", Errno::NotImplemented)),
        }
        // Mounted now, so a volume the attach gives up on is taken back
        // before the caller hears why.
        let reply = match self.finish_image(caller, attach, id, task) {
            Ok(reply) => reply,
            Err(Unfinished::Target(errno)) => {
                refused("target_refused", errno);
                building.take_back(task, audit);
                return answer(&ImageAttachReply::TargetRefused(errno));
            }
            Err(Unfinished::Failed(cause, errno)) => {
                let errno = refused(cause, errno);
                building.take_back(task, audit);
                return Err(errno);
            }
        };
        match answer(&ImageAttachReply::Attached(reply)) {
            Ok(()) => {
                building.end = BuildEnd::Stands;
                Ok(())
            }
            Err(errno) => {
                let errno = refused("answer_undelivered", errno);
                building.take_back(task, audit);
                Err(errno)
            }
        }
    }

    /// The work files of a writable archive held under `place`, which its
    /// attachment locks as `hold` for `task`.
    fn work_files(
        &self,
        place: &ImageDirectory,
        hold: &Arc<SleepLock<FileHold>>,
        task: u64,
    ) -> Result<alloc::boxed::Box<archive::WorkFiles>, Unprepared> {
        let audit = self
            .wiring()
            .map_err(|errno| Unprepared::Failed("no_audit", errno))?
            .audit;
        archive::WorkFiles::new(place, Arc::clone(hold), task, audit)
            .and_then(|files| alloc::boxed::Box::try_new(files).map_err(|_| Errno::OutOfMemory))
            .map_err(|errno| Unprepared::Failed("work_files_unbuilt", errno))
    }

    /// Recognise the file `source` holds, judge it and `caller`'s attach of
    /// it, lock it and build its served volume: the image volume, ready to
    /// enter, or why it goes no further.
    fn prepare_image(
        &self,
        caller: &Principal<'_>,
        source: ImageSource,
        attach: &ImageAttach<'_>,
        file_name: &str,
    ) -> Result<ImageVolume, Unprepared> {
        let presented = Presented::asked_by(caller, attach.presentation)
            .map_err(|errno| Unprepared::Failed("presentation_refused", errno))?;
        if attach.name.is_some_and(|name| !chosen(name.as_bytes())) {
            return Err(Unprepared::Name("name_reserved", Errno::OutOfRange));
        }
        let backing = source.file.projection.backing();
        let depth = self.images.depth_on(backing).saturating_add(1);
        if depth > IMAGE_NESTING_MAX {
            return Err(Unprepared::Failed("nested_too_deep", Errno::LimitExceeded));
        }
        let file_volume = Arc::clone(source.file.node.volume());
        let (mut block, file, probed) = recognised(source.file, attach)?;
        let id = identity(caller.uid, &file, &probed.identity);
        match self.images.attached_as(&id, source.session, attach) {
            Ok(Some(Attached::Mounted { reply, taken })) => {
                return Err(Unprepared::AlreadyAttached { reply, taken });
            }
            Ok(Some(Attached::Busy(cause))) => return Err(Unprepared::Failed(cause, Errno::Busy)),
            Ok(None) => {}
            Err(errno) => return Err(Unprepared::Failed("session_unrecorded", errno)),
        }
        // Settled under the registry's guard as the volume enters; this spares
        // building one that would be refused anyway.
        if self.images.owned_by(caller.uid) >= IMAGE_VOLUMES_PER_PRINCIPAL {
            return Err(Unprepared::Failed("too_many_images", Errno::LimitExceeded));
        }
        let archive = probed.fstype.is_archive();
        if archive {
            // A format recording no name of its own names its member by it.
            block.archive_name = ArchiveName::new(file_name.as_bytes());
        }
        let (writable, place) = granted_access(
            attach.access,
            source.writable,
            source.directory.as_ref(),
            &block,
            &file,
            archive,
        )?;
        // An archive is never written in place.
        block.writable = writable && !archive;
        if let Some(name) = attach.name {
            let vfs = LATE_FILESYSTEM
                .vfs()
                .map_err(|errno| Unprepared::Failed("no_mount_table", errno))?;
            if self.name_in_use(vfs, name, None) {
                return Err(Unprepared::Name(NAME_IN_USE.0, NAME_IN_USE.1));
            }
        }
        let (shown_from, asked) = owned_names(file_name, attach.name)?;
        let mut sessions = Vec::new();
        sessions
            .try_reserve_exact(1)
            .map_err(|_| Unprepared::Failed("session_unrecorded", Errno::OutOfMemory))?;
        sessions.push(source.session);
        let base = match NameBase::of(probed.label(), Some(BlkTransport::Virtual)) {
            NameBase::Unlabelled(_) => NameBase::of(stem(file_name), Some(BlkTransport::Virtual)),
            labelled @ NameBase::Label(_) => labelled,
        };
        let hold = FileHold::take(&block.file, writable, source.task)
            .map_err(|errno| Unprepared::Failed("file_locked", errno))?;
        let hold = Arc::try_new(SleepLock::new(hold))
            .map_err(|_| Unprepared::Failed("lock_unrecorded", Errno::OutOfMemory))?;
        if let (true, Some(place)) = (writable, place.as_ref()) {
            block.shape = Shape::Archive(Some(self.work_files(place, &hold, source.task)?));
        }
        let handle = self
            .images
            .mint_handle()
            .map_err(|errno| Unprepared::Failed("handle_unminted", errno))?;
        // The last step that can fail: the served volume is registered.
        let served = self
            .new_served(block, !writable)
            .map_err(|errno| Unprepared::Failed("served_volume_unbuilt", errno))?;
        Ok(ImageVolume {
            id,
            owner: caller.uid,
            sessions,
            task: source.task,
            file_volume,
            backing,
            depth,
            served,
            node: None,
            handle,
            fstype: probed.fstype,
            medium: image_medium(probed.form),
            source: shown_from,
            title: probed_title(probed.label()),
            asked,
            base,
            presented,
            access: attach.access,
            hold,
            stage: Stage::Awaiting,
            custody: Custody {
                answering: true,
                ..Custody::default()
            },
        })
    }

    /// Bind the image volume `id` at `attach`'s target as `caller`, and
    /// answer the attach on `task`. A failure, or a kill that landed
    /// meanwhile, leaves the volume to the attach's abandonment.
    fn finish_image(
        &self,
        caller: &Principal<'_>,
        attach: &ImageAttach<'_>,
        id: [u8; 16],
        task: u64,
    ) -> Result<AttachReply, Unfinished> {
        if let Some(target) = attach.target {
            let bind = ViewBindRequest {
                volume_id: id,
                subtree: "/",
                target,
                narrowing: MountFlags::default(),
            };
            FS_SERVICE
                .view_bind(caller.uid, &ShortOfMount(caller.caps), &bind)
                .map_err(|errno| {
                    if self.images.still_served(&id) {
                        Unfinished::Target(errno)
                    } else {
                        Unfinished::Failed(GONE.0, GONE.1)
                    }
                })?;
        }
        if tairix_kernel_core::procsignal::kill_pending(task) {
            return Err(Unfinished::Failed("interrupted", Errno::Interrupted));
        }
        self.images.reply_for(&id).map_err(|errno| match errno {
            Errno::NotAttached => Unfinished::Failed(GONE.0, GONE.1),
            errno => Unfinished::Failed("name_unrepliable", errno),
        })
    }

    /// Whether `name` is shown in the catalog, or held for a volume other than
    /// image volume `except` that awaits its driver.
    fn name_in_use(&self, vfs: &Vfs, name: &str, except: Option<u64>) -> bool {
        let shown =
            storage_path(name).is_ok_and(|path| vfs.mounts().resolve(&path).path() == &path);
        shown || self.device_name_reserved(name) || self.images.reserves(name, except)
    }

    /// Record the node image volume `volume` was published as; withdraw it
    /// at once when the volume left meanwhile.
    fn record_node(&self, volume: u64, node: u32) {
        let recorded = {
            let mut registry = self.images.registry.lock();
            let volumes = &mut registry.volumes;
            position(volumes, volume).is_some_and(|at| {
                let entry = &mut volumes[at];
                let live = !matches!(entry.stage, Stage::Refused(_)) && !entry.served.retired();
                if live {
                    entry.node = Some(node);
                }
                live
            })
        };
        if !recorded {
            self.retire_served::<ImageBlock>(None, image_node(Some(node)));
        }
    }

    /// Wait for image volume `volume`'s driver, on `task`, judging it by
    /// `activity`: until it serves or refuses the volume, falls silent, or the
    /// caller is killed or cannot be parked. While an operation holds the
    /// volume the driver is not judged, but the wait stays bounded and
    /// killable; the holder wakes it as it lets go.
    fn await_driver(&self, volume: u64, task: u64, activity: &ServerActivity) -> Waited {
        let now = || wait_arch().map_or(0, WaitQueueArch::now_ns);
        let arm = |deadline: u64| {
            let filed = VOLUME_ATTACH_WAITQ.register(task, deadline);
            if let Some(hook) = wait_arch() {
                hook.set_wakeup(nearest_timed_deadline());
            }
            filed.is_ok()
        };
        let mut wait = DriverWait::begin(now(), activity.mark());
        // A wait the heap cannot register could never be woken.
        if !arm(wait.judged_at) {
            return Waited::Unparkable;
        }
        let waited = loop {
            let (settled, claimed) = self.stage_of(volume);
            // An operation holding the volume settles it as the attach's
            // abandonment takes it out.
            let killed = tairix_kernel_core::procsignal::kill_pending(task);
            match wait.step(settled, claimed, killed, now(), activity.mark()) {
                Step::Over(waited) => break waited,
                Step::Rejudge(deadline) => {
                    if !arm(deadline) {
                        break Waited::Unparkable;
                    }
                }
                Step::Wait => {}
            }
            if !park_calling_task() {
                break Waited::Unparkable;
            }
        };
        VOLUME_ATTACH_WAITQ.deregister(task);
        if let Some(hook) = wait_arch() {
            hook.set_wakeup(nearest_timed_deadline());
        }
        waited
    }

    /// What image volume `volume`'s attach waits on has become, if it is
    /// settled, and whether an operation holds it.
    fn stage_of(&self, volume: u64) -> (Option<Waited>, bool) {
        let registry = self.images.registry.lock();
        let volumes = &registry.volumes;
        let Some(entry) = position(volumes, volume).map(|at| &volumes[at]) else {
            return (Some(Waited::Gone), false);
        };
        if entry.custody.claimed() {
            return (None, true);
        }
        let outcome = match &entry.stage {
            Stage::Awaiting => None,
            Stage::Refused(refusal) => Some(Waited::Refused(*refusal)),
            Stage::Mounted { .. } | Stage::Lost { .. } => Some(Waited::Mounted),
        };
        (outcome, false)
    }

    /// Take out image volume `volume`, whose attach gave up after mounting it,
    /// on that attach's own `task`: a plain detach, so nothing written to it
    /// meanwhile is discarded. A caller being killed, a volume something holds,
    /// or a refused detach leaves it owed to the settling tasks, which force
    /// it ([`Self::abandon`]).
    fn take_back(&self, volume: u64, task: u64, audit: &'static (dyn Sink + Sync)) {
        // Asked before the registry lock: the gates' lock never nests in it.
        let here = !tairix_kernel_core::procsignal::kill_pending(task);
        let claimed = {
            let mut registry = self.images.registry.lock();
            let since = registry.releases;
            let Some(at) = position(&registry.volumes, volume) else {
                return;
            };
            let entry = &mut registry.volumes[at];
            let mounted = matches!(entry.stage, Stage::Mounted { .. } | Stage::Lost { .. });
            (here && mounted && !entry.custody.claimed()).then(|| {
                entry.custody.answering = false;
                entry.sessions.clear();
                entry.custody.owed = Owed::Abandoned;
                Claimed::of(self, entry, Holder::Operation, since)
            })
        };
        match claimed {
            // A refusal leaves it owed, unclaimed, as its claim lets go.
            Some(claimed) => {
                let _ = self.detach_claimed(audit, claimed, DetachMode::Plain, "abandoned");
            }
            None => self.abandon(volume),
        }
    }

    /// Take out image volume `volume`, whose attach gave up without telling
    /// anyone its name: at once when nothing holds it and nothing was mounted,
    /// else by a settling task, since taking a mount down parks. While its
    /// attach answered for it no other could take it up, so no session holds
    /// it.
    fn abandon(&self, volume: u64) {
        let taken = {
            let mut registry = self.images.registry.lock();
            let volumes = &mut registry.volumes;
            let Some(at) = position(volumes, volume) else {
                return;
            };
            let entry = &mut volumes[at];
            entry.custody.answering = false;
            entry.sessions.clear();
            if !entry.custody.claimed()
                && matches!(entry.stage, Stage::Awaiting | Stage::Refused(_))
            {
                Some(volumes.remove(at))
            } else {
                entry.custody.owed = Owed::Abandoned;
                None
            }
        };
        match taken {
            Some(entry) => self.retire_served(Some(&entry.served), image_node(entry.node)),
            None => session_reaper::owe_work(),
        }
    }

    /// Do what one image volume owes, answering whether one was taken up:
    /// the first nothing holds, of an owner no settling task serves already.
    /// Another settling task is woken while more is owed. Parks while a
    /// mounted one is taken down.
    fn settle_image(&self) -> bool {
        let Ok(wiring) = self.wiring() else {
            return false;
        };
        let (work, more) = {
            let mut registry = self.images.registry.lock();
            let Some(at) = settleable_in(&registry) else {
                return false;
            };
            let since = registry.releases;
            let entry = &mut registry.volumes[at];
            let cause = match entry.custody.owed {
                Owed::Orphaned => "session_ended",
                Owed::Abandoned | Owed::Nothing => "abandoned",
            };
            let work = match entry.stage {
                Stage::Awaiting | Stage::Refused(_) => Err(registry.volumes.remove(at)),
                Stage::Mounted { .. } | Stage::Lost { .. } => {
                    Ok((Claimed::of(self, entry, Holder::Settler, since), cause))
                }
            };
            (work, settleable_in(&registry).is_some())
        };
        if more {
            session_reaper::owe_work();
        }
        match work {
            Err(entry) => self.retire_served(Some(&entry.served), image_node(entry.node)),
            Ok((claimed, cause)) => {
                // A failure leaves it stuck, owed, as its claim lets go.
                let _ = self.detach_claimed(wiring.audit, claimed, DetachMode::Force, cause);
            }
        }
        true
    }

    /// Serve the filesystem a driver `offer`ed on image volume `volume` over
    /// `channel`.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] for no such volume awaiting a driver,
    /// [`Errno::AlreadyExists`] for one served already, [`Errno::Busy`] for one
    /// another operation holds, and the refusals of mounting it.
    pub(super) fn serve_image(
        &self,
        wiring: &Wiring,
        volume: u64,
        offer: &ServeOffer,
        channel: ServeChannel,
    ) -> Result<(), Errno> {
        let audit = wiring.audit;
        let (claim, target) = self.claim_awaiting_image(volume)?;
        if target.mounted_as.is_some() {
            // What a lost driver left is no driver's to commit, and the loss
            // could not park to let it go.
            target.served.discard_work();
        }
        let server = channel.endpoint.id().0;
        let mounted = self.mount_image(&target, offer, channel, wiring);
        // What the attach records its own refusals as.
        let label = target
            .mounted_as
            .as_deref()
            .or(target.asked.as_deref())
            .unwrap_or(&target.source);
        let name = match mounted {
            Ok(name) => name,
            Err((cause, errno)) => {
                record(
                    audit,
                    Level::Warn,
                    IMAGE_ATTACH_REFUSED,
                    ATTACH_REFUSED,
                    label,
                    cause,
                );
                let refusal = if (cause, errno) == NAME_IN_USE && target.asked.is_some() {
                    Refusal::Name(errno)
                } else {
                    Refusal::Kernel(errno)
                };
                self.unserved(claim, &target, refusal, audit, "serve_refused");
                return Err(errno);
            }
        };
        if offer.write_refusal.is_some() {
            // The volume stops taking writes before its file stops being held
            // for one writer.
            target.served.refuse_writes();
            self.share_hold(volume, target.task);
        }
        let resumed = target.mounted_as.is_some();
        record(
            audit,
            Level::Info,
            IMAGE_VOLUME_ATTACHED,
            if resumed {
                "image-volume: a filesystem driver serves it again"
            } else {
                "image-volume: served and mounted for its attacher"
            },
            &name,
            offer.format.as_str(),
        );
        self.set_stage(
            volume,
            Stage::Mounted {
                name,
                format: offer.format,
                server: Some(server),
            },
        );
        drop(claim);
        Ok(())
    }

    /// The driver of image volume `volume` could not open it, for `why`.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] for no such volume awaiting a driver.
    pub(super) fn refuse_image(
        &self,
        wiring: &Wiring,
        volume: u64,
        why: DriverError,
    ) -> Result<(), Errno> {
        let (claim, target) = self.claim_awaiting_image(volume)?;
        // A first serve's refusal is recorded by the attach, with its reason.
        if let Some(name) = &target.mounted_as {
            record(
                wiring.audit,
                Level::Warn,
                IMAGE_ATTACH_REFUSED,
                ATTACH_REFUSED,
                name,
                "driver_refused",
            );
        }
        self.unserved(
            claim,
            &target,
            Refusal::Driver(why),
            wiring.audit,
            "driver_refused",
        );
        Ok(())
    }

    /// `target`, claimed by `claim`, will not be served, for `refusal`: its
    /// attach is told, or, once mounted, it leaves as its driver did, for
    /// `cause`, with every image volume on it.
    fn unserved(
        &self,
        claim: ImageClaim<'_>,
        target: &ServeTarget,
        refusal: Refusal,
        audit: &'static (dyn Sink + Sync),
        cause: &'static str,
    ) {
        if target.mounted_as.is_none() {
            self.set_stage(claim.volume, Stage::Refused(refusal));
            return;
        }
        if let Some(left) = self.retire_claimed(claim, cause, audit) {
            left.served.discard_work();
        }
        self.retire_images_on(target.handle);
    }

    /// Claim image volume `volume`, awaiting a driver.
    fn claim_awaiting_image(&self, volume: u64) -> Result<(ImageClaim<'_>, ServeTarget), Errno> {
        let mut registry = self.images.registry.lock();
        let since = registry.releases;
        let volumes = &mut registry.volumes;
        let entry = position(volumes, volume)
            .map(|at| &mut volumes[at])
            .filter(|v| !v.served.retired())
            .ok_or(Errno::NotFound)?;
        let mounted_as = match &entry.stage {
            Stage::Awaiting => None,
            Stage::Mounted {
                server: None, name, ..
            } => Some(concatenated(&[name]).ok_or(Errno::OutOfMemory)?),
            Stage::Mounted {
                server: Some(_), ..
            } => return Err(Errno::AlreadyExists),
            Stage::Refused(_) | Stage::Lost { .. } => return Err(Errno::NotFound),
        };
        if entry.custody.claimed() || entry.custody.retiring {
            return Err(Errno::Busy);
        }
        let owned = |text: &str| concatenated(&[text]).ok_or(Errno::OutOfMemory);
        let target = ServeTarget {
            id: entry.id,
            owner: entry.owner,
            handle: entry.handle,
            fstype: entry.fstype,
            medium: entry.medium,
            served: Arc::clone(&entry.served),
            source: owned(&entry.source)?,
            title: entry.title,
            asked: entry.asked.as_deref().map(owned).transpose()?,
            base: entry.base,
            presented: entry.presented,
            access: entry.access,
            task: entry.task,
            mounted_as,
        };
        entry.custody.holder = Holder::Operation;
        Ok((
            ImageClaim {
                service: self,
                volume,
                task: entry.task,
                by: Holder::Operation,
                since,
            },
            target,
        ))
    }

    /// Move image volume `volume` to `stage`.
    fn set_stage(&self, volume: u64, stage: Stage) {
        let mut registry = self.images.registry.lock();
        let volumes = &mut registry.volumes;
        if let Some(at) = position(volumes, volume) {
            volumes[at].stage = stage;
        }
    }

    /// Downgrade image volume `volume`'s lock to shared: its driver will not
    /// write it.
    fn share_hold(&self, volume: u64, task: u64) {
        let hold = {
            let registry = self.images.registry.lock();
            let volumes = &registry.volumes;
            position(volumes, volume).map(|at| Arc::clone(&volumes[at].hold))
        };
        if let Some(hold) = hold {
            let hold = hold.lock();
            // A downgrade conflicts with nothing.
            let _ = lock_whole(hold.file, hold.owner, Held::Shared, task);
        }
    }

    /// Mount what `offer` describes, served over `channel`, for `target`: in
    /// the catalog for the first time, or in place of the stand-in of a
    /// volume a driver served before. Answers the name it is shown as.
    ///
    /// # Errors
    ///
    /// `(cause, errno)` for the serve path to record.
    fn mount_image(
        &self,
        target: &ServeTarget,
        offer: &ServeOffer,
        channel: ServeChannel,
        wiring: &Wiring,
    ) -> Result<String, (&'static str, Errno)> {
        // The kernel named the format from its own probe; a driver names only
        // the generation it is.
        if offer.format.family() != Some(target.fstype) {
            return Err(("format_mismatch", Errno::BadMagic));
        }
        let read_only = target.served.read_only() || offer.write_refusal.is_some();
        // The attach's own contract; a driver serving it again keeps it
        // mounted, read-only or not.
        if !target.access.accepts(read_only) && target.mounted_as.is_none() {
            return Err(("write_protected", Errno::ReadOnly));
        }
        let vfs = LATE_FILESYSTEM
            .vfs()
            .map_err(|err| ("no_mount_table", err))?;
        let fs = ServedFs::new(
            &channel.endpoint,
            channel.window,
            *offer,
            &target.served.activity(),
            wiring.audit,
        )
        .map_err(|err| ("proxy_unbuilt", err))?;
        let state = fs.state();
        let (driver, watch) = cached(
            PresentedFs::whole(fs, target.presented),
            target.handle.as_u64(),
            target.id,
            wiring.pressure,
            wiring.audit,
            wiring.watches,
        )
        .map_err(|err| ("cache_unbuilt", err))?;
        let attachment =
            || Attachment::new(target.handle, Some(target.medium), mount_flags(read_only));
        let template = target.presented.template();
        let register = |driver, watch| {
            let names = VolumeNames {
                source: &target.source,
                fstype: offer.format.as_str(),
                volume_id: target.id,
                identity: target.id,
            };
            LATE_FILESYSTEM
                .register_served(
                    target.handle,
                    driver,
                    names,
                    watch,
                    Some(target.presented.owner(target.owner)),
                    VolumeClass::Image,
                )
                .map(drop)
                .map_err(|_| ("handle_in_use", Errno::Busy))
        };
        let name = if let Some(name) = &target.mounted_as {
            // Under the stand-in's lock while it still answers the mount, as
            // a device volume's driver serving again is.
            let standing = LATE_FILESYSTEM.driver(target.handle).ok();
            let _held = standing.as_deref().map(DriverLock::lock);
            reattach(vfs, attachment(), storage_path(name)?, template)?;
            let _ = LATE_FILESYSTEM.unregister(target.handle);
            register(driver, watch)?;
            concatenated(&[name]).ok_or(("name_unallocated", Errno::OutOfMemory))?
        } else {
            let name = self.mount_named(vfs, target, attachment, &template)?;
            let unmount = || {
                let _ = vfs
                    .mounts_write()
                    .detach(target.handle, BindsWithin::Withdraw);
            };
            // Published before the driver is reachable, so a refusal has no
            // operation to turn away.
            publish_volume_identity(target.id, &["Storage", &name], &name, wiring.audit).map_err(
                |err| {
                    unmount();
                    let errno = if err == tairix_kernel_core::VolumePublishError::OutOfMemory {
                        Errno::OutOfMemory
                    } else {
                        Errno::Busy
                    };
                    ("identity_unpublishable", errno)
                },
            )?;
            register(driver, watch).inspect_err(|_| {
                unmount();
                let _ = VOLUME_FOREST.unpublish(&target.id);
            })?;
            name
        };
        let _ = LATE_FILESYSTEM.set_served_state(target.handle, Some(state));
        // A format writing nothing of its files has its image shared among
        // its attachers, so its title, written apart, stays as mastered.
        let shared = offer.write_refusal.is_some();
        let titling = Titling::of(target.fstype, offer, target.served.read_only() || shared);
        let _ = LATE_FILESYSTEM.set_title(target.handle, target.title, titling);
        let _ = LATE_FILESYSTEM.set_availability(target.handle, MountAvailability::Available);
        Ok(name)
    }
}

impl RuntimeVolumeService {
    /// Show `target` in the catalog: under the name it asked for, or the
    /// first the naming policy gives that is free. Answers the name.
    ///
    /// # Errors
    ///
    /// `("name_in_use", AlreadyExists)` once no name it may take is free; the
    /// mount table's other refusals.
    fn mount_named(
        &self,
        vfs: &Vfs,
        target: &ServeTarget,
        attachment: impl Fn() -> Attachment,
        template: &tairix_kernel_core::Metadata,
    ) -> Result<String, (&'static str, Errno)> {
        let volume = target.served.id();
        let shown_as = |name: &str| -> Result<bool, (&'static str, Errno)> {
            if self.name_in_use(vfs, name, Some(volume)) {
                return Ok(false);
            }
            match vfs
                .mounts_write()
                .attach_at(attachment(), storage_path(name)?, template.clone())
            {
                Ok(()) => Ok(true),
                Err(VfsError::AlreadyExists) => Ok(false),
                Err(err) => Err(("mount_refused", err.to_errno())),
            }
        };
        let owned =
            |name: &str| concatenated(&[name]).ok_or(("name_unallocated", Errno::OutOfMemory));
        if let Some(name) = &target.asked {
            return if shown_as(name)? {
                owned(name)
            } else {
                Err(NAME_IN_USE)
            };
        }
        for candidate in candidates(target.base, target.id) {
            let name = core::str::from_utf8(candidate.as_bytes())
                .map_err(|_| ("name_invalid", Errno::OutOfRange))?;
            if shown_as(name)? {
                return owned(name);
            }
        }
        Err(NAME_IN_USE)
    }

    /// Detach the image volume published as `id`: for `caller`, its owner or
    /// a holder of the mount authority, or for the kernel itself when there is
    /// none, for `cause`. A force — `mode` forcing the volume as it stands
    /// once claimed — revokes its descriptors and detaches the image volumes
    /// on it first.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] for no such volume mounted;
    /// [`Errno::PermissionDenied`] for a caller with no authority over it;
    /// [`Errno::Busy`] for one another operation holds, or, without a force,
    /// one a descriptor or an image volume holds; a refused flush without a
    /// force.
    pub(super) fn detach_image(
        &self,
        audit: &'static (dyn Sink + Sync),
        id: &[u8; 16],
        mode: DetachMode,
        caller: Option<&Principal<'_>>,
        cause: &'static str,
    ) -> Result<(), Errno> {
        self.detach_held(audit, id, mode, caller, cause, Holder::Operation)
    }

    /// [`Self::detach_image`], the volume held `by` the holder named.
    fn detach_held(
        &self,
        audit: &'static (dyn Sink + Sync),
        id: &[u8; 16],
        mode: DetachMode,
        caller: Option<&Principal<'_>>,
        cause: &'static str,
        by: Holder,
    ) -> Result<(), Errno> {
        let claimed = {
            let mut registry = self.images.registry.lock();
            let since = registry.releases;
            let volumes = &mut registry.volumes;
            let entry = volumes
                .iter_mut()
                .find(|v| v.id == *id)
                .filter(|v| v.name().is_some())
                .ok_or(Errno::NotFound)?;
            if caller.is_some_and(|caller| entry.owner != caller.uid && !caller.may_mount()) {
                let name = ArrayString::<VOLUME_NAME_MAX>::from_str_truncating(
                    entry.name().unwrap_or_default(),
                );
                drop(registry);
                record(
                    audit,
                    Level::Warn,
                    IMAGE_DETACH_REFUSED,
                    DETACH_REFUSED,
                    name.as_str(),
                    "not_owner",
                );
                return Err(Errno::PermissionDenied);
            }
            if entry.custody.claimed() {
                return Err(Errno::Busy);
            }
            Claimed::of(self, entry, by, since)
        };
        self.detach_claimed(audit, claimed, mode, cause)
    }

    /// Detach `claimed`, for `cause`, as [`Self::detach_image`] does.
    fn detach_claimed(
        &self,
        audit: &'static (dyn Sink + Sync),
        claimed: Claimed<'_>,
        mode: DetachMode,
        cause: &'static str,
    ) -> Result<(), Errno> {
        let Claimed {
            claim,
            id,
            handle,
            name,
            writes,
        } = claimed;
        let force = mode.forces(writes == Writes::Lost);
        let refused = |why: &'static str, errno: Errno| {
            record(
                audit,
                refusal_level(mode, errno),
                IMAGE_DETACH_REFUSED,
                DETACH_REFUSED,
                name.as_str(),
                why,
            );
            errno
        };
        self.clear_dependents(handle, force, audit, claim.by.beneath())
            .map_err(|(why, errno)| refused(why, errno))?;
        let given_up =
            take_down(handle, id, force, writes).map_err(|(why, errno)| refused(why, errno))?;
        let taken = self.images.take(claim.volume);
        drop(claim);
        if let Some(entry) = taken {
            entry.served.discard_work();
            self.retire_served(Some(&entry.served), image_node(entry.node));
        }
        audit_detached(audit, name.as_str(), cause, force, given_up);
        // One that entered as it was detached is stranded with it.
        self.retire_images_on(handle);
        Ok(())
    }

    /// Title the image volume published as `id` `title` for `caller`, its
    /// owner: written as its filesystem's label through its driver and
    /// flushed, the volume held against every other operation meanwhile, and
    /// listed under it once its driver holds it. The catalog name it is shown
    /// under stays as it is while it is attached.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] for no such volume shown in the catalog;
    /// [`Errno::PermissionDenied`] for a caller other than its owner;
    /// [`Errno::Busy`] for one an operation holds, an attach answers for, or
    /// that owes its teardown; [`Errno::DeviceOffline`] for one no driver
    /// serves; [`Errno::NotSupported`] for a format that keeps no title;
    /// [`Errno::ReadOnly`] for one that takes no writes; the driver's refusal
    /// of the title or of the flush.
    pub(super) fn retitle_image(
        &self,
        audit: &'static (dyn Sink + Sync),
        caller: &Principal<'_>,
        id: &[u8; 16],
        title: &VolumeTitle,
    ) -> Result<(), Errno> {
        let claimed = {
            let mut registry = self.images.registry.lock();
            let since = registry.releases;
            let found = registry
                .volumes
                .iter_mut()
                .find(|v| v.id == *id)
                .filter(|v| v.name().is_some());
            match found {
                None => Err((
                    ArrayString::<VOLUME_NAME_MAX>::from_str_truncating("unknown"),
                    "identity_not_attached",
                    Errno::NotFound,
                )),
                Some(entry) => match retitle_refusal(entry, caller.uid) {
                    Some((cause, errno)) => Err((
                        ArrayString::<VOLUME_NAME_MAX>::from_str_truncating(
                            entry.name().unwrap_or_default(),
                        ),
                        cause,
                        errno,
                    )),
                    None => Ok(Claimed::of(self, entry, Holder::Operation, since)),
                },
            }
        };
        let claimed = match claimed {
            Ok(claimed) => claimed,
            Err((name, cause, errno)) => {
                return Err(refuse_retitle(audit, name.as_str(), cause, errno));
            }
        };
        let Claimed {
            claim,
            handle,
            name,
            ..
        } = claimed;
        let volume = claim.volume;
        let written = write_held_title(handle, title, || {
            let mut registry = self.images.registry.lock();
            if let Some(at) = position(&registry.volumes, volume) {
                registry.volumes[at].title = *title;
            }
        });
        drop(claim);
        written.map_err(|(cause, errno)| refuse_retitle(audit, name.as_str(), cause, errno))?;
        audit_retitled(audit, name.as_str(), "owner");
        Ok(())
    }

    /// Settle the image volumes whose files lie on the volume registered as
    /// `host`, before it is detached: a plain detach is refused while one
    /// does, and a force detaches them first.
    ///
    /// # Errors
    ///
    /// `("image_attached", Busy)` for a plain detach of a volume holding one;
    /// the refusal of detaching one.
    pub(super) fn settle_image_dependents(
        &self,
        host: DriverHandle,
        force: bool,
        audit: &'static (dyn Sink + Sync),
    ) -> Result<(), (&'static str, Errno)> {
        self.clear_dependents(host, force, audit, Holder::Operation)
    }

    /// [`Self::settle_image_dependents`], each held `by` the holder named.
    fn clear_dependents(
        &self,
        host: DriverHandle,
        force: bool,
        audit: &'static (dyn Sink + Sync),
        by: Holder,
    ) -> Result<(), (&'static str, Errno)> {
        if !self.images.backed_by(host) {
            return Ok(());
        }
        if !force {
            return Err(("image_attached", Errno::Busy));
        }
        loop {
            let next = self
                .images
                .registry
                .lock()
                .volumes
                .iter()
                .find(|v| v.backing == Some(host) && v.name().is_some())
                .map(|v| v.id);
            let Some(id) = next else {
                return Ok(());
            };
            self.detach_held(audit, &id, DetachMode::Force, None, "backing_detached", by)
                .map_err(|errno| ("image_detach_refused", errno))?;
        }
    }

    /// Retire every image volume whose file lies on the volume registered as
    /// `host`, counting it gone — a stand-in now answers for it, or it was
    /// detached — and every one on those in turn. Parks on nothing.
    pub(super) fn retire_images_on(&self, host: DriverHandle) {
        if !self.images.backed_by(host) {
            return;
        }
        let Ok(wiring) = self.wiring() else {
            return;
        };
        let mut gone = Vec::new();
        if gone.try_reserve(1).is_err() {
            return self.retire_where(&ImageVolume::stranded, Vec::new(), wiring.audit);
        }
        gone.push(host);
        self.retire_where(&|_| false, gone, wiring.audit);
    }

    /// Retire every image volume `pick` names, or whose file lies on a volume
    /// in `gone`, which each one retired joins, for `backing_gone`: one pass in
    /// the order they entered, so one lying on an earlier one's volume, which
    /// entered after it, is reached too. One awaiting its driver is refused to
    /// its attach; one an operation holds is retired by that operation as it
    /// lets go, with what lies on it. Parks on nothing.
    fn retire_where(
        &self,
        pick: &dyn Fn(&ImageVolume) -> bool,
        mut gone: Vec<DriverHandle>,
        audit: &'static (dyn Sink + Sync),
    ) {
        gone.sort_unstable();
        // With no room to note a volume gone, what lies on it is found by
        // asking each volume whether it can still reach its file.
        let mut exhaustive = false;
        // Served volume ids grow as volumes enter, and the registry is kept in
        // their order: those at or past `from` are still to be looked at.
        let mut from = 0;
        loop {
            let (claim, handle) = {
                let mut registry = self.images.registry.lock();
                let since = registry.releases;
                let volumes = &mut registry.volumes;
                let start = volumes.partition_point(|v| v.served.id() < from);
                let picked = |v: &ImageVolume| {
                    pick(v)
                        || v.backing.is_some_and(|on| gone.binary_search(&on).is_ok())
                        || (exhaustive && v.stranded())
                };
                let Some(at) = volumes[start..]
                    .iter()
                    .position(|v| {
                        !v.custody.retiring
                            && matches!(v.stage, Stage::Awaiting | Stage::Mounted { .. })
                            && picked(v)
                    })
                    .map(|at| start + at)
                else {
                    return;
                };
                let entry = &mut volumes[at];
                // Each case below leaves the entry unpicked, so this only spares
                // the scan; it cannot skip one.
                from = entry.served.id().saturating_add(1);
                if entry.custody.claimed() {
                    entry.custody.retiring = true;
                    continue;
                }
                if matches!(entry.stage, Stage::Awaiting) {
                    entry.stage = Stage::Refused(Refusal::Driver(DriverError::DeviceOffline));
                    entry.served.retire();
                    let task = entry.task;
                    drop(registry);
                    volume_attach_wake(task);
                    continue;
                }
                entry.custody.holder = Holder::Operation;
                let claim = ImageClaim {
                    service: self,
                    volume: entry.served.id(),
                    task: entry.task,
                    by: Holder::Operation,
                    since,
                };
                (claim, entry.handle)
            };
            // Its file's volume is gone, and its work files with it.
            drop(self.retire_claimed(claim, "backing_gone", audit));
            match gone.binary_search(&handle) {
                Ok(_) => {}
                Err(_) if gone.try_reserve(1).is_err() => exhaustive = true,
                Err(at) => gone.insert(at, handle),
            }
        }
    }

    /// Retire the mounted image volume `claim` holds, for `cause`, in place:
    /// it leaves the namespace, answering it for its caller to let go, or
    /// stays lost while it held writes it had not published. Parks on
    /// nothing.
    fn retire_claimed(
        &self,
        claim: ImageClaim<'_>,
        cause: &'static str,
        audit: &'static (dyn Sink + Sync),
    ) -> Option<ImageVolume> {
        let retiring = {
            let mut registry = self.images.registry.lock();
            let volumes = &mut registry.volumes;
            let at = position(volumes, claim.volume)?;
            let entry = &mut volumes[at];
            let node = entry.node.take();
            let Stage::Mounted { name, format, .. } = &entry.stage else {
                return None;
            };
            (
                Arc::clone(&entry.served),
                node,
                ArrayString::<VOLUME_NAME_MAX>::from_str_truncating(name),
                *format,
                entry.handle,
                entry.id,
            )
        };
        let (served, node, name, format, handle, id) = retiring;
        self.retire_served(Some(&served), image_node(node));
        if LATE_FILESYSTEM.holds_unpublished(handle) {
            let source = {
                let registry = self.images.registry.lock();
                let volumes = &registry.volumes;
                position(volumes, claim.volume)
                    .map(|at| ArrayString::<FS_NAME_MAX>::from_str_truncating(&volumes[at].source))
            };
            Self::stand_in(
                handle,
                (source.as_ref().map_or("", ArrayString::as_str), format, id),
                MountAvailability::UnavailableLost,
            );
            record_retired(
                audit,
                IMAGE_VOLUME_RETIRED,
                RETIRED,
                name.as_str(),
                cause,
                "unpublished_writes_lost",
            );
            let mut registry = self.images.registry.lock();
            let volumes = &mut registry.volumes;
            if let Some(at) = position(volumes, claim.volume) {
                let entry = &mut volumes[at];
                if let Stage::Mounted { name, .. } = &mut entry.stage {
                    let name = core::mem::take(name);
                    entry.stage = Stage::Lost { name };
                }
            }
            return None;
        }
        if let Ok(vfs) = LATE_FILESYSTEM.vfs() {
            let _ = vfs.mounts_write().detach(handle, BindsWithin::Withdraw);
        }
        let _ = LATE_FILESYSTEM.unregister(handle);
        let _ = VOLUME_FOREST.unpublish(&id);
        record_retired(
            audit,
            IMAGE_VOLUME_RETIRED,
            RETIRED,
            name.as_str(),
            cause,
            "retracted",
        );
        let taken = self.images.take(claim.volume);
        drop(claim);
        taken
    }

    /// The driver serving an image volume over `server` is gone: the
    /// fail-closed stand-in answers it, reporting itself recovering, until
    /// the driver `devmgr` restarts serves it again. Answers its handle, what
    /// lies on it stranded. Parks on nothing.
    pub(super) fn image_server_lost(
        &self,
        server: u64,
        audit: &'static (dyn Sink + Sync),
    ) -> Option<DriverHandle> {
        let lost = {
            let mut registry = self.images.registry.lock();
            let volumes = &mut registry.volumes;
            volumes
                .iter_mut()
                .filter(|v| !v.custody.claimed())
                .find_map(|v| {
                    let Stage::Mounted {
                        name,
                        format,
                        server: serving,
                    } = &mut v.stage
                    else {
                        return None;
                    };
                    if *serving != Some(server) {
                        return None;
                    }
                    *serving = None;
                    Some((
                        ArrayString::<VOLUME_NAME_MAX>::from_str_truncating(name),
                        *format,
                        v.handle,
                        v.id,
                        ArrayString::<FS_NAME_MAX>::from_str_truncating(&v.source),
                    ))
                })
        };
        let (name, format, handle, id, source) = lost?;
        let unpublished = LATE_FILESYSTEM.holds_unpublished(handle);
        Self::stand_in(handle, (&source, format, id), MountAvailability::Recovering);
        record_retired(
            audit,
            IMAGE_VOLUME_RETIRED,
            RETIRED,
            &name,
            "server_lost",
            if unpublished {
                "unpublished_writes_lost"
            } else {
                "nothing_unpublished"
            },
        );
        Some(handle)
    }
}

impl SessionObserver for RuntimeVolumeService {
    /// Leave every image volume none of whose login sessions is still open
    /// owed its detach, forced: those sessions' processes are gone, and the
    /// volume goes with them, on the settling tasks. What cannot be noted for
    /// want of memory is caught at the next telling.
    fn session_ended(&self, closed: &dyn Fn(ProcId) -> bool) {
        let Some(held) = self.images.held_sessions() else {
            return;
        };
        let mut ended: Vec<ProcId> = Vec::new();
        if ended.try_reserve_exact(held.len()).is_err() {
            return;
        }
        // In order, as `held` is.
        ended.extend(
            held.into_iter()
                .filter(|&session| session != tairix_kernel_sec::ROOT_SESSION && closed(session)),
        );
        if !ended.is_empty() && self.images.end_sessions(&ended) {
            session_reaper::owe_work();
        }
    }

    fn settle(&self) -> bool {
        self.settle_image()
    }

    fn owes(&self) -> bool {
        self.images.owes()
    }
}

/// The image `file` as a device, read-only until the volume it holds is known
/// not to be attached already, the file's identity, and the volume it holds,
/// as `attach` expects it: or why it goes no further.
///
/// The file is held through its own volume, never a bind it was opened
/// through, so the image leaves only with its eject, its session or that
/// volume.
fn recognised(
    file: NodeHandle,
    attach: &ImageAttach<'_>,
) -> Result<(ImageBlock, FileId, ProbedVolume), Unprepared> {
    let projection = LATE_FILESYSTEM
        .vfs()
        .map_err(|errno| Unprepared::Failed("no_mount_table", errno))?
        .mounts()
        .kept_through(&file.projection)
        .map_err(|err| match err {
            VfsError::NotSupported => Unprepared::Failed("file_unanchored", Errno::NotSupported),
            other => Unprepared::Failed("file_unreadable", other.to_errno()),
        })?;
    let file = NodeHandle { projection, ..file };
    let (mut block, stat) = ImageBlock::new(file, false)
        .map_err(|errno| Unprepared::Failed("file_unreadable", errno))?;
    if stat.id.volume == FileId::NONE.volume {
        return Err(Unprepared::Failed("file_unidentified", Errno::NotSupported));
    }
    let probed = probe(&mut block).map_err(Unprepared::Refused)?;
    if attach
        .expects
        .is_some_and(|expects| !expects.contains(probed.fstype))
    {
        return Err(Unprepared::Refused(ImageRefusal::NotExpected));
    }
    if records_owners(probed.fstype) {
        return Err(Unprepared::Refused(ImageRefusal::RecordsOwners));
    }
    block.form = probed.form;
    Ok((block, stat.id, probed))
}

/// Recognise the volume `block` holds, or say why it holds none the kernel
/// attaches: a disk format in its whole blocks, else an archive in its exact
/// bytes, which `block` is then read as.
fn probe(block: &mut ImageBlock) -> Result<ProbedVolume, ImageRefusal> {
    if block.blocks > 0 {
        let mut device = ByteDevice::new(&mut *block).map_err(|_| ImageRefusal::Unreadable)?;
        match tairix_fsprobe::recognise(&mut device) {
            Ok(Some(Recognised::Volume(volume))) => return Ok(volume),
            Ok(Some(Recognised::RaidMember(_))) => return Err(ImageRefusal::RaidMember),
            Ok(Some(Recognised::RigidDiskBlock)) => return Err(ImageRefusal::Partitioned),
            Ok(None) => {}
            Err(_) => return Err(ImageRefusal::Unreadable),
        }
    }
    if block.len == 0 {
        return Err(ImageRefusal::Unrecognised);
    }
    block.read_as_archive();
    let len = block.len;
    let mut device = ByteDevice::new(&mut *block).map_err(|_| ImageRefusal::Unreadable)?;
    match tairix_fsprobe::archive::recognise(&mut device, len) {
        Ok(Some(volume)) => Ok(volume),
        Ok(None) => Err(ImageRefusal::Unrecognised),
        Err(_) => Err(ImageRefusal::Unreadable),
    }
}

/// The file name an image was attached from and the name its attach asked
/// for, each held as the volume's own.
fn owned_names(
    file_name: &str,
    asked: Option<&str>,
) -> Result<(String, Option<String>), Unprepared> {
    let owned = |text: &str| {
        concatenated(&[text]).ok_or(Unprepared::Failed("name_unallocated", Errno::OutOfMemory))
    };
    Ok((owned(file_name)?, asked.map(owned).transpose()?))
}

/// Whether an attach asking `access` of an image whose file the caller may
/// write where `writable` makes it writable, and for an archive the
/// directory its replacement goes in.
fn granted_access(
    access: ImageAccess,
    writable: bool,
    directory: Option<&ImageDirectory>,
    block: &ImageBlock,
    file: &FileId,
    archive: bool,
) -> Result<(bool, Option<ImageDirectory>), Unprepared> {
    let place = if archive && access != ImageAccess::ReadOnly && writable {
        archive_place(block, file, directory)?
    } else {
        None
    };
    let may_write = writable && (!archive || place.is_some());
    let granted = match access {
        ImageAccess::ReadOnly => false,
        ImageAccess::WritableWherePossible => may_write,
        ImageAccess::Writable if may_write => true,
        ImageAccess::Writable => {
            return Err(Unprepared::Failed("write_protected", Errno::ReadOnly));
        }
    };
    Ok((granted, place))
}

/// Whether the archive `block` reads may be written: its replacement can
/// keep its security record — the attacher owns it, or its volume presents
/// every node as one owner's — `directory` names it, and the attacher may
/// change the directory's entries, the directory then held through its
/// volume's own projection as the file is. A directory naming another file
/// refuses the attach.
fn archive_place(
    block: &ImageBlock,
    file: &FileId,
    directory: Option<&ImageDirectory>,
) -> Result<Option<ImageDirectory>, Unprepared> {
    let Some(directory) = directory else {
        return Ok(None);
    };
    let who = block.acting();
    let owned = FS_SERVICE
        .stat_node(&block.file, &who)
        .map_err(|errno| Unprepared::Failed("file_unreadable", errno))?
        .uid
        == block.uid
        || !FS_SERVICE
            .records_ownership_node(&block.file)
            .map_err(|errno| Unprepared::Failed("file_unreadable", errno))?;
    let named = FS_SERVICE
        .stat(
            block.uid,
            &block.caps,
            At {
                start: Start::Dir(&directory.dir),
                path: &directory.name,
            },
            FinalLink::Keep,
        )
        .map_err(|errno| Unprepared::Failed("file_misnamed", errno))?;
    if named.id != *file {
        return Err(Unprepared::Failed("file_misnamed", Errno::Stale));
    }
    let changeable = [Access::Write, Access::Execute].into_iter().all(|want| {
        FS_SERVICE
            .access_node(&directory.dir, &who, want, Judged::AsQuery)
            .is_ok()
    });
    // A directory reached through a read-only view stays read-only: its
    // volume's own projection, which the commit holds it through, would
    // write past the view.
    if !owned || !changeable || directory.dir.projection.is_read_only() {
        return Ok(None);
    }
    let projection = LATE_FILESYSTEM
        .vfs()
        .map_err(|errno| Unprepared::Failed("no_mount_table", errno))?
        .mounts()
        .kept_through(&directory.dir.projection)
        .map_err(|err| Unprepared::Failed("directory_unanchored", err.to_errno()))?;
    let mut name = String::new();
    name.try_reserve_exact(directory.name.len())
        .map_err(|_| Unprepared::Failed("name_unallocated", Errno::OutOfMemory))?;
    name.push_str(&directory.name);
    Ok(Some(ImageDirectory {
        dir: NodeHandle {
            projection,
            ..directory.dir.clone()
        },
        name,
        name_key: directory.name_key,
    }))
}

/// Why `entry` may not be retitled for `uid` now: only its owner retitles it,
/// and only while a driver serves it and nothing else acts on it.
fn retitle_refusal(entry: &ImageVolume, uid: u32) -> Option<(&'static str, Errno)> {
    if entry.owner != uid {
        return Some(("not_owner", Errno::PermissionDenied));
    }
    if entry.custody.claimed() || entry.custody.answering || entry.custody.owed != Owed::Nothing {
        return Some(("volume_busy", Errno::Busy));
    }
    match entry.stage {
        Stage::Mounted {
            server: Some(_), ..
        } if !entry.stranded() => None,
        _ => Some(("volume_unserved", Errno::DeviceOffline)),
    }
}

/// Why an image is not attached, given its driver's refusal.
const fn refusal_of(why: DriverError) -> ImageRefusal {
    match why {
        DriverError::Unsupported | DriverError::NotImplemented => ImageRefusal::UnsupportedVariant,
        DriverError::DeviceFault
        | DriverError::MediumError
        | DriverError::DeviceOffline
        | DriverError::Stale
        | DriverError::PermissionDenied => ImageRefusal::Unreadable,
        _ => ImageRefusal::Damaged,
    }
}

/// The identity of the volume `uid` attaches from the file `file`, which the
/// probe found holds the volume `probed`.
fn identity(uid: u32, file: &FileId, probed: &[u8; 16]) -> [u8; 16] {
    let mut hash = Sha256Stream::new();
    hash.update(IDENTITY_CONTEXT);
    hash.update(&uid.to_le_bytes());
    hash.update(&file.volume);
    hash.update(&file.node.to_le_bytes());
    hash.update(probed);
    let digest = hash.finalize();
    let mut id = [0; 16];
    id.copy_from_slice(&digest[..16]);
    id
}

/// The last component of `path`.
fn last_component(path: &str) -> &str {
    path.rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(path)
}

/// `name` without its extension.
fn stem(name: &str) -> &[u8] {
    match name.rfind('.') {
        Some(dot) if dot > 0 => &name.as_bytes()[..dot],
        _ => name.as_bytes(),
    }
}

/// Record that the image `label` was refused for `why`, and answer it.
fn refused_reply(
    audit: &'static (dyn Sink + Sync),
    label: &str,
    why: ImageRefusal,
) -> ImageAttachReply {
    record(
        audit,
        Level::Warn,
        IMAGE_ATTACH_REFUSED,
        ATTACH_REFUSED,
        label,
        why.as_str(),
    );
    ImageAttachReply::Refused(why)
}

/// The message of a refused attach.
const ATTACH_REFUSED: &str = "image-volume: attach refused";

/// The cause and refusal of an attach whose volume was taken out before it
/// could be answered.
const GONE: (&str, Errno) = ("volume_gone", Errno::NotAttached);

/// Why a mounted image volume's attach could not be answered as attached.
enum Unfinished {
    /// The directory it was also to be shown at refused it.
    Target(Errno),
    /// Anything else, with its cause.
    Failed(&'static str, Errno),
}

/// The message of a refused detach.
const DETACH_REFUSED: &str = "image-volume: detach refused";

/// The message of a retirement.
const RETIRED: &str = "image-volume: retired";

/// Record that the image volume `name` was detached for `cause`, whether by
/// a force, and what the force gave up for it.
fn audit_detached(
    audit: &'static (dyn Sink + Sync),
    name: &str,
    cause: &str,
    forced: bool,
    given_up: GivenUp,
) {
    log(
        audit,
        &Event {
            level: if forced || given_up != GivenUp::default() {
                Level::Warn
            } else {
                Level::Info
            },
            id: IMAGE_VOLUME_DETACHED,
            message: "image-volume: detached",
            fields: &[
                Field {
                    key: "volume",
                    value: FieldValue::Str(name),
                },
                Field {
                    key: "cause",
                    value: FieldValue::Str(cause),
                },
                Field {
                    key: "outcome",
                    value: FieldValue::Str(if forced { "forced" } else { "clean" }),
                },
                Field {
                    key: "revoked_nodes",
                    value: FieldValue::UnsignedInt(given_up.revoked as u64),
                },
                Field {
                    key: "unflushed",
                    value: FieldValue::Bool(given_up.unflushed),
                },
            ],
        },
    );
}

#[cfg(test)]
impl RuntimeVolumeService {
    /// Run `during` while network volume `id` is held as an attach or detach
    /// holds it, then let it go: one retired meanwhile takes what lies on it.
    pub(super) fn while_share_held<T>(&self, id: &[u8; 16], during: impl FnOnce() -> T) -> T {
        let audit = self.wiring().expect("wired").audit;
        self.network
            .while_held(id, audit, &|gone| self.retire_images_on(gone), during)
    }

    /// Run `during` while image volume `id` is held as a slow eject or serve
    /// holds it, then let it go.
    pub(super) fn while_image_held<T>(&self, id: &[u8; 16], during: impl FnOnce() -> T) -> T {
        let held = {
            let mut registry = self.images.registry.lock();
            let since = registry.releases;
            registry
                .volumes
                .iter_mut()
                .find(|v| v.id == *id && !v.custody.claimed())
                .map(|entry| Claimed::of(self, entry, Holder::Operation, since).claim)
        };
        assert!(held.is_some(), "an image volume nothing holds");
        let outcome = during();
        drop(held);
        outcome
    }
}

#[cfg(test)]
#[path = "image_volume_tests.rs"]
mod tests;
