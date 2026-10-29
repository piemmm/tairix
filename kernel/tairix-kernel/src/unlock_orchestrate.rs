//! Architecture-neutral root-unlock / driver-store orchestration
//! (`plans/PI.md` design B).
//!
//! Every Tier-1 port that brings a bootstrap-floor root block device up
//! feeds it into the *same* two-task tail: the one brought-up disk is shared
//! for the life of the system by two independent preemptive tasks — a spawned
//! interactive root unlock, and the persistent driver-store serve
//! loop the user-space `devmgr` reads signed `/System` drivers through. That
//! tail is device- and architecture-independent, so it lives here rather than
//! being copied into each `kernel/arch/<target>/` sibling: a port supplies
//! only its bring-up, its console-0 seam ([`UnlockConsole`]), and its
//! process-spawn producer, and calls [`finish_unlock`].
//!
//! The kthread's admission ([`spawn_unlock`]) and the signed admission of the
//! floor driver the boot bound are shared too: a port implements
//! [`UnlockPort`] — its console, its serial log, and the bring-up of each
//! kind of floor device its silicon carries — and nothing else.
//!
//! The two hardware-specific seams a port injects:
//!
//! * [`UnlockConsole`] — the primary-console (index 0) write + raw-read pair
//!   the passphrase prompt uses, plus the release-to-`login` handoff the
//!   moment the unlock resolves. A framebuffer-vs-UART decision, an
//!   interrupt-driven receive arm, and the console-0 ownership gate are all
//!   the port's business; the orchestration only asks for a console and to
//!   release it.
//! * [`tairix_kernel_core::ArchImageBuilder`] — the architecture's process
//!   image producer, bridged through the arch-neutral
//!   [`crate::driver_spawn_loader::InitCtxDriverProcessSpawn`] so a matched
//!   user-space driver is spawned into its own hardware-isolated process
//!   granted exactly its matched node's resources (no ambient authority).

use core::convert::Infallible;

use tairix_abi::blkio::kernel_block_device;
use tairix_abi::driver::block::Block;
use tairix_abi::HwNode;
use tairix_caps::CapabilitySet;
use tairix_crypto::Ed25519PublicKey;
use tairix_drv_fs_arxfs::{Keying, ARXFS};
use tairix_drv_storage_virtio_blk::VirtioBlk;
use tairix_kernel_core::waitq::deadline_for;
use tairix_kernel_core::{
    park_on_until, wait_now_ns, ConsoleRead, ConsoleWrite, CooperativeYield, DriverLock,
    InitSpawnCtx, NotParkable, SecretFeedback, YieldHandle, HW_TREE_WAITQ,
};
use tairix_kernel_mem::FrameAllocator;
use tairix_kernel_sec::captable::TaskCapabilities;
use tairix_kernel_sec::identity::UserId;
use tairix_log::{log, Event, EventId, Field, FieldValue, Level, Sink};
use tairix_partition::{
    grow_root_to_medium, parse_partition_table, Partition, PartitionBlock, PartitionType,
};
use tairix_reclaim::MemoryPressure;
use tairix_virtio::{Transport, VirtioHost};

use tairix_kernel_core::fs::blkmeter::MeteredBlock;

use crate::block_cache::BlockCache;
use crate::driver_catalog::{FloorBringUp, HostFloor, InKernelDriver, KERNEL_DRIVER_SIGNER_PUBKEY};
use crate::driver_loader::KernelDriverLoader;
use crate::driver_spawn_loader::InitCtxDriverProcessSpawn;
use crate::root_mount::{
    unlock_root_disk_interactively, with_system_volume, AdminInstall, KeyArrival, SupervisorWindow,
    UnlockInstall, UnlockOutcome, WritableRootSink, KEY_SOURCE_WAIT_NS, LATE_GROUPS_DB,
    LATE_IDENTITY, LATE_USERS_ADMIN, LATE_USERS_DB,
};
use crate::root_storage::RootBlockBinding;
use crate::shared_block::{DriverStoreService, SharedBlock};
use crate::system_mount::{
    install_system_mount, register_writable_state, KernelFs, ROOT_VOLUME_HANDLE, SYSTEM_WRITE_SET,
};
use crate::transform_cache::{SharedClusterCache, TransformClusterCache};
use crate::unlock_service::{
    autoload_caps, loader_caps, note, service_caps, set_store_service_task,
    store_endpoint_binder_caps, take_boot, KthreadConsoleRead, UNLOCK_SERVICE, UNLOCK_TASK,
    USERS_DB_INSTALLED_MESSAGE,
};

/// The primary console a port lends the unlock.
pub struct Console0 {
    /// Where the prompt and the window are drawn.
    pub write: &'static dyn ConsoleWrite,
    /// The raw, interrupt-fed read half the prompt reads.
    pub read: &'static (dyn ConsoleRead + Sync + 'static),
    /// Where its keys come from.
    pub keys: KeySource,
}

/// Where console 0's keys come from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeySource {
    /// A line live from boot, such as a UART: a key can arrive now.
    Live,
    /// An input device's driver, so no key arrives before one binds.
    InputDevice,
}

/// The architecture console-0 seam the root-unlock orchestration reaches the
/// hardware console through.
///
/// A port decides *which* device is the primary console (a framebuffer text
/// console, the discovered UART, an SBI console) and *how* to arm its
/// interrupt-driven receive and its console-0 ownership gate; the
/// orchestration only asks for the write + raw-read pair for the passphrase
/// prompt and to release the console to `login` once the unlock resolves.
///
/// `Sync` so a `'static` seam reference can be captured by the `Send` unlock
/// kthread body (a spawned, admit-then-load service).
pub trait UnlockConsole: Sync {
    /// Acquire the primary console (index 0) for the passphrase prompt,
    /// arming any interrupt-driven receive the port needs so a keystroke
    /// wakes the parked reader rather than being missed.
    ///
    /// Its halves are `'static` (the console devices are boot statics), so
    /// the unlock kthread body can hold them for the life of the prompt.
    fn acquire_console0(&self) -> Console0;

