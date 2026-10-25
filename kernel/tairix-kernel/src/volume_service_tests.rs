//! Host tests for the runtime volume attach/detach service.
//!
//! The lifecycle test drives the whole D3b path in-process against the
//! real global boot statics (`LATE_FILESYSTEM`, `VOLUME_FOREST`,
//! `VOLUME_SERVICE`, `LATE_IDENTITY`, `LATE_STORAGE_GID`, the recorded
//! shared-memory facility): a FAT32 image is served over a genuine call
//! endpoint from another thread (the shape a user-space block driver
//! has), attached under the storage-group identity map (D3d), read and
//! written through the production `fs_*` service as a group member —
//! with a non-member's write refused — and detached. It is the **only**
//! test in this crate that touches those statics (the system-mount tests
//! deliberately avoid them), and it runs as one sequential test function
//! so the set-once cells are installed exactly once.
//!
//! Its scenarios extend past that first volume to the surprise-removal,
//! re-insert, sibling-extent, and unknown-medium paths, and to the link
//! round trip over an attached ARXFS volume — the one place both link
//! kinds are driven through the real service and therefore through the
//! production wrapper chain, which every other link test bypasses — and to
//! descriptors held across renames, unlinks, detaches and removals on real
//! formats, which no in-memory driver can stand in for.

extern crate std;

use std::boxed::Box;
use std::string::String;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc as StdArc, Mutex};
use std::thread;
use std::time::Duration;
use std::vec;
use std::vec::Vec;

use tairix_abi::blkio::{
    encode_error_completion, BlkCompletion, BlkMedium, BlkOp, BlkRemovability,
    BlkRemovabilityEvidence, BlkRequest, BlkTransport, BLK_COMPLETION_LEN, BLK_DATA_LEN,
    BLK_REQUEST_LEN,
};
use tairix_abi::driver::block::Block;
use tairix_abi::driver::filesystem::NameMatching;
use tairix_abi::driver::filesystem::{
    FilesystemRead, FilesystemSecurity, FilesystemStats, FilesystemWrite, NodeKind, NodeSecurity,
    Unpinned, VolumeStats,
};
use tairix_abi::fs::{
    OpenFlags, FS_INITIAL_UMASK, FS_NEW_DIR_MODE, FS_NEW_FILE_MODE, FS_OWNER_UNCHANGED,
};
use tairix_abi::sysinfo::{MountAvailability, MountRecord};
use tairix_abi::volume::{VolumeAttachRequest, VolumeDetachRequest, VolumeFsType};
use tairix_abi::{
    CapabilityId, CapabilityQuery, DriverError, DriverHandle, Errno, FileStat, LockRange,
    RenameFlags, UnlinkFlags,
};
use tairix_caps::CapabilitySet;
use tairix_drv_fs_adfs::{Adfs, AdfsVariant};
use tairix_drv_fs_arxfs::{EntropySource, Keying, ARXFS};
use tairix_drv_fs_ext4::Ext4;
use tairix_drv_fs_fat32::Fat32;
use tairix_drv_fs_iso9660::Iso9660;
use tairix_drv_fs_udf::{FormatOptions, Udf};
use tairix_hash::{HashSeed, SipHash13};
use tairix_kernel_core::devres::{install_shared_mem_facility, SharedChunk, SharedMemFacility};
use tairix_kernel_core::filelock::{self, LockedNode, Request};
use tairix_kernel_core::fs::{
    Acting, At, Authority, CreateMode, FilesystemService, FinalLink, FrozenAuthority, Grant,
    NodeHandle, Opened, Start,
};
use tairix_kernel_core::fswatch::WatchRegistry;
use tairix_kernel_core::{sharedreg, Path, Vfs};
use tairix_kernel_ipc::{CallEndpoint, CallEndpointLimits, EndpointId, RecvCall};
use tairix_kernel_sec::captable::TaskCapabilities;
use tairix_kernel_sec::{
    GroupId, GroupRecord, IdentityTableBuilder, ProcessId, UserId, UserRecord,
};
use tairix_log::{Event, FieldValue, Sink};
use tairix_reclaim::{FreeMemorySource, MemoryPressure};

use crate::root_mount::LATE_IDENTITY;
use crate::system_mount::{FS_SERVICE, LATE_FILESYSTEM, VOLUME_FOREST};
use crate::test_support::{RamBlock, BLOCK_SIZE};
use crate::volume_policy::LATE_STORAGE_GID;
use crate::volume_service::{RuntimeVolumeService, UnavailableFs, VOLUME_SERVICE};
use tairix_kernel_core::callreg::EndpointVanishObserver as _;
use tairix_kernel_core::VolumeService as _;

/// One audited decision: its event id, the volume it names, its cause, and
/// the counts it carries.
struct Audit {
    id: u32,
    volume: Option<String>,
    cause: Option<String>,
    counts: Vec<(String, u64)>,
}

/// Records every event the service audits, so a scenario can assert what it
/// was told.
struct Recorded(Mutex<Vec<Audit>>);

impl Sink for Recorded {
    fn write_event(&self, event: &Event<'_>) {
        let field = |key: &str| {
            event
                .fields
                .iter()
                .find(|field| field.key == key)
                .and_then(|field| match field.value {
                    FieldValue::Str(text) => Some(String::from(text)),
                    _ => None,
                })
        };
        let counts = event
            .fields
            .iter()
            .filter_map(|field| match field.value {
                FieldValue::UnsignedInt(count) => Some((String::from(field.key), count)),
                _ => None,
            })
            .collect();
        let audit = Audit {
            id: event.id.0,
            volume: field("volume"),
            cause: field("cause"),
            counts,
        };
        self.0.lock().unwrap().push(audit);
    }
}

static SINK: Recorded = Recorded(Mutex::new(Vec::new()));

/// The causes of every `id` event audited for `volume`, in order.
fn audited(id: u32, volume: &str) -> Vec<Option<String>> {
    SINK.0
        .lock()
        .unwrap()
        .iter()
        .filter(|audit| audit.id == id && audit.volume.as_deref() == Some(volume))
        .map(|audit| audit.cause.clone())
        .collect()
}

/// The `key` count of every `id` event audited for `volume`, in order.
fn audited_count(id: u32, volume: &str, key: &str) -> Vec<Option<u64>> {
    SINK.0
        .lock()
        .unwrap()
        .iter()
        .filter(|audit| audit.id == id && audit.volume.as_deref() == Some(volume))
        .map(|audit| {
            audit
                .counts
                .iter()
                .find(|(named, _)| named == key)
                .map(|&(_, count)| count)
        })
        .collect()
}

/// The service's own event ids, as the volume manager and an operator read
/// them back.
const ATTACH_REFUSED: u32 = 4173;
const DETACH_REFUSED: u32 = 4175;
const SURPRISE_REMOVED_CLEAN: u32 = 4176;
const SURPRISE_REMOVED_LOST: u32 = 4178;
const FORCE_UNMOUNTED: u32 = 4179;
const ATTACHED_READ_ONLY: u32 = 4180;
const NAME_TAKEN: u32 = 4181;

/// The address-space registry endpoint teardown revokes per-endpoint
/// grants against.
///
/// Destroying an endpoint and withdrawing the authority naming its id are
/// one step, so the teardown takes the registry that holds those grants.
/// These scenarios drive the block service directly rather than through a
/// granted task, so it stays empty and nothing is revoked; it exists so the
/// scenarios exercise the production teardown, not a variant of it.
static ASPACES: tairix_sync::RwLock<tairix_kernel_core::aspace::AddressSpaceRegistry> =
    tairix_sync::RwLock::new(tairix_kernel_core::aspace::AddressSpaceRegistry::new());

/// A fixed, ample memory source for the pressure gauge.
struct AmpleSource;
impl FreeMemorySource for AmpleSource {
    fn free_bytes(&self) -> usize {
        1 << 30
    }
    fn total_bytes(&self) -> usize {
        1 << 30
    }
}
static AMPLE: AmpleSource = AmpleSource;

/// A caller that holds no capability: the per-inode records on the test
/// volume gate nothing, so the secured read needs none.
struct NoCaps;
impl CapabilityQuery for NoCaps {
    fn holds(&self, _cap: CapabilityId) -> bool {
        false
    }
}

/// The storage group's gid on this test system, and the two ordinary
/// principals the identity-map assertions run as: a member of the group
/// and a non-member.
const STORAGE_GID: u32 = 100;
const MEMBER_UID: u32 = 1000;
const OUTSIDER_UID: u32 = 2000;

/// A principal whose primary group is the one owning the ext ACL fixture's
/// `/f`, so the ACL, not the mode's group triad, decides its access.
const GROUP_PEER_UID: u32 = 3000;
const FIXTURE_GID: u32 = 1000;

/// A caller holding only `CAP_FS_CHOWN`, the authority an owner reassignment
/// needs.
struct Chown;
impl CapabilityQuery for Chown {
    fn holds(&self, cap: CapabilityId) -> bool {
        cap == CapabilityId::FS_CHOWN
    }
}

/// A descriptor of what an open through the production service pinned,
/// acting as its opener; dropped, it lets the pin go as a close does.
struct Descriptor {
    handle: Option<NodeHandle>,
    uid: u32,
    granted: bool,
}

/// What a creator that names no mode asks of a new file under the initial
/// umask, when `flags` may create: born `0o644`.
fn creating(flags: OpenFlags) -> CreateMode {
    if flags.contains(OpenFlags::CREATE) {
        CreateMode::new(FS_NEW_FILE_MODE, FS_INITIAL_UMASK)
            .expect("a creator's default is in range")
    } else {
        CreateMode::NONE
    }
}

/// `path`, an absolute one, as the filesystem service takes it.
fn root(path: &str) -> At<'_> {
    At {
        start: Start::Root,
        path,
    }
}

/// What a creator that names no mode asks of a new directory under the
/// initial umask: born `0o755`.
fn new_dir() -> CreateMode {
    CreateMode::new(FS_NEW_DIR_MODE, FS_INITIAL_UMASK).expect("a creator's default is in range")
}

impl Descriptor {
    /// The opener's own descriptor of `path`, opened as `uid` under `flags`.
    fn open(uid: u32, path: &str, flags: OpenFlags) -> Result<Self, Errno> {
        match FS_SERVICE.open(uid, &NoCaps, root(path), flags, creating(flags))? {
            Opened::Node {
                node,
                projection,
                opened_as,
                at_floor,
                ..
            } => Ok(Self {
                handle: Some(NodeHandle {
                    node,
                    projection,
                    authority: Authority::Holder,
                    opened_as,
                    at_floor,
                }),
                uid,
                granted: false,
            }),
            Opened::Catalog(_) => Err(Errno::IsADirectory),
        }
    }

    /// What an `fd_grant` of this descriptor hands on: the same node, under
    /// its opener's identity frozen with a grant.
    fn granted(&self) -> Self {
        let authority = Authority::Frozen(FrozenAuthority {
            uid: self.uid,
            caps: CapabilitySet::empty(),
            grant: Some(Grant {
                write_ceiling: None,
            }),
        });
        Self {
            handle: Some(NodeHandle {
                authority,
                ..self.handle().clone()
            }),
            uid: self.uid,
            granted: true,
        }
    }

    fn handle(&self) -> &NodeHandle {
        self.handle.as_ref().expect("held until dropped")
    }

    fn acting(&self) -> Acting<'static> {
        Acting {
            uid: self.uid,
            caps: &NoCaps,
            granted: self.granted,
        }
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        FS_SERVICE.read_node(self.handle(), &self.acting(), offset, buf)
    }

    fn write(&self, offset: u64, data: &[u8]) -> Result<usize, Errno> {
        FS_SERVICE.write_node(self.handle(), &self.acting(), offset, false, data)
    }

    fn stat(&self) -> Result<FileStat, Errno> {
        FS_SERVICE.stat_node(self.handle(), &self.acting())
    }

    /// The key a lock taken through this descriptor is filed under.
    fn key(&self) -> LockedNode {
        LockedNode::of(&self.handle().node)
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let volume = StdArc::clone(handle.node.volume());
            drop(handle);
            FS_SERVICE.reap(&volume);
        }
    }
}

/// Read `path` at `offset` as `uid` through a descriptor of its own.
fn read_as(uid: u32, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
    Descriptor::open(uid, path, OpenFlags::READ)?.read(offset, buf)
}

/// Write `data` to `path` at `offset` as `uid` through a descriptor of its
/// own.
fn write_as(uid: u32, path: &str, offset: u64, data: &[u8]) -> Result<usize, Errno> {
    Descriptor::open(uid, path, OpenFlags::WRITE)?.write(offset, data)
}

/// Open `path` as `uid` under `flags` and close it again: what an open alone
/// does.
fn open_as(uid: u32, path: &str, flags: OpenFlags) -> Result<(), Errno> {
    Descriptor::open(uid, path, flags).map(drop)
}

/// 64 MiB — comfortably past the FAT32 minimum cluster count.
const SECTORS_64MIB: u64 = (64 << 20) / 512;

/// Backing size for the ARXFS fixture volume: large enough for the
/// format's metadata plus a handful of nodes, small enough that serving it
/// block by block over the call endpoint stays quick.
const SECTORS_8MIB: u64 = (8 << 20) / 512;

/// Inode count the ARXFS fixture volume is formatted with.
const ARXFS_INODES: u32 = 64;

/// The shared-memory facility double behind the kernel window: one leaked
/// `BLK_DATA_LEN` buffer serves every translation (the test creates one
/// region).
struct TestFacility {
    window: *mut u8,
}
// SAFETY: the window is leaked process-lifetime memory; the facility only
// hands the raw pointer out and frees nothing.
unsafe impl Sync for TestFacility {}

impl SharedMemFacility for TestFacility {
    fn alloc_region(&self, pages: u64) -> Result<Vec<SharedChunk>, Errno> {
        // A single contiguous chunk (the block-service data window is small).
        Ok(vec![SharedChunk {
            phys_base: 0x4000_0000,
            order: 0,
            pages,
        }])
    }
    fn map_region(
        &self,
        chunks: &[SharedChunk],
        _memory: tairix_kernel_mem::SharedMemory,
    ) -> Result<u64, Errno> {
        Ok(0x9000_0000 + chunks[0].phys_base)
    }
    fn unmap_region(&self, _base: u64, _len: usize) -> Result<(), Errno> {
        Ok(())
    }
    fn free_region(&self, _chunks: &[SharedChunk], _memory: tairix_kernel_mem::SharedMemory) {}
    fn kernel_window(&self, chunks: &[SharedChunk], _len: usize) -> Option<core::ptr::NonNull<u8>> {
        // The single-chunk region resolves to the one leaked test window.
        if chunks.len() == 1 {
            core::ptr::NonNull::new(self.window)
        } else {
            None
        }
    }
}

