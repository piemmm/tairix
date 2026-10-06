//! `plans/IOMMU.md` MI0, MI2 and IOM6: provoke a live DMA translation fault
//! behind a real emulated unit and witness it delivered through the unit's
//! fault interrupt, the path the register models cannot exercise. One binary
//! runs behind an `intel-iommu`, the other behind an `amd-iommu`.
//!
//! A bin-local PID 1 seam admits a misbehaving in-kernel virtio-blk-PCI
//! driver. It carves its rings through its node's domain, confirms a mapped
//! read reaches the device, then points a device write at an unmapped
//! address: a canary page's physical address. The unit refuses it and raises
//! its fault interrupt; the per-unit fault service drains the record into
//! `DmaTranslationFault` against the device's node. The driver then ends its
//! owner, and its function stops mastering (IOM7).
//!
//! PASS once, after `DmaTranslationUnit` `outcome=translating` with no
//! function found mastering, an `unmapped` fault arrives against the node
//! granted mastering — a write, where the unit records the direction — then
//! the canary, judged only after that, is reported clean, and the function
//! stopped mastering. Anything out of that order fails the run.
//!
//! The storm-and-silence path is proven against the register models: QEMU's
//! virtio device breaks after one refused DMA, so a live storm would need a
//! device reset per fault.

#[cfg(itest_x86_64)]
mod kernel {
    use core::convert::Infallible;
    use core::panic::PanicInfo;
    use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use alloc::boxed::Box;

    use tairix_abi::hwtree::snapshot_nodes;
    use tairix_abi::{CapabilityId, HwMatchKey, HwNode};
    use tairix_arch_x86_64::paging::{AddressSpace as ArchAddressSpace, PageTablePool};
    use tairix_arch_x86_64::qemu_exit;
    use tairix_caps::CapabilitySet;
    use tairix_drv_bus_virtio::PciTransport;
    use tairix_drv_storage_virtio_blk::VIRTIO_BLK_DEVICE_ID;
    use tairix_itest_witness::field;
    use tairix_kernel::hwtree_store::HW_TREE_SOURCE;
    use tairix_kernel::kalloc::{Heap, HEAP_BYTES};
    use tairix_kernel::x86_64::arch_wrapper::published_irq_table;
    use tairix_kernel::x86_64::init_spawn::X86_64InitSpawn;
    use tairix_kernel::x86_64::msi::{published_composite, MSIX_ENTRY};
    use tairix_kernel::x86_64::remapping::route_function;
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

    /// No node has been granted bus mastering yet.
    const NO_NODE: u64 = u64::MAX;

    /// Replays every event to serial and judges the run on the ordered witness
    /// set: the unit translating with no function mastering, then a
    /// node-attributed write fault, the driver's canary-clean marker, and its
    /// function's bus mastering withdrawn (in any order).
    struct FaultSink {
        translating: AtomicBool,
        fault_seen: AtomicBool,
        canary_clean: AtomicBool,
        /// The node granted bus mastering once the unit translates.
        mastered: AtomicU64,
        withdrawn: AtomicBool,
    }

    impl FaultSink {
        fn settle(&self) {
            if self.translating.load(Ordering::Acquire)
                && self.fault_seen.load(Ordering::Acquire)
                && self.canary_clean.load(Ordering::Acquire)
                && self.withdrawn.load(Ordering::Acquire)
            {
                qemu_exit::exit_success();
            }
        }

        /// A function's bus mastering changed: granted only once a unit
        /// translates, and withdrawn, read back, from the node granted it.
        fn mastering(&self, event: &Event<'_>) {
            let Some(FieldValue::UnsignedInt(node)) = field(event, "node") else {
                qemu_exit::exit_failure();
            };
            let applied = matches!(field(event, "outcome"), Some(FieldValue::Str("applied")));
            match field(event, "master") {
                Some(FieldValue::Str("on"))
                    if applied && self.translating.load(Ordering::Acquire) =>
                {
                    self.mastered.store(*node, Ordering::Release);
                }
                Some(FieldValue::Str("off"))
                    if applied && self.mastered.load(Ordering::Acquire) == *node =>
                {
                    self.withdrawn.store(true, Ordering::Release);
                    self.settle();
                }
                _ => qemu_exit::exit_failure(),
            }
        }
    }

