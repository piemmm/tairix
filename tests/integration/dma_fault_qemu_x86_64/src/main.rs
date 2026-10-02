//! `plans/IOMMU.md` IOM6 / MI0 QEMU integration test: provoke a **live** DMA
//! translation fault on real emulated silicon and witness it delivered through
//! the fault-event MSI — the path the register-level host model cannot exercise.
//!
//! Booting the production x86_64 pipeline on `q35` behind an `intel-iommu`, a
//! bin-local PID 1 spawn seam admits a misbehaving in-kernel virtio-blk-PCI
//! driver (its pre-dispatch, in place of the production root-unlock). The
//! driver carves its virtqueue rings through its node's translation domain, so
//! their device addresses are IOVAs; a valid read of sector 0 into a mapped
//! buffer confirms the device reaches its domain through the unit. It then
//! posts a read whose device-writable data descriptor points at an **unmapped**
//! device address (a canary page's physical address). The VT-d unit refuses the
//! write, records the fault, and raises its fault-event MSI; the kernel's
//! per-unit fault service drains it into `AuditEvent::DmaTranslationFault`
//! against the device's node and stream. The canary stays zero, proving the
//! refused write landed nowhere.
//!
//! PASS via QEMU `isa-debug-exit` once, after
//! `AuditEvent::DmaTranslationUnit` `outcome=translating`, a write
//! `DmaTranslationFault` attributed to a node arrives **and** the driver has
//! reported its canary clean. A unit that did not translate, a fault before any
//! unit translates, a canary the write reached, or an un-admittable driver each
//! fail the run.
//!
//! The 512-fault storm/silence path is proven against the register-level model
//! (`kernel/core/src/iommu/faults.rs`), not re-proven here: QEMU's virtio device
//! breaks after one refused DMA, so a live storm would need ~512 device resets
//! per window — a load-dependent, flaky mechanism the charter forbids.

#![cfg_attr(itest_x86_64, no_std)]
#![cfg_attr(itest_x86_64, no_main)]
#![deny(missing_docs)]

#[cfg(itest_x86_64)]
extern crate alloc;

#[cfg(itest_x86_64)]
mod kernel {
    use core::convert::Infallible;
    use core::panic::PanicInfo;
    use core::sync::atomic::{AtomicBool, Ordering};

    use alloc::boxed::Box;

    use tairix_abi::driver::msix::MsixBus;
    use tairix_abi::driver::pci::requester_id;
    use tairix_abi::hwtree::snapshot_nodes;
    use tairix_abi::{CapabilityId, HwNode};
    use tairix_arch_x86_64::paging::{AddressSpace as ArchAddressSpace, PageTablePool};
    use tairix_arch_x86_64::pio::x86_port_io;
    use tairix_arch_x86_64::qemu_exit;
    use tairix_caps::CapabilitySet;
    use tairix_drv_bus_virtio::PciTransport;
    use tairix_drv_storage_virtio_blk::VIRTIO_BLK_DEVICE_ID;
    use tairix_kernel::hwdiscovery::virtio_pci_modern_device_id;
    use tairix_kernel::hwtree_store::HW_TREE_SOURCE;
    use tairix_kernel::kalloc::{Heap, HEAP_BYTES};
    use tairix_kernel::x86_64::arch_wrapper::published_irq_table;
    use tairix_kernel::x86_64::init_spawn::X86_64InitSpawn;
    use tairix_kernel::x86_64::msi::{kernel_message, published_composite};
    use tairix_kernel::x86_64::spawn_producer::SPAWN_TABLE_PHYSMAP;
    use tairix_kernel::{
        boot_with_init, handle_panic_via_kernel_core, FreeListAllocator, SerialSink, SERIAL_SINK,
    };
    use tairix_kernel_core::iommu::{Translation, KERNEL_OWNER};
    use tairix_kernel_core::{AuditEvent, HwTreeSource, InitSpawnCtx, IrqParkWaiter, YieldHandle};
    use tairix_kernel_irq::{IrqController, IrqTable};
    use tairix_kernel_mem::{
        AddressSpace, DmaPool, DmaTranslator, FrameAllocator, MemoryClass, MmioMap, PhysMap,
        VirtAddr, PAGE_SIZE,
    };
    use tairix_kernel_sec::captable::TaskCapabilities;
    use tairix_kernel_sec::identity::UserId;
    use tairix_kernel_virtio::{provision_virtio_pci, KernelMmioMapper, KernelVirtioHost};
    use tairix_log::{Event, EventId, FieldValue, Level, Sink};
    use tairix_virtio::{
        ChainSegment, Direction, DmaHost, PoolId, RequestQueue, SplitQueue, Status, Transport,
        VirtioHost, TRANSPORT_FEATURES,
    };

