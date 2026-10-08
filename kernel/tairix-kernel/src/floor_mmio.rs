//! The one sequence that hands the kernel a virtio-MMIO block slot to drive
//! itself on a device-tree port: the floor disk the root unlock opens.
//!
//! The port supplies what its silicon decides — the bookkeeping page tables,
//! the maps the CPU reaches registers and frames through, and where its
//! devices take their interrupts — through [`MmioFloorPort`]; everything else
//! is this one definition.

use alloc::boxed::Box;

use tairix_abi::driver::dma::PoolId;
use tairix_abi::{DmaCoherence, HwNode};
use tairix_arch_api::fdtwalk::FdtPlatform;
use tairix_drv_bus_mmio::virtio_mmio_bus_from_dtb;
use tairix_drv_bus_virtio::MmioTransport;
use tairix_drv_storage_virtio_blk::VIRTIO_BLK_DEVICE_ID;
use tairix_fdt::{Fdt, Node};
use tairix_kernel_core::{InitSpawnCtx, IrqParkWaiter};
use tairix_kernel_irq::Trigger;
use tairix_kernel_mem::{
    AddressSpace, DmaPool, FrameAllocator, MmioMap, PageTable, PhysMap, VirtAddr,
};
use tairix_kernel_sec::captable::TaskCapabilities;
use tairix_kernel_virtio::{provision_virtio_mmio, KernelMmioMapper, KernelVirtioHost};
use tairix_log::Sink;

use crate::floor_dma::{translator, Confinement, WINDOW_PAGES};
use crate::floor_irq::LineHost;

/// Pages of the register-window map: one slot's registers.
const MMIO_CAP_PAGES: usize = 64;

/// The half of the bring-up a device-tree port supplies.
///
/// # Safety
///
/// The virtio-MMIO aperture the boot device tree describes is mapped at its
/// physical address as device memory for the life of the kernel, and nothing
/// holds a reference into it: it is reached only by volatile register access.
pub unsafe trait MmioFloorPort {
    /// The port's half of the device-tree walk, whose conventions a slot's
    /// coherence is read under.
    type Platform: FdtPlatform;
    /// The page tables the register map and the DMA window are accounted in.
    type Space: PageTable + 'static;
    /// Base of the register map in a bookkeeping space, above the extent the
    /// space identity-maps.
    const MMIO_VBASE: u64;
    /// Base of the DMA window in a bookkeeping space, likewise.
    const POOL_VBASE: u64;

    /// A fresh bookkeeping space. It is never made live: the CPU reaches
    /// registers through [`Self::registers`] and frames through
    /// [`Self::frames`].
    fn bookkeeping_space() -> Option<Self::Space>;

    /// The map the CPU reaches device registers through.
    fn registers() -> &'static dyn PhysMap;

    /// The map the CPU reaches DMA frames through.
    fn frames() -> &'static dyn PhysMap;

    /// The line the first `interrupts` specifier of `slot` names.
    fn slot_line(fdt: &Fdt<'_>, slot: &Node<'_>) -> Option<u32>;

    /// Where the port's devices take their interrupts, once the core has
    /// published its table.
    fn lines() -> Option<LineHost>;
}

/// The DMA host a floor driver allocates through.
pub type FloorHost<S> = KernelVirtioHost<'static, S, dyn Sink + Sync>;

/// A virtio-MMIO block slot the kernel drives itself.
pub struct FloorDevice<S: PageTable + 'static> {
    /// Its transport, provisioned and its interrupt armed.
    pub transport: MmioTransport,
    /// The DMA host its driver allocates through.
    pub host: &'static FloorHost<S>,
}

/// The register-window map a floor device is provisioned through. Leaked:
/// the device it serves is driven for the life of the system.
///
/// # Errors
///
/// The step that failed.
pub fn register_map<P: MmioFloorPort>(
) -> Result<&'static mut MmioMap<'static, P::Space>, &'static str> {
    let space = P::bookkeeping_space().ok_or("floor: mmio bookkeeping space")?;
    let map = MmioMap::new(
        AddressSpace::new(space),
        VirtAddr::new(P::MMIO_VBASE),
        MMIO_CAP_PAGES,
        P::registers(),
    )
    .map_err(|_| "floor: mmio map")?;
    Ok(Box::leak(Box::new(map)))
}