/// The served device: image bytes, the storage medium its geometry reply
/// declares, a flush counter, and a count of the geometry queries served.
///
/// The geometry count is how many times a consumer *connected* to this
/// device: connecting is the only thing that asks for the geometry, so the
/// count is the observable form of "how many block clients exist for this
/// one device" — the fact every volume on a disk must share one
/// (`plans/FIX-IO.md`).
struct ServedDevice {
    block: RamBlock,
    medium: Option<BlkMedium>,
    flushes: usize,
    geometries: usize,
    /// The block whose write the stick is pulled straight after, if any.
    pull_after: Option<u64>,
    pulled: bool,
    /// Taken by the first request: the server says it has the request, then
    /// answers nothing until the gate opens — a device that stops answering.
    hold_first: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
    /// The publication the device's medium is served under: a request made
    /// for another is refused stale, as a medium-changing driver's is.
    binding: u64,
}

impl ServedDevice {
    /// A device whose geometry reply declares the medium its block backing
    /// reports.
    fn new(block: RamBlock) -> Self {
        Self {
            medium: block.medium(),
            block,
            flushes: 0,
            geometries: 0,
            pull_after: None,
            pulled: false,
            hold_first: None,
            binding: 0,
        }
    }

    /// A device pulled the moment a write covering `lba` lands: that write
    /// is kept, and every request after it fails as a removed stick's does.
    fn pulled_after(block: RamBlock, lba: u64) -> Self {
        Self {
            pull_after: Some(lba),
            ..Self::new(block)
        }
    }

    /// A device whose geometry reply carries a medium word this ABI does not
    /// define — a driver naming a medium the client cannot recognise.
    fn unclassified(block: RamBlock) -> Self {
        Self {
            medium: None,
            ..Self::new(block)
        }
    }
}

/// Serve blkio requests over `endpoint` against `device`, moving data
/// through the raw `window` pointer, until `stop` is set.
fn serve(
    endpoint: StdArc<CallEndpoint>,
    window: usize,
    device: StdArc<Mutex<ServedDevice>>,
    stop: StdArc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let window = window as *mut u8;
        while !stop.load(Ordering::Relaxed) {
            let RecvCall::Received(call) = endpoint.recv_call(BLK_REQUEST_LEN) else {
                thread::yield_now();
                continue;
            };
            let held = device.lock().unwrap().hold_first.take();
            if let Some((received, gate)) = held {
                received.send(()).unwrap();
                gate.recv().unwrap();
            }
            let mut reply = [0u8; BLK_COMPLETION_LEN];
            let len = match BlkRequest::decode(&call.request) {
                Err(err) => encode_error_completion(&mut reply, err).unwrap(),
                Ok(_) if device.lock().unwrap().pulled => {
                    encode_error_completion(&mut reply, Errno::DeviceOffline).unwrap()
                }
                Ok(request)
                    if request.op != BlkOp::Geometry
                        && request.binding != device.lock().unwrap().binding =>
                {
                    encode_error_completion(&mut reply, Errno::Stale).unwrap()
                }
                Ok(request) => {
                    let mut device = device.lock().unwrap();
                    let bytes = request.blocks as usize * BLOCK_SIZE;
                    match request.op {
                        BlkOp::Geometry => {
                            device.geometries += 1;
                            let geometry = device.block.geometry().unwrap();
                            BlkCompletion {
                                block_size: geometry.block_size,
                                block_count: geometry.block_count,
                                flags: 0,
                                medium: device.medium,
                                disc_layout: None,
                                name: device.block.device_name(),
                                binding: device.binding,
                            }
                            .encode(&mut reply)
                            .unwrap()
                        }
                        BlkOp::Read => {
                            let mut buf = vec![0u8; bytes];
                            match device.block.read_blocks(request.lba, &mut buf) {
                                // SAFETY: test window of BLK_DATA_LEN bytes,
                                // protocol-alternated access, bytes bounded
                                // by the serving contract.
                                Ok(()) => unsafe {
                                    core::ptr::copy_nonoverlapping(buf.as_ptr(), window, bytes);
                                    BlkCompletion::default().encode(&mut reply).unwrap()
                                },
                                Err(err) => {
                                    encode_error_completion(&mut reply, err.as_errno()).unwrap()
                                }
                            }
                        }
                        BlkOp::Write => {
                            let mut buf = vec![0u8; bytes];
                            // SAFETY: as for the read arm.
                            unsafe {
                                core::ptr::copy_nonoverlapping(window, buf.as_mut_ptr(), bytes);
                            }
                            match device.block.write_blocks(request.lba, &buf) {
                                Ok(()) => {
                                    let end = request.lba + u64::from(request.blocks);
                                    device.pulled = device
                                        .pull_after
                                        .is_some_and(|lba| (request.lba..end).contains(&lba));
                                    BlkCompletion::default().encode(&mut reply).unwrap()
                                }
                                Err(err) => {
                                    encode_error_completion(&mut reply, err.as_errno()).unwrap()
                                }
                            }
                        }
                        BlkOp::Flush => {
                            device.flushes += 1;
                            BlkCompletion::default().encode(&mut reply).unwrap()
                        }
                    }
                }
            };
            endpoint.reply(call.ticket, &reply[..len], &SINK).unwrap();
        }
    })
}

/// Build, register, and return the per-LUN block-service endpoint, owned
/// by the task `owner` (the serving driver's identity — the one the
/// endpoint-teardown path matches on unplug).
fn register_endpoint(id: u64, owner: u64) -> StdArc<CallEndpoint> {
    let creator = TaskCapabilities::derive(
        ProcessId(owner),
        UserId(0),
        CapabilitySet::empty(),
        CapabilitySet::empty(),
        &SINK,
    );
    let endpoint = StdArc::new(
        CallEndpoint::create(
            EndpointId(id),
            &creator,
            CapabilitySet::empty(),
            CapabilitySet::empty(),
            CallEndpointLimits {
                max_request: u32::try_from(BLK_REQUEST_LEN).unwrap(),
                max_reply: u32::try_from(BLK_COMPLETION_LEN).unwrap(),
                capacity: 4,
            },
            tairix_abi::RLIMIT_INFINITY,
            &SINK,
        )
        .expect("endpoint"),
    );
    tairix_kernel_core::callreg::register(StdArc::clone(&endpoint), &SINK)
        .expect("register endpoint");
    endpoint
}

#[test]
fn a_recovered_volume_is_served_only_under_its_recovered_posture() {
    let vfs = Vfs::with_default_layout(UserId(0), GroupId(0));
    let handle = tairix_abi::DriverHandle::from_raw(0x7A11).expect("handle");
    let path = tairix_kernel_core::Path::parse("/Storage/usb9").expect("path");
    vfs.mounts_write()
        .attach_at(
            tairix_kernel_core::Attachment::new(handle, None, super::mount_flags(false)),
            path.clone(),
            super::mount_template(None),
        )
        .expect("the volume was attached writable");
    let mut served = false;
    super::reattach_then_serve(
        &vfs,
        tairix_kernel_core::Attachment::new(handle, None, super::mount_flags(true)),
        path.clone(),
        super::mount_template(None),
        || {
            served = true;
            assert!(
                vfs.mounts().resolve(&path).is_read_only(),
                "the live driver is served only once the mount is read-only"
            );
            Ok(())
        },
    )
    .expect("the volume is served again");
    assert!(served);
}

/// The stand-in answers a climb and a byte write with the transient device
/// fault a vanished disk is, never a refusal that would read as a format
/// limit.
#[test]
fn the_unavailable_stand_in_faults_a_climb_and_every_byte_write() {
    let mut fs = UnavailableFs;
    let root = fs.root();
    assert_eq!(fs.parent(root), Err(DriverError::DeviceFault));
    assert_eq!(fs.write_at(root, 0, b"x"), Err(DriverError::DeviceFault));
    assert_eq!(fs.write_all(root, 0, b"x"), Err(DriverError::DeviceFault));
    assert_eq!(fs.truncate(root, 0), Err(DriverError::DeviceFault));
    assert_eq!(fs.punch(root, 0, 1), Err(DriverError::DeviceFault));
}

/// The stand-in refuses a new hold with the device fault, and every release
/// of one taken on the driver it replaced succeeds: that hold went with it.
#[test]
fn the_unavailable_stand_in_refuses_a_hold_and_settles_every_release() {
    let mut fs = UnavailableFs;
    let root = fs.root();
    assert_eq!(fs.pin(root), Err(DriverError::DeviceFault));
    assert_eq!(fs.unpin(root), Ok(Unpinned::Released));
    assert_eq!(fs.release_pins(), Ok(()));
}

#[test]
fn an_unwired_service_fails_closed() {
    let service = RuntimeVolumeService::new();
    let attach = VolumeAttachRequest {
        endpoint: 1,
        window: 1,
        first_lba: 0,
        blocks: 8,
        fstype: VolumeFsType::Fat32,
        name: b"usb1",
    };
    assert_eq!(service.attach(&attach), Err(Errno::NotImplemented));
    assert_eq!(
        service.detach(&VolumeDetachRequest {
            volume_id: [7; 16],
            force: false
        }),
        Err(Errno::NotImplemented)
    );
}

/// The identity table the lifecycle test installs: the system principal,
/// a member of the storage group, and a non-member.
fn identity_with_storage_member() -> tairix_kernel_sec::IdentityTable {
    let mut identity = IdentityTableBuilder::new();
    for gid in [0, STORAGE_GID, OUTSIDER_UID, FIXTURE_GID] {
        identity.push_group(GroupRecord { gid: GroupId(gid) });
    }
    for (uid, gid) in [
        (0, 0),
        (MEMBER_UID, STORAGE_GID),
        (OUTSIDER_UID, OUTSIDER_UID),
        (GROUP_PEER_UID, FIXTURE_GID),
    ] {
        identity.push_user(UserRecord {
            uid: UserId(uid),
            primary_gid: GroupId(gid),
            supplementary_gids: Vec::new(),
            capability_grants: CapabilitySet::empty(),
        });
    }
    identity.verify(&SINK).expect("identity table")
}

/// Format a FAT32 image carrying `hello.txt` with `payload` under the
/// caller-minted BPB `serial`, returning the image and its volume
/// identity.
fn fat32_image(serial: u32, payload: &[u8]) -> (RamBlock, [u8; 16]) {
    let mut fs = Fat32::format(RamBlock::new(SECTORS_64MIB), serial).expect("format");
    let root = fs.root();
    let file = fs
        .create(
            root,
            b"hello.txt",
            NodeKind::RegularFile,
            &NodeSecurity::new(0o644, 0, 0),
        )
        .expect("create");
    fs.write_at(file, 0, payload).expect("write");
    let image = fs.into_block();
    let identity = Fat32::open(RamBlock {
        data: image.data.clone(),
    })
    .expect("probe")
    .volume_identity();
    (image, identity)
}

/// The mount-table record the System Information mount list publishes for
/// the volume attached under `source`.
fn mount_record(source: &[u8]) -> MountRecord {
    FS_SERVICE
        .mount_snapshot()
        .into_iter()
        .find(|record| record.source_bytes() == source)
        .unwrap_or_else(|| std::panic!("no mount snapshot record for {source:?}"))
}

/// The storage-group identity map governs ordinary users on the attached
/// volume: every node appears system-owned under the storage group, files
/// `rw-rw-r--`, so a member reads and writes, a non-member reads but is
/// refused a write — and the on-disk FAT volume never stores any of it.
fn assert_identity_mapped_access() {
    let mut buf = [0u8; 64];
    let stat = FS_SERVICE
        .stat(
            MEMBER_UID,
            &NoCaps,
            root("/Storage/usb1/hello.txt"),
            FinalLink::Follow,
        )
        .expect("member stats the file");
    assert_eq!(
        (stat.mode, stat.uid, stat.gid),
        (0o664, 0, STORAGE_GID),
        "the identity map presents system ownership under the storage group"
    );
    let n = read_as(MEMBER_UID, "/Storage/usb1/hello.txt", 0, &mut buf).expect("member reads");
    assert_eq!(&buf[..n], b"runtime volume payload");
    write_as(MEMBER_UID, "/Storage/usb1/hello.txt", 0, b"R")
        .expect("member writes through the group grant");
    assert_eq!(
        write_as(OUTSIDER_UID, "/Storage/usb1/hello.txt", 0, b"X").err(),
        Some(Errno::PermissionDenied),
        "a non-member's write is refused"
    );
    let n = read_as(OUTSIDER_UID, "/Storage/usb1/hello.txt", 0, &mut buf)
        .expect("a non-member still reads (other class)");
    assert_eq!(&buf[..1], b"R", "the member's write landed");
    assert!(n >= 1);
}

/// A member makes a file and a folder of its own on the mapped volume — what
/// a copy onto removable media is — and an explicit mode change is refused as
/// one the format cannot store.
fn assert_a_member_creates_on_the_volume() {
    let made = "/Storage/usb1/made.txt";
    let create = OpenFlags::WRITE
        .union(OpenFlags::CREATE)
        .union(OpenFlags::EXCLUSIVE);
    open_as(MEMBER_UID, made, create).expect("a member creates a file on the medium");
    let wrote = write_as(MEMBER_UID, made, 0, b"copied").expect("and fills it");
    assert_eq!(wrote, 6);
    let mut buf = [0u8; 16];
    let n = read_as(MEMBER_UID, made, 0, &mut buf).expect("and reads it back");
    assert_eq!(&buf[..n], b"copied");
    let stat = FS_SERVICE
        .stat(MEMBER_UID, &NoCaps, root(made), FinalLink::Follow)
        .expect("stat");
    assert_eq!(
        (stat.mode, stat.uid, stat.gid),
        (0o664, 0, STORAGE_GID),
        "a new file is owned as every file of the volume is"
    );
    FS_SERVICE
        .mkdir(MEMBER_UID, &NoCaps, root("/Storage/usb1/folder"), new_dir())
        .expect("a member makes a folder");
    open_as(MEMBER_UID, "/Storage/usb1/folder/inner.txt", create).expect("and a file inside it");
    assert_eq!(
        FS_SERVICE
            .set_mode(0, &NoCaps, root(made), 0o600, FinalLink::Follow)
            .err(),
        Some(Errno::NotSupported),
        "the format stores no mode of its own"
    );
    assert_eq!(
        FS_SERVICE
            .set_owner(
                0,
                &Chown,
                root(made),
                OUTSIDER_UID,
                FS_OWNER_UNCHANGED,
                FinalLink::Follow
            )
            .err(),
        Some(Errno::NotSupported),
        "nor any owner"
    );
}

/// The boot statics the lifecycle test installs, the only test that touches
/// them: what its scenarios read back of that wiring.
struct Booted {
    pressure: &'static MemoryPressure,
    watches: &'static WatchRegistry,
    window: *mut u8,
    facility: &'static TestFacility,
}

/// Install the boot statics as production wires them.
fn boot() -> Booted {
    let pressure: &'static MemoryPressure = Box::leak(Box::new(MemoryPressure::over(&AMPLE)));
    // The directory-watch registry production wires, so every volume the
    // scenarios attach claims its change table as a hot-plugged one does.
    let watches: &'static WatchRegistry = Box::leak(Box::new(WatchRegistry::new()));
    VOLUME_SERVICE.install(&SINK, pressure, Some(watches));
    // The production boot wiring also registers the service as the
    // endpoint-vanish observer (the surprise-removal trigger).
    tairix_kernel_core::callreg::install_vanish_observer(&VOLUME_SERVICE);
    // Arm the storage-group identity map (production: the root unlock
    // resolves the group by name and installs its gid) and install the
    // identity table the `fs_*` group resolution reads.
    LATE_STORAGE_GID.install(GroupId(STORAGE_GID));
    LATE_IDENTITY
        .install(identity_with_storage_member())
        .expect("install identity once");
    let window: &'static mut [u8] = Box::leak(vec![0u8; BLK_DATA_LEN].into_boxed_slice());
    let window = window.as_mut_ptr();
    let facility: &'static TestFacility = Box::leak(Box::new(TestFacility { window }));
    install_shared_mem_facility(facility);
    LATE_FILESYSTEM
        .install_vfs(Vfs::with_default_layout(UserId(0), GroupId(0)))
        .expect("install the default-layout mount table once");
    Booted {
        pressure,
        watches,
        window,
        facility,
    }
}