    /// Release console 0 to `login` the instant the unlock resolves — open
    /// the console-0 ownership gate, arm the receive interrupt for a `login`
    /// reader where the port needs it, and resolve the late users-database
    /// pending wait. Called exactly once on every unlock return path, so a
    /// successful unlock can never leave the console latched shut.
    fn release_console0_to_login(&self);
}

/// What every floor bring-up runs with: the unlock kthread's capability
/// context, the frames the device's DMA is carved from, and the firmware's
/// device-tree pointer (`0` where none was handed over).
#[derive(Clone, Copy)]
pub struct FloorEnv {
    /// The unlock kthread's capability context, which every map and carve
    /// is checked against.
    pub caller: &'static TaskCapabilities,
    /// The frames the device's DMA is carved from.
    pub frames: &'static FrameAllocator,
    /// The firmware's device-tree pointer.
    pub dtb: u64,
}

/// What a port supplies the shared root unlock: its console, the log its
/// decisions fall back to, and the bring-up of each kind of floor device its
/// silicon carries.
pub trait UnlockPort: UnlockConsole + 'static {
    /// The serial log the unlock's decisions go to where the init seam wired
    /// no audit sink.
    fn serial_audit(&self) -> &'static (dyn Sink + Sync);

    /// Bring up the virtio block device `node` names, on the transport the
    /// port provisions, and run [`finish_virtio_unlock`] over it; on success
    /// this never returns.
    ///
    /// # Errors
    ///
    /// The step that failed.
    fn virtio_unlock(
        &'static self,
        coop: &CooperativeYield<'_>,
        node: &HwNode,
        floor: FloorEnv,
        env: UnlockEnv,
    ) -> Result<Infallible, &'static str>;

    /// Bring up the device `node` names, which `driver` opens over a host
    /// the kernel lends it, and run [`finish_unlock`] over it; on success
    /// this never returns. A port whose silicon carries no such device
    /// refuses.
    ///
    /// # Errors
    ///
    /// The step that failed.
    fn host_unlock(
        &'static self,
        _coop: &CooperativeYield<'_>,
        _node: &HwNode,
        _driver: &HostFloor,
        _floor: FloorEnv,
        _env: UnlockEnv,
    ) -> Result<Infallible, &'static str> {
        Err("root-unlock: this port opens no host-attached floor driver")
    }
}

/// Admit the in-kernel root-unlock kthread for the floor device the boot
/// bound, returning whether it was started.
///
/// With no binding (headless, no disk, or an ambiguous set) or no `'static`
/// frame allocator it starts nothing, releases console 0 to `login` — which
/// then refuses every attempt, as no database is installed — and returns
/// `false`. The kthread releases console 0 itself once the unlock resolves,
/// so it is never left latched shut.
#[must_use]
pub fn spawn_unlock<P: UnlockPort>(
    ctx: &'static (dyn InitSpawnCtx + Sync),
    port: &'static P,
) -> bool {
    let boot = take_boot();
    let audit: &'static (dyn Sink + Sync) =
        ctx.static_audit().unwrap_or_else(|| port.serial_audit());
    // Nothing will publish `/System`, so the application store resolves now:
    // a store-bundle spawn fails closed rather than parking forever.
    let give_up = || {
        port.release_console0_to_login();
        crate::app_store::APP_STORE.note_unavailable();
    };
    let Some(binding) = boot.binding else {
        note(
            audit,
            Level::Info,
            "root-unlock: no root block device bound; root unbound, login refuses",
        );
        give_up();
        return false;
    };
    let Some(frames) = ctx.static_frames() else {
        note(
            audit,
            Level::Error,
            "root-unlock: no kernel frame allocator; root unbound, login refuses",
        );
        give_up();
        return false;
    };
    let caps = service_caps();
    // Every mounted volume's cache samples this one gauge over the spawn
    // path's frame allocator, whose free frames are the authoritative reading.
    let env = UnlockEnv {
        ctx,
        audit,
        pressure: tairix_kernel_core::memstats::MEM_STATS.system_pressure(frames),
    };
    let dtb = boot.dtb;
    let body = move |yielder: &mut dyn YieldHandle| {
        // A live unlock parks for life as the driver-store service, so only
        // a failed bring-up gets here.
        let Err(stage) = run_unlock(port, yielder, &binding, dtb, frames, caps, env);
        note(audit, Level::Error, stage);
        port.release_console0_to_login();
        crate::app_store::APP_STORE.note_unavailable();
    };
    let admitted = ctx.spawn_kernel_service(alloc::boxed::Box::new(body));
    let Some(task_id) = admitted else {
        note(
            audit,
            Level::Error,
            "root-unlock service kthread could not be admitted; opening console gate",
        );
        give_up();
        return false;
    };
    // The serve loop registers on `SERVE_WAITQ` under this id, so a posted
    // request unparks it: a real wake, never a busy-yield.
    set_store_service_task(task_id);
    note_field(
        audit,
        (
            UNLOCK_SERVICE,
            "root-unlock service kthread admitted (bring-up runs on first dispatch)",
        ),
        "task",
        Some(task_id),
    );
    true
}

/// Admit the bound floor driver at the signed load gate, then bring its
/// device up through `port` and run the shared tail over it; on success this
/// never returns.
fn run_unlock<P: UnlockPort>(
    port: &'static P,
    yielder: &mut dyn YieldHandle,
    binding: &RootBlockBinding,
    dtb: u64,
    frames: &'static FrameAllocator,
    caps: CapabilitySet,
    env: UnlockEnv,
) -> Result<Infallible, &'static str> {
    // The kthread's one yield handle, shared by the re-arming IRQ waiter and
    // the console reader.
    let coop = CooperativeYield::new(yielder);
    // Leaked because the brought-up device's host borrows it for the life of
    // the shared disk.
    let caller: &'static TaskCapabilities = alloc::boxed::Box::leak(alloc::boxed::Box::new(
        TaskCapabilities::derive(UNLOCK_TASK, UserId(0), caps, caps, env.audit),
    ));
    admit_floor(binding.driver, env.audit)?;
    let floor = FloorEnv {
        caller,
        frames,
        dtb,
    };
    match &binding.driver.bring_up {
        FloorBringUp::Virtio => port.virtio_unlock(&coop, &binding.node, floor, env),
        FloorBringUp::Host(driver) => port.host_unlock(&coop, &binding.node, driver, floor, env),
    }
}

