//! The aarch64 half of the root unlock (`plans/PI.md` P11 Chunk B-2
//! INCREMENT (2)): the shared admission and tail are
//! [`crate::unlock_orchestrate`]'s, and this port brings up the floor device
//! [`crate::root_storage`] bound — a virtio-blk slot through the shared
//! [`crate::floor_mmio`] bring-up, or a device whose driver the kernel lends
//! a host over the device's own resources, with the channel to this port's
//! firmware ([`tairix_arch_aarch64::firmware`]) for the bring-up alone.

use core::convert::Infallible;

use tairix_abi::driver::mailbox::MailboxChannel;
use tairix_abi::{HwNode, IrqHandle, MmioMapper, RegisterWindow};
use tairix_arch_aarch64::fdt::gic_device_intid;
use tairix_arch_aarch64::firmware::{with_floor_channel, FLOOR_BUFFER_LEN, FLOOR_BUFFER_LIMIT};
use tairix_arch_aarch64::kernel_arch::clean_invalidate_dcache_range;
use tairix_arch_aarch64::paging::{
    configured_identity_gigapages, AddressSpace as ArchAddressSpace, PageTablePool,
};
use tairix_arch_aarch64::platform::Aarch64Fdt;
use tairix_arch_aarch64::SERIAL_SINK;
use tairix_fdt::{Fdt, Node};
use tairix_kernel_core::{CooperativeYield, InitSpawnCtx};
use tairix_kernel_irq::IrqTable;
use tairix_kernel_mem::{PhysAddr, PhysMap};
use tairix_log::Sink;

use crate::aarch64::arch_wrapper::{UART_CONSOLE, UART_CONSOLE_READ};
use crate::aarch64::gic_irq::{published_irq_table, COMPOSITE_IRQ_CONTROLLER};
use crate::aarch64::spawn_producer::SPAWN_TABLE_PHYSMAP;
use crate::console_layout::framebuffer_console0;
use crate::driver_catalog::HostFloor;
use crate::floor_dma::Confinement;
use crate::floor_irq::LineHost;
use crate::floor_mmio::{bring_up_host_floor, bring_up_virtio_mmio, HostFloorPort, MmioFloorPort};
use crate::root_mount::LATE_USERS_DB;
use crate::unlock_orchestrate::{
    finish_unlock, finish_virtio_unlock, spawn_unlock, Console0, FloorEnv, KeySource,
    UnlockConsole, UnlockEnv, UnlockPort,
};
use crate::unlock_service::{boot_tree, CONSOLE0_GATE};

/// The page-table frames the floor's bookkeeping spaces take their tables
/// from, apart from the boot and init pools so the unlock never contends them.
static UNLOCK_PT_POOL: PageTablePool = PageTablePool::new();

/// The aarch64 half of the floor bring-up.
struct Aarch64Floor;

// SAFETY: the boot maps the tree's virtio-MMIO aperture into the identity
// window's Device gigapages (`DeviceWindows`) for the life of the kernel, and
// nothing holds a reference into it.
unsafe impl MmioFloorPort for Aarch64Floor {
    type Platform = Aarch64Fdt;
    type Space = ArchAddressSpace;
    const MMIO_VBASE: u64 = 0x40_0000_0000;
    const POOL_VBASE: u64 = 0x60_0000_0000;

    fn bookkeeping_space() -> Option<ArchAddressSpace> {
        ArchAddressSpace::new_identity_gigapages(&UNLOCK_PT_POOL, configured_identity_gigapages())
    }

    fn registers() -> &'static dyn PhysMap {
        &DeviceWindows
    }

    fn frames() -> &'static dyn PhysMap {
        &SPAWN_TABLE_PHYSMAP
    }

    fn slot_line(_fdt: &Fdt<'_>, slot: &Node<'_>) -> Option<u32> {
        gic_device_intid(slot)
    }

    fn lines() -> Option<LineHost> {
        Some(LineHost {
            table: published_irq_table()?,
            controller: &COMPOSITE_IRQ_CONTROLLER,
            park: wfi_fallback_park,
        })
    }
}

