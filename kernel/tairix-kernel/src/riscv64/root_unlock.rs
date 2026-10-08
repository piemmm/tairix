//! The riscv64 (QEMU `virt` / SiFive) live root-unlock bring-up
//! (`plans/NETWORK.md` N4e-riscv64).
//!
//! The freestanding-riscv64 half of the in-kernel root-unlock service, the
//! cross-port sibling of [`crate::aarch64::root_unlock`]. It lives in the
//! architecture subtree because it names the riscv64 port directly (the
//! external-IRQ dispatch, the Sv39 paging primitives, the firmware device
//! tree); the device- and architecture-independent two-task tail — the
//! spawned interactive unlock and the persistent driver-store serve loop —
//! stays in the shared [`crate::unlock_orchestrate::finish_unlock`].
//!
//! It admits the in-kernel unlock kthread at the init seam, brings the
//! bootstrap virtio-blk root device up through the shared
//! [`crate::floor_mmio`] bring-up, and hands the opened disk to the shared
//! tail. The `virt` board's only floor block driver is virtio-blk (the
//! aarch64 EMMC2 SD path has no riscv64 analogue), so there is one bring-up.

use core::convert::Infallible;

use tairix_abi::{HwNode, IrqHandle};
use tairix_arch_api::fdtwalk::FdtPlatform;
use tairix_arch_riscv64::paging::{AddressSpace as ArchAddressSpace, PageTablePool};
use tairix_arch_riscv64::platform::Riscv64Fdt;
use tairix_arch_riscv64::{trap, SERIAL_SINK};
use tairix_caps::CapabilitySet;
use tairix_fdt::{Fdt, Node};
use tairix_kernel_core::{ConsoleRead, ConsoleWrite, CooperativeYield, InitSpawnCtx, YieldHandle};
use tairix_kernel_irq::IrqTable;
use tairix_kernel_mem::{FrameAllocator, PhysMap};
use tairix_kernel_sec::captable::TaskCapabilities;
use tairix_kernel_sec::identity::UserId;
use tairix_log::{Level, Sink};
use tairix_reclaim::MemoryPressure;

use crate::driver_catalog::VIRTIO_BLK_PATH;
use crate::floor_dma::Confinement;
use crate::floor_irq::LineHost;
use crate::floor_mmio::{bring_up_virtio_mmio, MmioFloorPort};
use crate::riscv64::irq::{controller as irq_controller, published_irq_table};
use crate::riscv64::spawn_producer::SPAWN_TABLE_PHYSMAP;
use crate::root_storage::RootBlockBinding;
use crate::unlock_orchestrate::{finish_virtio_unlock, UnlockConsole, UnlockEnv};
use crate::unlock_service::{note, service_caps, take_boot, CONSOLE0_GATE, UNLOCK_TASK};

/// Identity extent, in GiB, a bookkeeping space maps: enough for the kernel
/// image and low RAM, and below both window bases.
const BOOKKEEPING_GIB: usize = 4;

/// The page-table frames the floor's bookkeeping spaces take their tables
/// from, apart from the boot and init pools so the unlock never contends them.
static UNLOCK_PT_POOL: PageTablePool = PageTablePool::new();

/// The riscv64 half of the floor bring-up.
struct RiscvFloor;

// SAFETY: the boot identity-maps the tree's virtio-MMIO aperture for the life
// of the kernel, with the MMU on before `try_boot` and the physical memory
// attributes making it device memory, and nothing holds a reference into it.
unsafe impl MmioFloorPort for RiscvFloor {
    type Platform = Riscv64Fdt;
    type Space = ArchAddressSpace;
    // Above the bookkeeping identity extent and inside the Sv39 canonical
    // lower half, below 256 GiB.
    const MMIO_VBASE: u64 = 64 << 30;
    const POOL_VBASE: u64 = 128 << 30;