/// Admit `driver` at the signed load gate before anything of its device is
/// touched.
///
/// # Errors
///
/// The build's trust anchor is corrupt, or the gate refused the driver.
fn admit_floor(driver: &InKernelDriver, audit: &(dyn Sink + Sync)) -> Result<(), &'static str> {
    let loader = KernelDriverLoader::new(audit).ok_or("root-unlock: driver trust anchor")?;
    loader
        .admit(driver.path, &loader_caps())
        .map(|_| ())
        .map_err(|_| "root-unlock: the floor driver was refused at the signed load gate")
}

/// The `'static` boot environment a root-unlock bring-up threads through: the
/// init-spawn context (the per-arch driver-spawn seam) and the audit sink.
/// The matched-node grants a driver load mints are resolved from the live
/// [`crate::hwtree_store::HW_TREE`] inventory directly, so no boot-tree
/// snapshot rides along here.
///
/// Grouped because both travel together from the kthread body through the
/// per-device bring-up into the shared [`finish_unlock`] tail; passing one
/// cohesive `Copy` value rather than re-listing two `'static` references in
/// every signature keeps the seams readable and below the argument-count bar.
#[derive(Clone, Copy)]
pub struct UnlockEnv {
    /// The boot-time init-spawn context owning the live kernel registries the
    /// driver spawn and the unlock kthread are admitted through.
    pub ctx: &'static (dyn InitSpawnCtx + Sync),
    /// The `'static` boot audit sink every security-relevant decision (mount,
    /// install, give-up, autoload) is logged through.
    pub audit: &'static (dyn Sink + Sync),
    /// The system memory-pressure gauge the mounted volumes' caches sample
    /// (`plans/SMARTRAM.md` SMART2), built over the kernel frame allocator
    /// before the unlock kthread spawns.
    pub pressure: &'static MemoryPressure,
}

/// The identity the bootstrap-floor disk's readings are reported under.
///
/// The floor brings up exactly **one** block device, and it is driven
/// in-kernel with no serving block-service endpoint, so its identity comes
/// from the reserved kernel-driven device space rather than from an endpoint
/// id. Index zero is that one disk; the space cannot collide with an endpoint
/// id, so a consumer grouping volumes by device can never fold this disk
/// together with a served one.
const BOOT_DISK_DEV: u64 = kernel_block_device(0);

/// Audit event: the root grew to fill its medium — its partition (`what` is
/// `partition`, `blocks` its new length) or the volume on it (`what` is
/// `volume`, `blocks` what it gained).
pub const ROOT_GROWN: EventId = EventId(4188);

/// Audit event: growing the root to its medium was refused at `what`; it
/// mounts at the size it had.
pub const ROOT_GROWTH_REFUSED: EventId = EventId(4189);

/// Audit event: the root prefetch was admitted as `task`.
pub const ROOT_PREFETCH_ADMITTED: EventId = EventId(4182);

/// Audit event: the root prefetch finished, `elapsed_ms` after it began.
pub const ROOT_PREFETCH_FINISHED: EventId = EventId(4183);

/// The live [`WritableRootSink`]: on a successful unlock it opens a second,
/// independent `'static` read-write [`ARXFS`] window onto the `ARXFSRoot`
/// partition under the just-derived key and registers it as the **writable
/// root volume** backing — `/` itself, `/Users` with it, and the writable
/// sub-mounts `/System/Logs` and `/System/Settings`, which all resolve to
/// this one volume
/// ([`crate::system_mount::register_writable_state`]).
///
/// This is the only path that can mount the writable state: the root is the
/// one writable partition, so the keying it was mounted with — an encrypted
/// root's key is live only at the moment of a successful unlock — is
/// required, and until it lands every write to `/` and its subtrees fails
/// closed. The read window the unlock used for
/// `/System/Security` is already dropped, so this read-write view is the sole
/// writer of the volume. Fail-soft and audited: any partition/window/mount
/// refusal leaves the writable tree failing closed and never disturbs the
/// users/identity install.
struct WritableStateSink<B: Block + 'static> {
    store: &'static DriverStoreService<B>,
    audit: &'static (dyn Sink + Sync),
    /// The system memory-pressure gauge the writable root volume's cache
    /// samples, threaded from [`UnlockEnv`].
    pressure: &'static MemoryPressure,
    /// The directory-watch registry the writable root volume claims its
    /// table from.
    watches: Option<&'static tairix_kernel_core::fswatch::WatchRegistry>,
}

/// The root partition `extent` on the disk `store` serves, grown to the end
/// of the medium when it is short of it; as it was when there is nothing to
/// grow, or growing is refused.
fn grown_root<B: Block>(
    store: &DriverStoreService<B>,
    audit: &dyn Sink,
    extent: Partition,
) -> Partition {
    match grow_root_to_medium(&mut store.window()) {
        Ok(Some(grown)) => {
            note_growth(audit, ROOT_GROWN, "partition", grown.block_count);
            grown
        }
        Ok(None) => extent,
        Err(_) => {
            note_growth(audit, ROOT_GROWTH_REFUSED, "partition", 0);
            extent
        }
    }
}

/// Grow the root volume `fs` into whatever its partition gained.
fn grow_volume<B: Block>(fs: &mut ARXFS<B>, audit: &dyn Sink) {
    match fs.grow() {
        Ok(0) => {}
        Ok(added) => note_growth(audit, ROOT_GROWN, "volume", added),
        Err(_) => note_growth(audit, ROOT_GROWTH_REFUSED, "volume", 0),
    }
}