impl HostFloorPort for Aarch64Floor {
    const FIRMWARE_BUFFER: usize = FLOOR_BUFFER_LEN;
    const FIRMWARE_LIMIT: u64 = FLOOR_BUFFER_LIMIT;

    fn sync_dma(base: *const u8, len: usize) {
        clean_invalidate_dcache_range(base as usize, len);
    }

    fn busy_delay_us(us: u32) {
        tairix_arch_aarch64::kernel_arch::busy_delay_us(us);
    }

    fn with_firmware<R>(
        fdt: &Fdt<'_>,
        map: &dyn MmioMapper,
        buffer: Option<RegisterWindow>,
        node: &HwNode,
        body: impl FnOnce(Option<&dyn MailboxChannel>) -> R,
    ) -> R {
        with_floor_channel(fdt, map, buffer, node, body)
    }
}

/// The park a device wait takes where the scheduler cannot park it: mask IRQ
/// taking, re-check the line's ready flag and `wfi` only if it is still not
/// ready, so a completion landing in that window stays pending and wakes the
/// `wfi` rather than being lost.
fn wfi_fallback_park(table: &IrqTable, handle: IrqHandle) {
    // SAFETY: the EL1 vector table is installed (boot
    // `exceptions::init_vectors`) and the production device dispatch is
    // published (the kernel-core `irq` phase), so the woken IRQ is handled
    // rather than faulting; the three calls only manipulate `DAIF.I` and issue
    // the `wfi` hint.
    unsafe {
        tairix_arch_aarch64::exceptions::mask_irq();
        if !table.ready_for(handle) {
            tairix_arch_aarch64::exceptions::wait_for_interrupt();
        }
        tairix_arch_aarch64::exceptions::enable_irq();
    }
}

/// The GIC INTID the operational `virtio,mmio` slot at `slot_base` raises,
/// as [`gic_device_intid`] decodes it; [`None`] when no slot is there or its
/// `interrupts` specifier is unrepresentable.
pub(crate) fn device_spi(fdt: &Fdt<'_>, slot_base: u64) -> Option<u32> {
    gic_device_intid(&crate::hwdiscovery::virtio_mmio_slot(fdt, slot_base)?)
}

/// Release console 0 to `login` and mark the users-database source
/// resolved.
///
/// The two always happen together — both mean "the unlock window is over,
/// `login` may take the console now" — so they are flipped through one
/// helper to keep them from diverging. Opening the gate
/// lets `login`'s gated console reads through ([`GatedConsoleRead`]);
/// [`LateUsersDb::resolve`](tairix_kernel_core::LateUsersDb::resolve) flips
/// a `login` parked on the pending (`WouldBlock`) `users_db_read` into its
/// prompt — against the installed database if the unlock succeeded
/// ([`install`](tairix_kernel_core::LateUsersDb) ran first and wins), else
/// fail-closed deny-all.
fn release_console0_to_login() {
    CONSOLE0_GATE.open();
    // Opening the gate is an input-availability edge for any `login` already
    // parked on the (until now) withheld console-0 read: nudge the console
    // wait-queue so it re-polls the now-open gate at once, draining any
    // type-ahead buffered in the console-0 queue during the closed window
    // rather than waiting for the next keystroke to push and wake it
    // (the wake that closes the gated-park race). A
    // no-op before the wait-queue arch hook is installed, and a spurious
    // wake for a reader on another console is harmless (it re-polls and
    // re-parks).
    tairix_kernel_core::console_wake();
    LATE_USERS_DB.resolve();
    // The passphrase poll is over: on a serial-only boot, switch the UART
    // console from polled to interrupt-driven so a `login` reader now parks
    // off the run queue and the receive interrupt wakes it. A no-op when the
    // boot path discovered no console interrupt (the console then stays on
    // the polled path). With an active video console the UART is the
    // session-free debug log line — no console is installed on it, so its
    // receive interrupt is left disabled rather than armed for a reader
    // that can never exist.
    if !crate::console_layout::CONSOLE.is_active() {
        crate::aarch64::gic_irq::enable_uart_console_irq();
    }
}