    fn bookkeeping_space() -> Option<ArchAddressSpace> {
        ArchAddressSpace::new_identity_gigapages(&UNLOCK_PT_POOL, BOOKKEEPING_GIB)
    }

    fn registers() -> &'static dyn PhysMap {
        &SPAWN_TABLE_PHYSMAP
    }

    fn frames() -> &'static dyn PhysMap {
        &SPAWN_TABLE_PHYSMAP
    }

    fn slot_line(fdt: &Fdt<'_>, slot: &Node<'_>) -> Option<u32> {
        Riscv64Fdt::from_tree(fdt)
            .node_line(slot)
            .map(|line| line.line)
    }

    fn lines() -> Option<LineHost> {
        Some(LineHost {
            table: published_irq_table()?,
            controller: irq_controller()?,
            park: wfi_fallback_park,
        })
    }
}

/// The park a device wait takes where the scheduler cannot park it: mask
/// S-mode interrupt taking, re-check the line's ready flag and `wfi` only if
/// it is still not ready, so a completion landing in that window stays pending
/// and wakes the `wfi` rather than being lost.
fn wfi_fallback_park(table: &IrqTable, handle: IrqHandle) {
    // SAFETY: the S-mode trap vector is installed (boot
    // `enable_mmu_and_vectors`) and the production dispatch is published
    // (`irq::install_dispatch`), so a woken external interrupt is handled
    // rather than faulting; the calls only toggle `sstatus.SIE` and issue the
    // `wfi` hint.
    unsafe {
        trap::set_supervisor_interrupts(false);
        if !table.ready_for(handle) {
            trap::wait_for_interrupt();
        }
        trap::set_supervisor_interrupts(true);
    }
}

/// Release console 0 to `login` and mark the users-database source resolved.
///
/// Both mean "the unlock window is over, `login` may take the console now", so
/// they flip together. Opening the gate lets `login`'s gated console reads
/// through; resolving the late users-database flips a `login` parked on the
/// pending `users_db_read` into its prompt — against the installed database if
/// the unlock succeeded, else fail-closed deny-all. There is no
/// receive-interrupt to arm, unlike the sibling ports: the firmware console
/// raises none, so a parked reader comes back on the backing's declared
/// re-poll interval instead.
fn release_console0_to_login() {
    CONSOLE0_GATE.open();
    // Nudge the console wait-queue so any `login` already parked on the
    // (until now) withheld console-0 read re-polls the now-open gate; a no-op
    // before the wait-queue arch hook is installed.
    tairix_kernel_core::console_wake();
    crate::root_mount::LATE_USERS_DB.resolve();
}

/// The riscv64 console-0 seam the shared root-unlock orchestration reaches the
/// primary console through.
///
/// The SBI console is the primary console: its write half streams the
/// passphrase prompt and its read half drains the legacy `console_getchar`
/// service, so an operator can answer the prompt on this port exactly as on
/// the others. The drain is non-blocking, so the shared reader parks between
/// polls rather than spinning, and a console that never delivers a byte
/// leaves the unlock waiting rather than mounting on a guess.
struct RiscvUnlockConsole;

/// The single `'static` [`RiscvUnlockConsole`] the bring-up hands the shared
/// orchestration.
static RISCV_UNLOCK_CONSOLE: RiscvUnlockConsole = RiscvUnlockConsole;

impl UnlockConsole for RiscvUnlockConsole {
    fn acquire_console0(
        &self,
    ) -> (
        &'static dyn ConsoleWrite,
        &'static (dyn ConsoleRead + Sync + 'static),
    ) {
        let write: &'static dyn ConsoleWrite = &crate::riscv64::boot::RISCV_UART_CONSOLE;
        let read: &'static (dyn ConsoleRead + Sync + 'static) =
            &crate::riscv64::boot::RISCV_UART_CONSOLE_READ;
        (write, read)
    }

    fn release_console0_to_login(&self) {
        release_console0_to_login();
    }
}