fn note_growth(audit: &dyn Sink, id: EventId, what: &'static str, blocks: u64) {
    let (level, message) = if id == ROOT_GROWN {
        (Level::Info, "root-unlock: root grown to its medium")
    } else {
        (
            Level::Warn,
            "root-unlock: root growth refused; mounting at its size",
        )
    };
    log(
        audit,
        &Event {
            level,
            id,
            message,
            fields: &[
                Field {
                    key: "what",
                    value: FieldValue::Str(what),
                },
                Field {
                    key: "blocks",
                    value: FieldValue::UnsignedInt(blocks),
                },
            ],
        },
    );
}

impl<B: Block + 'static> WritableRootSink for WritableStateSink<B> {
    fn publish(
        &self,
        keying: Keying<'_>,
    ) -> Option<alloc::sync::Arc<DriverLock<alloc::boxed::Box<dyn KernelFs>>>> {
        // Locate the ARXFSRoot extent on a throwaway probe window, then open
        // the durable owned `'static` window onto it.
        let extent = {
            let mut probe = self.store.window();
            let Ok(table) = parse_partition_table(&mut probe) else {
                note(
                    self.audit,
                    Level::Error,
                    "root-unlock: writable-state partition table invalid",
                );
                return None;
            };
            let Some(extent) = table.first_of_type(PartitionType::ARXFSRoot) else {
                note(
                    self.audit,
                    Level::Error,
                    "root-unlock: writable-state no root partition",
                );
                return None;
            };
            extent
        };
        // An image written to a larger medium leaves its root, the last
        // partition, short of the medium's end: it takes the rest of it.
        let extent = grown_root(self.store, self.audit, extent);
        let Ok(window) = PartitionBlock::from_partition(self.store.window(), &extent) else {
            note(
                self.audit,
                Level::Error,
                "root-unlock: writable-state window out of range",
            );
            return None;
        };
        // Re-open the same volume read-write as the mount found it; an
        // encrypted root's driver retains its master key for the life of the
        // mount, exactly as the read mount does. Compressed clusters are
        // served through the SMART3 transform cache, charged to the root
        // volume's mount identity and governed by the shared pressure gauge.
        let Ok(mut fs) = ARXFS::open(window, keying) else {
            note(
                self.audit,
                Level::Error,
                "root-unlock: writable-state mount failed",
            );
            return None;
        };
        grow_volume(&mut fs, self.audit);
        let fs = fs.with_cluster_cache(alloc::boxed::Box::new(TransformClusterCache::for_volume(
            ROOT_VOLUME_HANDLE,
            self.pressure,
            self.audit,
        )));
        // The root backs the system's writable grafts, so it holds the whole
        // system reserve: the system keeps writing once users fill it.
        let Ok(fs) = fs.with_system_reserve(&SYSTEM_WRITE_SET) else {
            note(
                self.audit,
                Level::Error,
                "root-unlock: writable-state reserve unavailable",
            );
            return None;
        };
        // Bound the volume's pinned write-back set to this machine's RAM and
        // publish its unwritten bytes, so a writer that outruns the device is
        // throttled by forced commits rather than by growth. A refusal means
        // the machine cannot spare one device transfer per volume, and the
        // writable tree stays failing closed rather than mounting unbounded.
        let Ok(fs) =
            crate::writeback_bound::bound_volume(fs, ROOT_VOLUME_HANDLE, self.pressure, self.audit)
        else {
            note(
                self.audit,
                Level::Error,
                "root-unlock: writable-state write-back cache unbounded",
            );
            return None;
        };
        // The registered driver is the volume's single writer; the
        // `CAP_USER_ADMIN` account-administration engine shares this same
        // lock (`plans/CAPABILITY_USE.md` CU4) — `/System/Security` is
        // shadowed by the read-only `/System` mount, so the engine persists
        // through this driver directly. Fail-soft: a registration refusal
        // leaves the writable tree and `users_admin` failing closed.
        let volume_uuid = fs.volume_uuid();
        let driver: alloc::boxed::Box<dyn KernelFs> = alloc::boxed::Box::new(fs);
        register_writable_state(
            driver,
            volume_uuid,
            self.store.io_source(),
            self.audit,
            self.pressure,
            self.watches,
        )
    }
}

/// Console 0's key source as the Supervisor window waits for it: a live line
/// at once, an input device once its driver in the live hardware tree says
/// its keys reach the seat.
struct HwTreeKeyArrival {
    source: KeySource,
    audit: &'static (dyn Sink + Sync),
}

impl KeyArrival for HwTreeKeyArrival {
    fn key_source_ready(&self) -> bool {
        self.source == KeySource::Live || keyboard_present()
    }

    fn await_key_source(&self) -> bool {
        if self.key_source_ready() {
            return true;
        }
        note(
            self.audit,
            Level::Info,
            "root-unlock: waiting for a keyboard before the supervisor window",
        );
        let present = wait_now_ns().is_some_and(|now| {
            park_on_until(
                &HW_TREE_WAITQ,
                deadline_for(now, KEY_SOURCE_WAIT_NS),
                keyboard_present,
            )
            .unwrap_or_else(|NotParkable| keyboard_present())
        });
        if !present {
            note(
                self.audit,
                Level::Warn,
                "root-unlock: no keyboard in time; no supervisor window",
            );
        }
        present
    }
}

/// Whether a node in the live hardware tree delivers its keys to the seat.
fn keyboard_present() -> bool {
    holds_key_source(&crate::hwtree_store::HW_TREE)
}

fn holds_key_source(tree: &crate::hwtree_store::HwTreeStore) -> bool {
    let mut present = false;
    tree.for_each_node(&mut |node| present |= node.is_key_source());
    present
}

/// Open the whole virtio disk over `transport` and `host` and run
/// [`finish_unlock`] over it: the tail every port's virtio bring-up ends in,
/// once its driver was admitted.
///
/// On success this never returns.
///
/// # Errors
///
/// The step that failed.
pub fn finish_virtio_unlock<T: Transport + 'static>(
    transport: T,
    host: &'static dyn VirtioHost,
    coop: &CooperativeYield<'_>,
    env: UnlockEnv,
    console: &'static dyn UnlockConsole,
) -> Result<Infallible, &'static str> {
    let blk = VirtioBlk::open(transport, host).map_err(|_| "root-unlock: virtio-blk open")?;
    finish_unlock(blk, coop, env, console)
}

