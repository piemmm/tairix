//! How a device the kernel drives itself — a bootstrap-floor disk — reaches
//! the memory it masters: through the kernel's own domain where a
//! translation unit stands in front of it, physical otherwise, and for a
//! device the kernel lends its driver a host, inside the one window its node
//! declares.

use core::ptr::NonNull;

use tairix_abi::driver::dma::{DmaHost, DmaSlab, PoolId, SlabCoherencyFn};
use tairix_abi::{DmaCoherence, DriverError, HwNode, HwResourceKind};
use tairix_kernel_core::iommu::{DmaPath, KERNEL_OWNER};
use tairix_kernel_core::{dma_constraint, translate_device_addr, DmaConstraint, InitSpawnCtx};
use tairix_kernel_mem::{DmaPool, DmaTranslator, PageTable};
use tairix_kernel_sec::captable::TaskCapabilities;
use tairix_kernel_sec::dma::{alloc_dma, DmaGateError};
use tairix_log::Sink;
use tairix_sync::SpinLock;

/// Pages of the DMA window a floor virtio driver carves its rings and request
/// buffers from.
pub const WINDOW_PAGES: usize = 64;

/// How a floor device's DMA is confined.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Confinement {
    /// Through the unit standing in front of it, or, where none does, as it
    /// masters physical memory.
    WhereTranslated,
    /// Through the unit standing in front of it, or not at all.
    Translated,
}

/// The domain `node`'s device carves through when a unit translates it: the
/// kernel's own, which no driver can take. [`None`] for one no unit stands in
/// front of, where `confinement` allows it.
///
/// # Errors
///
/// A unit that translates nothing stands in front of it, or none does and
/// `confinement` demands one.
pub fn translator(
    ctx: &dyn InitSpawnCtx,
    node: &HwNode,
    confinement: Confinement,
) -> Result<Option<DmaTranslator>, &'static str> {
    let translation = ctx.dma_translation();
    match translation.map(|translation| (translation, translation.dma_path(node.id()))) {
        Some((translation, DmaPath::Translated { output_limit })) => Ok(Some(DmaTranslator {
            node: node.id(),
            generation: KERNEL_OWNER,
            domains: translation,
            output_limit,
        })),
        Some((_, DmaPath::Stranded { .. })) => {
            Err("floor: the device is behind a unit that translates nothing")
        }
        _ if confinement == Confinement::Translated => Err("floor: no unit confines the device"),
        _ => Ok(None),
    }
}

/// The window `node`'s device reaches memory through: its one declared DMA
/// window, or [`None`] — so no DMA — when it declares none or several.
#[must_use]
pub fn node_window(node: &HwNode) -> Option<DmaConstraint> {
    let mut windows = node
        .resources()
        .iter()
        .filter(|resource| resource.kind() == Some(HwResourceKind::Dma));
    let window = windows.next()?;
    if windows.next().is_some() {
        return None;
    }
    dma_constraint(window).ok()
}

/// The DMA host a host-opened floor driver carves through: frames of its own
/// pool inside its node's window, addressed as the device's bus sees them.
///
/// Every carve is re-checked against the caller's `CAP_MEM_DMA` and audited
/// by `alloc_dma`, so the host adds no authority. The carves are minted
/// [`DmaSlab::from_leaked`]: the host is leaked to `'static` with the device
/// it serves, so its pool's frames stay the device's for the kernel's life.
pub struct WindowDmaHost<'a, P: PageTable, S: Sink + ?Sized> {
    /// Locked so `&self` can carve; uncontended, as one bring-up carves.
    pool: SpinLock<DmaPool<'a, P>>,
    /// The CPU window the device reaches and the bus address it starts at;
    /// `None` refuses every carve, so the driver moves nothing by DMA.
    window: Option<DmaConstraint>,
    caller: &'a TaskCapabilities,
    audit: &'a S,
    id: PoolId,
    /// The port's cache maintenance, attached to a carve for a device that
    /// does not snoop.
    sync: SlabCoherencyFn,
}