/// Whether some volume could claim the change table of the volume `identity`:
/// refused while that volume is attached, granted once it has gone.
fn table_free(booted: &Booted, identity: [u8; 16]) -> bool {
    booted
        .watches
        .claim(identity, NameMatching::Exact, booted.pressure)
        .is_some()
}

#[test]
fn attach_read_detach_lifecycle_over_a_served_fat32_volume() {
    let booted = boot();
    let (window_ptr, facility) = (booted.window, booted.facility);

    // --- The volume: a formatted FAT32 image carrying one file. ---
    let (image, expected_identity) = fat32_image(0x0D15_C001, b"runtime volume payload");
    let device_blocks = (image.data.len() / BLOCK_SIZE) as u64;

    // --- The served block service. ---
    let endpoint_id = 0xB1D0_5E17_0000_0001_u64;
    let endpoint = register_endpoint(endpoint_id, 0x70_0001);
    let device = StdArc::new(Mutex::new(ServedDevice::new(image)));
    let stop = StdArc::new(AtomicBool::new(false));
    let server = serve(
        StdArc::clone(&endpoint),
        window_ptr as usize,
        StdArc::clone(&device),
        StdArc::clone(&stop),
    );

    // The shared region the request names: created through the recorded
    // facility so the kernel hold resolves it.
    let (_va, region_id) = sharedreg::create(facility, ProcessId(0x70_0002), 8).expect("region");

    // --- Attach. ---
    let attach = VolumeAttachRequest {
        endpoint: endpoint_id,
        window: region_id,
        first_lba: 0,
        blocks: device_blocks,
        fstype: VolumeFsType::Fat32,
        name: b"usb1",
    };
    VOLUME_SERVICE.attach(&attach).expect("attach");
    assert_eq!(
        VOLUME_FOREST.resolve(&expected_identity),
        Some(vec![String::from("Storage"), String::from("usb1")]),
        "the volume's durable root is published under the catalog"
    );
    assert!(
        !table_free(&booted, expected_identity),
        "the attached volume feeds its change table, and no other volume may"
    );
    // A second attach of the same volume under another name is refused as
    // busy — it is attached already — and unwinds cleanly. It is not the name
    // conflict the volume manager would answer by trying yet another name.
    let dup = VolumeAttachRequest {
        name: b"usb2",
        ..attach
    };
    assert_eq!(VOLUME_SERVICE.attach(&dup), Err(Errno::Busy));

    // --- Use: the mounted volume serves reads through the production
    // `fs_*` service under the system principal. ---
    let mut buf = [0u8; 64];
    let n = read_as(0, "/Storage/usb1/hello.txt", 0, &mut buf)
        .expect("read through the mounted volume");
    assert_eq!(&buf[..n], b"runtime volume payload");

    // The mount snapshot reports the live volume as available and carries
    // its stable identity — the facts the unmount tooling resolves by.
    let record = mount_record(b"usb1");
    assert_eq!(record.availability(), MountAvailability::Available);
    assert_eq!(record.volume_id(), expected_identity);
    // ...and the storage medium the served device declared, which is how a
    // file manager picks a drive icon without guessing one.
    assert_eq!(
        record.medium(),
        device.lock().unwrap().block.medium(),
        "the mount reports the medium the block device declared"
    );

    assert_identity_mapped_access();
    assert_a_member_creates_on_the_volume();

    // An operation that resolved its file before the volume detached and
    // reaches the driver only after.
    let in_flight = LATE_FILESYSTEM
        .driver(served_handle("usb1"))
        .expect("served");
    let file = {
        let mut fs = in_flight.lock();
        let root = fs.root();
        fs.lookup(root, b"hello.txt").expect("resolves")
    };

    // --- Detach. ---
    let detach = VolumeDetachRequest {
        volume_id: expected_identity,
        force: false,
    };
    VOLUME_SERVICE.detach(&detach).expect("detach");
    assert_eq!(
        in_flight.lock().write_at(file, 0, b"lost"),
        Err(DriverError::DeviceFault),
        "a write reaching a volume that has left is refused, not acknowledged and dropped"
    );
    drop(in_flight);
    assert_eq!(
        VOLUME_FOREST.resolve(&expected_identity),
        None,
        "the withdrawn identity no longer resolves"
    );
    assert_eq!(
        read_as(0, "/Storage/usb1/hello.txt", 0, &mut buf).err(),
        Some(Errno::NotFound),
        "the retracted mount fails closed"
    );
    // A repeated detach fails closed.
    assert_eq!(VOLUME_SERVICE.detach(&detach), Err(Errno::NotFound));
    assert!(
        table_free(&booted, expected_identity),
        "detaching let its change table go"
    );

    stop.store(true, Ordering::Relaxed);
    server.join().expect("server thread");
    assert!(
        device.lock().unwrap().flushes >= 1,
        "detach commits the device cache"
    );

    run_scenarios(window_ptr, facility);
}

/// Every further scenario, over the service the lifecycle test booted.
fn run_scenarios(window_ptr: *mut u8, facility: &'static TestFacility) {
    dirty_surprise_removal_scenario(window_ptr, facility);
    clean_surprise_removal_scenario(window_ptr, facility);
    pulling_a_partitioned_stick_retracts_every_volume_scenario(window_ptr, facility);
    forced_unmount_of_a_healthy_volume_scenario(window_ptr, facility);
    verified_reinsert_replays_scenario(window_ptr, facility);
    mutated_reinsert_conflicts_scenario(window_ptr, facility);
    sibling_volumes_share_one_client_scenario(window_ptr, facility);
    unrecognised_medium_mounts_as_unknown_scenario(window_ptr, facility);
    link_round_trip_through_the_real_service_scenario(window_ptr, facility);
    a_taken_catalog_name_is_answered_as_taken_scenario(window_ptr, facility);
    a_volume_its_driver_refuses_to_write_mounts_read_only_scenario(window_ptr, facility);
    an_ext2_volume_mounts_as_ext2_scenario(window_ptr, facility);
    an_ext_volume_keeps_its_on_disk_ownership_scenario(window_ptr, facility);
    a_stick_pulled_mid_commit_returns_read_only_scenario(window_ptr, facility);
    an_unpublished_transaction_is_recorded_lost_scenario(window_ptr, facility);
    a_device_that_stops_answering_holds_up_no_other_scenario(window_ptr, facility);
    a_disc_image_attaches_read_only_scenario(window_ptr, facility);
    a_udf_volume_attaches_writable_scenario(window_ptr, facility);
    an_adfs_volume_attaches_writable_scenario(window_ptr, facility);
    a_changed_medium_retracts_its_volume_and_serves_the_next_scenario(window_ptr, facility);
    a_medium_that_leaves_while_its_volume_attaches_retracts_it_scenario(window_ptr, facility);
    a_medium_that_leaves_while_its_volume_recovers_retracts_it_scenario(window_ptr, facility);
    an_attach_naming_a_retracted_publication_is_refused_scenario();
    a_held_descriptor_holds_its_volume_attached_scenario(window_ptr, facility);
    a_descriptor_stays_with_its_node_scenario(window_ptr, facility);
    a_fat_files_lock_follows_it_through_writes_and_renames_scenario(window_ptr, facility);
}

/// A drive whose medium changes retracts the node it published the first
/// under, its endpoint living on: the first medium's volume is retracted at
/// once, not left mounted refusing every request, and the next medium
/// attaches over the same endpoint through a fresh client and the window
/// its own publication conferred.
fn a_changed_medium_retracts_its_volume_and_serves_the_next_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (first, first_identity) = fat32_image(0x0D15_C040, b"first disc payload");
    let (second, second_identity) = fat32_image(0x0D15_C041, b"second disc payload");
    let owner = 0x70_1050;
    let endpoint_id = 0xB1D0_5E17_0000_0050_u64;
    let endpoint = register_endpoint(endpoint_id, owner);
    let blocks = (first.data.len() / BLOCK_SIZE) as u64;
    let device = StdArc::new(Mutex::new(ServedDevice {
        binding: 1,
        ..ServedDevice::new(first)
    }));
    let stop = StdArc::new(AtomicBool::new(false));
    let server = serve(
        StdArc::clone(&endpoint),
        window_ptr as usize,
        StdArc::clone(&device),
        StdArc::clone(&stop),
    );
    let (_va, first_region) = sharedreg::create(facility, ProcessId(0x70_0050), 8).expect("region");
    let attach = |window: u64, name: &[u8]| {
        VOLUME_SERVICE.attach(&VolumeAttachRequest {
            endpoint: endpoint_id,
            window,
            first_lba: 0,
            blocks,
            fstype: VolumeFsType::Fat32,
            name,
        })
    };
    attach(first_region, b"disc1").expect("attach the first disc");
    assert_volume_reads("/Storage/disc1/hello.txt", b"first disc payload");

    {
        let mut drive = device.lock().unwrap();
        drive.block = second;
        drive.binding = 2;
    }
    tairix_kernel_core::callreg::notify_publication_retracted(&[endpoint_id]);
    assert_eq!(
        VOLUME_FOREST.resolve(&first_identity),
        None,
        "the first disc's root retracts"
    );
    let mut buf = [0u8; 8];
    assert_eq!(
        read_as(0, "/Storage/disc1/hello.txt", 0, &mut buf).err(),
        Some(Errno::NotFound),
        "the first disc's mount is gone, not left refusing"
    );

    let (_va, second_region) =
        sharedreg::create(facility, ProcessId(0x70_0051), 8).expect("region");
    attach(second_region, b"disc2").expect("the next disc attaches over the same endpoint");
    assert_volume_reads("/Storage/disc2/hello.txt", b"second disc payload");
    assert!(VOLUME_FOREST.resolve(&second_identity).is_some());
    assert_eq!(
        device.lock().unwrap().geometries,
        2,
        "the next disc is reached through a fresh client"
    );
    VOLUME_SERVICE
        .detach(&VolumeDetachRequest {
            volume_id: second_identity,
            force: false,
        })
        .expect("the second disc detaches");
    tairix_kernel_core::callreg::teardown_owned_by(owner, &ASPACES, &SINK);
    stop.store(true, Ordering::Relaxed);
    server.join().expect("server thread");
    assert_every_attached_volume_is_served();
}

/// The driver answers every request an attach makes, then retracts the
/// publication before the volume is entered: the volume is retracted as the
/// attach lets go, never published over a medium that has left.
fn a_medium_that_leaves_while_its_volume_attaches_retracts_it_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (image, identity) = fat32_image(0x0D15_C060, b"departing payload");
    let blocks = (image.data.len() / BLOCK_SIZE) as u64;
    let owner = 0x70_1060;
    let endpoint_id = 0xB1D0_5E17_0000_0060_u64;
    let (received, has_request) = mpsc::channel();
    let (open_gate, gate) = mpsc::channel();
    let device = StdArc::new(Mutex::new(ServedDevice {
        hold_first: Some((received, gate)),
        ..ServedDevice::new(image)
    }));
    let stop = StdArc::new(AtomicBool::new(false));
    let server = serve(
        register_endpoint(endpoint_id, owner),
        window_ptr as usize,
        StdArc::clone(&device),
        StdArc::clone(&stop),
    );
    let (_va, region) = sharedreg::create(facility, ProcessId(0x70_0060), 8).expect("region");
    let attaching = thread::spawn(move || {
        VOLUME_SERVICE.attach(&VolumeAttachRequest {
            endpoint: endpoint_id,
            window: region,
            first_lba: 0,
            blocks,
            fstype: VolumeFsType::Fat32,
            name: b"gone1",
        })
    });
    let held = has_request.recv_timeout(Duration::from_secs(60));
    if held.is_ok() {
        tairix_kernel_core::callreg::notify_publication_retracted(&[endpoint_id]);
    }
    // Answered whatever happened, so a failure never wedges the suite.
    open_gate.send(()).unwrap();
    held.expect("the device holds the attach's first request");
    assert_eq!(attaching.join().expect("the attach thread"), Ok(()));
    assert_eq!(
        VOLUME_FOREST.resolve(&identity),
        None,
        "a volume built over a medium that left is not published"
    );
    let mut buf = [0u8; 8];
    assert_eq!(
        read_as(0, "/Storage/gone1/hello.txt", 0, &mut buf).err(),
        Some(Errno::NotFound),
        "nor mounted"
    );
    assert_eq!(audited(SURPRISE_REMOVED_CLEAN, "gone1"), [None]);
    tairix_kernel_core::callreg::teardown_owned_by(owner, &ASPACES, &SINK);
    stop.store(true, Ordering::Relaxed);
    server.join().expect("scenario server thread");
    assert_every_attached_volume_is_served();
}

/// A card pulled with a write unflushed, put back, and pulled again while
/// its volume is recovered, the reader's endpoint living on throughout: the
/// recovered volume is retracted as the recovery lets go, never restored
/// over a card that has left.
fn a_medium_that_leaves_while_its_volume_recovers_retracts_it_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (image, identity) = fat32_image(0x0D15_C061, b"scenario payload");
    let pristine = image.data.clone();
    let blocks = (image.data.len() / BLOCK_SIZE) as u64;
    let owner = 0x70_1061;
    let endpoint_id = 0xB1D0_5E17_0000_0061_u64;
    let device = StdArc::new(Mutex::new(ServedDevice {
        binding: 1,
        ..ServedDevice::new(image)
    }));
    let stop = StdArc::new(AtomicBool::new(false));
    let server = serve(
        register_endpoint(endpoint_id, owner),
        window_ptr as usize,
        StdArc::clone(&device),
        StdArc::clone(&stop),
    );
    let attach = move |window: u64| {
        VOLUME_SERVICE.attach(&VolumeAttachRequest {
            endpoint: endpoint_id,
            window,
            first_lba: 0,
            blocks,
            fstype: VolumeFsType::Fat32,
            name: b"card1",
        })
    };
    let (_va, first) = sharedreg::create(facility, ProcessId(0x70_0061), 8).expect("region");
    attach(first).expect("attach the card");
    write_as(0, "/Storage/card1/hello.txt", 0, b"D").expect("write dirties the journal");
    {
        let mut reader = device.lock().unwrap();
        reader.block = RamBlock { data: pristine };
        reader.binding = 2;
    }
    tairix_kernel_core::callreg::notify_publication_retracted(&[endpoint_id]);
    assert_eq!(
        mount_record(b"card1").availability(),
        MountAvailability::UnavailableDirty,
        "the pulled card's volume keeps its unflushed write"
    );

    let (received, has_request) = mpsc::channel();
    let (open_gate, gate) = mpsc::channel();
    {
        let mut reader = device.lock().unwrap();
        reader.binding = 3;
        reader.hold_first = Some((received, gate));
    }
    let (_va, second) = sharedreg::create(facility, ProcessId(0x70_0062), 8).expect("region");
    let recovering = thread::spawn(move || attach(second));
    let held = has_request.recv_timeout(Duration::from_secs(60));
    if held.is_ok() {
        tairix_kernel_core::callreg::notify_publication_retracted(&[endpoint_id]);
    }
    open_gate.send(()).unwrap();
    held.expect("the reader holds the recovery's first request");
    assert_eq!(recovering.join().expect("the recovery thread"), Ok(()));
    assert_eq!(
        VOLUME_FOREST.resolve(&identity),
        None,
        "a volume recovered over a card that left again is retracted"
    );
    assert_eq!(
        audited(SURPRISE_REMOVED_CLEAN, "card1"),
        [None],
        "the replay was committed, so its retraction is clean"
    );
    tairix_kernel_core::callreg::teardown_owned_by(owner, &ASPACES, &SINK);
    stop.store(true, Ordering::Relaxed);
    server.join().expect("scenario server thread");
    assert_every_attached_volume_is_served();
}