/// Admit the in-kernel root-unlock kthread if the boot path bound a virtio-blk
/// root block device, returning whether it was started.
///
/// With no binding (headless / no disk / ambiguous), a non-virtio-blk binding
/// (there is no other floor block driver on the `virt` board), or no `'static`
/// frame allocator, it starts nothing, opens the console-0 gate so `login`
/// proceeds (and fails closed, as no database is installed), and returns
/// `false`. The console-0 gate is also opened by the kthread body once the
/// unlock resolves, so it is never left latched closed.
#[must_use]
pub fn spawn_if_present(ctx: &'static (dyn InitSpawnCtx + Sync)) -> bool {
    let boot = take_boot();
    // Route the unlock service's security-relevant decisions onto the boot
    // audit channel when the init seam wired a `'static` audit sink; fall back
    // to the SBI serial log otherwise. The kthread body and the unlock policy
    // share it.
    let audit: &'static (dyn Sink + Sync) = ctx.static_audit().unwrap_or(&SERIAL_SINK);
    let Some(binding) = boot.binding else {
        note(
            audit,
            Level::Info,
            "root-unlock: no root block device bound; root unbound, login refuses",
        );
        release_console0_to_login();
        // No disk means no on-disk application store this boot: resolve the
        // readiness latch so a store-bundle spawn fails closed instead of
        // parking forever.
        crate::app_store::APP_STORE.note_unavailable();
        return false;
    };
    if binding.driver_path != VIRTIO_BLK_PATH {
        // The bound driver is not the one bootstrap-floor block driver this
        // seam knows how to bring up (virtio-blk). Fail closed rather than
        // guess at a bring-up. `root_storage` only ever binds a
        // `provides_root_block` floor driver, so reaching here is a packaging
        // defect, not an expected path.
        note(
            audit,
            Level::Error,
            "root-unlock: bound block driver is not a known floor driver; root unbound",
        );
        release_console0_to_login();
        crate::app_store::APP_STORE.note_unavailable();
        return false;
    }
    let Some(frames) = ctx.static_frames() else {
        note(
            audit,
            Level::Error,
            "root-unlock: no kernel frame allocator; root unbound, login refuses",
        );
        release_console0_to_login();
        crate::app_store::APP_STORE.note_unavailable();
        return false;
    };

    let dtb = boot.dtb;
    let caps = service_caps();
    // The system memory-pressure gauge every mounted volume's cache samples,
    // over the same `'static` frame allocator the spawn path uses — physical
    // free frames are the authoritative reading. Fetched from the
    // memory-statistics registry so this boot path, every cache, and the
    // System Information export all share the one gauge.
    let pressure: &'static MemoryPressure =
        tairix_kernel_core::memstats::MEM_STATS.system_pressure(frames);
    let env = UnlockEnv {
        ctx,
        audit,
        pressure,
    };
    let body = move |yielder: &mut dyn YieldHandle| {
        // On success the root-unlock service never returns: it parks for life
        // as the persistent driver-store service, having already logged the
        // unlock outcome and released console 0. Only an early bring-up failure
        // returns here — and because the success arm is the uninhabited
        // [`Infallible`], the `Err` binding is irrefutable. Fail closed: log
        // the stage and open the console-0 gate so `login` proceeds (it refuses
        // every attempt, as a failed unlock installs no database).
        let Err(stage) = run_unlock(yielder, &binding, dtb, frames, caps, env);
        note(audit, Level::Error, stage);
        release_console0_to_login();
        crate::app_store::APP_STORE.note_unavailable();
    };

    let admitted = ctx.spawn_kernel_service(alloc::boxed::Box::new(body));
    if let Some(task_id) = admitted {
        // Publish the disk-owning kthread's scheduler id so its driver-store
        // serve loop registers on `SERVE_WAITQ` and is unparked the instant a
        // request is posted (a real wake, never a busy-yield).
        crate::unlock_service::set_store_service_task(task_id);
    }
    let started = admitted.is_some();
    note(
        audit,
        if started { Level::Info } else { Level::Error },
        if started {
            "root-unlock service kthread admitted (bring-up runs on first dispatch)"
        } else {
            "root-unlock service kthread could not be admitted; opening console gate"
        },
    );
    if !started {
        // Admission failed: nothing will open the gate or publish the `/System`
        // mount, so do both here or console-0 `login` would park forever.
        release_console0_to_login();
        crate::app_store::APP_STORE.note_unavailable();
    }
    started
}