/// The shared two-task tail both floor block bring-ups feed, turning the one
/// brought-up disk into a disk shared for life by two independent preemptive
/// tasks (Design D D2b-2c).
///
/// `blk` is the brought-up whole-disk [`Block`] device (virtio-blk or an SD host),
/// already boot-leaked to `'static` by its bring-up. It is wrapped in a leaked
/// `&'static` [`DriverStoreService`] (over the [`SharedBlock`] layer), so two
/// tasks reach it through independent serialised windows:
///
/// * A **separate, spawned** preemptive task runs the interactive
///   root unlock against the *user-data* volume and, when it
///   resolves (installed or fail-closed), releases the console to `login`
///   through [`UnlockConsole::release_console0_to_login`].
/// * **This** task becomes the persistent driver-store serve loop: it binds
///   and serves the capability-gated store IPC endpoint the user-space
///   `devmgr` loads signed `/System` drivers through, real-parking on
///   `SERVE_WAITQ` between requests, and never returns on success.
///
/// Crucially the store endpoint binds **independently of** the user-data
/// passphrase (the signed driver store lives on the always-readable `/System`
/// volume, `plans/PI.md` design B), so the keyboard driver loads in user space
/// *before* the unlock prompt — no chicken-and-egg, and no cooperative
/// interleaving of the two on one kthread.
///
/// `console` is the port's console-0 seam — the one hardware-specific input
/// the otherwise arch-neutral tail needs. The architecture image builder a
/// driver spawn drives is the boot-installed one (`SpawnServices`), so it is
/// no longer threaded here (`plans/FIX-DESKTOP.md` §2.6.5).
///
/// On success this never returns. Every fallible *setup* step fails closed
/// with a stable stage string the caller logs.
pub fn finish_unlock<B: Block + 'static>(
    blk: B,
    coop: &CooperativeYield<'_>,
    env: UnlockEnv,
    console: &'static dyn UnlockConsole,
) -> Result<Infallible, &'static str> {
    let UnlockEnv {
        ctx,
        audit,
        pressure,
    } = env;

    // The write-back flusher, before any volume can be registered: a
    // filesystem may only batch commits while the task that publishes an
    // aged-out transaction is live, so it is admitted first and arms deferral
    // itself once it can park. This is the one place every port with a
    // storage floor passes through.
    crate::writeback_service::start(ctx, audit);

    // The boot disk has no serving block-service endpoint to fold its
    // counters at, so the fold is wrapped directly around the device here,
    // *under* the cache: a cache hit never reaches the medium, and counting
    // one as device work would report throughput and utilisation the disk
    // never did. The readings travel with the store below so both boot-floor
    // mounts publish them.
    let blk = MeteredBlock::new(blk, BOOT_DISK_DEV, audit);
    let io = blk.io_source();
    // The one brought-up disk, boot-leaked to `'static` behind the
    // block-sharing layer so two independent preemptive tasks drive it through
    // their own serialised windows: *this* task is the driver-store serve loop
    // (below), and a *separate* spawned task runs the root unlock. A
    // geometry fault refuses the device fail-closed. The device sits behind
    // the SMART11 block cache (`plans/SMARTRAM.md`), on the device side of the
    // sharing lock, so every window reads through one coherent,
    // pressure-governed cache of recently used blocks.
    let blk = BlockCache::for_disk(blk, crate::block_cache::BOOT_DISK_OWNER, pressure, audit)
        .map_err(|_| "root-unlock: block device geometry")?;
    // A `None` means classification refused the cache at birth (it is then
    // poisoned and admits nothing), so there is nothing to register — the
    // refusal is already in the audit log.
    if let Some(ledger) = blk.ledger() {
        tairix_kernel_core::memstats::MEM_STATS.register_ledger(ledger);
    }
    let store: &'static DriverStoreService<BlockCache<MeteredBlock<B>>> =
        alloc::boxed::Box::leak(alloc::boxed::Box::new(DriverStoreService::new(
            SharedBlock::new(blk).map_err(|_| "root-unlock: block device geometry")?,
            io,
        )));

    // Spawn the root unlock as its own preemptive task. The user-data
    // volume's protection is independent of the always-readable
    // `/System` driver store (`plans/PI.md` design B), so the store endpoint
    // (bound + served by *this* task below) answers `devmgr` immediately — the
    // keyboard driver loads in user space *before* the prompt, with no
    // cooperative interleaving on one kthread (two independent tasks sharing
    // the disk). The unlock task drives its own console reader over its own
    // scheduler yield handle.
    let unlock_body = move |yielder: &mut dyn YieldHandle| {
        let coop = CooperativeYield::new(yielder);
        // The primary console (index 0): the port's console-0 seam decides
        // the device (framebuffer text console, discovered UART, …) and arms
        // its interrupt-driven receive so a keystroke wakes the parked reader
        // (nothing cooperative). The read half is the raw device behind the
        // console-0 gate `login` reads through (the gate stays closed until
        // this unlock resolves), so the two never contend.
        let console0 = console.acquire_console0();
        let console_write = console0.write;
        // The passphrase prompt's secret-entry feedback: the same
        // `[input active...]` marker a user-space password read shows, drawn
        // to this console's own output. It is armed only for the span of an
        // actual passphrase read (`read_passphrase_line` brackets its read
        // through the reader's `set_secret` seam) and stays disarmed for the
        // ESC boot-screen window and the echoed Supervisor REPL that share
        // this reader, so the marker never paints over the REPL's own echo.
        let secret = SecretFeedback::new(console_write);
        // The kthread's own scheduler id (published at admission), so the
        // reader registers on `CONSOLE_WAITQ` and the RX interrupt unparks it
        // by id.
        let reader = KthreadConsoleRead::new(
            console0.read,
            &coop,
            crate::unlock_service::unlock_console_task(),
            Some(&secret),
        );
        // Publish the writable root volume backing (`/` and its writable
        // `/System` subtrees) on a successful unlock, from a second `'static`
        // read-write window onto the same `'static`-leaked disk (park-safe via
        // the device `SleepLock`), under the just-derived key.
        let writable = WritableStateSink {
            store,
            audit,
            pressure,
            watches: ctx.watches(),
        };
        // The unlock owns console 0 for the passphrase prompt (its
        // `GatedConsoleRead` keeps `login` parked). The moment it resolves — a
        // database installed *or* given up — console 0 must be released to
        // `login`: the port's seam opens the gate, arms the receive interrupt
        // where a `login` reader needs it, and resolves the `LateUsersDb`
        // pending wait. `unlock_root_disk_interactively` calls this
        // `on_resolved` callback exactly once on every internal return path,
        // so a successful unlock can no longer leave the console latched shut.
        let release = || console.release_console0_to_login();
        // The anti-brute-force pause after a wrong passphrase: a genuine timed
        // park (never a busy-wait) for at least three seconds, so a scripted
        // brute-force gains no faster oracle than the honest operator, before
        // the "Incorrect passphrase" notice and re-prompt.
        let retry_delay = || {
            crate::unlock_service::park_for_ns(
                &coop,
                crate::unlock_service::unlock_console_task(),
                crate::unlock_service::WRONG_PASSPHRASE_RETRY_DELAY_NS,
            );
        };
        // The set-once publish destinations a successful unlock fills. Bound
        // by name so the pre-unlock Supervisor's `mount` command publishes
        // through the *same* cells the interactive prompt would — one install,
        // no second definition.
        let install = UnlockInstall {
            users: &LATE_USERS_DB,
            identity: &LATE_IDENTITY,
            groups: &LATE_GROUPS_DB,
            writable: &writable,
            admin: Some(AdminInstall {
                cell: &LATE_USERS_ADMIN,
                users: &LATE_USERS_DB,
                identity: &LATE_IDENTITY,
                groups: &LATE_GROUPS_DB,
                audit,
            }),
            storage_gid: &crate::volume_policy::LATE_STORAGE_GID,
        };
        // The pre-boot Supervisor console the ESC boot-screen window drops
        // into: it presents the already-computed bootstrap-floor state and,
        // on `mount`, runs the real unlock through `install`. It reads the
        // shared boot disk through its own windows, so it never contends with
        // the passphrase prompt's window.
        let mut supervisor =
            crate::supervisor_host::KernelSupervisorHost::new(store, &install, audit, &retry_delay);
        let keys = HwTreeKeyArrival {
            source: console0.keys,
            audit,
        };
        match unlock_root_disk_interactively(
            store.window(),
            console_write,
            &reader,
            &install,
            audit,
            &retry_delay,
            &release,
            Some(SupervisorWindow {
                host: &mut supervisor,
                keys: &keys,
            }),
        ) {
            UnlockOutcome::Installed => note(audit, Level::Info, USERS_DB_INSTALLED_MESSAGE),
            UnlockOutcome::GaveUp => note(
                audit,
                Level::Error,
                "root-unlock: gave up fail-closed; login refused until reboot",
            ),
        }
        // The unlock task then ends (the disk stays alive — it is
        // `'static`-leaked — and this task's window borrow ends with it).
    };
    spawn_root_prefetch(ctx, store, audit);
    if let Some(unlock_task) = ctx.spawn_kernel_service(alloc::boxed::Box::new(unlock_body)) {
        // Publish the interactive unlock kthread's scheduler id so its
        // passphrase reader can register on `CONSOLE_WAITQ` and the console RX
        // interrupt can unpark it by id. On a multi-core boot the spawned body
        // can start on another CPU before this store lands; the reader
        // tolerates that by re-resolving the id on every poll
        // (`KthreadConsoleRead::read`), so a pre-publish start degrades to at
        // most one transient cooperative yield, never a lost wake.
        crate::unlock_service::set_unlock_console_task(unlock_task);
    } else {
        // The unlock task could not be admitted: nothing will prompt for the
        // passphrase or open the console-0 gate, so open it here (login still
        // refuses, as no database is installed) and serve the store anyway so
        // `devmgr` can load drivers.
        note(
            audit,
            Level::Error,
            "root-unlock: unlock task not admitted; console gate opened, store still served",
        );
        console.release_console0_to_login();
    }

    // Publish the read-only `/System` volume as the userland `fs_*` mount
    // before entering the serve loop: a second, park-safe `'static` window
    // onto the same `'static`-leaked disk. The store
    // serve loop below keeps its own independent window, so the two never
    // conflict. Fail-soft and audited: a disk with no readable `/System`
    // volume simply leaves the `fs_*` syscalls failing closed.
    let system_cache = install_system_mount(store, audit, pressure, ctx.watches());

    // Become the persistent capability-gated driver-store serve loop the
    // user-space `devmgr` autoloads signed `/System` drivers through — bound
    // over the always-readable `/System` volume, independent of the user-data
    // passphrase (`plans/PI.md` design B). Never returns on success.
    serve_driver_store(store, coop, env, system_cache)
}