/// A bookkeeping-accounted DMA window of `pages` over `frames`, reached
/// through the port's frame map and meeting the caches as `coherence` says.
///
/// # Errors
///
/// The step that failed.
pub fn dma_window<P: MmioFloorPort>(
    pages: usize,
    frames: &'static FrameAllocator,
    coherence: DmaCoherence,
) -> Result<DmaPool<'static, P::Space>, &'static str> {
    let space = P::bookkeeping_space().ok_or("floor: dma bookkeeping space")?;
    DmaPool::new(
        AddressSpace::new(space),
        VirtAddr::new(P::POOL_VBASE),
        pages,
        frames,
        P::frames(),
        coherence,
    )
    .map_err(|_| "floor: dma pool")
}

/// Bring up the virtio-MMIO block slot `node` names, in the boot device tree
/// `dtb`, for the kernel to drive as `caller`'s process under its
/// capabilities, over frames from `frames`, confined as `confinement` says.
///
/// The node is claimed for the kernel first, so no process admitted as its
/// driver can reach the device. A slot that does not snoop the caches is
/// refused before its interrupt is bound: the host reaches its buffers through
/// the cached direct map, so the device would read stale rings.
///
/// # Errors
///
/// The step that failed.
pub fn bring_up_virtio_mmio<P: MmioFloorPort>(
    ctx: &dyn InitSpawnCtx,
    node: &HwNode,
    dtb: &'static [u8],
    caller: &'static TaskCapabilities,
    audit: &'static (dyn Sink + Sync),
    frames: &'static FrameAllocator,
    confinement: Confinement,
) -> Result<FloorDevice<P::Space>, &'static str> {
    let owner = caller.process();
    let slot =
        crate::hwdiscovery::virtio_mmio_block_slot(node).ok_or("floor: the node names no slot")?;
    ctx.claim_for_kernel(node.id())
        .map_err(|_| "floor: the node has a driver")?;
    let translator = translator(ctx, node, confinement)?;
    let fdt = Fdt::new(dtb).map_err(|_| "floor: device tree unreadable")?;

    // SAFETY: the port's `MmioFloorPort` contract: the aperture the tree
    // describes stays mapped as device memory and is reached only by volatile
    // register access.
    let bus = unsafe { virtio_mmio_bus_from_dtb(dtb) }.map_err(|_| "floor: virtio bus")?;
    let mmio = register_map::<P>()?;
    let provision = provision_virtio_mmio(
        &bus,
        VIRTIO_BLK_DEVICE_ID,
        slot,
        &KernelMmioMapper::new(mmio, caller, audit),
        MmioTransport::new,
    )
    .map_err(|_| "floor: virtio provisioning")?;

    let base = provision.base;
    if crate::iommu_fdt::slot_coherence(&fdt, base, P::Platform::DEFAULT_DMA_COHERENCE)
        != Some(DmaCoherence::Snooped)
    {
        return Err("floor: the virtio device does not snoop the caches");
    }
    let line = crate::hwdiscovery::virtio_mmio_slot(&fdt, base)
        .and_then(|slot| P::slot_line(&fdt, &slot))
        .ok_or("floor: no device interrupt in the tree")?;
    // A virtio-MMIO transport signals by level until its status is acknowledged.
    let armed = P::lines().ok_or("floor: no published IRQ table")?.bind(
        line,
        Some(Trigger::Level),
        owner,
    )?;

    let pool = dma_window::<P>(WINDOW_PAGES, frames, DmaCoherence::Snooped)?;
    let pool = match translator {
        Some(translator) => pool.translated(translator),
        None => pool,
    };
    let waiter: &'static IrqParkWaiter = Box::leak(Box::new(armed.waiter()));
    let host: &'static FloorHost<P::Space> = Box::leak(Box::new(KernelVirtioHost::new(
        pool,
        caller,
        audit,
        PoolId::fresh(),
        armed.table(),
        armed.handle(),
        waiter,
    )));
    Ok(FloorDevice {
        transport: provision.transport,
        host,
    })
}