/// Bring up the bound virtio-blk root device and run the interactive unlock
/// policy.
///
/// **On success this never returns:** the shared tail logs the outcome,
/// releases the console-0 gate, and parks the kthread for life as the
/// persistent driver-store service, so the [`Infallible`] `Ok` is never
/// produced. Only an early bring-up failure returns `Err` with a stable stage
/// string; on that path the caller logs it and opens the console-0 gate.
fn run_unlock(
    yielder: &mut dyn YieldHandle,
    binding: &RootBlockBinding,
    dtb: u64,
    frames: &'static FrameAllocator,
    caps: CapabilitySet,
    env: UnlockEnv,
) -> Result<Infallible, &'static str> {
    // Move the kthread's single yield handle into the shared cell both the
    // re-arming IRQ waiter and the cooperative console reader suspend through
    // (one cooperative-yield definition).
    let coop = CooperativeYield::new(yielder);

    // The bus-driver task capability context: the unlock kthread's caps, owner
    // `UNLOCK_TASK`, audited onto the service's audit sink; the virtio
    // register-window map gates on its `CAP_MMIO_MAP`. Leaked to `'static`
    // because the brought-up device host borrows it for the life of the (now
    // `'static`, shared) disk (kernel state is never freed).
    let caller: &'static TaskCapabilities = alloc::boxed::Box::leak(alloc::boxed::Box::new(
        TaskCapabilities::derive(UNLOCK_TASK, UserId(0), caps, caps, env.audit),
    ));

    match binding.driver_path {
        VIRTIO_BLK_PATH => virtio_blk_unlock(&coop, caller, &binding.node, dtb, frames, env),
        _ => Err("root-unlock: bound block driver is not a known floor driver"),
    }
}

/// Bring the virtio-blk root device up through the shared floor bring-up and
/// open it for the unlock (the QEMU `virt` root).
fn virtio_blk_unlock<'a>(
    coop: &'a CooperativeYield<'a>,
    caller: &'static TaskCapabilities,
    node: &HwNode,
    dtb: u64,
    frames: &'static FrameAllocator,
    env: UnlockEnv,
) -> Result<Infallible, &'static str> {
    if dtb == 0 {
        return Err("root-unlock: no device tree; root unbound");
    }
    // SAFETY: on the boot hand-off `dtb` is the firmware/OpenSBI device-tree
    // pointer (`a1`, preserved by the arch trampoline), identity-mapped and
    // immutable for the life of the kernel. `Fdt::from_ptr` validates the magic
    // and bounds the blob by its own `totalsize` before any read.
    let fdt = unsafe { Fdt::from_ptr(dtb as *const u8) }
        .map_err(|_| "root-unlock: device tree unreadable; root unbound")?;
    // SAFETY: `dtb` and the size bound the same firmware blob `Fdt::from_ptr`
    // validated; it is identity-mapped, read-only, and outlives the kernel.
    let tree: &'static [u8] =
        unsafe { core::slice::from_raw_parts(dtb as *const u8, fdt.total_size()) };
    let device = bring_up_virtio_mmio::<RiscvFloor>(
        env.ctx,
        node,
        tree,
        caller,
        env.audit,
        frames,
        Confinement::WhereTranslated,
    )?;
    finish_virtio_unlock(
        device.transport,
        device.host,
        coop,
        env,
        &RISCV_UNLOCK_CONSOLE,
    )
}