    impl Sink for FaultSink {
        fn write_event(&self, event: &Event<'_>) {
            SerialSink::new().write_event(event);
            let id = event.id.0;
            if id == AuditEvent::DmaTranslationUnit.id().0 {
                if matches!(
                    field(event, "outcome"),
                    Some(FieldValue::Str("translating"))
                ) && matches!(field(event, "stopped"), Some(FieldValue::UnsignedInt(0)))
                    && matches!(field(event, "refused"), Some(FieldValue::UnsignedInt(0)))
                {
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
                let mastered = self.mastered.load(Ordering::Acquire);
                let ours = matches!(field(event, "node"), Some(FieldValue::UnsignedInt(node)) if *node == mastered);
                let unmapped = matches!(field(event, "reason"), Some(FieldValue::Str("unmapped")));
                if (write || !crate::DIRECTION_RECORDED) && ours && unmapped {
                    self.fault_seen.store(true, Ordering::Release);
                    self.settle();
                }
            } else if id == CANARY_CLEAN_EVENT_ID {
                // A canary judged before the refusal was recorded proves
                // nothing: the write may not have been tried yet.
                if !self.fault_seen.load(Ordering::Acquire) {
                    qemu_exit::exit_failure();
                }
                self.canary_clean.store(true, Ordering::Release);
                self.settle();
            } else if id == AuditEvent::DmaBusMaster.id().0 {
                self.mastering(event);
            }
        }
    }

    static AUDIT_SINK: FaultSink = FaultSink {
        translating: AtomicBool::new(false),
        fault_seen: AtomicBool::new(false),
        canary_clean: AtomicBool::new(false),
        mastered: AtomicU64::new(NO_NODE),
        withdrawn: AtomicBool::new(false),
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

    /// The translated virtio-blk node the probe published: the device this
    /// vertical drives, whose DMA its unit confines (it carries an
    /// `IommuStream`).
    fn translated_block_node() -> Result<HwNode, &'static str> {
        let snapshot = HW_TREE_SOURCE
            .snapshot()
            .map_err(|_| "hardware-tree snapshot")?;
        let key = HwMatchKey::virtio(VIRTIO_BLK_DEVICE_ID);
        snapshot_nodes(&snapshot)
            .ok_or("hardware-tree decode")?
            .find(|node| {
                node.match_keys().contains(&key)
                    && node.resources().iter().any(|r| r.iommu_streams().is_ok())
            })
            .ok_or("no translated virtio-blk node")
    }

    /// The provisioned device: its transport, the `'static` translated DMA
    /// host its rings and buffers carve through, and its node.
    type OpenedDevice = (
        PciTransport,
        &'static KernelVirtioHost<'static, ArchAddressSpace, dyn Sink + Sync>,
        u32,
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
        let pci = tairix_kernel::pci_host::published().ok_or("no PCI host")?;

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

        // The device's node, and the function the probe published it for.
        let node = translated_block_node()?;
        let function = pci
            .published(node.id())
            .ok_or("node's function unrecorded")?;
        let (segment, bdf) = (function.segment, function.address);
        let transport = {
            let mapper = KernelMmioMapper::new(&mut *mmio, caller, audit);
            pci.with(segment, |bus| {
                provision_virtio_pci(bus, bdf, &mapper, |windows| {
                    PciTransport::new(windows, Some(MSIX_ENTRY))
                })
            })
            .ok_or("node's segment unowned")?
            .map_err(|_| "virtio-PCI provisioning")?
            .transport
        };

        // The domain the device carves through: the kernel's own, which no
        // driver can take.
        if translation.dma_path(node.id()) != tairix_kernel_core::iommu::DmaPath::Translated {
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
        let msi_vector = route_function(pci, &function)?;
        let bind = table
            .bind(msi_vector.line, MISBEHAVE_OWNER)
            .map_err(|_| "bind device source")?;
        let handle = bind.handle;

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
            MISBEHAVE_OWNER,
            composite,
            None,
        )));
        let host: &'static KernelVirtioHost<'static, _, dyn Sink + Sync> = Box::leak(Box::new(
            KernelVirtioHost::new(pool, caller, audit, PoolId::fresh(), table, handle, waiter),
        ));
        Ok((transport, host, node.id()))
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

        let (mut transport, host, node) = open_translated_device(audit, frames, phys, translation)?;
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
        // The canary is judged only once the unit's refusal is recorded: the
        // device may try the write after the wait above gave up on it.
        let mut waits = HAPPY_BUDGET_NS / FAULT_BUDGET_NS;
        while !AUDIT_SINK.fault_seen.load(Ordering::Acquire) {
            if waits == 0 {
                return Err("the refused write was never recorded");
            }
            waits -= 1;
            let _ = host.notify_wait(0, FAULT_BUDGET_NS);
        }

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

        // End the owner the way every driver's end does: the function stops
        // mastering before its domain goes.
        if !translation.revoke(node, KERNEL_OWNER) {
            return Err("the unit could not confirm the owner's end");
        }

        // Work done; the witness sink ends the run on the records and the
        // marker. Park for life so nothing the device was handed is freed.
        loop {
            let _ = host.notify_wait(0, u64::MAX);
        }
    }
}