/// Take the root mount's reads off `store`'s disk while the Supervisor window
/// waits: it can outlast its 2 s by however long console 0's input device
/// takes to bind. Its admission and how long it ran are recorded
/// ([`ROOT_PREFETCH_ADMITTED`], [`ROOT_PREFETCH_FINISHED`]).
fn spawn_root_prefetch<B: Block + 'static>(
    ctx: &'static (dyn InitSpawnCtx + Sync),
    store: &'static DriverStoreService<B>,
    audit: &'static (dyn Sink + Sync),
) {
    let prefetch = move |_: &mut dyn YieldHandle| {
        let began = wait_now_ns();
        crate::root_mount::prefetch_root(store.window());
        let elapsed_ns = began
            .zip(wait_now_ns())
            .map(|(began, now)| now.saturating_sub(began));
        note_field(
            audit,
            (
                ROOT_PREFETCH_FINISHED,
                "root-unlock: root prefetch finished",
            ),
            "elapsed_ms",
            elapsed_ns.map(|ns| ns / 1_000_000),
        );
    };
    match ctx.spawn_kernel_service(alloc::boxed::Box::new(prefetch)) {
        Some(task) => note_field(
            audit,
            (
                ROOT_PREFETCH_ADMITTED,
                "root-unlock: root prefetch admitted",
            ),
            "task",
            Some(task),
        ),
        None => note(
            audit,
            Level::Warn,
            "root-unlock: root prefetch not admitted; the mount reads the disk cold",
        ),
    }
}