/// An attach naming a window whose publication was retracted, while an
/// operation still holds its slot, is refused before the window's client is
/// asked anything: all it would be answered is stale.
fn an_attach_naming_a_retracted_publication_is_refused_scenario() {
    let pressure: &'static MemoryPressure = Box::leak(Box::new(MemoryPressure::over(&AMPLE)));
    let service: &'static RuntimeVolumeService = Box::leak(Box::new(RuntimeVolumeService::new()));
    service.install(&SINK, pressure, None);
    let (endpoint, window) = (0xB1D0_5E17_0000_0063_u64, 0x63);
    let holder = service.pin_device(endpoint, window).expect("a slot");
    service.publication_retracted(EndpointId(endpoint));
    let request = VolumeAttachRequest {
        endpoint,
        window,
        first_lba: 0,
        blocks: 1,
        fstype: VolumeFsType::Fat32,
        name: b"stale1",
    };
    assert_eq!(service.attach(&request), Err(Errno::Stale));
    assert_eq!(
        audited(ATTACH_REFUSED, "stale1"),
        [Some(String::from("publication_retracted"))]
    );
    drop(holder);
}

/// The driver handle of the volume shown at `/Storage/<name>`.
fn served_handle(name: &str) -> DriverHandle {
    let at = Path::parse(&std::format!("/Storage/{name}")).expect("a path");
    LATE_FILESYSTEM
        .vfs()
        .expect("installed")
        .mounts()
        .volumes()
        .iter()
        .find(|(_, shown)| shown.as_ref() == Some(&at))
        .map(|(volume, _)| volume.driver())
        .expect("the volume is attached")
}

/// The per-scenario facts for [`attach_then_yank`]: the owning task, the
/// endpoint id, the image's minted serial, the shared-region creator, the
/// catalog name, and whether the journal is dirtied before the yank.
struct YankScenario {
    owner: u64,
    endpoint_id: u64,
    serial: u32,
    region_task: u64,
    name: &'static [u8],
    unflushed: Unflushed,
}

/// What a scenario volume holds that its medium never took when it is yanked.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Unflushed {
    Nothing,
    /// A write the device took into its cache, which the journal retains.
    DeviceWrite,
    /// A transaction the volume's driver reports open and never published.
    DriverTransaction,
}

/// One served, attached FAT32 volume for the surprise-removal scenarios,
/// yanked by tearing its endpoint's owner down. Returns the volume's
/// identity after the server thread has ended, and a descriptor of its
/// `hello.txt` held across the removal.
fn attach_then_yank(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
    scenario: &YankScenario,
) -> ([u8; 16], Descriptor) {
    let endpoint = register_endpoint(scenario.endpoint_id, scenario.owner);
    let (image, identity) = fat32_image(scenario.serial, b"scenario payload");
    let blocks = (image.data.len() / BLOCK_SIZE) as u64;
    let device = StdArc::new(Mutex::new(ServedDevice::new(image)));
    let stop = StdArc::new(AtomicBool::new(false));
    let server = serve(
        StdArc::clone(&endpoint),
        window_ptr as usize,
        StdArc::clone(&device),
        StdArc::clone(&stop),
    );
    let (_va, region) =
        sharedreg::create(facility, ProcessId(scenario.region_task), 8).expect("region");
    VOLUME_SERVICE
        .attach(&VolumeAttachRequest {
            endpoint: scenario.endpoint_id,
            window: region,
            first_lba: 0,
            blocks,
            fstype: VolumeFsType::Fat32,
            name: scenario.name,
        })
        .expect("attach scenario volume");
    let name = core::str::from_utf8(scenario.name).expect("ascii name");
    let path = std::format!("/Storage/{name}/hello.txt");
    match scenario.unflushed {
        Unflushed::Nothing => {}
        // Dirty the volume's journal through the production write path.
        Unflushed::DeviceWrite => {
            write_as(0, &path, 0, b"D").expect("write dirties the journal");
        }
        // What a driver deferring its commit reports, far enough out that
        // nothing publishes it first.
        Unflushed::DriverTransaction => {
            LATE_FILESYSTEM.note_writeback_due(served_handle(name), Some(u64::MAX - 1));
        }
    }
    let held = Descriptor::open(0, &path, OpenFlags::READ).expect("a descriptor of the volume");
    // Yank the stick: the serving driver dies and the kernel tears its
    // endpoint down, which drives the vanish observer synchronously.
    tairix_kernel_core::callreg::teardown_owned_by(scenario.owner, &ASPACES, &SINK);
    stop.store(true, Ordering::Relaxed);
    server.join().expect("scenario server thread");
    (identity, held)
}

/// Surprise removal with uncommitted writes (`plans/DEVICES.md` D4a): the
/// volume becomes unavailable-dirty — root visible, every operation
/// failing closed, plain detach refused.
fn dirty_surprise_removal_scenario(window_ptr: *mut u8, facility: &'static TestFacility) {
    let (identity, held) = attach_then_yank(
        window_ptr,
        facility,
        &YankScenario {
            owner: 0x70_1002,
            endpoint_id: 0xB1D0_5E17_0000_0002,
            serial: 0x0D15_C002,
            region_task: 0x70_0003,
            name: b"usb2",
            unflushed: Unflushed::DeviceWrite,
        },
    );
    let mut buf = [0u8; 32];
    assert_eq!(
        held.read(0, &mut buf),
        Err(Errno::DeviceFault),
        "a descriptor held across the removal answers the device fault while it is away"
    );
    assert_links_and_attributes_fault_on_a_vanished_usb2(&held);
    drop(held);
    assert_eq!(
        VOLUME_FOREST.resolve(&identity),
        Some(vec![String::from("Storage"), String::from("usb2")]),
        "an unavailable-dirty volume's durable root stays visible"
    );
    assert_eq!(
        read_as(0, "/Storage/usb2/hello.txt", 0, &mut buf).err(),
        Some(Errno::DeviceFault),
        "new I/O on an unavailable-dirty volume fails closed, cache included"
    );
    assert_eq!(
        VOLUME_SERVICE.detach(&VolumeDetachRequest {
            volume_id: identity,
            force: false,
        }),
        Err(Errno::DeviceFault),
        "a plain detach never discards the retained set"
    );
    // The mount snapshot says the volume is unavailable-dirty — never a
    // volume that looks healthy — and still names its identity.
    let record = FS_SERVICE
        .mount_snapshot()
        .into_iter()
        .find(|r| r.source_bytes() == b"usb2")
        .expect("the unavailable volume stays in the mount snapshot");
    assert_eq!(record.availability(), MountAvailability::UnavailableDirty);
    assert_eq!(record.volume_id(), identity);
    // The audited force-unmount is the deliberate exit: the retained set
    // is discarded, the root withdrawn, and the mount retracted.
    VOLUME_SERVICE
        .detach(&VolumeDetachRequest {
            volume_id: identity,
            force: true,
        })
        .expect("force-unmount discards and retracts the unavailable volume");
    assert_eq!(
        VOLUME_FOREST.resolve(&identity),
        None,
        "the force-unmounted identity no longer resolves"
    );
    assert_eq!(
        read_as(0, "/Storage/usb2/hello.txt", 0, &mut buf).err(),
        Some(Errno::NotFound),
        "the force-retracted mount fails closed"
    );
    assert!(
        FS_SERVICE
            .mount_snapshot()
            .iter()
            .all(|r| r.source_bytes() != b"usb2"),
        "the force-unmounted volume leaves the mount snapshot"
    );
    assert_every_attached_volume_is_served();
}

/// The link and attribute surfaces of the vanished `usb2` fault like every
/// other operation, attributes reached through `held`, a descriptor kept
/// across the removal. Left at their ABI defaults the stand-in would answer
/// `NotSupported`, reporting a yanked disk as a format that never had links
/// or attributes — a permanent claim about a transient condition.
fn assert_links_and_attributes_fault_on_a_vanished_usb2(held: &Descriptor) {
    let mut buf = [0u8; 32];
    assert_eq!(
        FS_SERVICE
            .readlink(0, &NoCaps, root("/Storage/usb2/hello.txt"))
            .err(),
        Some(Errno::DeviceFault),
        "readlink on a vanished volume is a device fault, not a format limit"
    );
    assert_eq!(
        FS_SERVICE
            .symlink(0, &NoCaps, "hello.txt", root("/Storage/usb2/link"))
            .err(),
        Some(Errno::DeviceFault),
        "symlink on a vanished volume is a device fault, not a format limit"
    );
    assert_eq!(
        FS_SERVICE
            .link(
                0,
                &NoCaps,
                root("/Storage/usb2/hello.txt"),
                root("/Storage/usb2/second"),
                FinalLink::Keep,
            )
            .err(),
        Some(Errno::DeviceFault),
        "link on a vanished volume is a device fault, not a format limit"
    );
    assert_eq!(
        FS_SERVICE
            .attr_get_node(held.handle(), &held.acting(), b"user.k", &mut buf)
            .err(),
        Some(Errno::DeviceFault),
        "an attribute read on a vanished volume is a device fault, not a format limit"
    );
}

/// A volume that leaves leaves the namespace, not only the driver registry:
/// every volume the mount table still holds has a live driver.
fn assert_every_attached_volume_is_served() {
    let volumes = LATE_FILESYSTEM.vfs().expect("installed").mounts().volumes();
    assert!(
        volumes
            .iter()
            .all(|(volume, _)| LATE_FILESYSTEM.driver(volume.driver()).is_ok()),
        "a retracted volume is still attached: {volumes:?}"
    );
}

/// Surprise removal with nothing uncommitted: the volume is simply
/// retracted — no drama.
fn clean_surprise_removal_scenario(window_ptr: *mut u8, facility: &'static TestFacility) {
    let (identity, held) = attach_then_yank(
        window_ptr,
        facility,
        &YankScenario {
            owner: 0x70_1004,
            endpoint_id: 0xB1D0_5E17_0000_0003,
            serial: 0x0D15_C003,
            region_task: 0x70_0005,
            name: b"usb3",
            unflushed: Unflushed::Nothing,
        },
    );
    let mut buf = [0u8; 32];
    assert_eq!(
        held.read(0, &mut buf),
        Err(Errno::Stale),
        "a descriptor held across a clean removal reaches nothing"
    );
    drop(held);
    assert_eq!(
        VOLUME_FOREST.resolve(&identity),
        None,
        "a clean surprise removal retracts the durable root"
    );
    assert_eq!(
        read_as(0, "/Storage/usb3/hello.txt", 0, &mut buf).err(),
        Some(Errno::NotFound),
        "the retracted mount fails closed"
    );
    assert_eq!(
        VOLUME_SERVICE.detach(&VolumeDetachRequest {
            volume_id: identity,
            force: false,
        }),
        Err(Errno::NotFound),
        "nothing remains to detach"
    );
    assert_every_attached_volume_is_served();
}

/// Pulling a stick that carries two volumes retracts both — they share the
/// one endpoint that vanished — and lets go of what its cache held, so no
/// volume is left over a dead device and none of the medium stays in memory.
fn pulling_a_partitioned_stick_retracts_every_volume_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (first, first_identity) = fat32_image(0x0D15_C010, b"first partition payload");
    let (second, second_identity) = fat32_image(0x0D15_C011, b"second partition payload");
    let extent_blocks = (first.data.len() / BLOCK_SIZE) as u64;
    let mut data = first.data;
    data.extend_from_slice(&second.data);
    let owner = 0x70_1030;
    let endpoint_id = 0xB1D0_5E17_0000_0030_u64;
    let endpoint = register_endpoint(endpoint_id, owner);
    let device = StdArc::new(Mutex::new(ServedDevice::new(RamBlock { data })));
    let stop = StdArc::new(AtomicBool::new(false));
    let server = serve(
        StdArc::clone(&endpoint),
        window_ptr as usize,
        StdArc::clone(&device),
        StdArc::clone(&stop),
    );
    let (_va, region) = sharedreg::create(facility, ProcessId(0x70_0030), 8).expect("region");
    for (name, first_lba) in [(&b"pull1"[..], 0), (b"pull2", extent_blocks)] {
        VOLUME_SERVICE
            .attach(&VolumeAttachRequest {
                endpoint: endpoint_id,
                window: region,
                first_lba,
                blocks: extent_blocks,
                fstype: VolumeFsType::Fat32,
                name,
            })
            .expect("attach a partition");
    }
    assert_volume_reads("/Storage/pull1/hello.txt", b"first partition payload");
    assert_volume_reads("/Storage/pull2/hello.txt", b"second partition payload");

    tairix_kernel_core::callreg::teardown_owned_by(owner, &ASPACES, &SINK);
    stop.store(true, Ordering::Relaxed);
    server.join().expect("server thread");

    for (identity, path) in [
        (first_identity, "/Storage/pull1/hello.txt"),
        (second_identity, "/Storage/pull2/hello.txt"),
    ] {
        assert_eq!(VOLUME_FOREST.resolve(&identity), None, "{path} retracts");
        let mut buf = [0u8; 8];
        assert_eq!(
            read_as(0, path, 0, &mut buf).err(),
            Some(Errno::NotFound),
            "{path} fails closed"
        );
    }
    assert_every_attached_volume_is_served();
    let cached: Vec<usize> = VOLUME_SERVICE
        .devices
        .lock()
        .iter()
        .filter(|slot| slot.endpoint == endpoint_id)
        .filter_map(|slot| slot.connected.as_ref())
        .map(|shared| {
            let mut held = usize::MAX;
            shared
                .device
                .try_with_device(|cache| held = cache.accounting().total_bytes());
            held
        })
        .collect();
    assert!(
        cached.iter().all(|&held| held == 0),
        "the removed medium's cached blocks are let go at once: {cached:?}"
    );
}