    /// The boot heap, in `.bss`.
    static HEAP: Heap = Heap::ZERO;

    /// Global allocator backed by [`HEAP`].
    ///
    /// SAFETY: the page-aligned `HEAP` static outlives the binary and the
    /// allocator is its only consumer.
    #[global_allocator]
    static ALLOCATOR: FreeListAllocator =
        unsafe { FreeListAllocator::new(HEAP.as_mut_ptr(), HEAP_BYTES) };

    /// The kernel service identity the misbehaving driver binds its device MSI
    /// line to and derives its capabilities under: a fixed kernel id below the
    /// task-id draw, distinct from the unlock and fault-service identities.
    const MISBEHAVE_OWNER: tairix_kernel_sec::captable::ProcessId =
        tairix_kernel_sec::captable::ProcessId(0x5b8);

    /// The MSI-X table entry the device's completion vector is programmed into.
    const MSIX_ENTRY: u16 = 0;

    /// Queue 0 depth and the longest chain posted on it (header + data +
    /// status).
    const QUEUE_SIZE: u16 = 8;
    const REQUEST_CHAIN_LEN: u16 = 3;

    /// Independent bookkeeping bases for the two throwaway arch spaces (never
    /// made live; device access is through the identity direct map), both above
    /// the 32 MiB low identity each maps.
    const MMIO_VBASE: u64 = 0x6000_0000;
    const POOL_VBASE: u64 = 0x2000_0000;
    const MMIO_CAP_PAGES: usize = 64;
    const POOL_PAGES: usize = 64;

    /// virtio-blk request header (virtio 1.1 §5.2.6): a read of sector 0.
    const VIRTIO_BLK_T_IN: u32 = 0;
    const HEADER_LEN: u32 = 16;
    const SECTOR_SIZE: u32 = 512;
    const STATUS_LEN: u32 = 1;

    /// Budgets (host clock): generous for the happy-path completion, short for
    /// the fault request, which never completes.
    const HAPPY_BUDGET_NS: u64 = 2_000_000_000;
    const FAULT_BUDGET_NS: u64 = 100_000_000;

    /// Witness marker ids the driver emits onto the audit sink. Far above the
    /// `AuditEvent` id range, so they cannot collide with a kernel event.
    const CANARY_CLEAN_EVENT_ID: u32 = 0x7E57_0001;
    const DRIVER_STAGE_EVENT_ID: u32 = 0x7E57_0002;

    /// The page-table frame pool the throwaway bookkeeping arch spaces draw
    /// their tables from. Private to this driver so it never contends the
    /// boot/init pools.
    static PT_POOL: PageTablePool = PageTablePool::new();

    /// Replays every event to serial and judges the run on the ordered witness
    /// set: the unit translating, then a node-attributed write fault and the
    /// driver's canary-clean marker (in any order).
    struct FaultSink {
        translating: AtomicBool,
        fault_seen: AtomicBool,
        canary_clean: AtomicBool,
    }

    impl FaultSink {
        fn settle(&self) {
            if self.translating.load(Ordering::Acquire)
                && self.fault_seen.load(Ordering::Acquire)
                && self.canary_clean.load(Ordering::Acquire)
            {
                qemu_exit::exit_success();
            }
        }
    }