/// The aarch64 half of the shared root unlock: its console-0 seam and its
/// floor device bring-ups.
///
/// Selects the framebuffer video console when a display is active, else the
/// discovered UART — arming the UART's interrupt-driven receive so a keystroke
/// wakes the parked passphrase reader — and releases console 0 to `login`
/// through [`release_console0_to_login`] the instant the unlock resolves.
struct Aarch64Unlock;

/// The single `'static` [`Aarch64Unlock`] the shared orchestration runs.
static AARCH64_UNLOCK: Aarch64Unlock = Aarch64Unlock;

impl UnlockConsole for Aarch64Unlock {
    fn acquire_console0(&self) -> Console0 {
        // Both input halves are interrupt-fed queues whose push wakes the
        // console wait-queue, so the reader parks for input rather than
        // polling a raw FIFO.
        framebuffer_console0().unwrap_or_else(|| {
            // Interrupt-driven receive for the unlock window, so a keystroke
            // wakes the parked reader; idempotent with the `login` handoff.
            crate::aarch64::gic_irq::enable_uart_console_irq();
            Console0 {
                write: &UART_CONSOLE,
                read: &UART_CONSOLE_READ,
                keys: KeySource::Live,
            }
        })
    }

    fn release_console0_to_login(&self) {
        release_console0_to_login();
    }
}

impl UnlockPort for Aarch64Unlock {
    fn serial_audit(&self) -> &'static (dyn Sink + Sync) {
        &SERIAL_SINK
    }

    fn virtio_unlock(
        &'static self,
        coop: &CooperativeYield<'_>,
        node: &HwNode,
        floor: FloorEnv,
        env: UnlockEnv,
    ) -> Result<Infallible, &'static str> {
        // SAFETY: `floor.dtb` is the pointer the boot recorded from the
        // loader's hand-off (`boot.s` preserves x0), identity-mapped and
        // immutable for the life of the kernel.
        let tree = unsafe { boot_tree(floor.dtb as *const u8) }?;
        let device = bring_up_virtio_mmio::<Aarch64Floor>(
            env.ctx,
            node,
            tree,
            floor.caller,
            env.audit,
            floor.frames,
            Confinement::WhereTranslated,
        )?;
        finish_virtio_unlock(device.transport, device.host, coop, env, self)
    }

    fn host_unlock(
        &'static self,
        coop: &CooperativeYield<'_>,
        node: &HwNode,
        driver: &HostFloor,
        floor: FloorEnv,
        env: UnlockEnv,
    ) -> Result<Infallible, &'static str> {
        // SAFETY: as for `virtio_unlock`.
        let tree = unsafe { boot_tree(floor.dtb as *const u8) }?;
        let disk = bring_up_host_floor::<Aarch64Floor>(
            env.ctx,
            node,
            driver,
            tree,
            floor.caller,
            env.audit,
            floor.frames,
        )?;
        // The disk is the system's for life, shared behind the block layer.
        finish_unlock(alloc::boxed::Box::leak(disk), coop, env, self)
    }
}

/// Admit the in-kernel root-unlock kthread for the floor device the boot
/// bound, returning whether it was started ([`spawn_unlock`]).
#[must_use]
pub fn spawn_if_present(ctx: &'static (dyn InitSpawnCtx + Sync)) -> bool {
    spawn_unlock(ctx, &AARCH64_UNLOCK)
}

/// The physical map register windows are reached through: the kernel's
/// Device gigapages, where the board's discovered MMIO lives.
///
/// Those are sparse, so this is no general physical map. RAM is reached
/// through the kernel's direct map instead; a window that is not wholly
/// inside a Device gigapage fails closed rather than faulting on an invalid
/// slot or aliasing memory the direct map holds cacheable.
struct DeviceWindows;

impl PhysMap for DeviceWindows {
    fn translate(&self, phys: PhysAddr, len: usize) -> Option<core::ptr::NonNull<u8>> {
        crate::aarch64::boot::device_registers(phys.as_u64(), len)
    }

    /// Device memory is never cached, so there is no alias to clean.
    fn clean_invalidate(&self, _phys: PhysAddr, _len: usize) {}

    /// Register windows are never executed.
    fn sync_instruction_cache(&self, _phys: PhysAddr, _len: usize) {}
}