/// Surprise removal while the volume's driver holds a transaction it never
/// published: those writes were acknowledged and never reached the device, so
/// the removal is recorded as the loss it is, never as a clean one.
fn an_unpublished_transaction_is_recorded_lost_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (identity, held) = attach_then_yank(
        window_ptr,
        facility,
        &YankScenario {
            owner: 0x70_1030,
            endpoint_id: 0xB1D0_5E17_0000_0030,
            serial: 0x0D15_C00C,
            region_task: 0x70_0030,
            name: b"usb9",
            unflushed: Unflushed::DriverTransaction,
        },
    );
    assert_eq!(
        VOLUME_FOREST.resolve(&identity),
        Some(vec![String::from("Storage"), String::from("usb9")]),
        "a volume that lost writes stays visible rather than retracting"
    );
    assert_eq!(
        mount_record(b"usb9").availability(),
        MountAvailability::UnavailableLost
    );
    assert_eq!(
        audited(SURPRISE_REMOVED_LOST, "usb9"),
        vec![Some(String::from("unpublished_writes"))]
    );
    let mut buf = [0u8; 8];
    assert_eq!(held.read(0, &mut buf), Err(Errno::DeviceFault));
    VOLUME_SERVICE
        .detach(&VolumeDetachRequest {
            volume_id: identity,
            force: true,
        })
        .expect("the audited force retracts it");
    assert_eq!(
        held.read(0, &mut buf),
        Err(Errno::Stale),
        "the force revoked the descriptor held across the removal"
    );
    assert_eq!(
        audited_count(FORCE_UNMOUNTED, "usb9", "revoked_nodes"),
        [Some(1)]
    );
    assert_every_attached_volume_is_served();
}

/// One yank round for the re-insert scenarios: format a FAT32 image
/// carrying `hello.txt`, serve it, attach it under `name`, dirty the
/// journal through the production write path, and yank the serving
/// driver. Returns the volume identity, the image bytes as they stood
/// **before** the dirty write (the state a device that lost its cache
/// presents), the image bytes at yank time, and a descriptor of `hello.txt`
/// held across the removal.
struct YankedRound {
    identity: [u8; 16],
    pristine: Vec<u8>,
    at_yank: Vec<u8>,
    held: Descriptor,
}

fn dirty_yank_round(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
    scenario: &YankScenario,
) -> YankedRound {
    assert_eq!(
        scenario.unflushed,
        Unflushed::DeviceWrite,
        "a re-insert round always dirties the journal"
    );
    let endpoint = register_endpoint(scenario.endpoint_id, scenario.owner);
    let (image, identity) = fat32_image(scenario.serial, b"scenario payload");
    let pristine = image.data.clone();
    let blocks = (image.data.len() / BLOCK_SIZE) as u64;
    let device = StdArc::new(Mutex::new(ServedDevice::new(image)));
    let stop = StdArc::new(AtomicBool::new(false));
    let server = serve(
        StdArc::clone(&endpoint),
        window_ptr as usize,
        StdArc::clone(&device),
        StdArc::clone(&stop),
    );
    let (_va, region) =
        sharedreg::create(facility, ProcessId(scenario.region_task), 8).expect("region");
    VOLUME_SERVICE
        .attach(&VolumeAttachRequest {
            endpoint: scenario.endpoint_id,
            window: region,
            first_lba: 0,
            blocks,
            fstype: VolumeFsType::Fat32,
            name: scenario.name,
        })
        .expect("attach re-insert round volume");
    let path = std::format!(
        "/Storage/{}/hello.txt",
        core::str::from_utf8(scenario.name).expect("ascii name")
    );
    write_as(0, &path, 0, b"D").expect("write dirties the journal");
    let held = Descriptor::open(0, &path, OpenFlags::READ).expect("a descriptor of the volume");
    tairix_kernel_core::callreg::teardown_owned_by(scenario.owner, &ASPACES, &SINK);
    stop.store(true, Ordering::Relaxed);
    server.join().expect("re-insert round server thread");
    let at_yank = device.lock().unwrap().block.data.clone();
    YankedRound {
        identity,
        pristine,
        at_yank,
        held,
    }
}

/// A served, inserted device and its round facts: the device (for image
/// assertions), the stop/join pair the caller ends the round with, and the
/// attach outcome.
struct Inserted {
    device: StdArc<Mutex<ServedDevice>>,
    stop: StdArc<AtomicBool>,
    server: thread::JoinHandle<()>,
    outcome: Result<(), Errno>,
}

/// The facts one inserted device is served and attached under.
struct Insertion {
    endpoint_id: u64,
    owner: u64,
    region_task: u64,
    name: &'static [u8],
    fstype: VolumeFsType,
}

/// Serve `device` and attach the volume it holds, as an inserted stick is.
fn insert(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
    insertion: &Insertion,
    device: ServedDevice,
) -> Inserted {
    let endpoint = register_endpoint(insertion.endpoint_id, insertion.owner);
    let blocks = (device.block.data.len() / BLOCK_SIZE) as u64;
    let device = StdArc::new(Mutex::new(device));
    let stop = StdArc::new(AtomicBool::new(false));
    let server = serve(
        StdArc::clone(&endpoint),
        window_ptr as usize,
        StdArc::clone(&device),
        StdArc::clone(&stop),
    );
    let (_va, region) =
        sharedreg::create(facility, ProcessId(insertion.region_task), 8).expect("region");
    let outcome = VOLUME_SERVICE.attach(&VolumeAttachRequest {
        endpoint: insertion.endpoint_id,
        window: region,
        first_lba: 0,
        blocks,
        fstype: insertion.fstype,
        name: insertion.name,
    });
    Inserted {
        device,
        stop,
        server,
        outcome,
    }
}

/// Verified re-insert (`plans/DEVICES.md` D4c): the device lost its
/// cached writes (the medium is the pre-write image), non-mutation is
/// proven from the evidence window, and the retained writes replay — the
/// dirty write is back on the medium and the volume returns to full
/// service under its original mount and published root.
fn verified_reinsert_replays_scenario(window_ptr: *mut u8, facility: &'static TestFacility) {
    let round = dirty_yank_round(
        window_ptr,
        facility,
        &YankScenario {
            owner: 0x70_1007,
            endpoint_id: 0xB1D0_5E17_0000_0005,
            serial: 0x0D15_C005,
            region_task: 0x70_0007,
            name: b"usb5",
            unflushed: Unflushed::DeviceWrite,
        },
    );
    // Re-insert the pre-write image: the device accepted the writes into
    // its volatile cache and lost them with the power.
    let inserted = insert(
        window_ptr,
        facility,
        &Insertion {
            endpoint_id: 0xB1D0_5E17_0000_0006,
            owner: 0x70_1008,
            region_task: 0x70_0008,
            name: b"usb5",
            fstype: VolumeFsType::Fat32,
        },
        ServedDevice::new(RamBlock {
            data: round.pristine,
        }),
    );
    inserted.outcome.expect("the re-insert recovers the volume");

    // The volume is back in full service: available, its root published,
    // and the replayed write visible through the production service.
    let record = FS_SERVICE
        .mount_snapshot()
        .into_iter()
        .find(|r| r.source_bytes() == b"usb5")
        .expect("the recovered volume appears in the mount snapshot");
    assert_eq!(record.availability(), MountAvailability::Available);
    assert_eq!(record.volume_id(), round.identity);
    assert_eq!(
        VOLUME_FOREST.resolve(&round.identity),
        Some(vec![String::from("Storage"), String::from("usb5")]),
        "the durable root survived the whole unplug/re-insert cycle"
    );
    let mut buf = [0u8; 32];
    let n = read_as(0, "/Storage/usb5/hello.txt", 0, &mut buf)
        .expect("the recovered volume serves reads");
    assert_eq!(
        &buf[..1],
        b"D",
        "the retained dirty write was replayed onto the medium"
    );
    assert!(n >= 1);
    assert_eq!(
        round.held.read(0, &mut buf),
        Err(Errno::Stale),
        "the volume returned under a fresh registration a held descriptor never reaches"
    );
    // The recovered volume is writable again.
    write_as(0, "/Storage/usb5/hello.txt", 0, b"E").expect("the recovered volume accepts writes");

    // A clean detach ends the round: the replay left nothing uncommitted
    // that a flush cannot commit.
    VOLUME_SERVICE
        .detach(&VolumeDetachRequest {
            volume_id: round.identity,
            force: false,
        })
        .expect("the recovered volume detaches cleanly");
    inserted.stop.store(true, Ordering::Relaxed);
    inserted.server.join().expect("reinsert server thread");
    assert!(
        inserted.device.lock().unwrap().flushes >= 1,
        "the replay committed the device cache"
    );
}

/// Mutated re-insert (`plans/DEVICES.md` D4c): the medium's evidence
/// window changed while unplugged (a foreign mount touched `FSInfo`), so
/// replay is refused — the volume returns read-only in the
/// recovery-conflict state with the retained set kept; a plain detach
/// stays refused and the audited force-unmount is the exit.
fn mutated_reinsert_conflicts_scenario(window_ptr: *mut u8, facility: &'static TestFacility) {
    let round = dirty_yank_round(
        window_ptr,
        facility,
        &YankScenario {
            owner: 0x70_1009,
            endpoint_id: 0xB1D0_5E17_0000_0007,
            serial: 0x0D15_C006,
            region_task: 0x70_0009,
            name: b"usb6",
            unflushed: Unflushed::DeviceWrite,
        },
    );
    // A foreign mount updated the FSInfo free-cluster hint (sector 1,
    // offset 488) — inside the evidence window, outside the identity.
    let mut mutated = round.at_yank;
    mutated[512 + 488..512 + 492].copy_from_slice(&1234u32.to_le_bytes());
    let inserted = insert(
        window_ptr,
        facility,
        &Insertion {
            endpoint_id: 0xB1D0_5E17_0000_0008,
            owner: 0x70_100A,
            region_task: 0x70_000A,
            name: b"usb6",
            fstype: VolumeFsType::Fat32,
        },
        ServedDevice::new(RamBlock { data: mutated }),
    );
    inserted
        .outcome
        .expect("the conflicted re-insert still mounts (read-only)");

    // The volume is visible but conflicted: reads serve, writes are
    // refused by the read-only mount, and the snapshot says why.
    let record = FS_SERVICE
        .mount_snapshot()
        .into_iter()
        .find(|r| r.source_bytes() == b"usb6")
        .expect("the conflicted volume appears in the mount snapshot");
    assert_eq!(record.availability(), MountAvailability::RecoveryConflict);
    let mut buf = [0u8; 32];
    read_as(0, "/Storage/usb6/hello.txt", 0, &mut buf).expect("the conflicted volume serves reads");
    assert_eq!(round.held.read(0, &mut buf), Err(Errno::Stale));
    assert_eq!(
        write_as(0, "/Storage/usb6/hello.txt", 0, b"X").err(),
        Some(Errno::ReadOnly),
        "the conflicted volume is read-only until acknowledged"
    );
    // The retained set is still held: a plain detach would discard it
    // silently, so it is refused; the audited force is the exit.
    assert_eq!(
        VOLUME_SERVICE.detach(&VolumeDetachRequest {
            volume_id: round.identity,
            force: false,
        }),
        Err(Errno::NotEmpty),
        "a plain detach never discards the conflicted volume's retained set"
    );
    VOLUME_SERVICE
        .detach(&VolumeDetachRequest {
            volume_id: round.identity,
            force: true,
        })
        .expect("force-unmount discards and retracts the conflicted volume");
    assert_eq!(VOLUME_FOREST.resolve(&round.identity), None);
    inserted.stop.store(true, Ordering::Relaxed);
    inserted
        .server
        .join()
        .expect("conflict reinsert server thread");
}

/// A force detach of a **healthy** volume commits cleanly — the flush
/// still runs and nothing is discarded; force only changes the outcome
/// when a clean commit is impossible (`plans/DEVICES.md` D4b).
fn forced_unmount_of_a_healthy_volume_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let endpoint_id = 0xB1D0_5E17_0000_0004_u64;
    let endpoint = register_endpoint(endpoint_id, 0x70_1006);
    let (image, identity) = fat32_image(0x0D15_C004, b"healthy force payload");
    let blocks = (image.data.len() / BLOCK_SIZE) as u64;
    let device = StdArc::new(Mutex::new(ServedDevice::new(image)));
    let stop = StdArc::new(AtomicBool::new(false));
    let server = serve(
        StdArc::clone(&endpoint),
        window_ptr as usize,
        StdArc::clone(&device),
        StdArc::clone(&stop),
    );
    let (_va, region) = sharedreg::create(facility, ProcessId(0x70_0006), 8).expect("region");
    VOLUME_SERVICE
        .attach(&VolumeAttachRequest {
            endpoint: endpoint_id,
            window: region,
            first_lba: 0,
            blocks,
            fstype: VolumeFsType::Fat32,
            name: b"usb4",
        })
        .expect("attach healthy volume");
    // Dirty the journal so a discard would be observable, then force.
    write_as(0, "/Storage/usb4/hello.txt", 0, b"F").expect("write dirties the journal");
    VOLUME_SERVICE
        .detach(&VolumeDetachRequest {
            volume_id: identity,
            force: true,
        })
        .expect("a force detach of a healthy volume succeeds");
    stop.store(true, Ordering::Relaxed);
    server.join().expect("healthy-force server thread");
    assert!(
        device.lock().unwrap().flushes >= 1,
        "a force detach of a healthy volume still commits the device cache"
    );
    assert_eq!(
        VOLUME_FOREST.resolve(&identity),
        None,
        "the force-detached identity no longer resolves"
    );
}

/// Assert the file on the mounted volume at `path` holds exactly `expected`.
fn assert_volume_reads(path: &str, expected: &[u8]) {
    let mut buf = [0u8; 64];
    let n = read_as(0, path, 0, &mut buf).unwrap_or_else(|err| std::panic!("read {path}: {err:?}"));
    assert_eq!(&buf[..n], expected, "{path}");
}

/// Both mounted siblings of one disk serve, and rewrite, their own extent's
/// bytes: the staging window they share never hands one volume the other's
/// data (`plans/FIX-IO.md` invariant 9).
fn assert_each_sibling_serves_its_own_extent() {
    assert_volume_reads("/Storage/part1/hello.txt", b"first partition payload");
    assert_volume_reads("/Storage/part2/hello.txt", b"second partition payload");

    // Interleaved writes through both mounts stay on their own extents.
    write_as(0, "/Storage/part1/hello.txt", 0, b"1").expect("write the first partition");
    write_as(0, "/Storage/part2/hello.txt", 0, b"2").expect("write the second partition");
    assert_volume_reads("/Storage/part1/hello.txt", b"1irst partition payload");
    assert_volume_reads("/Storage/part2/hello.txt", b"2econd partition payload");
}

