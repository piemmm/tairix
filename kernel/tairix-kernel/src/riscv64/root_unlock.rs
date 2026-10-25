//! The riscv64 half of the root unlock (`plans/NETWORK.md` N4e-riscv64): the
//! shared admission and tail are [`crate::unlock_orchestrate`]'s, and this
//! port brings up a virtio-blk slot through the shared [`crate::floor_mmio`]
//! bring-up. It names the riscv64 port directly — the external-IRQ dispatch,
//! the Sv39 paging primitives, the firmware device tree — so it lives in the
//! architecture subtree. It has no host to lend a floor driver that opens
//! its own device, so it refuses one.

use core::convert::Infallible;

use tairix_abi::{HwNode, IrqHandle};
use tairix_arch_api::fdtwalk::FdtPlatform;
use tairix_arch_riscv64::paging::{AddressSpace as ArchAddressSpace, PageTablePool};
use tairix_arch_riscv64::platform::Riscv64Fdt;
use tairix_arch_riscv64::{trap, SERIAL_SINK};
use tairix_fdt::{Fdt, Node};
use tairix_kernel_core::{CooperativeYield, InitSpawnCtx};
use tairix_kernel_irq::IrqTable;
use tairix_kernel_mem::PhysMap;
use tairix_log::Sink;

use crate::floor_dma::Confinement;
use crate::floor_irq::LineHost;
use crate::floor_mmio::{bring_up_virtio_mmio, MmioFloorPort};
use crate::riscv64::irq::{controller as irq_controller, published_irq_table};
use crate::riscv64::spawn_producer::SPAWN_TABLE_PHYSMAP;
use crate::unlock_orchestrate::{
    finish_virtio_unlock, spawn_unlock, Console0, FloorEnv, KeySource, UnlockConsole, UnlockEnv,
    UnlockPort,
};
use crate::unlock_service::{boot_tree, CONSOLE0_GATE};

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

/// The riscv64 half of the shared root unlock: its console-0 seam and its
/// virtio floor bring-up.
///
/// The SBI console is the primary console: its write half streams the
/// passphrase prompt and its read half drains the legacy `console_getchar`
/// service, so an operator can answer the prompt on this port exactly as on
/// the others. The drain is non-blocking, so the shared reader parks between
/// polls rather than spinning, and a console that never delivers a byte
/// leaves the unlock waiting rather than mounting on a guess.
struct RiscvUnlock;

/// The single `'static` [`RiscvUnlock`] the shared orchestration runs.
static RISCV_UNLOCK: RiscvUnlock = RiscvUnlock;

impl UnlockConsole for RiscvUnlock {
    fn acquire_console0(&self) -> Console0 {
        crate::console_layout::framebuffer_console0().unwrap_or(Console0 {
            write: &crate::riscv64::boot::RISCV_UART_CONSOLE,
            read: &crate::riscv64::boot::RISCV_UART_CONSOLE_READ,
            keys: KeySource::Live,
        })
    }

    fn release_console0_to_login(&self) {
        release_console0_to_login();
    }
}

impl UnlockPort for RiscvUnlock {
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
        // firmware's hand-off (`a1`, preserved by the arch trampoline),
        // identity-mapped and immutable for the life of the kernel.
        let tree = unsafe { boot_tree(floor.dtb as *const u8) }?;
        let device = bring_up_virtio_mmio::<RiscvFloor>(
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
}

/// Admit the in-kernel root-unlock kthread for the floor device the boot
/// bound, returning whether it was started ([`spawn_unlock`]).
#[must_use]
pub fn spawn_if_present(ctx: &'static (dyn InitSpawnCtx + Sync)) -> bool {
    spawn_unlock(ctx, &RISCV_UNLOCK)
}