/// Record `event` with its one `key` field.
fn note_field(
    audit: &dyn Sink,
    (id, message): (EventId, &'static str),
    key: &'static str,
    value: Option<u64>,
) {
    log(
        audit,
        &Event {
            level: Level::Info,
            id,
            message,
            fields: &[Field {
                key,
                value: value.map_or(FieldValue::Null, FieldValue::UnsignedInt),
            }],
        },
    );
}

/// Become the persistent capability-gated driver-store serve loop the
/// user-space `devmgr` autoloads signed `/System` drivers through.
///
/// Binds and serves the store IPC endpoint over its own `/System` window onto
/// the `'static`-leaked shared disk, real-parking on `SERVE_WAITQ` between
/// requests and never returning on success. Every fallible setup step (a
/// missing trust anchor, an unbindable endpoint, no readable `/System`
/// volume) fails closed — logged, then the kthread parks for life still
/// owning the disk so an `ipc_call` to the unbound endpoint fails closed with
/// `NotFound` rather than blocking.
///
/// A matched user-space driver is spawned into its own hardware-isolated
/// process (granted exactly its matched node's resources, no ambient
/// authority) through the arch-neutral [`InitCtxDriverProcessSpawn`], which
/// admits it as a deferred-load child that builds its own image through the
/// boot-installed architecture image builder (`plans/FIX-DESKTOP.md` §2.6.5).
fn serve_driver_store<B: Block + 'static>(
    store: &'static DriverStoreService<BlockCache<B>>,
    coop: &CooperativeYield<'_>,
    env: UnlockEnv,
    system_cache: Option<SharedClusterCache>,
) -> Result<Infallible, &'static str> {
    let audit = env.audit;
    // The driver-signing trust anchor the autoload load gate verifies each
    // winning bundle against — the kernel's own embedded key, the single
    // source `KernelDriverLoader` also trusts. A corrupt key is a broken
    // build, not an admissible state: fail closed rather than autoload against
    // no anchor.
    let trust_anchor = Ed25519PublicKey::from_bytes(&KERNEL_DRIVER_SIGNER_PUBKEY)
        .map_err(|_| "root-unlock: driver trust anchor")?;
    let trusted = [trust_anchor];
    // The scheduler-agnostic driver-spawn seam over the captured boot context
    // + the port's process producer — the one per-arch input the otherwise
    // arch-neutral driver-store load op needs to spawn a verified driver into
    // its own process.
    let driver_spawn = InitCtxDriverProcessSpawn::new(env.ctx);
    // The kernel-side load mechanism the persistent driver-store service keeps
    // in its trusted base (Design D D2b-2c): the driver-signing trust anchor,
    // the delegatable `autoload_caps` gate superset (`CAP_DRV_LOAD` to pass
    // the gate plus the resource caps an autoloaded driver's class may request
    // — `CAP_INPUT_INJECT`/`CAP_IRQ_BIND` for an input driver and
    // `CAP_IPC_BIND_PRIVILEGED` for a bus service driver such as a firmware
    // mailbox, intersected per driver with its signed manifest request),
    // the port process-spawn seam, and the **live** hardware inventory
    // (`crate::hwtree_store::HW_TREE`) a matched `node_id` is resolved against
    // to mint exactly that node's grants (no ambient authority). Resolving
    // against the live store (not a frozen boot snapshot) is what lets a node
    // a user-space bus driver publishes at runtime through `hw_emit_node` be
    // loaded the moment it appears — the recursive bus chain (root complex →
    // USB host → keyboard) depends on it. The user-space `devmgr` owns the matching
    // *policy*; this kthread serves the *mechanism* over the capability-gated
    // store endpoint below.
    let serve_ctx = crate::driver_store_server::StoreServeContext {
        trusted: &trusted,
        caps: autoload_caps(),
        spawn: &driver_spawn,
        nodes: &crate::hwtree_store::HW_TREE,
    };

    // This task is now the persistent driver-store service: it binds and
    // serves the capability-gated store IPC endpoint the user-space `devmgr`
    // reads the signed `/System` driver store through, real-parking on
    // `SERVE_WAITQ` between requests. It serves over its own `/System` window
    // onto the `'static`-leaked shared disk, independently of the
    // root-unlock task spawned above (`plans/PI.md` design B).
    // `login`, PID 1, `devmgr`, the unlock task, and every other task run on
    // their own tasks.
    //
    // The binder context holds only `IPC_BIND_PRIVILEGED` (the privileged
    // authority to bind the restricted-sender store endpoint), distinct from
    // the kthread's own minimal `service_caps` (no ambient authority).
    let binder = TaskCapabilities::derive(
        UNLOCK_TASK,
        UserId(0),
        store_endpoint_binder_caps(),
        store_endpoint_binder_caps(),
        audit,
    );
    // The persistent `/System` window is taken in an inner scope so that, on a
    // fail-closed fallback, the window borrow of `store` ends before the
    // `store.hold` park. On the success path `serve_system_store` never
    // returns, so the window stays borrowed for the life of the system.
    let outcome = {
        let mut window = store.window();
        with_system_volume(&mut window, audit, system_cache, |volume| {
            crate::driver_store_server::serve_system_store(volume, &serve_ctx, &binder, coop, audit)
        })
    };
    match outcome {
        // The serve loop never returns on success (`Infallible`).
        Some(Ok(never)) => match never {},
        // The endpoint could not be bound (e.g. its well-known id is already
        // registered, or the mount became unreadable). Fail closed: log the
        // stage and park the kthread for life still owning the disk, so an
        // `ipc_call` to the unbound store endpoint fails closed with
        // `NotFound` rather than blocking.
        Some(Err(stage)) => {
            note(audit, Level::Error, stage);
            store.hold(coop)
        }
        // No read-only `/System` volume on this disk (already audited
        // `SYSTEM_VOLUME_UNAVAILABLE`): nothing to serve. Park for life owning
        // the disk; `devmgr`'s store reads fail closed with `NotFound`.
        None => {
            note(
                audit,
                Level::Error,
                "driver-store: no /System volume to serve; driver-store endpoint not bound",
            );
            store.hold(coop)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every driver the floor holds passes the signed gate, the virtio ones
    /// included now that they are admitted before their device is touched;
    /// a driver the gate holds no image for is refused.
    #[test]
    fn every_floor_driver_is_admitted_and_no_other() {
        use crate::driver_catalog::IN_KERNEL_DRIVERS;
        use crate::test_support::NullSink;

        for driver in &IN_KERNEL_DRIVERS {
            assert_eq!(admit_floor(driver, &NullSink), Ok(()), "{}", driver.path);
        }
        let known = &IN_KERNEL_DRIVERS[0];
        let stranger = InKernelDriver {
            path: "/System/Drivers/storage/stranger",
            bind_keys: known.bind_keys,
            image: known.image,
            register: known.register,
            bring_up: known.bring_up,
        };
        assert!(admit_floor(&stranger, &NullSink).is_err());
    }

    /// An image written to a medium four times its size grows its root
    /// partition to the medium's end, and the volume on it then grows into
    /// what the partition gained.
    #[test]
    fn a_root_written_to_a_larger_medium_grows_to_fill_it() {
        use tairix_abi::blkio::kernel_block_device;
        use tairix_abi::driver::filesystem::FilesystemStats;
        use tairix_kernel_core::fs::blkmeter::MeteredBlock;
        use tairix_kernel_core::test_sink::TestSink;

        use crate::shared_block::SharedBlock;
        use crate::test_support::{RamBlock, BLOCK_SIZE};

        let image = tairix_test_encrypted_root_image::build_plaintext_image().expect("image");
        let medium = (image.len() / BLOCK_SIZE) as u64 * 4;
        let mut disk = RamBlock::new(medium);
        disk.data[..image.len()].copy_from_slice(&image);
        let audit: &'static TestSink =
            alloc::boxed::Box::leak(alloc::boxed::Box::new(TestSink::new()));
        let device = MeteredBlock::new(disk, kernel_block_device(0), audit);
        let io = device.io_source();
        let store = DriverStoreService::new(SharedBlock::new(device).expect("geometry"), io);
        let extent = parse_partition_table(&mut store.window())
            .expect("table")
            .first_of_type(PartitionType::ARXFSRoot)
            .expect("root");

        let grown = grown_root(&store, audit, extent);
        assert_eq!(grown.start_lba, extent.start_lba);
        assert_eq!(
            grown.start_lba + grown.block_count,
            medium,
            "to the medium's end"
        );
        assert_eq!(
            grown_root(&store, audit, grown),
            grown,
            "and a second boot finds nothing to grow"
        );

        let window = PartitionBlock::from_partition(store.window(), &grown).expect("window");
        let mut fs = ARXFS::open(window, Keying::Plaintext).expect("the root mounts");
        grow_volume(&mut fs, audit);
        assert_eq!(fs.stats().expect("stats").total_blocks, grown.block_count);
        let ids = audit.event_ids();
        assert_eq!(
            ids.iter().filter(|&&id| id == ROOT_GROWN.0).count(),
            2,
            "the partition and the volume each grew once: {ids:?}"
        );
        assert!(!ids.contains(&ROOT_GROWTH_REFUSED.0));
    }

    /// The prefetch's records name it by its task and say how long it ran,
    /// or `null` where there was no clock to time it by.
    #[test]
    fn the_prefetch_records_its_task_and_how_long_it_ran() {
        use tairix_kernel_core::test_sink::TestSink;

        let sink = TestSink::new();
        note_field(
            &sink,
            (ROOT_PREFETCH_ADMITTED, "admitted"),
            "task",
            Some(42),
        );
        note_field(
            &sink,
            (ROOT_PREFETCH_FINISHED, "finished"),
            "elapsed_ms",
            None,
        );
        let events = sink.snapshot();
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[0].id, ROOT_PREFETCH_ADMITTED);
        assert_eq!(events[0].fields, [("task".into(), "42".into())]);
        assert_eq!(events[1].id, ROOT_PREFETCH_FINISHED);
        assert_eq!(events[1].fields, [("elapsed_ms".into(), "null".into())]);
    }

    /// Regression: the window opened once any input device was bound, so a
    /// mouse binding before the keyboard, or an interface its driver was
    /// about to decline, started the countdown while no key could arrive.
    #[test]
    fn a_bound_input_device_is_no_key_source_until_its_driver_says_so() {
        use crate::hwtree_store::HwTreeStore;
        use tairix_abi::driver::DriverEnding;
        use tairix_abi::hwtree::{HW_NODE_ROOT, HW_NODE_ROOT_ID};
        use tairix_abi::{HwDeviceClass, HwNode};
        use tairix_kernel_core::hwtree::HwDriverTree;

        let tree = HwTreeStore::new();
        tree.seed(alloc::vec![
            HwNode::new(HW_NODE_ROOT_ID, HW_NODE_ROOT, HwDeviceClass::Root),
            HwNode::new(2, HW_NODE_ROOT_ID, HwDeviceClass::Input),
            HwNode::new(3, HW_NODE_ROOT_ID, HwDeviceClass::Input),
        ])
        .expect("a fresh store seeds");
        tree.driver_admitted(2, 1, false);
        assert!(
            !holds_key_source(&tree),
            "a mouse bound first opens nothing"
        );
        tree.driver_admitted(3, 2, false);
        assert_eq!(tree.mark_key_source(3), Ok(()));
        assert!(
            holds_key_source(&tree),
            "the keyboard's keys reach the seat"
        );
        tree.driver_released(3, 2, DriverEnding::Failed);
        assert!(!holds_key_source(&tree), "an ended keyboard delivers none");
    }
}