/// Two volumes on **one** disk share one block client, and that client is
/// released when the last of them goes (`plans/FIX-IO.md`).
///
/// A disk's volumes are served over one endpoint and one shared data
/// window, and the blkio protocol stages every transfer's bytes in that
/// window. A second client on the same device would therefore be a second
/// concurrent user of the one staging buffer: two transfers in flight
/// would overwrite each other's bytes and hand a reader another extent's
/// data — silent corruption, not a fault. So the service connects the
/// device once and serialises every volume's operations onto it.
///
/// Connecting is the only thing that queries the geometry, so the served
/// device's geometry count *is* the number of clients that ever existed for
/// it: this scenario pins that count rather than racing two mounts and
/// hoping to observe a corruption.
fn sibling_volumes_share_one_client_scenario(window_ptr: *mut u8, facility: &'static TestFacility) {
    // One disk carrying two distinct FAT32 volumes back to back — the
    // ordinary partitioned stick.
    let (first, first_identity) = fat32_image(0x0D15_C007, b"first partition payload");
    let (second, second_identity) = fat32_image(0x0D15_C008, b"second partition payload");
    let extent_blocks = (first.data.len() / BLOCK_SIZE) as u64;
    let mut data = first.data;
    data.extend_from_slice(&second.data);
    let disk = RamBlock { data };

    let endpoint_id = 0xB1D0_5E17_0000_0007_u64;
    let endpoint = register_endpoint(endpoint_id, 0x70_1007);
    let device = StdArc::new(Mutex::new(ServedDevice::new(disk)));
    let stop = StdArc::new(AtomicBool::new(false));
    let server = serve(
        StdArc::clone(&endpoint),
        window_ptr as usize,
        StdArc::clone(&device),
        StdArc::clone(&stop),
    );
    let (_va, region) = sharedreg::create(facility, ProcessId(0x70_0008), 8).expect("region");

    let attach_extent = |name: &'static [u8], first_lba: u64| VolumeAttachRequest {
        endpoint: endpoint_id,
        window: region,
        first_lba,
        blocks: extent_blocks,
        fstype: VolumeFsType::Fat32,
        name,
    };
    VOLUME_SERVICE
        .attach(&attach_extent(b"part1", 0))
        .expect("attach the first partition");
    VOLUME_SERVICE
        .attach(&attach_extent(b"part2", extent_blocks))
        .expect("attach the second partition");

    assert_eq!(
        device.lock().unwrap().geometries,
        1,
        "both volumes on one disk drive one shared client, not one each"
    );

    assert_each_sibling_serves_its_own_extent();

    // Detaching commits through the same shared client: the device-cache
    // flush no longer opens a client of its own behind the sibling's back.
    VOLUME_SERVICE
        .detach(&VolumeDetachRequest {
            volume_id: first_identity,
            force: false,
        })
        .expect("detach the first partition");
    assert_eq!(
        device.lock().unwrap().geometries,
        1,
        "the detach-time device flush reuses the shared client"
    );
    // The surviving sibling still serves: retiring one volume never closes
    // a device another volume is still mounted on.
    assert_volume_reads("/Storage/part2/hello.txt", b"2econd partition payload");
    assert_eq!(
        device.lock().unwrap().geometries,
        1,
        "the surviving volume kept the one client, it did not reconnect"
    );

    VOLUME_SERVICE
        .detach(&VolumeDetachRequest {
            volume_id: second_identity,
            force: false,
        })
        .expect("detach the second partition");

    // The disk's last volume went, so the shared client was released: a
    // fresh attach connects anew rather than reusing a retired one.
    VOLUME_SERVICE
        .attach(&attach_extent(b"part1", 0))
        .expect("re-attach the first partition");
    assert_eq!(
        device.lock().unwrap().geometries,
        2,
        "the released client is re-connected, not resurrected"
    );
    VOLUME_SERVICE
        .detach(&VolumeDetachRequest {
            volume_id: first_identity,
            force: false,
        })
        .expect("detach the re-attached partition");

    stop.store(true, Ordering::Relaxed);
    server.join().expect("sibling scenario server thread");
}

/// A device whose geometry reply names a storage medium this ABI does not
/// define mounts as *unknown*, and still serves.
///
/// The mount record is what a file manager picks a drive icon from, so
/// answering with a class nobody declared would put a specific medium on
/// screen on the strength of a word the client could not read. Unknown is
/// the honest answer, and it resolves to the generic drive icon. The medium
/// is advisory, so failing to read it never costs the volume its mount.
fn unrecognised_medium_mounts_as_unknown_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (image, identity) = fat32_image(0x0D15_C009, b"unclassified payload");
    let blocks = (image.data.len() / BLOCK_SIZE) as u64;
    let endpoint_id = 0xB1D0_5E17_0000_0009_u64;
    let endpoint = register_endpoint(endpoint_id, 0x70_1009);
    let device = StdArc::new(Mutex::new(ServedDevice::unclassified(image)));
    let stop = StdArc::new(AtomicBool::new(false));
    let server = serve(
        StdArc::clone(&endpoint),
        window_ptr as usize,
        StdArc::clone(&device),
        StdArc::clone(&stop),
    );
    let (_va, region) = sharedreg::create(facility, ProcessId(0x70_0009), 8).expect("region");

    VOLUME_SERVICE
        .attach(&VolumeAttachRequest {
            endpoint: endpoint_id,
            window: region,
            first_lba: 0,
            blocks,
            fstype: VolumeFsType::Fat32,
            name: b"odd1",
        })
        .expect("attach the volume whose class word is unreadable");

    assert_eq!(
        mount_record(b"odd1").medium(),
        None,
        "an unreadable class word is reported unknown, never guessed"
    );
    assert_volume_reads("/Storage/odd1/hello.txt", b"unclassified payload");

    VOLUME_SERVICE
        .detach(&VolumeDetachRequest {
            volume_id: identity,
            force: false,
        })
        .expect("detach the volume whose class word is unreadable");

    stop.store(true, Ordering::Relaxed);
    server
        .join()
        .expect("unrecognised-medium scenario server thread");
}

/// A disk that stops answering mid-attach holds up only its own volumes:
/// another disk attaches and serves while the first waits on its device.
///
/// The stalled disk withholds its first answer, the connect's geometry, so
/// no transfer of its own has staged bytes in the one test window the other
/// disk's transfers use.
fn a_device_that_stops_answering_holds_up_no_other_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (stalled_image, stalled_identity) = fat32_image(0x0D15_C040, b"stalled payload");
    let stalled_blocks = (stalled_image.data.len() / BLOCK_SIZE) as u64;
    let stalled_endpoint = 0xB1D0_5E17_0000_0040_u64;
    let (received, has_request) = mpsc::channel();
    let (open_gate, gate) = mpsc::channel();
    let stalled = StdArc::new(Mutex::new(ServedDevice {
        hold_first: Some((received, gate)),
        ..ServedDevice::new(stalled_image)
    }));
    let stalled_stop = StdArc::new(AtomicBool::new(false));
    let stalled_server = serve(
        register_endpoint(stalled_endpoint, 0x70_1040),
        window_ptr as usize,
        StdArc::clone(&stalled),
        StdArc::clone(&stalled_stop),
    );
    let (_va, stalled_region) =
        sharedreg::create(facility, ProcessId(0x70_0040), 8).expect("region");
    let stalled_attach = thread::spawn(move || {
        VOLUME_SERVICE.attach(&VolumeAttachRequest {
            endpoint: stalled_endpoint,
            window: stalled_region,
            first_lba: 0,
            blocks: stalled_blocks,
            fstype: VolumeFsType::Fat32,
            name: b"stall1",
        })
    });
    has_request
        .recv_timeout(Duration::from_secs(60))
        .expect("the stalled disk holds the attach's first request");

    let (image, identity) = fat32_image(0x0D15_C041, b"healthy payload");
    let blocks = (image.data.len() / BLOCK_SIZE) as u64;
    let endpoint = 0xB1D0_5E17_0000_0041_u64;
    let stop = StdArc::new(AtomicBool::new(false));
    let server = serve(
        register_endpoint(endpoint, 0x70_1041),
        window_ptr as usize,
        StdArc::new(Mutex::new(ServedDevice::new(image))),
        StdArc::clone(&stop),
    );
    let (_va, region) = sharedreg::create(facility, ProcessId(0x70_0041), 8).expect("region");
    let (attached, outcome) = mpsc::channel();
    let healthy_attach = thread::spawn(move || {
        let result = VOLUME_SERVICE.attach(&VolumeAttachRequest {
            endpoint,
            window: region,
            first_lba: 0,
            blocks,
            fstype: VolumeFsType::Fat32,
            name: b"well1",
        });
        attached.send(result).unwrap();
    });
    let healthy = outcome.recv_timeout(Duration::from_secs(60));
    if healthy.is_ok() {
        assert_volume_reads("/Storage/well1/hello.txt", b"healthy payload");
    }
    // Answered whatever happened, so a failure never wedges the suite.
    open_gate.send(()).unwrap();
    assert_eq!(
        healthy,
        Ok(Ok(())),
        "the other disk attached while the stalled one waited on its device"
    );
    healthy_attach.join().expect("the healthy attach thread");
    assert_eq!(
        stalled_attach.join().expect("the stalled attach thread"),
        Ok(()),
        "the stalled disk attaches once it answers"
    );
    assert_volume_reads("/Storage/stall1/hello.txt", b"stalled payload");
    assert_every_attached_volume_is_served();

    for volume_id in [identity, stalled_identity] {
        VOLUME_SERVICE
            .detach(&VolumeDetachRequest {
                volume_id,
                force: false,
            })
            .expect("detach");
    }
    for (stop, server) in [(stop, server), (stalled_stop, stalled_server)] {
        stop.store(true, Ordering::Relaxed);
        server.join().expect("scenario server thread");
    }
}

/// Deterministic entropy for the fixture volume's key hierarchy. The
/// image never leaves this test, so a counter is the right source: a real
/// RNG would make the volume unreproducible for no gain.
struct FixtureEntropy {
    next: u8,
}

impl EntropySource for FixtureEntropy {
    fn fill(&mut self, out: &mut [u8]) -> Result<(), DriverError> {
        for byte in out.iter_mut() {
            *byte = self.next;
            self.next = self.next.wrapping_add(1);
        }
        Ok(())
    }
}

/// Format a plaintext ARXFS image, as the attach path opens a removable
/// ARXFS volume, carrying one regular file.
fn arxfs_image(payload: &[u8]) -> (RamBlock, [u8; 16]) {
    let mut fs = ARXFS::format(
        RamBlock::new(SECTORS_8MIB),
        ARXFS_INODES,
        Keying::Plaintext,
        &mut FixtureEntropy { next: 1 },
    )
    .expect("format the fixture volume");
    let root = fs.root();
    let file = fs
        .create(
            root,
            b"target.txt",
            NodeKind::RegularFile,
            &NodeSecurity::new(0o644, 0, 0),
        )
        .expect("create");
    fs.write_at(file, 0, payload).expect("write");
    // The volume root must admit the system principal the service calls as,
    // because ARXFS carries its own per-inode records rather than taking a
    // mount template.
    fs.set_security(root, NodeSecurity::new(0o755, 0, 0))
        .expect("root security");
    fs.set_security(file, NodeSecurity::new(0o644, 0, 0))
        .expect("file security");
    fs.flush().expect("commit the fixture");
    let identity = fs.volume_uuid();
    (fs.into_block().expect("the volume closes"), identity)
}

/// Both link kinds, created and read back through the **real** global
/// `FS_SERVICE` over a served, attached ARXFS volume.
///
/// Every other link test in the tree builds a `Vfs` over a driver
/// directly and so never touches the production wrapper chain
/// (`CachedFs` inside `Box<dyn KernelFs>` behind
/// `MountedFilesystemService`). That is exactly how `read_link` /
/// `create_link` / `link` came to sit at their trait defaults in three
/// wrappers at once: the link surface answered "this volume has no links"
/// on every genuinely mounted volume while the whole `posix_fs_suite`
/// link vertical stayed green. This scenario is the one that would have
/// failed.
fn link_round_trip_through_the_real_service_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (image, identity) = arxfs_image(b"link target payload");
    let device_blocks = (image.data.len() / BLOCK_SIZE) as u64;
    let endpoint_id = 0xB1D0_5E17_0000_000A_u64;
    let endpoint = register_endpoint(endpoint_id, 0x70_100A);
    let device = StdArc::new(Mutex::new(ServedDevice::new(image)));
    let stop = StdArc::new(AtomicBool::new(false));
    let server = serve(
        StdArc::clone(&endpoint),
        window_ptr as usize,
        StdArc::clone(&device),
        StdArc::clone(&stop),
    );
    let (_va, region_id) = sharedreg::create(facility, ProcessId(0x70_200A), 8).expect("region");

    VOLUME_SERVICE
        .attach(&VolumeAttachRequest {
            endpoint: endpoint_id,
            window: region_id,
            first_lba: 0,
            blocks: device_blocks,
            fstype: VolumeFsType::ARXFS,
            name: b"arx1",
        })
        .expect("attach the ARXFS volume");

    // A symbolic link: created, then read back verbatim.
    FS_SERVICE
        .symlink(0, &NoCaps, "target.txt", root("/Storage/arx1/alias"))
        .expect("symlink through the production service");
    assert_eq!(
        FS_SERVICE
            .readlink(0, &NoCaps, root("/Storage/arx1/alias"))
            .expect("readlink through the production service"),
        String::from("target.txt"),
        "the stored target comes back verbatim through the wrapper chain"
    );
    // Following it reaches the target's bytes, so the link resolves rather
    // than merely existing.
    let mut buf = [0u8; 64];
    let read = read_as(0, "/Storage/arx1/alias", 0, &mut buf).expect("read through the link");
    assert_eq!(&buf[..read], b"link target payload");

    // A hard link: a second name reaching the same node, with the count
    // the format keeps visible through `stat`.
    FS_SERVICE
        .link(
            0,
            &NoCaps,
            root("/Storage/arx1/target.txt"),
            root("/Storage/arx1/second"),
            FinalLink::Keep,
        )
        .expect("link through the production service");
    let original = FS_SERVICE
        .stat(
            0,
            &NoCaps,
            root("/Storage/arx1/target.txt"),
            FinalLink::Keep,
        )
        .expect("stat the first name");
    let second = FS_SERVICE
        .stat(0, &NoCaps, root("/Storage/arx1/second"), FinalLink::Keep)
        .expect("stat the second name");
    assert_eq!(original.id, second.id, "both names reach one node");
    assert_eq!(original.nlink, 2, "the format's own count is carried up");
    assert_eq!(second.nlink, 2);

    VOLUME_SERVICE
        .detach(&VolumeDetachRequest {
            volume_id: identity,
            force: false,
        })
        .expect("detach the ARXFS volume");
    stop.store(true, Ordering::Relaxed);
    server.join().expect("arxfs server thread");
}

/// The byte of an ext4 superblock's `s_state` and its "errors detected" bit,
/// and the device block the superblock starts in.
const EXT4_STATE_BYTE: usize = 1024 + 0x3A;
const EXT4_STATE_ERRORS: u8 = 0x02;
const EXT4_SUPERBLOCK_LBA: u64 = (1024 / BLOCK_SIZE) as u64;

/// Format the unjournaled ext4 image `Ext4::format` lays, carrying
/// `hello.txt`.
fn ext4_image(uuid: [u8; 16]) -> RamBlock {
    let mut fs = Ext4::format(RamBlock::new(SECTORS_8MIB), 128, uuid).expect("format");
    let root = fs.root();
    let file = fs
        .create(
            root,
            b"hello.txt",
            NodeKind::RegularFile,
            &NodeSecurity::new(0o644, 0, 0),
        )
        .expect("create");
    fs.write_at(file, 0, b"ext4 payload").expect("write");
    fs.into_block().expect("the volume closes")
}

