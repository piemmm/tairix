//! The one sequence that hands the kernel a virtio-PCI function to drive
//! itself: the floor disk root unlock opens, and the misbehaving device the
//! DMA-fault vertical provokes.
//!
//! The function delivers through MSI-X on a vector of its own, an edge message
//! that never uses an IO-APIC pin, so its line's controller half is the
//! composite controller's edge no-op and the waiter's ready-flag consume is the
//! whole interlock.

use alloc::boxed::Box;

use tairix_abi::{HwNode, IrqHandle};
use tairix_arch_x86_64::paging::{AddressSpace as ArchAddressSpace, PageTablePool};
use tairix_drv_bus_virtio::PciTransport;
use tairix_kernel_core::iommu::{MasterOwner, MasterTarget, KERNEL_OWNER};
use tairix_kernel_core::{InitSpawnCtx, IrqParkWaiter};
use tairix_kernel_irq::IrqTable;
use tairix_kernel_mem::{AddressSpace, DmaPool, FrameAllocator, MmioMap, VirtAddr};
use tairix_kernel_sec::captable::TaskCapabilities;
use tairix_kernel_virtio::{provision_virtio_pci, KernelMmioMapper, KernelVirtioHost};
use tairix_log::Sink;
use tairix_virtio::PoolId;

use crate::floor_dma::{translator, Confinement, WINDOW_PAGES};
use crate::floor_irq::LineHost;
use crate::x86_64::arch_wrapper::published_irq_table;
use crate::x86_64::spawn_producer::SPAWN_TABLE_PHYSMAP;

/// Pages of the register-window map: a virtio function's four configuration
/// windows.
const MMIO_CAP_PAGES: usize = 64;

/// Bookkeeping base of the register-window map. Its tables are written into a
/// space never made live — the CPU reaches the registers through the identity
/// direct map — and both bases sit above the 32 MiB identity that space maps.
const MMIO_VBASE: u64 = 0x6000_0000;

/// Bookkeeping base of the DMA window (see [`MMIO_VBASE`]).
const POOL_VBASE: u64 = 0x2000_0000;

/// The frames those two bookkeeping spaces take their tables from, apart from
/// the boot and init pools so a floor bring-up never contends them.
static FLOOR_PT_POOL: PageTablePool = PageTablePool::new();

/// The DMA host a floor driver allocates through.
pub type FloorHost = KernelVirtioHost<'static, ArchAddressSpace, dyn Sink + Sync>;

/// A virtio-PCI function the kernel drives itself.
pub struct FloorDevice {
    /// Its transport, provisioned and interrupt-routed.
    pub transport: PciTransport,
    /// The DMA host its driver allocates through, every carve confined as
    /// the bring-up's [`Confinement`] said.
    pub host: &'static FloorHost,
}