impl<'a, P: PageTable, S: Sink + ?Sized> WindowDmaHost<'a, P, S> {
    /// Carve from `pool` inside `window` for `caller`, auditing on `audit`,
    /// with `sync` keeping an unsnooped device's carves coherent.
    #[must_use]
    pub fn new(
        pool: DmaPool<'a, P>,
        window: Option<DmaConstraint>,
        caller: &'a TaskCapabilities,
        audit: &'a S,
        id: PoolId,
        sync: SlabCoherencyFn,
    ) -> Self {
        Self {
            pool: SpinLock::new(pool),
            window,
            caller,
            audit,
            id,
            sync,
        }
    }

    /// Carve `size` bytes inside `window`: the buffer's CPU base, device
    /// address, and length.
    fn carve(
        &self,
        size: usize,
        window: &DmaConstraint,
    ) -> Result<(NonNull<u8>, u64, usize), DriverError> {
        let mut pool = self.pool.lock();
        let buf = alloc_dma(&mut *pool, self.caller, size, window.addr_limit, self.audit)
            .map_err(DmaGateError::as_driver_error)?;
        let base = pool
            .slot_base(&buf)
            .map_err(|_| DriverError::LengthOutOfRange)?;
        let device = translate_device_addr(window, buf.phys().as_u64(), buf.len() as u64)
            .map_err(|_| DriverError::LengthOutOfRange)?;
        Ok((base, device, buf.len()))
    }
}