/// A real `mke2fs` ext2 volume, as the ext driver's own tests keep it.
const EXT2_FIXTURE: &[u8] =
    include_bytes!("../../../drivers/filesystem/ext4/tests/fixtures/ext2.sparse");

/// A real e2fsprogs volume whose root is `0:0` `0755` and whose `/f` is
/// `1000:1000` `0664` under a POSIX ACL: named user 1000 `rw-`, named group
/// 2000 `r--`, owning group `r--`, mask `rw-`.
const EXT_ACL_FIXTURE: &[u8] =
    include_bytes!("../../../drivers/filesystem/ext4/tests/fixtures/acl_block.sparse");

#[path = "../../../drivers/filesystem/ext4/tests/support/sparse.rs"]
mod ext_fixture;

/// Detach the volume `identity` and end its serving round.
fn retire(identity: [u8; 16], force: bool, round: Inserted) {
    VOLUME_SERVICE
        .detach(&VolumeDetachRequest {
            volume_id: identity,
            force,
        })
        .expect("the volume detaches");
    round.stop.store(true, Ordering::Relaxed);
    round.server.join().expect("server thread");
}

/// A second volume asking for a catalog name the first holds is answered
/// "taken" and recorded as such, not as a refused volume, because the volume
/// manager tries its next name on that answer; the first volume is untouched.
/// The answer comes before the second filesystem is opened, so one that would
/// not even mount is still told its name is taken.
fn a_taken_catalog_name_is_answered_as_taken_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (image, identity) = fat32_image(0x0D15_C00A, b"first");
    let first = insert(
        window_ptr,
        facility,
        &Insertion {
            endpoint_id: 0xB1D0_5E17_0000_0010,
            owner: 0x70_1020,
            region_task: 0x70_0020,
            name: b"usb8",
            fstype: VolumeFsType::Fat32,
        },
        ServedDevice::new(image),
    );
    first.outcome.expect("the first volume attaches");
    let unmountable = RamBlock::new(SECTORS_64MIB);
    let second = insert(
        window_ptr,
        facility,
        &Insertion {
            endpoint_id: 0xB1D0_5E17_0000_0011,
            owner: 0x70_1021,
            region_task: 0x70_0021,
            name: b"usb8",
            fstype: VolumeFsType::Fat32,
        },
        ServedDevice::new(unmountable),
    );
    assert_eq!(second.outcome, Err(Errno::AlreadyExists));
    assert_eq!(
        audited(NAME_TAKEN, "usb8"),
        [None],
        "recorded as a taken name"
    );
    assert!(
        audited(ATTACH_REFUSED, "usb8").is_empty(),
        "a taken name is not a refused volume"
    );
    let mut buf = [0u8; 8];
    let n =
        read_as(0, "/Storage/usb8/hello.txt", 0, &mut buf).expect("the first volume still serves");
    assert_eq!(&buf[..n], b"first");
    retire(identity, false, first);
    second.stop.store(true, Ordering::Relaxed);
    second.server.join().expect("second server thread");
}

/// An ext4 volume carrying errors a checker recorded is attached read-only
/// and the audit names why, so a write reads as a read-only file system and
/// not as a permission the user could be granted.
fn a_volume_its_driver_refuses_to_write_mounts_read_only_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let identity = [0xE4; 16];
    let mut image = ext4_image(identity);
    image.data[EXT4_STATE_BYTE] |= EXT4_STATE_ERRORS;
    let round = insert(
        window_ptr,
        facility,
        &Insertion {
            endpoint_id: 0xB1D0_5E17_0000_0012,
            owner: 0x70_1022,
            region_task: 0x70_0022,
            name: b"ext4ro",
            fstype: VolumeFsType::Ext4,
        },
        ServedDevice::new(image),
    );
    round.outcome.expect("the volume attaches");
    assert_eq!(
        audited(ATTACHED_READ_ONLY, "ext4ro"),
        [Some(String::from("errors_recorded"))]
    );
    let mut buf = [0u8; 16];
    let n = read_as(0, "/Storage/ext4ro/hello.txt", 0, &mut buf)
        .expect("a read-only volume serves reads");
    assert_eq!(&buf[..n], b"ext4 payload");
    assert_eq!(
        write_as(0, "/Storage/ext4ro/hello.txt", 0, b"X").err(),
        Some(Errno::ReadOnly)
    );
    retire(identity, false, round);
}

/// The mount table names the ext generation a volume's features make it, as
/// `mount` reports it, not the driver serving it.
fn an_ext2_volume_mounts_as_ext2_scenario(window_ptr: *mut u8, facility: &'static TestFacility) {
    let image = ext_fixture::sparse(EXT2_FIXTURE);
    let identity = Ext4::open_read_only(RamBlock {
        data: image.clone(),
    })
    .expect("an ext2 volume")
    .volume_uuid();
    let round = insert(
        window_ptr,
        facility,
        &Insertion {
            endpoint_id: 0xB1D0_5E17_0000_0015,
            owner: 0x70_1025,
            region_task: 0x70_0025,
            name: b"ext2vol",
            fstype: VolumeFsType::Ext4,
        },
        ServedDevice::new(RamBlock { data: image }),
    );
    round.outcome.expect("the volume attaches");
    assert_eq!(mount_record(b"ext2vol").fstype_bytes(), b"ext2");
    retire(identity, false, round);
}

/// An ext volume made elsewhere, served as a removable USB stick, is
/// presented with the ownership it stores — never mapped as an ownerless FAT
/// volume is — so its records reach every caller unchanged, a user who is
/// not its root's owner is refused at its top level, a node made there
/// carries its creator, and `chmod` and `chown` follow the ordinary owner and
/// capability rules.
fn an_ext_volume_keeps_its_on_disk_ownership_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let image = ext_fixture::sparse(EXT_ACL_FIXTURE);
    let identity = Ext4::open_read_only(RamBlock {
        data: image.clone(),
    })
    .expect("an e2fsprogs volume")
    .volume_uuid();
    let removable = BlkMedium::of_transport(BlkTransport::Usb)
        .with_removability(BlkRemovability::Removable(BlkRemovabilityEvidence::Inquiry));
    let round = insert(
        window_ptr,
        facility,
        &Insertion {
            endpoint_id: 0xB1D0_5E17_0000_0016,
            owner: 0x70_1026,
            region_task: 0x70_0026,
            name: b"extown",
            fstype: VolumeFsType::Ext4,
        },
        ServedDevice {
            medium: Some(removable),
            ..ServedDevice::new(RamBlock { data: image })
        },
    );
    round.outcome.expect("the volume attaches");
    assert_the_volume_presents_its_records();
    assert_ownership_rules_govern_what_is_made();

    VOLUME_SERVICE
        .detach(&VolumeDetachRequest {
            volume_id: identity,
            force: false,
        })
        .expect("the volume detaches");
    round.stop.store(true, Ordering::Relaxed);
    round.server.join().expect("server thread");
    let kept = round.device.lock().unwrap().block.data.clone();
    let mut fs = Ext4::open(RamBlock { data: kept }).expect("the volume reopens");
    let shared = fs.lookup(fs.root(), b"shared").expect("shared");
    let file = fs.lookup(shared, b"mine.txt").expect("mine.txt");
    let stored = fs.security(file).expect("its record");
    assert_eq!(
        (stored.uid, stored.gid, stored.mode),
        (OUTSIDER_UID, STORAGE_GID, 0o600),
        "the medium stores what was made on it"
    );
}

/// The `(uid, gid, mode)` the node at `path` presents to `uid`.
fn owned(uid: u32, path: &str) -> (u32, u32, u32) {
    let stat = FS_SERVICE
        .stat(uid, &NoCaps, root(path), FinalLink::Follow)
        .expect("stat");
    (stat.uid, stat.gid, stat.mode)
}

/// The ACL fixture's records reach every caller as stored, the ACL decides
/// over the mode's group triad, and its root's owner alone makes anything
/// at the top level.
fn assert_the_volume_presents_its_records() {
    assert_eq!(owned(OUTSIDER_UID, "/Storage/extown"), (0, 0, 0o755));
    assert_eq!(
        owned(OUTSIDER_UID, "/Storage/extown/f"),
        (1000, FIXTURE_GID, 0o664)
    );
    let create = OpenFlags::WRITE
        .union(OpenFlags::CREATE)
        .union(OpenFlags::EXCLUSIVE);
    assert_eq!(
        FS_SERVICE
            .mkdir(MEMBER_UID, &NoCaps, root("/Storage/extown/mine"), new_dir())
            .err(),
        Some(Errno::PermissionDenied),
        "the volume's owner refuses a folder at its top level"
    );
    assert_eq!(
        open_as(MEMBER_UID, "/Storage/extown/mine.txt", create).err(),
        Some(Errno::PermissionDenied),
        "and a file"
    );
    let mut buf = [0u8; 8];
    read_as(GROUP_PEER_UID, "/Storage/extown/f", 0, &mut buf).expect("the owning group reads");
    assert_eq!(
        write_as(GROUP_PEER_UID, "/Storage/extown/f", 0, b"X").err(),
        Some(Errno::PermissionDenied),
        "the ACL's owning-group entry decides, not the mode's group triad"
    );
    write_as(MEMBER_UID, "/Storage/extown/f", 0, b"X").expect("the owner writes");
}

/// A node made on the volume carries its creator, and `chmod` and `chown`
/// follow the owner and capability rules of any volume.
fn assert_ownership_rules_govern_what_is_made() {
    FS_SERVICE
        .mkdir(0, &NoCaps, root("/Storage/extown/shared"), new_dir())
        .expect("the root's owner makes a folder");
    assert_eq!(owned(0, "/Storage/extown/shared").0, 0);
    FS_SERVICE
        .set_mode(
            0,
            &NoCaps,
            root("/Storage/extown/shared"),
            0o777,
            FinalLink::Follow,
        )
        .expect("its owner opens it to everyone");
    let mine = "/Storage/extown/shared/mine.txt";
    let create = OpenFlags::WRITE
        .union(OpenFlags::CREATE)
        .union(OpenFlags::EXCLUSIVE);
    open_as(MEMBER_UID, mine, create).expect("a member makes a file there");
    let (uid, gid, _) = owned(MEMBER_UID, mine);
    assert_eq!(
        (uid, gid),
        (MEMBER_UID, STORAGE_GID),
        "a new node carries its creator's user and group"
    );
    assert_eq!(
        FS_SERVICE
            .set_mode(OUTSIDER_UID, &NoCaps, root(mine), 0o666, FinalLink::Follow)
            .err(),
        Some(Errno::PermissionDenied),
        "only the owner changes its mode"
    );
    FS_SERVICE
        .set_mode(MEMBER_UID, &NoCaps, root(mine), 0o600, FinalLink::Follow)
        .expect("its owner does");
    assert_eq!(owned(MEMBER_UID, mine).2, 0o600);
    assert_eq!(
        FS_SERVICE
            .set_owner(
                MEMBER_UID,
                &NoCaps,
                root(mine),
                OUTSIDER_UID,
                FS_OWNER_UNCHANGED,
                FinalLink::Follow,
            )
            .err(),
        Some(Errno::PermissionDenied),
        "giving a node away needs the chown authority"
    );
    FS_SERVICE
        .set_owner(
            MEMBER_UID,
            &Chown,
            root(mine),
            OUTSIDER_UID,
            FS_OWNER_UNCHANGED,
            FinalLink::Follow,
        )
        .expect("which grants it");
    assert_eq!(owned(OUTSIDER_UID, mine).0, OUTSIDER_UID);
}

/// A stick pulled mid-commit (`plans/DEVICES.md` D4c): the volume had been
/// marked not clean and the pull came before the commit finished. Once its
/// retained writes are replayed the volume needs a check, so it returns
/// read-only and the recovery audit says why.
fn a_stick_pulled_mid_commit_returns_read_only_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let identity = [0xE5; 16];
    let owner = 0x70_1023;
    let pulled = insert(
        window_ptr,
        facility,
        &Insertion {
            endpoint_id: 0xB1D0_5E17_0000_0013,
            owner,
            region_task: 0x70_0023,
            name: b"ext4pull",
            fstype: VolumeFsType::Ext4,
        },
        ServedDevice::pulled_after(ext4_image(identity), EXT4_SUPERBLOCK_LBA),
    );
    pulled.outcome.expect("the volume attaches writable");
    assert!(
        audited(ATTACHED_READ_ONLY, "ext4pull").is_empty(),
        "a clean volume attaches writable"
    );
    assert!(
        write_as(0, "/Storage/ext4pull/hello.txt", 0, b"D").is_err(),
        "the pull interrupts the commit"
    );
    tairix_kernel_core::callreg::teardown_owned_by(owner, &ASPACES, &SINK);
    pulled.stop.store(true, Ordering::Relaxed);
    pulled.server.join().expect("pulled server thread");
    let kept = pulled.device.lock().unwrap().block.data.clone();

    let back = insert(
        window_ptr,
        facility,
        &Insertion {
            endpoint_id: 0xB1D0_5E17_0000_0014,
            owner: 0x70_1024,
            region_task: 0x70_0024,
            name: b"ext4pull",
            fstype: VolumeFsType::Ext4,
        },
        ServedDevice::new(RamBlock { data: kept }),
    );
    back.outcome.expect("the re-insert recovers the volume");
    assert_eq!(
        audited(ATTACHED_READ_ONLY, "ext4pull"),
        [Some(String::from("needs_check"))]
    );
    assert_eq!(
        write_as(0, "/Storage/ext4pull/hello.txt", 0, b"X").err(),
        Some(Errno::ReadOnly)
    );
    retire(identity, false, back);
}

/// A real `genisoimage` disc with Rock Ridge and Joliet.
const ISO_FIXTURE: &[u8] =
    include_bytes!("../../../drivers/filesystem/iso9660/tests/fixtures/rr-joliet.sparse");

/// An ISO 9660 disc image attaches read-only, the refusal audited as the
/// format's, and serves its files.
fn a_disc_image_attaches_read_only_scenario(window_ptr: *mut u8, facility: &'static TestFacility) {
    let image = ext_fixture::sparse(ISO_FIXTURE);
    let identity = Iso9660::open(RamBlock {
        data: image.clone(),
    })
    .expect("a disc")
    .volume_identity();
    let round = insert(
        window_ptr,
        facility,
        &Insertion {
            endpoint_id: 0xB1D0_5E17_0000_0030,
            owner: 0x70_1030,
            region_task: 0x70_0030,
            name: b"disc",
            fstype: VolumeFsType::Iso9660,
        },
        ServedDevice::new(RamBlock { data: image }),
    );
    round.outcome.expect("the disc attaches");
    assert_eq!(
        audited(ATTACHED_READ_ONLY, "disc"),
        [Some(String::from("format_read_only"))]
    );
    assert_eq!(mount_record(b"disc").fstype_bytes(), b"iso9660");
    assert_volume_reads("/Storage/disc/README.TXT", b"hello\n");
    assert_eq!(
        write_as(0, "/Storage/disc/README.TXT", 0, b"X").err(),
        Some(Errno::ReadOnly)
    );
    retire(identity, false, round);
}

