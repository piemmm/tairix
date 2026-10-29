//! The sequences that hand the kernel a floor disk to drive itself on a
//! device-tree port: a virtio-MMIO block slot, and a device whose driver the
//! kernel lends a host over the device's own resources.
//!
//! The port supplies what its silicon decides — the bookkeeping page tables,
//! the maps the CPU reaches registers and frames through, where its devices
//! take their interrupts, and for a host-opened device its cache maintenance,
//! its spin and its firmware channel — through [`MmioFloorPort`] and
//! [`HostFloorPort`]; everything else is this one definition.

use alloc::boxed::Box;

use tairix_abi::driver::block::Block;
use tairix_abi::driver::dma::PoolId;
use tairix_abi::driver::mailbox::MailboxChannel;
use tairix_abi::{DmaCoherence, HwNode, MmioMapper, RegisterWindow};
use tairix_arch_api::fdtwalk::FdtPlatform;
use tairix_drv_bus_mmio::virtio_mmio_bus_from_dtb;
use tairix_drv_bus_virtio::MmioTransport;
use tairix_drv_storage_virtio_blk::VIRTIO_BLK_DEVICE_ID;
use tairix_fdt::{Fdt, Node};
use tairix_kernel_core::{InitSpawnCtx, IrqParkWaiter};
use tairix_kernel_irq::Trigger;
use tairix_kernel_mem::{
    window_slots, AddressSpace, DmaPool, FrameAllocator, MmioMap, PageTable, PhysMap, VirtAddr,
};
use tairix_kernel_sec::captable::TaskCapabilities;
use tairix_kernel_sec::dma::alloc_dma;
use tairix_kernel_virtio::{provision_virtio_mmio, KernelMmioMapper, KernelVirtioHost};
use tairix_log::Sink;

use crate::driver_catalog::HostFloor;
use crate::floor_dma::{node_window, translator, Confinement, WindowDmaHost, WINDOW_PAGES};
use crate::floor_host::{claim_and_bind, FloorDriverHost};
use crate::floor_irq::{FloorCompletion, LineHost};

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

/// The half of a host-opened floor device's bring-up a port supplies beyond
/// its MMIO floor.
pub trait HostFloorPort: MmioFloorPort {
    /// Bytes of the buffer the port's firmware channel exchanges through.
    const FIRMWARE_BUFFER: usize;
    /// The exclusive CPU-physical bound that buffer lies below.
    const FIRMWARE_LIMIT: u64;

    /// Bring cacheable memory at `[base, base + len)` and a device that does
    /// not snoop the caches into agreement.
    fn sync_dma(base: *const u8, len: usize);

    /// Wait `us` microseconds where the caller cannot be parked.
    fn busy_delay_us(us: u32);

    /// Run `body` with the port's channel to its firmware for the driver of
    /// `node`, over the doorbell `fdt` names, mapped through `map`, and the
    /// carved `buffer` — or with `None` where the port has none to lend.
    fn with_firmware<R>(
        fdt: &Fdt<'_>,
        map: &dyn MmioMapper,
        buffer: Option<RegisterWindow>,
        node: &HwNode,
        body: impl FnOnce(Option<&dyn MailboxChannel>) -> R,
    ) -> R;
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
    let waiter: &'static IrqParkWaiter = Box::leak(Box::new(armed.waiter(audit)));
    let host: &'static FloorHost<P::Space> = Box::leak(Box::new(KernelVirtioHost::new(
        pool,
        caller,
        audit,
        PoolId::fresh(),
        waiter,
    )));
    Ok(FloorDevice {
        transport: provision.transport,
        host,
    })
}

/// Bring up the device `node` names, which `floor`'s driver opens over the
/// host the kernel lends it, for the kernel to drive as `caller`'s process
/// under its capabilities, over frames from `frames`.
///
/// The node is claimed and its line bound and armed before anything of the
/// device is mapped. The driver then maps only the node's own windows,
/// carves inside the node's DMA window, and borrows the port's firmware
/// channel for its bring-up alone; it records its own outcome on `audit`.
///
/// # Errors
///
/// The step that failed.
pub fn bring_up_host_floor<P: HostFloorPort>(
    ctx: &dyn InitSpawnCtx,
    node: &HwNode,
    floor: &HostFloor,
    dtb: &'static [u8],
    caller: &'static TaskCapabilities,
    audit: &'static (dyn Sink + Sync),
    frames: &'static FrameAllocator,
) -> Result<Box<dyn Block>, &'static str> {
    let lines = P::lines().ok_or("floor: no published IRQ table")?;
    let armed = claim_and_bind(ctx, node, lines, caller.process())?;
    let wait: &'static FloorCompletion = Box::leak(Box::new(FloorCompletion::new(
        armed.waiter(audit),
        P::busy_delay_us,
    )));
    let fdt = Fdt::new(dtb).map_err(|_| "floor: device tree unreadable")?;
    let mmio = register_map::<P>()?;
    let window = node_window(node);
    // A device with no window moves nothing by DMA, so its carves take no pages.
    let carves = window.map_or(0, |_| floor.dma_pages);
    let pages = window_slots(P::FIRMWARE_BUFFER)
        .and_then(|firmware| carves.checked_add(firmware))
        .ok_or("floor: the firmware buffer fits no pool")?;
    let mut pool = dma_window::<P>(
        pages,
        frames,
        window.map_or(DmaCoherence::Unsnooped, |window| window.coherence),
    )?;
    let buffer = firmware_buffer::<P>(&mut pool, caller, audit);
    let dma: &'static WindowDmaHost<'static, P::Space, dyn Sink + Sync> = Box::leak(Box::new(
        WindowDmaHost::new(pool, window, caller, audit, PoolId::fresh(), P::sync_dma),
    ));
    let mapper = KernelMmioMapper::new(mmio, caller, audit);
    P::with_firmware(&fdt, &mapper, buffer, node, |mailbox| {
        let host = FloorDriverHost::new(*caller.effective(), node, &mapper, Some(dma), mailbox);
        (floor.open)(&host, node, wait, audit)
            .map_err(|_| "floor: the driver could not open its device")
    })
}

/// The firmware channel's buffer, carved from `pool` below the port's limit;
/// `None` when the carve is refused, which leaves the driver no channel.
fn firmware_buffer<P: HostFloorPort>(
    pool: &mut DmaPool<'_, P::Space>,
    caller: &TaskCapabilities,
    audit: &dyn Sink,
) -> Option<RegisterWindow> {
    let buffer = alloc_dma(pool, caller, P::FIRMWARE_BUFFER, P::FIRMWARE_LIMIT, audit).ok()?;
    let base = pool.slot_base(&buffer).ok()?;
    // SAFETY: `alloc_dma` carved at least `FIRMWARE_BUFFER` zeroed bytes at
    // `base`, the CPU address of `buffer.phys()`, reached by nothing else; the
    // carve is never freed, so neither the window nor a late reply outlives it.
    Some(unsafe { RegisterWindow::from_mapping(buffer.phys().as_u64(), base, P::FIRMWARE_BUFFER) })
}