impl<P: PageTable, S: Sink + ?Sized> DmaHost for WindowDmaHost<'_, P, S> {
    fn alloc_dma_zeroed(&self, size: usize) -> Result<DmaSlab, DriverError> {
        if size == 0 {
            return Err(DriverError::BufferTooSmall);
        }
        let window = self.window.ok_or(DriverError::Unsupported)?;
        let (base, device, len) = self.carve(size, &window)?;
        // SAFETY: `carve` took `len` bytes of zeroed, contiguous,
        // guard-bracketed DMA memory at `base`, which the device sees at
        // `device` through the node's window, for this slab alone; nothing
        // frees it, as the host is leaked with the device it serves.
        let slab = unsafe { DmaSlab::from_leaked(device, base, len, self.id, 0) };
        // A device that does not snoop sees its carves only through the
        // port's clean and invalidate at each hand-off.
        Ok(match window.coherence {
            DmaCoherence::Snooped => slab,
            DmaCoherence::Unsnooped => slab.with_coherency(self.sync),
        })
    }

    /// Nothing to release: the carves live for the kernel's life.
    fn device_quiesced(&self) {}
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use tairix_abi::driver::dma::{DmaHost, PoolId};
    use tairix_abi::{CapabilityId, DmaCoherence, DriverError, HwDeviceClass, HwNode, HwResource};
    use tairix_caps::CapabilitySet;
    use tairix_kernel_mem::bootinfo::{BootMemoryMap, MemoryRegion, RegionKind};
    use tairix_kernel_mem::{
        AddressSpace, DmaPool, FrameAllocator, HostPageTable, PhysAddr, SimPhysMap, VirtAddr,
        PAGE_SIZE,
    };
    use tairix_kernel_sec::captable::{ProcessId, TaskCapabilities};
    use tairix_kernel_sec::identity::UserId;

    use super::{node_window, WindowDmaHost};
    use crate::test_support::NullSink;

    /// Where the simulated RAM the pool carves from starts, and its pages.
    const RAM: u64 = PAGE_SIZE as u64 * 16;
    const RAM_PAGES: usize = 16;

    /// The bus address the RAM's first byte is seen at.
    const BUS: u64 = 0xc000_0000;

    std::thread_local! {
        static SYNCED: Cell<usize> = const { Cell::new(0) };
    }

    fn record_sync(_base: *const u8, len: usize) {
        SYNCED.with(|synced| synced.set(synced.get() + len));
    }

    fn frames() -> FrameAllocator {
        let mut map = BootMemoryMap::new();
        map.push(MemoryRegion {
            kind: RegionKind::Usable,
            start: PhysAddr::new(RAM),
            length: (PAGE_SIZE * RAM_PAGES) as u64,
        });
        FrameAllocator::new(&map).expect("one region")
    }

    fn ram() -> SimPhysMap {
        SimPhysMap::new(PhysAddr::new(RAM), PAGE_SIZE * RAM_PAGES)
    }

    fn caller(sink: &NullSink) -> TaskCapabilities {
        let mut caps = CapabilitySet::empty();
        caps.insert(CapabilityId::MEM_DMA);
        TaskCapabilities::derive(ProcessId(7), UserId(0), caps, caps, sink)
    }

    fn pool<'a>(frames: &'a FrameAllocator, ram: &'a SimPhysMap) -> DmaPool<'a, HostPageTable> {
        DmaPool::new(
            AddressSpace::new(HostPageTable::new()),
            VirtAddr::new(0x2000_0000),
            RAM_PAGES,
            frames,
            ram,
            DmaCoherence::Unsnooped,
        )
        .expect("a pool")
    }

    /// A node whose one DMA window is the RAM, seen from [`BUS`].
    fn windowed(coherence: DmaCoherence) -> HwNode {
        let mut node = HwNode::new(3, 0, HwDeviceClass::Storage);
        let top = RAM + (PAGE_SIZE * RAM_PAGES) as u64;
        node.push_resource(HwResource::dma_translated(
            top,
            (PAGE_SIZE * RAM_PAGES) as u64,
            BUS,
            coherence,
        ))
        .expect("room");
        node
    }

    #[test]
    fn a_node_declaring_one_window_is_carved_in_it_and_no_other_is() {
        assert!(node_window(&windowed(DmaCoherence::Snooped)).is_some());
        let mut two = windowed(DmaCoherence::Snooped);
        two.push_resource(HwResource::dma(
            RAM,
            PAGE_SIZE as u64,
            DmaCoherence::Snooped,
        ))
        .expect("room");
        assert_eq!(node_window(&two), None, "two windows are ambiguous");
        assert_eq!(
            node_window(&HwNode::new(4, 0, HwDeviceClass::Storage)),
            None
        );
    }

    #[test]
    fn a_carve_lands_in_the_window_addressed_as_the_bus_sees_it() {
        let (frames, ram, sink) = (frames(), ram(), NullSink);
        let caller = caller(&sink);
        let host = WindowDmaHost::new(
            pool(&frames, &ram),
            node_window(&windowed(DmaCoherence::Snooped)),
            &caller,
            &sink,
            PoolId::fresh(),
            record_sync,
        );
        let slab = host.alloc_dma_zeroed(PAGE_SIZE).expect("a carve");
        let device = slab.device_addr();
        assert!(
            (BUS..BUS + (PAGE_SIZE * RAM_PAGES) as u64).contains(&device),
            "{device:#x} is the window's"
        );
        assert_eq!(device % PAGE_SIZE as u64, 0);
        slab.sync_range(0, PAGE_SIZE);
        assert_eq!(
            SYNCED.with(Cell::get),
            0,
            "a snooping device needs no maintenance"
        );
        assert_eq!(
            host.alloc_dma_zeroed(0).err(),
            Some(DriverError::BufferTooSmall)
        );
    }

    #[test]
    fn an_unsnooped_windows_carves_carry_the_ports_maintenance() {
        let (frames, ram, sink) = (frames(), ram(), NullSink);
        let caller = caller(&sink);
        let host = WindowDmaHost::new(
            pool(&frames, &ram),
            node_window(&windowed(DmaCoherence::Unsnooped)),
            &caller,
            &sink,
            PoolId::fresh(),
            record_sync,
        );
        let slab = host.alloc_dma_zeroed(PAGE_SIZE).expect("a carve");
        slab.sync_range(0, 512);
        assert_eq!(SYNCED.with(Cell::get), 512);
    }

    #[test]
    fn a_node_declaring_no_window_is_carved_nothing() {
        let (frames, ram, sink) = (frames(), ram(), NullSink);
        let caller = caller(&sink);
        let host = WindowDmaHost::new(
            pool(&frames, &ram),
            None,
            &caller,
            &sink,
            PoolId::fresh(),
            record_sync,
        );
        assert_eq!(
            host.alloc_dma_zeroed(PAGE_SIZE).err(),
            Some(DriverError::Unsupported)
        );
    }
}