/// A UDF volume attaches writable: a file written through the production
/// service reads back.
fn a_udf_volume_attaches_writable_scenario(window_ptr: *mut u8, facility: &'static TestFacility) {
    let options = FormatOptions {
        label: "Exchange",
        revision: 0x0201,
        serial: [4; 8],
        now: tairix_abi::time::Time64::from_secs(1_700_000_000),
    };
    let udf = Udf::format(RamBlock::new(SECTORS_8MIB), &options).expect("formats");
    let identity = udf.volume_identity();
    let round = insert(
        window_ptr,
        facility,
        &Insertion {
            endpoint_id: 0xB1D0_5E17_0000_0031,
            owner: 0x70_1031,
            region_task: 0x70_0031,
            name: b"exchange",
            fstype: VolumeFsType::Udf,
        },
        ServedDevice::new(udf.into_device()),
    );
    round.outcome.expect("the volume attaches");
    assert!(audited(ATTACHED_READ_ONLY, "exchange").is_empty());
    let create = OpenFlags::WRITE.union(OpenFlags::CREATE);
    open_as(0, "/Storage/exchange/note.txt", create).expect("a file is made");
    write_as(0, "/Storage/exchange/note.txt", 0, b"udf payload").expect("and written");
    assert_volume_reads("/Storage/exchange/note.txt", b"udf payload");
    retire(identity, false, round);
}

/// An ADFS volume attaches writable through the same path.
fn an_adfs_volume_attaches_writable_scenario(window_ptr: *mut u8, facility: &'static TestFacility) {
    let adfs = Adfs::format(RamBlock::new(1600), AdfsVariant::E).expect("formats");
    let identity = adfs.volume_identity();
    let round = insert(
        window_ptr,
        facility,
        &Insertion {
            endpoint_id: 0xB1D0_5E17_0000_0032,
            owner: 0x70_1032,
            region_task: 0x70_0032,
            name: b"acorn",
            fstype: VolumeFsType::Adfs,
        },
        ServedDevice::new(adfs.into_device()),
    );
    round.outcome.expect("the volume attaches");
    let create = OpenFlags::WRITE.union(OpenFlags::CREATE);
    open_as(0, "/Storage/acorn/ReadMe", create).expect("a file is made");
    write_as(0, "/Storage/acorn/ReadMe", 0, b"filecore").expect("and written");
    assert_volume_reads("/Storage/acorn/ReadMe", b"filecore");
    retire(identity, false, round);
}

/// What the volume shown at `/Storage/<name>` reports of its space once
/// what it holds is committed.
fn space(name: &str) -> VolumeStats {
    let driver = LATE_FILESYSTEM.driver(served_handle(name)).expect("served");
    let mut fs = driver.lock();
    fs.flush().expect("commits");
    fs.stats().expect("its space")
}

/// A plain detach is refused while a descriptor holds one of the volume's
/// nodes, and changes nothing; a force revokes the descriptor, which then
/// answers stale, never reaching a volume attached under its name since.
fn a_held_descriptor_holds_its_volume_attached_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (image, identity) = fat32_image(0x0D15_C070, b"held payload");
    let first = insert(
        window_ptr,
        facility,
        &Insertion {
            endpoint_id: 0xB1D0_5E17_0000_0070,
            owner: 0x70_1070,
            region_task: 0x70_0070,
            name: b"held1",
            fstype: VolumeFsType::Fat32,
        },
        ServedDevice::new(image),
    );
    first.outcome.expect("the volume attaches");
    let held = Descriptor::open(0, "/Storage/held1/hello.txt", OpenFlags::READ).expect("opens");
    let plain = VolumeDetachRequest {
        volume_id: identity,
        force: false,
    };
    assert_eq!(VOLUME_SERVICE.detach(&plain), Err(Errno::Busy));
    assert_eq!(
        audited(DETACH_REFUSED, "held1"),
        [Some(String::from("nodes_open"))]
    );
    assert_eq!(
        audited_count(DETACH_REFUSED, "held1", "open_nodes"),
        [Some(1)]
    );
    let mut buf = [0u8; 16];
    let n = held.read(0, &mut buf).expect("the refusal changed nothing");
    assert_eq!(&buf[..n], b"held payload");

    VOLUME_SERVICE
        .detach(&VolumeDetachRequest {
            volume_id: identity,
            force: true,
        })
        .expect("a force detach revokes what is held");
    assert_eq!(held.read(0, &mut buf), Err(Errno::Stale));
    assert_eq!(
        audited(FORCE_UNMOUNTED, "held1"),
        [Some(String::from("nodes_open"))]
    );
    assert_eq!(
        audited_count(FORCE_UNMOUNTED, "held1", "revoked_nodes"),
        [Some(1)]
    );
    first.stop.store(true, Ordering::Relaxed);
    first.server.join().expect("server thread");

    let (image, other) = fat32_image(0x0D15_C071, b"other payload");
    let second = insert(
        window_ptr,
        facility,
        &Insertion {
            endpoint_id: 0xB1D0_5E17_0000_0071,
            owner: 0x70_1071,
            region_task: 0x70_0071,
            name: b"held1",
            fstype: VolumeFsType::Fat32,
        },
        ServedDevice::new(image),
    );
    second
        .outcome
        .expect("another volume attaches under the name");
    assert_volume_reads("/Storage/held1/hello.txt", b"other payload");
    assert_eq!(
        held.read(0, &mut buf),
        Err(Errno::Stale),
        "a revoked descriptor never reaches the volume attached under its name since"
    );
    drop(held);
    retire(other, false, second);
}

/// A descriptor of an ARXFS file stays with the node it opened: a rename,
/// another file under its old name, a link planted there and an unlink move
/// nothing, and the last close frees what the unlink left. Its opener keeps
/// writing a file it owns whatever the mode, while a grant and another
/// principal lose what the mode no longer gives.
fn a_descriptor_stays_with_its_node_scenario(window_ptr: *mut u8, facility: &'static TestFacility) {
    let (image, identity) = arxfs_image(b"original payload");
    let round = insert(
        window_ptr,
        facility,
        &Insertion {
            endpoint_id: 0xB1D0_5E17_0000_0074,
            owner: 0x70_1074,
            region_task: 0x70_0074,
            name: b"arxn",
            fstype: VolumeFsType::ARXFS,
        },
        ServedDevice::new(image),
    );
    round.outcome.expect("the volume attaches");
    a_descriptor_follows_its_node_past_its_names("arxn");
    an_unlinked_file_keeps_its_blocks_until_its_last_close("arxn");
    a_chmod_withdraws_what_a_fresh_open_would_be_refused("arxn");
    retire(identity, false, round);
}

/// The descriptor of `volume`'s `target.txt` reads and writes the node it
/// opened past a rename, another file taking the name, and a link planted
/// at the name it moved to.
fn a_descriptor_follows_its_node_past_its_names(volume: &str) {
    let at = |name: &str| std::format!("/Storage/{volume}/{name}");
    let rename = |from: &str, to: &str| {
        FS_SERVICE
            .rename(
                0,
                &NoCaps,
                root(&at(from)),
                root(&at(to)),
                RenameFlags::empty(),
                None,
            )
            .expect("renames");
    };
    let create = OpenFlags::WRITE.union(OpenFlags::CREATE);
    let mut buf = [0u8; 32];
    let first = Descriptor::open(
        0,
        &at("target.txt"),
        OpenFlags::READ.union(OpenFlags::WRITE),
    )
    .expect("opens");
    rename("target.txt", "moved.txt");
    open_as(0, &at("target.txt"), create).expect("another file takes the name");
    write_as(0, &at("target.txt"), 0, b"replacement").expect("fills it");
    let n = first.read(0, &mut buf).expect("reads");
    assert_eq!(
        &buf[..n],
        b"original payload",
        "it reads the node it opened"
    );
    first.write(0, b"O").expect("writes");
    assert_volume_reads(&at("moved.txt"), b"Original payload");
    assert_volume_reads(&at("target.txt"), b"replacement");

    open_as(0, &at("victim.txt"), create).expect("a victim");
    write_as(0, &at("victim.txt"), 0, b"victim payload").expect("fills it");
    rename("moved.txt", "away.txt");
    FS_SERVICE
        .symlink(0, &NoCaps, "victim.txt", root(&at("moved.txt")))
        .expect("a link planted at the name");
    first.write(0, b"P").expect("writes");
    assert_volume_reads(&at("victim.txt"), b"victim payload");
    assert_volume_reads(&at("away.txt"), b"Priginal payload");
}

/// An unlinked file of `volume` reads whole through its descriptor and keeps
/// its blocks until that descriptor, its last, closes.
fn an_unlinked_file_keeps_its_blocks_until_its_last_close(volume: &str) {
    let at = |name: &str| std::format!("/Storage/{volume}/{name}");
    // Incompressible, so the volume stores as many blocks as the file is long.
    let body: Vec<u8> = (0..(32u64 << 10) / 8)
        .flat_map(|word| {
            SipHash13::hash_bytes(HashSeed::UNKEYED, &word.to_le_bytes()).to_le_bytes()
        })
        .collect();
    open_as(0, &at("big.bin"), OpenFlags::WRITE.union(OpenFlags::CREATE)).expect("creates");
    write_as(0, &at("big.bin"), 0, &body).expect("fills it");
    let big = Descriptor::open(0, &at("big.bin"), OpenFlags::READ).expect("opens");
    let named = space(volume);
    let data = body.len() as u64 / u64::from(named.block_size);
    FS_SERVICE
        .unlink(0, &NoCaps, root(&at("big.bin")), UnlinkFlags::empty(), None)
        .expect("unlinks");
    let mut back = vec![0u8; body.len()];
    assert_eq!(big.read(0, &mut back), Ok(body.len()));
    assert_eq!(
        back, body,
        "an unlinked file still reads through its descriptor"
    );
    assert_eq!(
        big.stat().map(|stat| stat.nlink),
        Ok(0),
        "no name reaches it"
    );
    let held = space(volume).free_blocks;
    assert!(
        held < named.free_blocks + data,
        "nothing came back with the name: {held} free against {}",
        named.free_blocks
    );
    drop(big);
    let after = space(volume).free_blocks;
    assert!(
        after >= held + data,
        "the last close gave its data blocks back: {after} free against {held} + {data}"
    );
}

/// A chmod of `volume`'s files leaves their owner's open descriptors
/// working, and withdraws from a grant and another principal what a fresh
/// open would now be refused.
fn a_chmod_withdraws_what_a_fresh_open_would_be_refused(volume: &str) {
    let at = |name: &str| std::format!("/Storage/{volume}/{name}");
    let mut buf = [0u8; 32];
    let made = Descriptor::open(0, &at("ro.txt"), OpenFlags::WRITE.union(OpenFlags::CREATE))
        .expect("creates");
    FS_SERVICE
        .set_mode(0, &NoCaps, root(&at("ro.txt")), 0o444, FinalLink::Follow)
        .expect("its owner makes it read-only");
    assert_eq!(
        made.write(0, b"still mine"),
        Ok(10),
        "the creating descriptor writes a file its opener owns"
    );
    assert_eq!(
        open_as(0, &at("ro.txt"), OpenFlags::WRITE),
        Err(Errno::PermissionDenied),
        "a fresh open asks the mode"
    );
    drop(made);

    let own = Descriptor::open(0, &at("target.txt"), OpenFlags::READ).expect("its owner opens it");
    let granted = own.granted();
    let other =
        Descriptor::open(MEMBER_UID, &at("target.txt"), OpenFlags::READ).expect("another opens it");
    assert!(granted.read(0, &mut buf).is_ok() && other.read(0, &mut buf).is_ok());
    FS_SERVICE
        .set_mode(
            0,
            &NoCaps,
            root(&at("target.txt")),
            0o000,
            FinalLink::Follow,
        )
        .expect("its owner shuts it");
    assert_eq!(granted.read(0, &mut buf), Err(Errno::PermissionDenied));
    assert_eq!(other.read(0, &mut buf), Err(Errno::PermissionDenied));
    let n = own
        .read(0, &mut buf)
        .expect("the owner's own descriptor keeps reading");
    assert_eq!(&buf[..n], b"replacement");
}

/// A lock is filed under its file's pin, which a write that grows a FAT file
/// across clusters and a rename that moves its entry leave alone: an opener
/// of the renamed file meets the lock the first opener took.
fn a_fat_files_lock_follows_it_through_writes_and_renames_scenario(
    window_ptr: *mut u8,
    facility: &'static TestFacility,
) {
    let (image, identity) = fat32_image(0x0D15_C075, b"lock payload");
    let round = insert(
        window_ptr,
        facility,
        &Insertion {
            endpoint_id: 0xB1D0_5E17_0000_0075,
            owner: 0x70_1075,
            region_task: 0x70_0075,
            name: b"lockfat",
            fstype: VolumeFsType::Fat32,
        },
        ServedDevice::new(image),
    );
    round.outcome.expect("the volume attaches");
    let rw = OpenFlags::READ.union(OpenFlags::WRITE);
    let first = Descriptor::open(0, "/Storage/lockfat/hello.txt", rw).expect("opens");
    let owner = filelock::mint_owner();
    filelock::acquire(&Request {
        file: first.key(),
        owner,
        pid: ProcessId(0x70_0075),
        task: 0x70_0075,
        range: LockRange::WHOLE,
        held: filelock::Held::Exclusive,
        limit: u64::MAX,
        waiting: false,
    })
    .expect("the first opener locks the file");
    first
        .write(0, &[0x4Cu8; 8192])
        .expect("a write grows the file across clusters");
    FS_SERVICE
        .rename(
            0,
            &NoCaps,
            root("/Storage/lockfat/hello.txt"),
            root("/Storage/lockfat/locked.txt"),
            RenameFlags::empty(),
            None,
        )
        .expect("renames");
    let second = Descriptor::open(0, "/Storage/lockfat/locked.txt", OpenFlags::READ)
        .expect("opens the renamed file");
    assert_eq!(second.key(), first.key(), "both opens reach one node");
    let conflict = filelock::query(
        second.key(),
        filelock::mint_owner(),
        LockRange::WHOLE,
        filelock::Held::Shared,
    );
    assert_eq!(
        conflict.map(|held| held.owner),
        Some(owner),
        "the first opener's lock stands in the way"
    );
    let _ = filelock::release_owner(owner);
    drop((first, second));
    retire(identity, false, round);
}

#[test]
fn an_evidence_window_anywhere_in_an_extent_is_read_as_its_whole_blocks() {
    let request = VolumeAttachRequest {
        endpoint: 1,
        window: 1,
        first_lba: 100,
        blocks: 1000,
        fstype: VolumeFsType::Udf,
        name: b"x",
    };
    let evidence = tairix_fsprobe::Evidence {
        offset: 64 * 2048 + 10,
        len: 4096,
    };
    // 512-byte blocks: from the block the window opens in to the one it
    // closes in, in the device's own block numbers.
    assert_eq!(
        crate::volume_service::evidence_blocks(&request, 512, evidence),
        Some((100 + 256, 9 * 512))
    );
    let past = tairix_fsprobe::Evidence {
        offset: 999 * 512,
        len: 1024,
    };
    assert_eq!(
        crate::volume_service::evidence_blocks(&request, 512, past),
        None
    );
}