/// Bring up the virtio-PCI function the probe published for `node` for the
/// kernel to drive as `caller`'s process under its capabilities, over frames
/// from `frames`, confined as `confinement` says.
///
/// The node is claimed for the kernel first, so no process admitted as its
/// driver can reach the device or take its function back.
///
/// # Errors
///
/// The step that failed.
pub fn bring_up_virtio_pci(
    ctx: &dyn InitSpawnCtx,
    node: &HwNode,
    caller: &'static TaskCapabilities,
    audit: &'static (dyn Sink + Sync),
    frames: &'static FrameAllocator,
    confinement: Confinement,
) -> Result<FloorDevice, &'static str> {
    let owner = caller.process();
    ctx.claim_for_kernel(node.id())
        .map_err(|_| "virtio-PCI floor: the node has a driver")?;
    let host = crate::pci_host::published().ok_or("virtio-PCI floor: no PCI host")?;
    let phys = &SPAWN_TABLE_PHYSMAP;

    let mmio_space = ArchAddressSpace::new_bookkeeping_identity_32mib(&FLOOR_PT_POOL)
        .ok_or("virtio-PCI floor: mmio bookkeeping space")?;
    let mmio: &'static mut MmioMap<'static, ArchAddressSpace> = Box::leak(Box::new(
        MmioMap::new(
            AddressSpace::new(mmio_space),
            VirtAddr::new(MMIO_VBASE),
            MMIO_CAP_PAGES,
            phys,
        )
        .map_err(|_| "virtio-PCI floor: mmio map")?,
    ));

    let function = host
        .published(node.id())
        .ok_or("virtio-PCI floor: node unrecorded")?;
    let transport = {
        let mapper = KernelMmioMapper::new(&mut *mmio, caller, audit);
        host.with(function.segment, |bus| {
            provision_virtio_pci(bus, function.address, &mapper, |windows| {
                PciTransport::new(windows, Some(crate::pci_host::MSIX_ENTRY))
            })
        })
        .ok_or("virtio-PCI floor: node's segment unowned")?
        .map_err(|_| "virtio-PCI floor: provisioning")?
        .transport
    };

    let lines = LineHost {
        table: published_irq_table().ok_or("virtio-PCI floor: no published IRQ table")?,
        controller: crate::x86_64::msi::published_composite()
            .ok_or("virtio-PCI floor: no interrupt controller")?,
        park: hlt_fallback_park,
    };
    let dma_space = ArchAddressSpace::new_bookkeeping_identity_32mib(&FLOOR_PT_POOL)
        .ok_or("virtio-PCI floor: dma bookkeeping space")?;
    let pool = DmaPool::new(
        AddressSpace::new(dma_space),
        VirtAddr::new(POOL_VBASE),
        WINDOW_PAGES,
        frames,
        phys,
        tairix_arch_x86_64::DMA_COHERENCE,
    )
    .map_err(|_| "virtio-PCI floor: dma pool")?;
    let translator = translator(ctx, node, confinement)?;

    // Routed last but for the bind, whose refusal gives the route back. A
    // message has no trigger to configure.
    let routed = crate::x86_64::remapping::route_function(host, &function)
        .map_err(|_| "virtio-PCI floor: route MSI-X")?;
    let line = routed.vector.line;
    let armed = lines
        .bind(line, None, owner)
        .inspect_err(|_| routed.release())?;
    // A translated function masters once its domain is attached at the first
    // carve; an untranslated one is handed over here, once its message is
    // routed and before its driver can program it.
    let pool = if let Some(translator) = translator {
        pool.translated(translator)
    } else {
        if let Some(mastering) = ctx.bus_mastering() {
            let master = MasterOwner {
                node: node.id(),
                generation: KERNEL_OWNER,
                epoch: mastering.begin(),
            };
            mastering.set(MasterTarget::Node(master.node), true, master);
        }
        pool
    };

    let waiter: &'static IrqParkWaiter = Box::leak(Box::new(armed.waiter()));
    let host: &'static FloorHost = Box::leak(Box::new(KernelVirtioHost::new(
        pool,
        caller,
        audit,
        PoolId::fresh(),
        armed.table(),
        armed.handle(),
        waiter,
    )));
    Ok(FloorDevice { transport, host })
}

/// Race-free CPU park for a device wait whose context cannot be
/// scheduler-parked — a kthread bringing the device up before the dispatch
/// loop runs its first task ([`IrqParkWaiter`]'s fallback).
///
/// Mask maskable interrupts, re-check the line's ready flag, and only if it is
/// still not ready enter the atomic `sti; hlt`, which enables interrupts
/// exactly as the halt begins, so a completion landing in the check-park
/// window is taken during the halt and no edge is lost.
fn hlt_fallback_park(table: &IrqTable, handle: IrqHandle) {
    // SAFETY: the IDT and LAPIC are installed by this point (the boot
    // pipeline's per-CPU init), and the device's MSI-X vector is the routed
    // wake source, so a taken interrupt dispatches through a valid handler.
    // `cli`/`sti`/`hlt` are privileged but well-defined in ring 0 and touch
    // only `IF`; `preserves_flags` is intentionally omitted because
    // `sti`/`cli` modify `IF`.
    unsafe {
        core::arch::asm!("cli", options(nomem, nostack));
        if !table.ready_for(handle) {
            core::arch::asm!("sti; hlt; cli", options(nomem, nostack));
        }
        core::arch::asm!("sti", options(nomem, nostack));
    }
}