    fn field<'e>(event: &'e Event<'_>, key: &str) -> Option<&'e FieldValue<'e>> {
        event
            .fields
            .iter()
            .find(|field| field.key == key)
            .map(|field| &field.value)
    }

    impl Sink for FaultSink {
        fn write_event(&self, event: &Event<'_>) {
            SerialSink::new().write_event(event);
            let id = event.id.0;
            if id == AuditEvent::DmaTranslationUnit.id().0 {
                if matches!(
                    field(event, "outcome"),
                    Some(FieldValue::Str("translating"))
                ) {
                    self.translating.store(true, Ordering::Release);
                    self.settle();
                } else {
                    // The unit did not come up translating with a served fault
                    // path (e.g. `faults_unrouted`); the live fault cannot be
                    // delivered, so the run cannot prove anything.
                    qemu_exit::exit_failure();
                }
            } else if id == AuditEvent::DmaTranslationFault.id().0 {
                if !self.translating.load(Ordering::Acquire) {
                    // A fault before any unit translates is impossible on a
                    // correct kernel.
                    qemu_exit::exit_failure();
                }
                let write = matches!(field(event, "access"), Some(FieldValue::Str("write")));
                let attributed = field(event, "node").is_some();
                if write && attributed {
                    self.fault_seen.store(true, Ordering::Release);
                    self.settle();
                }
            } else if id == CANARY_CLEAN_EVENT_ID {
                self.canary_clean.store(true, Ordering::Release);
                self.settle();
            }
        }
    }

    static AUDIT_SINK: FaultSink = FaultSink {
        translating: AtomicBool::new(false),
        fault_seen: AtomicBool::new(false),
        canary_clean: AtomicBool::new(false),
    };

    /// The bin-local PID 1 spawn seam: the production PID 1 build, with the
    /// misbehaving driver admitted in place of root-unlock before the dispatch
    /// loop.
    static DMA_FAULT_INIT_SPAWN: X86_64InitSpawn =
        X86_64InitSpawn::with_pre_dispatch(misbehave_pre_dispatch);

    /// A panic halts the guest; the run times out and fails loud.
    #[panic_handler]
    fn tairix_dma_fault_qemu_x86_64_panic(info: &PanicInfo<'_>) -> ! {
        handle_panic_via_kernel_core(info)
    }

    /// The symbol the arch crate's boot trampoline calls. Boots at `Debug` so
    /// every audit record (the `Warn` fault, the `Info` unit outcome) reaches
    /// the witness sink regardless of the global filter.
    #[no_mangle]
    pub extern "C" fn kernel_main(multiboot_info: u64) -> ! {
        boot_with_init(
            multiboot_info,
            &ALLOCATOR,
            &SERIAL_SINK,
            &AUDIT_SINK,
            tairix_log::Level::Debug,
            &DMA_FAULT_INIT_SPAWN,
        )
    }

    /// Admit the misbehaving driver kthread before the dispatch loop diverges.
    /// It runs on first dispatch (the run queue is live), so it parks normally.
    fn misbehave_pre_dispatch(ctx: &'static (dyn InitSpawnCtx + Sync)) {
        let body = Box::new(move |_: &mut dyn YieldHandle| {
            let audit = ctx.static_audit().unwrap_or(&SERIAL_SINK);
            let Err(stage) = run_driver(ctx, audit);
            log_stage(audit, stage);
            qemu_exit::exit_failure();
        });
        if ctx.spawn_kernel_service(body).is_none() {
            // Nothing will provoke the fault, so the run can prove nothing.
            qemu_exit::exit_failure();
        }
    }

    /// One descriptor-chain segment.
    fn seg(device_addr: u64, len: u32, direction: Direction) -> ChainSegment {
        ChainSegment {
            device_addr,
            len,
            direction,
        }
    }

    /// The capabilities the driver reaches its hardware through: the DMA host's
    /// `alloc_dma` gates on `CAP_MEM_DMA`, the register-window map on
    /// `CAP_MMIO_MAP`.
    fn driver_caps() -> CapabilitySet {
        let mut caps = CapabilitySet::empty();
        caps.insert(CapabilityId::MMIO_MAP);
        caps.insert(CapabilityId::MEM_DMA);
        caps
    }

    /// Emit a stage string onto the audit sink (replayed to serial) before a
    /// fail-closed exit, so a failed run names where it stopped.
    fn log_stage(audit: &(dyn Sink + Sync), stage: &'static str) {
        tairix_log::log(
            audit,
            &Event {
                level: Level::Error,
                id: EventId(DRIVER_STAGE_EVENT_ID),
                message: stage,
                fields: &[],
            },
        );
    }

    /// Tell the witness sink the refused write reached nothing.
    fn mark_canary_clean(audit: &(dyn Sink + Sync)) {
        tairix_log::log(
            audit,
            &Event {
                level: Level::Info,
                id: EventId(CANARY_CLEAN_EVENT_ID),
                message: "dma-fault vertical: canary clean",
                fields: &[],
            },
        );
    }

    /// The translated hardware-tree node of the function at requester id
    /// `requester`: the one that masters DMA through a unit (carries an
    /// `IommuStream`).
    fn translated_node(requester: u32) -> Result<HwNode, &'static str> {
        let snapshot = HW_TREE_SOURCE
            .snapshot()
            .map_err(|_| "hardware-tree snapshot")?;
        snapshot_nodes(&snapshot)
            .ok_or("hardware-tree decode")?
            .find(|node| {
                node.address() == requester
                    && node.resources().iter().any(|r| r.iommu_streams().is_ok())
            })
            .ok_or("no translated node for the provisioned function")
    }

    /// The provisioned device: its transport and the `'static` translated DMA
    /// host its rings and buffers carve through.
    type OpenedDevice = (
        PciTransport,
        &'static KernelVirtioHost<'static, ArchAddressSpace, dyn Sink + Sync>,
    );

    /// Provision the misbehaving virtio-blk-PCI device through its node's
    /// translation domain and build the translated DMA host its rings and
    /// buffers carve through, with its completion MSI routed. Every borrow is
    /// leaked to `'static` because the device is shared for the kernel's life.
    fn open_translated_device(
        audit: &'static (dyn Sink + Sync),
        frames: &'static FrameAllocator,
        phys: &'static dyn PhysMap,
        translation: &'static Translation,
    ) -> Result<OpenedDevice, &'static str> {
        let caller: &'static TaskCapabilities = Box::leak(Box::new(TaskCapabilities::derive(
            MISBEHAVE_OWNER,
            UserId(0),
            driver_caps(),
            driver_caps(),
            audit,
        )));
        let bus = tairix_pci::mechanism_one(x86_port_io());

        // Throwaway MMIO register-window map (bookkeeping only; device access is
        // through the identity direct map).
        let mmio_space = ArchAddressSpace::new_bookkeeping_identity_32mib(&PT_POOL)
            .ok_or("mmio bookkeeping space")?;
        let mmio: &'static mut MmioMap<'static, ArchAddressSpace> = Box::leak(Box::new(
            MmioMap::new(
                AddressSpace::new(mmio_space),
                VirtAddr::new(MMIO_VBASE),
                MMIO_CAP_PAGES,
                phys,
            )
            .map_err(|_| "mmio map")?,
        ));

        let device_id = u16::try_from(virtio_pci_modern_device_id(VIRTIO_BLK_DEVICE_ID))
            .map_err(|_| "virtio-blk device id out of range")?;
        let (mut transport, bdf) = {
            let mapper = KernelMmioMapper::new(&mut *mmio, caller, audit);
            let prov = provision_virtio_pci(&bus, device_id, &mapper, PciTransport::new)
                .map_err(|_| "virtio-PCI provisioning")?;
            (prov.transport, prov.bdf)
        };

        // The device's node, by its requester id, and the domain it carves
        // through: the kernel's own, which no driver can take.
        let node = translated_node(u32::from(requester_id(bdf)))?;
        if !translation.translates(node.id()) {
            return Err("provisioned function is not behind a translation unit");
        }
        let translator = DmaTranslator {
            node: node.id(),
            generation: KERNEL_OWNER,
            domains: translation,
        };

        // A dedicated MSI vector + line for the device's completion interrupt,
        // bound to the driver's kernel identity.
        let table: &'static IrqTable = published_irq_table().ok_or("no published IRQ table")?;
        let composite: &'static (dyn IrqController + Sync) =
            published_composite().ok_or("no interrupt controller")?;
        let (msi_vector, msi) = kernel_message().map_err(|_| "no free MSI vector")?;
        let bind = table
            .bind(msi_vector.line, MISBEHAVE_OWNER)
            .map_err(|_| "bind device source")?;
        let handle = bind.handle;
        {
            let mapper = KernelMmioMapper::new(&mut *mmio, caller, audit);
            bus.route_msix(bdf, MSIX_ENTRY, msi, &mapper)
                .map_err(|_| "route MSI-X")?;
        }
        transport.enable_msix(MSIX_ENTRY);

        // The translated per-driver DMA host: every ring and buffer carved here
        // maps into the device's own domain, so its device address is an IOVA.
        let dma_space = ArchAddressSpace::new_bookkeeping_identity_32mib(&PT_POOL)
            .ok_or("dma bookkeeping space")?;
        let pool = DmaPool::new(
            AddressSpace::new(dma_space),
            VirtAddr::new(POOL_VBASE),
            POOL_PAGES,
            frames,
            phys,
        )
        .map_err(|_| "dma pool")?
        .translated(translator);
        let waiter: &'static IrqParkWaiter = Box::leak(Box::new(IrqParkWaiter::new(
            table,
            handle,
            msi_vector.line,
            composite,
            None,
        )));
        let host: &'static KernelVirtioHost<'static, _, dyn Sink + Sync> = Box::leak(Box::new(
            KernelVirtioHost::new(pool, caller, audit, PoolId::fresh(), table, handle, waiter),
        ));
        Ok((transport, host))
    }

    /// Bring the device online (virtio 1.1 §3.1) and set up queue 0. Accepting
    /// the platform-translation feature is required, or the device would bypass
    /// the unit and nothing here would be translated.
    fn online(
        transport: &mut PciTransport,
        host: &'static KernelVirtioHost<'static, ArchAddressSpace, dyn Sink + Sync>,
    ) -> Result<RequestQueue, &'static str> {
        transport.reset().map_err(|_| "device reset")?;
        let mut status = Status::default().with(Status::ACKNOWLEDGE);
        transport.set_status(status);
        status = status.with(Status::DRIVER);
        transport.set_status(status);
        let features = transport.device_features() & TRANSPORT_FEATURES;
        transport.set_driver_features(features);
        status = status.with(Status::FEATURES_OK);
        transport.set_status(status);
        if !transport.status().contains(Status::FEATURES_OK) {
            return Err("device rejected feature negotiation");
        }
        let queue = SplitQueue::new(transport, host, 0, QUEUE_SIZE, REQUEST_CHAIN_LEN)
            .map_err(|_| "queue setup")?;
        status = status.with(Status::DRIVER_OK);
        transport.set_status(status);
        Ok(RequestQueue::new(queue))
    }

    /// Bring the misbehaving virtio-blk-PCI device up through its node's
    /// translation domain, confirm a mapped read, then provoke one live fault
    /// and prove the refused write landed nowhere. On success it parks for
    /// life; the witness sink ends the run.
    fn run_driver(
        ctx: &'static (dyn InitSpawnCtx + Sync),
        audit: &'static (dyn Sink + Sync),
    ) -> Result<Infallible, &'static str> {
        let frames = ctx.static_frames().ok_or("no kernel frame allocator")?;
        let translation = ctx.dma_translation().ok_or("no dma translation unit")?;
        let phys = &SPAWN_TABLE_PHYSMAP;

        let (mut transport, host) = open_translated_device(audit, frames, phys, translation)?;
        let mut rq = online(&mut transport, host)?;

        // A read-request header (device-read): sector 0.
        let mut header = host
            .alloc_dma_zeroed(HEADER_LEN as usize)
            .map_err(|_| "header carve")?;
        header.as_bytes_mut()[0..4].copy_from_slice(&VIRTIO_BLK_T_IN.to_le_bytes());

        // Happy path: a valid read into a mapped buffer completes, proving the
        // device reaches its domain's mappings through the unit.
        let data = host
            .alloc_dma_zeroed(SECTOR_SIZE as usize)
            .map_err(|_| "data carve")?;
        let rstatus = host
            .alloc_dma_zeroed(STATUS_LEN as usize)
            .map_err(|_| "status carve")?;
        let happy = [
            seg(header.device_addr(), HEADER_LEN, Direction::DeviceRead),
            seg(data.device_addr(), SECTOR_SIZE, Direction::DeviceWrite),
            seg(rstatus.device_addr(), STATUS_LEN, Direction::DeviceWrite),
        ];
        rq.submit_and_wait(&mut transport, host, &happy, HAPPY_BUDGET_NS)
            .map_err(|_| "happy-path read did not complete through the unit")?;

        // The canary: a page we own, zeroed, at a known physical address we
        // point the device's write at — a valid-but-unmapped IOVA the unit must
        // refuse. A leaked write would change it.
        let canary = frames
            .alloc(MemoryClass::Kernel)
            .map_err(|_| "canary frame")?;
        let canary_phys = canary.start().as_u64();
        let canary_ptr = phys
            .translate(canary.start(), PAGE_SIZE)
            .ok_or("canary not in direct map")?;
        // SAFETY: `canary_ptr` covers `PAGE_SIZE` bytes of a frame just
        // allocated and reached only through this pointer; no other mapping of
        // it is live.
        unsafe { core::ptr::write_bytes(canary_ptr.as_ptr(), 0, PAGE_SIZE) };

        // The fault: the device-writable data descriptor points at the unmapped
        // canary address. The unit refuses the write and records the fault; the
        // request never completes, so the wait times out.
        let fault = [
            seg(header.device_addr(), HEADER_LEN, Direction::DeviceRead),
            seg(canary_phys, SECTOR_SIZE, Direction::DeviceWrite),
        ];
        let _ = rq.submit_and_wait(&mut transport, host, &fault, FAULT_BUDGET_NS);

        // The refused write landed nowhere: the canary is still zero.
        // SAFETY: the same frame and exclusive pointer as the zeroing write.
        let leaked =
            unsafe { core::slice::from_raw_parts(canary_ptr.as_ptr().cast_const(), PAGE_SIZE) }
                .iter()
                .any(|&byte| byte != 0);
        if leaked {
            return Err("refused DMA write reached the canary page");
        }
        mark_canary_clean(audit);

        // Work done; the witness sink ends the run on the fault record and the
        // marker. Park for life so nothing the device was handed is torn down.
        loop {
            let _ = host.notify_wait(0, u64::MAX);
        }
    }
}

#[cfg(not(itest_x86_64))]
fn main() {}
